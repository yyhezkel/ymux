package term

// history.go — session history for the browser (Phase 104, WEB-DESIGN §4.2,
// B6): an ended tmux session that ran Claude stays listable, its transcript
// readable, and the conversation resumable.
//
// The CLI (cli/src/session_meta.rs) now stamps `ended_at` on a gone session
// instead of deleting its row, and records `cwd`. This file only READS that
// file — it stays the CLI's to write — plus Claude Code's own transcripts
// under ~/.claude/projects.
//
//	GET  /api/v2/term/history                     ended rows, newest first;
//	                                              ?q=&branch=&model= searches,
//	                                              ?offset=&limit= pages
//	GET  /api/v2/claude/sessions/{id}/transcript  the turns, paged
//	POST /api/v2/term/history/{name}/resume       a new tmux session running
//	                                              `claude --resume <id>`
//
// Rule #1: a transcript is the user's conversation. It is rendered to the
// user's own client and never logged — the handler logs the session id, the
// byte count and the turn count only.

import (
	"bufio"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
)

// HistoryRow is one ended session.
type HistoryRow struct {
	Name            string `json:"name"`
	Display         string `json:"display"`
	ClaudeSessionID string `json:"claude_session_id"`
	AutoName        string `json:"auto_name,omitempty"`
	ClaudeTitle     string `json:"claude_title,omitempty"`
	Label           string `json:"label,omitempty"`
	Cwd             string `json:"cwd,omitempty"`
	EndedAt         string `json:"ended_at"`
	// Set only on a search (q, branch or model given): read from the
	// transcript, as the resume picker reads them.
	GitBranch string `json:"git_branch,omitempty"`
	Model     string `json:"model,omitempty"`
	Preview   string `json:"preview,omitempty"`
}

// searchHistory keeps the rows that match q, then returns the page
// offset..offset+limit (limit <= 0: to the end). Without a query it reads no
// transcript. With one it peeks each row's transcript once; the CLI keeps at
// most 100 ended rows (HISTORY_MAX_ENDED), and the peek is bounded, so the
// scan is bounded too. It stops when the page is full.
func searchHistory(home string, rows []HistoryRow, q sessionQuery, offset, limit int) ([]HistoryRow, int) {
	offset = max(offset, 0)
	out := []HistoryRow{}
	peeked, skipped := 0, 0
	for _, row := range rows {
		if limit > 0 && len(out) >= limit {
			break
		}
		if q.active() {
			var pk claudePeek
			if p, ok := transcriptPath(home, row.ClaudeSessionID); ok {
				peeked++
				pk, _ = peekClaudeJSONL(p, "")
			}
			title := strings.Join([]string{row.Display, row.ClaudeTitle, row.Label, row.AutoName, pk.title}, "\n")
			if !q.match(title, row.Cwd, pk.gitBranch, pk.model, pk.firstUser, pk.lastAssistant) {
				continue
			}
			row.GitBranch, row.Model, row.Preview = pk.gitBranch, pk.model, pk.firstUser
		}
		if skipped < offset {
			skipped++
			continue
		}
		out = append(out, row)
	}
	return out, peeked
}

// History lists the meta rows that ended and still name a Claude session,
// newest first. A row whose tmux name is live again is not history, even
// if the CLI has not re-pruned yet.
func History(meta map[string]MetaEntry, live []Session) []HistoryRow {
	alive := map[string]bool{}
	for _, s := range live {
		alive[s.Name] = true
	}
	out := []HistoryRow{}
	for name, e := range meta {
		if e.EndedAt == "" || e.ClaudeSessionID == "" || alive[name] {
			continue
		}
		out = append(out, HistoryRow{Name: name, Display: DisplayName(name, e), ClaudeSessionID: e.ClaudeSessionID,
			AutoName: e.AutoName, ClaudeTitle: e.ClaudeTitle, Label: e.Label, Cwd: e.Cwd, EndedAt: e.EndedAt})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].EndedAt > out[j].EndedAt })
	return out
}

