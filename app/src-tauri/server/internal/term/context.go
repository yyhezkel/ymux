package term

// context.go — per-Claude-session context for the browser's Context Rail
// (Phase 117, WEB-DESIGN F3). A port of the desktop's context_store.rs
// (docs/CONTEXT.md): one record per Claude Code session id, fed by the
// hooks this daemon already folds — the first prompt (UserPromptSubmit,
// only while empty), one `turn` line per Stop from the parsed [ymux-brief]
// (degraded included), one `closed` line per SessionEnd with Claude Code's
// reason; goal / done from the brief's sticky keys, last non-empty wins.
// Nothing calls an LLM.
//
// Files: <data>/context/sessions/<session_id>.json, atomic (Rule #7), the id
// limited to [A-Za-z0-9_-]{1,128}; a file that will not parse is never
// overwritten (that session is refused for the process); files older than
// 30 days are pruned at load. Every write announces `context:changed`
// {session_id, ws_id}. Prompts and briefs are user content (Rule #1): they
// live in the files and the API only — logs carry ids and counts.
// Not ported: injection back into the agent (context.inject, 105.C).

import (
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"ymux-server/internal/agent"
)

const (
	firstPromptMaxChars = 2000
	contextLogMax       = 200
	contextRetention    = 30 * 24 * time.Hour
)

// ContextLogEntry mirrors context_store.rs LogEntry.
type ContextLogEntry struct {
	TsMs     int64             `json:"ts_ms"`
	Kind     string            `json:"kind"` // turn | closed
	Status   agent.BriefStatus `json:"status"`
	Task     *string           `json:"task"`
	Delta    *string           `json:"delta"`
	Next     *string           `json:"next"`
	Ask      *string           `json:"ask"`
	Rec      *string           `json:"rec"`
	Degraded bool              `json:"degraded"`
}

// SessionContext mirrors context_store.rs SessionContext (the file shape too).
type SessionContext struct {
	Schema        int               `json:"schema"`
	SessionID     string            `json:"session_id"`
	WsID          *string           `json:"ws_id"`
	PaneID        *string           `json:"pane_id"`
	Cwd           *string           `json:"cwd"`
	FirstPrompt   *string           `json:"first_prompt"`
	FirstPromptMs *int64            `json:"first_prompt_ms"`
	Goal          *string           `json:"goal"`
	DoneWhen      *string           `json:"done_when"`
	Log           []ContextLogEntry `json:"log"`
	Version       int64             `json:"version"`
}

func (c *SessionContext) lastActivity() int64 {
	var t int64
	if n := len(c.Log); n > 0 {
		t = c.Log[n-1].TsMs
	}
	if c.FirstPromptMs != nil && *c.FirstPromptMs > t {
		t = *c.FirstPromptMs
	}
	return t
}

// touch: where the session lives now; an empty value never erases one.
func (c *SessionContext) touch(ws, pane, cwd string) bool {
	changed := false
	set := func(dst **string, v string) {
		if v != "" && (*dst == nil || **dst != v) {
			*dst = &v
			changed = true
		}
	}
	set(&c.WsID, ws)
	set(&c.PaneID, pane)
	set(&c.Cwd, agent.ClipChars(cwd, 512))
	return changed
}

func (c *SessionContext) push(e ContextLogEntry) {
	c.Log = append(c.Log, e)
	if len(c.Log) > contextLogMax {
		c.Log = append([]ContextLogEntry{}, c.Log[len(c.Log)-contextLogMax:]...)
	}
}

func validSessionID(id string) bool {
	if id == "" || len(id) > 128 {
		return false
	}
	for _, r := range id {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '_' || r == '-') {
			return false
		}
	}
	return true
}

// contextEvent is one hook, as the store needs it.
type contextEvent struct {
	sessionID, wsID, paneID, cwd string
	prompt                       string       // user-prompt-submit
	stop                         *agent.Brief // stop
	ended                        bool         // session-end
	reason                       string
}

type contextStore struct {
	mu       sync.Mutex
	dir      string // "" = memory only (tests, no data dir)
	sessions map[string]*SessionContext
	broken   map[string]bool
	now      func() time.Time
}

