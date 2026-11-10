package term

// schedroutes.go — the REST surface of the scheduled prompts (schedule.go)
// and the wiring into the Service. Raw handlers behind s.gate, like the
// other term routes, so they need shell:attach: a schedule runs commands on
// the host. They are not in the OpenAPI spec.
//
//	GET    /api/v2/schedules                list (no history)
//	POST   /api/v2/schedules                add → 201
//	GET    /api/v2/schedules/{id}           one (no history)
//	DELETE /api/v2/schedules/{id}           remove
//	POST   /api/v2/schedules/{id}/enable    enable
//	POST   /api/v2/schedules/{id}/disable   disable
//	GET    /api/v2/schedules/{id}/history   the history, newest first
//
// The CLI verbs `ymux schedule add|list|rm|enable|disable|history` call these
// on 127.0.0.1 with the daemon's own token.

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"
)

var errTooManySchedules = errors.New("too many schedules (64 at most)")

// scheduleView is a schedule as the API returns it: no history, plus the
// next firing and today's run count.
type scheduleView struct {
	Schedule
	History   []RunRecord `json:"history,omitempty"`
	NextRun   string      `json:"next_run,omitempty"`
	LastRun   *RunRecord  `json:"last_run,omitempty"`
	RunsToday int         `json:"runs_today"`
}

func viewSchedule(s Schedule, now time.Time) scheduleView {
	v := scheduleView{Schedule: s, RunsToday: runsToday(&s, now)}
	if n := len(s.History); n > 0 {
		last := s.History[n-1]
		v.LastRun = &last
	}
	v.Schedule.History = nil
	if spec, err := parseCron(s.Cron); err == nil && s.Enabled {
		if t := spec.next(now); !t.IsZero() {
			v.NextRun = t.Format(time.RFC3339)
		}
	}
	return v
}

// scheduleCreate is the add body. Pointers tell "absent" from zero.
type scheduleCreate struct {
	Name             string   `json:"name"`
	Cron             string   `json:"cron"`
	Cwd              string   `json:"cwd"`
	Prompt           string   `json:"prompt"`
	Precheck         []string `json:"precheck"`
	PrecheckTimeoutS *int     `json:"precheck_timeout_seconds"`
	MaxRunsPerDay    *int     `json:"max_runs_per_day"`
	Enabled          *bool    `json:"enabled"`
}

// validate checks the body and returns the schedule to store. home expands a
// leading `~` in cwd.
func (b scheduleCreate) validate(home string, now time.Time) (*Schedule, error) {
	name := strings.TrimSpace(b.Name)
	if name == "" || utf8.RuneCountInString(name) > 64 || hasControl(name) {
		return nil, errors.New("name: 1-64 characters, no control characters")
	}
	if _, err := parseCron(b.Cron); err != nil {
		return nil, err
	}
	cwd := b.Cwd
	if cwd == "~" || strings.HasPrefix(cwd, "~/") {
		cwd = filepath.Join(home, strings.TrimPrefix(cwd, "~"))
	}
	if !filepath.IsAbs(cwd) {
		return nil, errors.New("cwd: must be an absolute path")
	}
	cwd = filepath.Clean(cwd)
	if st, err := os.Stat(cwd); err != nil || !st.IsDir() {
		return nil, errors.New("cwd: directory not found on the host")
	}
	// The prompt is one argv entry of `claude`. A leading `-` would read as
	// a flag, so it is refused.
	if strings.TrimSpace(b.Prompt) == "" || len(b.Prompt) > 4096 ||
		strings.ContainsRune(b.Prompt, 0) || strings.HasPrefix(b.Prompt, "-") {
		return nil, errors.New("prompt: 1-4096 bytes, no NUL, must not start with '-'")
	}
	if len(b.Precheck) > 32 {
		return nil, errors.New("precheck: at most 32 arguments")
	}
	for i, a := range b.Precheck {
		if len(a) > 4096 || strings.ContainsRune(a, 0) || (i == 0 && strings.TrimSpace(a) == "") {
			return nil, errors.New("precheck: invalid argument")
		}
	}
	timeout := precheckDefaultTimeoutS
	if b.PrecheckTimeoutS != nil {
		timeout = min(max(*b.PrecheckTimeoutS, 1), precheckMaxTimeoutS)
	}
	limit := scheduleDefaultCap
	if b.MaxRunsPerDay != nil {
		if *b.MaxRunsPerDay < 1 || *b.MaxRunsPerDay > scheduleMaxCap {
			return nil, errors.New("max_runs_per_day: 1-24")
		}
		limit = *b.MaxRunsPerDay
	}
	enabled := b.Enabled == nil || *b.Enabled
	var raw [4]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return nil, err
	}
	return &Schedule{
		ID: "s" + hex.EncodeToString(raw[:]), Name: name, Cron: strings.Join(strings.Fields(b.Cron), " "),
		Cwd: cwd, Prompt: b.Prompt, Precheck: append([]string(nil), b.Precheck...),
		PrecheckTimeoutS: timeout, MaxRunsPerDay: limit, Enabled: enabled,
		CreatedAt: now.Format(time.RFC3339), History: []RunRecord{},
	}, nil
}

