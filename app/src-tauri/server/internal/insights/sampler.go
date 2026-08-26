package insights

import (
	"sort"
	"sync"
	"sync/atomic"
	"time"

	"github.com/shirou/gopsutil/v4/cpu"
	"github.com/shirou/gopsutil/v4/disk"
	"github.com/shirou/gopsutil/v4/load"
	"github.com/shirou/gopsutil/v4/mem"
	gnet "github.com/shirou/gopsutil/v4/net"
	"github.com/shirou/gopsutil/v4/process"
)

// ─── JSON shapes (the /current snapshot) ────────────────────────────────

type CPUInfo struct {
	Pct     float64   `json:"pct"`
	PerCore []float64 `json:"per_core"`
	Load    []float64 `json:"load"`
}
type MemInfo struct {
	Total    uint64 `json:"total"`
	Used     uint64 `json:"used"`
	Cached   uint64 `json:"cached"`
	SwapUsed uint64 `json:"swap_used"`
}
type DiskInfo struct {
	Mount string  `json:"mount"`
	Total uint64  `json:"total"`
	Used  uint64  `json:"used"`
	Pct   float64 `json:"pct"`
}
type NetIface struct {
	Iface string `json:"iface"`
	RxBps uint64 `json:"rx_bps"`
	TxBps uint64 `json:"tx_bps"`
}
type ProcInfo struct {
	PID  int32   `json:"pid"`
	Name string  `json:"name"`
	CPU  float64 `json:"cpu"`
	RSS  uint64  `json:"rss"`
}

type Snapshot struct {
	TS            int64             `json:"ts"`
	CPU           CPUInfo           `json:"cpu"`
	Mem           MemInfo           `json:"mem"`
	Disks         []DiskInfo        `json:"disks"`
	Net           []NetIface        `json:"net"`
	DockerRunning int               `json:"docker_running"`
	DockerTotal   int               `json:"docker_total"`
	Top           []ProcInfo        `json:"top"`
	// Stored, not in /current JSON (the /docker endpoint serves the full list).
	NetRxBps uint64            `json:"-"`
	NetTxBps uint64            `json:"-"`
	Docker   []DockerContainer `json:"-"`
}

// ─── Sampler ────────────────────────────────────────────────────────────

type Sampler struct {
	mu          sync.Mutex
	lastNet     map[string][2]uint64 // iface -> {rxBytes, txBytes}
	lastNetT    time.Time
	dockerLogAt time.Time // rate-limit the docker-unavailable log

	// Phase 86 cadence. Docker list+stats and the top-processes walk are the
	// two expensive phases (measured 2.8s of a 3.1s sample on a 23-container
	// host), so they run on a sub-cadence of the ticker and the previous
	// result is carried forward on the other ticks. A call counter, not a
	// clock, so tests are deterministic. Guarded by mu.
	calls      uint64
	lastDocker []DockerContainer // nil until the first successful list (or after docker went away)
	lastTop    []ProcInfo

	// last holds the freshest snapshot produced by the background ticker.
	// /current serves it without blocking on live collection (Phase 72.3).
	last atomic.Pointer[Snapshot]
}

func NewSampler() *Sampler {
	return &Sampler{lastNet: map[string][2]uint64{}}
}

// Sub-cadences in Sample calls (5s ticker → docker every 30s, top every 10s).
const (
	dockerEvery = 6
	topEvery    = 2
)

// Current returns the freshest cached snapshot for the /current endpoint.
// The background ticker refreshes it every interval, so this NEVER blocks on
// live collection — a slow docker-stats round-trip or a hung disk mount could
// otherwise exceed the desktop's HTTP timeout and make the Monitor report the
// daemon "unreachable" (Phase 72.3 root cause: handleCurrent ran Sample(true)
// synchronously). Falls back to one live sample only before the first tick.
func (s *Sampler) Current() *Snapshot {
	if snap := s.last.Load(); snap != nil {
		return snap
	}
	return s.Sample(true)
}

