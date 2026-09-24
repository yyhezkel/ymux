// Package api is the HTTP front door: it owns the mux, unauthenticated liveness
// + version-negotiation endpoints, and mounts each subsystem's routes behind
// the auth middleware. Subsystems never import api; api imports them (no cycle).
package api

import (
	"context"
	_ "embed"
	"fmt"
	"net/http"
	"sync"
	"time"

	"ymux-server/internal/auth"
	"ymux-server/internal/chat"
	"ymux-server/internal/files"
	"ymux-server/internal/insights"
	"ymux-server/internal/logging"
	"ymux-server/internal/logs"
	"ymux-server/internal/push"
	"ymux-server/internal/term"
	"ymux-server/internal/workspace"
)

// logger is the API front door's component logger (Phase 79.D unified format).
var logger = logging.New("SRV:API")

// Deps is the set of subsystems the front door mounts. Any field may be nil to
// disable that subsystem.
type Deps struct {
	Insights  *insights.Service
	Chat      *chat.ChatAPI      // nil if chat disabled
	Files     *files.Service     // nil if files disabled
	Logs      *logs.Service      // nil if logs disabled
	Workspace *workspace.Service // nil if workspace disabled
	Push      *push.Server       // nil if native push disabled
	Term      *term.Service      // nil if the terminal API is disabled
}

// Server wires the subsystems into one HTTP listener.
type Server struct {
	token   string
	port    int
	deps    Deps
	started time.Time
	mu      sync.Mutex
	httpSrv *http.Server // set in Run; used by Shutdown for a graceful drain
}

// NewServer builds the front door.
func NewServer(token string, port int, deps Deps) *Server {
	return &Server{token: token, port: port, deps: deps, started: time.Now()}
}

// Shutdown gracefully drains the HTTP listener (in-flight requests finish or the
// context deadline hits). Safe to call before Run has bound (no-op).
func (s *Server) Shutdown(ctx context.Context) error {
	s.mu.Lock()
	srv := s.httpSrv
	s.mu.Unlock()
	if srv == nil {
		return nil
	}
	return srv.Shutdown(ctx)
}

// asyncapi.json is the hand-authored streaming (WebSocket) contract — OpenAPI
// doesn't describe WS frames, so the frame schema lives here (PHASE-77-DESIGN
// §4.4). The REST openapi.json is now generated from the huma handlers (S4).
//
//go:embed asyncapi.json
var asyncapiSpec []byte

// frames.schema.json is the canonical machine schema for the WS frames
// (JSON-Schema 2020-12) — the source the SDK generators turn into typed frame
// unions. Served alongside the specs so clients + CI can fetch it live.
//
//go:embed frames.schema.json
var framesSchema []byte

func serveSpec(spec []byte) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Cache-Control", "public, max-age=300")
		_, _ = w.Write(spec)
	}
}

// Handler builds the fully-wired mux (exported so tests can exercise routes via
// httptest without binding a port).
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	authMW := auth.Bearer(s.token)

	// The huma-hosted surface: liveness + version negotiation (public) and the
	// Files + Logs operations (bearer-gated by huma middleware). huma reflects
	// these into the OpenAPI we serve below, so the spec tracks the handlers.
	hapi := s.newHumaAPI(mux)
	spec, err := hapi.OpenAPI().MarshalJSON()
	if err != nil {
		logger.Error("OpenAPI marshal failed", "err", err) // serve an empty doc rather than crash
		spec = []byte("{}")
	}
	mux.HandleFunc("/api/openapi.json", serveSpec(spec))
	mux.HandleFunc("/api/asyncapi.json", serveSpec(asyncapiSpec))
	mux.HandleFunc("/api/frames.schema.json", serveSpec(framesSchema))

	// Insights keeps its raw legacy + /api/v2 stdlib handlers behind auth
	// (desktop Monitor surface — not part of the generated SDK spec).
	if s.deps.Insights != nil {
		s.deps.Insights.RegisterRoutes(mux, authMW)
	}
	// Files + Logs are already registered on the huma API above.
	if s.deps.Workspace != nil {
		s.deps.Workspace.RegisterRoutes(mux, authMW)
	}
	if s.deps.Chat != nil {
		// Chat brings its own auth (device tokens + shared bearer) and registers
		// its legacy /api/claude/* + /ws/claude/* routes. v2 chat aliases land in
		// a later sprint; legacy paths keep existing clients working (S1 compat).
		s.deps.Chat.RegisterRoutes(mux)
	}
	// Native push WS (§4): self-authenticating (device long-term token via
	// Bearer/?token=), so it's mounted raw rather than behind authMW.
	if s.deps.Push != nil {
		mux.HandleFunc("/api/v2/push/subscribe", s.deps.Push.Handler)
	}
	// Terminal API (Phase 95). Mounted raw for the same reason push is: authMW
	// only knows the shared token, and these routes must also accept a paired
	// device's token — but ONLY one carrying the opt-in auth.ScopeShellAttach
	// grant. term.Service.gate does both checks and fails closed.
	if s.deps.Term != nil {
		s.deps.Term.RegisterRoutes(mux)
	}
	return mux
}

// Run serves until the listener errors or Shutdown is called (which returns a
// nil error here, mapping http.ErrServerClosed to a clean stop).
func (s *Server) Run() error {
	srv := &http.Server{
		Addr:         fmt.Sprintf("127.0.0.1:%d", s.port),
		Handler:      s.Handler(),
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 20 * time.Second,
	}
	s.mu.Lock()
	s.httpSrv = srv
	s.mu.Unlock()
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		return err
	}
	return nil
}
