package term

// hibernate.go — stop an idle claude and resume it in the same pane
// (Orca idea #8, 2.22.0).
//
// The goal is RAM on a crowded server. The feature is OFF by default. The
// settings document's `daemon.hibernate_idle_minutes` turns it on (0 = off).
//
// A pane is a candidate when all of these are true:
//   - the session is one of ours (hookEntry), not released, not hibernated;
//   - the agent state is done or needs-input, no turn is in flight, and both
//     the state and the last hook are older than N minutes;
//   - no tmux client is attached and no gate card waits for a decision.
//
// Before the stop, more guards apply. A failed guard skips the pane and logs
// the reason once (not on every tick):
//   - a known claude_session_id (a UUID), from the hooks or session-meta;
//   - the pane's own process IS claude. When claude runs under a shell, a
//     resume would have to type a command into that shell, so we skip it;
//   - claude has no child process (a background job, an MCP server).
//
// The stop sets `remain-on-exit` on the pane, so the pane stays when claude
// exits. Then it presses Ctrl-C, as a user does to exit claude. After a
// timeout it sends SIGTERM, then SIGKILL — each one to the claude pid only,
// after a check that the pid is still the same claude. Nothing else is ever
// signalled.
//
// The resume is `respawn-pane` on the dead pane with the argv that
// handleResume uses (`claude --resume <id>`, resumeArgv). Without -k, tmux
// refuses a pane that is alive, so a resume can never kill anything. Three
// triggers call it: an attach through the daemon (handleAttach), an agent
// `send` to the pane (agentverbs.go), and the tick, when tmux reports an
// attached client (a desktop or a plain `tmux attach`).
//
// State: hookEntry.hib, persisted in <data dir>/hibernated.json (tmp + fsync
// + rename) so a restarted daemon still resumes the pane.
//
// Rule #1: logs carry the pane id, the session, the RSS and the reason.

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"ymux-server/internal/agent"
)

// hibernation is one hibernated pane. It is the persisted record too.
type hibernation struct {
	PaneID          string `json:"pane_id"`   // the hook pane id (stable over renames)
	Session         string `json:"session"`   // the tmux session name at the stop
	TmuxPane        string `json:"tmux_pane"` // tmux's own pane id, "%N"
	ClaudeSessionID string `json:"claude_session_id"`
	Cwd             string `json:"cwd"`
	SinceMs         int64  `json:"since_ms"`
	RSSKB           int64  `json:"rss_kb"`
}

const (
	// hibernateTick is how often the idle rule and the attach trigger run.
	hibernateTick = 10 * time.Second
	// The stop sequence: Ctrl-C presses, then SIGTERM, then SIGKILL.
	hibCtrlCPresses = 3
	hibPressGap     = time.Second
	hibGraceTimeout = 10 * time.Second
	hibTermTimeout  = 5 * time.Second
	hibKillTimeout  = 2 * time.Second
	// hibSkipRetry is how long a pane that failed a guard waits for the next check.
	hibSkipRetry = 5 * time.Minute
	// hibMaxMinutes clamps the setting to one week.
	hibMaxMinutes = 7 * 24 * 60
)

// tmuxPaneID is tmux's global pane id. It is the only target the stop and the
// resume use, so a renamed session or a changed current pane is not a risk.
var tmuxPaneID = regexp.MustCompile(`^%[0-9]+$`)

// hibernator holds what the stop and the resume need besides the registry.
type hibernator struct {
	mu    sync.Mutex             // guards locks and the file write
	locks map[string]*sync.Mutex // one per hook pane id: one stop or resume at a time
	path  string                 // "" = memory only
	proc  string                 // the procfs root ("/proc"); tests use a temp dir
	home  string                 // for session-meta's claude_session_id
	bin   func() string          // claude's path (Service.claudeBinary)
	sleep func(time.Duration)
	// signal sends sig to pid. os.Process, not syscall.Kill, so the package
	// still builds on a mac or Windows dev box.
	signal func(pid int, sig os.Signal) error
}

