package term

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ymux-server/internal/agent"
)

func at(s string) time.Time {
	t, err := time.ParseInLocation("2006-01-02 15:04", s, time.UTC)
	if err != nil {
		panic(err)
	}
	return t
}

func TestCronParseRejects(t *testing.T) {
	for _, expr := range []string{
		"", "* * * *", "* * * * * *", "60 * * * *", "* 24 * * *", "* * 0 * *",
		"* * 32 * *", "* * * 13 *", "* * * * 8", "5-1 * * * *", "*/0 * * * *",
		"a * * * *", "-1 * * * *", "1,,2 * * * *", "@daily", "* * * foo *",
	} {
		if _, err := parseCron(expr); err == nil {
			t.Errorf("parseCron(%q) accepted", expr)
		}
	}
}

func TestCronMatches(t *testing.T) {
	cases := []struct {
		expr string
		when string
		want bool
	}{
		{"* * * * *", "2026-11-10 13:37", true},
		{"0 3 * * *", "2026-11-10 03:00", true},
		{"0 3 * * *", "2026-11-10 03:01", false},
		{"*/15 * * * *", "2026-11-10 10:45", true},
		{"*/15 * * * *", "2026-11-10 10:46", false},
		{"10-20/5 * * * *", "2026-11-10 10:15", true},
		{"10-20/5 * * * *", "2026-11-10 10:25", false},
		{"5/20 * * * *", "2026-11-10 10:45", true},
		{"1,2,30 * * * *", "2026-11-10 10:30", true},
		{"0 9 * * mon-fri", "2026-11-10 09:00", true},  // a Tuesday
		{"0 9 * * MON-FRI", "2026-11-15 09:00", false}, // a Sunday
		{"0 0 * * 7", "2026-11-15 00:00", true},        // 7 is Sunday
		{"0 0 * * 0", "2026-11-15 00:00", true},
		{"0 0 1 jan *", "2027-01-01 00:00", true},
		{"0 0 * NOV *", "2026-11-10 00:00", true},
		// Both day fields restricted: either one matches (Vixie rule).
		{"0 0 1 * 2", "2026-11-10 00:00", true},  // Tuesday, not the 1st
		{"0 0 1 * 2", "2026-12-01 00:00", true},  // the 1st, a Tuesday too
		{"0 0 1 * 3", "2026-11-10 00:00", false}, // neither
		// One day field is `*`: both must match.
		{"0 0 */2 * 2", "2026-11-10 00:00", false}, // the 10th is even, */2 is odd days
		{"0 0 * * 2", "2026-11-10 00:00", true},
	}
	for _, c := range cases {
		spec, err := parseCron(c.expr)
		if err != nil {
			t.Fatalf("parseCron(%q): %v", c.expr, err)
		}
		if got := spec.matches(at(c.when)); got != c.want {
			t.Errorf("%q at %s = %v, want %v", c.expr, c.when, got, c.want)
		}
	}
}

func TestCronNext(t *testing.T) {
	cases := []struct{ expr, from, want string }{
		{"0 3 * * *", "2026-11-10 03:00", "2026-11-11 03:00"}, // strictly after
		{"0 3 * * *", "2026-11-10 02:59", "2026-11-10 03:00"},
		{"30 9 * * mon", "2026-11-10 12:00", "2026-11-16 09:30"},
		{"0 0 29 2 *", "2026-11-10 00:00", "2028-02-29 00:00"},
	}
	for _, c := range cases {
		spec, _ := parseCron(c.expr)
		if got := spec.next(at(c.from)); !got.Equal(at(c.want)) {
			t.Errorf("next(%q, %s) = %s, want %s", c.expr, c.from, got, c.want)
		}
	}
	spec, _ := parseCron("0 0 31 2 *")
	if got := spec.next(at("2026-11-10 00:00")); !got.IsZero() {
		t.Errorf("Feb 31 should never fire, got %s", got)
	}
}

