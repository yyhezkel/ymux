package term

// schedule.go — scheduled prompts (Orca idea #5): the daemon starts `claude`
// with a saved prompt on a cron schedule, with the desktop off.
//
// A firing goes through these steps, in this order:
//
//  1. overlap — the previous run of this schedule is still a live tmux
//     session, or a firing of it is still in its precheck: skip.
//  2. cap     — the schedule already ran max_runs_per_day times today: skip.
//  3. cwd     — the working directory is gone: failed.
//  4. precheck (optional) — an argv run in cwd with a timeout; a non-zero
//     exit or a timeout skips the run, so it costs no quota.
//  5. run     — spawnSession starts `claude <prompt>` in a new tmux session
//     with policy gate. It is a normal browser session: hooks, the Queue, the
//     session list. It NEVER gets policy none, because none answers every
//     permission request with allow.
//
// Each step writes one history row. The file is <data dir>/schedules.json,
// written with writeFileAtomic (Rule #7). It holds the prompt, because the
// daemon must run it, but never the output of a run or of a precheck: both
// go to the tmux pane or to /dev/null.
//
// Rule #1: logs carry the schedule id, the outcome, the exit code and the
// session name. Never the prompt and never the precheck argv.
//
// Firings missed while the daemon was down are not caught up. Times are the
// host's local time.

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"sync"
	"time"
)

// Run outcomes, as the history and the logs spell them.
const (
	outcomeRan              = "ran"
	outcomeSkippedPrecheck  = "skipped-precheck"
	outcomeSkippedOverlap   = "skipped-overlap"
	outcomeSkippedCap       = "skipped-cap"
	outcomeFailed           = "failed"
	scheduleHistoryMax      = 50
	scheduleMax             = 64
	scheduleDefaultCap      = 4
	scheduleMaxCap          = 24
	precheckDefaultTimeoutS = 60
	precheckMaxTimeoutS     = 600
)

// Schedule is one saved prompt and its run state.
type Schedule struct {
	ID               string   `json:"id"`
	Name             string   `json:"name"`
	Cron             string   `json:"cron"`
	Cwd              string   `json:"cwd"`
	Prompt           string   `json:"prompt"`
	Precheck         []string `json:"precheck,omitempty"`
	PrecheckTimeoutS int      `json:"precheck_timeout_seconds"`
	MaxRunsPerDay    int      `json:"max_runs_per_day"`
	Enabled          bool     `json:"enabled"`
	CreatedAt        string   `json:"created_at"`

	// RunDay (local YYYY-MM-DD) and RunsToday count the runs for the cap.
	// They are kept apart from History, because skip rows can push the
	// day's runs out of a short history.
	RunDay    string `json:"run_day,omitempty"`
	RunsToday int    `json:"runs_today"`
	// LastSession is the tmux session of the last run, for the overlap rule.
	LastSession string `json:"last_session,omitempty"`
	// History holds the newest scheduleHistoryMax rows, oldest first.
	History []RunRecord `json:"history"`
}

// RunRecord is one firing. Reason is a fixed word, never tool output.
type RunRecord struct {
	Start    string `json:"start"`
	Outcome  string `json:"outcome"`
	Session  string `json:"session,omitempty"`
	ExitCode *int   `json:"exit_code,omitempty"`
	Reason   string `json:"reason,omitempty"`
}

type schedulesFile struct {
	Schedules []*Schedule `json:"schedules"`
}

var errNoSchedule = errors.New("no such schedule")

// scheduleStore is the schedules file plus a lock. An empty path keeps the
// schedules in memory only (tests).
type scheduleStore struct {
	mu   sync.Mutex
	path string
	list []*Schedule
}

func newScheduleStore(path string) *scheduleStore {
	st := &scheduleStore{path: path}
	if path != "" {
		var f schedulesFile
		if loadJSON(path, &f, "schedules") {
			for _, s := range f.Schedules {
				if s != nil && s.ID != "" {
					st.list = append(st.list, s)
				}
			}
		}
	}
	return st
}

// saveLocked writes the file. The caller holds st.mu.
func (st *scheduleStore) saveLocked() error {
	if st.path == "" {
		return nil
	}
	b, err := json.Marshal(schedulesFile{Schedules: st.list})
	if err != nil {
		return err
	}
	return writeFileAtomic(st.path, b)
}

func (st *scheduleStore) findLocked(id string) *Schedule {
	for _, s := range st.list {
		if s.ID == id {
			return s
		}
	}
	return nil
}

// copySchedule is a deep copy, so a caller can read it without the lock.
func copySchedule(s *Schedule) Schedule {
	c := *s
	c.Precheck = append([]string(nil), s.Precheck...)
	c.History = append([]RunRecord(nil), s.History...)
	return c
}

