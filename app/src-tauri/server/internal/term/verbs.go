package term

// verbs.go — the small hook-RPC verbs of a browser-created session (Phase
// 102, WEB-DESIGN B4): set-status, notify, note-*, port.opened/closed.
//
// Each is the desktop's dispatch() arm (rpc_server.rs) ported, answering the
// same JSON the CLI already expects, and announcing the same event on the
// events socket that the desktop emits to its webview:
//
//	set-status        → pane:status       {pane_id, text}
//	notify            → notification:new  NotificationItem
//	note-*            → notes:changed     (no data — clients re-list)
//	port.opened/closed→ port-detected / port-undetected (ports.go)
//
// A verb names a pane only to say "mine": set-status for ANOTHER pane is
// refused, the same defense feed.push applies. Logs carry ids, never text.

import (
	"encoding/json"
	"errors"
	"net/http"
	"strconv"
	"sync"
	"sync/atomic"

	"ymux-server/internal/core"
)

// NotificationItem is the desktop's (lib.rs), plus `session` — the tmux
// session it came from, since there is no workspace to name on the daemon.
type NotificationItem struct {
	ID          uint64  `json:"id"`
	Title       string  `json:"title"`
	Body        string  `json:"body"`
	WorkspaceID *string `json:"workspace_id"`
	TimestampMs int64   `json:"timestamp_ms"`
	Kind        string  `json:"kind"`
	Session     string  `json:"session,omitempty"`
}

// notifMax bounds the in-memory list; the desktop's is unbounded, which on a
// daemon that runs for months is a leak.
const notifMax = 200

type notifStore struct {
	mu    sync.Mutex
	items []NotificationItem
	next  atomic.Uint64
}

func (s *notifStore) add(it NotificationItem) NotificationItem {
	it.ID = s.next.Add(1)
	s.mu.Lock()
	s.items = append(s.items, it)
	if over := len(s.items) - notifMax; over > 0 {
		s.items = append(s.items[:0:0], s.items[over:]...)
	}
	s.mu.Unlock()
	return it
}

func (s *notifStore) list() []NotificationItem {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]NotificationItem{}, s.items...)
}

func (s *notifStore) clear() {
	s.mu.Lock()
	s.items = nil
	s.mu.Unlock()
}

// rpcErr is the -32000 error the desktop's dispatch returns for a bad call.
func rpcErr(msg string) *core.RPCError { return &core.RPCError{Code: -32000, Message: msg} }

// verbParams is the union of the fields the B4 verbs read.
type verbParams struct {
	PaneID      string           `json:"pane_id"`
	Pane        string           `json:"pane"` // set-status's older spelling
	Text        *string          `json:"text"`
	Title       string           `json:"title"`
	Body        string           `json:"body"`
	Kind        string           `json:"kind"`
	ID          string           `json:"id"`
	// absent (len 0) vs null vs "": a RawMessage keeps the literal `null`,
	// where a pointer field would turn it into nil — indistinguishable from
	// absent, and note-update's null means "clear the tag".
	Tag         json.RawMessage  `json:"tag"`
	Status      string           `json:"status"`
	WorkspaceID string           `json:"workspace_id"`
	Limit       int              `json:"limit"`
	Port        uint16           `json:"port"`
	Addr        string           `json:"addr"`
	Family      string           `json:"family"`
}

// tagValue reads note-update's tag: (nil, false) when absent; a JSON null
// or "" clears it.
func (p verbParams) tagValue() (*string, bool) {
	if len(p.Tag) == 0 {
		return nil, false
	}
	var s string
	_ = json.Unmarshal(p.Tag, &s) // null → ""
	return &s, true
}