func TestGateOutcomeOverlapAndCap(t *testing.T) {
	now := at("2026-11-10 03:00")
	dead := func(string) bool { return false }
	live := func(string) bool { return true }
	s := &Schedule{MaxRunsPerDay: 2}
	if o := gateOutcome(s, now, false, dead, unknownRunState); o != "" {
		t.Fatalf("fresh schedule: %q", o)
	}
	if o := gateOutcome(s, now, true, dead, unknownRunState); o != outcomeSkippedOverlap {
		t.Fatalf("in flight: %q", o)
	}
	s.LastSession = "sched-x-1"
	if o := gateOutcome(s, now, false, live, unknownRunState); o != outcomeSkippedOverlap {
		t.Fatalf("previous run alive: %q", o)
	}
	if o := gateOutcome(s, now, false, dead, unknownRunState); o != "" {
		t.Fatalf("previous run gone: %q", o)
	}
	s.RunDay, s.RunsToday = "2026-11-10", 2
	if o := gateOutcome(s, now, false, dead, unknownRunState); o != outcomeSkippedCap {
		t.Fatalf("cap reached: %q", o)
	}
	// A new day resets the count.
	if o := gateOutcome(s, at("2026-11-11 00:00"), false, dead, unknownRunState); o != "" {
		t.Fatalf("next day: %q", o)
	}
	// No cap set: the default applies.
	s.MaxRunsPerDay, s.RunsToday = 0, scheduleDefaultCap
	if o := gateOutcome(s, now, false, dead, unknownRunState); o != outcomeSkippedCap {
		t.Fatalf("default cap: %q", o)
	}
}

// TestGateOutcomeHookState: a live previous session blocks the next firing
// only while its claude has not finished (owner decision, option a).
func TestGateOutcomeHookState(t *testing.T) {
	now := at("2026-11-10 03:00")
	live := func(string) bool { return true }
	dead := func(string) bool { return false }
	cases := []struct {
		name     string
		st       agent.State
		turnOpen bool
		want     string
	}{
		{"running blocks", agent.StateRunning, true, outcomeSkippedOverlap},
		{"done does not block", agent.StateDone, false, ""},
		{"failed does not block", agent.StateFailed, false, ""},
		{"unknown blocks", agent.StateUnknown, false, outcomeSkippedOverlap},
		{"needs-input after stop does not block", agent.StateNeedsInput, false, ""},
		{"needs-input in a turn blocks", agent.StateNeedsInput, true, outcomeSkippedOverlap},
	}
	for _, c := range cases {
		s := &Schedule{MaxRunsPerDay: 4, LastSession: "sched-x-1"}
		state := func(name string) (agent.State, bool) {
			if name != "sched-x-1" {
				t.Fatalf("%s: state asked for %q", c.name, name)
			}
			return c.st, c.turnOpen
		}
		if o := gateOutcome(s, now, false, live, state); o != c.want {
			t.Errorf("%s: got %q, want %q", c.name, o, c.want)
		}
		// A gone session never blocks, whatever its last state.
		if o := gateOutcome(s, now, false, dead, state); o != "" {
			t.Errorf("%s, session gone: got %q", c.name, o)
		}
		// A firing in flight blocks, whatever the state.
		if o := gateOutcome(s, now, true, live, state); o != outcomeSkippedOverlap {
			t.Errorf("%s, in flight: got %q", c.name, o)
		}
	}
}

