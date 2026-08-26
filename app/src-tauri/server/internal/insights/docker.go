package insights

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"
)

// logDockerUnavailable writes a single consistent diagnostic line. Used by
// both the /docker endpoint and the sampler so the cause is unambiguous in
// insights.log (this patch's whole point).
func logDockerUnavailable(socket, reason, detail string) {
	logger.Warn("docker unavailable",
		"reason", reason, "socket", socket, "uid", os.Getuid(), "detail", detail)
}

// dockerCandidates lists the socket paths we probe, in priority order. The
// daemon runs as the SSH user (systemd --user / nohup), so the standard root
// socket at /var/run/docker.sock is often unreadable; rootless Docker exposes
// a per-user socket under $XDG_RUNTIME_DIR / /run/user/<uid> instead.
func dockerCandidates() []string {
	if h := os.Getenv("DOCKER_HOST"); strings.HasPrefix(h, "unix://") {
		return []string{strings.TrimPrefix(h, "unix://")}
	}
	candidates := []string{}
	if xdg := os.Getenv("XDG_RUNTIME_DIR"); xdg != "" {
		candidates = append(candidates, xdg+"/docker.sock")
	}
	return append(candidates,
		fmt.Sprintf("/run/user/%d/docker.sock", os.Getuid()),
		"/var/run/docker.sock",
		"/run/docker.sock",
	)
}

// dockerSockPath returns the first candidate that exists on disk, falling back
// to the standard path so error messages stay meaningful.
func dockerSockPath() string {
	for _, c := range dockerCandidates() {
		if _, err := os.Stat(c); err == nil {
			return c
		}
	}
	return "/var/run/docker.sock"
}

// dockerResolve picks the socket and classifies reachability in one pass,
// returning a machine reason + a human detail. Centralises the logic so both
// the /docker endpoint and the sampler log a consistent, actionable line —
// the whole point of this patch: a failing server must explain itself.
//
// reason: "" (ok) | "not_installed" | "permission" | "no_socket".
func dockerResolve() (socket, reason, detail string) {
	socket = dockerSockPath()
	info, err := os.Stat(socket)
	if err != nil {
		if os.IsNotExist(err) {
			return socket, "not_installed", "no docker socket at any known path (" +
				strings.Join(dockerCandidates(), ", ") + ")"
		}
		return socket, "no_socket", err.Error()
	}
	if info.Mode()&os.ModeSocket == 0 {
		return socket, "no_socket", "path exists but is not a unix socket"
	}
	conn, err := net.DialTimeout("unix", socket, 2*time.Second)
	if err != nil {
		if os.IsPermission(err) || strings.Contains(err.Error(), "permission denied") {
			return socket, "permission", err.Error()
		}
		return socket, "no_socket", err.Error()
	}
	_ = conn.Close()
	return socket, "", ""
}

// dockerHint is an English, actionable one-liner per reason. Sent in the API
// response so even an old desktop UI shows guidance; the new UI localises by
// the machine `reason`.
func dockerHint(reason string) string {
	switch reason {
	case "permission":
		return "the daemon user can't access the Docker socket — add it to the 'docker' group " +
			"(sudo usermod -aG docker $USER) and fully reconnect the workspace so the daemon restarts " +
			"with the new group, or run rootless Docker."
	case "not_installed":
		return "no Docker socket found — is Docker installed and running on this server?"
	case "no_socket":
		return "the Docker socket exists but isn't responding — is the Docker daemon running?"
	case "api_error":
		return "reached the Docker socket but the API call failed — check the daemon log."
	}
	return ""
}

// DockerContainer is the slim view the Monitor UI renders.
type DockerContainer struct {
	ID      string  `json:"id"`
	Name    string  `json:"name"`
	Image   string  `json:"image"`
	State   string  `json:"state"`
	Status  string  `json:"status"`
	CPUPct  float64 `json:"cpu_pct"`
	MemUsed uint64  `json:"mem_used"`
	MemPct  float64 `json:"mem_pct"`
}