// handleHistory serves GET /api/v2/term/history?q=&branch=&model=&offset=
// &limit=. Without any of them the reply is every row, as before.
func (s *Service) handleHistory(w http.ResponseWriter, r *http.Request) {
	live, err := s.tmux.List()
	if err != nil {
		failErr(w, err)
		return
	}
	rows := History(LoadMeta(s.home), live)
	v := r.URL.Query()
	q := parseSessionQuery(v)
	offset, _ := strconv.Atoi(v.Get("offset"))
	limit, _ := strconv.Atoi(v.Get("limit"))
	if !q.active() && offset <= 0 && limit <= 0 {
		logger.Info("session history listed", "rows", len(rows))
		writeJSON(w, http.StatusOK, rows)
		return
	}
	start := time.Now()
	page, peeked := searchHistory(s.home, rows, q, offset, limit)
	// Rule #1: the query is user text. Counts and timing only.
	logger.Info("session history searched", "rows", len(rows), "matched", len(page), "peeked", peeked,
		"ms", time.Since(start).Milliseconds())
	writeJSON(w, http.StatusOK, page)
}

// claudeSessionID is what Claude Code names a session: a UUID. Checked
// before it is ever joined into a path or an argv — it arrives from a URL
// and from a file other processes write.
var claudeSessionID = regexp.MustCompile(`^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`)

// transcriptPath finds <id>.jsonl under any project directory. The project
// directory is Claude's encoding of the cwd, so a lookup by id alone does not
// need the cwd (and survives a row that has none).
func transcriptPath(home, id string) (string, bool) {
	if !claudeSessionID.MatchString(id) {
		return "", false
	}
	matches, _ := filepath.Glob(filepath.Join(home, ".claude", "projects", "*", id+".jsonl"))
	if len(matches) == 0 {
		return "", false
	}
	return matches[0], true
}

// Turn is one rendered step of a conversation.
type Turn struct {
	Role string `json:"role"` // "user" | "assistant" | "tool"
	Text string `json:"text,omitempty"`
	Tool string `json:"tool,omitempty"` // role "tool": the tool's name
	Ts   string `json:"ts,omitempty"`
}

// transcriptLine is the subset of a Claude Code JSONL line read here.
type transcriptLine struct {
	Type        string `json:"type"`
	IsMeta      bool   `json:"isMeta"`
	IsSidechain bool   `json:"isSidechain"`
	Timestamp   string `json:"timestamp"`
	Message     struct {
		Content json.RawMessage `json:"content"`
	} `json:"message"`
}

type contentBlock struct {
	Type string `json:"type"`
	Text string `json:"text"`
	Name string `json:"name"`
}

// parseTranscript turns a JSONL transcript into turns: the user's prompts,
// Claude's text, and a one-line marker per tool call. Tool results, thinking,
// sub-agent (sidechain) lines and Claude Code's internal meta lines are left
// out — this is a reader, not a debugger. Unparsable lines are skipped.
func parseTranscript(f *os.File) ([]Turn, int64, error) {
	var turns []Turn
	var bytes int64
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16<<20) // one line can carry a large paste
	for sc.Scan() {
		line := sc.Bytes()
		bytes += int64(len(line)) + 1
		var l transcriptLine
		if json.Unmarshal(line, &l) != nil || l.IsMeta || l.IsSidechain {
			continue
		}
		switch l.Type {
		case "user":
			var text string
			if json.Unmarshal(l.Message.Content, &text) == nil {
				if t := strings.TrimSpace(text); t != "" {
					turns = append(turns, Turn{Role: "user", Text: t, Ts: l.Timestamp})
				}
			}
			// A list here is tool_result blocks — the tool's output, skipped.
		case "assistant":
			var blocks []contentBlock
			if json.Unmarshal(l.Message.Content, &blocks) != nil {
				continue
			}
			for _, b := range blocks {
				switch b.Type {
				case "text":
					if t := strings.TrimSpace(b.Text); t != "" {
						turns = append(turns, Turn{Role: "assistant", Text: t, Ts: l.Timestamp})
					}
				case "tool_use":
					turns = append(turns, Turn{Role: "tool", Tool: b.Name, Ts: l.Timestamp})
				}
			}
		}
	}
	return turns, bytes, sc.Err()
}

