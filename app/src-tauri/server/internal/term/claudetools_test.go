package term

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

const usageSample = `{"type":"result","subtype":"success","is_error":false,"result":"You are currently using your subscription to power your Claude Code usage\n\nCurrent session: 33% used · resets Jul 8, 4:10am (Europe/Berlin)\nCurrent week (all models): 11% used · resets Jul 14, 10pm (Europe/Berlin)\nCurrent week (Fable): 16% used · resets Jul 14, 10pm (Europe/Berlin)\n\nWhat's contributing to your limits usage?\nApproximate, based on local sessions on this machine.\n\nLast 24h · 3466 requests · 10 sessions\n  94% of your usage came from subagent-heavy sessions\n  Top subagents: implementer 40%, loop 8%\n\nLast 7d · 13897 requests · 26 sessions\n  99% of your usage came from subagent-heavy sessions","total_cost_usd":0}`

// The Rust test's sample and assertions (claude_usage.rs parses_real_usage).
func TestParseUsage(t *testing.T) {
	u, err := parseUsage(usageSample, 1_700_000_000)
	if err != nil {
		t.Fatal(err)
	}
	if u.SessionPct != 33 || u.SessionReset != "Jul 8, 4:10am (Europe/Berlin)" || u.WeekPct != 11 ||
		len(u.Models) != 1 || u.Models[0].Name != "Fable" || u.Models[0].Pct != 16 ||
		len(u.Contributing24h) != 3 || len(u.Contributing7d) != 2 || u.FetchedUnix != 1_700_000_000 {
		t.Fatalf("%+v", u)
	}
	if _, err := parseUsage("not json", 0); err == nil {
		t.Error("garbage parsed")
	}
	if _, err := parseUsage(`{"result":"hello world"}`, 0); err == nil {
		t.Error("no session line parsed")
	}
}

func TestUsageCacheAndSingleProbe(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	calls := 0
	old := runClaude
	defer func() { runClaude = old }()
	runClaude = func(_ context.Context, _ string, args []string, _ string) (string, error) {
		calls++
		if args[1] != "/usage" {
			t.Fatalf("argv %q", args)
		}
		return usageSample, nil
	}
	for i := 0; i < 2; i++ {
		if w := do(s, "GET", "/api/v2/claude/usage", "owner-token", ""); w.Code != 200 || !strings.Contains(w.Body.String(), `"session_pct":33`) {
			t.Fatalf("→ %d %s", w.Code, w.Body)
		}
	}
	if calls != 1 {
		t.Fatalf("probed %d times, want 1 (cache)", calls)
	}
	_ = do(s, "GET", "/api/v2/claude/usage?force=1", "owner-token", "")
	if calls != 2 {
		t.Fatalf("force did not re-probe: %d", calls)
	}
}

func TestOverviewParseAndArgv(t *testing.T) {
	names := []string{"a", "b", "c"}
	raw := `{"result":"sure:\n[{\"i\":2,\"status\":\"Working\",\"summary\":\"building\"},{\"i\":9,\"status\":\"idle\"},{\"i\":1,\"status\":\"weird\"}]"}`
	got := parseOverview(raw, names)
	if got[0].Status != "unknown" || got[1].Status != "working" || got[1].Summary != "building" || got[2].Status != "unknown" {
		t.Fatalf("%+v", got)
	}
	if all := parseOverview("no json", names); all[2].Status != "unknown" {
		t.Fatal("garbage must give unknown rows")
	}
	if c := clipCapture("\x1b[31mred\x1b[0m\n\x1b]0;title\x07x\n\n\n"); c != "red\nx" {
		t.Fatalf("clip = %q", c)
	}

	s, _ := hookService("tmux 3.4")
	old := runClaude
	defer func() { runClaude = old }()
	var argv []string
	var input string
	runClaude = func(_ context.Context, _ string, args []string, stdin string) (string, error) {
		argv, input = args, stdin
		return `{"result":"[{\"i\":1,\"status\":\"idle\",\"summary\":\"שקט\"}]"}`, nil
	}
	w := do(s, "POST", "/api/v2/term/sessions/summarize", "owner-token", `{"names":["s1","s1"," "],"lang":"he"}`)
	var rows []SessionSummary
	_ = json.Unmarshal(w.Body.Bytes(), &rows)
	if w.Code != 200 || len(rows) != 1 || rows[0].Summary != "שקט" {
		t.Fatalf("→ %d %s", w.Code, w.Body)
	}
	joined := strings.Join(argv, "\x00")
	if !strings.Contains(joined, "--tools\x00\x00") || !strings.Contains(joined, "--no-session-persistence") || !strings.Contains(argv[1], "in Hebrew") {
		t.Fatalf("argv = %q", argv)
	}
	if !strings.Contains(input, "### SESSION 1") {
		t.Fatalf("input = %q", input)
	}
	if w := do(s, "POST", "/api/v2/term/sessions/summarize", "owner-token", `{"names":["bad:name"]}`); w.Code != http.StatusBadRequest {
		t.Fatalf("bad name → %d", w.Code)
	}
}