// TestRegistryRunState reads the state the hooks fold into a session.
func TestRegistryRunState(t *testing.T) {
	r := NewHookRegistry()
	if st, open := r.runState("sched-x-1"); st != agent.StateUnknown || open {
		t.Fatalf("unregistered: %s %v", st, open)
	}
	e := &hookEntry{name: "sched-x-1", paneID: "term_1"}
	r.add(e)
	if st, _ := r.runState("sched-x-1"); st != agent.StateUnknown {
		t.Fatalf("no hook yet: %s", st)
	}
	r.mu.Lock()
	r.applyLocked(e, "user-prompt-submit", hookPayload{}, "")
	r.mu.Unlock()
	if st, open := r.runState("sched-x-1"); st != agent.StateRunning || !open {
		t.Fatalf("after prompt: %s %v", st, open)
	}
	r.mu.Lock()
	r.applyLocked(e, "stop", hookPayload{}, "")
	r.applyLocked(e, "notification", hookPayload{NotificationType: "idle_prompt"}, "")
	r.mu.Unlock()
	st, open := r.runState("sched-x-1")
	if st != agent.StateNeedsInput || open || !runFinished(st, open) {
		t.Fatalf("idle after stop: %s %v", st, open)
	}
	// Hibernation stops claude; its session-end resets the run to unknown.
	// A hibernated session still reads as finished.
	r.mu.Lock()
	r.applyLocked(e, "session-end", hookPayload{}, "")
	e.hib = &hibernation{Session: "sched-x-1"}
	r.mu.Unlock()
	if st, open := r.runState("sched-x-1"); st != agent.StateDone || open {
		t.Fatalf("hibernated: %s %v", st, open)
	}
	r.mu.Lock()
	e.hib = nil
	e.released = true
	r.mu.Unlock()
	if st, _ := r.runState("sched-x-1"); st != agent.StateUnknown {
		t.Fatalf("released: %s", st)
	}
}

// TestFireAfterFinishedRunKeepsSession: a finished run does not block the
// next firing, and the scheduler does not kill its session.
func TestFireAfterFinishedRunKeepsSession(t *testing.T) {
	sc, f := testScheduler(t, &Schedule{ID: "s1", Cron: "* * * * *", Prompt: "p", Enabled: true, MaxRunsPerDay: 5})
	states := map[string]agent.State{}
	sc.runState = func(n string) (agent.State, bool) {
		if st, ok := states[n]; ok {
			return st, st == agent.StateRunning
		}
		return agent.StateUnknown, false
	}
	first := sc.fire("s1", at("2026-11-10 03:00"))
	if first.Outcome != outcomeRan {
		t.Fatalf("first firing: %+v", first)
	}
	states[first.Session] = agent.StateRunning
	if r := sc.fire("s1", at("2026-11-10 03:01")); r.Outcome != outcomeSkippedOverlap {
		t.Fatalf("while running: %+v", r)
	}
	states[first.Session] = agent.StateDone
	if r := sc.fire("s1", at("2026-11-10 03:02")); r.Outcome != outcomeRan {
		t.Fatalf("after done: %+v", r)
	}
	if !f.alive[first.Session] {
		t.Fatal("the finished session must stay open")
	}
}

// testScheduler is a scheduler over an in-memory store with fake seams.
type fakeRuns struct {
	alive     map[string]bool
	started   []string
	prompts   []string
	startErr  error
	precheck  func() (int, error)
	prechecks int
}

func testScheduler(t *testing.T, s *Schedule) (*scheduler, *fakeRuns) {
	t.Helper()
	f := &fakeRuns{alive: map[string]bool{}}
	sc := newScheduler(newScheduleStore(""))
	sc.alive = func(n string) bool { return f.alive[n] }
	sc.start = func(name, cwd, prompt string) (string, error) {
		if f.startErr != nil {
			return "spawn-failed", f.startErr
		}
		f.started = append(f.started, name)
		f.prompts = append(f.prompts, prompt)
		f.alive[name] = true
		return "", nil
	}
	sc.precheck = func(context.Context, string, []string) (int, error) {
		f.prechecks++
		if f.precheck == nil {
			return 0, nil
		}
		return f.precheck()
	}
	if s.Cwd == "" {
		s.Cwd = t.TempDir()
	}
	if err := sc.store.add(s); err != nil {
		t.Fatal(err)
	}
	return sc, f
}

