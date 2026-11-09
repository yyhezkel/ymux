package term

// inject.go — context.inject (docs/CONTEXT.md "Injection back into the
// agent"), a port of build_injection / injection_for_hook in
// app/src-tauri/src/context_store.rs. Change both together:
// internal/agent/testdata/injection.json is read by the tests on both sides.
//
// The SessionStart hook asks here for the text it hands to Claude Code as
// additionalContext. It runs inside the CLI's ~300 ms budget, so it reads
// memory only. The daemon's store is its own: a session the desktop created
// has no record here and gets nothing.
//
// Rule #1: logs carry the source, the ids and the byte count, never the text.

import (
	"encoding/json"
	"slices"
	"sort"
	"strings"
	"unicode"

	"ymux-server/internal/agent"
)

const (
	injectMaxBytes    = 1536 // the cap on what the agent gets, in UTF-8 bytes
	injectLogLines    = 5    // delta lines on compact / resume
	injectMaxSiblings = 8    // sessions listed on startup
	injectPromptChars = 400  // "Original request", clipped harder than stored
	injectHeader      = "[ymux-context]"
)

// injectFlat makes s one line with no control characters: what an agent reads
// must not carry stray escapes or fake structure from a multi-line field.
func injectFlat(s string) string {
	var b strings.Builder
	for _, r := range s {
		if r == '\n' || r == '\r' || r == '\t' {
			r = ' '
		}
		if !unicode.IsControl(r) {
			b.WriteRune(r)
		}
	}
	return strings.Join(strings.Fields(b.String()), " ")
}

// injectDeltaLine is one progress line: the turn's delta, or the closed marker.
func injectDeltaLine(e ContextLogEntry) (string, bool) {
	if e.Kind == "closed" {
		return "- session closed", true
	}
	if e.Delta == nil {
		return "", false
	}
	d := injectFlat(*e.Delta)
	return "- " + d, d != ""
}

// injectClip caps s at injectMaxBytes on a character boundary.
func injectClip(s string) string {
	if len(s) <= injectMaxBytes {
		return s
	}
	const ell = "…"
	return clipBytes(s, injectMaxBytes-len(ell)) + ell
}

// injectAssemble joins the header, head, as many items as fit (the oldest
// drop first) and tail. A result that is still too long is clipped.
func injectAssemble(head, items, tail []string) string {
	for start := 0; ; start++ {
		lines := append([]string{injectHeader}, head...)
		lines = append(lines, items[start:]...)
		lines = append(lines, tail...)
		text := strings.Join(lines, "\n")
		if len(text) <= injectMaxBytes || start >= len(items) {
			return injectClip(text)
		}
	}
}

// buildInjection is the additionalContext for a SessionStart hook, "" when
// there is nothing worth saying. intent is "" when the workspace has none.
func buildInjection(source string, this *SessionContext, intent string, siblings []SessionContext) string {
	switch source {
	case "compact", "resume":
		return injectResume(this)
	case "startup":
		return injectStartup(this, intent, siblings)
	}
	return ""
}

// injectResume has the Context Rail card's shape.
func injectResume(c *SessionContext) string {
	if c == nil {
		return ""
	}
	var head, items, tail []string
	addField := func(to *[]string, label string, v *string) {
		if v == nil {
			return
		}
		if f := injectFlat(*v); f != "" {
			*to = append(*to, label+": "+f)
		}
	}
	addField(&head, "Goal", c.Goal)
	addField(&head, "Done when", c.DoneWhen)
	for i := len(c.Log) - 1; i >= 0; i-- {
		t := c.Log[i]
		if t.Kind != "turn" {
			continue
		}
		task := "(no task reported)"
		if t.Task != nil {
			if f := injectFlat(*t.Task); f != "" {
				task = f
			}
		}
		head = append(head, "Now: ["+string(t.Status)+"] "+task)
		addField(&head, "Next", t.Next)
		break
	}
	for i := len(c.Log) - 1; i >= 0 && len(items) < injectLogLines; i-- {
		if line, ok := injectDeltaLine(c.Log[i]); ok {
			items = append(items, line)
		}
	}
	slices.Reverse(items) // oldest → newest
	if c.FirstPrompt != nil {
		p := agent.ClipChars(*c.FirstPrompt, injectPromptChars)
		addField(&tail, "Original request", &p)
	}
	if len(head) == 0 && len(items) == 0 && len(tail) == 0 {
		return ""
	}
	if len(items) > 0 {
		head = append(head, "Recent progress (oldest → newest):")
	}
	return injectAssemble(head, items, tail)
}

// injectStartup is the workspace intent plus one line per OTHER open session
// of the workspace, most recent first.
func injectStartup(this *SessionContext, intent string, siblings []SessionContext) string {
	var head []string
	if i := injectFlat(intent); i != "" {
		head = append(head, "Workspace goal: "+i)
	}
	var open []SessionContext
	for _, s := range siblings {
		isSelf := this != nil && this.SessionID == s.SessionID
		if !isSelf && len(s.Log) > 0 && s.Log[len(s.Log)-1].Kind == "turn" {
			open = append(open, s)
		}
	}
	sort.SliceStable(open, func(i, j int) bool { return open[i].lastActivity() > open[j].lastActivity() })
	if len(open) > injectMaxSiblings {
		open = open[:injectMaxSiblings]
	}
	var items []string
	for _, s := range open {
		task := "(untitled session)"
		for i := len(s.Log) - 1; i >= 0; i-- {
			if s.Log[i].Task != nil {
				task = injectFlat(*s.Log[i].Task)
				break
			}
		}
		items = append(items, "- "+task+" — "+string(s.Log[len(s.Log)-1].Status))
	}
	if len(head) == 0 && len(items) == 0 {
		return ""
	}
	if len(items) > 0 {
		head = append(head, "Other agent sessions in this workspace:")
	}
	return injectAssemble(head, items, nil)
}

// contextInject answers the context.inject RPC. pane_id and tmux_session are
// not read: the HMAC already identified the session (t.e). Any failure is an
// empty answer, because the hook fails open.
func (t termHookTarget) contextInject(raw json.RawMessage) map[string]any {
	empty := map[string]any{"additional_context": ""}
	var p struct {
		SessionID string `json:"session_id"`
		Source    string `json:"source"`
	}
	if json.Unmarshal(raw, &p) != nil {
		return empty
	}
	r := t.r
	r.mu.Lock()
	released, pane, ws := t.e.released, t.e.paneID, t.e.workspaceID
	r.mu.Unlock()
	if released || r.context == nil || !r.settings.rules().injectContext {
		return empty
	}
	this := r.context.get(p.SessionID)
	if ws == "" && this != nil && this.WsID != nil {
		ws = *this.WsID
	}
	intent := ""
	if w, ok := r.webws.get(ws); ok {
		intent = w.Intent
	}
	var siblings []SessionContext
	if p.Source == "startup" && ws != "" {
		siblings = r.context.list(ws)
	}
	text := buildInjection(p.Source, this, intent, siblings)
	logger.Debug("context inject", "source", p.Source, "session", p.SessionID, "pane", pane, "bytes", len(text))
	return map[string]any{"additional_context": text}
}