func (st *scheduleStore) all() []Schedule {
	st.mu.Lock()
	defer st.mu.Unlock()
	out := make([]Schedule, 0, len(st.list))
	for _, s := range st.list {
		out = append(out, copySchedule(s))
	}
	return out
}

func (st *scheduleStore) get(id string) (Schedule, error) {
	st.mu.Lock()
	defer st.mu.Unlock()
	s := st.findLocked(id)
	if s == nil {
		return Schedule{}, errNoSchedule
	}
	return copySchedule(s), nil
}

func (st *scheduleStore) add(s *Schedule) error {
	st.mu.Lock()
	defer st.mu.Unlock()
	if len(st.list) >= scheduleMax {
		return errTooManySchedules
	}
	st.list = append(st.list, s)
	if err := st.saveLocked(); err != nil {
		st.list = st.list[:len(st.list)-1]
		return err
	}
	return nil
}

func (st *scheduleStore) remove(id string) error {
	st.mu.Lock()
	defer st.mu.Unlock()
	for i, s := range st.list {
		if s.ID == id {
			prev := st.list
			st.list = append(append([]*Schedule(nil), st.list[:i]...), st.list[i+1:]...)
			if err := st.saveLocked(); err != nil {
				st.list = prev
				return err
			}
			return nil
		}
	}
	return errNoSchedule
}

func (st *scheduleStore) setEnabled(id string, on bool) (Schedule, error) {
	st.mu.Lock()
	defer st.mu.Unlock()
	s := st.findLocked(id)
	if s == nil {
		return Schedule{}, errNoSchedule
	}
	was := s.Enabled
	s.Enabled = on
	if err := st.saveLocked(); err != nil {
		s.Enabled = was
		return Schedule{}, err
	}
	return copySchedule(s), nil
}

// record appends a history row and, for a run, updates the cap counter and
// the overlap session. A schedule removed during the firing is ignored.
func (st *scheduleStore) record(id string, at time.Time, rec RunRecord) {
	st.mu.Lock()
	defer st.mu.Unlock()
	s := st.findLocked(id)
	if s == nil {
		return
	}
	if rec.Outcome == outcomeRan {
		s.RunsToday = runsToday(s, at) + 1
		s.RunDay = at.Format("2006-01-02")
		s.LastSession = rec.Session
	}
	s.History = append(s.History, rec)
	if n := len(s.History); n > scheduleHistoryMax {
		s.History = append([]RunRecord(nil), s.History[n-scheduleHistoryMax:]...)
	}
	if err := st.saveLocked(); err != nil {
		logger.Error("schedules file write failed", "schedule", id, "err", err)
	}
}

// runsToday is the cap counter for the local day of at.
func runsToday(s *Schedule, at time.Time) int {
	if s.RunDay != at.Format("2006-01-02") {
		return 0
	}
	return s.RunsToday
}

// gateOutcome applies the overlap and cap rules. It returns "" when the
// firing may go on, else the skip outcome. inflight is true while another
// firing of this schedule is still in its precheck or its start.
func gateOutcome(s *Schedule, at time.Time, inflight bool, alive func(string) bool) string {
	if inflight || (s.LastSession != "" && alive(s.LastSession)) {
		return outcomeSkippedOverlap
	}
	limit := s.MaxRunsPerDay
	if limit < 1 {
		limit = scheduleDefaultCap
	}
	if runsToday(s, at) >= limit {
		return outcomeSkippedCap
	}
	return ""
}

var errPrecheckTimeout = errors.New("precheck timed out")

// scheduler runs the minute tick and the firings. Every outside effect is a
// field, so the tests never exec or touch tmux.
type scheduler struct {
	store *scheduleStore
	now   func() time.Time
	// alive reports whether a tmux session still exists.
	alive func(name string) bool
	// precheck runs argv in cwd until ctx ends. It returns the exit code;
	// err is errPrecheckTimeout or a start failure.
	precheck func(ctx context.Context, cwd string, argv []string) (int, error)
	// start creates the run's session. It returns a fixed reason word on
	// failure.
	start func(session, cwd, prompt string) (reason string, err error)

	mu       sync.Mutex
	inflight map[string]bool
	lastTick time.Time
}

func newScheduler(store *scheduleStore) *scheduler {
	return &scheduler{store: store, now: time.Now, precheck: runPrecheck, inflight: map[string]bool{}}
}

// due returns the ids of the enabled schedules that fire at minute. A minute
// is handed out once only, so a late or early timer cannot fire it twice.
func (sc *scheduler) due(minute time.Time) []string {
	sc.mu.Lock()
	if !minute.After(sc.lastTick) {
		sc.mu.Unlock()
		return nil
	}
	sc.lastTick = minute
	sc.mu.Unlock()
	var ids []string
	for _, s := range sc.store.all() {
		if !s.Enabled {
			continue
		}
		spec, err := parseCron(s.Cron)
		if err != nil {
			continue // validated at add; a hand-edited file costs only itself
		}
		if spec.matches(minute) {
			ids = append(ids, s.ID)
		}
	}
	return ids
}

