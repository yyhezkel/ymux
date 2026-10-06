package api

// huma.go — the server-wide huma API. Phase 77 S4 moves the client-SDK HTTP
// surface (version/health negotiation + Files + Logs) onto typed huma
// operations so /api/openapi.json is generated from the handlers and can't
// drift. Insights stays on raw stdlib handlers (dynamic metric/docker/process
// maps, consumed by the desktop Monitor's Rust client, not by generated SDKs)
// and is intentionally out of the SDK spec — see DECISIONS.md (S4.1 scope).

import (
	"context"
	"net/http"
	"strings"
	"time"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/adapters/humago"

	"ymux-server/internal/auth"
	"ymux-server/internal/core"
)

// opScopes maps a secured operation to the scope a paired device must hold to
// call it. Operations absent here need only a valid token. The shared/admin
// token bypasses scope checks; devices default to "all" (backwards compat), so
// this only bites once an owner narrows a device's grants.
var opScopes = map[string]auth.Scope{
	"workspace-list":           auth.ScopeWorkspaceRead,
	"workspace-create-session": auth.ScopeSessionWrite,
	"workspace-get-session":    auth.ScopeSessionRead,
	"session-resolve-hook":     auth.ScopeHookApprove,
	"term-list":                auth.ScopeShellAttach,
	"term-create":              auth.ScopeShellAttach,
	"term-rename":              auth.ScopeShellAttach,
	"term-kill":                auth.ScopeShellAttach,
}

// newHumaAPI builds the shared huma API on the given mux: security scheme,
// bearer middleware, and the version/health/files/logs operations. It returns
// the API so the caller can marshal OpenAPI() after registration.
func (s *Server) newHumaAPI(mux *http.ServeMux) huma.API {
	cfg := huma.DefaultConfig("ymux-server", core.Version)
	cfg.Info.Description = "ymux server daemon API — the client-SDK surface " +
		"(version negotiation, Files, Logs, Terminal). Streaming WebSocket frames are " +
		"described in asyncapi.json (PHASE-77-DESIGN §4.4). Generated from the " +
		"huma handlers (S4); the Insights metrics API is desktop-internal and " +
		"documented separately."
	cfg.OpenAPI.Components.SecuritySchemes = map[string]*huma.SecurityScheme{
		"bearerAuth": {Type: "http", Scheme: "bearer"},
	}
	// We serve the spec ourselves (with CORS + caching) and don't want the
	// $schema link transformer mutating response bodies, so strip the auto
	// endpoints + create hooks.
	cfg.CreateHooks = nil
	cfg.OpenAPIPath = ""
	cfg.DocsPath = ""
	cfg.SchemasPath = ""

	api := humago.New(mux, cfg)
	api.UseMiddleware(s.bearerMiddleware(api))

	s.registerMetaOps(api)
	if s.deps.Files != nil {
		s.deps.Files.RegisterHuma(api)
	}
	if s.deps.Logs != nil {
		s.deps.Logs.RegisterHuma(api)
	}
	if s.deps.Term != nil {
		s.deps.Term.RegisterHuma(api)
	}
	s.registerMobileOps(api) // pairing/redeem + workspace list/get-session/create-session
	return api
}

// OpenAPISpec returns the generated OpenAPI document as JSON. It builds the
// huma API on a throwaway mux (registration only reflects types; the handlers
// are never invoked), so it works even with nil subsystem providers — the
// `ymux-server openapi` subcommand and the SDK pipeline use it to emit the
// spec without a running server.
func (s *Server) OpenAPISpec() ([]byte, error) {
	return s.newHumaAPI(http.NewServeMux()).OpenAPI().MarshalJSON()
}

// tokenOK reports whether a bearer token is accepted: the shared (desktop)
// token, or — when chat is enabled — a registered device's long-term token, so
// a paired phone can call the /api/v2/* surface with the token it got from
// /api/pairing/redeem.
func (s *Server) tokenOK(token string) bool {
	if token != "" && token == s.token {
		return true
	}
	return s.deps.Chat != nil && s.deps.Chat.TokenValid(token)
}

// bearerMiddleware enforces auth for every operation that declares a security
// requirement; operations with no Security (version, health, pairing/redeem)
// are public. 401 on a missing/unknown token.
func (s *Server) bearerMiddleware(api huma.API) func(huma.Context, func(huma.Context)) {
	return func(ctx huma.Context, next func(huma.Context)) {
		if len(ctx.Operation().Security) == 0 {
			next(ctx)
			return
		}
		got := strings.TrimPrefix(ctx.Header("Authorization"), "Bearer ")
		if !s.tokenOK(got) {
			_ = huma.WriteErr(api, ctx, http.StatusUnauthorized, "unauthorized")
			return
		}
		// Per-device scope enforcement (Q6): if this op needs a scope, the
		// device must grant it. The shared/admin token bypasses; devices
		// default to "all" until an owner narrows them.
		if need, needed := opScopes[ctx.Operation().OperationID]; needed && s.deps.Chat != nil {
			scopes, admin, _ := s.deps.Chat.DeviceScopes(got)
			if !admin && !auth.HasScope(scopes, need) {
				_ = huma.WriteErr(api, ctx, http.StatusForbidden, "missing scope: "+string(need))
				return
			}
		}
		next(ctx)
	}
}

// VersionBody is the capability-negotiation payload (§4, §4.4).
type VersionBody struct {
	Name         string `json:"name"`
	Version      string `json:"version"`
	APIVersions  []int  `json:"api_versions"`
	FrameVersion int    `json:"frame_version"`
}

// HealthBody is the liveness payload.
type HealthBody struct {
	OK            bool   `json:"ok"`
	Version       string `json:"version"`
	UptimeSeconds int64  `json:"uptime_seconds"`
}

// registerMetaOps mounts the unauthenticated liveness + version endpoints.
func (s *Server) registerMetaOps(api huma.API) {
	huma.Register(api, huma.Operation{
		OperationID: "health", Method: http.MethodGet, Path: "/healthz",
		Summary: "Liveness probe (unauthenticated)", Tags: []string{"meta"},
	}, func(_ context.Context, _ *struct{}) (*struct{ Body HealthBody }, error) {
		return &struct{ Body HealthBody }{Body: HealthBody{
			OK: true, Version: core.Version, UptimeSeconds: int64(time.Since(s.started).Seconds()),
		}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "version", Method: http.MethodGet, Path: "/api/version",
		Summary: "Version + capability negotiation (unauthenticated)", Tags: []string{"meta"},
	}, func(_ context.Context, _ *struct{}) (*struct{ Body VersionBody }, error) {
		return &struct{ Body VersionBody }{Body: VersionBody{
			Name:         "ymux-server",
			Version:      core.Version,
			APIVersions:  []int{2},
			FrameVersion: core.FrameVersion,
		}}, nil
	})
}
