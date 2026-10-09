package term

// webws.go — the browser's workspaces (Phase 103, WEB-DESIGN B5).
//
// WEB-DESIGN §4: server-native workspaces, `{id, name, layout, tabs_mode,
// intent, is_project_root}`, the layout stored opaque and written with an
// optimistic version. They live HERE, in <data dir>/web-workspaces.json, not
// in internal/workspace's SQLite (DECISIONS 2026-10-05): the agent verbs that
// change a layout (split, set-pane-title) run in this package, and reaching
// into a sibling subsystem's store would break the import rule that keeps
// `core` a leaf; and every field added to the huma-described Workspace there
// is an SDK regeneration for clients that do not use it. These are the same
// shape as the desktop's workspaces.json entries, so the frontend type does
// not fork.
//
// A pane of a browser workspace is a tmux session created through this API;
// its layout leaf's pane_id is that session's hook pane id (term_<hex>), and
// the session remembers its workspace (hookEntry.workspaceID) — which is what
// scopes an agent's `send` to its own workspace (Yossi, 2026-10-05).
//
// Every change announces `workspaces:changed` {workspace_id, version} on the
// events socket; a client holding an older version re-reads.
//
// Phase 115 (F1, the desktop's workspace tree in the browser): `meta` carries
// the tree fields of the desktop's Workspace — parent_id, cwd, is_folder,
// is_collapsed, sort_order, group_id, color, emoji, tmux_session — as an
// opaque JSON object. The browser owns their semantics (backend/web/tree.ts
// ports lib.rs); the daemon only stores them, like the layout. The groups
// list is one opaque document beside the workspaces, with its own version.
// A file written before F1 has flat rows (a layout, no meta); load turns each
// into a header + the old row as its screen, once (migrateFlat).

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"sync"
	"sync/atomic"
	"time"
)

// WebWorkspace is one browser workspace.
type WebWorkspace struct {
	ID            string          `json:"id"`
	Name          string          `json:"name"`
	Version       int64           `json:"version"`
	Layout        json.RawMessage `json:"layout,omitempty"`
	TabsMode      bool            `json:"tabs_mode,omitempty"`
	Intent        string          `json:"intent,omitempty"`
	IsProjectRoot bool            `json:"is_project_root,omitempty"`
	Meta          json.RawMessage `json:"meta,omitempty"` // F1: the desktop's tree fields, opaque
	CreatedAt     string          `json:"created_at"`
	UpdatedAt     string          `json:"updated_at"`
}

type webWSFile struct {
	Version       int             `json:"version"`
	Workspaces    []WebWorkspace  `json:"workspaces"`
	Groups        json.RawMessage `json:"groups,omitempty"` // F1: WorkspaceGroup[], opaque
	GroupsVersion int64           `json:"groups_version,omitempty"`
}

// WebGroups is the groups document a client reads and writes whole.
type WebGroups struct {
	Version int64           `json:"version"`
	Groups  json.RawMessage `json:"groups"`
}

var (
	errNoWorkspace = errors.New("no such workspace")
	errVersion     = errors.New("version conflict")
)

type webWSStore struct {
	mu      sync.Mutex
	path    string
	ws      []WebWorkspace
	groups  json.RawMessage
	groupsV int64
	seq     atomic.Uint64
	now  func() time.Time
}

func newWebWSStore(path string) *webWSStore {
	s := &webWSStore{path: path, now: time.Now}
	if path == "" {
		return s
	}
	var f webWSFile
	if loadJSON(path, &f, "web workspaces") {
		s.ws, s.groups, s.groupsV = f.Workspaces, f.Groups, f.GroupsVersion
	}
	if s.migrateFlat() {
		if err := s.saveLocked(); err != nil {
			logger.Warn("web workspaces: migration not saved", "err", err)
		} else {
			logger.Info("web workspaces: flat rows moved under headers", "count", len(s.ws))
		}
	}
	return s
}