func newHibernator() *hibernator {
	return &hibernator{locks: map[string]*sync.Mutex{}, proc: "/proc",
		bin: func() string { return "claude" }, sleep: time.Sleep, signal: signalPid}
}

func signalPid(pid int, sig os.Signal) error {
	if pid <= 1 {
		return errors.New("refusing to signal pid <= 1")
	}
	p, err := os.FindProcess(pid)
	if err != nil {
		return err
	}
	return p.Signal(sig)
}

func (h *hibernator) lock(paneID string) *sync.Mutex {
	h.mu.Lock()
	defer h.mu.Unlock()
	l, ok := h.locks[paneID]
	if !ok {
		l = &sync.Mutex{}
		h.locks[paneID] = l
	}
	return l
}

// resumeArgv is the argv that continues a Claude conversation. handleResume
// (a history row) and the hibernation resume (the same pane) both use it.
func resumeArgv(bin, claudeSessionID string) []string {
	return []string{bin, "--resume", claudeSessionID}
}

// ── the idle rule ──────────────────────────────────────────────────────

// idleEnough is the idle rule, as a pure function of one entry. attached is
// tmux's client count for the session; pending is whether a gate card waits.
func idleEnough(e *hookEntry, now time.Time, idle time.Duration, attached int, pending bool) bool {
	if idle <= 0 || e.released || e.hib != nil || e.hibBusy || attached > 0 || pending {
		return false
	}
	switch e.run.CurrentState() {
	case agent.StateDone, agent.StateNeedsInput:
	default:
		return false // never running, failed or unknown
	}
	if !e.run.TurnStartedAt.IsZero() || e.run.StateSince.IsZero() || e.lastHook.IsZero() {
		return false
	}
	return now.Sub(e.run.StateSince) >= idle && now.Sub(e.lastHook) >= idle
}

// touch records hook activity on an entry: the idle clock restarts.
func (r *HookRegistry) touch(e *hookEntry) {
	r.mu.Lock()
	e.lastHook = r.now()
	r.mu.Unlock()
}

// ── procfs ─────────────────────────────────────────────────────────────

// procStat returns the fields of /proc/<pid>/stat after the comm field: [0]
// is the state, [1] the ppid, [19] the start time. comm can hold spaces and
// parentheses, so the split is at the LAST ")".
func procStat(root string, pid int) ([]string, bool) {
	b, err := os.ReadFile(filepath.Join(root, strconv.Itoa(pid), "stat"))
	if err != nil {
		return nil, false
	}
	i := strings.LastIndexByte(string(b), ')')
	if i < 0 {
		return nil, false
	}
	f := strings.Fields(string(b[i+1:]))
	if len(f) < 20 {
		return nil, false
	}
	return f, true
}

// procStart is the process start time. With the pid, it names one process:
// a reused pid has a different start time.
func procStart(root string, pid int) string {
	if f, ok := procStat(root, pid); ok {
		return f[19]
	}
	return ""
}

// procAlive reports whether pid is still the process that started at start.
// A zombie is not alive: it has exited and waits for tmux to reap it.
func procAlive(root string, pid int, start string) bool {
	f, ok := procStat(root, pid)
	return ok && f[19] == start && f[0] != "Z" && f[0] != "X"
}

// procIsClaude reports whether pid is a claude process: its comm (node sets
// it from process.title) or the base name of its argv[0].
func procIsClaude(root string, pid int) bool {
	if b, err := os.ReadFile(filepath.Join(root, strconv.Itoa(pid), "comm")); err == nil &&
		strings.TrimSpace(string(b)) == "claude" {
		return true
	}
	b, err := os.ReadFile(filepath.Join(root, strconv.Itoa(pid), "cmdline"))
	if err != nil {
		return false
	}
	argv0, _, _ := strings.Cut(string(b), "\x00")
	return filepath.Base(argv0) == "claude"
}