func TestFireRunsThenSkipsOverlap(t *testing.T) {
	sc, f := testScheduler(t, &Schedule{ID: "s1", Cron: "* * * * *", Prompt: "triage", Enabled: true, MaxRunsPerDay: 5})
	r := sc.fire("s1", at("2026-11-10 03:00"))
	if r.Outcome != outcomeRan || r.Session != "sched-s1-20261110-0300" {
		t.Fatalf("first firing: %+v", r)
	}
	if len(f.prompts) != 1 || f.prompts[0] != "triage" {
		t.Fatalf("prompt not handed to start: %v", f.prompts)
	}
	// The run is still alive one minute later: skip.
	if r := sc.fire("s1", at("2026-11-10 03:01")); r.Outcome != outcomeSkippedOverlap {
		t.Fatalf("second firing: %+v", r)
	}
	// The session ended: the next firing runs.
	f.alive["sched-s1-20261110-0300"] = false
	if r := sc.fire("s1", at("2026-11-10 03:02")); r.Outcome != outcomeRan {
		t.Fatalf("third firing: %+v", r)
	}
	got, _ := sc.store.get("s1")
	if got.RunsToday != 2 || got.LastSession != "sched-s1-20261110-0302" || len(got.History) != 3 {
		t.Fatalf("state after three firings: %+v", got)
	}
}

func TestFireInFlightIsOverlap(t *testing.T) {
	sc, f := testScheduler(t, &Schedule{ID: "s1", Cron: "* * * * *", Prompt: "p", Enabled: true})
	sc.inflight["s1"] = true // another firing is in its precheck
	if r := sc.fire("s1", at("2026-11-10 03:00")); r.Outcome != outcomeSkippedOverlap {
		t.Fatalf("got %+v", r)
	}
	if !sc.inflight["s1"] {
		t.Fatal("a skipped firing must not clear the other firing's mark")
	}
	if len(f.started) != 0 {
		t.Fatal("started while in flight")
	}
}

func TestFireCapPerDay(t *testing.T) {
	sc, f := testScheduler(t, &Schedule{ID: "s1", Cron: "* * * * *", Prompt: "p", Enabled: true, MaxRunsPerDay: 2})
	for i, want := range []string{outcomeRan, outcomeRan, outcomeSkippedCap} {
		f.alive = map[string]bool{} // every previous run has ended
		r := sc.fire("s1", at("2026-11-10 03:00").Add(time.Duration(i)*time.Minute))
		if r.Outcome != want {
			t.Fatalf("firing %d: %+v, want %s", i, r, want)
		}
	}
	if f.prechecks != 0 {
		t.Fatal("no precheck configured, none must run")
	}
	if r := sc.fire("s1", at("2026-11-11 00:00")); r.Outcome != outcomeRan {
		t.Fatalf("a new day: %+v", r)
	}
}

func TestFirePrecheck(t *testing.T) {
	sc, f := testScheduler(t, &Schedule{ID: "s1", Cron: "* * * * *", Prompt: "p", Enabled: true,
		Precheck: []string{"git", "fetch"}, MaxRunsPerDay: 10})
	f.precheck = func() (int, error) { return 1, nil }
	r := sc.fire("s1", at("2026-11-10 03:00"))
	if r.Outcome != outcomeSkippedPrecheck || r.ExitCode == nil || *r.ExitCode != 1 || len(f.started) != 0 {
		t.Fatalf("non-zero precheck: %+v", r)
	}
	f.precheck = func() (int, error) { return -1, errPrecheckTimeout }
	if r := sc.fire("s1", at("2026-11-10 03:01")); r.Outcome != outcomeSkippedPrecheck || r.Reason != "precheck-timeout" {
		t.Fatalf("timeout: %+v", r)
	}
	f.precheck = func() (int, error) { return -1, errors.New("exec: not found") }
	if r := sc.fire("s1", at("2026-11-10 03:02")); r.Outcome != outcomeFailed || r.Reason != "precheck-start-failed" {
		t.Fatalf("start failure: %+v", r)
	}
	f.precheck = nil
	r = sc.fire("s1", at("2026-11-10 03:03"))
	if r.Outcome != outcomeRan || r.ExitCode == nil || *r.ExitCode != 0 {
		t.Fatalf("passing precheck: %+v", r)
	}
	got, _ := sc.store.get("s1")
	if got.RunsToday != 1 {
		t.Fatalf("skips must not count against the cap: %d", got.RunsToday)
	}
}