// Phase 77.hotfix (2.1.3): single package-level http.Client shared across every
// poll. Prior versions constructed a fresh Client + Transport per call — every
// dropped Transport leaked its keep-alive idle pool, so a 34-container host
// polled every ~2.5s leaked ~14 conns/sec into dockerd (fd numbers climbed
// monotonically, dockerd RSS grew to 8 GB in ~16 h). Reusing one Transport
// keeps the idle pool bounded and reusable.
var (
	dockerClientOnce sync.Once
	dockerClientPtr  *http.Client
)

// dockerHTTP returns the shared http.Client that talks to the Docker unix
// socket (no heavy SDK dependency — keeps the daemon tiny). A single Client
// with a single Transport is used across all polls so idle keep-alive conns
// are pooled and reused instead of leaked (see 2.1.3 hotfix note above).
func dockerHTTP() *http.Client {
	dockerClientOnce.Do(func() {
		transport := &http.Transport{
			// Small pool: we only ever talk to the local unix socket. Cap
			// idle conns so bursty polls (list + N concurrent stats) can't
			// grow the pool unboundedly, and expire idle conns quickly.
			MaxIdleConns:        4,
			MaxIdleConnsPerHost: 4,
			IdleConnTimeout:     30 * time.Second,
			// Resolve the socket path on every dial so the client survives a
			// Docker restart or an $XDG_RUNTIME_DIR change without needing
			// to rebuild the Client. os.Stat calls are cheap.
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				var d net.Dialer
				return d.DialContext(ctx, "unix", dockerSockPath())
			},
		}
		dockerClientPtr = &http.Client{
			Timeout:   4 * time.Second,
			Transport: transport,
		}
	})
	return dockerClientPtr
}

// dockerListEntry is the subset of `GET /containers/json` we decode.
type dockerListEntry struct {
	Id     string   `json:"Id"`
	Names  []string `json:"Names"`
	Image  string   `json:"Image"`
	State  string   `json:"State"`
	Status string   `json:"Status"`
}

func dockerList() ([]DockerContainer, error) {
	resp, err := dockerHTTP().Get("http://d/containers/json?all=1")
	if err != nil {
		return nil, err
	}
	// Drain before Close so the keep-alive conn can be pooled and reused;
	// an un-drained body forces the transport to discard the conn (2.1.3).
	defer func() {
		_, _ = io.Copy(io.Discard, resp.Body)
		_ = resp.Body.Close()
	}()
	if resp.StatusCode != 200 {
		return nil, fmt.Errorf("docker list: %d", resp.StatusCode)
	}
	var raw []dockerListEntry
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		return nil, err
	}
	// Build the base list, then fetch per-container stats CONCURRENTLY. The
	// old sequential loop cost up to (4s timeout × running-containers), which
	// could push a /current sample past the desktop's HTTP deadline; in
	// parallel the worst case is a single stat's timeout (Phase 72.3).
	out := make([]DockerContainer, len(raw))
	var wg sync.WaitGroup
	for i, r := range raw {
		name := ""
		if len(r.Names) > 0 {
			name = strings.TrimPrefix(r.Names[0], "/")
		}
		out[i] = DockerContainer{
			ID: shortID(r.Id), Name: name, Image: r.Image,
			State: r.State, Status: r.Status,
		}
		if r.State == "running" {
			wg.Add(1)
			go func(i int, id string) {
				defer wg.Done()
				if cpu, mu, mp, ok := dockerStats(id); ok {
					out[i].CPUPct, out[i].MemUsed, out[i].MemPct = cpu, mu, mp
				}
			}(i, r.Id)
		}
	}
	wg.Wait()
	evictCPUPrev(raw)
	return out, nil
}

// cpuPrev is the previous CPU counters of one container, kept so CPU% can be
// computed from OUR delta between polls (see dockerStats).
type cpuPrev struct {
	total, system uint64
}