// verb runs one B4 method. ok is false when method is not a B4 verb.
func (t termHookTarget) verb(method string, raw json.RawMessage) (res any, err *core.RPCError, ok bool) {
	var p verbParams
	if len(raw) > 0 && json.Unmarshal(raw, &p) != nil {
		return nil, rpcErr("bad params"), true
	}
	r := t.r
	r.mu.Lock()
	pane, session := t.e.paneID, t.e.name
	r.mu.Unlock()

	switch method {
	case "set-status":
		target := p.PaneID
		if target == "" {
			target = p.Pane
		}
		if target == "" {
			return nil, rpcErr("missing pane_id"), true
		}
		if target != pane {
			logger.Warn("set-status for another pane — refused", "pane", pane, "claimed", target)
			return nil, rpcErr("pane_id is not this session's pane"), true
		}
		text := ""
		if p.Text != nil {
			text = *p.Text
		}
		r.mu.Lock()
		t.e.status = text
		r.mu.Unlock()
		r.hub.publish("pane:status", same(map[string]string{"pane_id": pane, "text": text}))
		return map[string]any{"ok": true}, nil, true

	case "notify":
		title, kind := p.Title, p.Kind
		if title == "" {
			title = "(no title)"
		}
		if kind == "" {
			kind = "agent"
		}
		var ws *string
		if p.WorkspaceID != "" {
			ws = &p.WorkspaceID
		}
		it := r.notifs.add(NotificationItem{Title: title, Body: p.Body, WorkspaceID: ws,
			TimestampMs: r.now().UnixMilli(), Kind: kind, Session: session})
		logger.Info("notification", "id", it.ID, "pane", pane, "kind", kind)
		r.hub.publish("notification:new", same(it))
		return map[string]any{"ok": true, "id": it.ID}, nil, true

	case "note-add":
		if p.Text == nil {
			return nil, rpcErr("missing text"), true
		}
		paneID := p.PaneID
		if paneID == "" {
			paneID = pane
		}
		tag := ""
		if v, ok := p.tagValue(); ok {
			tag = *v
		}
		n, err := r.notes.add(*p.Text, tag, p.WorkspaceID, paneID)
		if err != nil {
			logger.Error("note add failed", "err", err)
			return nil, rpcErr("note save failed"), true
		}
		r.notesChanged("add", n.ID)
		return n, nil, true

	case "note-list":
		tag := ""
		if v, ok := p.tagValue(); ok {
			tag = *v
		}
		status := p.Status
		if !validNoteStatus(status) {
			status = ""
		}
		return r.notes.list(tag, status, p.WorkspaceID, p.Limit), nil, true

	case "note-update", "note-done":
		if p.ID == "" {
			return nil, rpcErr("missing id"), true
		}
		var tag, status *string
		if method == "note-done" {
			done := "done"
			status = &done
		} else {
			tag, _ = p.tagValue()
			if validNoteStatus(p.Status) {
				status = &p.Status
			}
		}
		text := p.Text
		if method == "note-done" {
			text = nil
		}
		n, err := r.notes.update(p.ID, text, tag, status)
		if err != nil {
			return nil, rpcErr(noteErr(err, p.ID)), true
		}
		r.notesChanged("update", n.ID)
		return n, nil, true

	case "note-delete":
		if p.ID == "" {
			return nil, rpcErr("missing id"), true
		}
		if err := r.notes.remove(p.ID); err != nil {
			return nil, rpcErr(noteErr(err, p.ID)), true
		}
		r.notesChanged("delete", p.ID)
		return map[string]any{"ok": true}, nil, true

	case "port.opened":
		if p.Port == 0 {
			return nil, rpcErr("missing port"), true
		}
		addr, family := p.Addr, p.Family
		if addr == "" {
			addr = "127.0.0.1"
		}
		if family == "" {
			family = "v4"
		}
		if r.ports != nil {
			r.ports.opened(ListenPort{Addr: addr, RemotePort: p.Port, Family: family})
		}
		return map[string]any{"ok": true, "detected": true}, nil, true

	case "port.closed":
		if p.Port == 0 {
			return nil, rpcErr("missing port"), true
		}
		if r.ports != nil {
			r.ports.closed(p.Port)
		}
		return map[string]any{"ok": true}, nil, true
	}
	return nil, nil, false
}

func noteErr(err error, id string) string {
	if errors.Is(err, errNoNote) {
		return "no note " + id
	}
	logger.Error("note save failed", "id", id, "err", err)
	return "note save failed"
}

func (r *HookRegistry) notesChanged(op, id string) {
	logger.Info("notes changed", "op", op, "id", id)
	r.hub.publish("notes:changed", same(nil))
}

// ── REST, for the browser UI (behind Service.gate) ─────────────────────

// handleNotes serves GET /api/v2/notes?tag=&status=&limit= and POST /api/v2/notes.
func (s *Service) handleNotes(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "notes unavailable", http.StatusServiceUnavailable)
		return
	}
	reg := s.hooks
	if r.Method == http.MethodGet {
		q := r.URL.Query()
		limit, _ := strconv.Atoi(q.Get("limit"))
		status := q.Get("status")
		if !validNoteStatus(status) {
			status = ""
		}
		writeJSON(w, http.StatusOK, reg.notes.list(q.Get("tag"), status, q.Get("workspace_id"), limit))
		return
	}
	var body struct {
		Text   string `json:"text"`
		Tag    string `json:"tag"`
		PaneID string `json:"pane_id"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.Text == "" {
		http.Error(w, "text required", http.StatusBadRequest)
		return
	}
	n, err := reg.notes.add(body.Text, body.Tag, "", body.PaneID)
	if err != nil {
		http.Error(w, noteErr(err, ""), http.StatusInternalServerError)
		return
	}
	reg.notesChanged("add", n.ID)
	writeJSON(w, http.StatusCreated, n)
}

// handleNote serves PATCH and DELETE /api/v2/notes/{id}.
func (s *Service) handleNote(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "notes unavailable", http.StatusServiceUnavailable)
		return
	}
	reg, id := s.hooks, r.PathValue("id")
	if r.Method == http.MethodDelete {
		if err := reg.notes.remove(id); err != nil {
			code := http.StatusInternalServerError
			if errors.Is(err, errNoNote) {
				code = http.StatusNotFound
			}
			http.Error(w, noteErr(err, id), code)
			return
		}
		reg.notesChanged("delete", id)
		w.WriteHeader(http.StatusNoContent)
		return
	}
	var body struct {
		Text   *string `json:"text"`
		Tag    *string `json:"tag"`
		Status *string `json:"status"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.Status != nil && !validNoteStatus(*body.Status) {
		http.Error(w, `status must be "open" or "done"`, http.StatusBadRequest)
		return
	}
	n, err := reg.notes.update(id, body.Text, body.Tag, body.Status)
	if err != nil {
		code := http.StatusInternalServerError
		if errors.Is(err, errNoNote) {
			code = http.StatusNotFound
		}
		http.Error(w, noteErr(err, id), code)
		return
	}
	reg.notesChanged("update", id)
	writeJSON(w, http.StatusOK, n)
}

// handleClearNotifications is DELETE /api/v2/notifications.
func (s *Service) handleClearNotifications(w http.ResponseWriter, _ *http.Request) {
	if s.hooks == nil {
		http.Error(w, "notifications unavailable", http.StatusServiceUnavailable)
		return
	}
	s.hooks.notifs.clear()
	s.hooks.hub.publish("notifications:cleared", same(nil))
	w.WriteHeader(http.StatusNoContent)
}
