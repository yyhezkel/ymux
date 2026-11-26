package term

// Orca idea #9: session-history search. The matcher cases in
// testdata/search_match.json are shared with app/src/paletteMatch.test.ts,
// so the daemon and the desktop picker match the same way.

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"strings"
	"testing"
	"time"
)

func TestMatchTokensSharedCases(t *testing.T) {
	b, err := os.ReadFile("testdata/search_match.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name   string   `json:"name"`
		Query  string   `json:"query"`
		Fields []string `json:"fields"`
		Want   bool     `json:"want"`
	}
	if err := json.Unmarshal(b, &cases); err != nil {
		t.Fatal(err)
	}
	if len(cases) == 0 {
		t.Fatal("no cases")
	}
	for _, c := range cases {
		if got := matchTokens(c.Query, c.Fields...); got != c.Want {
			t.Errorf("%s: matchTokens(%q, %q) = %v, want %v", c.Name, c.Query, c.Fields, got, c.Want)
		}
	}
}

func TestSessionQueryFilters(t *testing.T) {
	q := parseSessionQuery(url.Values{"q": {"  login "}, "branch": {"Feat/"}, "model": {"opus"}})
	if !q.active() || q.Q != "login" {
		t.Fatalf("parsed = %+v", q)
	}
	if !q.match("", "/srv", "feat/login", "claude-opus-4", "fix login") {
		t.Error("all filters hold, want a match")
	}
	if q.match("", "/srv", "main", "claude-opus-4", "fix login") {
		t.Error("branch filter ignored")
	}
	if q.match("", "/srv", "feat/login", "claude-sonnet-4", "fix login") {
		t.Error("model filter ignored")
	}
	if (sessionQuery{}).active() || !(sessionQuery{}).match("", "", "", "") {
		t.Error("the zero query must be inactive and match everything")
	}
	long := parseSessionQuery(url.Values{"q": {strings.Repeat("א", 500)}})
	if n := len([]rune(long.Q)); n != searchMaxRunes {
		t.Errorf("query kept %d runes, want %d", n, searchMaxRunes)
	}
}

// searchRoot writes four transcripts, newest first: s4 … s1.
func searchRoot(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "-srv-api", "s1", now.Add(-4*time.Minute),
		`{"type":"summary","summary":"Payment retries"}`,
		`{"type":"user","cwd":"/srv/api","gitBranch":"main","message":{"content":"add retries to payments"}}`,
		`{"type":"assistant","gitBranch":"main","message":{"model":"claude-sonnet-4","content":[{"type":"text","text":"Added."}]}}`)
	writeTranscript(t, root, "-srv-api", "s2", now.Add(-3*time.Minute),
		`{"type":"user","cwd":"/srv/api","gitBranch":"feat/login","message":{"content":"fix the login bug"}}`,
		`{"type":"assistant","gitBranch":"feat/login","message":{"model":"claude-opus-4","content":[{"type":"text","text":"Fixed the session cookie."}]}}`,
		`{"type":"custom-title","customTitle":"Login cookie"}`)
	writeTranscript(t, root, "-srv-web", "s3", now.Add(-2*time.Minute),
		`{"type":"user","cwd":"/srv/web","gitBranch":"main","message":{"content":"login page layout"}}`,
		`{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"ok"}]}}`)
	writeTranscript(t, root, "-srv-web", "s4", now.Add(-time.Minute),
		`{"type":"user","cwd":"/srv/web","gitBranch":"main","message":{"content":"bump deps"}}`)
	return root
}

func ids(rows []ClaudeSessionInfo) string {
	out := make([]string, len(rows))
	for i, r := range rows {
		out[i] = r.SessionID
	}
	return strings.Join(out, ",")
}

func TestPeekReadsTitleBranchModel(t *testing.T) {
	all := listClaudeSessions(searchRoot(t), 10, "")
	if ids(all) != "s4,s3,s2,s1" {
		t.Fatalf("order = %s", ids(all))
	}
	s2, s1, s3 := all[2], all[3], all[1]
	if s2.Title != "Login cookie" || s2.GitBranch != "feat/login" || s2.Model != "claude-opus-4" {
		t.Errorf("s2 = %+v", s2)
	}
	if s1.Title != "Payment retries" || s1.Model != "claude-sonnet-4" {
		t.Errorf("s1 = %+v", s1)
	}
	if s3.Model != "" {
		t.Errorf("a <synthetic> model leaked: %+v", s3)
	}
}

