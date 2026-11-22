package term

// Grok Build sessions live in ~/.grok/sessions/<group>/<id>/, not in
// Claude's jsonl tree. summary.json is the list row. updates.jsonl is
// the transcript. chat_history.jsonl is model context and is not read.

import (
	"bufio"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

func safeGrokID(id string) bool {
	if id == "" || len(id) > 80 {
		return false
	}
	for i := 0; i < len(id); i++ {
		c := id[i]
		ok := (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-'
		if !ok || (i == 0 && (c == '_' || c == '-')) {
			return false
		}
	}
	return true
}

// grokSessionsRoot is where the list is read. Tests point it at a temp dir.
var grokSessionsRoot = func() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".grok", "sessions")
}

func grokUpdatesPath(home, id string) (string, bool) {
	if !safeGrokID(id) {
		return "", false
	}
	matches, _ := filepath.Glob(filepath.Join(home, ".grok", "sessions", "*", id, "updates.jsonl"))
	if len(matches) == 0 {
		return "", false
	}
	return matches[0], true
}

func (s *Service) handleGrokSessions(w http.ResponseWriter, r *http.Request) {
	v := r.URL.Query()
	limit := 30
	if n, err := strconv.Atoi(v.Get("limit")); err == nil && n > 0 {
		limit = n
	}
	if limit > 200 {
		limit = 200
	}
	scope := strings.TrimSpace(v.Get("project_path"))
	q := strings.TrimSpace(v.Get("q"))
	rows := listGrokSessions(grokSessionsRoot(), scope)
	if q != "" {
		filtered := rows[:0]
		for _, row := range rows {
			if matchTokens(q, row.Title, row.ProjectPath, row.SessionID, row.LastUser, row.LastAssistant) {
				filtered = append(filtered, row)
			}
		}
		rows = filtered
	}
	if len(rows) > limit {
		rows = rows[:limit]
	}
	if rows == nil {
		rows = []ClaudeSessionInfo{}
	}
	logger.Debug("grok sessions listed", "count", len(rows), "scoped", scope != "")
	writeJSON(w, http.StatusOK, rows)
}

func listGrokSessions(root, scope string) []ClaudeSessionInfo {
	var out []ClaudeSessionInfo
	if root == "" {
		return out
	}
	groups, err := os.ReadDir(root)
	if err != nil {
		return out
	}
	for _, group := range groups {
		if !group.IsDir() {
			continue
		}
		gpath := filepath.Join(root, group.Name())
		sessions, err := os.ReadDir(gpath)
		if err != nil {
			continue
		}
		for _, session := range sessions {
			if !session.IsDir() || !safeGrokID(session.Name()) {
				continue
			}
			dir := filepath.Join(gpath, session.Name())
			updates := filepath.Join(dir, "updates.jsonl")
			info, err := os.Stat(updates)
			if err != nil || !info.Mode().IsRegular() {
				continue
			}
			summary := readGrokSummary(filepath.Join(dir, "summary.json"))
			if summary.Hidden {
				continue
			}
			if scope != "" && summary.Cwd != scope {
				continue
			}
			row := ClaudeSessionInfo{
				SessionID:     session.Name(),
				ProjectPath:   summary.Cwd,
				JsonlPath:     updates,
				MtimeUnix:     summary.mtime,
				Title:         summary.Title,
				IsSubagent:    summary.subagent,
				LastUser:      grokPreview(updates, true, "user_message_chunk"),
				LastAssistant: grokPreview(updates, false, "agent_message_chunk"),
			}
			if row.MtimeUnix == 0 {
				row.MtimeUnix = info.ModTime().Unix()
			}
			out = append(out, row)
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].MtimeUnix != out[j].MtimeUnix {
			return out[i].MtimeUnix > out[j].MtimeUnix
		}
		return out[i].SessionID < out[j].SessionID
	})
	return out
}

type grokSummary struct {
	Title    string
	Cwd      string
	Hidden   bool
	mtime    int64
	subagent bool
}

