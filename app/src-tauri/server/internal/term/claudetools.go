package term

// claudetools.go — the desktop's `claude -p` tools for a browser (Phase 119,
// WEB-DESIGN F5). Ports, run on this box with the daemon's own claude:
//
//   GET  /api/v2/claude/usage?force=1        claude_usage.rs — the /usage
//        quota (session %, week %, per model), cached 5 min, a failure backs
//        off 5 min, one probe at a time (callers get the cached value).
//   POST /api/v2/claude/summarize {pane_id, session, workspace_id}
//        claude_summary.rs — the pane's Claude session's last N turns →
//        `claude -p <prompt>` → a note tagged "summary". The session is the
//        pane's newest context record, else session-meta's id for the tmux
//        session, else the newest transcript (the desktop's pick).
//   POST /api/v2/term/sessions/summarize {names, lang}
//        sessions_overview.rs — the last 40 screen lines of each session
//        (240 chars a line, escapes stripped) → one JSON-array answer → a
//        status + one-line summary per session, parsed leniently.
//
// Safety, beyond the desktop: the prompts are fixed here or read from the
// settings document (never from the request); the model gets `--tools ""`
// (transcripts and screens are untrusted input — nothing in them may act) and
// `--no-session-persistence` (a summary must not appear as a session to
// resume). Every call is argv (Rule #3), has a timeout, and runs one at a
// time per kind. Logs carry counts and ids only — never the text (Rule #1).

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	usageTimeout     = 20 * time.Second
	usageCacheTTL    = 5 * time.Minute
	usageBackoff     = 5 * time.Minute
	summarizeTimeout = 45 * time.Second
	overviewTimeout  = 90 * time.Second
	overviewMax      = 25
	captureLines     = 40
	captureLineChars = 240
	defaultSummary   = "Summarize the last conversation in 2-3 sentences in the same language the conversation used."
)

// runClaude runs claude with args, stdin piped; swapped in tests.
var runClaude = func(ctx context.Context, bin string, args []string, stdin string) (string, error) {
	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.Stdin = strings.NewReader(stdin)
	out, err := cmd.Output()
	if ctx.Err() != nil {
		return "", errors.New("claude -p timed out")
	}
	if err != nil && len(out) == 0 {
		return "", fmt.Errorf("claude: %v", err)
	}
	return string(out), nil
}

// quietArgs are added to every model call that reads untrusted text.
var quietArgs = []string{"--tools", "", "--no-session-persistence"}

type claudeTools struct {
	summarizeMu, overviewMu sync.Mutex

	usageMu       sync.Mutex // guards the fields below
	usageCache    *ClaudeUsage
	usageFailedAt time.Time
	usageBusy     bool
}

// ── /usage ──────────────────────────────────────────────────────────────

// ModelUsage / ClaudeUsage mirror claude_usage.rs (ts-rs bindings).
type ModelUsage struct {
	Name  string `json:"name"`
	Pct   int    `json:"pct"`
	Reset string `json:"reset"`
}

type ClaudeUsage struct {
	SessionPct      int          `json:"session_pct"`
	SessionReset    string       `json:"session_reset"`
	WeekPct         int          `json:"week_pct"`
	WeekReset       string       `json:"week_reset"`
	Models          []ModelUsage `json:"models"`
	Contributing24h []string     `json:"contributing_24h"`
	Contributing7d  []string     `json:"contributing_7d"`
	FetchedUnix     int64        `json:"fetched_unix"`
}

// pctAndReset: "33% used · resets Jul 8, 4:10am (X)" → 33, "Jul 8, 4:10am (X)".
func pctAndReset(rest string) (int, string) {
	p, _ := strconv.Atoi(strings.TrimSpace(strings.SplitN(rest, "%", 2)[0]))
	if p < 0 || p > 255 {
		p = 0 // the Rust parses a u8
	}
	reset := ""
	if parts := strings.SplitN(rest, "resets ", 2); len(parts) == 2 {
		reset = strings.TrimSpace(parts[1])
	}
	return p, reset
}