// migrateFlat gives every pre-F1 row (a layout and no meta — it was a
// screen with nothing above it) a header of the same name, and makes the row
// that header's child. The row keeps its id, name and layout, so its panes'
// tmux sessions and the browsers' restore hints still match. Runs before any
// client can see the store; reports whether anything changed.
func (s *webWSStore) migrateFlat() bool {
	var out []WebWorkspace
	changed := false
	for _, w := range s.ws {
		if len(w.Meta) > 0 || len(w.Layout) == 0 || string(w.Layout) == "null" {
			out = append(out, w)
			continue
		}
		h := WebWorkspace{ID: s.newID(), Name: w.Name, Version: 1, Meta: json.RawMessage(`{}`),
			CreatedAt: w.CreatedAt, UpdatedAt: w.UpdatedAt}
		meta, _ := json.Marshal(map[string]string{"parent_id": h.ID})
		w.Meta = meta
		w.Version++
		out = append(out, h, w)
		changed = true
	}
	if changed {
		s.ws = out
	}
	return changed
}

func (s *webWSStore) newID() string {
	return fmt.Sprintf("w_%x_%x", s.now().UnixNano(), s.seq.Add(1)-1)
}

func (s *webWSStore) iso() string { return s.now().UTC().Format(time.RFC3339) }

func (s *webWSStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	// Compact, not indented: MarshalIndent re-indents the embedded layout
	// RawMessage, so a reload would hand clients a reformatted document.
	b, err := json.Marshal(webWSFile{Version: 1, Workspaces: s.ws, Groups: s.groups, GroupsVersion: s.groupsV})
	if err != nil {
		return err
	}
	return writeFileAtomic(s.path, b)
}

func (s *webWSStore) indexLocked(id string) int {
	for i := range s.ws {
		if s.ws[i].ID == id {
			return i
		}
	}
	return -1
}

func (s *webWSStore) list() []WebWorkspace {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]WebWorkspace{}, s.ws...)
}

func (s *webWSStore) get(id string) (WebWorkspace, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if i := s.indexLocked(id); i >= 0 {
		return s.ws[i], true
	}
	return WebWorkspace{}, false
}

func (s *webWSStore) exists(id string) bool { _, ok := s.get(id); return ok }

// workspaceCreate is POST's body: a name, and since F1 optionally the rest of
// the row, so a header + screen is two creates and not create-then-put.
type workspaceCreate struct {
	Name          string          `json:"name"`
	Layout        json.RawMessage `json:"layout"`
	TabsMode      bool            `json:"tabs_mode"`
	Intent        string          `json:"intent"`
	IsProjectRoot bool            `json:"is_project_root"`
	Meta          json.RawMessage `json:"meta"`
}

func (s *webWSStore) create(in workspaceCreate) (WebWorkspace, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.iso()
	w := WebWorkspace{ID: s.newID(), Name: in.Name, Version: 1, CreatedAt: now, UpdatedAt: now,
		TabsMode: in.TabsMode, Intent: in.Intent, IsProjectRoot: in.IsProjectRoot}
	if isJSONObject(in.Layout) {
		w.Layout = append(json.RawMessage{}, in.Layout...)
	}
	if isJSONObject(in.Meta) {
		w.Meta = append(json.RawMessage{}, in.Meta...)
	}
	s.ws = append(s.ws, w)
	if err := s.saveLocked(); err != nil {
		s.ws = s.ws[:len(s.ws)-1]
		return WebWorkspace{}, err
	}
	return w, nil
}

// workspacePut is the client's whole-document write.
type workspacePut struct {
	Version       int64           `json:"version"`
	Name          *string         `json:"name"`
	Layout        json.RawMessage `json:"layout"`
	TabsMode      *bool           `json:"tabs_mode"`
	Intent        *string         `json:"intent"`
	IsProjectRoot *bool           `json:"is_project_root"`
	Meta          json.RawMessage `json:"meta"` // absent = keep; an object replaces
}

// put applies a client write if in.Version is the stored version
// (last-writer-wins with a guard — WEB-DESIGN §10, no CRDT). On a mismatch it
// returns errVersion with the CURRENT document so the client can rebase.
func (s *webWSStore) put(id string, in workspacePut) (WebWorkspace, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	i := s.indexLocked(id)
	if i < 0 {
		return WebWorkspace{}, errNoWorkspace
	}
	if in.Version != s.ws[i].Version {
		return s.ws[i], errVersion
	}
	prev := s.ws[i]
	w := &s.ws[i]
	if in.Name != nil && *in.Name != "" {
		w.Name = *in.Name
	}
	if in.Layout != nil {
		if string(in.Layout) == "null" {
			w.Layout = nil
		} else {
			w.Layout = append(json.RawMessage{}, in.Layout...)
		}
	}
	if in.TabsMode != nil {
		w.TabsMode = *in.TabsMode
	}
	if in.Intent != nil {
		w.Intent = *in.Intent
	}
	if in.IsProjectRoot != nil {
		w.IsProjectRoot = *in.IsProjectRoot
	}
	if isJSONObject(in.Meta) {
		w.Meta = append(json.RawMessage{}, in.Meta...)
	}
	w.Version++
	w.UpdatedAt = s.iso()
	if err := s.saveLocked(); err != nil {
		s.ws[i] = prev
		return WebWorkspace{}, err
	}
	return *w, nil
}

