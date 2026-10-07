package term

// gitdiff.go — the Diff pane and worktree creation for a browser (Phase 118,
// WEB-DESIGN F4). Ports of the desktop's local paths:
//
//   POST /api/v2/git/diff {cwd, source}   diff_pane.rs fetch_bundle_local —
//        one snapshot: toplevel → status (porcelain v1 -z, branch, all
//        untracked) → diff against the source → up to 40 untracked files as
//        --no-index diffs (256 KB each) → a 2 MB overall cap. Always 200; a
//        git failure is the bundle's `error`, as in the desktop event.
//   POST /api/v2/git/worktree-add {cwd, branch, base, target}
//        worktrees.rs workspace_create_project_worktree — `git worktree add
//        -- <target> -b <branch> <base>`, then the fresh list.
//
// The desktop runs a poller per pane and emits `diff-pane-updated`; here the
// browser polls this endpoint and emits the event itself (web.ts), so the
// daemon keeps no per-pane state. Every git runs as an argv (Rule #3); a ref,
// a base and a target are refused when they would read as an option. Behind
// gate — the caller can already run a shell here.

import (
	"context"
	"encoding/json"
	"net/http"
	"os/exec"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	diffMaxUntracked      = 40
	diffUntrackedMaxBytes = 262144
	diffMaxBytes          = 2_000_000
	gitTimeout            = 20 * time.Second
)

// gitG is diff_pane.rs's fixed -c block: stable, uncoloured, a/ b/ prefixes.
var gitG = []string{"-c", "core.quotepath=false", "-c", "color.ui=never", "-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false"}

// DiffSource mirrors ymux-types DiffSource (tag "kind").
type DiffSource struct {
	Kind   string `json:"kind"` // working | head | ref
	GitRef string `json:"git_ref,omitempty"`
}

// StatusEntry mirrors diff_pane.rs StatusEntry.
type StatusEntry struct {
	XY       string  `json:"xy"`
	Path     string  `json:"path"`
	OrigPath *string `json:"orig_path"`
}

// DiffBundle is the desktop's diff-pane-updated payload minus pane_id.
type DiffBundle struct {
	DiffText  string        `json:"diff_text"`
	Files     []StatusEntry `json:"files"`
	Error     *string       `json:"error"`
	Cwd       string        `json:"cwd"`
	Branch    *string       `json:"branch"`
	Truncated bool          `json:"truncated"`
}

type gitResult struct {
	out, errOut string
	code        int
	spawnErr    error
}

// runGit is swapped in tests: git -C dir --no-pager args….
var runGit = func(ctx context.Context, dir string, args ...string) gitResult {
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", dir, "--no-pager"}, args...)...)
	cmd.Env = append(cmd.Environ(), "GIT_TERMINAL_PROMPT=0", "LC_ALL=C")
	var stdout, stderr strings.Builder
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	err := cmd.Run()
	r := gitResult{out: stdout.String(), errOut: stderr.String()}
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			r.code = ee.ExitCode()
		} else {
			r.spawnErr, r.code = err, -1
		}
	}
	return r
}

func g(args ...string) []string { return append(append([]string{}, gitG...), args...) }

// optionLike refuses what git would read as a flag, or a control character.
func optionLike(s string) bool {
	if strings.HasPrefix(s, "-") {
		return true
	}
	for _, r := range s {
		if r < 0x20 || r == 0x7f {
			return true
		}
	}
	return false
}

func diffBundle(ctx context.Context, cwd string, src DiffSource) DiffBundle {
	fail := func(msg string) DiffBundle {
		return DiffBundle{Files: []StatusEntry{}, Error: &msg, Cwd: cwd}
	}
	cwd = strings.TrimSpace(cwd)
	if cwd == "" {
		return fail("this workspace has no project directory")
	}
	if dir, ok := expandHome(cwd); ok {
		cwd = dir
	} else {
		return fail("the project directory must be an absolute path")
	}
	var rev []string
	switch src.Kind {
	case "", "working":
	case "head":
		rev = []string{"HEAD"}
	case "ref":
		ref := strings.TrimSpace(src.GitRef)
		if ref == "" || optionLike(ref) {
			return fail("invalid git ref")
		}
		rev = []string{ref}
	default:
		return fail("unknown diff source")
	}
	r := runGit(ctx, cwd, g("rev-parse", "--show-toplevel")...)
	if r.code != 0 {
		return fail(gitErrorOf(r))
	}
	top := strings.TrimSpace(r.out)
	if top == "" {
		return fail("not a git repository")
	}
	r = runGit(ctx, top, g("status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all")...)
	if r.code != 0 {
		return fail(gitErrorOf(r))
	}
	files, branch := parseStatusZ(r.out)
	args := g("diff", "--no-color", "--no-ext-diff")
	args = append(append(args, rev...), "--")
	r = runGit(ctx, top, args...)
	if r.code != 0 && strings.TrimSpace(r.errOut) != "" {
		return fail(gitErrorOf(gitResult{errOut: r.errOut}))
	}
	var b strings.Builder
	b.WriteString(r.out)
	n := 0
	for _, f := range files {
		if f.XY != "??" {
			continue
		}
		if n >= diffMaxUntracked {
			break
		}
		n++
		u := runGit(ctx, top, g("diff", "--no-color", "--no-ext-diff", "--no-index", "--", "/dev/null", f.Path)...)
		if u.spawnErr != nil {
			continue
		}
		b.WriteString(clipBytes(u.out, diffUntrackedMaxBytes))
	}
	text, truncated := b.String(), false
	if len(text) > diffMaxBytes {
		text, truncated = clipBytes(text, diffMaxBytes)+"\n… ymux: output truncated\n", true
	}
	return DiffBundle{DiffText: text, Files: files, Cwd: cwd, Branch: branch, Truncated: truncated}
}