// procChildren counts the processes whose parent is pid.
func procChildren(root string, pid int) int {
	ents, err := os.ReadDir(root)
	if err != nil {
		return 0
	}
	want := strconv.Itoa(pid)
	n := 0
	for _, d := range ents {
		c, err := strconv.Atoi(d.Name())
		if err != nil || c == pid {
			continue
		}
		if f, ok := procStat(root, c); ok && f[1] == want {
			n++
		}
	}
	return n
}

// procRSSKB is VmRSS in kB, 0 when unknown.
func procRSSKB(root string, pid int) int64 {
	b, err := os.ReadFile(filepath.Join(root, strconv.Itoa(pid), "status"))
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(b), "\n") {
		if v, ok := strings.CutPrefix(line, "VmRSS:"); ok {
			n, _ := strconv.ParseInt(strings.TrimSuffix(strings.TrimSpace(v), " kB"), 10, 64)
			return n
		}
	}
	return 0
}

// ── tmux ───────────────────────────────────────────────────────────────

// tmuxPane is one pane as `display-message` reports it.
type tmuxPane struct {
	id      string // %N
	pid     int
	dead    bool
	cwd     string
	session string
}

const paneFormat = "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{session_name}\t#{pane_current_path}"

func (t *Tmux) paneInfo(tgt string) (tmuxPane, error) {
	out, err := t.exec("display-message", "-p", "-t", tgt, paneFormat)
	if err != nil {
		return tmuxPane{}, err
	}
	f := strings.SplitN(strings.TrimRight(string(out), "\n"), "\t", 5)
	if len(f) < 5 || !tmuxPaneID.MatchString(f[0]) {
		return tmuxPane{}, errors.New("unexpected display-message output")
	}
	pid, _ := strconv.Atoi(f[1])
	return tmuxPane{id: f[0], pid: pid, dead: f[2] == "1", session: f[3], cwd: f[4]}, nil
}

// remainOnExit sets (on) or unsets the pane option that keeps a pane after
// its process exits.
func (t *Tmux) remainOnExit(pane string, on bool) error {
	if on {
		_, err := t.exec("set-option", "-p", "-t", pane, "remain-on-exit", "on")
		return err
	}
	_, err := t.exec("set-option", "-p", "-u", "-t", pane, "remain-on-exit")
	return err
}

// ctrlC presses Ctrl-C in a pane, as a user does to exit claude.
func (t *Tmux) ctrlC(pane string) error {
	_, err := t.exec("send-keys", "-t", pane, "-H", "03")
	return err
}

// respawn starts argv in a DEAD pane. No -k: tmux refuses a pane that is
// alive, so this never kills a process.
func (t *Tmux) respawn(pane, cwd string, argv ...string) error {
	args := []string{"respawn-pane", "-t", pane}
	if cwd != "" {
		args = append(args, "-c", cwd)
	}
	_, err := t.exec(append(append(args, "--"), argv...)...)
	return err
}

// ── the tick ───────────────────────────────────────────────────────────

// StartHibernation runs the idle rule and the attach trigger until ctx ends.
// Call it after RecoverHooks and LoadHibernated.
func (s *Service) StartHibernation(ctx context.Context) {
	if s.hooks == nil {
		return
	}
	go func() {
		t := time.NewTicker(hibernateTick)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				s.hooks.hibernateTick()
			}
		}
	}()
}

// hibernateTick runs once: it wakes each hibernated pane that has a client
// attached and stops each pane that passes the idle rule. The work runs in
// goroutines; each pane has its own lock.
func (r *HookRegistry) hibernateTick() {
	minutes := r.settings.rules().hibernateMinutes
	r.mu.Lock()
	anyHib := false
	for _, e := range r.byName {
		if e.hib != nil {
			anyHib = true
			break
		}
	}
	r.mu.Unlock()
	if minutes == 0 && !anyHib {
		return // off, and nothing to wake: no tmux call at all
	}
	sessions, err := r.tmux.List()
	if err != nil {
		return
	}
	wake, sleep := r.hibernateScan(sessions, time.Duration(minutes)*time.Minute)
	for _, p := range wake {
		go func() { _, _ = r.wake(p, "attached") }()
	}
	for _, p := range sleep {
		go r.hibernate(p)
	}
}