// dockerCPUPrev: container id → cpuPrev. Package-level (not on the Sampler)
// because dockerList is also called live by the /docker endpoint and both
// callers must share the same baseline.
var dockerCPUPrev sync.Map

// evictCPUPrev drops baselines for containers that no longer exist.
func evictCPUPrev(listed []dockerListEntry) {
	alive := make(map[string]bool, len(listed))
	for _, r := range listed {
		alive[r.Id] = true
	}
	dockerCPUPrev.Range(func(k, _ any) bool {
		if id, _ := k.(string); !alive[id] {
			dockerCPUPrev.Delete(k)
		}
		return true
	})
}

// dockerStats fetches a one-shot stats sample and computes CPU% the same way
// `docker stats` does. Phase 86: `one-shot=true` — without it dockerd sleeps
// ~1-2s per call to fill `precpu_stats` itself (measured: 2.8s of a 3.1s
// sample on a 23-container host). In one-shot mode precpu_stats is empty, so
// the delta comes from the counters we saw on the previous poll; the first
// sample for a container reports 0%.
func dockerStats(id string) (cpuPct float64, memUsed uint64, memPct float64, ok bool) {
	resp, err := dockerHTTP().Get("http://d/containers/" + id + "/stats?stream=false&one-shot=true")
	if err != nil {
		return 0, 0, 0, false
	}
	// Drain + Close so the transport can reuse this keep-alive conn on the
	// next stats poll (this is the hot path — N calls per list cycle).
	// Un-drained bodies force the transport to discard the conn, which was
	// the core leak fixed in 2.1.3.
	defer func() {
		_, _ = io.Copy(io.Discard, resp.Body)
		_ = resp.Body.Close()
	}()
	var s struct {
		CPUStats struct {
			CPUUsage    struct{ TotalUsage uint64 `json:"total_usage"` } `json:"cpu_usage"`
			SystemUsage uint64 `json:"system_cpu_usage"`
			OnlineCPUs  uint64 `json:"online_cpus"`
		} `json:"cpu_stats"`
		MemoryStats struct {
			Usage uint64 `json:"usage"`
			Limit uint64 `json:"limit"`
		} `json:"memory_stats"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&s); err != nil {
		return 0, 0, 0, false
	}
	cur := cpuPrev{total: s.CPUStats.CPUUsage.TotalUsage, system: s.CPUStats.SystemUsage}
	if prev, had := dockerCPUPrev.Swap(id, cur); had {
		cpuPct = cpuPctFromDelta(prev.(cpuPrev), cur, s.CPUStats.OnlineCPUs)
	}
	memUsed = s.MemoryStats.Usage
	if s.MemoryStats.Limit > 0 {
		memPct = round1(float64(memUsed) / float64(s.MemoryStats.Limit) * 100)
	}
	return cpuPct, memUsed, memPct, true
}

func dockerAction(id, cmd string) error {
	switch cmd {
	case "start", "stop", "restart", "kill":
	default:
		return fmt.Errorf("bad cmd %q (want start|stop|restart|kill)", cmd)
	}
	resp, err := dockerHTTP().Post("http://d/containers/"+id+"/"+cmd, "application/json", nil)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode >= 300 {
		return fmt.Errorf("docker %s: HTTP %d", cmd, resp.StatusCode)
	}
	return nil
}

// cpuPctFromDelta is the `docker stats` formula over two consecutive
// readings: cpuDelta/systemDelta*onlineCPUs*100. Pure — unit-tested. A
// counter that went backwards (container restarted under the same id) yields 0.
func cpuPctFromDelta(prev, cur cpuPrev, onlineCPUs uint64) float64 {
	if cur.total <= prev.total || cur.system <= prev.system {
		return 0
	}
	cpus := float64(onlineCPUs)
	if cpus == 0 {
		cpus = 1
	}
	return round1(float64(cur.total-prev.total) / float64(cur.system-prev.system) * cpus * 100)
}

func shortID(id string) string {
	if len(id) > 12 {
		return id[:12]
	}
	return id
}