func newContextStore(dir string) *contextStore {
	s := &contextStore{dir: dir, sessions: map[string]*SessionContext{}, broken: map[string]bool{}, now: time.Now}
	if dir == "" {
		return s
	}
	_ = os.MkdirAll(dir, 0o700)
	ents, _ := os.ReadDir(dir)
	pruned := 0
	for _, e := range ents {
		name := e.Name()
		if e.IsDir() || !strings.HasSuffix(name, ".json") {
			continue
		}
		p := filepath.Join(dir, name)
		if info, err := e.Info(); err == nil && s.now().Sub(info.ModTime()) > contextRetention {
			if os.Remove(p) == nil {
				pruned++
			}
			continue
		}
		id := strings.TrimSuffix(name, ".json")
		if !validSessionID(id) {
			continue
		}
		b, err := os.ReadFile(p)
		var c SessionContext
		if err != nil || json.Unmarshal(b, &c) != nil || c.SessionID != id {
			s.broken[id] = true
			logger.Warn("context file unreadable; session left alone", "session", id)
			continue
		}
		if c.Log == nil {
			c.Log = []ContextLogEntry{}
		}
		s.sessions[id] = &c
	}
	logger.Info("context store loaded", "sessions", len(s.sessions), "pruned", pruned)
	return s
}

// apply folds one hook; it returns the updated copy, or nil when nothing
// was written.
func (s *contextStore) apply(ev contextEvent) (*SessionContext, error) {
	if !validSessionID(ev.sessionID) {
		return nil, nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.broken[ev.sessionID] {
		return nil, errors.New("context file unreadable")
	}
	// Log is never nil: the rail reads log.length, and a nil slice is null.
	next := SessionContext{Schema: 1, SessionID: ev.sessionID, Log: []ContextLogEntry{}}
	if cur := s.sessions[ev.sessionID]; cur != nil {
		next = *cur
		next.Log = append([]ContextLogEntry{}, cur.Log...)
	}
	now := s.now().UnixMilli()
	changed := next.touch(ev.wsID, ev.paneID, ev.cwd)
	switch {
	case ev.prompt != "":
		// Only the first prompt is kept; later ones are just a touch.
		if next.FirstPrompt == nil {
			if p := agent.ClipChars(ev.prompt, firstPromptMaxChars); p != "" {
				next.FirstPrompt, next.FirstPromptMs = &p, &now
				changed = true
			}
		}
	case ev.stop != nil:
		b := ev.stop
		if b.Goal != nil && strings.TrimSpace(*b.Goal) != "" {
			g := strings.TrimSpace(*b.Goal)
			next.Goal = &g
		}
		if b.Done != nil && strings.TrimSpace(*b.Done) != "" {
			d := strings.TrimSpace(*b.Done)
			next.DoneWhen = &d
		}
		next.push(ContextLogEntry{TsMs: now, Kind: "turn", Status: b.Status, Task: b.Task,
			Delta: b.Delta, Next: b.Next, Ask: b.Ask, Rec: b.Rec, Degraded: b.Degraded})
		changed = true
	case ev.ended:
		var task *string
		for i := len(next.Log) - 1; i >= 0; i-- {
			if next.Log[i].Task != nil {
				task = next.Log[i].Task
				break
			}
		}
		var delta *string
		if r := agent.ClipChars(ev.reason, 80); r != "" {
			delta = &r
		}
		next.push(ContextLogEntry{TsMs: now, Kind: "closed", Status: agent.BriefDone, Task: task, Delta: delta})
		changed = true
	}
	if !changed {
		return nil, nil
	}
	next.Version++
	if s.dir != "" {
		b, err := json.Marshal(next)
		if err != nil {
			return nil, err
		}
		if err := writeFileAtomic(filepath.Join(s.dir, ev.sessionID+".json"), b); err != nil {
			return nil, err
		}
	}
	s.sessions[ev.sessionID] = &next
	out := next
	return &out, nil
}

// list returns one workspace's sessions, most recent activity first.
func (s *contextStore) list(wsID string) []SessionContext {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := []SessionContext{}
	for _, c := range s.sessions {
		if c.WsID != nil && *c.WsID == wsID {
			out = append(out, *c)
		}
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].lastActivity() > out[j].lastActivity() })
	return out
}

// recordContext applies a hook and announces it. Best-effort: a failure is
// logged with ids only and never touches the hook's own answer.
func (r *HookRegistry) recordContext(ev contextEvent) {
	if r.context == nil || ev.sessionID == "" {
		return
	}
	c, err := r.context.apply(ev)
	if err != nil {
		logger.Warn("context update failed", "session", ev.sessionID, "err", err)
		return
	}
	if c == nil {
		return
	}
	logger.Debug("context updated", "session", ev.sessionID, "pane", ev.paneID, "log", len(c.Log), "v", c.Version)
	r.hub.publish("context:changed", same(map[string]any{"session_id": c.SessionID, "ws_id": c.WsID}))
}

// handleContextSessions: GET /api/v2/context/sessions?ws_id= — the rail's
// session_context_list.
func (s *Service) handleContextSessions(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil || s.hooks.context == nil {
		writeJSON(w, http.StatusOK, []SessionContext{})
		return
	}
	writeJSON(w, http.StatusOK, s.hooks.context.list(r.URL.Query().Get("ws_id")))
}
