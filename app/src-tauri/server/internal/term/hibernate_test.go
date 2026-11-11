package term

// Hibernation of idle agents (hibernate.go): the idle rule, the guards
// before a stop (child processes first), the stop itself, and the three
// resume triggers. tmux and /proc are both fakes: no test signals or spawns
// a real process.

import (
	"encoding/json"
	"fmt"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

const hibSID = "0f8fad5b-d9cb-469f-a165-70867728950e"

// writeProc writes a fake /proc/<pid> with the fields the guards read.
func writeProc(t *testing.T, root string, pid, ppid int, comm, start string, rssKB int) {
	t.Helper()
	dir := filepath.Join(root, fmt.Sprint(pid))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	// After ")": state, ppid, 17 filler fields, then the start time.
	stat := fmt.Sprintf("%d (%s) S %d %s%s 0 0\n", pid, comm, ppid, strings.Repeat("0 ", 17), start)
	files := map[string]string{
		"stat":    stat,
		"comm":    comm + "\n",
		"cmdline": "/usr/bin/" + comm + "\x00--flag\x00",
		"status":  fmt.Sprintf("Name:\t%s\nVmRSS:\t  %d kB\n", comm, rssKB),
	}
	for n, c := range files {
		if err := os.WriteFile(filepath.Join(dir, n), []byte(c), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

// hibFixture is a service with one idle claude pane ("box", pid 100).
type hibFixture struct {
	s       *Service
	r       *HookRegistry
	e       *hookEntry
	proc    string
	mu      sync.Mutex
	calls   [][]string
	dead    bool // the pane's process has exited
	signals []string
	clock   time.Time
}

func (f *hibFixture) record(args []string) {
	f.mu.Lock()
	f.calls = append(f.calls, args)
	f.mu.Unlock()
}

func (f *hibFixture) callsWith(verb string) [][]string {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out [][]string
	for _, c := range f.calls {
		if c[0] == verb {
			out = append(out, c)
		}
	}
	return out
}

func newHibFixture(t *testing.T, ctrlCExits bool) *hibFixture {
	t.Helper()
	f := &hibFixture{proc: t.TempDir(), clock: time.Unix(1_700_000_000, 0)}
	f.s, _ = testService(func(args []string) ([]byte, error) {
		f.record(args)
		switch args[0] {
		case "-V":
			return []byte("tmux 3.4\n"), nil
		case "has-session":
			return nil, nil
		case "display-message":
			dead := "0"
			f.mu.Lock()
			if f.dead {
				dead = "1"
			}
			f.mu.Unlock()
			return []byte("%5\t100\t" + dead + "\tbox\t/\n"), nil
		case "send-keys":
			if ctrlCExits {
				f.mu.Lock()
				f.dead = true
				f.mu.Unlock()
				_ = os.RemoveAll(filepath.Join(f.proc, "100"))
			}
		case "list-sessions":
			return []byte("box\t1\t0\t0\t/\n"), nil
		}
		return nil, nil
	})
	f.s.claudeBin = "/opt/claude"
	f.s.attachHooks(NewHookRegistry())
	f.r = f.s.hooks
	f.r.now = func() time.Time { return f.clock }
	f.r.hib.proc = f.proc
	f.r.hib.path = filepath.Join(t.TempDir(), "hibernated.json")
	f.r.hib.sleep = func(time.Duration) {}
	f.r.hib.signal = func(pid int, sig os.Signal) error {
		f.mu.Lock()
		f.signals = append(f.signals, fmt.Sprintf("%d:%v", pid, sig))
		f.dead = true
		f.mu.Unlock()
		return os.RemoveAll(filepath.Join(f.proc, fmt.Sprint(pid)))
	}
	if _, err := f.r.settings.put(0, json.RawMessage(`{"daemon":{"hibernate_idle_minutes":30}}`)); err != nil {
		t.Fatal(err)
	}
	writeProc(t, f.proc, 100, 50, "claude", "777", 412_000)
	writeProc(t, f.proc, 50, 1, "tmux: server", "10", 9000)

	old := f.clock.Add(-31 * time.Minute)
	f.e = &hookEntry{name: "box", token: strings.Repeat("a", 64), paneID: "term_box", policy: policyNone}
	f.e.run.ApplyHook("stop", "", old)
	f.e.run.SessionID = hibSID
	f.e.lastHook = old
	f.r.add(f.e)
	return f
}

func TestIdleRule(t *testing.T) {
	now := time.Unix(1_700_000_000, 0)
	idle := 30 * time.Minute
	mk := func(sub, nt string, stateAgo, hookAgo time.Duration) *hookEntry {
		e := &hookEntry{}
		e.run.ApplyHook(sub, nt, now.Add(-stateAgo))
		e.lastHook = now.Add(-hookAgo)
		return e
	}
	cases := []struct {
		name     string
		e        *hookEntry
		attached int
		pending  bool
		idle     time.Duration
		want     bool
	}{
		{"done, idle long enough", mk("stop", "", 31*time.Minute, 31*time.Minute), 0, false, idle, true},
		{"needs-input, idle long enough", mk("notification", "idle_prompt", 40*time.Minute, 35*time.Minute), 0, false, idle, true},
		{"working is never idle", mk("pre-tool-use", "", 2*time.Hour, 2*time.Hour), 0, false, idle, false},
		{"failed is not idle", mk("stop-failure", "", 2*time.Hour, 2*time.Hour), 0, false, idle, false},
		{"unknown is not idle", &hookEntry{lastHook: now.Add(-time.Hour)}, 0, false, idle, false},
		{"a recent hook restarts the clock", mk("stop", "", time.Hour, 5*time.Minute), 0, false, idle, false},
		{"state too recent", mk("stop", "", 10*time.Minute, time.Hour), 0, false, idle, false},
		{"a client is attached", mk("stop", "", time.Hour, time.Hour), 1, false, idle, false},
		{"a gate card waits", mk("stop", "", time.Hour, time.Hour), 0, true, idle, false},
		{"setting off", mk("stop", "", time.Hour, time.Hour), 0, false, 0, false},
	}
	for _, c := range cases {
		if got := idleEnough(c.e, now, c.idle, c.attached, c.pending); got != c.want {
			t.Errorf("%s: idleEnough = %v, want %v", c.name, got, c.want)
		}
	}
	turn := mk("stop", "", time.Hour, time.Hour)
	turn.run.TurnStartedAt = now.Add(-time.Hour)
	if idleEnough(turn, now, idle, 0, false) {
		t.Error("a turn in flight must never be idle")
	}
	hib := mk("stop", "", time.Hour, time.Hour)
	hib.hib = &hibernation{}
	if idleEnough(hib, now, idle, 0, false) {
		t.Error("a hibernated pane is not a candidate again")
	}
}

func TestHibernateSettingFailsToOff(t *testing.T) {
	for doc, want := range map[string]int{
		`{}`: 0, `null`: 0, `{"daemon":{"hibernate_idle_minutes":30}}`: 30,
		`{"daemon":{"hibernate_idle_minutes":0}}`: 0, `{"daemon":{"hibernate_idle_minutes":-5}}`: 0,
		`{"daemon":{"hibernate_idle_minutes":"30"}}`: 0, `{"daemon":{"hibernate_idle_minutes":2.5}}`: 0,
		`{"daemon":{"hibernate_idle_minutes":99999999}}`: hibMaxMinutes,
	} {
		if got := parseHookRules(json.RawMessage(doc)).hibernateMinutes; got != want {
			t.Errorf("%s → %d, want %d", doc, got, want)
		}
	}
}

func TestProcGuards(t *testing.T) {
	root := t.TempDir()
	writeProc(t, root, 100, 50, "claude", "777", 412_000)
	writeProc(t, root, 101, 100, "node", "800", 1)
	writeProc(t, root, 102, 100, "bash", "801", 1)
	writeProc(t, root, 103, 1, "claude", "802", 1)
	writeProc(t, root, 104, 50, "bash", "803", 1)
	if n := procChildren(root, 100); n != 2 {
		t.Errorf("children of 100 = %d, want 2", n)
	}
	if n := procChildren(root, 103); n != 0 {
		t.Errorf("children of 103 = %d, want 0", n)
	}
	if !procIsClaude(root, 100) || procIsClaude(root, 104) {
		t.Error("procIsClaude")
	}
	if procRSSKB(root, 100) != 412_000 {
		t.Errorf("rss = %d", procRSSKB(root, 100))
	}
	if !procAlive(root, 100, "777") || procAlive(root, 100, "778") || procAlive(root, 999, "777") {
		t.Error("procAlive must match the pid AND its start time")
	}
}

func TestHibernateSkipsAClaudeWithChildren(t *testing.T) {
	f := newHibFixture(t, true)
	writeProc(t, f.proc, 200, 100, "node", "900", 1) // an MCP server, a background job
	_, sleep := f.r.hibernateScan([]Session{{Name: "box"}}, 30*time.Minute)
	if len(sleep) != 1 {
		t.Fatalf("the idle pane must be a candidate: %v", sleep)
	}
	f.r.hibernate("term_box")
	if len(f.callsWith("set-option")) != 0 || len(f.callsWith("send-keys")) != 0 || len(f.signals) != 0 {
		t.Fatalf("a claude with a child must not be touched: calls %v signals %v", f.calls, f.signals)
	}
	if f.e.hib != nil || f.e.hibSkip != "claude-has-children" {
		t.Errorf("hib=%v skip=%q", f.e.hib, f.e.hibSkip)
	}
	if _, sleep := f.r.hibernateScan([]Session{{Name: "box"}}, 30*time.Minute); len(sleep) != 0 {
		t.Error("a skipped pane waits hibSkipRetry before the next check")
	}
}

func TestHibernateSkipsWithoutASessionIDOrUnderAShell(t *testing.T) {
	f := newHibFixture(t, true)
	f.e.run.SessionID = ""
	f.r.hibernate("term_box")
	if f.e.hibSkip != "no-claude-session-id" || len(f.callsWith("set-option")) != 0 {
		t.Errorf("no session id: skip=%q calls=%v", f.e.hibSkip, f.calls)
	}

	g := newHibFixture(t, true)
	writeProc(t, g.proc, 100, 50, "bash", "777", 1) // the pane runs a shell
	g.r.hibernate("term_box")
	if g.e.hibSkip != "pane-process-is-not-claude" || len(g.callsWith("send-keys")) != 0 {
		t.Errorf("shell pane: skip=%q calls=%v", g.e.hibSkip, g.calls)
	}
}

func TestHibernateStopsWithCtrlCAndPersists(t *testing.T) {
	f := newHibFixture(t, true)
	f.r.hibernate("term_box")
	if f.e.hib == nil {
		t.Fatalf("not hibernated; skip=%q calls=%v", f.e.hibSkip, f.calls)
	}
	if got := f.callsWith("set-option"); len(got) != 1 || strings.Join(got[0], " ") != "set-option -p -t %5 remain-on-exit on" {
		t.Errorf("remain-on-exit = %v", got)
	}
	if got := f.callsWith("send-keys"); len(got) != 1 || strings.Join(got[0], " ") != "send-keys -t %5 -H 03" {
		t.Errorf("ctrl-c = %v", got)
	}
	if len(f.signals) != 0 {
		t.Errorf("Ctrl-C was enough; no signal expected: %v", f.signals)
	}
	if f.e.hib.RSSKB != 412_000 || f.e.hib.TmuxPane != "%5" || f.e.hib.ClaudeSessionID != hibSID {
		t.Errorf("record = %+v", *f.e.hib)
	}
	b, err := os.ReadFile(f.r.hib.path)
	if err != nil || !strings.Contains(string(b), `"term_box"`) || !strings.Contains(string(b), hibSID) {
		t.Errorf("persisted = %s %v", b, err)
	}
	snap := f.r.Snapshot()["term_box"]
	if !snap.Hibernated {
		t.Error("Snapshot must show the pane as hibernated")
	}
	if !f.r.hello("en").Panes["term_box"].Hibernated {
		t.Error("hello must show the pane as hibernated")
	}
}

func TestHibernateFallsBackToSignalsOnTheClaudePidOnly(t *testing.T) {
	f := newHibFixture(t, false) // Ctrl-C does nothing
	f.r.hibernate("term_box")
	if f.e.hib == nil {
		t.Fatalf("not hibernated; calls=%v", f.calls)
	}
	if len(f.callsWith("send-keys")) != hibCtrlCPresses {
		t.Errorf("presses = %d", len(f.callsWith("send-keys")))
	}
	if len(f.signals) != 1 || f.signals[0] != "100:terminated" {
		t.Errorf("signals = %v, want SIGTERM to 100 only", f.signals)
	}
}

func TestHibernateAbortsWhenAHookArrives(t *testing.T) {
	f := newHibFixture(t, false)
	pressed := 0
	f.r.hib.sleep = func(time.Duration) {
		pressed++
		if pressed == 1 { // a hook lands during the first wait
			f.r.mu.Lock()
			f.e.lastHook = f.clock
			f.r.mu.Unlock()
		}
	}
	f.r.hibernate("term_box")
	if f.e.hib != nil || len(f.signals) != 0 {
		t.Fatalf("an active pane must not be stopped: hib=%v signals=%v", f.e.hib, f.signals)
	}
	if got := f.callsWith("set-option"); len(got) != 2 || strings.Join(got[1], " ") != "set-option -p -u -t %5 remain-on-exit" {
		t.Errorf("remain-on-exit must be undone: %v", got)
	}
}

// hibernated puts the fixture in the state after a stop.
func (f *hibFixture) hibernated() {
	f.mu.Lock()
	f.dead = true
	f.mu.Unlock()
	f.r.mu.Lock()
	f.e.hib = &hibernation{PaneID: "term_box", Session: "box", TmuxPane: "%5", ClaudeSessionID: hibSID,
		Cwd: "/", SinceMs: f.clock.UnixMilli()}
	f.r.mu.Unlock()
}

func (f *hibFixture) assertResumed(t *testing.T) {
	t.Helper()
	got := f.callsWith("respawn-pane")
	want := "respawn-pane -t %5 -c / -- /opt/claude --resume " + hibSID
	if len(got) != 1 || strings.Join(got[0], " ") != want {
		t.Fatalf("respawn = %v, want %q", got, want)
	}
	if f.e.hib != nil {
		t.Error("the record must be cleared")
	}
	if len(f.callsWith("kill-session")) != 0 || len(f.signals) != 0 {
		t.Error("a resume must never kill anything")
	}
}

func TestResumeOnAttach(t *testing.T) {
	f := newHibFixture(t, true)
	f.hibernated()
	req := httptest.NewRequest("GET", "/api/v2/term/sessions/box/attach", nil)
	req.SetPathValue("name", "box")
	f.s.handleAttach(httptest.NewRecorder(), req) // not a websocket: the upgrade fails after the resume
	f.assertResumed(t)
}

func TestResumeOnSend(t *testing.T) {
	f := newHibFixture(t, true)
	f.hibernated()
	res, err := call(t, matched(t, f.r, f.e), "send", map[string]any{"data": "hi"})
	if err != nil || asMap(t, res)["resumed"] != true {
		t.Fatalf("send → %v %+v", res, err)
	}
	f.assertResumed(t)
	keys := f.callsWith("send-keys")
	if len(keys) != 1 || keys[0][2] != "=box:" {
		t.Errorf("the bytes go after the resume: %v", keys)
	}
}

func TestResumeWhenATmuxClientAttaches(t *testing.T) {
	f := newHibFixture(t, true)
	f.hibernated()
	if wake, _ := f.r.hibernateScan([]Session{{Name: "box", Attached: 0}}, 0); len(wake) != 0 {
		t.Fatalf("no client: no wake, got %v", wake)
	}
	wake, sleep := f.r.hibernateScan([]Session{{Name: "box", Attached: 1}}, 30*time.Minute)
	if len(wake) != 1 || wake[0] != "term_box" || len(sleep) != 0 {
		t.Fatalf("scan = %v %v", wake, sleep)
	}
	if ok, err := f.r.wake("term_box", "attached"); !ok || err != nil {
		t.Fatalf("wake = %v %v", ok, err)
	}
	f.assertResumed(t)
}

func TestResumeDropsAStaleRecordAndTouchesNothing(t *testing.T) {
	f := newHibFixture(t, true)
	f.hibernated()
	f.mu.Lock()
	f.dead = false // someone respawned the pane by hand
	f.mu.Unlock()
	if ok, _ := f.r.wake("term_box", "attach"); ok {
		t.Fatal("a live pane must not be respawned")
	}
	if len(f.callsWith("respawn-pane")) != 0 || f.e.hib != nil {
		t.Errorf("respawn=%v hib=%v", f.callsWith("respawn-pane"), f.e.hib)
	}
}

func TestLoadHibernatedAfterRestart(t *testing.T) {
	f := newHibFixture(t, true)
	f.hibernated()
	f.r.persistHibernated()
	path := f.r.hib.path

	g := newHibFixture(t, true)
	g.r.hib.path = path
	g.mu.Lock()
	g.dead = true
	g.mu.Unlock()
	g.s.LoadHibernated()
	if g.e.hib == nil || g.e.hib.ClaudeSessionID != hibSID {
		t.Fatalf("record not restored: %+v", g.e.hib)
	}
}