func hasControl(s string) bool {
	for _, r := range s {
		if r < 0x20 || r == 0x7f {
			return true
		}
	}
	return false
}

// startScheduledRun is the scheduler's start seam in production: a gate
// session that runs claude with the prompt. A session that did not get hook
// routing is killed, because its permission requests would reach whatever
// policy the global environment points at, and that may approve them.
func (s *Service) startScheduledRun(name, cwd, prompt string) (string, error) {
	// Checked first, so claude never starts, even briefly, without the gate.
	if s.hooks == nil || s.hooks.hookAddr() == "" || !s.tmux.SupportsSessionEnv() {
		return "no-hook-routing", errors.New("hook routing unavailable")
	}
	_, hooks, err := s.spawnSession(name, cwd, policyGate, "", "", s.claudeBinary(), prompt)
	if err != nil {
		return "spawn-failed", err
	}
	if !hooks {
		_ = s.tmux.Kill(name)
		return "no-hook-routing", errors.New("hook routing unavailable")
	}
	return "", nil
}

// newServiceScheduler builds the scheduler over <dir>/schedules.json.
func (s *Service) newServiceScheduler(dir string) *scheduler {
	sc := newScheduler(newScheduleStore(filepath.Join(dir, "schedules.json")))
	sc.alive = s.tmux.Has
	sc.start = s.startScheduledRun
	return sc
}

// StartScheduler starts the minute tick of the scheduled prompts until ctx
// ends. Call after SetDataDir; without a data dir it does nothing.
func (s *Service) StartScheduler(ctx context.Context) {
	if s.sched == nil {
		return
	}
	go s.sched.run(ctx)
	logger.Info("scheduled prompts enabled", "schedules", len(s.sched.store.all()))
}

func (s *Service) handleSchedules(w http.ResponseWriter, r *http.Request) {
	if s.sched == nil {
		http.Error(w, "schedules unavailable", http.StatusServiceUnavailable)
		return
	}
	now := s.sched.now()
	if r.Method == http.MethodGet {
		list := s.sched.store.all()
		out := make([]scheduleView, 0, len(list))
		for _, sc := range list {
			out = append(out, viewSchedule(sc, now))
		}
		writeJSON(w, http.StatusOK, map[string]any{"schedules": out})
		return
	}
	var body scheduleCreate
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&body); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	sc, err := body.validate(s.home, now)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if err := s.sched.store.add(sc); err != nil {
		if errors.Is(err, errTooManySchedules) {
			http.Error(w, err.Error(), http.StatusConflict)
			return
		}
		logger.Error("schedule add failed", "err", err)
		http.Error(w, "could not save the schedule", http.StatusInternalServerError)
		return
	}
	logger.Info("schedule added", "schedule", sc.ID, "cron", sc.Cron, "precheck", len(sc.Precheck) > 0,
		"enabled", sc.Enabled, "max_runs_per_day", sc.MaxRunsPerDay)
	writeJSON(w, http.StatusCreated, viewSchedule(copySchedule(sc), now))
}

func (s *Service) handleSchedule(w http.ResponseWriter, r *http.Request) {
	if s.sched == nil {
		http.Error(w, "schedules unavailable", http.StatusServiceUnavailable)
		return
	}
	id := r.PathValue("id")
	now := s.sched.now()
	switch {
	case r.Method == http.MethodGet:
		sc, err := s.sched.store.get(id)
		if err != nil {
			http.Error(w, err.Error(), http.StatusNotFound)
			return
		}
		writeJSON(w, http.StatusOK, viewSchedule(sc, now))
	case r.Method == http.MethodDelete:
		if err := s.sched.store.remove(id); err != nil {
			scheduleErr(w, err)
			return
		}
		logger.Info("schedule removed", "schedule", id)
		writeJSON(w, http.StatusOK, map[string]any{"ok": true, "id": id})
	case strings.HasSuffix(r.URL.Path, "/enable"), strings.HasSuffix(r.URL.Path, "/disable"):
		on := strings.HasSuffix(r.URL.Path, "/enable")
		sc, err := s.sched.store.setEnabled(id, on)
		if err != nil {
			scheduleErr(w, err)
			return
		}
		logger.Info("schedule toggled", "schedule", id, "enabled", on)
		writeJSON(w, http.StatusOK, viewSchedule(sc, now))
	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

func (s *Service) handleScheduleHistory(w http.ResponseWriter, r *http.Request) {
	if s.sched == nil {
		http.Error(w, "schedules unavailable", http.StatusServiceUnavailable)
		return
	}
	sc, err := s.sched.store.get(r.PathValue("id"))
	if err != nil {
		http.Error(w, err.Error(), http.StatusNotFound)
		return
	}
	rows := make([]RunRecord, 0, len(sc.History))
	for i := len(sc.History) - 1; i >= 0; i-- {
		rows = append(rows, sc.History[i])
	}
	writeJSON(w, http.StatusOK, map[string]any{"id": sc.ID, "history": rows})
}

func scheduleErr(w http.ResponseWriter, err error) {
	if errors.Is(err, errNoSchedule) {
		http.Error(w, err.Error(), http.StatusNotFound)
		return
	}
	logger.Error("schedules file write failed", "err", err)
	http.Error(w, "could not save the schedule", http.StatusInternalServerError)
}