// clipBytes cuts at max bytes, backed off to a rune boundary (the Rust
// String::truncate would panic mid-codepoint).
func clipBytes(s string, max int) string {
	if len(s) <= max {
		return s
	}
	cut := max
	for cut > 0 && !utf8.RuneStart(s[cut]) {
		cut--
	}
	return s[:cut]
}

// gitErrorOf is worktrees.rs git_error over stderr, else stdout.
func gitErrorOf(r gitResult) string {
	src := r.errOut
	if strings.TrimSpace(src) == "" {
		src = r.out
	}
	for _, l := range strings.Split(src, "\n") {
		if l = strings.TrimSpace(l); l != "" {
			return "git: " + l
		}
	}
	if r.spawnErr != nil {
		return "git: " + r.spawnErr.Error()
	}
	return "git: git failed"
}

// parseStatusZ is diff_pane.rs parse_status_z.
func parseStatusZ(out string) ([]StatusEntry, *string) {
	recs := strings.FieldsFunc(out, func(r rune) bool { return r == 0 || r == '\n' })
	files := []StatusEntry{}
	var branch *string
	for i := 0; i < len(recs); i++ {
		rec := recs[i]
		if strings.HasPrefix(rec, "## ") {
			branch = parseBranchHeader(rec[3:])
			continue
		}
		if len(rec) < 4 {
			continue
		}
		e := StatusEntry{XY: rec[:2], Path: rec[3:]}
		if (e.XY[0] == 'R' || e.XY[0] == 'C') && i+1 < len(recs) {
			orig := recs[i+1]
			e.OrigPath = &orig
			i++
		}
		files = append(files, e)
	}
	return files, branch
}

// parseBranchHeader is diff_pane.rs parse_branch_header.
func parseBranchHeader(h string) *string {
	if strings.HasPrefix(h, "HEAD (no branch)") {
		return nil
	}
	if rest, ok := strings.CutPrefix(h, "No commits yet on "); ok {
		if rest = strings.TrimSpace(rest); rest != "" {
			return &rest
		}
		return nil
	}
	if i := strings.Index(h, "..."); i >= 0 {
		h = h[:i]
	} else if i := strings.Index(h, " ["); i >= 0 {
		h = h[:i]
	}
	if h = strings.TrimSpace(h); h == "" {
		return nil
	}
	return &h
}

func (s *Service) handleGitDiff(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Cwd    string     `json:"cwd"`
		Source DiffSource `json:"source"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 8<<10)).Decode(&in); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), gitTimeout)
	defer cancel()
	writeJSON(w, http.StatusOK, diffBundle(ctx, in.Cwd, in.Source))
}

// sanitizeBranch is worktrees.rs sanitize_branch_name (for the directory).
func sanitizeBranch(b string) (string, bool) {
	out := strings.Map(func(r rune) rune {
		if r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || strings.ContainsRune("-_./", r) {
			return r
		}
		return '-'
	}, b)
	if out == "" || strings.HasPrefix(out, "-") || strings.Contains(out, "..") {
		return "", false
	}
	return out, true
}

// defaultWorktreeTarget is worktrees.rs default_worktree_target:
// <parent>/<base>-<safe with / → ->.
func defaultWorktreeTarget(cwd, safe string) string {
	c := strings.TrimRight(strings.ReplaceAll(cwd, `\`, "/"), "/")
	suffix := strings.ReplaceAll(safe, "/", "-")
	i := strings.LastIndex(c, "/")
	if i < 0 {
		return c + "-" + suffix
	}
	return c[:i] + "/" + c[i+1:] + "-" + suffix
}

func (s *Service) handleGitWorktreeAdd(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Cwd    string `json:"cwd"`
		Branch string `json:"branch"`
		Base   string `json:"base"`
		Target string `json:"target"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 8<<10)).Decode(&in); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	branch := strings.TrimSpace(in.Branch)
	safe, ok := sanitizeBranch(branch)
	if !ok || optionLike(branch) {
		http.Error(w, "invalid branch name", http.StatusBadRequest)
		return
	}
	base := strings.TrimSpace(in.Base)
	if base == "" {
		http.Error(w, "base branch is required", http.StatusBadRequest)
		return
	}
	if optionLike(base) {
		http.Error(w, "invalid base branch", http.StatusBadRequest)
		return
	}
	cwd, ok := expandHome(in.Cwd)
	if strings.TrimSpace(in.Cwd) == "" || !ok {
		http.Error(w, "this workspace has no project directory", http.StatusBadRequest)
		return
	}
	target := strings.TrimSpace(in.Target)
	if target == "" {
		target = defaultWorktreeTarget(cwd, safe)
	}
	if t, ok := expandHome(target); ok {
		target = t
	} else {
		http.Error(w, "the worktree path must be absolute", http.StatusBadRequest)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 2*gitTimeout)
	defer cancel()
	if res := runGit(ctx, cwd, "rev-parse", "--is-inside-work-tree"); res.code != 0 {
		http.Error(w, gitErrorOf(res), http.StatusUnprocessableEntity)
		return
	}
	// `-b <branch>` before the positional pair, `--` so neither reads as a flag.
	if res := runGit(ctx, cwd, "worktree", "add", "-b", branch, "--", target, base); res.code != 0 {
		http.Error(w, gitErrorOf(res), http.StatusUnprocessableEntity)
		return
	}
	logger.Info("git worktree added", "branch_len", len(branch))
	res := runGit(ctx, cwd, "worktree", "list", "--porcelain")
	writeJSON(w, http.StatusOK, parseWorktreePorcelain(res.out))
}
