package term

// gitprobe.go — `git worktree list` for a folder on this box (Phase 115, F1).
//
// The browser's `project_folder_probe` and `git_probe_worktrees` (WEB-DESIGN
// §4, "worktrees / project probe") — the desktop runs the same git command
// locally or over SSH (worktrees.rs). One route answers both: a folder that
// does not exist is a 404 (the probe's only hard error); otherwise 200 with
// git's verdict, so the probe can read "not a repo" as false while the
// "check git" flow shows git's own message.
//
// Rule #3: git runs as an argv, the path is one argument. The caller already
// holds shell:attach (Service.gate), so reading a folder adds no reach.

import (
	"bufio"
	"context"
	"encoding/json"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// WorktreeEntry mirrors the desktop's (worktrees.rs, bindings/WorktreeEntry.ts).
type WorktreeEntry struct {
	Path       string  `json:"path"`
	Branch     *string `json:"branch"`
	Head       string  `json:"head"`
	IsMain     bool    `json:"is_main"`
	IsDetached bool    `json:"is_detached"`
	IsLocked   bool    `json:"is_locked"`
	IsPrunable bool    `json:"is_prunable"`
}

type gitProbeResult struct {
	OK        bool            `json:"ok"`
	Worktrees []WorktreeEntry `json:"worktrees"`
	Error     string          `json:"error,omitempty"` // git's message when !ok
}

// gitRun is swapped in tests.
var gitRun = func(ctx context.Context, dir string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...)
	cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0", "LC_ALL=C")
	out, err := cmd.CombinedOutput()
	return string(out), err
}

// expandHome turns a leading "~" into $HOME; anything else must be absolute.
func expandHome(p string) (string, bool) {
	p = strings.TrimSpace(p)
	if p == "~" || strings.HasPrefix(p, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", false
		}
		p = filepath.Join(home, strings.TrimPrefix(p, "~"))
	}
	if !filepath.IsAbs(p) {
		return "", false
	}
	return filepath.Clean(p), true
}

func (s *Service) handleGitWorktrees(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Path string `json:"path"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 8<<10)).Decode(&in); err != nil || strings.TrimSpace(in.Path) == "" {
		http.Error(w, "project path is required", http.StatusBadRequest)
		return
	}
	dir, ok := expandHome(in.Path)
	if !ok {
		http.Error(w, "the path must be absolute", http.StatusBadRequest)
		return
	}
	if st, err := os.Stat(dir); err != nil || !st.IsDir() {
		http.Error(w, "directory not found on the host: "+in.Path, http.StatusNotFound)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	defer cancel()
	out, err := gitRun(ctx, dir, "worktree", "list", "--porcelain")
	list := parseWorktreePorcelain(out)
	res := gitProbeResult{OK: err == nil && len(list) > 0, Worktrees: list}
	if !res.OK {
		res.Worktrees = []WorktreeEntry{}
		res.Error = gitError(out, err)
	}
	writeJSON(w, http.StatusOK, res)
}

// gitError is git's first "fatal:"/"error:" line, else the first line.
func gitError(out string, err error) string {
	first := ""
	for _, l := range strings.Split(out, "\n") {
		l = strings.TrimSpace(l)
		if l == "" {
			continue
		}
		if strings.HasPrefix(l, "fatal:") || strings.HasPrefix(l, "error:") {
			return l
		}
		if first == "" {
			first = l
		}
	}
	if first != "" {
		return first
	}
	if err != nil {
		return "git failed: " + err.Error()
	}
	return "not a git repository"
}

// parseWorktreePorcelain is worktrees.rs parse_worktree_porcelain.
func parseWorktreePorcelain(text string) []WorktreeEntry {
	out := []WorktreeEntry{}
	var cur *WorktreeEntry
	flush := func() {
		if cur != nil {
			out = append(out, *cur)
			cur = nil
		}
	}
	sc := bufio.NewScanner(strings.NewReader(text))
	for sc.Scan() {
		line := strings.TrimRight(sc.Text(), "\r")
		if strings.TrimSpace(line) == "" {
			flush()
			continue
		}
		key, rest, _ := strings.Cut(line, " ")
		switch key {
		case "worktree":
			flush()
			cur = &WorktreeEntry{Path: rest}
		case "HEAD":
			if cur != nil {
				cur.Head = rest
			}
		case "branch":
			if cur != nil {
				b := strings.TrimPrefix(rest, "refs/heads/")
				cur.Branch = &b
			}
		case "detached":
			if cur != nil {
				cur.IsDetached = true
			}
		case "locked":
			if cur != nil {
				cur.IsLocked = true
			}
		case "prunable":
			if cur != nil {
				cur.IsPrunable = true
			}
		}
	}
	flush()
	if len(out) > 0 {
		out[0].IsMain = true
	}
	return out
}
