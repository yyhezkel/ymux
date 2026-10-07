package term

// service.go — /api/v2/term/*: the gate, the shared plumbing, and every raw
// route. The four REST ops list/create/rename/kill live in huma.go as huma
// operations (api's bearerMiddleware guards them); attach and the rest stay
// raw here.
//
// Every route in this package sits behind ONE gate (see gate below) and that
// gate demands auth.ScopeShellAttach. Listing is gated too, not just attach:
// session names carry project and branch names, and a token that cannot open a
// terminal has no reason to enumerate them.
//
// The routes below are mounted raw rather than behind api's Bearer middleware,
// because that middleware only knows the shared token and this package must
// also accept a paired device's token — the same reason push mounts raw.

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"path/filepath"
	"strconv"
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

	// logBudget bounds the diagnostic page's log sink (page.go).
	logBudget *logBudget

	// hooks holds the hook tokens of the sessions created here (Phase 100,
	// hookreg.go). nil disables hook routing entirely.
	hooks *HookRegistry

	// claudeBin overrides the resolved `claude` path for a resume (Phase
	// 104, history.go); tests set it, production resolves it.
	claudeBin string

	// webRoot is <data dir>/www/current, the installed web bundle (Phase 108,
	// webapp.go); "" serves the diagnostic page at `/`.
	webRoot string

	// webpush sends the browser's notifications (Phase 114, webpush.go);
	// nil until SetWebPush.
	webpush *webPusher
}

// NewService wires the terminal API. token is the daemon's shared token; home
// is the user's home directory (where ~/.ymux/session-meta.json lives).
func NewService(token, home string) *Service {
	s := &Service{tmux: NewTmux(), token: token, home: home, logBudget: &logBudget{}}
	s.attachHooks(NewHookRegistry())
	return s
}

// attachHooks gives the service its registry and the registry what an
// agent's split needs from the service (Phase 103): tmux, and spawnSession.
func (s *Service) attachHooks(r *HookRegistry) {
	s.hooks = r
	r.tmux = s.tmux
	r.spawn = func(name, cwd, policy, workspaceID string) (hookEntry, error) {
		e, hooks, err := s.spawnSession(name, cwd, policy, workspaceID, "")
		if err == nil && !hooks {
			// A split pane must be addressable by pane id; one without hook
			// routing (tmux < 3.2, no listener) is not. Undo it.
			_ = s.tmux.Kill(e.name)
			return hookEntry{}, errors.New("hook routing unavailable")
		}
		return e, err
	}
}

// Hooks is the registry hooks.Start must be given, so a claude inside a
// browser-created session reaches the daemon (core.HookResolver + AddrSink).
func (s *Service) Hooks() *HookRegistry { return s.hooks }

// SetDataDir makes notes persistent in <dir>/notes.json (Phase 102). Without
// it notes live in memory only. Call before serving.
func (s *Service) SetDataDir(dir string) {
	if s.hooks != nil {
		s.hooks.notes = newNoteStore(filepath.Join(dir, "notes.json"))
		s.hooks.webws = newWebWSStore(filepath.Join(dir, "web-workspaces.json"))
		s.hooks.settings = newSettingsStore(filepath.Join(dir, "web-settings.json"))
		s.hooks.context = newContextStore(filepath.Join(dir, "context", "sessions"))
	}
}

// StartPortWatch starts listening-port detection for the box (Phase 102,
// ports.go) until ctx ends. apiPort is the daemon's own HTTP port, never
// reported; the hook listener's port is excluded the same way.
func (s *Service) StartPortWatch(ctx context.Context, apiPort int) {
	if s.hooks == nil {
		return
	}
	reg := s.hooks
	reg.ports = newPortWatch(reg.hub, func() []uint16 {
		own := []uint16{uint16(apiPort)}
		if _, p, err := net.SplitHostPort(reg.hookAddr()); err == nil {
			if n, err := strconv.ParseUint(p, 10, 16); err == nil {
				own = append(own, uint16(n))
			}
		}
		return own
	})
	go reg.ports.run(ctx)
	logger.Info("port detection enabled")
}

// SetScopeResolver wires per-device scope lookups. Without it only the owner
// token can reach these routes.
func (s *Service) SetScopeResolver(fn ScopeResolver) { s.scopes = fn }