// mutateLayout runs a daemon-side tree operation (split, title) against the
// stored layout and bumps the version. fn gets nil for an empty layout.
func (s *webWSStore) mutateLayout(id string, fn func(node) (node, error)) (WebWorkspace, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	i := s.indexLocked(id)
	if i < 0 {
		return WebWorkspace{}, errNoWorkspace
	}
	root, err := parseLayout(s.ws[i].Layout)
	if err != nil {
		return WebWorkspace{}, fmt.Errorf("stored layout unreadable: %w", err)
	}
	out, err := fn(root)
	if err != nil {
		return WebWorkspace{}, err
	}
	b, err := json.Marshal(out)
	if err != nil {
		return WebWorkspace{}, err
	}
	prev := s.ws[i]
	s.ws[i].Layout, s.ws[i].Version, s.ws[i].UpdatedAt = b, s.ws[i].Version+1, s.iso()
	if err := s.saveLocked(); err != nil {
		s.ws[i] = prev
		return WebWorkspace{}, err
	}
	return s.ws[i], nil
}

func (s *webWSStore) remove(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	i := s.indexLocked(id)
	if i < 0 {
		return errNoWorkspace
	}
	prev := s.ws
	s.ws = append(append([]WebWorkspace{}, s.ws[:i]...), s.ws[i+1:]...)
	if err := s.saveLocked(); err != nil {
		s.ws = prev
		return err
	}
	return nil
}

// getGroups returns the groups document ("[]" when none was ever written).
func (s *webWSStore) getGroups() WebGroups {
	s.mu.Lock()
	defer s.mu.Unlock()
	g := s.groups
	if len(g) == 0 {
		g = json.RawMessage(`[]`)
	}
	return WebGroups{Version: s.groupsV, Groups: g}
}

// putGroups replaces the groups list if version matches (409 otherwise, with
// the current document — the workspace rule).
func (s *webWSStore) putGroups(in WebGroups) (WebGroups, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	cur := func() WebGroups {
		g := s.groups
		if len(g) == 0 {
			g = json.RawMessage(`[]`)
		}
		return WebGroups{Version: s.groupsV, Groups: g}
	}
	if in.Version != s.groupsV {
		return cur(), errVersion
	}
	prevG, prevV := s.groups, s.groupsV
	s.groups, s.groupsV = append(json.RawMessage{}, in.Groups...), s.groupsV+1
	if err := s.saveLocked(); err != nil {
		s.groups, s.groupsV = prevG, prevV
		return WebGroups{}, err
	}
	return cur(), nil
}

// isJSONObject: a non-empty raw value that is a JSON object.
func isJSONObject(b json.RawMessage) bool {
	var m map[string]json.RawMessage
	return len(b) > 0 && b[0] == '{' && json.Unmarshal(b, &m) == nil
}

func isJSONArray(b json.RawMessage) bool {
	var a []json.RawMessage
	return len(b) > 0 && b[0] == '[' && json.Unmarshal(b, &a) == nil
}

func (r *HookRegistry) workspacesChanged(id string, version int64) {
	logger.Info("workspace changed", "workspace", id, "version", version)
	r.hub.publish("workspaces:changed", same(map[string]any{"workspace_id": id, "version": version}))
}

// ── shared file helpers (notes.go uses them too) ───────────────────────