func TestFireFailures(t *testing.T) {
	sc, f := testScheduler(t, &Schedule{ID: "s1", Cron: "* * * * *", Prompt: "p", Enabled: true})
	f.startErr = errors.New("tmux")
	if r := sc.fire("s1", at("2026-11-10 03:00")); r.Outcome != outcomeFailed || r.Reason != "spawn-failed" {
		t.Fatalf("start error: %+v", r)
	}
	if sc.inflight["s1"] {
		t.Fatal("in-flight mark left behind")
	}
	sc2, _ := testScheduler(t, &Schedule{ID: "s2", Cron: "* * * * *", Prompt: "p", Enabled: true,
		Cwd: "/nonexistent/schedule/cwd"})
	if r := sc2.fire("s2", at("2026-11-10 03:00")); r.Outcome != outcomeFailed || r.Reason != "cwd-missing" {
		t.Fatalf("missing cwd: %+v", r)
	}
	// Disabled: nothing happens, nothing is recorded.
	sc3, f3 := testScheduler(t, &Schedule{ID: "s3", Cron: "* * * * *", Prompt: "p"})
	if r := sc3.fire("s3", at("2026-11-10 03:00")); r.Outcome != "" || len(f3.started) != 0 {
		t.Fatalf("disabled: %+v", r)
	}
	if got, _ := sc3.store.get("s3"); len(got.History) != 0 {
		t.Fatal("disabled schedule recorded a row")
	}
}

func TestDueHandsOutAMinuteOnce(t *testing.T) {
	sc, _ := testScheduler(t, &Schedule{ID: "s1", Cron: "0 3 * * *", Prompt: "p", Enabled: true})
	_ = sc.store.add(&Schedule{ID: "s2", Cron: "0 3 * * *", Prompt: "p", Enabled: false})
	if ids := sc.due(at("2026-11-10 02:59")); len(ids) != 0 {
		t.Fatalf("02:59: %v", ids)
	}
	if ids := sc.due(at("2026-11-10 03:00")); len(ids) != 1 || ids[0] != "s1" {
		t.Fatalf("03:00: %v", ids)
	}
	if ids := sc.due(at("2026-11-10 03:00")); len(ids) != 0 {
		t.Fatalf("03:00 twice: %v", ids)
	}
}

func TestScheduleStorePersistsAndCapsHistory(t *testing.T) {
	path := filepath.Join(t.TempDir(), "schedules.json")
	st := newScheduleStore(path)
	if err := st.add(&Schedule{ID: "s1", Name: "n", Cron: "* * * * *", Prompt: "p", Enabled: true}); err != nil {
		t.Fatal(err)
	}
	base := at("2026-11-10 00:00")
	for i := 0; i < scheduleHistoryMax+7; i++ {
		st.record("s1", base, RunRecord{Start: base.Add(time.Duration(i) * time.Minute).Format(time.RFC3339),
			Outcome: outcomeSkippedOverlap})
	}
	st.record("s1", base, RunRecord{Start: "last", Outcome: outcomeRan, Session: "sched-s1-x"})

	again := newScheduleStore(path)
	got, err := again.get("s1")
	if err != nil {
		t.Fatal(err)
	}
	if len(got.History) != scheduleHistoryMax || got.History[len(got.History)-1].Start != "last" {
		t.Fatalf("history: %d rows, last %+v", len(got.History), got.History[len(got.History)-1])
	}
	if got.RunsToday != 1 || got.LastSession != "sched-s1-x" || got.RunDay != "2026-11-10" {
		t.Fatalf("run state not persisted: %+v", got)
	}
	if _, err := again.setEnabled("s1", false); err != nil {
		t.Fatal(err)
	}
	if got, _ := newScheduleStore(path).get("s1"); got.Enabled {
		t.Fatal("disable not persisted")
	}
	if err := again.remove("s1"); err != nil {
		t.Fatal(err)
	}
	if _, err := newScheduleStore(path).get("s1"); !errors.Is(err, errNoSchedule) {
		t.Fatal("remove not persisted")
	}
	// No temp file is left beside the store.
	entries, _ := os.ReadDir(filepath.Dir(path))
	if len(entries) != 1 {
		t.Fatalf("files in data dir: %d", len(entries))
	}
}

