package term

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestParseWorktreePorcelain(t *testing.T) {
	text := "worktree /srv/app\nHEAD abc\nbranch refs/heads/main\n\n" +
		"worktree /srv/app-fix\nHEAD def\ndetached\nlocked\n\nworktree /gone\nHEAD 0\nprunable gitdir file points to non-existent location\n"
	got := parseWorktreePorcelain(text)
	if len(got) != 3 || !got[0].IsMain || got[1].IsMain || *got[0].Branch != "main" ||
		got[1].Branch != nil || !got[1].IsDetached || !got[1].IsLocked || !got[2].IsPrunable || got[0].Head != "abc" {
		b, _ := json.Marshal(got)
		t.Fatalf("parsed %s", b)
	}
	if len(parseWorktreePorcelain("fatal: not a git repository\n")) != 0 {
		t.Error("an error line parsed as a worktree")
	}
}

func TestGitWorktreesREST(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	dir := t.TempDir()
	var gotArgs []string
	old := gitRun
	defer func() { gitRun = old }()
	gitRun = func(_ context.Context, d string, args ...string) (string, error) {
		gotArgs = append([]string{d}, args...)
		if strings.HasSuffix(d, "repo") {
			return "worktree " + d + "\nHEAD 1\nbranch refs/heads/main\n", nil
		}
		return "fatal: not a git repository (or any of the parent directories): .git\n", errors.New("exit status 128")
	}
	if w := do(s, "POST", "/api/v2/web/git/worktrees", "owner-token", `{"path":""}`); w.Code != http.StatusBadRequest {
		t.Errorf("empty path → %d", w.Code)
	}
	if w := do(s, "POST", "/api/v2/web/git/worktrees", "owner-token", `{"path":"relative/x"}`); w.Code != http.StatusBadRequest {
		t.Errorf("relative path → %d", w.Code)
	}
	if w := do(s, "POST", "/api/v2/web/git/worktrees", "owner-token", `{"path":"`+dir+`/missing"}`); w.Code != http.StatusNotFound ||
		!strings.Contains(w.Body.String(), "directory not found on the host") {
		t.Errorf("missing dir → %d %s", w.Code, w.Body)
	}
	w := do(s, "POST", "/api/v2/web/git/worktrees", "owner-token", `{"path":"`+dir+`"}`)
	var res gitProbeResult
	_ = json.Unmarshal(w.Body.Bytes(), &res)
	if w.Code != http.StatusOK || res.OK || !strings.HasPrefix(res.Error, "fatal: not a git repository") || res.Worktrees == nil {
		t.Errorf("not a repo → %d %s", w.Code, w.Body)
	}
	if strings.Join(gotArgs[1:], " ") != "worktree list --porcelain" || gotArgs[0] != dir {
		t.Errorf("git argv = %q", gotArgs)
	}
	repo := filepath.Join(dir, "repo")
	_ = os.Mkdir(repo, 0o700)
	w = do(s, "POST", "/api/v2/web/git/worktrees", "owner-token", `{"path":"`+repo+`"}`)
	res = gitProbeResult{}
	_ = json.Unmarshal(w.Body.Bytes(), &res)
	if !res.OK || len(res.Worktrees) != 1 || !res.Worktrees[0].IsMain || res.Error != "" {
		t.Errorf("repo → %s", w.Body)
	}
}