// hibernateScan splits the live entries into panes to wake and panes to
// stop. It takes no action, so the tests call it directly.
func (r *HookRegistry) hibernateScan(sessions []Session, idle time.Duration) (wake, sleep []string) {
	attached := make(map[string]int, len(sessions))
	for _, s := range sessions {
		attached[s.Name] = s.Attached
	}
	now := r.now()
	var cand []string
	r.mu.Lock()
	for _, e := range r.byName {
		n, live := attached[e.name]
		if !live || e.released || e.hibBusy {
			continue
		}
		if e.hib != nil {
			if n > 0 {
				wake = append(wake, e.paneID)
			}
			continue
		}
		if now.After(e.hibRetryAt) && idleEnough(e, now, idle, n, false) {
			cand = append(cand, e.paneID)
		}
	}
	r.mu.Unlock()
	for _, p := range cand { // the feed has its own lock: never under r.mu
		if len(r.feed.pendingFor(p)) == 0 {
			sleep = append(sleep, p)
		}
	}
	return wake, sleep
}

// entryByPane returns the live entry for a hook pane id. Caller holds r.mu.
func (r *HookRegistry) entryByPane(paneID string) *hookEntry {
	for _, e := range r.byName {
		if e.paneID == paneID && !e.released {
			return e
		}
	}
	return nil
}

// skip logs why a pane was not stopped, once per reason, and holds the pane
// back for hibSkipRetry: a guard such as a child process does not change in
// ten seconds, and each check reads all of /proc.
func (r *HookRegistry) skip(paneID, reason string, attrs ...any) {
	r.mu.Lock()
	e := r.entryByPane(paneID)
	seen := e == nil || e.hibSkip == reason
	if e != nil {
		e.hibSkip = reason
		e.hibRetryAt = r.now().Add(hibSkipRetry)
	}
	r.mu.Unlock()
	if !seen {
		logger.Info("hibernate skipped", append([]any{"pane", paneID, "reason", reason}, attrs...)...)
	}
}

// ── stop ───────────────────────────────────────────────────────────────

// hibernate stops claude in one pane, when every guard passes.
func (r *HookRegistry) hibernate(paneID string) {
	h := r.hib
	l := h.lock(paneID)
	if !l.TryLock() {
		return // a stop or a resume of this pane is in progress
	}
	defer l.Unlock()

	idle := time.Duration(r.settings.rules().hibernateMinutes) * time.Minute
	r.mu.Lock()
	e := r.entryByPane(paneID)
	if e == nil || !idleEnough(e, r.now(), idle, 0, false) {
		r.mu.Unlock()
		return
	}
	e.hibBusy = true
	name, sid, lastHook := e.name, e.run.SessionID, e.lastHook
	r.mu.Unlock()
	defer func() {
		r.mu.Lock()
		if e := r.entryByPane(paneID); e != nil {
			e.hibBusy = false
		}
		r.mu.Unlock()
	}()

	if !claudeSessionID.MatchString(sid) {
		sid = LoadMeta(h.home)[name].ClaudeSessionID
	}
	if !claudeSessionID.MatchString(sid) {
		r.skip(paneID, "no-claude-session-id")
		return
	}
	p, err := r.tmux.paneInfo(paneTarget(name))
	if err != nil || p.dead || p.session != name {
		r.skip(paneID, "pane-unavailable")
		return
	}
	if !procIsClaude(h.proc, p.pid) {
		r.skip(paneID, "pane-process-is-not-claude")
		return
	}
	if n := procChildren(h.proc, p.pid); n > 0 {
		r.skip(paneID, "claude-has-children", "children", n)
		return
	}
	start := procStart(h.proc, p.pid)
	if start == "" {
		r.skip(paneID, "pane-unavailable")
		return
	}
	rss := procRSSKB(h.proc, p.pid)
	if err := r.tmux.remainOnExit(p.id, true); err != nil {
		r.skip(paneID, "remain-on-exit-failed")
		return
	}
	how, ok := r.stopClaude(paneID, p, start, lastHook)
	if !ok {
		_ = r.tmux.remainOnExit(p.id, false)
		if how == "aborted" {
			logger.Info("hibernate aborted: the pane became active", "pane", paneID, "session", name)
		} else {
			logger.Warn("hibernate failed: claude did not exit", "pane", paneID, "session", name)
		}
		return
	}
	rec := &hibernation{PaneID: paneID, Session: name, TmuxPane: p.id, ClaudeSessionID: sid,
		Cwd: p.cwd, SinceMs: r.now().UnixMilli(), RSSKB: rss}
	r.mu.Lock()
	if e := r.entryByPane(paneID); e != nil {
		e.hib, e.hibSkip = rec, ""
	}
	r.mu.Unlock()
	r.persistHibernated()
	r.publishHibernated(rec, true)
	logger.Info("agent hibernated", "pane", paneID, "session", name, "rss_kb", rss, "stop", how)
}

