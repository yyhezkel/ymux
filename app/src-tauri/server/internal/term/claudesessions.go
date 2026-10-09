package term

// claudesessions.go — the resume picker's session list (Phase 117,
// WEB-DESIGN F3): GET /api/v2/claude/sessions?limit=&project_path=.
//
// A port of lib.rs pane_list_claude_sessions' local path
// (list_claude_sessions_local + peek_claude_jsonl + extract_text_field): the
// transcripts under ~/.claude/projects/*/*.jsonl, newest first, each peeked
// at its first and last 256 KB for the real cwd, the sidechain flag, the
// first user line and the last assistant line. A project_path scope is
// checked against the transcript's own "cwd" (the directory NAME is a lossy
// encoding) and applied BEFORE the limit, as on the desktop. Behind gate.
// Nothing here logs transcript content (Rule #1) — only counts.

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
}

const claudePeekBytes = 256 * 1024

// claudeProjectsRoot is swapped in tests.
var claudeProjectsRoot = func() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".claude", "projects")
}

func (s *Service) handleClaudeSessions(w http.ResponseWriter, r *http.Request) {
	limit := 30
	if n, err := strconv.Atoi(r.URL.Query().Get("limit")); err == nil && n > 0 {
		limit = n
	}
	if limit > 200 {
		limit = 200
	}
	scope := strings.TrimSpace(r.URL.Query().Get("project_path"))
	out := listClaudeSessions(claudeProjectsRoot(), limit, scope)
	logger.Debug("claude sessions listed", "count", len(out), "scoped", scope != "")
	writeJSON(w, http.StatusOK, out)
}

func listClaudeSessions(root string, limit int, scope string) []ClaudeSessionInfo {
	out := []ClaudeSessionInfo{}
	if root == "" {
		return out
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
	for _, e := range entries {
		if len(out) >= limit {
			break
		}
		// The scope is checked against the transcript's own cwd, before the
		// limit, so a scoped list is never short because newer sessions
		// belonged to other projects; a mismatch skips the tail read.
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
		out = append(out, ClaudeSessionInfo{
			SessionID:     strings.TrimSuffix(filepath.Base(e.path), ".jsonl"),
			ProjectPath:   project,
			JsonlPath:     e.path,
			MtimeUnix:     e.mtime,
			LastUser:      pk.firstUser,
			LastAssistant: pk.lastAssistant,
			IsSubagent:    pk.isSubagent,
		})
	}
	return out
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
	isSubagent                    bool
}

// peekClaudeJSONL reads the head (and, when the scope matches, the tail);
// ok is false when a scope was given and the transcript's cwd is not it.
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
		var v struct {
			Cwd         string `json:"cwd"`
			IsSidechain bool   `json:"isSidechain"`
			Type        string `json:"type"`
		}
		if json.Unmarshal([]byte(line), &v) != nil {
			continue
		}
		if out.cwd == "" {
			out.cwd = v.Cwd
		}
		if v.IsSidechain {
			out.isSubagent = true
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
	// Tail: the last whole "assistant" line; the first line after a seek
	// into the middle is partial and skipped.
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
		for i := len(lines) - 1; i >= 0; i-- {
			line := lines[i]
			if !strings.Contains(line, `"assistant"`) {
				continue
			}
			var v struct {
				Type string `json:"type"`
			}
			if json.Unmarshal([]byte(line), &v) != nil || v.Type != "assistant" {
				continue
			}
			if t := extractTextField(line); t != "" {
				out.lastAssistant = t
				break
			}
		}
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
