package api

// mobile.go — Phase 77 S6. The mobile-consumed endpoints that live in the chat
// (pairing) + workspace subsystems are surfaced here as typed huma ops so they
// land in the generated OpenAPI + the Kotlin/TS SDKs. They compose deps.Chat +
// deps.Workspace (raw duplicates of these paths were removed from those packages
// to avoid mux collisions). Auth: redeem is public (the one-shot token is the
// credential); the workspace ops require a bearer that tokenOK accepts (shared
// token or a paired device's long-term token).

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strings"

	"github.com/danielgtaylor/huma/v2"

	"ymux-server/internal/auth"
	"ymux-server/internal/workspace"
)

// HookForwardRequest is a desktop-origin hook forwarded to paired devices (B).
type HookForwardRequest struct {
	ReqID       string `json:"req_id"`
	WorkspaceID string `json:"workspace_id"`
	PaneID      string `json:"pane_id"`
	ToolName    string `json:"tool_name"`
	Title       string `json:"title"`
	TimeoutAt   int64  `json:"timeout_at"`
}

// HookForwardResponse returns the virtual session id the hook was published on.
type HookForwardResponse struct {
	OK        bool   `json:"ok"`
	SessionID string `json:"session_id"`
}

// HookStatus is a forwarded hook's resolution, polled by the desktop.
type HookStatus struct {
	Resolved bool   `json:"resolved"`
	Decision string `json:"decision"`
}

// virtualDesktopSession is the synthetic session id a desktop-forwarded hook is
// published on — stable per (workspace, pane) so the phone sees a consistent
// session. Non-identifier bytes are mapped to '_'.
func virtualDesktopSession(ws, pane string) string {
	safe := func(s string) string {
		return strings.Map(func(r rune) rune {
			switch {
			case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9', r == '_', r == '-':
				return r
			default:
				return '_'
			}
		}, s)
	}
	return "desktop-" + safe(ws) + "-" + safe(pane)
}

// ScopesBody carries a device's grant list (GET response + PUT request).
type ScopesBody struct {
	Scopes []string `json:"scopes"`
}

func scopeStrings(stored string) []string {
	sc := auth.ParseScopes(stored)
	out := make([]string, len(sc))
	for i, s := range sc {
		out[i] = string(s)
	}
	return out
}

var mobileSecured = []map[string][]string{{"bearerAuth": {}}}

// OKResponse is a minimal {"ok": true} body.
type OKResponse struct {
	OK bool `json:"ok"`
}

// HookDecisionRequest resolves a pending hook.
type HookDecisionRequest struct {
	Decision string `json:"decision"` // "allow" | "deny"
}

// HookResolved is the result of a hook resolution (won = this caller's decision
// was the winner-takes-all one that got broadcast).
type HookResolved struct {
	ReqID    string `json:"req_id"`
	Decision string `json:"decision"`
	Won      bool   `json:"won"`
}

// PairingRedeemResponse is the device credential handed to a freshly paired
// phone, plus the workspace it should connect to (saves a list round-trip).
type PairingRedeemResponse struct {
	DeviceID           string `json:"device_id"`
	LongTermToken      string `json:"long_term_token"`
	DefaultWorkspaceID string `json:"default_workspace_id"`
}

// Workspace is a workspace list item.
type Workspace struct {
	ID                 string `json:"id"`
	Name               string `json:"name"`
	CreatedAt          int64  `json:"created_at"`
	ActiveSessionCount int    `json:"active_session_count"`
}

// Session is a session's detail (get-session).
type Session struct {
	ID              string                     `json:"id"`
	Kind            string                     `json:"kind"`
	WorkspaceID     string                     `json:"workspace_id"`
	Subscribers     int                        `json:"subscribers"`
	PendingRequests []workspace.PendingRequest `json:"pending_requests"`
	EventCount      int64                      `json:"event_count"`
}

// CreateSessionRequest is the body for creating a session.
type CreateSessionRequest struct {
	Kind string `json:"kind"`
}

// SessionCreated is the response to a session create.
type SessionCreated struct {
	SessionID string `json:"session_id"`
	Kind      string `json:"kind"`
}