// stopClaude ends the claude process: Ctrl-C presses, then SIGTERM, then
// SIGKILL. Every signal goes to pid alone, and only while pid is still the
// claude that started at start. It returns how claude ended. "aborted" means
// a hook arrived during the presses, so a person or an agent uses the pane.
func (r *HookRegistry) stopClaude(paneID string, p tmuxPane, start string, lastHook time.Time) (string, bool) {
	h := r.hib
	gone := func() bool { return !procAlive(h.proc, p.pid, start) }
	waitGone := func(d time.Duration) bool {
		for step := 100 * time.Millisecond; d > 0; d -= step {
			if gone() {
				return true
			}
			h.sleep(step)
		}
		return gone()
	}
	active := func() bool {
		r.mu.Lock()
		defer r.mu.Unlock()
		e := r.entryByPane(paneID)
		return e == nil || !e.lastHook.Equal(lastHook)
	}
	for i := 0; i < hibCtrlCPresses; i++ {
		if active() {
			return "aborted", false
		}
		if err := r.tmux.ctrlC(p.id); err != nil {
			break
		}
		if waitGone(hibPressGap) {
			return "ctrl-c", true
		}
	}
	if waitGone(hibGraceTimeout - hibCtrlCPresses*hibPressGap) {
		return "ctrl-c", true
	}
	for _, step := range []struct {
		sig  os.Signal
		name string
		wait time.Duration
	}{{syscall.SIGTERM, "sigterm", hibTermTimeout}, {syscall.SIGKILL, "sigkill", hibKillTimeout}} {
		if gone() {
			return "ctrl-c", true
		}
		if !procIsClaude(h.proc, p.pid) {
			return "", false // the pid is no longer claude: send nothing
		}
		if err := h.signal(p.pid, step.sig); err != nil {
			return "", false
		}
		if waitGone(step.wait) {
			return step.name, true
		}
	}
	return "", false
}

// ── resume ─────────────────────────────────────────────────────────────

// wakeSession resumes the pane of a tmux session, when it is hibernated.
func (r *HookRegistry) wakeSession(name, why string) {
	r.mu.Lock()
	e, ok := r.byName[name]
	pane := ""
	if ok && !e.released {
		pane = e.paneID
	}
	r.mu.Unlock()
	if pane != "" {
		_, _ = r.wake(pane, why)
	}
}

