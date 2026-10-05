package term

// Phase 104 (WEB-DESIGN §4.2, B6): session history — the ended rows, the
// transcript reader, resume.

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const sid = "4f51e2fc-a5e6-4510-9565-b390290a09c1"

// historyHome writes a session-meta.json and one transcript under a temp HOME.
func historyHome(t *testing.T, sessions map[string]any, transcript string) string {
	t.Helper()
	home := t.TempDir()
	_ = os.MkdirAll(filepath.Join(home, ".ymux"), 0o755)
	b, _ := json.Marshal(map[string]any{"version": 1, "sessions": sessions})
	if err := os.WriteFile(MetaPath(home), b, 0o600); err != nil {
		t.Fatal(err)
	}
	if transcript != "" {
		dir := filepath.Join(home, ".claude", "projects", "-srv-api")
		_ = os.MkdirAll(dir, 0o755)
		if err := os.WriteFile(filepath.Join(dir, sid+".jsonl"), []byte(transcript), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	return home
}

func TestHistoryListsOnlyEndedClaudeRows(t *testing.T) {
	meta := map[string]MetaEntry{
		"old":     {ClaudeSessionID: "a", EndedAt: "2026-10-01T00:00:00Z", AutoName: "fix login · 2026-10-01"},
		"newer":   {ClaudeSessionID: "b", EndedAt: "2026-10-04T00:00:00Z", Cwd: "/srv"},
		"live":    {ClaudeSessionID: "c"},
		"noclaud": {EndedAt: "2026-10-04T00:00:00Z"},
		"revived": {ClaudeSessionID: "d", EndedAt: "2026-10-03T00:00:00Z"},
	}
	rows := History(meta, []Session{{Name: "live"}, {Name: "revived"}})
	if len(rows) != 2 || rows[0].Name != "newer" || rows[1].Name != "old" {
		t.Fatalf("history = %+v", rows)
	}
	if rows[1].Display != "fix login · 2026-10-01" || rows[0].Cwd != "/srv" {
		t.Errorf("row fields = %+v", rows)
	}
}

const transcriptFixture = `{"type":"user","message":{"role":"user","content":"fix the login bug"},"timestamp":"t1"}
{"type":"user","isMeta":true,"message":{"role":"user","content":"<internal>"}}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]},"timestamp":"t2"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"secret file list"}]}}
{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent noise"}]}}
not json at all
{"type":"ai-title","title":"x"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed."}]},"timestamp":"t3"}
`

func TestTranscriptRoute(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	s.home = historyHome(t, map[string]any{}, transcriptFixture)

	w := do(s, "GET", "/api/v2/claude/sessions/"+sid+"/transcript", "owner-token", "")
	if w.Code != http.StatusOK {
		t.Fatalf("transcript → %d %s", w.Code, w.Body)
	}
	var got struct {
		Total int    `json:"total"`
		Turns []Turn `json:"turns"`
	}
	_ = json.Unmarshal(w.Body.Bytes(), &got)
	want := []Turn{{"user", "fix the login bug", "", "t1"}, {"assistant", "Looking.", "", "t2"},
		{"tool", "", "Bash", "t2"}, {"assistant", "Fixed.", "", "t3"}}
	if got.Total != 4 || len(got.Turns) != 4 {
		t.Fatalf("turns = %+v", got)
	}
	for i := range want {
		if got.Turns[i] != want[i] {
			t.Errorf("turn %d = %+v, want %+v", i, got.Turns[i], want[i])
		}
	}
	if strings.Contains(w.Body.String(), "secret file list") || strings.Contains(w.Body.String(), "subagent noise") {
		t.Error("tool results and sidechain lines must be left out")
	}
	w = do(s, "GET", "/api/v2/claude/sessions/"+sid+"/transcript?offset=3&limit=10", "owner-token", "")
	_ = json.Unmarshal(w.Body.Bytes(), &got)
	if got.Total != 4 || len(got.Turns) != 1 || got.Turns[0].Text != "Fixed." {
		t.Errorf("page 2 = %+v", got)
	}
}

func TestTranscriptIDIsValidatedBeforeAnyPath(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	s.home = historyHome(t, map[string]any{}, transcriptFixture)
	// (A literal ".." is not listed: ServeMux cleans such a path and answers
	// 301 before any handler runs — the encoded form is the real probe.)
	for _, id := range []string{"%2e%2e%2fetc", "*", "4f51e2fc-a5e6-4510-9565-b390290a09c", "00000000-0000-0000-0000-000000000000"} {
		if w := do(s, "GET", "/api/v2/claude/sessions/"+id+"/transcript", "owner-token", ""); w.Code != http.StatusNotFound {
			t.Errorf("id %q → %d, want 404", id, w.Code)
		}
	}
}

func TestResumeRunsClaudeAsAnArgv(t *testing.T) {
	s, calls := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:9")
	s.claudeBin = "/opt/claude"
	cwd := t.TempDir()
	s.home = historyHome(t, map[string]any{
		"api":  map[string]any{"claude_session_id": sid, "ended_at": "2026-10-04T00:00:00Z", "cwd": cwd},
		"live": map[string]any{"claude_session_id": sid},
	}, "")

	w := do(s, "POST", "/api/v2/term/history/api/resume", "owner-token", `{"policy":"gate"}`)
	if w.Code != http.StatusCreated {
		t.Fatalf("resume → %d %s", w.Code, w.Body)
	}
	var resp map[string]any
	_ = json.Unmarshal(w.Body.Bytes(), &resp)
	if resp["name"] != "api" || resp["policy"] != "gate" || resp["claude_session_id"] != sid {
		t.Errorf("resume reply = %v (a free name is reused)", resp)
	}
	argv := lastCreate(t, calls)
	joined := strings.Join(argv, " ")
	if !strings.HasSuffix(joined, "-- /opt/claude --resume "+sid) || !strings.Contains(joined, "-c "+cwd) {
		t.Errorf("new-session argv = %v", argv)
	}
	if w := do(s, "POST", "/api/v2/term/history/live/resume", "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("a live row is not history → %d", w.Code)
	}
	if w := do(s, "POST", "/api/v2/term/history/nope/resume", "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("unknown row → %d", w.Code)
	}
}

func TestResumeRefusesAForgedSessionID(t *testing.T) {
	s, calls := hookService("tmux 3.4")
	s.home = historyHome(t, map[string]any{
		"x": map[string]any{"claude_session_id": "abc; rm -rf ~", "ended_at": "2026-10-04T00:00:00Z"},
	}, "")
	if w := do(s, "POST", "/api/v2/term/history/x/resume", "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("forged id → %d", w.Code)
	}
	for _, c := range *calls {
		if len(c) > 1 && c[1] == "new-session" {
			t.Fatalf("a forged id reached tmux: %v", c)
		}
	}
}

func TestHistoryRoute(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	s.home = historyHome(t, map[string]any{
		"api": map[string]any{"claude_session_id": sid, "ended_at": "2026-10-04T00:00:00Z", "label": "API"},
	}, "")
	w := do(s, "GET", "/api/v2/term/history", "owner-token", "")
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), `"display":"API"`) {
		t.Errorf("history → %d %s", w.Code, w.Body)
	}
}