// parseUsage is claude_usage.rs parse_usage.
func parseUsage(raw string, now int64) (ClaudeUsage, error) {
	var env struct {
		Result *string `json:"result"`
	}
	if json.Unmarshal([]byte(strings.TrimSpace(raw)), &env) != nil || env.Result == nil {
		return ClaudeUsage{}, errors.New("could not parse /usage output (claude installed & authenticated?)")
	}
	u := ClaudeUsage{Models: []ModelUsage{}, Contributing24h: []string{}, Contributing7d: []string{}, FetchedUnix: now}
	found := false
	mode := 0 // 0 none, 1 day, 2 week
	for _, line := range strings.Split(*env.Result, "\n") {
		t := strings.TrimSpace(line)
		switch {
		case strings.HasPrefix(t, "Current session:"):
			u.SessionPct, u.SessionReset = pctAndReset(strings.TrimPrefix(t, "Current session:"))
			found, mode = true, 0
		case strings.HasPrefix(t, "Current week (all models):"):
			u.WeekPct, u.WeekReset = pctAndReset(strings.TrimPrefix(t, "Current week (all models):"))
			mode = 0
		case strings.HasPrefix(t, "Current week ("):
			if _, r, ok := strings.Cut(t, "("); ok {
				if name, _, ok := strings.Cut(r, "):"); ok {
					if _, rest, ok := strings.Cut(t, "): "); ok {
						p, reset := pctAndReset(rest)
						u.Models = append(u.Models, ModelUsage{Name: name, Pct: p, Reset: reset})
					}
				}
			}
			mode = 0
		case strings.HasPrefix(t, "Last 24h"):
			mode = 1
			u.Contributing24h = append(u.Contributing24h, t)
		case strings.HasPrefix(t, "Last 7d"):
			mode = 2
			u.Contributing7d = append(u.Contributing7d, t)
		case t == "":
			mode = 0
		case strings.HasPrefix(line, "  "):
			if mode == 1 {
				u.Contributing24h = append(u.Contributing24h, t)
			} else if mode == 2 {
				u.Contributing7d = append(u.Contributing7d, t)
			}
		default:
			mode = 0
		}
	}
	if !found {
		return ClaudeUsage{}, errors.New("unexpected /usage format — no session line")
	}
	return u, nil
}

func (s *Service) handleClaudeUsage(w http.ResponseWriter, r *http.Request) {
	ct := s.claude
	force := r.URL.Query().Get("force") == "1" || r.URL.Query().Get("force") == "true"
	now := time.Now()
	ct.usageMu.Lock()
	cached := ct.usageCache
	fresh := cached != nil && now.Sub(time.Unix(cached.FetchedUnix, 0)) < usageCacheTTL
	backoff := !ct.usageFailedAt.IsZero() && now.Sub(ct.usageFailedAt) < usageBackoff
	if (fresh && !force) || ct.usageBusy || (backoff && !force) {
		ct.usageMu.Unlock()
		if cached != nil {
			writeJSON(w, http.StatusOK, cached)
			return
		}
		http.Error(w, "claude usage is not available right now — try again in a few minutes", http.StatusServiceUnavailable)
		return
	}
	ct.usageBusy = true
	ct.usageMu.Unlock()

	ctx, cancel := context.WithTimeout(r.Context(), usageTimeout)
	defer cancel()
	out, err := runClaude(ctx, s.claudeBinary(), []string{"-p", "/usage", "--output-format", "json"}, "")
	var u ClaudeUsage
	if err == nil {
		u, err = parseUsage(out, time.Now().Unix())
	}
	ct.usageMu.Lock()
	ct.usageBusy = false
	if err != nil {
		ct.usageFailedAt = time.Now()
		cached = ct.usageCache
	} else {
		ct.usageFailedAt = time.Time{}
		ct.usageCache = &u
	}
	ct.usageMu.Unlock()
	if err != nil {
		logger.Warn("claude usage probe failed", "err", err) // never the body (Rule #1)
		if cached != nil {
			writeJSON(w, http.StatusOK, cached)
			return
		}
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, http.StatusOK, u)
}

