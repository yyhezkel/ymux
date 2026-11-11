package term

// claudesessions.go — the resume picker's session list (Phase 117,
// WEB-DESIGN F3): GET /api/v2/claude/sessions?limit=&offset=&project_path=
// &q=&branch=&model=.
//
// A port of lib.rs pane_list_claude_sessions' local path
// (list_claude_sessions_local + peek_claude_jsonl + extract_text_field): the
// transcripts under ~/.claude/projects/*/*.jsonl, newest first, each peeked
// at its first and last 256 KB for the real cwd, the sidechain flag, the
// first user line, the last assistant line, the git branch, the model and
// the title. A project_path scope is checked against the transcript's own
// "cwd" (the directory NAME is a lossy encoding) and applied BEFORE the
// limit, as on the desktop. Behind gate.
//
// Search (Orca idea #9): q is token-AND over title, cwd, branch, model and
// the two previews (search.go); branch and model are field filters. Paging
// applies to the filtered rows. A search peeks at most searchScanMax files
// within searchScanBudget, and stops when the page is full.
//
// Nothing here logs transcript content or the query (Rule #1) — only counts
// and timing.

import (
	"bufio"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

// ClaudeSessionInfo mirrors lib.rs ClaudeSessionInfo.
type ClaudeSessionInfo struct {
	SessionID     string `json:"session_id"`
	ProjectPath   string `json:"project_path"`
	JsonlPath     string `json:"jsonl_path"`
	MtimeUnix     int64  `json:"mtime_unix"`
	LastUser      string `json:"last_user,omitempty"`
	LastAssistant string `json:"last_assistant,omitempty"`
	IsSubagent    bool   `json:"is_subagent"`
	Title         string `json:"title,omitempty"`
	GitBranch     string `json:"git_branch,omitempty"`
	Model         string `json:"model,omitempty"`
}

const claudePeekBytes = 256 * 1024

// The search cost caps. Each peek reads at most 2 × claudePeekBytes, so one
// search reads at most searchScanMax × 512 KB.
const (
	searchScanMax    = 500
	searchScanBudget = 3 * time.Second
)

// claudeProjectsRoot is swapped in tests.
var claudeProjectsRoot = func() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".claude", "projects")
}

func (s *Service) handleClaudeSessions(w http.ResponseWriter, r *http.Request) {
	v := r.URL.Query()
	limit := 30
	if n, err := strconv.Atoi(v.Get("limit")); err == nil && n > 0 {
		limit = n
	}
	if limit > 200 {
		limit = 200
	}
	offset, _ := strconv.Atoi(v.Get("offset"))
	offset = max(offset, 0)
	scope := strings.TrimSpace(v.Get("project_path"))
	q := parseSessionQuery(v)
	start := time.Now()
	out, st := searchClaudeSessions(claudeProjectsRoot(), scope, q, offset, limit)
	if q.active() {
		logger.Info("claude sessions searched", "count", len(out), "peeked", st.peeked,
			"capped", st.capped, "ms", time.Since(start).Milliseconds())
	} else {
		logger.Debug("claude sessions listed", "count", len(out), "scoped", scope != "")
	}
	writeJSON(w, http.StatusOK, out)
}

// listClaudeSessions is the unfiltered first page.
func listClaudeSessions(root string, limit int, scope string) []ClaudeSessionInfo {
	out, _ := searchClaudeSessions(root, scope, sessionQuery{}, 0, limit)
	return out
}

type searchStats struct {
	peeked int
	capped bool // the scan stopped at searchScanMax or searchScanBudget
}