func TestScheduleStoreCorruptFileIsMovedAside(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "schedules.json")
	if err := os.WriteFile(path, []byte("{not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	st := newScheduleStore(path)
	if len(st.all()) != 0 {
		t.Fatal("corrupt file loaded")
	}
	matches, _ := filepath.Glob(path + ".corrupt-*")
	if len(matches) != 1 {
		t.Fatalf("corrupt file not moved aside: %v", matches)
	}
}

func TestScheduleStoreLimit(t *testing.T) {
	st := newScheduleStore("")
	for i := 0; i < scheduleMax; i++ {
		if err := st.add(&Schedule{ID: string(rune('a' + i%26)), Cron: "* * * * *"}); err != nil {
			t.Fatal(err)
		}
	}
	if err := st.add(&Schedule{ID: "x"}); !errors.Is(err, errTooManySchedules) {
		t.Fatalf("over the limit: %v", err)
	}
}

func TestScheduleValidate(t *testing.T) {
	dir := t.TempDir()
	now := at("2026-11-10 00:00")
	good := func() scheduleCreate {
		return scheduleCreate{Name: "nightly", Cron: "0  3 * * *", Cwd: dir, Prompt: "triage the issues"}
	}
	s, err := good().validate("/home/u", now)
	if err != nil {
		t.Fatal(err)
	}
	if !s.Enabled || s.MaxRunsPerDay != scheduleDefaultCap || s.PrecheckTimeoutS != precheckDefaultTimeoutS ||
		s.Cron != "0 3 * * *" || !strings.HasPrefix(s.ID, "s") {
		t.Fatalf("defaults: %+v", s)
	}
	ten, zero, big := 10, 0, 100000
	no := false
	bad := []func(*scheduleCreate){
		func(b *scheduleCreate) { b.Name = "" },
		func(b *scheduleCreate) { b.Name = "a\nb" },
		func(b *scheduleCreate) { b.Cron = "61 * * * *" },
		func(b *scheduleCreate) { b.Cwd = "relative/dir" },
		func(b *scheduleCreate) { b.Cwd = "/nonexistent/dir" },
		func(b *scheduleCreate) { b.Prompt = "" },
		func(b *scheduleCreate) { b.Prompt = "--dangerously-skip-permissions" },
		func(b *scheduleCreate) { b.Prompt = strings.Repeat("x", 4097) },
		func(b *scheduleCreate) { b.Prompt = "a\x00b" },
		func(b *scheduleCreate) { b.Precheck = []string{""} },
		func(b *scheduleCreate) { b.Precheck = make([]string, 33) },
		func(b *scheduleCreate) { b.MaxRunsPerDay = &zero },
		func(b *scheduleCreate) { b.MaxRunsPerDay = &big },
	}
	for i, mut := range bad {
		b := good()
		mut(&b)
		if _, err := b.validate("/home/u", now); err == nil {
			t.Errorf("case %d accepted", i)
		}
	}
	b := good()
	b.MaxRunsPerDay, b.PrecheckTimeoutS, b.Enabled = &ten, &big, &no
	b.Precheck = []string{"test", "-f", "TODO"}
	s, err = b.validate("/home/u", now)
	if err != nil || s.MaxRunsPerDay != 10 || s.PrecheckTimeoutS != precheckMaxTimeoutS || s.Enabled {
		t.Fatalf("explicit values: %+v %v", s, err)
	}
	// `~` expands against home.
	b = good()
	b.Cwd = "~"
	if s, err := b.validate(dir, now); err != nil || s.Cwd != filepath.Clean(dir) {
		t.Fatalf("~: %+v %v", s, err)
	}
}

func TestScheduleRoutes(t *testing.T) {
	s, _ := testService(ok(""))
	s.SetDataDir(t.TempDir())
	cwd := t.TempDir()
	body, _ := json.Marshal(map[string]any{"name": "nightly", "cron": "0 3 * * *", "cwd": cwd,
		"prompt": "triage", "precheck": []string{"true"}})

	if w := do(s, "GET", "/api/v2/schedules", "device-all", ""); w.Code != http.StatusForbidden {
		t.Fatalf("a device without shell:attach: %d", w.Code)
	}
	w := do(s, "POST", "/api/v2/schedules", "owner-token", string(body))
	if w.Code != http.StatusCreated {
		t.Fatalf("add: %d %s", w.Code, w.Body)
	}
	var created scheduleView
	_ = json.Unmarshal(w.Body.Bytes(), &created)
	if created.ID == "" || created.NextRun == "" || !created.Enabled {
		t.Fatalf("add answer: %s", w.Body)
	}
	if w := do(s, "POST", "/api/v2/schedules", "owner-token", `{"name":"x","bogus":1}`); w.Code != http.StatusBadRequest {
		t.Fatalf("unknown field: %d", w.Code)
	}
	w = do(s, "GET", "/api/v2/schedules", "owner-token", "")
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), created.ID) || strings.Contains(w.Body.String(), `"history"`) {
		t.Fatalf("list: %d %s", w.Code, w.Body)
	}
	w = do(s, "POST", "/api/v2/schedules/"+created.ID+"/disable", "owner-token", "")
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), `"enabled":false`) || strings.Contains(w.Body.String(), "next_run") {
		t.Fatalf("disable: %d %s", w.Code, w.Body)
	}
	if w := do(s, "POST", "/api/v2/schedules/"+created.ID+"/enable", "owner-token", ""); w.Code != http.StatusOK {
		t.Fatalf("enable: %d", w.Code)
	}
	s.sched.store.record(created.ID, at("2026-11-10 03:00"), RunRecord{Start: "a", Outcome: outcomeSkippedOverlap})
	s.sched.store.record(created.ID, at("2026-11-10 03:01"), RunRecord{Start: "b", Outcome: outcomeRan, Session: "sched-x"})
	w = do(s, "GET", "/api/v2/schedules/"+created.ID+"/history", "owner-token", "")
	var h struct {
		History []RunRecord `json:"history"`
	}
	_ = json.Unmarshal(w.Body.Bytes(), &h)
	if w.Code != http.StatusOK || len(h.History) != 2 || h.History[0].Start != "b" {
		t.Fatalf("history (newest first): %d %s", w.Code, w.Body)
	}
	if w := do(s, "DELETE", "/api/v2/schedules/"+created.ID, "owner-token", ""); w.Code != http.StatusOK {
		t.Fatalf("rm: %d", w.Code)
	}
	if w := do(s, "GET", "/api/v2/schedules/"+created.ID, "owner-token", ""); w.Code != http.StatusNotFound {
		t.Fatalf("after rm: %d", w.Code)
	}
}

