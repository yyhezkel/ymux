package term

// hookdispatch.go — what the daemon does with a hook from a browser-created
// tmux session (Phase 100, WEB-DESIGN B2).
//
// It is the daemon's counterpart of the per-subkind arms in the desktop's
// feed.push (app/src-tauri/src/rpc_server.rs), reduced to what B2 needs:
// fold the hook into the pane's traffic light and brief (internal/agent, the
// Phase-99 port) and answer. There is no feed, no card and no gate yet — B3
// adds those. Until then every permission request is ALLOWED: the browser
// session's policy is "none" (Yossi, 2026-10-04), because a gate with no way
// to reach a human would block every tool call.
//
// Rule #1: a hook payload carries the user's prompt, Claude's reply and tool
// input. Logs here carry the pane id, the subkind and nothing else.

import (
	"encoding/json"
	"strings"
	"time"

	"ymux-server/internal/agent"
	"ymux-server/internal/core"
)

// termHookTarget is one matched session as a core.HookTarget.
type termHookTarget struct {
	r *HookRegistry
	e *hookEntry
}

// feedPushParams is the subset of the CLI's feed.push params B2 reads
// (cli/src/main.rs claude-hook).
type feedPushParams struct {
	RequestID   string          `json:"request_id"`
	Kind        string          `json:"kind"`
	Subkind     string          `json:"subkind"`
	PaneID      string          `json:"pane_id"`
	Payload     json.RawMessage `json:"payload"`
	TmuxSession string          `json:"tmux_session"`
	ClaudeTitle string          `json:"claude_title"`
}

// hookPayload is the subset of Claude Code's hook payload the state needs.
type hookPayload struct {
	NotificationType     string  `json:"notification_type"`
	Prompt               string  `json:"prompt"`
	LastAssistantMessage *string `json:"last_assistant_message"`
}

var errUnknownMethod = &core.RPCError{Code: -32000, Message: "unknown method"}

// DispatchHook implements core.HookTarget.
func (t termHookTarget) DispatchHook(method string, raw json.RawMessage) (any, *core.RPCError) {
	switch method {
	case "ping":
		return map[string]any{"ok": true}, nil
	case "feed.push":
		return t.feedPush(raw), nil
	}
	logger.Warn("hook method not handled", "method", method)
	return nil, errUnknownMethod
}

func (t termHookTarget) feedPush(raw json.RawMessage) map[string]any {
	var p feedPushParams
	if json.Unmarshal(raw, &p) != nil {
		return map[string]any{"decision": "deny"}
	}
	deny := map[string]any{"request_id": p.RequestID, "decision": "deny"}

	t.r.mu.Lock()
	defer t.r.mu.Unlock()
	e := t.e
	// Defense in depth: the HMAC already identified the session, so a hook
	// naming another pane or another tmux session is something forging
	// across sessions — refuse rather than fold it into the wrong light.
	if p.PaneID != e.paneID {
		logger.Warn("hook pane_id mismatch — denying", "pane", e.paneID, "claimed", p.PaneID)
		return deny
	}
	if p.TmuxSession != "" && p.TmuxSession != e.name {
		logger.Warn("hook tmux_session mismatch — denying", "pane", e.paneID)
		return deny
	}

	var pl hookPayload
	if len(p.Payload) > 0 {
		_ = json.Unmarshal(p.Payload, &pl)
	}
	t.r.applyLocked(e, p.Subkind, pl, p.ClaudeTitle)
	logger.Debug("hook folded", "pane", e.paneID, "subkind", p.Subkind,
		"state", string(e.run.CurrentState()), "seq", e.run.Seq)

	if p.Kind == "permission_request" {
		return map[string]any{"request_id": p.RequestID, "decision": "allow", "policy": "none"}
	}
	return map[string]any{"request_id": p.RequestID, "decision": "passive"}
}

// applyLocked folds one hook into the entry, mirroring the desktop's
// feed.push arms subkind by subkind. Caller holds r.mu.
func (r *HookRegistry) applyLocked(e *hookEntry, subkind string, pl hookPayload, claudeTitle string) {
	now := r.now()
	nowMs := now.UnixMilli()
	switch subkind {
	case "pre-tool-use", "notification":
		e.run.ApplyHook(subkind, pl.NotificationType, now)
	case "user-prompt-submit":
		e.run.TurnStartedAt = now
		e.run.ApplyHook(subkind, "", now)
		if prompt := strings.TrimSpace(pl.Prompt); prompt != "" {
			clipped := agent.ClipChars(prompt, agent.PromptMaxChars)
			e.brief.LastPrompt = &clipped
			e.brief.PromptMs = &nowMs
			e.brief.SessionEnded = false
			e.brief.Seq++
		}
	case "stop":
		// Rust: turn_started_at.take() — fold the turn, then clear it.
		if start := e.run.TurnStartedAt; !start.IsZero() {
			if d := now.Sub(start); d >= 0 {
				e.run.RecordTurn(uint64(d.Milliseconds()))
			}
			e.run.TurnStartedAt = time.Time{}
		}
		e.run.ApplyHook(subkind, "", now)
		// An absent and an empty claude_title both mean "no title" here; the
		// desktop would keep an empty one as task:"" — not worth a pointer.
		var title *string
		if claudeTitle != "" {
			title = &claudeTitle
		}
		b := agent.BriefFromStop(pl.LastAssistantMessage, title, nowMs)
		e.brief.Brief = &b
		e.brief.SessionEnded = false
		e.brief.Seq++
	case "session-end":
		// The desktop drops the pane's run and emits Unknown at seq+1; keep
		// that seq so a later frontend still sees the reset as newer.
		e.run = agent.Run{Seq: e.run.Seq + 1}
		e.brief.SessionEnded = true
		e.brief.Seq++
	}
}
