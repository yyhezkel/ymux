package term

// service.go — /api/v2/term/*: list, create, rename, kill, attach.
//
// Every route in this package sits behind ONE gate (see gate below) and that
// gate demands auth.ScopeShellAttach. Listing is gated too, not just attach:
// session names carry project and branch names, and a token that cannot open a
// terminal has no reason to enumerate them.
//
// The routes are mounted raw rather than behind api's Bearer middleware,
// because that middleware only knows the shared token and this package must
// also accept a paired device's token — the same reason push mounts raw.

import (
	"encoding/json"
	"errors"
	"net/http"
	"strings"

	"github.com/google/uuid"

	"ymux-server/internal/auth"
)

// ScopeResolver maps a bearer token to its grants. Satisfied by
// chat.ChatAPI.DeviceScopes; nil when the chat subsystem failed to start, in
// which case only the shared (owner) token is accepted.
type ScopeResolver func(token string) (scopes string, admin, ok bool)

// Service serves the terminal API over a Tmux.
type Service struct {
	tmux   *Tmux
	token  string // shared/owner token
	scopes ScopeResolver
	home   string // for session-meta.json
}

// NewService wires the terminal API. token is the daemon's shared token; home
// is the user's home directory (where ~/.ymux/session-meta.json lives).
func NewService(token, home string) *Service {
	return &Service{tmux: NewTmux(), token: token, home: home}
}

// SetScopeResolver wires per-device scope lookups. Without it only the owner
// token can reach these routes.
func (s *Service) SetScopeResolver(fn ScopeResolver) { s.scopes = fn }

// RegisterRoutes mounts /api/v2/term/*.
func (s *Service) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("GET /api/v2/term/sessions", s.gate(s.handleList))
	mux.HandleFunc("POST /api/v2/term/sessions", s.gate(s.handleCreate))
	mux.HandleFunc("POST /api/v2/term/sessions/{name}/rename", s.gate(s.handleRename))
	mux.HandleFunc("DELETE /api/v2/term/sessions/{name}", s.gate(s.handleKill))
	mux.HandleFunc("GET /api/v2/term/sessions/{name}/attach", s.gate(s.handleAttach))
}

// bearer pulls the token from the Authorization header, falling back to
// ?token= — a browser cannot set headers on a WebSocket handshake, and the
// attach route is a WebSocket. Same concession ws.go makes.
func bearer(r *http.Request) string {
	if h := r.Header.Get("Authorization"); h != "" {
		return strings.TrimPrefix(h, "Bearer ")
	}
	return r.URL.Query().Get("token")
}

// gate authorizes a request and rejects it otherwise.
//
// It FAILS CLOSED: a Service with no shared token and no resolver rejects
// everything. That is the opposite of the workspace subsystem's "no auth
// configured ⇒ open" convenience, and deliberately so — the thing behind this
// door is a shell.
func (s *Service) gate(h http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		tok := bearer(r)
		if tok == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		if s.token != "" && tok == s.token {
			h(w, r) // the owner/desktop token
			return
		}
		if s.scopes == nil {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		scopes, admin, ok := s.scopes(tok)
		if !ok {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		if admin || auth.HasScope(scopes, auth.ScopeShellAttach) {
			h(w, r)
			return
		}
		// 403, not 401: the token is real, it just was not granted a shell.
		// Retrying with the same credential will not help, and the message is
		// what a UI should show the user.
		logger.Warn("terminal access denied: missing shell:attach grant")
		http.Error(w, "forbidden: shell:attach not granted", http.StatusForbidden)
	}
}

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

// failErr maps a package error to a status.
func failErr(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, ErrNoSession):
		http.Error(w, err.Error(), http.StatusNotFound)
	case errors.Is(err, ErrBadName):
		http.Error(w, err.Error(), http.StatusBadRequest)
	default:
		logger.Error("tmux command failed", "err", err)
		http.Error(w, "tmux command failed", http.StatusInternalServerError)
	}
}

func (s *Service) handleList(w http.ResponseWriter, _ *http.Request) {
	sessions, err := s.tmux.List()
	if err != nil {
		failErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, Annotate(sessions, LoadMeta(s.home)))
}

func (s *Service) handleCreate(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name string `json:"name"`
		Cwd  string `json:"cwd"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	name := strings.TrimSpace(body.Name)
	if name == "" {
		name = "ymux-" + uuid.NewString()[:8]
	}
	if !ValidName(name) {
		failErr(w, ErrBadName)
		return
	}
	// Checked rather than left to tmux's "duplicate session" error so the
	// caller gets a status it can branch on instead of a 500.
	if s.tmux.Has(name) {
		http.Error(w, "session already exists", http.StatusConflict)
		return
	}
	if err := s.tmux.Create(name, body.Cwd); err != nil {
		failErr(w, err)
		return
	}
	writeJSON(w, http.StatusCreated, map[string]any{"name": name, "display": name})
}

func (s *Service) handleRename(w http.ResponseWriter, r *http.Request) {
	var body struct {
		NewName string `json:"new_name"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	to := strings.TrimSpace(body.NewName)
	from := r.PathValue("name")
	if !ValidName(to) {
		failErr(w, ErrBadName)
		return
	}
	if s.tmux.Has(to) && to != from {
		http.Error(w, "session already exists", http.StatusConflict)
		return
	}
	if err := s.tmux.Rename(from, to); err != nil {
		failErr(w, err)
		return
	}
	// The session-meta entry is keyed by NAME, so a rename orphans it. The
	// CLI's hooks re-key it on the session's next turn and its pruning drops
	// the stale key, so this is self-healing rather than something to patch up
	// from here — and writing that file from the daemon would put a second
	// writer on it (the CLI's atomic tmp+rename assumes one).
	writeJSON(w, http.StatusOK, map[string]any{"name": to})
}

func (s *Service) handleKill(w http.ResponseWriter, r *http.Request) {
	if err := s.tmux.Kill(r.PathValue("name")); err != nil {
		failErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}