func TestSearchClaudeSessions(t *testing.T) {
	root := searchRoot(t)
	cases := []struct {
		name          string
		q             sessionQuery
		offset, limit int
		want          string
	}{
		{"preview text", sessionQuery{Q: "login"}, 0, 10, "s3,s2"},
		{"title", sessionQuery{Q: "PAYMENT"}, 0, 10, "s1"},
		{"cwd", sessionQuery{Q: "srv/web"}, 0, 10, "s4,s3"},
		{"model", sessionQuery{Q: "opus"}, 0, 10, "s2"},
		{"token-AND across fields", sessionQuery{Q: "login main"}, 0, 10, "s3"},
		{"assistant preview", sessionQuery{Q: "cookie"}, 0, 10, "s2"},
		{"branch filter", sessionQuery{Branch: "feat"}, 0, 10, "s2"},
		{"model filter", sessionQuery{Model: "sonnet"}, 0, 10, "s1"},
		{"page 1 of the filtered rows", sessionQuery{Q: "srv"}, 0, 2, "s4,s3"},
		{"page 2 of the filtered rows", sessionQuery{Q: "srv"}, 2, 2, "s2,s1"},
		{"offset past the end", sessionQuery{Q: "srv"}, 9, 2, ""},
		{"no match", sessionQuery{Q: "kubernetes"}, 0, 10, ""},
		{"no query pages too", sessionQuery{}, 1, 2, "s3,s2"},
	}
	for _, c := range cases {
		got, st := searchClaudeSessions(root, "", c.q, c.offset, c.limit)
		if ids(got) != c.want {
			t.Errorf("%s: got %q, want %q", c.name, ids(got), c.want)
		}
		if st.capped {
			t.Errorf("%s: capped on 4 files", c.name)
		}
	}
	// Early exit: a full page stops the scan before the older files.
	if _, st := searchClaudeSessions(root, "", sessionQuery{Q: "srv"}, 0, 1); st.peeked != 1 {
		t.Errorf("peeked %d files for a 1-row page, want 1", st.peeked)
	}
	// A scope and a query combine.
	if got, _ := searchClaudeSessions(root, "/srv/api", sessionQuery{Q: "login"}, 0, 10); ids(got) != "s2" {
		t.Errorf("scoped search = %q", ids(got))
	}
	// The unfiltered first page reads no file before the offset.
	if _, st := searchClaudeSessions(root, "", sessionQuery{}, 3, 10); st.peeked != 1 {
		t.Errorf("no-query offset peeked %d, want 1", st.peeked)
	}
}

func TestSearchClaudeSessionsScanCap(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	for i := 0; i < searchScanMax+5; i++ {
		writeTranscript(t, root, "-p", fmt.Sprintf("x%04d", i),
			now.Add(-time.Duration(i)*time.Second), `{"type":"user","cwd":"/p","message":{"content":"nothing"}}`)
	}
	got, st := searchClaudeSessions(root, "", sessionQuery{Q: "absent"}, 0, 10)
	if len(got) != 0 || !st.capped || st.peeked != searchScanMax {
		t.Fatalf("got %d rows, peeked %d, capped %v", len(got), st.peeked, st.capped)
	}
}

func TestClaudeSessionsSearchREST(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	root := searchRoot(t)
	old := claudeProjectsRoot
	defer func() { claudeProjectsRoot = old }()
	claudeProjectsRoot = func() string { return root }
	w := do(s, "GET", "/api/v2/claude/sessions?q=login+main&limit=5", "owner-token", "")
	var rows []ClaudeSessionInfo
	if w.Code != http.StatusOK || json.Unmarshal(w.Body.Bytes(), &rows) != nil || ids(rows) != "s3" {
		t.Fatalf("→ %d %s", w.Code, w.Body)
	}
	w = do(s, "GET", "/api/v2/claude/sessions?branch=feat&offset=0", "owner-token", "")
	if json.Unmarshal(w.Body.Bytes(), &rows) != nil || ids(rows) != "s2" || rows[0].GitBranch != "feat/login" {
		t.Fatalf("branch → %s", w.Body)
	}
}

func TestHistorySearchAndPaging(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	s.home = historyHome(t, map[string]any{
		"api":  map[string]any{"claude_session_id": sid, "ended_at": "2026-10-04T00:00:00Z", "label": "API", "cwd": "/srv/api"},
		"web":  map[string]any{"claude_session_id": "b", "ended_at": "2026-10-03T00:00:00Z", "auto_name": "web layout", "cwd": "/srv/web"},
		"docs": map[string]any{"claude_session_id": "c", "ended_at": "2026-10-02T00:00:00Z", "label": "Docs", "cwd": "/srv/docs"},
	}, `{"type":"user","cwd":"/srv/api","gitBranch":"feat/pay","message":{"content":"add payment retries"}}`+"\n"+
		`{"type":"assistant","message":{"model":"claude-opus-4","content":[{"type":"text","text":"Done."}]}}`+"\n")
	get := func(query string) []HistoryRow {
		t.Helper()
		w := do(s, "GET", "/api/v2/term/history"+query, "owner-token", "")
		var rows []HistoryRow
		if w.Code != http.StatusOK || json.Unmarshal(w.Body.Bytes(), &rows) != nil {
			t.Fatalf("%s → %d %s", query, w.Code, w.Body)
		}
		return rows
	}
	names := func(rows []HistoryRow) string {
		out := []string{}
		for _, r := range rows {
			out = append(out, r.Name)
		}
		return strings.Join(out, ",")
	}
	if got := names(get("")); got != "api,web,docs" {
		t.Errorf("no query = %s", got)
	}
	if rows := get(""); rows[0].Preview != "" || rows[0].Model != "" {
		t.Errorf("the no-query path read a transcript: %+v", rows[0])
	}
	if got := names(get("?q=srv&offset=1&limit=1")); got != "web" {
		t.Errorf("paged search = %s", got)
	}
	if got := names(get("?offset=1")); got != "web,docs" {
		t.Errorf("offset only = %s", got)
	}
	rows := get("?q=payment+opus")
	if names(rows) != "api" || rows[0].GitBranch != "feat/pay" || rows[0].Model != "claude-opus-4" || rows[0].Preview != "add payment retries" {
		t.Errorf("transcript search = %+v", rows)
	}
	if got := names(get("?q=layout")); got != "web" {
		t.Errorf("display search = %s", got)
	}
	if got := names(get("?branch=feat")); got != "api" {
		t.Errorf("branch filter = %s", got)
	}
	if got := names(get("?q=nothing-here")); got != "" {
		t.Errorf("no match = %s", got)
	}
}
