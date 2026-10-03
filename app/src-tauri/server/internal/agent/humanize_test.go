package agent

// Golden tests for Humanize. The Rust humanize_notification has no tests of
// its own, so the expected strings here are copied from its source
// (rpc_server.rs) — they ARE the cross-check that the port matches.

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestHumanizeGolden(t *testing.T) {
	stopPayload := map[string]any{"cwd": "/srv/app", "last_assistant_message": "All tests pass."}
	toolPayload := map[string]any{
		"cwd":        "/srv/app",
		"tool_name":  "Bash",
		"tool_input": map[string]any{"command": "go test ./..."},
	}
	cases := []struct {
		subkind, lang   string
		payload         map[string]any
		wantTitle, want string
	}{
		{"session-start", "en", map[string]any{"cwd": "/srv/app"}, "Claude", "Session started in /srv/app"},
		{"session-start", "he", map[string]any{"cwd": "/srv/app"}, "Claude", "סשן התחיל ב-/srv/app"},
		{"session-end", "en", map[string]any{"cwd": "/srv/app", "session_duration_seconds": float64(125)}, "🎯 Session ended", "/srv/app · 2m 5s elapsed"},
		{"session-end", "he", map[string]any{"cwd": "/srv/app", "session_duration_seconds": float64(42)}, "🎯 Session נסגר", "/srv/app · משך 42s"},
		{"session-end", "en", map[string]any{"cwd": "/srv/app"}, "🎯 Session ended", "Session ended — /srv/app"},
		{"session-end", "he", map[string]any{"cwd": "/srv/app"}, "🎯 Session נסגר", "סשן הסתיים — /srv/app"},
		{"stop", "en", stopPayload, "🎯 Claude finished — your turn", "All tests pass."},
		{"stop", "he", stopPayload, "🎯 Claude סיים — התור שלך", "All tests pass."},
		{"stop", "en", map[string]any{"cwd": "/srv/app"}, "🎯 Claude finished — your turn", "Finished in /srv/app"},
		{"stop", "he", map[string]any{"cwd": "/srv/app"}, "🎯 Claude סיים — התור שלך", "סיים ב-/srv/app"},
		{"notification", "en", map[string]any{"message": "Claude needs your permission"}, "Claude", "Claude needs your permission"},
		{"notification", "en", map[string]any{}, "Claude", "Claude needs you"},
		{"notification", "he", map[string]any{}, "Claude", "Claude זקוק לך"},
		{"pre-tool-use", "en", toolPayload, "Claude wants to run: Bash", "go test ./..."},
		{"pre-tool-use", "he", toolPayload, "Claude רוצה להריץ: Bash", "go test ./..."},
		{"post-tool-use", "en", toolPayload, "🔧 Bash completed", ""},
		{"post-tool-use", "he", toolPayload, "🔧 Bash סיים", ""},
		{"subagent-stop", "en", nil, "🤖 sub-agent finished", ""},
		{"subagent-stop", "he", nil, "🤖 סוכן-משנה סיים", ""},
		{"user-prompt-submit", "en", nil, "💬 prompt sent", ""},
		{"user-prompt-submit", "he", nil, "💬 שאלה נשלחה", ""},
		{"pre-compact", "en", nil, "📦 context compacted", ""},
		{"pre-compact", "he", nil, "📦 קונטקסט דוחס", ""},
		{"something-new", "en", toolPayload, "Claude", ""},
		// ar/ru fall back to English.
		{"stop", "ar", map[string]any{"cwd": "/x"}, "🎯 Claude finished — your turn", "Finished in /x"},
	}
	for _, c := range cases {
		title, body := Humanize(c.subkind, c.payload, "", c.lang)
		if title != c.wantTitle || body != c.want {
			t.Errorf("%s/%s = (%q, %q), want (%q, %q)", c.subkind, c.lang, title, body, c.wantTitle, c.want)
		}
	}
}

func TestHumanizeResponseSummaryWinsUnlessBlank(t *testing.T) {
	p := map[string]any{"response_summary": "From summary", "last_assistant_message": "From message"}
	if _, b := Humanize("stop", p, "", "en"); b != "From summary" {
		t.Errorf("body = %q", b)
	}
	p["response_summary"] = "   "
	if _, b := Humanize("stop", p, "", "en"); b != "From message" {
		t.Errorf("a blank summary must fall through, body = %q", b)
	}
}

func TestHumanizeClipsLongBodies(t *testing.T) {
	long := strings.Repeat("א", 300)
	_, b := Humanize("stop", map[string]any{"last_assistant_message": long}, "", "he")
	if b != strings.Repeat("א", 120)+"…" {
		t.Errorf("stop body not clipped to 120: %d runes", len([]rune(b)))
	}
	_, b = Humanize("pre-tool-use", map[string]any{"tool_input": map[string]any{"command": strings.Repeat("x", 300)}}, "", "en")
	if b != strings.Repeat("x", 100)+"…" {
		t.Errorf("command not clipped to 100: %q", b)
	}
}

func TestHumanizeWorkspacePrefix(t *testing.T) {
	_, b := Humanize("stop", map[string]any{"cwd": "/a"}, "api", "en")
	if b != "[api] 🎯 Finished in /a" {
		t.Errorf("body = %q", b)
	}
	// The prefix applies to an empty body too (the observability hooks).
	if _, b := Humanize("pre-compact", nil, "api", "en"); b != "[api] 🎯 " {
		t.Errorf("body = %q", b)
	}
	// …but not to an unknown subkind, which returns no body at all.
	if _, b := Humanize("nope", nil, "api", "en"); b != "" {
		t.Errorf("body = %q", b)
	}
}

func TestHumanizeDurationNeedsAWholeNonNegativeNumber(t *testing.T) {
	for _, v := range []any{float64(-1), 1.5, "12", nil} {
		_, b := Humanize("session-end", map[string]any{"cwd": "/a", "session_duration_seconds": v}, "", "en")
		if b != "Session ended — /a" {
			t.Errorf("%v: body = %q", v, b)
		}
	}
	_, b := Humanize("session-end", map[string]any{"cwd": "/a", "session_duration_seconds": json.Number("61")}, "", "en")
	if b != "/a · 1m 1s elapsed" {
		t.Errorf("json.Number: body = %q", b)
	}
}
