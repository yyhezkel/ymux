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

import (
	"encoding/json"
	"errors"
	"fmt"
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
	CreatedAt     string          `json:"created_at"`
	UpdatedAt     string          `json:"updated_at"`
}

type webWSFile struct {
	Version    int            `json:"version"`
	Workspaces []WebWorkspace `json:"workspaces"`
}

var (
	errNoWorkspace = errors.New("no such workspace")
	errVersion     = errors.New("version conflict")
)

type webWSStore struct {
	mu   sync.Mutex
	path string
	ws   []WebWorkspace
	seq  atomic.Uint64
	now  func() time.Time
}

func newWebWSStore(path string) *webWSStore {
	s := &webWSStore{path: path, now: time.Now}
	if path == "" {
		return s
	}
	var f webWSFile
	if loadJSON(path, &f, "web workspaces") {
		s.ws = f.Workspaces
	}
	return s
}

func (s *webWSStore) iso() string { return s.now().UTC().Format(time.RFC3339) }

func (s *webWSStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	b, err := json.MarshalIndent(webWSFile{Version: 1, Workspaces: s.ws}, "", "  ")
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

func (s *webWSStore) create(name string) (WebWorkspace, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.iso()
	w := WebWorkspace{ID: fmt.Sprintf("w_%x_%x", s.now().UnixNano(), s.seq.Add(1)-1), Name: name,
		Version: 1, CreatedAt: now, UpdatedAt: now}
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
	var body struct {
		Name string `json:"name"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.Name == "" {
		http.Error(w, "name required", http.StatusBadRequest)
		return
	}
	ws, err := st.create(body.Name)
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
