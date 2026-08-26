package insights

// Phase 76: server-side process hygiene. Detects the two leaks Yossi hit —
// duplicate `ymux port-watch` processes (should be exactly one per
// workspace) and orphaned long-running `claude` sessions with no terminal —
// and can reap the safe ones on request. The desktop Monitor surfaces this in
// a "Cleanup" tab. Uses gopsutil (already a dep) so it stays cross-platform
// for `go test` on the dev box.

import (
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/shirou/gopsutil/v4/process"
)

// PortWatcher is one `ymux port-watch --workspace X` process. Duplicate is
// true for every instance of a workspace EXCEPT the newest (smallest etime) —
// those are the safe-to-kill leaks. Orphan is true when the process has been
// reparented to init (ppid 1) for more than a minute: the SSH channel that
// spawned it is gone, so nobody will ever read its output (Phase 86 — 15 of
// these were found on a real server, each living forever because the reaper
// only handled duplicates).
type PortWatcher struct {
	PID        int32   `json:"pid"`
	Workspace  string  `json:"workspace"`
	EtimeSec   int64   `json:"etime_sec"`
	CPUTimeSec float64 `json:"cpu_time_sec"`
	Duplicate  bool    `json:"duplicate"`
	Orphan     bool    `json:"orphan"`
}

// OrphanSession is a `claude` process that LOOKS abandoned: no controlling
// terminal, a real --session-id/--resume, and alive beyond the threshold.
// Reported for the user to decide — never auto-killed (it could be a
// deliberate long-running background workflow).
type OrphanSession struct {
	PID       int32   `json:"pid"`
	SessionID string  `json:"session_id"`
	Resume    string  `json:"resume"`
	EtimeSec  int64   `json:"etime_sec"`
	CPUPct    float64 `json:"cpu_pct"`
}

type Hygiene struct {
	PortWatchers   []PortWatcher   `json:"port_watchers"`
	DuplicateCount int             `json:"duplicate_count"`
	OrphanSessions []OrphanSession `json:"orphan_sessions"`
}

// orphanEtimeThreshold: a claude with no tty older than this is flagged.
const orphanEtimeThreshold int64 = 24 * 3600

// orphanWatcherEtime: a port-watcher under init older than this is an orphan.
// The grace period covers the brief window where a freshly spawned watcher is
// still being adopted by its SSH session.
const orphanWatcherEtime int64 = 60

// isOrphan classifies a port-watcher by parent pid + age. Pure — unit-tested.
func isOrphan(ppid int32, etimeSec int64) bool {
	return ppid == 1 && etimeSec > orphanWatcherEtime
}

// argAfter returns the token following `flag` in an argv slice ("" if absent).
func argAfter(args []string, flag string) string {
	for i := 0; i+1 < len(args); i++ {
		if args[i] == flag {
			return args[i+1]
		}
	}
	return ""
}

// markDuplicates flags every port-watcher for a workspace except the newest
// (smallest etime) and returns how many were flagged. Pure — unit-tested.
func markDuplicates(ws []PortWatcher) int {
	byWs := map[string][]int{}
	for i := range ws {
		byWs[ws[i].Workspace] = append(byWs[ws[i].Workspace], i)
	}
	dups := 0
	for _, idxs := range byWs {
		if len(idxs) <= 1 {
			continue
		}
		sort.Slice(idxs, func(a, b int) bool {
			return ws[idxs[a]].EtimeSec < ws[idxs[b]].EtimeSec // newest first
		})
		for _, k := range idxs[1:] {
			ws[k].Duplicate = true
			dups++
		}
	}
	return dups
}

