package term

// hookdispatch.go — what the daemon does with a hook from a browser-created
// tmux session (Phase 100, WEB-DESIGN B2).
//
// It is the daemon's counterpart of the per-subkind arms in the desktop's
// feed.push (app/src-tauri/src/rpc_server.rs): fold the hook into the pane's
// traffic light and brief (internal/agent, the Phase-99 port), tell the
// events subscribers (Phase 101), make the card the desktop would make, and
// answer. A permission request follows the session's policy (DECISIONS
// 2026-10-05): `none` — the default — answers allow at once and makes no card,
// as the desktop's Auto does; `gate` makes a blocking card and waits for a
// feed.decide, up to wait_timeout_seconds.
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
	// Card text the CLI derived; the lifecycle subkinds are re-humanized.
	Title   string `json:"title"`
	Summary string `json:"summary"`
	// How long a gated request may wait; default 120, clamped 1–600 exactly
	// as the desktop does, so a buggy client cannot pin a goroutine forever.
	WaitTimeoutSeconds *int64 `json:"wait_timeout_seconds"`
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
	if res, err, ok := t.verb(method, raw); ok { // B4 (verbs.go)
		return res, err
	}
	if res, err, ok := t.agentVerb(method, raw); ok { // B5 (agentverbs.go)
		return res, err
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
	r := t.r

	r.mu.Lock()
	e := t.e
	// Defense in depth: the HMAC already identified the session, so a hook
	// naming another pane or another tmux session is something forging
	// across sessions — refuse rather than fold it into the wrong light.
	if p.PaneID != e.paneID {
		r.mu.Unlock()
		logger.Warn("hook pane_id mismatch — denying", "pane", e.paneID, "claimed", p.PaneID)
		return deny
	}
	if p.TmuxSession != "" && p.TmuxSession != e.name {
		r.mu.Unlock()
		logger.Warn("hook tmux_session mismatch — denying", "pane", e.paneID)
		return deny
	}

	var pl hookPayload
	if len(p.Payload) > 0 {
		_ = json.Unmarshal(p.Payload, &pl)
	}
	runSeq, briefSeq := e.run.Seq, e.brief.Seq
	r.applyLocked(e, p.Subkind, pl, p.ClaudeTitle)
	// Copies taken under the lock; everything below runs without it, so a
	// gate that waits two minutes holds nothing another hook needs.
	var runEv *agent.AgentRunEvent
	if e.run.Seq != runSeq {
		ev := e.run.Event(e.paneID)
		runEv = &ev
	}
	var briefEv *agent.BriefEntry
	if e.brief.Seq != briefSeq {
		b := e.brief
		briefEv = &b
	}
	var stopBrief *agent.Brief
	if p.Subkind == "stop" && e.brief.Brief != nil {
		b := *e.brief.Brief
		stopBrief = &b
	}
	pane, session, policy := e.paneID, e.name, e.policy
	logger.Debug("hook folded", "pane", pane, "subkind", p.Subkind,
		"state", string(e.run.CurrentState()), "seq", e.run.Seq)
	r.mu.Unlock()

	if runEv != nil {
		r.hub.publish("pane:agent-run", same(*runEv))
	}
	if briefEv != nil {
		r.hub.publish("pane:brief", same(map[string]any{"pane_id": pane, "entry": *briefEv}))
	}

	passive := map[string]any{"request_id": p.RequestID, "decision": "passive"}
	// The desktop's early returns: a prompt is turn bookkeeping, a
	// notification or stop-failure is a state signal only — none ever makes a card.
	if p.Subkind == "user-prompt-submit" || p.Subkind == "notification" || p.Subkind == "stop-failure" {
		return passive
	}
	blocking := p.Kind == "permission_request"
	if blocking && policy != policyGate {
		return map[string]any{"request_id": p.RequestID, "decision": "allow", "policy": policyNone}
	}

	item, ch := r.addCard(p, pane, session, stopBrief, blocking)
	if ch == nil {
		return passive
	}
	decision := r.await(item.RequestID, ch, waitTimeout(p.WaitTimeoutSeconds))
	return map[string]any{"request_id": item.RequestID, "decision": decision, "policy": policyGate}
}

// addCard builds the desktop's card for a hook, stores it and announces it.
// A blocking card comes back with the channel its decision arrives on.
func (r *HookRegistry) addCard(p feedPushParams, pane, session string, stopBrief *agent.Brief, blocking bool) (FeedItem, chan string) {
	reqID := p.RequestID
	if reqID == "" {
		h, _ := randHex(8)
		reqID = "req_" + h
	}
	title := p.Title
	if title == "" {
		title = "(no title)"
	}
	var payload map[string]any
	if len(p.Payload) > 0 {
		_ = json.Unmarshal(p.Payload, &payload)
	}
	tEn, sEn := cardText(p.Subkind, title, p.Summary, payload, stopBrief, "en")
	tHe, sHe := cardText(p.Subkind, title, p.Summary, payload, stopBrief, "he")
	state := statePassive
	if blocking {
		state = statePending
	}
	kind := p.Kind
	if kind == "" {
		kind = "passive"
	}
	entry := &feedEntry{
		item: FeedItem{
			RequestID: reqID, Kind: kind, Subkind: p.Subkind, PaneID: pane, Session: session,
			Title: tEn, Summary: sEn, Payload: p.Payload, State: state,
			CreatedMs: r.now().UnixMilli(), Blocking: blocking,
		},
		titleHe: tHe, summaryHe: sHe,
	}
	ch := r.feed.add(entry)
	logger.Info("feed item added", "request", reqID, "pane", pane, "subkind", p.Subkind, "blocking", blocking)
	r.hub.publish("feed:item-added", func(lang string) any { return r.feed.viewOf(entry, lang) })
	return entry.item, ch
}

// await blocks until a gated request is decided or its timeout fires. A
// timeout resolves the card as "timeout" (the CLI treats it as deny), unless
// a decision won the race, in which case that decision stands.
func (r *HookRegistry) await(reqID string, ch chan string, timeout time.Duration) string {
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case d := <-ch:
		return d
	case <-timer.C:
		if r.decide(reqID, "timeout", "timer") {
			return "timeout"
		}
		return <-ch // decided just before the timer; its value is buffered
	}
}

// waitTimeout is the desktop's clamp: default 120 s, 1–600.
func waitTimeout(secs *int64) time.Duration {
	n := int64(120)
	if secs != nil {
		n = min(max(*secs, 1), 600)
	}
	return time.Duration(n) * time.Second
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
	case "stop-failure":
		// Rust: turn_started_at.take() without record_turn — a failed turn
		// must not skew the average.
		e.run.TurnStartedAt = time.Time{}
		e.run.ApplyHook(subkind, "", now)
	case "session-end":
		// The desktop drops the pane's run and emits Unknown at seq+1; keep
		// that seq so a later frontend still sees the reset as newer.
		e.run = agent.Run{Seq: e.run.Seq + 1}
		e.brief.SessionEnded = true
		e.brief.Seq++
	}
}