// searchClaudeSessions walks the transcripts newest first and returns rows
// offset..offset+limit of those that match scope and q.
func searchClaudeSessions(root, scope string, q sessionQuery, offset, limit int) ([]ClaudeSessionInfo, searchStats) {
	out := []ClaudeSessionInfo{}
	var st searchStats
	if root == "" {
		return out, st
	}
	type entry struct {
		path  string
		mtime int64
	}
	var entries []entry
	projs, _ := os.ReadDir(root)
	for _, p := range projs {
		if !p.IsDir() {
			continue
		}
		files, _ := os.ReadDir(filepath.Join(root, p.Name()))
		for _, f := range files {
			if f.IsDir() || filepath.Ext(f.Name()) != ".jsonl" {
				continue
			}
			var mt int64
			if info, err := f.Info(); err == nil {
				mt = info.ModTime().Unix()
			}
			entries = append(entries, entry{filepath.Join(root, p.Name(), f.Name()), mt})
		}
	}
	sort.SliceStable(entries, func(i, j int) bool { return entries[i].mtime > entries[j].mtime })
	searching := q.active()
	deadline := time.Now().Add(searchScanBudget)
	skipped := 0
	for _, e := range entries {
		if len(out) >= limit {
			break
		}
		// Without a scope or a query a row needs no peek to be counted, so
		// the rows before the offset cost nothing.
		if scope == "" && !searching && skipped < offset {
			skipped++
			continue
		}
		if searching && (st.peeked >= searchScanMax || time.Now().After(deadline)) {
			st.capped = true
			break
		}
		// The scope is checked against the transcript's own cwd, before the
		// limit, so a scoped list is never short because newer sessions
		// belonged to other projects; a mismatch skips the tail read.
		st.peeked++
		pk, ok := peekClaudeJSONL(e.path, scope)
		if !ok {
			continue
		}
		project := pk.cwd
		if project == "" {
			// Display-only fallback; the frontend never cds to a path that
			// does not start with "/".
			project = filepath.Base(filepath.Dir(e.path))
		}
		if searching && !q.match(pk.title, project, pk.gitBranch, pk.model, pk.firstUser, pk.lastAssistant) {
			continue
		}
		if skipped < offset {
			skipped++
			continue
		}
		out = append(out, ClaudeSessionInfo{
			SessionID:     strings.TrimSuffix(filepath.Base(e.path), ".jsonl"),
			ProjectPath:   project,
			JsonlPath:     e.path,
			MtimeUnix:     e.mtime,
			LastUser:      pk.firstUser,
			LastAssistant: pk.lastAssistant,
			IsSubagent:    pk.isSubagent,
			Title:         pk.title,
			GitBranch:     pk.gitBranch,
			Model:         pk.model,
		})
	}
	return out, st
}