func readGrokSummary(path string) grokSummary {
	var raw struct {
		Title        string `json:"title"`
		Hidden       bool   `json:"hidden"`
		SessionKind  string `json:"session_kind"`
		UpdatedAt    string `json:"updated_at"`
		LastActiveAt string `json:"last_active_at"`
		CreatedAt    string `json:"created_at"`
		Info         struct {
			Cwd string `json:"cwd"`
		} `json:"info"`
	}
	b, err := os.ReadFile(path)
	if err != nil || json.Unmarshal(b, &raw) != nil {
		return grokSummary{}
	}
	var mtime int64
	for _, stamp := range []string{raw.UpdatedAt, raw.LastActiveAt, raw.CreatedAt} {
		if stamp == "" {
			continue
		}
		if t, err := time.Parse(time.RFC3339, stamp); err == nil {
			mtime = t.Unix()
			break
		}
	}
	kind := strings.ToLower(raw.SessionKind)
	return grokSummary{
		Title:    strings.TrimSpace(raw.Title),
		Cwd:      strings.TrimSpace(raw.Info.Cwd),
		Hidden:   raw.Hidden,
		mtime:    mtime,
		subagent: strings.Contains(kind, "subagent") || strings.Contains(kind, "sub-agent"),
	}
}

func grokPreview(path string, head bool, kind string) string {
	f, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer f.Close()
	const edge = 8 * 1024
	buf := make([]byte, edge)
	if head {
		n, _ := f.Read(buf)
		buf = buf[:n]
	} else {
		st, err := f.Stat()
		if err != nil {
			return ""
		}
		start := st.Size() - edge
		if start < 0 {
			start = 0
		}
		if _, err := f.Seek(start, 0); err != nil {
			return ""
		}
		n, _ := f.Read(buf)
		buf = buf[:n]
	}
	lines := strings.Split(string(buf), "\n")
	if !head {
		for i, j := 0, len(lines)-1; i < j; i, j = i+1, j-1 {
			lines[i], lines[j] = lines[j], lines[i]
		}
	}
	for _, line := range lines {
		if text := grokChunkText(line, kind); text != "" {
			return clipPreview(text)
		}
	}
	return ""
}

func grokChunkText(line, kind string) string {
	var ev struct {
		Params struct {
			Update struct {
				SessionUpdate string          `json:"sessionUpdate"`
				Content       json.RawMessage `json:"content"`
			} `json:"update"`
		} `json:"params"`
	}
	if json.Unmarshal([]byte(line), &ev) != nil || ev.Params.Update.SessionUpdate != kind {
		return ""
	}
	return strings.TrimSpace(grokContentText(ev.Params.Update.Content))
}

func grokContentText(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var s string
	if json.Unmarshal(raw, &s) == nil {
		return s
	}
	var obj struct {
		Text    string          `json:"text"`
		Content json.RawMessage `json:"content"`
	}
	if json.Unmarshal(raw, &obj) == nil && (obj.Text != "" || len(obj.Content) > 0) {
		if obj.Text != "" {
			return obj.Text
		}
		return grokContentText(obj.Content)
	}
	var arr []json.RawMessage
	if json.Unmarshal(raw, &arr) == nil {
		var b strings.Builder
		for _, item := range arr {
			b.WriteString(grokContentText(item))
		}
		return b.String()
	}
	return ""
}

func clipPreview(s string) string {
	fields := strings.Fields(s)
	flat := strings.Join(fields, " ")
	r := []rune(flat)
	if len(r) > 120 {
		return string(r[:120])
	}
	return flat
}

// parseGrokTranscript turns updates.jsonl into the same turns the Claude
// reader produces: user text, assistant text, and a one-line tool marker.
// Consecutive chunks of the same role are one turn. Tool results and
// thinking are left out.
func parseGrokTranscript(f *os.File) ([]Turn, int64, error) {
	var turns []Turn
	var bytes int64
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16<<20)
	for sc.Scan() {
		line := sc.Bytes()
		bytes += int64(len(line)) + 1
		var ev struct {
			Params struct {
				Update struct {
					SessionUpdate string          `json:"sessionUpdate"`
					Content       json.RawMessage `json:"content"`
					Title         string          `json:"title"`
					Name          string          `json:"name"`
				} `json:"update"`
			} `json:"params"`
		}
		if json.Unmarshal(line, &ev) != nil {
			continue
		}
		upd := ev.Params.Update
		switch upd.SessionUpdate {
		case "user_message_chunk", "agent_message_chunk":
			text := strings.TrimSpace(grokContentText(upd.Content))
			if text == "" {
				continue
			}
			role := "user"
			if upd.SessionUpdate == "agent_message_chunk" {
				role = "assistant"
			}
			if n := len(turns); n > 0 && turns[n-1].Role == role && turns[n-1].Tool == "" {
				turns[n-1].Text += text
				continue
			}
			turns = append(turns, Turn{Role: role, Text: text})
		case "tool_call":
			name := upd.Title
			if name == "" {
				name = upd.Name
			}
			if name == "" {
				name = "tool"
			}
			turns = append(turns, Turn{Role: "tool", Tool: name})
		}
	}
	return turns, bytes, sc.Err()
}
