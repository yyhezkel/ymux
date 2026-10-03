package agent

// humanize.go — port of humanize_notification + clip in
// app/src-tauri/src/rpc_server.rs. Change both together; the copy below is
// the desktop's, character for character.

import (
	"encoding/json"
	"fmt"
	"math"
	"strings"
)

// Humanize turns a raw hook payload into a friendly (title, body) for a
// feed card or toast instead of dumping the JSON.
//
// subkind is the hook event (session-start, stop, notification, …). wsName,
// when non-empty, prefixes the body as "[ws] 🎯 " so several projects can
// be told apart. lang is the UI language: "he" is first-class, everything
// else falls back to English. Unknown subkinds return ("Claude", "").
func Humanize(subkind string, payload map[string]any, wsName, lang string) (title, body string) {
	cwd := str(payload, "cwd")
	tool := str(payload, "tool_name")
	cmd := ""
	if ti, ok := payload["tool_input"].(map[string]any); ok {
		cmd = str(ti, "command")
	}
	msg := str(payload, "message")
	// Stop carries response_summary on older Claude Code and
	// last_assistant_message on current ones; a blank summary falls
	// through to the latter.
	summary := str(payload, "response_summary")
	if strings.TrimSpace(summary) == "" {
		summary = str(payload, "last_assistant_message")
	}
	he := lang == "he"
	prefix := func(s string) string {
		if wsName == "" {
			return s
		}
		return fmt.Sprintf("[%s] 🎯 %s", wsName, s)
	}
	pick := func(heText, enText string) string {
		if he {
			return heText
		}
		return enText
	}

	switch subkind {
	case "session-start":
		return "Claude", prefix(pick("סשן התחיל ב-"+cwd, "Session started in "+cwd))
	case "session-end":
		var b string
		if secs, ok := uintField(payload, "session_duration_seconds"); ok {
			b = pick(cwd+" · משך "+fmtDur(secs), cwd+" · "+fmtDur(secs)+" elapsed")
		} else {
			b = pick("סשן הסתיים — "+cwd, "Session ended — "+cwd)
		}
		return pick("🎯 Session נסגר", "🎯 Session ended"), prefix(b)
	case "stop":
		var b string
		if summary != "" {
			b = ClipChars(summary, 120)
		} else {
			b = pick("סיים ב-"+cwd, "Finished in "+cwd)
		}
		return pick("🎯 Claude סיים — התור שלך", "🎯 Claude finished — your turn"), prefix(b)
	case "notification":
		b := msg
		if b == "" {
			b = pick("Claude זקוק לך", "Claude needs you")
		}
		return "Claude", prefix(b)
	case "pre-tool-use":
		return pick("Claude רוצה להריץ: "+tool, "Claude wants to run: "+tool), prefix(ClipChars(cmd, 100))
	case "post-tool-use":
		return pick("🔧 "+tool+" סיים", "🔧 "+tool+" completed"), prefix("")
	case "subagent-stop":
		return pick("🤖 סוכן-משנה סיים", "🤖 sub-agent finished"), prefix("")
	case "user-prompt-submit":
		return pick("💬 שאלה נשלחה", "💬 prompt sent"), prefix("")
	case "pre-compact":
		return pick("📦 קונטקסט דוחס", "📦 context compacted"), prefix("")
	}
	return "Claude", ""
}

func fmtDur(secs uint64) string {
	m, s := secs/60, secs%60
	if m > 0 {
		return fmt.Sprintf("%dm %ds", m, s)
	}
	return fmt.Sprintf("%ds", s)
}

// str reads a string field, "" when absent or not a string (serde's
// `.as_str().unwrap_or("")`).
func str(m map[string]any, key string) string {
	s, _ := m[key].(string)
	return s
}

// uintField mirrors serde_json's as_u64: a non-negative whole number.
// Accepts what encoding/json produces (float64, or json.Number under
// UseNumber) plus native integers for callers that build payloads in Go.
func uintField(m map[string]any, key string) (uint64, bool) {
	switch v := m[key].(type) {
	case float64:
		if v >= 0 && v == math.Trunc(v) && v < math.MaxUint64 {
			return uint64(v), true
		}
	case json.Number:
		var u uint64
		if _, err := fmt.Sscan(string(v), &u); err == nil && !strings.ContainsAny(string(v), ".eE") {
			return u, true
		}
	case int:
		if v >= 0 {
			return uint64(v), true
		}
	case int64:
		if v >= 0 {
			return uint64(v), true
		}
	case uint64:
		return v, true
	}
	return 0, false
}
