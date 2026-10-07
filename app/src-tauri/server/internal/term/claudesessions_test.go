package term

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func writeTranscript(t *testing.T, root, proj, id string, mtime time.Time, lines ...string) {
	t.Helper()
	dir := filepath.Join(root, proj)
	_ = os.MkdirAll(dir, 0o755)
	p := filepath.Join(dir, id+".jsonl")
	if err := os.WriteFile(p, []byte(strings.Join(lines, "\n")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	_ = os.Chtimes(p, mtime, mtime)
}

func TestListClaudeSessions(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "-srv-app", "old", now.Add(-time.Hour),
		`{"type":"user","cwd":"/srv/app","message":{"content":[{"type":"text","text":"fix the \"login\" bug\nplease"}]}}`,
		`{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}`)
	writeTranscript(t, root, "-srv-app", "new", now,
		`{"type":"user","cwd":"/srv/app/","isSidechain":true,"message":{"content":"`+strings.Repeat("א", 100)+`"}}`)
	writeTranscript(t, root, "-srv-other", "other", now.Add(-time.Minute),
		`{"type":"user","cwd":"/srv/other","message":{"content":"x"}}`)

	all := listClaudeSessions(root, 10, "")
	if len(all) != 3 || all[0].SessionID != "new" || all[1].SessionID != "other" || all[2].SessionID != "old" {
		b, _ := json.Marshal(all)
		t.Fatalf("order: %s", b)
	}
	if !all[0].IsSubagent || !strings.HasSuffix(all[0].LastUser, "…") || len([]rune(all[0].LastUser)) != 81 {
		t.Fatalf("new = %+v", all[0])
	}
	if all[2].LastUser != `fix the "login" bug please` || all[2].LastAssistant != "done" || all[2].ProjectPath != "/srv/app" {
		t.Fatalf("old = %+v", all[2])
	}
	scoped := listClaudeSessions(root, 1, "/srv/app")
	if len(scoped) != 1 || scoped[0].SessionID != "new" {
		t.Fatalf("scoped (limit after filter) = %+v", scoped)
	}
	if got := listClaudeSessions(root, 10, "/srv/app"); len(got) != 2 {
		t.Fatalf("scope kept %d, want 2", len(got))
	}
	if got := listClaudeSessions(filepath.Join(root, "missing"), 10, ""); got == nil || len(got) != 0 {
		t.Fatalf("missing root = %#v", got)
	}
}

func TestClaudeSessionsREST(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	root := t.TempDir()
	old := claudeProjectsRoot
	defer func() { claudeProjectsRoot = old }()
	claudeProjectsRoot = func() string { return root }
	writeTranscript(t, root, "-p", "s1", time.Now(), `{"type":"user","cwd":"/p","message":{"content":"hi"}}`)
	w := do(s, "GET", "/api/v2/claude/sessions?limit=5&project_path=%2Fp", "owner-token", "")
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), `"session_id":"s1"`) {
		t.Fatalf("→ %d %s", w.Code, w.Body)
	}
}