const (
	transcriptPageDefault = 200
	transcriptPageMax     = 1000
)

func (s *Service) handleTranscript(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	path, ok := transcriptPath(s.home, id)
	if !ok {
		http.Error(w, "no transcript for that session", http.StatusNotFound)
		return
	}
	f, err := os.Open(path)
	if err != nil {
		http.Error(w, "transcript unreadable", http.StatusInternalServerError)
		return
	}
	defer f.Close()
	turns, n, err := parseTranscript(f)
	if err != nil {
		logger.Warn("transcript read stopped early", "session", id, "err", err)
	}
	q := r.URL.Query()
	offset, _ := strconv.Atoi(q.Get("offset"))
	limit, _ := strconv.Atoi(q.Get("limit"))
	if limit <= 0 {
		limit = transcriptPageDefault
	}
	limit = min(limit, transcriptPageMax)
	offset = min(max(offset, 0), len(turns))
	page := turns[offset:min(offset+limit, len(turns))]
	if page == nil {
		page = []Turn{}
	}
	logger.Info("transcript served", "session", id, "bytes", n, "turns", len(turns), "offset", offset)
	writeJSON(w, http.StatusOK, map[string]any{"session_id": id, "total": len(turns), "offset": offset, "turns": page})
}

var errNotHistory = errors.New("not an ended session with a Claude conversation")

// handleResume starts `claude --resume <id>` in a new tmux session, in the
// row's cwd, as a normal browser session (hooks, policy, workspace). The
// row's own tmux name is reused when it is free, so the row flips back to
// live on the CLI's next prune; otherwise a fresh name is minted. When
// claude exits the session ends, and the row becomes history again.
func (s *Service) handleResume(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	var body struct {
		WorkspaceID string `json:"workspace_id"`
		Policy      string `json:"policy"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.Policy == "" {
		body.Policy = policyNone
	}
	if !validPolicy(body.Policy) {
		http.Error(w, `policy must be "none" or "gate"`, http.StatusBadRequest)
		return
	}
	if body.WorkspaceID != "" && (s.hooks == nil || !s.hooks.webws.exists(body.WorkspaceID)) {
		http.Error(w, "no such workspace", http.StatusBadRequest)
		return
	}
	e, ok := LoadMeta(s.home)[name]
	if !ok || e.EndedAt == "" || !claudeSessionID.MatchString(e.ClaudeSessionID) {
		http.Error(w, errNotHistory.Error(), http.StatusNotFound)
		return
	}
	newName := name
	if !ValidName(newName) || s.tmux.Has(newName) {
		newName = "" // spawnSession mints one
	}
	cwd := e.Cwd
	if st, err := os.Stat(cwd); cwd == "" || err != nil || !st.IsDir() {
		cwd = "" // tmux falls back to its default; claude --resume still finds it by id
	}
	created, hooks, err := s.spawnSession(newName, cwd, body.Policy, body.WorkspaceID, "",
		s.claudeBinary(), "--resume", e.ClaudeSessionID)
	if err != nil {
		failErr(w, err)
		return
	}
	logger.Info("session resumed", "from", name, "session", created.name, "claude_session", e.ClaudeSessionID)
	resp := map[string]any{"name": created.name, "resumed_from": name, "hooks": hooks,
		"claude_session_id": e.ClaudeSessionID}
	if hooks {
		resp["pane_id"], resp["policy"] = created.paneID, created.policy
	}
	writeJSON(w, http.StatusCreated, resp)
}

// claudeBinary is claude's absolute path when the daemon can resolve it
// (config.AugmentUserPath merged the user's login PATH at start), so the
// session does not depend on the tmux server's PATH. Overridable in tests.
func (s *Service) claudeBinary() string {
	if s.claudeBin != "" {
		return s.claudeBin
	}
	if p, err := exec.LookPath("claude"); err == nil {
		return p
	}
	return "claude"
}
