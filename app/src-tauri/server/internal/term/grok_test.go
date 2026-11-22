package term

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const grokSid = "abc123"

func writeGrokSession(t *testing.T, root, id, summary, updates string) {
	t.Helper()
	dir := filepath.Join(root, "grp", id)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if summary != "" {
		if err := os.WriteFile(filepath.Join(dir, "summary.json"), []byte(summary), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(dir, "updates.jsonl"), []byte(updates), 0o600); err != nil {
		t.Fatal(err)
	}
}

const grokUpdates = `{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"text":"hello"}}}}
{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"text":"world"}}}}
{"params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"ok"}}}}
{"params":{"update":{"sessionUpdate":"tool_call","title":"Bash"}}}
{"params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"text":"secret thought"}}}}
`

func TestGrokTranscriptMergesChunks(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	home := historyHome(t, map[string]any{}, "")
	s.home = home
	writeGrokSession(t, filepath.Join(home, ".grok", "sessions"), grokSid,
		`{"title":"Hello","info":{"cwd":"/work"}}`, grokUpdates)
	w := do(s, "GET", "/api/v2/claude/sessions/"+grokSid+"/transcript", "owner-token", "")
	if w.Code != http.StatusOK {
		t.Fatalf("transcript → %d %s", w.Code, w.Body)
	}
	var got struct {
		Total int    `json:"total"`
		Turns []Turn `json:"turns"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.Total != 3 || len(got.Turns) != 3 {
		t.Fatalf("turns = %+v", got.Turns)
	}
	if got.Turns[0] != (Turn{Role: "user", Text: "helloworld"}) ||
		got.Turns[1] != (Turn{Role: "assistant", Text: "ok"}) ||
		got.Turns[2] != (Turn{Role: "tool", Tool: "Bash"}) {
		t.Fatalf("turns = %+v", got.Turns)
	}
	if strings.Contains(w.Body.String(), "secret thought") {
		t.Error("thinking must be left out")
	}
}

func TestGrokTranscriptRejectsUnsafeID(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	s.home = historyHome(t, map[string]any{}, "")
	for _, id := range []string{"-abc", "abc.def"} {
		if w := do(s, "GET", "/api/v2/claude/sessions/"+id+"/transcript", "owner-token", ""); w.Code != http.StatusNotFound {
			t.Errorf("id %q → %d, want 404", id, w.Code)
		}
	}
}

func TestResumeGrokWhenThatIsTheOnlyTranscript(t *testing.T) {
	s, calls := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:9")
	s.claudeBin = "/opt/claude"
	cwd := t.TempDir()
	home := historyHome(t, map[string]any{
		"g": map[string]any{"claude_session_id": grokSid, "ended_at": "2026-10-04T00:00:00Z", "cwd": cwd},
	}, "")
	s.home = home
	writeGrokSession(t, filepath.Join(home, ".grok", "sessions"), grokSid, `{"info":{"cwd":"/work"}}`, grokUpdates)

	w := do(s, "POST", "/api/v2/term/history/g/resume", "owner-token", `{"policy":"gate"}`)
	if w.Code != http.StatusCreated {
		t.Fatalf("resume → %d %s", w.Code, w.Body)
	}
	joined := strings.Join(lastCreate(t, calls), " ")
	if !strings.Contains(joined, "grok --resume "+grokSid) || strings.Contains(joined, "/opt/claude") {
		t.Errorf("new-session argv = %s", joined)
	}

	// A safe-looking id with no file is not a Claude UUID, so it does not launch.
	s.home = historyHome(t, map[string]any{
		"x": map[string]any{"claude_session_id": "nope999", "ended_at": "2026-10-04T00:00:00Z"},
	}, "")
	if w := do(s, "POST", "/api/v2/term/history/x/resume", "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("missing grok file → %d", w.Code)
	}
}

func TestListGrokSessions(t *testing.T) {
	root := t.TempDir()
	writeGrokSession(t, root, "keep1",
		`{"title":"Hello login","hidden":false,"updated_at":"2026-11-22T10:00:00Z","info":{"cwd":"/work"}}`,
		`{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"text":"fix the login"}}}}`+"\n")
	writeGrokSession(t, root, "hide1",
		`{"title":"Hidden","hidden":true,"info":{"cwd":"/work"}}`,
		`{"params":{"update":{"sessionUpdate":"user_message_chunk","content":"x"}}}`+"\n")
	writeGrokSession(t, root, "other",
		`{"title":"Other","info":{"cwd":"/other"}}`,
		`{"params":{"update":{"sessionUpdate":"user_message_chunk","content":"x"}}}`+"\n")
	if err := os.MkdirAll(filepath.Join(root, "grp", "bad id"), 0o755); err != nil {
		t.Fatal(err)
	}
	old := grokSessionsRoot
	grokSessionsRoot = func() string { return root }
	t.Cleanup(func() { grokSessionsRoot = old })

	s, _ := hookService("tmux 3.4")
	w := do(s, "GET", "/api/v2/grok/sessions?project_path=%2Fwork", "owner-token", "")
	if w.Code != http.StatusOK {
		t.Fatalf("list → %d %s", w.Code, w.Body)
	}
	var rows []ClaudeSessionInfo
	if err := json.Unmarshal(w.Body.Bytes(), &rows); err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].SessionID != "keep1" || rows[0].ProjectPath != "/work" || rows[0].Title != "Hello login" {
		t.Fatalf("rows = %+v", rows)
	}
	if rows[0].LastUser != "fix the login" {
		t.Fatalf("preview = %q", rows[0].LastUser)
	}
	w = do(s, "GET", "/api/v2/grok/sessions?q=login", "owner-token", "")
	if err := json.Unmarshal(w.Body.Bytes(), &rows); err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].SessionID != "keep1" {
		t.Fatalf("search rows = %+v", rows)
	}
}