// run ticks at each minute boundary until ctx ends. Each firing gets its own
// goroutine, so a slow precheck does not delay the next minute.
func (sc *scheduler) run(ctx context.Context) {
	for {
		now := sc.now()
		wait := now.Truncate(time.Minute).Add(time.Minute).Sub(now)
		t := time.NewTimer(wait)
		select {
		case <-ctx.Done():
			t.Stop()
			return
		case <-t.C:
		}
		minute := sc.now().Truncate(time.Minute)
		for _, id := range sc.due(minute) {
			go sc.fire(id, minute)
		}
	}
}

// fire runs one firing of schedule id at minute at, and records the outcome.
func (sc *scheduler) fire(id string, at time.Time) RunRecord {
	s, err := sc.store.get(id)
	if err != nil || !s.Enabled {
		return RunRecord{}
	}
	sc.mu.Lock()
	inflight := sc.inflight[id]
	if !inflight {
		sc.inflight[id] = true
	}
	sc.mu.Unlock()
	rec := RunRecord{Start: at.Format(time.RFC3339)}
	if inflight {
		rec.Outcome = outcomeSkippedOverlap
		return sc.finish(id, at, rec, false)
	}
	if o := gateOutcome(&s, at, false, sc.alive); o != "" {
		rec.Outcome = o
		return sc.finish(id, at, rec, true)
	}
	if st, err := os.Stat(s.Cwd); err != nil || !st.IsDir() {
		rec.Outcome, rec.Reason = outcomeFailed, "cwd-missing"
		return sc.finish(id, at, rec, true)
	}
	if len(s.Precheck) > 0 {
		secs := s.PrecheckTimeoutS
		if secs < 1 || secs > precheckMaxTimeoutS {
			secs = precheckDefaultTimeoutS
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Duration(secs)*time.Second)
		code, err := sc.precheck(ctx, s.Cwd, s.Precheck)
		cancel()
		switch {
		case errors.Is(err, errPrecheckTimeout):
			rec.Outcome, rec.Reason = outcomeSkippedPrecheck, "precheck-timeout"
			return sc.finish(id, at, rec, true)
		case err != nil:
			rec.Outcome, rec.Reason = outcomeFailed, "precheck-start-failed"
			return sc.finish(id, at, rec, true)
		case code != 0:
			rec.Outcome, rec.ExitCode = outcomeSkippedPrecheck, &code
			return sc.finish(id, at, rec, true)
		}
		zero := 0
		rec.ExitCode = &zero
	}
	name := scheduleSessionName(id, at)
	if reason, err := sc.start(name, s.Cwd, s.Prompt); err != nil {
		rec.Outcome, rec.Reason = outcomeFailed, reason
		return sc.finish(id, at, rec, true)
	}
	rec.Outcome, rec.Session = outcomeRan, name
	return sc.finish(id, at, rec, true)
}

// finish records rec, logs it and, when owned, clears the in-flight mark.
func (sc *scheduler) finish(id string, at time.Time, rec RunRecord, owned bool) RunRecord {
	sc.store.record(id, at, rec)
	if owned {
		sc.mu.Lock()
		delete(sc.inflight, id)
		sc.mu.Unlock()
	}
	code := -1
	if rec.ExitCode != nil {
		code = *rec.ExitCode
	}
	logger.Info("schedule fired", "schedule", id, "outcome", rec.Outcome, "exit_code", code,
		"session", rec.Session, "reason", rec.Reason)
	return rec
}

// scheduleSessionName is the run's tmux name. It carries the id and the
// minute, never any text of the prompt (tmux ls and the logs show it).
func scheduleSessionName(id string, at time.Time) string {
	return "sched-" + id + "-" + at.Format("20060102-1504")
}

// runPrecheck runs argv in cwd with no stdin and its output sent to
// /dev/null. The output is never read, stored or logged.
func runPrecheck(ctx context.Context, cwd string, argv []string) (int, error) {
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = cwd
	// The hooks of any claude the precheck starts must stay silent.
	cmd.Env = append(os.Environ(), "YMUX_HEADLESS=1")
	err := cmd.Run()
	if ctx.Err() == context.DeadlineExceeded {
		return -1, errPrecheckTimeout
	}
	var ee *exec.ExitError
	if errors.As(err, &ee) {
		return ee.ExitCode(), nil
	}
	if err != nil {
		return -1, err
	}
	return 0, nil
}