// RegisterRoutes mounts /api/v2/term/*.
func (s *Service) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("GET /api/v2/term/sessions/{name}/attach", s.gate(s.handleAttach))
	// Phase 101 (WEB-DESIGN B3): the live channel, the feed decision, and a
	// session's hook policy — same gate (events.go explains why).
	mux.HandleFunc("POST /api/v2/term/sessions/{name}/policy", s.gate(s.handlePolicy))
	mux.HandleFunc("GET /api/v2/events", s.gate(s.handleEvents))
	mux.HandleFunc("POST /api/v2/feed/{request_id}/decide", s.gate(s.handleDecide))
	// Phase 102 (B4): notes and notifications for the browser UI (verbs.go).
	mux.HandleFunc("GET /api/v2/notes", s.gate(s.handleNotes))
	mux.HandleFunc("POST /api/v2/notes", s.gate(s.handleNotes))
	mux.HandleFunc("PATCH /api/v2/notes/{id}", s.gate(s.handleNote))
	mux.HandleFunc("DELETE /api/v2/notes/{id}", s.gate(s.handleNote))
	mux.HandleFunc("DELETE /api/v2/notifications", s.gate(s.handleClearNotifications))
	// Phase 103 (B5): the browser's workspaces + layout document (webws.go).
	mux.HandleFunc("GET /api/v2/web/workspaces", s.gate(s.handleWebWorkspaces))
	mux.HandleFunc("POST /api/v2/web/workspaces", s.gate(s.handleWebWorkspaces))
	mux.HandleFunc("GET /api/v2/web/workspaces/{id}", s.gate(s.handleWebWorkspace))
	mux.HandleFunc("PUT /api/v2/web/workspaces/{id}", s.gate(s.handleWebWorkspace))
	mux.HandleFunc("DELETE /api/v2/web/workspaces/{id}", s.gate(s.handleWebWorkspace))
	// Phase 115 (F1): the workspace tree's groups, and git for a folder.
	mux.HandleFunc("GET /api/v2/web/groups", s.gate(s.handleWebGroups))
	mux.HandleFunc("PUT /api/v2/web/groups", s.gate(s.handleWebGroups))
	mux.HandleFunc("POST /api/v2/web/git/worktrees", s.gate(s.handleGitWorktrees))
	// Phase 117 (F3): the resume picker's claude sessions.
	mux.HandleFunc("GET /api/v2/claude/sessions", s.gate(s.handleClaudeSessions))
	mux.HandleFunc("GET /api/v2/context/sessions", s.gate(s.handleContextSessions))
	// Phase 118 (F4): the Diff pane's snapshot and worktree creation.
	mux.HandleFunc("POST /api/v2/git/diff", s.gate(s.handleGitDiff))
	mux.HandleFunc("POST /api/v2/git/worktree-add", s.gate(s.handleGitWorktreeAdd))
	// Phase 104 (B6): session history — ended rows, their transcript, resume.
	// Phase 114 (E): Web Push subscriptions for the browser's notifications.
	mux.HandleFunc("GET /api/v2/webpush/key", s.gate(s.handleWebPushKey))
	mux.HandleFunc("POST /api/v2/webpush/subscriptions", s.gate(s.handleWebPushSubs))
	mux.HandleFunc("DELETE /api/v2/webpush/subscriptions", s.gate(s.handleWebPushSubs))
	mux.HandleFunc("POST /api/v2/webpush/test", s.gate(s.handleWebPushTest))
	mux.HandleFunc("GET /api/v2/settings", s.gate(s.handleSettings))
	mux.HandleFunc("PUT /api/v2/settings", s.gate(s.handleSettings))
	mux.HandleFunc("GET /api/v2/term/history", s.gate(s.handleHistory))
	mux.HandleFunc("POST /api/v2/term/history/{name}/resume", s.gate(s.handleResume))
	mux.HandleFunc("GET /api/v2/claude/sessions/{id}/transcript", s.gate(s.handleTranscript))
	// Phase 97: the diagnostic page + its log sink, both public (page.go).
	s.registerPageRoutes(mux)
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
		// Every rejection says WHY in the log. Rule #8: the reason, never the
		// token — "which token was it" is not a question worth a credential in
		// a log file, and "why was it refused" is the only one being asked.
		deny := func(code int, why string) {
			logger.Warn("terminal request refused", "why", why,
				"path", r.URL.Path, "ip", clientIP(r))
			http.Error(w, why, code)
		}
		tok := bearer(r)
		if tok == "" {
			deny(http.StatusUnauthorized, "no bearer token")
			return
		}
		if s.token != "" && tok == s.token {
			logger.Debug("terminal request authorized", "as", "owner", "path", r.URL.Path)
			h(w, r) // the owner/desktop token
			return
		}
		if s.scopes == nil {
			deny(http.StatusUnauthorized, "unauthorized")
			return
		}
		scopes, admin, ok := s.scopes(tok)
		if !ok {
			deny(http.StatusUnauthorized, "unknown token")
			return
		}
		if admin || auth.HasScope(scopes, auth.ScopeShellAttach) {
			logger.Debug("terminal request authorized", "as", "device", "path", r.URL.Path)
			h(w, r)
			return
		}
		// 403, not 401: the token is real, it just was not granted a shell.
		// Retrying with the same credential will not help, and the message is
		// what a UI should show the user. This is the EXPECTED state for a
		// freshly approved browser — "all" does not imply shell:attach — so
		// the line has to be unmistakable in the log rather than look like an
		// auth failure.
		deny(http.StatusForbidden, "forbidden: shell:attach not granted")
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

