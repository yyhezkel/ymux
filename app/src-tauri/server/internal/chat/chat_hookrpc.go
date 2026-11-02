package chat

// Phase 69.C — hook bridge. The daemon implements the SERVER half of the
// Phase 66 RPC dialect so that Claude's hooks, fired inside a mobile-spawned
// session, reach the phone for approval with ZERO changes to the ymux CLI
// or hooks/claude-code.json.
//
// Flow: the daemon injects YMUX_SOCKET_ADDR=<the hook listener>,
// YMUX_TUNNEL_TOKEN=<per-session token>, YMUX_PANE_ID=mob_<id> into the
// claude child (see spawnEnv). When Claude fires a PreToolUse hook, the CLI
// dials the listener, does the HMAC challenge-response, and pushes a
// permission_request. We map it to the session (by which session's token
// validates the HMAC), forward a hook_request over the WS, wait for the
// phone's allow/deny, and reply with the decision the CLI expects.
//
// Phase 100: the handshake itself now lives in internal/hooks (term mints
// hook tokens too, for browser-created tmux sessions). This file is chat's
// core.HookResolver: MatchHookHMAC identifies the session, dispatchHook
// answers the request — both unchanged in behaviour.

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/json"
	"time"

	"ymux-server/internal/core"
)

// SetHookAddr records the hook-RPC listener's bound address so spawned claude
// children get YMUX_SOCKET_ADDR pointed at it. Called by hooks.Start
// (SessionManager satisfies core.AddrSink).
func (m *SessionManager) SetHookAddr(addr string) {
	m.mu.Lock()
	m.rpcAddr = addr
	m.mu.Unlock()
}

// SetWorkspaceResolver (beta.3 Fix 3) injects a workspace-id → display-name
// resolver from the workspace manager. Called from `cmd` after both managers
// exist. Left nil in tests / when the workspace subsystem isn't wired — in
// that case chat sessions emit workspace_name="" and downstream (mobile UI)
// falls back to session id.
func (m *SessionManager) SetWorkspaceResolver(r func(id string) (string, bool)) {
	m.mu.Lock()
	m.wsResolver = r
	m.mu.Unlock()
}

// resolveWorkspace looks up the (id, name) pair for a chat session. Chat
// sessions aren't linked to a workspace explicitly today, so the id defaults
// to the shared "ws_default" sentinel (workspace.DefaultID) which the mobile
// app also uses. Name resolution is best-effort — a nil resolver or a missing
// workspace yields an empty name and the mobile app renders the id.
func (m *SessionManager) resolveWorkspace(_ *Session) (string, string) {
	const defaultWorkspaceID = "ws_default"
	m.mu.Lock()
	r := m.wsResolver
	m.mu.Unlock()
	if r == nil {
		return defaultWorkspaceID, ""
	}
	if name, ok := r(defaultWorkspaceID); ok {
		return defaultWorkspaceID, name
	}
	return defaultWorkspaceID, ""
}

// chatHookTarget is a matched chat session as a core.HookTarget.
type chatHookTarget struct {
	m *SessionManager
	s *Session
}

// DispatchHook answers the request. Chat never returns a JSON-RPC error
// object: an unknown method is a `deny` result, as it always was.
func (t chatHookTarget) DispatchHook(method string, params json.RawMessage) (any, *core.RPCError) {
	return t.m.dispatchHook(t.s, method, params), nil
}

// MatchHookHMAC implements core.HookResolver: the session whose per-session
// token produced mac over nonce.
func (m *SessionManager) MatchHookHMAC(nonce, mac []byte) (core.HookTarget, bool) {
	s := m.matchSessionByHMAC(nonce, mac)
	if s == nil {
		return nil, false
	}
	return chatHookTarget{m: m, s: s}, true
}

// matchSessionByHMAC finds the session whose per-session token produces the
// given HMAC over the nonce. O(active sessions); constant-time compare.
func (m *SessionManager) matchSessionByHMAC(nonce, mac []byte) *Session {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, s := range m.sessions {
		h := hmac.New(sha256.New, []byte(s.rpcToken))
		h.Write(nonce)
		if hmac.Equal(h.Sum(nil), mac) {
			return s
		}
	}
	return nil
}

type feedPushParams struct {
	RequestID          string          `json:"request_id"`
	Kind               string          `json:"kind"`
	Subkind            string          `json:"subkind"`
	PaneID             string          `json:"pane_id"`
	Title              string          `json:"title"`
	Summary            string          `json:"summary"`
	Payload            json.RawMessage `json:"payload"`
	WaitTimeoutSeconds int             `json:"wait_timeout_seconds"`
}