// collectHygiene walks the process table once and classifies port-watchers +
// orphan claude sessions.
func collectHygiene() Hygiene {
	procs, err := process.Processes()
	if err != nil {
		return Hygiene{}
	}
	nowUnix := time.Now().Unix()
	// Non-nil so empty results marshal as JSON [] (not null) — a nil slice
	// would crash the desktop's `.length` / <For>.
	watchers := []PortWatcher{}
	orphans := []OrphanSession{}
	for _, p := range procs {
		args, err := p.CmdlineSlice()
		if err != nil || len(args) == 0 {
			continue
		}
		joined := strings.Join(args, " ")
		etime := int64(0)
		if ct, err := p.CreateTime(); err == nil && ct > 0 {
			etime = nowUnix - ct/1000
		}
		switch {
		case strings.Contains(joined, "ymux port-watch"):
			cpu := 0.0
			if t, err := p.Times(); err == nil {
				cpu = t.User + t.System
			}
			ppid, _ := p.Ppid() // 0 on error → never an orphan
			watchers = append(watchers, PortWatcher{
				PID:        p.Pid,
				Workspace:  argAfter(args, "--workspace"),
				EtimeSec:   etime,
				CPUTimeSec: round1(cpu),
				Orphan:     isOrphan(ppid, etime),
			})
		case strings.Contains(joined, "claude"):
			sid := argAfter(args, "--session-id")
			resume := argAfter(args, "--resume")
			tty, _ := p.Terminal()
			if (sid != "" || resume != "") && tty == "" && etime > orphanEtimeThreshold {
				pct, _ := p.CPUPercent()
				orphans = append(orphans, OrphanSession{
					PID:       p.Pid,
					SessionID: sid,
					Resume:    resume,
					EtimeSec:  etime,
					CPUPct:    round1(pct),
				})
			}
		}
	}
	dupCount := markDuplicates(watchers)
	return Hygiene{PortWatchers: watchers, DuplicateCount: dupCount, OrphanSessions: orphans}
}

// reapable is the auto-kill / user-kill policy for a port-watcher: a
// duplicate (a newer one exists for the workspace) or an orphan (parent is
// init, SSH channel long gone). Exactly one live watcher per connected
// workspace is ever correct, so both are safe unattended.
func reapable(w PortWatcher) bool { return w.Duplicate || w.Orphan }

// autoReap SIGTERMs duplicate and orphaned port-watchers automatically. It
// NEVER touches claude sessions (those are report-only; the user may keep a
// long-running loop alive and kills them manually from the Cleanup tab).
// Returns how many were reaped. Kills by PID, never `pkill -f`.
func autoReap() int {
	h := collectHygiene()
	var pids []int32
	reasons := map[int32]string{}
	for _, w := range h.PortWatchers {
		if !reapable(w) {
			continue
		}
		pids = append(pids, w.PID)
		switch {
		case w.Duplicate && w.Orphan:
			reasons[w.PID] = "duplicate+orphan"
		case w.Duplicate:
			reasons[w.PID] = "duplicate"
		default:
			reasons[w.PID] = "orphan"
		}
	}
	if len(pids) == 0 {
		return 0
	}
	killed := killPids(pids)
	if len(killed) > 0 {
		why := make([]string, 0, len(killed))
		for _, pid := range killed {
			why = append(why, fmt.Sprintf("%d=%s", pid, reasons[pid]))
		}
		logger.Info("hygiene auto-reaped port-watchers", "count", len(killed), "pids", killed, "reasons", strings.Join(why, ","))
	}
	return len(killed)
}

// PortWatchReaper auto-reaps leaked port-watchers every 5 minutes: duplicates
// (more than one per workspace) and orphans (reparented to init because the
// SSH channel that spawned them died without the desktop reconnecting). Only
// port-watchers — claude sessions are never auto-killed.
func PortWatchReaper(stop <-chan struct{}) {
	autoReap() // once at boot
	t := time.NewTicker(5 * time.Minute)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
			autoReap()
		}
	}
}

// killPids terminates (SIGTERM) only PIDs that are currently classified as a
// duplicate/orphan port-watcher or an orphan session — a caller can't ask us
// to kill an arbitrary process. Returns the PIDs actually signalled.
func killPids(requested []int32) []int32 {
	h := collectHygiene()
	allowed := map[int32]bool{}
	for _, w := range h.PortWatchers {
		if reapable(w) {
			allowed[w.PID] = true
		}
	}
	for _, o := range h.OrphanSessions {
		allowed[o.PID] = true
	}
	killed := []int32{}
	for _, pid := range requested {
		if !allowed[pid] {
			continue
		}
		if p, err := process.NewProcess(pid); err == nil {
			if p.Terminate() == nil {
				killed = append(killed, pid)
			}
		}
	}
	return killed
}