// normPath is lib.rs norm_path for POSIX paths: trimmed, no trailing "/".
func normPath(p string) string {
	p = strings.TrimSpace(strings.ReplaceAll(p, `\`, "/"))
	if t := strings.TrimRight(p, "/"); t != "" {
		return t
	}
	return p
}

type claudePeek struct {
	cwd, firstUser, lastAssistant string
	title, gitBranch, model       string
	isSubagent                    bool
}

// peekLine is the subset of one transcript line the peek reads.
type peekLine struct {
	Cwd         string `json:"cwd"`
	IsSidechain bool   `json:"isSidechain"`
	Type        string `json:"type"`
	GitBranch   string `json:"gitBranch"`
	Summary     string `json:"summary"`
	Title       string `json:"title"`
	CustomTitle string `json:"customTitle"`
	Message     struct {
		Model string `json:"model"`
	} `json:"message"`
}

// lineTitle is the session title a line carries: Claude Code's "summary"
// line, or an "ai-title" / "custom-title" line. "" for any other line.
func (v peekLine) lineTitle() string {
	switch v.Type {
	case "summary", "ai-title", "custom-title":
	default:
		return ""
	}
	for _, t := range []string{v.CustomTitle, v.Title, v.Summary} {
		if t = strings.TrimSpace(t); t != "" {
			return clipRunes(t, 120)
		}
	}
	return ""
}

// lineModel is the assistant line's model; Claude Code's "<synthetic>"
// placeholder is not a model.
func (v peekLine) lineModel() string {
	if v.Message.Model == "<synthetic>" {
		return ""
	}
	return v.Message.Model
}

// mayCarryTitle is a cheap pre-check before a line is unmarshalled. Claude
// Code writes compact JSON, so the type key has no spaces.
func mayCarryTitle(line string) bool {
	return strings.Contains(line, `"type":"summary"`) || strings.Contains(line, `"type":"ai-title"`) ||
		strings.Contains(line, `"type":"custom-title"`)
}

func clipRunes(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	return string([]rune(s)[:n]) + "…"
}

// peekClaudeJSONL reads the head (and, when the scope matches, the tail);
// ok is false when a scope was given and the transcript's cwd is not it.
// The title is the latest title line; the branch and the model are the
// latest the tail shows, else the head's.
func peekClaudeJSONL(path, scope string) (claudePeek, bool) {
	var out claudePeek
	f, err := os.Open(path)
	if err != nil {
		return out, scope == ""
	}
	defer f.Close()
	var size int64
	if st, err := f.Stat(); err == nil {
		size = st.Size()
	}
	head, _ := io.ReadAll(io.LimitReader(f, claudePeekBytes))
	sc := bufio.NewScanner(strings.NewReader(string(head)))
	sc.Buffer(make([]byte, 0, 64*1024), claudePeekBytes+1)
	for sc.Scan() {
		line := sc.Text()
		var v peekLine
		if json.Unmarshal([]byte(line), &v) != nil {
			continue
		}
		if out.cwd == "" {
			out.cwd = v.Cwd
		}
		if out.gitBranch == "" {
			out.gitBranch = v.GitBranch
		}
		if v.IsSidechain {
			out.isSubagent = true
		}
		if t := v.lineTitle(); t != "" {
			out.title = t
		}
		if out.firstUser == "" && v.Type == "user" {
			out.firstUser = extractTextField(line)
		}
		if out.cwd != "" && out.firstUser != "" {
			break
		}
	}
	if scope != "" && (out.cwd == "" || normPath(out.cwd) != normPath(scope)) {
		return out, false
	}
	// Tail: the last whole "assistant" line and the last title line; the
	// first line after a seek into the middle is partial and skipped.
	start := size - claudePeekBytes
	if start < 0 {
		start = 0
	}
	if _, err := f.Seek(start, io.SeekStart); err == nil {
		tail, _ := io.ReadAll(f)
		lines := strings.Split(string(tail), "\n")
		if start > 0 && len(lines) > 0 {
			lines = lines[1:]
		}
		tailTitle, tailBranch, tailModel := "", "", ""
		for i := len(lines) - 1; i >= 0; i-- {
			line := lines[i]
			// The model comes from the assistant lines read anyway, up to the
			// one with text, so no extra line is unmarshalled for it.
			needAssistant := out.lastAssistant == ""
			if tailTitle == "" && mayCarryTitle(line) {
				var v peekLine
				if json.Unmarshal([]byte(line), &v) == nil {
					tailTitle = v.lineTitle()
				}
				continue
			}
			if !needAssistant {
				if tailTitle != "" {
					break
				}
				continue
			}
			if !strings.Contains(line, `"assistant"`) {
				continue
			}
			var v peekLine
			if json.Unmarshal([]byte(line), &v) != nil || v.Type != "assistant" {
				continue
			}
			if tailBranch == "" {
				tailBranch = v.GitBranch
			}
			if tailModel == "" {
				tailModel = v.lineModel()
			}
			if out.lastAssistant == "" {
				out.lastAssistant = extractTextField(line)
			}
		}
		if tailTitle != "" {
			out.title = tailTitle
		}
		if tailBranch != "" {
			out.gitBranch = tailBranch
		}
		out.model = tailModel
	}
	return out, true
}

// extractTextField is lib.rs extract_text_field: the first "text":"…" (else
// "content":"…") in a JSONL line, escapes decoded, trimmed to 80 chars + "…".
func extractTextField(fragment string) string {
	one := func(key string) (string, bool) {
		needle := `"` + key + `":"`
		i := strings.Index(fragment, needle)
		if i < 0 {
			return "", false
		}
		rest := fragment[i+len(needle):]
		var b strings.Builder
		esc := false
		for _, c := range rest {
			if esc {
				esc = false
				switch c {
				case '"', '\\', '/':
					b.WriteRune(c)
				case 'n', 't':
					b.WriteByte(' ')
				case 'r':
				default:
					b.WriteRune(c)
				}
			} else if c == '\\' {
				esc = true
			} else if c == '"' {
				break
			} else {
				b.WriteRune(c)
			}
			if b.Len() > 600 {
				break
			}
		}
		return b.String(), true
	}
	s, ok := one("text")
	if !ok {
		s, _ = one("content")
	}
	s = strings.TrimSpace(s)
	if utf8.RuneCountInString(s) <= 80 {
		return s
	}
	return string([]rune(s)[:80]) + "…"
}