// ── summarize one pane's Claude session ─────────────────────────────────

// SummaryResult mirrors claude_summary.rs SummaryResult.
type SummaryResult struct {
	Text          string `json:"text"`
	SessionID     string `json:"session_id"`
	MessagesCount int    `json:"messages_count"`
	GeneratedAt   string `json:"generated_at"`
	NoteID        string `json:"note_id,omitempty"`
}

// summarySettings reads claude.summary_prompt / summary_history_count from the
// browser's settings document (defaults as the desktop's).
func (s *Service) summarySettings() (string, int) {
	prompt, n := defaultSummary, 10
	if s.hooks == nil || s.hooks.settings == nil {
		return prompt, n
	}
	var doc struct {
		Claude struct {
			Prompt *string `json:"summary_prompt"`
			Count  *int    `json:"summary_history_count"`
		} `json:"claude"`
	}
	if json.Unmarshal(s.hooks.settings.get().Settings, &doc) == nil {
		if doc.Claude.Prompt != nil && strings.TrimSpace(*doc.Claude.Prompt) != "" {
			prompt = strings.TrimSpace(*doc.Claude.Prompt)
		}
		if doc.Claude.Count != nil && *doc.Claude.Count > 0 && *doc.Claude.Count <= 200 {
			n = *doc.Claude.Count
		}
	}
	return strings.ReplaceAll(prompt, "{N}", strconv.Itoa(n)), n
}

// sessionForPane picks the Claude session to summarize (see the file doc).
func (s *Service) sessionForPane(paneID, tmuxName string) string {
	if s.hooks != nil && s.hooks.context != nil && paneID != "" {
		var best *SessionContext
		s.hooks.context.mu.Lock()
		for _, c := range s.hooks.context.sessions {
			if c.PaneID != nil && *c.PaneID == paneID && (best == nil || c.lastActivity() > best.lastActivity()) {
				best = c
			}
		}
		s.hooks.context.mu.Unlock()
		if best != nil && claudeSessionID.MatchString(best.SessionID) {
			return best.SessionID
		}
	}
	if tmuxName != "" {
		if e, ok := LoadMeta(s.home)[tmuxName]; ok && claudeSessionID.MatchString(e.ClaudeSessionID) {
			return e.ClaudeSessionID
		}
	}
	if l := listClaudeSessions(claudeProjectsRoot(), 1, ""); len(l) > 0 {
		return l[0].SessionID
	}
	return ""
}