// logDockerOnce logs why Docker is unavailable at most every 5 minutes, so the
// sampler's 0/0 count is explained in insights.log without spamming it.
func (s *Sampler) logDockerOnce() {
	s.mu.Lock()
	due := time.Since(s.dockerLogAt) > 5*time.Minute
	if due {
		s.dockerLogAt = time.Now()
	}
	s.mu.Unlock()
	if due {
		socket, reason, detail := dockerResolve()
		if reason == "" {
			reason = "api_error"
		}
		logDockerUnavailable(socket, reason, detail)
	}
}

// Sample collects one snapshot. includeTop adds the top-processes list
// (slightly heavier — only done for on-demand /current, not stored ticks).
//
// Docker and top run every dockerEvery / topEvery calls; in between the
// snapshot carries the previous result (so /current and the docker_samples
// rows never go blank). Call 0 runs everything, which also covers the
// pre-first-tick live fallback in Current().
func (s *Sampler) Sample(includeTop bool) *Snapshot {
	now := time.Now()
	snap := &Snapshot{TS: now.Unix()}
	t0 := now

	s.mu.Lock()
	n := s.calls
	s.calls++
	doDocker := n%dockerEvery == 0 || s.lastDocker == nil
	doTop := includeTop && (n%topEvery == 0 || s.lastTop == nil)
	s.mu.Unlock()

	// CPU: overall % since last call (non-blocking), + per-core, + load avg.
	if pct, err := cpu.Percent(0, false); err == nil && len(pct) > 0 {
		snap.CPU.Pct = round1(pct[0])
	}
	if per, err := cpu.Percent(0, true); err == nil {
		snap.CPU.PerCore = make([]float64, len(per))
		for i, v := range per {
			snap.CPU.PerCore[i] = round1(v)
		}
	}
	if l, err := load.Avg(); err == nil {
		snap.CPU.Load = []float64{l.Load1, l.Load5, l.Load15}
	}

	// Memory + swap.
	if vm, err := mem.VirtualMemory(); err == nil {
		snap.Mem = MemInfo{Total: vm.Total, Used: vm.Used, Cached: vm.Cached}
	}
	if sw, err := mem.SwapMemory(); err == nil {
		snap.Mem.SwapUsed = sw.Used
	}

	// Disks: real (non-virtual) mounts only.
	if parts, err := disk.Partitions(false); err == nil {
		seen := map[string]bool{}
		for _, p := range parts {
			if seen[p.Mountpoint] || isVirtualFS(p.Fstype) {
				continue
			}
			seen[p.Mountpoint] = true
			if u, err := disk.Usage(p.Mountpoint); err == nil && u.Total > 0 {
				snap.Disks = append(snap.Disks, DiskInfo{
					Mount: p.Mountpoint, Total: u.Total, Used: u.Used,
					Pct: round1(u.UsedPercent),
				})
			}
		}
	}

	tDisk := time.Now()

	// Network rates (per-iface delta since last sample + aggregate).
	s.computeNet(snap, now)

	// Docker (best-effort; skipped if the socket is unreachable). Bounded
	// internally (concurrent per-container stats) so it can't stall a sample.
	var conts []DockerContainer
	if doDocker {
		var err error
		if conts, err = dockerList(); err != nil {
			s.logDockerOnce()
			conts = nil // docker down: report empty, retry next tick (nil forces doDocker)
		}
		s.mu.Lock()
		s.lastDocker = conts
		s.mu.Unlock()
	} else {
		s.mu.Lock()
		conts = s.lastDocker
		s.mu.Unlock()
	}
	snap.Docker = conts
	snap.DockerTotal = len(conts)
	for _, c := range conts {
		if c.State == "running" {
			snap.DockerRunning++
		}
	}
	tDocker := time.Now()

	if doTop {
		top := topProcesses(10)
		s.mu.Lock()
		s.lastTop = top
		s.mu.Unlock()
		snap.Top = top
	} else if includeTop {
		s.mu.Lock()
		snap.Top = s.lastTop
		s.mu.Unlock()
	}

	// Timing self-diagnostic: if a sample is slow, log which phase ate the
	// budget so insights.log (served by /logs) pinpoints it. Metadata only.
	tEnd := time.Now()
	if d := tEnd.Sub(t0); d > 2*time.Second {
		logger.Warn("sample slow",
			"total_ms", d.Milliseconds(), "cpu_mem_disk_ms", tDisk.Sub(t0).Milliseconds(),
			"docker_ms", tDocker.Sub(tDisk).Milliseconds(), "top_ms", tEnd.Sub(tDocker).Milliseconds(),
			"include_top", includeTop, "did_docker", doDocker, "did_top", doTop)
	}

	s.last.Store(snap) // publish for /current (Phase 72.3)
	return snap
}