// dispatchHook handles a feed.push and returns the JSON-RPC result object.
func (m *SessionManager) dispatchHook(sess *Session, method string, rawParams json.RawMessage) map[string]any {
	if method != "feed.push" {
		return map[string]any{"decision": "deny", "error": "unknown method"}
	}
	var p feedPushParams
	if json.Unmarshal(rawParams, &p) != nil {
		return map[string]any{"decision": "deny"}
	}
	// Defense in depth: the pane_id must match the HMAC-identified session.
	if p.PaneID != "" && p.PaneID != "mob_"+sess.id {
		logger.Warn("hook pane_id mismatch — denying", "pane_id", p.PaneID, "session", sess.id)
		return map[string]any{"request_id": p.RequestID, "decision": "deny"}
	}

	// Passive lifecycle hooks: surface as a notification, ack immediately.
	if p.Kind != "permission_request" {
		sess.emit(jsonEvent(map[string]any{
			"type": "notification", "subkind": p.Subkind,
			"title": p.Title, "summary": p.Summary,
		}))
		return map[string]any{"request_id": p.RequestID, "decision": "passive"}
	}

	// Blocking permission request — apply the session policy.
	switch sess.policy {
	case "auto":
		return map[string]any{"request_id": p.RequestID, "decision": "allow"}
	case "block":
		return map[string]any{"request_id": p.RequestID, "decision": "deny"}
	}

	// gate (default): ask the phone.
	decision := sess.awaitHookDecision(&p, m.cfg.hookDenyOnTimeout)
	return map[string]any{"request_id": p.RequestID, "decision": decision}
}

// awaitHookDecision forwards a hook_request to connected clients and blocks
// until one answers, the timeout fires, or the session dies. Returns
// "allow" / "deny". Deny is the safe default on mobile (docs §9-Q5).
func (s *Session) awaitHookDecision(p *feedPushParams, denyOnTimeout bool) string {
	// No phone attached → nobody can approve; deny fast instead of stalling.
	if !s.hasSubscribers() {
		return "deny"
	}

	ch := make(chan string, 1)
	s.hookMu.Lock()
	s.pendingHooks[p.RequestID] = ch
	s.hookMu.Unlock()
	defer func() {
		s.hookMu.Lock()
		delete(s.pendingHooks, p.RequestID)
		s.hookMu.Unlock()
	}()

	toolName, toolInput := extractToolFields(p.Payload)
	prev := s.getStatus()
	s.setStatus(stWaitingHook)
	// beta.3 Fix 3: include workspace_id + workspace_name so the mobile app can
	// render "[<workspace>] approve X?" rather than surfacing the raw session
	// id. Chat sessions aren't linked to a workspace yet (Phase 77 §16 lands
	// that wiring); until then the workspace_name is filled in by the manager
	// when it can resolve it, else empty. `workspace_id` defaults to the
	// always-present "ws_default" sentinel so downstream code has a stable key.
	wsID, wsName := s.mgr.resolveWorkspace(s)
	s.emit(jsonEvent(map[string]any{
		"type":              "hook_request",
		"req_id":            p.RequestID,
		"subkind":           p.Subkind,
		"tool_name":         toolName,
		"tool_input":        toolInput,
		"title":             p.Title,
		"workspace_id":      wsID,
		"workspace_name":    wsName,
		"decision_required": true,
	}))

	timeout := time.Duration(clampTimeout(p.WaitTimeoutSeconds)) * time.Second
	var decision, reason string
	select {
	case d := <-ch:
		decision, reason = d, "client"
	case <-time.After(timeout):
		reason = "timeout"
		if denyOnTimeout {
			decision = "deny"
		} else {
			decision = "allow"
		}
	}
	s.emit(jsonEvent(map[string]any{
		"type": "hook_resolved", "req_id": p.RequestID,
		"decision": decision, "reason": reason,
	}))
	if prev == stActive || prev == stWaitingInput {
		s.setStatus(prev)
	}
	return decision
}

func clampTimeout(s int) int {
	if s < 1 {
		return 120
	}
	if s > 600 {
		return 600
	}
	return s
}

// extractToolFields pulls tool_name / tool_input from a PreToolUse payload for
// a richer hook_request card. Best-effort.
func extractToolFields(payload json.RawMessage) (string, json.RawMessage) {
	if len(payload) == 0 {
		return "", json.RawMessage("null")
	}
	var p struct {
		ToolName  string          `json:"tool_name"`
		ToolInput json.RawMessage `json:"tool_input"`
	}
	if json.Unmarshal(payload, &p) != nil {
		return "", json.RawMessage("null")
	}
	if len(p.ToolInput) == 0 {
		p.ToolInput = json.RawMessage("null")
	}
	return p.ToolName, p.ToolInput
}