func (s *Service) handleClaudeSummarize(w http.ResponseWriter, r *http.Request) {
	var in struct {
		PaneID      string `json:"pane_id"`
		Session     string `json:"session"`
		WorkspaceID string `json:"workspace_id"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 8<<10)).Decode(&in); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	if in.Session != "" && !ValidName(in.Session) {
		http.Error(w, ErrBadName.Error(), http.StatusBadRequest)
		return
	}
	if !s.claude.summarizeMu.TryLock() {
		http.Error(w, "a summary is already running", http.StatusConflict)
		return
	}
	defer s.claude.summarizeMu.Unlock()

	sid := s.sessionForPane(in.PaneID, in.Session)
	path, ok := transcriptPath(s.home, sid)
	if !ok {
		http.Error(w, "no Claude session found for this pane", http.StatusNotFound)
		return
	}
	f, err := os.Open(path)
	if err != nil {
		http.Error(w, "the transcript cannot be read", http.StatusInternalServerError)
		return
	}
	turns, _, err := parseTranscript(f)
	f.Close()
	if err != nil {
		http.Error(w, "the transcript cannot be read", http.StatusInternalServerError)
		return
	}
	prompt, n := s.summarySettings()
	var talk []Turn
	for _, t := range turns {
		if (t.Role == "user" || t.Role == "assistant") && strings.TrimSpace(t.Text) != "" {
			talk = append(talk, t)
		}
	}
	if len(talk) > n {
		talk = talk[len(talk)-n:]
	}
	if len(talk) == 0 {
		http.Error(w, "the session has no conversation to summarize yet", http.StatusUnprocessableEntity)
		return
	}
	var b strings.Builder
	for _, t := range talk {
		fmt.Fprintf(&b, "%s: %s\n", strings.ToUpper(t.Role), t.Text)
	}
	ctx, cancel := context.WithTimeout(r.Context(), summarizeTimeout)
	defer cancel()
	args := append([]string{"-p", prompt}, quietArgs...)
	out, err := runClaude(ctx, s.claudeBinary(), args, b.String())
	text := strings.TrimSpace(out)
	if err != nil || text == "" {
		msg := "claude returned no summary"
		if err != nil {
			msg = err.Error()
		}
		logger.Warn("summary failed", "session", sid, "err", msg)
		http.Error(w, msg, http.StatusBadGateway)
		return
	}
	res := SummaryResult{Text: text, SessionID: sid, MessagesCount: len(talk), GeneratedAt: time.Now().UTC().Format(time.RFC3339)}
	if s.hooks != nil {
		if note, err := s.hooks.notes.add(text, "summary", in.WorkspaceID, in.PaneID); err == nil {
			res.NoteID = note.ID
			s.hooks.hub.publish("notes:changed", same(nil))
		}
	}
	logger.Info("summary written", "session", sid, "turns", len(talk), "chars", len(text))
	writeJSON(w, http.StatusOK, res)
}

// ── the sessions overview ───────────────────────────────────────────────

// SessionSummary mirrors sessions_overview.rs SessionSummary.
type SessionSummary struct {
	Name    string `json:"name"`
	Status  string `json:"status"`
	Summary string `json:"summary"`
}

func languageName(lang string) string {
	switch lang {
	case "he":
		return "Hebrew"
	case "ar":
		return "Arabic"
	case "ru":
		return "Russian"
	}
	return "English"
}

func overviewPrompt(lang string) string {
	return "Below are terminal screens of several multiplexer sessions, separated by lines of the form " +
		"### SESSION <i>. Reply with ONLY a JSON array, one object per session, each shaped like " +
		"{i: <number>, status: <one of idle, working, waiting_input, error>, summary: <one short " +
		"sentence, at most 120 characters, in " + languageName(lang) + ", saying what the session is doing>}. " +
		"waiting_input means a prompt, question or permission request is waiting for the human. " +
		"working means a command or an agent is still running. error means the last thing on " +
		"screen is a failure. idle means a shell prompt with nothing running. No prose, no code fences."
}

// stripANSI drops CSI / OSC / two-byte escapes (sessions_overview.rs strip_ansi).
func stripANSI(s string) string {
	var b strings.Builder
	rs := []rune(s)
	for i := 0; i < len(rs); i++ {
		if rs[i] != 0x1b {
			b.WriteRune(rs[i])
			continue
		}
		i++
		if i >= len(rs) {
			break
		}
		switch rs[i] {
		case '[':
			for i++; i < len(rs) && !(rs[i] >= 0x40 && rs[i] <= 0x7e); i++ {
			}
		case ']':
			prev := rune(0)
			for i++; i < len(rs); i++ {
				if rs[i] == 0x07 || (prev == 0x1b && rs[i] == '\\') {
					break
				}
				prev = rs[i]
			}
		}
	}
	return b.String()
}

// clipCapture: the last 40 non-trailing-blank lines, 240 chars each.
func clipCapture(raw string) string {
	lines := strings.Split(stripANSI(raw), "\n")
	for i := range lines {
		lines[i] = strings.TrimRight(lines[i], "\r")
	}
	for len(lines) > 0 && strings.TrimSpace(lines[len(lines)-1]) == "" {
		lines = lines[:len(lines)-1]
	}
	if len(lines) > captureLines {
		lines = lines[len(lines)-captureLines:]
	}
	for i, l := range lines {
		if r := []rune(l); len(r) > captureLineChars {
			lines[i] = string(r[:captureLineChars])
		}
	}
	return strings.Join(lines, "\n")
}

// parseOverview is parse_summary_envelope: never an error — a missing or bad
// row is `unknown`.
func parseOverview(raw string, names []string) []SessionSummary {
	text := strings.TrimSpace(raw)
	var env struct {
		Result  *string `json:"result"`
		IsError bool    `json:"is_error"`
	}
	if json.Unmarshal([]byte(text), &env) == nil {
		if env.Result != nil {
			text = *env.Result
		} else if env.IsError {
			text = ""
		}
	}
	out := make([]SessionSummary, len(names))
	for i, n := range names {
		out[i] = SessionSummary{Name: n, Status: "unknown"}
	}
	start, end := strings.Index(text, "["), strings.LastIndex(text, "]")
	if start < 0 || end <= start {
		return out
	}
	var rows []struct {
		I       *int    `json:"i"`
		Status  *string `json:"status"`
		Summary *string `json:"summary"`
	}
	if json.Unmarshal([]byte(text[start:end+1]), &rows) != nil {
		return out
	}
	for _, row := range rows {
		if row.I == nil || *row.I < 1 || *row.I > len(out) {
			continue
		}
		slot := &out[*row.I-1]
		st := ""
		if row.Status != nil {
			st = strings.ToLower(strings.TrimSpace(*row.Status))
		}
		switch st {
		case "idle", "working", "waiting_input", "error":
			slot.Status = st
		default:
			slot.Status = "unknown"
		}
		if row.Summary != nil {
			if r := []rune(strings.TrimSpace(*row.Summary)); len(r) > 200 {
				slot.Summary = string(r[:200])
			} else {
				slot.Summary = string(r)
			}
		}
	}
	return out
}

func (s *Service) handleSessionsSummarize(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Names []string `json:"names"`
		Lang  string   `json:"lang"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&in); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	seen := map[string]bool{}
	var names []string
	for _, n := range in.Names {
		n = strings.TrimSpace(n)
		if n == "" || seen[n] {
			continue
		}
		if !ValidName(n) {
			http.Error(w, ErrBadName.Error(), http.StatusBadRequest)
			return
		}
		seen[n] = true
		names = append(names, n)
		if len(names) == overviewMax {
			break
		}
	}
	if len(names) == 0 {
		writeJSON(w, http.StatusOK, []SessionSummary{})
		return
	}
	lang := in.Lang
	if len(lang) != 2 || strings.ToLower(lang) != lang {
		lang = "en"
	}
	if !s.claude.overviewMu.TryLock() {
		http.Error(w, "a summary is already running", http.StatusConflict)
		return
	}
	defer s.claude.overviewMu.Unlock()
	var b strings.Builder
	for i, n := range names {
		fmt.Fprintf(&b, "\n### SESSION %d\n", i+1)
		b.WriteString(clipCapture(s.tmux.Capture(n, captureLines)))
		b.WriteByte('\n')
	}
	ctx, cancel := context.WithTimeout(r.Context(), overviewTimeout)
	defer cancel()
	args := append([]string{"-p", overviewPrompt(lang), "--output-format", "json"}, quietArgs...)
	out, err := runClaude(ctx, s.claudeBinary(), args, b.String())
	if err != nil {
		logger.Warn("sessions overview failed", "sessions", len(names), "err", err)
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	logger.Info("sessions summarized", "sessions", len(names))
	writeJSON(w, http.StatusOK, parseOverview(out, names))
}