func (s *Sampler) computeNet(snap *Snapshot, now time.Time) {
	io, err := gnet.IOCounters(true)
	if err != nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	dt := now.Sub(s.lastNetT).Seconds()
	first := s.lastNetT.IsZero() || dt <= 0
	var aggRx, aggTx uint64
	for _, c := range io {
		if c.Name == "lo" {
			continue
		}
		prev, ok := s.lastNet[c.Name]
		var rxBps, txBps uint64
		if ok && !first {
			if c.BytesRecv >= prev[0] {
				rxBps = uint64(float64(c.BytesRecv-prev[0]) / dt)
			}
			if c.BytesSent >= prev[1] {
				txBps = uint64(float64(c.BytesSent-prev[1]) / dt)
			}
		}
		s.lastNet[c.Name] = [2]uint64{c.BytesRecv, c.BytesSent}
		snap.Net = append(snap.Net, NetIface{Iface: c.Name, RxBps: rxBps, TxBps: txBps})
		aggRx += rxBps
		aggTx += txBps
	}
	s.lastNetT = now
	snap.NetRxBps = aggRx
	snap.NetTxBps = aggTx
}

func topProcesses(n int) []ProcInfo {
	procs, err := process.Processes()
	if err != nil {
		return nil
	}
	out := make([]ProcInfo, 0, len(procs))
	for _, p := range procs {
		cpuPct, _ := p.CPUPercent()
		name, _ := p.Name()
		var rss uint64
		if mi, err := p.MemoryInfo(); err == nil && mi != nil {
			rss = mi.RSS
		}
		out = append(out, ProcInfo{PID: p.Pid, Name: name, CPU: round1(cpuPct), RSS: rss})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].CPU > out[j].CPU })
	if len(out) > n {
		out = out[:n]
	}
	return out
}

func isVirtualFS(fstype string) bool {
	switch fstype {
	case "tmpfs", "devtmpfs", "proc", "sysfs", "cgroup", "cgroup2", "overlay",
		"squashfs", "devpts", "mqueue", "debugfs", "tracefs", "fusectl", "configfs":
		return true
	}
	return false
}

func round1(f float64) float64 { return float64(int64(f*10+0.5)) / 10 }

// RunSampler drives the periodic store-writing loop + hourly retention sweep.
func RunSampler(store *Store, sm *Sampler, interval time.Duration, stop <-chan struct{}) {
	if interval < time.Second {
		interval = 5 * time.Second
	}
	tick := time.NewTicker(interval)
	defer tick.Stop()
	sweep := time.NewTicker(time.Hour)
	defer sweep.Stop()
	// Prime the CPU/net deltas (first reading is a baseline).
	sm.Sample(false)
	for {
		select {
		case <-stop:
			return
		case <-tick.C:
			// includeTop=true so the cached snapshot served by /current
			// carries the top-processes list without a live (blocking) walk.
			store.insert(sm.Sample(true))
		case <-sweep.C:
			store.Sweep()
		}
	}
}