func (s *Server) registerMobileOps(api huma.API) {
	// POST /api/pairing/redeem — public; the one-shot token IS the credential.
	if s.deps.Chat != nil {
		huma.Register(api, huma.Operation{
			OperationID: "pairing-redeem", Method: http.MethodPost, Path: "/api/pairing/redeem",
			Summary: "Redeem a one-shot pairing token for a device credential",
			Tags:    []string{"pairing"},
		}, func(_ context.Context, in *struct {
			RealIP string `header:"X-Real-IP"`
			Body   struct {
				OneShotToken string `json:"one_shot_token"`
			}
		}) (*struct{ Body PairingRedeemResponse }, error) {
			id, longTerm, ok := s.deps.Chat.Redeem(in.Body.OneShotToken, in.RealIP)
			if !ok {
				return nil, huma.Error401Unauthorized("invalid or expired pairing token")
			}
			return &struct{ Body PairingRedeemResponse }{Body: PairingRedeemResponse{
				DeviceID: id, LongTermToken: longTerm, DefaultWorkspaceID: workspace.DefaultID,
			}}, nil
		})

		// GET /api/v2/devices/{id}/scopes — read a device's grants. A device may
		// read its OWN scopes (id = its device id, or the alias "me"); the
		// shared/admin token may read any device's.
		huma.Register(api, huma.Operation{
			OperationID: "device-get-scopes", Method: http.MethodGet,
			Path:    "/api/v2/devices/{id}/scopes",
			Summary: "Read a device's scope grants",
			Tags:    []string{"pairing"}, Security: mobileSecured,
		}, func(_ context.Context, in *struct {
			ID            string `path:"id"`
			Authorization string `header:"Authorization"`
		}) (*struct{ Body ScopesBody }, error) {
			caller, admin, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer "))
			if !ok {
				return nil, huma.Error401Unauthorized("unauthorized")
			}
			id := in.ID
			if id == "me" {
				id = caller
			}
			if !admin && id != caller {
				return nil, huma.Error403Forbidden("cannot read another device's scopes")
			}
			stored, found := s.deps.Chat.GetDeviceScopes(id)
			if !found {
				return nil, huma.Error404NotFound("device not found")
			}
			return &struct{ Body ScopesBody }{Body: ScopesBody{Scopes: scopeStrings(stored)}}, nil
		})

		// PUT /api/v2/devices/{id}/scopes — owner-only: set a device's grants.
		huma.Register(api, huma.Operation{
			OperationID: "device-set-scopes", Method: http.MethodPut,
			Path:    "/api/v2/devices/{id}/scopes",
			Summary: "Set a device's scope grants (owner only)",
			Tags:    []string{"pairing"}, Security: mobileSecured,
		}, func(_ context.Context, in *struct {
			ID            string `path:"id"`
			Authorization string `header:"Authorization"`
			Body          ScopesBody
		}) (*struct{ Body ScopesBody }, error) {
			_, admin, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer "))
			if !ok {
				return nil, huma.Error401Unauthorized("unauthorized")
			}
			if !admin {
				return nil, huma.Error403Forbidden("owner token required to set scopes")
			}
			stored := auth.NormalizeScopes(in.Body.Scopes)
			if !s.deps.Chat.SetDeviceScopes(in.ID, stored) {
				return nil, huma.Error404NotFound("device not found or not active")
			}
			return &struct{ Body ScopesBody }{Body: ScopesBody{Scopes: scopeStrings(stored)}}, nil
		})
	}

	if s.deps.Workspace == nil {
		return
	}
	mgr := s.deps.Workspace.Mgr()

	huma.Register(api, huma.Operation{
		OperationID: "workspace-list", Method: http.MethodGet, Path: "/api/v2/workspace/list",
		Summary: "List workspaces", Tags: []string{"workspace"}, Security: mobileSecured,
	}, func(context.Context, *struct{}) (*struct{ Body []Workspace }, error) {
		wss, err := mgr.ListWorkspaces()
		if err != nil {
			return nil, huma.Error500InternalServerError(err.Error())
		}
		out := make([]Workspace, 0, len(wss))
		for _, ws := range wss {
			sess, _ := mgr.ListSessions(ws.ID)
			out = append(out, Workspace{ID: ws.ID, Name: ws.Name, CreatedAt: ws.CreatedAt, ActiveSessionCount: len(sess)})
		}
		return &struct{ Body []Workspace }{Body: out}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "workspace-create-session", Method: http.MethodPost, Path: "/api/v2/workspace/{id}/sessions",
		Summary: "Create a session in a workspace", Tags: []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		ID   string `path:"id"`
		Body CreateSessionRequest
	}) (*struct{ Body SessionCreated }, error) {
		se, err := mgr.CreateSession(in.ID, in.Body.Kind)
		if err != nil {
			return nil, wsErr(err)
		}
		return &struct{ Body SessionCreated }{Body: SessionCreated{SessionID: se.ID, Kind: se.Kind}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "workspace-get-session", Method: http.MethodGet, Path: "/api/v2/workspace/{id}/session/{sid}",
		Summary: "Get a session's detail", Tags: []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		ID  string `path:"id"`
		SID string `path:"sid"`
	}) (*struct{ Body Session }, error) {
		se, err := mgr.GetSession(in.SID)
		if err != nil {
			return nil, wsErr(err)
		}
		pending, _ := mgr.ListPending(se.ID)
		if pending == nil {
			pending = []workspace.PendingRequest{}
		}
		return &struct{ Body Session }{Body: Session{
			ID: se.ID, Kind: se.Kind, WorkspaceID: se.WorkspaceID,
			Subscribers: mgr.SubscriberCount(se.ID), PendingRequests: pending, EventCount: mgr.EventCount(se.ID),
		}}, nil
	})

	// PUT /api/v2/session/{sid}/hook/{req_id} — approve/deny a pending hook
	// over REST (the mobile alternative to the workspace-WS hook_decision
	// frame). Winner-takes-all: `won` reports whether this decision was the
	// first and got broadcast. resolved_by = the caller's device id.
	huma.Register(api, huma.Operation{
		OperationID: "session-resolve-hook", Method: http.MethodPut,
		Path:    "/api/v2/session/{sid}/hook/{req_id}",
		Summary: "Approve or deny a pending hook request",
		Tags:    []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		SID           string `path:"sid"`
		ReqID         string `path:"req_id"`
		Authorization string `header:"Authorization"`
		Body          HookDecisionRequest
	}) (*struct{ Body HookResolved }, error) {
		decision := "deny"
		if in.Body.Decision == "allow" {
			decision = "allow"
		}
		clientID := "desktop" // shared/admin token
		if s.deps.Chat != nil {
			if id, _, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer ")); ok && id != "" {
				clientID = id
			}
		}
		won, err := mgr.ResolveHook(in.ReqID, clientID, decision)
		if err != nil {
			return nil, wsErr(err)
		}
		return &struct{ Body HookResolved }{Body: HookResolved{ReqID: in.ReqID, Decision: decision, Won: won}}, nil
	})

	// POST /api/v2/hooks/forward — B path: the desktop forwards a hook that
	// fired in a desktop-terminal Claude session so paired phones get a push.
	// Owner/shared-token only (only the desktop forwards). The desktop keeps
	// the authoritative local hook and polls hooks-status for the decision.
	huma.Register(api, huma.Operation{
		OperationID: "hooks-forward", Method: http.MethodPost, Path: "/api/v2/hooks/forward",
		Summary: "Forward a desktop-origin hook to paired devices",
		Tags:    []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		Authorization string `header:"Authorization"`
		Body          HookForwardRequest
	}) (*struct{ Body HookForwardResponse }, error) {
		if s.deps.Chat != nil {
			if _, admin, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer ")); !ok || !admin {
				return nil, huma.Error403Forbidden("owner token required to forward hooks")
			}
		}
		if in.Body.ReqID == "" {
			return nil, huma.Error400BadRequest("req_id required")
		}
		// A browser-created tmux session (pane id term_…, Phase 100) is the
		// DAEMON's own: its hook already arrived over the hook RPC, where
		// term folds it and, under a gate, waits for a feed.decide. The CLI
		// forwards every pre-tool-use regardless of where its RPC went, so
		// forwarding this one too would push a phone a second card whose
		// answer nobody polls for. Acknowledge and drop (Phase 101).
		if strings.HasPrefix(in.Body.PaneID, "term_") {
			return &struct{ Body HookForwardResponse }{Body: HookForwardResponse{OK: true}}, nil
		}
		vsess := virtualDesktopSession(in.Body.WorkspaceID, in.Body.PaneID)
		payload, _ := json.Marshal(map[string]any{
			"req_id":            in.Body.ReqID,
			"tool_name":         in.Body.ToolName,
			"title":             in.Body.Title,
			"decision_required": true,
			"origin":            "desktop",
		})
		if err := mgr.ForwardHook(vsess, in.Body.ReqID, payload, in.Body.TimeoutAt); err != nil {
			return nil, huma.Error500InternalServerError(err.Error())
		}
		return &struct{ Body HookForwardResponse }{Body: HookForwardResponse{OK: true, SessionID: vsess}}, nil
	})

	// GET /api/v2/hooks/{req_id} — poll a forwarded hook's resolution. The
	// desktop polls this to learn a phone's allow/deny for a B-path hook and
	// then resolves its authoritative local hook.
	huma.Register(api, huma.Operation{
		OperationID: "hooks-status", Method: http.MethodGet, Path: "/api/v2/hooks/{req_id}",
		Summary: "Poll a forwarded hook's resolution",
		Tags:    []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		ReqID string `path:"req_id"`
	}) (*struct{ Body HookStatus }, error) {
		decision, resolved := mgr.HookResolution(in.ReqID)
		return &struct{ Body HookStatus }{Body: HookStatus{Resolved: resolved, Decision: decision}}, nil
	})
}

// wsErr maps a workspace error to the matching HTTP status.
func wsErr(err error) error {
	if errors.Is(err, workspace.ErrNotFound) {
		return huma.Error404NotFound(err.Error())
	}
	return huma.Error500InternalServerError(err.Error())
}