// wake resumes a hibernated pane. It waits for a stop in progress. resumed
// is false when the pane was not hibernated. On a respawn error the record
// stays, so the next trigger tries again.
func (r *HookRegistry) wake(paneID, why string) (resumed bool, err error) {
	h := r.hib
	l := h.lock(paneID)
	l.Lock()
	defer l.Unlock()

	r.mu.Lock()
	e := r.entryByPane(paneID)
	var rec *hibernation
	name := ""
	if e != nil && e.hib != nil {
		rec, name = e.hib, e.name
	}
	r.mu.Unlock()
	if rec == nil {
		return false, nil
	}
	p, perr := r.tmux.paneInfo(rec.TmuxPane)
	if perr != nil || p.session != name || !p.dead {
		// The pane is gone, moved, or alive again (a person respawned it).
		// The record is stale: drop it and touch nothing.
		r.clearHibernated(paneID, rec)
		logger.Info("hibernation record dropped: the pane changed", "pane", paneID, "session", name)
		return false, nil
	}
	cwd := rec.Cwd
	if st, serr := os.Stat(cwd); cwd == "" || serr != nil || !st.IsDir() {
		cwd = "" // claude --resume finds the conversation by id
	}
	if err := r.tmux.respawn(rec.TmuxPane, cwd, resumeArgv(h.bin(), rec.ClaudeSessionID)...); err != nil {
		logger.Warn("resume after hibernate failed", "pane", paneID, "session", name, "why", why, "err", err)
		return false, err
	}
	_ = r.tmux.remainOnExit(rec.TmuxPane, false)
	r.clearHibernated(paneID, rec)
	logger.Info("agent resumed", "pane", paneID, "session", name, "why", why,
		"hibernated_s", (r.now().UnixMilli()-rec.SinceMs)/1000)
	return true, nil
}

func (r *HookRegistry) clearHibernated(paneID string, rec *hibernation) {
	r.mu.Lock()
	if e := r.entryByPane(paneID); e != nil && e.hib == rec {
		e.hib = nil
		e.lastHook = r.now() // a fresh idle clock
	}
	r.mu.Unlock()
	r.persistHibernated()
	r.publishHibernated(rec, false)
}

// ── state for the UI, and on disk ──────────────────────────────────────

func (r *HookRegistry) publishHibernated(rec *hibernation, on bool) {
	r.hub.publish("pane:hibernated", same(map[string]any{
		"pane_id": rec.PaneID, "session": rec.Session, "hibernated": on,
		"since_ms": rec.SinceMs, "rss_kb": rec.RSSKB,
	}))
}

// persistHibernated writes every record. The file lock is taken first and
// r.mu inside it, so two writes can never land out of order.
func (r *HookRegistry) persistHibernated() {
	h := r.hib
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.path == "" {
		return
	}
	recs := map[string]hibernation{}
	r.mu.Lock()
	for _, e := range r.byName {
		if e.hib != nil {
			recs[e.paneID] = *e.hib
		}
	}
	r.mu.Unlock()
	b, err := json.Marshal(recs)
	if err == nil {
		err = writeFileAtomic(h.path, b)
	}
	if err != nil {
		logger.Warn("hibernation state not saved", "err", err)
	}
}

// LoadHibernated reads the records back after a restart. A record is kept
// only when its pane id is registered (RecoverHooks), the tmux pane still
// belongs to that session, and the pane is dead. Call it after RecoverHooks.
func (s *Service) LoadHibernated() {
	r := s.hooks
	if r == nil || r.hib.path == "" {
		return
	}
	var recs map[string]hibernation
	if !loadJSON(r.hib.path, &recs, "hibernation") {
		return
	}
	kept := 0
	for paneID, rec := range recs {
		if rec.PaneID != paneID || !tmuxPaneID.MatchString(rec.TmuxPane) || !claudeSessionID.MatchString(rec.ClaudeSessionID) {
			continue
		}
		r.mu.Lock()
		e := r.entryByPane(paneID)
		name := ""
		if e != nil {
			name = e.name
		}
		r.mu.Unlock()
		if name == "" {
			continue
		}
		if p, err := r.tmux.paneInfo(rec.TmuxPane); err != nil || p.session != name || !p.dead {
			continue
		}
		rec.Session = name
		r.mu.Lock()
		if e := r.entryByPane(paneID); e != nil {
			e.hib = &rec
			kept++
		}
		r.mu.Unlock()
	}
	r.persistHibernated()
	logger.Info("hibernation state loaded", "kept", kept, "stored", len(recs))
}

// hibernatedSince is the hello's view of one entry. Caller holds r.mu.
func hibernatedSince(e *hookEntry) *int64 {
	if e.hib == nil {
		return nil
	}
	v := e.hib.SinceMs
	return &v
}