// errSessionExists is a create whose name tmux already has.
var errSessionExists = errors.New("session already exists")

var (
	errBadPaneID = errors.New("pane_id must be 1-64 of [A-Za-z0-9_-]")
	errPaneInUse = errors.New("pane_id is already carried by a live session")
)

// sessionArgv vets a create's optional argv (Phase 110). Bounded so a
// request cannot park megabytes in tmux's argv; NUL is refused because it
// would truncate an argument. A bare `claude` resolves to the daemon's
// absolute path — the tmux server's PATH is not the daemon's (history.go).
func (s *Service) sessionArgv(cmd []string) ([]string, error) {
	if len(cmd) == 0 {
		return nil, nil
	}
	if len(cmd) > 32 {
		return nil, errors.New("cmd: at most 32 arguments")
	}
	out := make([]string, len(cmd))
	for i, a := range cmd {
		if len(a) > 4096 || strings.ContainsRune(a, 0) || (i == 0 && strings.TrimSpace(a) == "") {
			return nil, errors.New("cmd: invalid argument")
		}
		out[i] = a
	}
	if out[0] == "claude" {
		out[0] = s.claudeBinary()
	}
	return out, nil
}

// ValidPaneID is the shape a caller-chosen pane id may take (Phase 109). It
// lands in the session's environment and in hook payloads, so it is kept to a
// plain token: no spaces, quotes, separators or control characters.
func ValidPaneID(id string) bool {
	if id == "" || len(id) > 64 {
		return false
	}
	for _, c := range id {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '_' || c == '-') {
			return false
		}
	}
	return true
}

// spawnSession creates a tmux session, hook-enabled when it can be (Phase
// 100), with a policy (101) and a workspace (103). It is the one place a
// session is born — the create route and an agent's split both use it.
// hooks is false when the session was created without hook routing.
//
// cmd (Phase 104) is an optional argv the session runs instead of a shell —
// `claude --resume <id>` for a resumed history row.
//
// paneID (Phase 109) is the caller's pane id for the session's hooks — a
// browser layout leaf — or "" to mint term_<hex>. It must be ValidPaneID and
// not carried by another live session.
func (s *Service) spawnSession(name, cwd, policy, workspaceID, paneID string, cmd ...string) (entry hookEntry, hooks bool, err error) {
	name = strings.TrimSpace(name)
	if name == "" {
		name = "ymux-" + uuid.NewString()[:8]
	}
	if !ValidName(name) {
		return hookEntry{}, false, ErrBadName
	}
	// Checked rather than left to tmux's "duplicate session" error so the
	// caller gets a status it can branch on instead of a 500.
	if s.tmux.Has(name) {
		return hookEntry{}, false, errSessionExists
	}
	if paneID != "" {
		if !ValidPaneID(paneID) {
			return hookEntry{}, false, errBadPaneID
		}
		if s.hooks != nil && s.hooks.paneInUse(paneID) {
			return hookEntry{}, false, errPaneInUse
		}
	}
	// Phase 100: point the session's hooks at the daemon. Without a listener
	// address, or on a tmux too old for `-e`, the session is created exactly
	// as before and its hooks go wherever the global environment says.
	var e *hookEntry
	var env map[string]string
	if s.hooks != nil {
		if addr := s.hooks.hookAddr(); addr != "" && s.tmux.SupportsSessionEnv() {
			var err error
			if e, env, err = s.hooks.mint(name, addr, paneID); err != nil {
				logger.Error("hook token mint failed; creating without hooks", "err", err)
				e, env = nil, nil
			} else {
				e.policy, e.workspaceID = policy, workspaceID
				// Phase 111: what a restarted daemon needs to rebuild this
				// entry lives with the session (RecoverHooks).
				env["YMUX_POLICY"] = policy
				if workspaceID != "" {
					env["YMUX_WORKSPACE_ID"] = workspaceID
				}
			}
		}
	}
	if err := s.tmux.Create(name, cwd, env, cmd...); err != nil {
		return hookEntry{}, false, err
	}
	if e == nil {
		return hookEntry{name: name}, false, nil
	}
	s.hooks.add(e)
	return *e, true, nil
}