// writeFileAtomic writes tmp + fsync + rename (Rule #7).
func writeFileAtomic(path string, b []byte) error {
	tmp := fmt.Sprintf("%s.%d.tmp", path, os.Getpid())
	f, err := os.OpenFile(tmp, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	if _, err := f.Write(b); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// loadJSON reads path into v. A missing file is a quiet false; a file that
// will not parse is moved aside (never overwritten) and reported.
func loadJSON(path string, v any, what string) bool {
	b, err := os.ReadFile(path)
	if err != nil {
		if !os.IsNotExist(err) {
			logger.Warn(what+" file unreadable; starting empty", "err", err)
		}
		return false
	}
	if err := json.Unmarshal(b, v); err != nil {
		bad := path + ".corrupt-" + time.Now().UTC().Format("20060102T150405")
		_ = os.Rename(path, bad)
		logger.Warn(what+" file corrupt; moved aside", "to", bad)
		return false
	}
	return true
}

// ── REST (behind Service.gate) ─────────────────────────────────────────

func (s *Service) handleWebWorkspaces(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "workspaces unavailable", http.StatusServiceUnavailable)
		return
	}
	st := s.hooks.webws
	if r.Method == http.MethodGet {
		writeJSON(w, http.StatusOK, st.list())
		return
	}
	var body workspaceCreate
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&body); err != nil && !errors.Is(err, io.EOF) {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	if body.Name == "" {
		http.Error(w, "name required", http.StatusBadRequest)
		return
	}
	if (len(body.Layout) > 0 && string(body.Layout) != "null" && !isJSONObject(body.Layout)) ||
		(len(body.Meta) > 0 && string(body.Meta) != "null" && !isJSONObject(body.Meta)) {
		http.Error(w, "layout and meta must be JSON objects", http.StatusBadRequest)
		return
	}
	ws, err := st.create(body)
	if err != nil {
		logger.Error("workspace create failed", "err", err)
		http.Error(w, "save failed", http.StatusInternalServerError)
		return
	}
	s.hooks.workspacesChanged(ws.ID, ws.Version)
	writeJSON(w, http.StatusCreated, ws)
}

func (s *Service) handleWebWorkspace(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "workspaces unavailable", http.StatusServiceUnavailable)
		return
	}
	st, id := s.hooks.webws, r.PathValue("id")
	switch r.Method {
	case http.MethodGet:
		ws, ok := st.get(id)
		if !ok {
			http.Error(w, errNoWorkspace.Error(), http.StatusNotFound)
			return
		}
		writeJSON(w, http.StatusOK, ws)
	case http.MethodDelete:
		if err := st.remove(id); err != nil {
			code := http.StatusInternalServerError
			if errors.Is(err, errNoWorkspace) {
				code = http.StatusNotFound
			}
			http.Error(w, err.Error(), code)
			return
		}
		s.hooks.workspacesChanged(id, 0)
		w.WriteHeader(http.StatusNoContent)
	case http.MethodPut:
		var in workspacePut
		if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
			http.Error(w, "bad body", http.StatusBadRequest)
			return
		}
		if len(in.Layout) > 0 && string(in.Layout) != "null" {
			if _, err := parseLayout(in.Layout); err != nil {
				http.Error(w, "layout is not a JSON object", http.StatusBadRequest)
				return
			}
		}
		if len(in.Meta) > 0 && string(in.Meta) != "null" && !isJSONObject(in.Meta) {
			http.Error(w, "meta is not a JSON object", http.StatusBadRequest)
			return
		}
		ws, err := st.put(id, in)
		switch {
		case errors.Is(err, errVersion):
			// 409 with the current document: the client rebases and retries.
			writeJSON(w, http.StatusConflict, ws)
		case errors.Is(err, errNoWorkspace):
			http.Error(w, err.Error(), http.StatusNotFound)
		case err != nil:
			logger.Error("workspace save failed", "workspace", id, "err", err)
			http.Error(w, "save failed", http.StatusInternalServerError)
		default:
			s.hooks.workspacesChanged(ws.ID, ws.Version)
			writeJSON(w, http.StatusOK, ws)
		}
	}
}

// handleWebGroups: GET the groups document, PUT it whole with its version.
func (s *Service) handleWebGroups(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "workspaces unavailable", http.StatusServiceUnavailable)
		return
	}
	st := s.hooks.webws
	if r.Method == http.MethodGet {
		writeJSON(w, http.StatusOK, st.getGroups())
		return
	}
	var in WebGroups
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 256<<10)).Decode(&in); err != nil || !isJSONArray(in.Groups) {
		http.Error(w, "body must be {version, groups: [...]}", http.StatusBadRequest)
		return
	}
	g, err := st.putGroups(in)
	switch {
	case errors.Is(err, errVersion):
		writeJSON(w, http.StatusConflict, g)
	case err != nil:
		logger.Error("workspace groups save failed", "err", err)
		http.Error(w, "save failed", http.StatusInternalServerError)
	default:
		s.hooks.workspacesChanged("", 0)
		writeJSON(w, http.StatusOK, g)
	}
}