func TestStartScheduledRunUsesGateAndClaude(t *testing.T) {
	s, calls := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:9")
	s.claudeBin = "/opt/claude"
	if reason, err := s.startScheduledRun("sched-s1-20261110-0300", "/tmp", "triage"); err != nil {
		t.Fatalf("start: %q %v", reason, err)
	}
	argv := lastCreate(t, calls)
	if env := envArgs(argv); env["YMUX_POLICY"] != policyGate {
		t.Fatalf("a scheduled run must be gated, env: %v", env)
	}
	if tail := strings.Join(argv[len(argv)-3:], " "); tail != "-- /opt/claude triage" {
		t.Fatalf("argv tail: %q", tail)
	}
	if e := s.hooks.byName["sched-s1-20261110-0300"]; e == nil || e.policy != policyGate {
		t.Fatalf("registry entry: %+v", e)
	}

	// No hook listener: the run must not start without the gate.
	s2, calls2 := hookService("tmux 3.4")
	s2.claudeBin = "/opt/claude"
	if reason, err := s2.startScheduledRun("sched-s1-20261110-0301", "/tmp", "triage"); err == nil || reason != "no-hook-routing" {
		t.Fatalf("without hooks: %q %v", reason, err)
	}
	for _, c := range *calls2 {
		if len(c) > 1 && c[1] == "new-session" {
			t.Fatal("claude started without hook routing")
		}
	}
}
