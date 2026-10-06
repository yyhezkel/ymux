package insights

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// writeTranscript lays out one `<projects>/<dir>/<session>.jsonl` and stamps
// its mtime, since the mtime prune is load-bearing.
func writeTranscript(t *testing.T, root, dir, session string, lines []string, mtime time.Time) {
	t.Helper()
	d := filepath.Join(root, dir)
	if err := os.MkdirAll(d, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	p := filepath.Join(d, session+".jsonl")
	body := ""
	for _, l := range lines {
		body += l + "\n"
	}
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		t.Fatalf("write: %v", err)
	}
	if err := os.Chtimes(p, mtime, mtime); err != nil {
		t.Fatalf("chtimes: %v", err)
	}
}

// assistantLine is a transcript line in the real shape — the field names come
// from an actual Claude Code transcript, not from memory.
func assistantLine(ts time.Time, session, cwd, model string, in, out, cr, w5, w1h int, sidechain bool, speed string) string {
	return fmt.Sprintf(`{"type":"assistant","timestamp":%q,"sessionId":%q,"cwd":%q,`+
		`"isSidechain":%t,"message":{"model":%q,"usage":{"input_tokens":%d,"output_tokens":%d,`+
		`"cache_read_input_tokens":%d,"cache_creation_input_tokens":%d,`+
		`"cache_creation":{"ephemeral_5m_input_tokens":%d,"ephemeral_1h_input_tokens":%d},`+
		`"speed":%q}}}`,
		ts.UTC().Format(time.RFC3339), session, cwd, sidechain, model,
		in, out, cr, w5+w1h, w5, w1h, speed)
}

func TestScanCountsTokensAndSplitsCacheWrites(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "proj-a", "s1", []string{
		assistantLine(now.Add(-30*time.Minute), "s1", "/home/y/a", "claude-opus-5", 100, 200, 3000, 400, 500, false, "standard"),
		assistantLine(now.Add(-20*time.Minute), "s1", "/home/y/a", "claude-opus-5", 10, 20, 30, 40, 0, false, "standard"),
	}, now)

	rep, err := scanClaudeUsage(root, now.Add(-2*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.Totals.Calls != 2 {
		t.Fatalf("calls: want 2 got %d", rep.Totals.Calls)
	}
	if rep.Totals.In != 110 || rep.Totals.Out != 220 || rep.Totals.CacheRead != 3030 {
		t.Fatalf("token totals wrong: %+v", rep.Totals)
	}
	// The 5m/1h split is the whole reason this is not one number: a 1-hour
	// write costs 2x base input, a 5-minute one 1.25x.
	if rep.Totals.CacheW5m != 440 || rep.Totals.CacheW1h != 500 {
		t.Fatalf("cache write split wrong: 5m=%d 1h=%d", rep.Totals.CacheW5m, rep.Totals.CacheW1h)
	}
	if rep.Totals.CacheWrite != 940 {
		t.Fatalf("cache_write total: want 940 got %d", rep.Totals.CacheWrite)
	}
	if rep.Sessions != 1 || rep.Projects != 1 {
		t.Fatalf("sessions=%d projects=%d, want 1/1", rep.Sessions, rep.Projects)
	}
}

// A transcript written before the window cannot hold an in-window line, so it
// must never be opened. This is what keeps a 240 MB tree affordable inside a
// six-second curl.
func TestScanPrunesByMtime(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	old := now.Add(-72 * time.Hour)
	writeTranscript(t, root, "proj-old", "s-old", []string{
		assistantLine(old, "s-old", "/home/y/old", "claude-opus-5", 999, 999, 0, 0, 0, false, "standard"),
	}, old)
	writeTranscript(t, root, "proj-new", "s-new", []string{
		assistantLine(now.Add(-10*time.Minute), "s-new", "/home/y/new", "claude-opus-5", 1, 2, 0, 0, 0, false, "standard"),
	}, now)

	rep, err := scanClaudeUsage(root, now.Add(-1*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.ScannedFiles != 1 {
		t.Fatalf("scanned: want 1 got %d", rep.ScannedFiles)
	}
	if rep.SkippedFiles != 1 {
		t.Fatalf("skipped: want 1 got %d", rep.SkippedFiles)
	}
	if rep.Totals.In != 1 {
		t.Fatalf("the pruned file leaked into the totals: %+v", rep.Totals)
	}
}

// A file whose mtime is inside the window may still hold lines outside it —
// the per-line timestamp filter is a separate guard from the mtime prune.
func TestScanFiltersLinesOutsideTheWindow(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "proj", "s", []string{
		assistantLine(now.Add(-48*time.Hour), "s", "/home/y/p", "claude-opus-5", 500, 500, 0, 0, 0, false, "standard"),
		assistantLine(now.Add(-5*time.Minute), "s", "/home/y/p", "claude-opus-5", 7, 8, 0, 0, 0, false, "standard"),
	}, now)

	rep, err := scanClaudeUsage(root, now.Add(-1*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.Totals.Calls != 1 || rep.Totals.In != 7 {
		t.Fatalf("stale line counted: %+v", rep.Totals)
	}
}

func TestScanRollsUpByModelSpeedProjectAndSession(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "a", "s1", []string{
		assistantLine(now.Add(-10*time.Minute), "s1", "/home/y/a", "claude-opus-5", 100, 100, 0, 0, 0, false, "standard"),
		assistantLine(now.Add(-9*time.Minute), "s1", "/home/y/a", "claude-opus-5", 50, 50, 0, 0, 0, false, "fast"),
	}, now)
	writeTranscript(t, root, "b", "s2", []string{
		assistantLine(now.Add(-8*time.Minute), "s2", "/home/y/b", "claude-haiku-4-5", 10, 10, 0, 0, 0, true, "standard"),
	}, now)

	rep, err := scanClaudeUsage(root, now.Add(-1*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}

	// Same model at two speeds must NOT collapse — fast mode is priced apart.
	if len(rep.ByModel) != 3 {
		t.Fatalf("by_model: want 3 rows (opus std, opus fast, haiku), got %d: %+v", len(rep.ByModel), rep.ByModel)
	}
	var fastFound, stdFound bool
	for _, m := range rep.ByModel {
		if m.Key == "claude-opus-5" && m.Speed == "fast" {
			fastFound = true
			if m.In != 50 {
				t.Fatalf("fast row tokens: want 50 got %d", m.In)
			}
		}
		if m.Key == "claude-opus-5" && m.Speed == "standard" {
			stdFound = true
		}
		// The model id must arrive clean, with the speed in its own field.
		if m.Key == "" || m.Speed == "" {
			t.Fatalf("model row not split: %+v", m)
		}
	}
	if !fastFound || !stdFound {
		t.Fatalf("speed rows missing: %+v", rep.ByModel)
	}

	if len(rep.ByProject) != 2 || len(rep.BySession) != 2 {
		t.Fatalf("projects=%d sessions=%d, want 2/2", len(rep.ByProject), len(rep.BySession))
	}
	// Subagent work is tracked separately — "where did my quota go" is usually
	// answered by this number.
	if rep.Sidechain.Calls != 1 || rep.Sidechain.In != 10 {
		t.Fatalf("sidechain split wrong: %+v", rep.Sidechain)
	}
	for _, s := range rep.BySession {
		if s.Key == "s1" && s.Project != "/home/y/a" {
			t.Fatalf("session row lost its project: %+v", s)
		}
	}
}

func TestScanBucketsHourlyInOrder(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "p", "s", []string{
		assistantLine(now.Add(-150*time.Minute), "s", "/p", "claude-opus-5", 1, 1, 0, 0, 0, false, "standard"),
		assistantLine(now.Add(-90*time.Minute), "s", "/p", "claude-opus-5", 2, 2, 0, 0, 0, false, "standard"),
		assistantLine(now.Add(-85*time.Minute), "s", "/p", "claude-opus-5", 3, 3, 0, 0, 0, false, "standard"),
	}, now)

	rep, err := scanClaudeUsage(root, now.Add(-4*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.BucketS != 3600 {
		t.Fatalf("bucket_s: want 3600 got %d", rep.BucketS)
	}
	if len(rep.Series) == 0 {
		t.Fatal("empty series")
	}
	for i := 1; i < len(rep.Series); i++ {
		if rep.Series[i].T <= rep.Series[i-1].T {
			t.Fatalf("series not strictly ascending at %d: %+v", i, rep.Series)
		}
	}
	var calls int
	for _, b := range rep.Series {
		calls += b.Calls
		if b.T%3600 != 0 {
			t.Fatalf("bucket %d is not on an hour boundary", b.T)
		}
	}
	if calls != 3 {
		t.Fatalf("series lost calls: want 3 got %d", calls)
	}
}

// Older transcripts carry only the flat cache_creation_input_tokens. Attribute
// it to the CHEAPER bucket so an unknown split under-states rather than
// over-states what the user is told they spent.
func TestScanFallsBackToTheCheaperCacheBucket(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	line := fmt.Sprintf(`{"type":"assistant","timestamp":%q,"sessionId":"s","cwd":"/p",`+
		`"message":{"model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1,`+
		`"cache_read_input_tokens":0,"cache_creation_input_tokens":900}}}`,
		now.Add(-5*time.Minute).UTC().Format(time.RFC3339))
	writeTranscript(t, root, "p", "s", []string{line}, now)

	rep, err := scanClaudeUsage(root, now.Add(-1*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.Totals.CacheW5m != 900 || rep.Totals.CacheW1h != 0 {
		t.Fatalf("legacy cache write misattributed: 5m=%d 1h=%d", rep.Totals.CacheW5m, rep.Totals.CacheW1h)
	}
}

// Non-assistant lines, malformed JSON and lines with no usage block are the
// bulk of a transcript. None of them may count, and none may abort the scan.
func TestScanIgnoresNoiseWithoutFailing(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "p", "s", []string{
		`{"type":"user","timestamp":"` + now.UTC().Format(time.RFC3339) + `","sessionId":"s"}`,
		`{"type":"attachment","usage":`,             // truncated JSON, has the marker
		`not json at all`,                           // no marker — cheap reject
		`{"type":"system","message":{"usage":{}}}`,  // no model, no timestamp
		assistantLine(now.Add(-time.Minute), "s", "/p", "claude-opus-5", 5, 5, 0, 0, 0, false, "standard"),
	}, now)

	rep, err := scanClaudeUsage(root, now.Add(-1*time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.Totals.Calls != 1 || rep.Totals.In != 5 {
		t.Fatalf("noise counted: %+v", rep.Totals)
	}
	if rep.ParseErrors == 0 {
		t.Fatal("the truncated line should have been counted as a parse error")
	}
}

// A machine that has never run Claude Code has no ~/.claude/projects at all.
// That is an empty report, not a 500.
func TestScanMissingRootIsEmptyNotAnError(t *testing.T) {
	rep, err := scanClaudeUsage(filepath.Join(t.TempDir(), "nope"), 0, time.Now().Unix())
	if err != nil {
		t.Fatalf("missing root should not error: %v", err)
	}
	if rep.Totals.Calls != 0 {
		t.Fatalf("want empty, got %+v", rep.Totals)
	}
	b, err := json.Marshal(rep)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	// Empty slices, not null — the client maps over them without a guard.
	for _, want := range []string{`"series":[]`, `"by_model":[]`, `"by_project":[]`, `"by_session":[]`} {
		if !strings.Contains(string(b), want) {
			t.Fatalf("missing %s in %s", want, b)
		}
	}
}

// The handler clamps a nonsense range instead of erroring, same contract as
// /analytics.
func TestHandleClaudeUsageClamps(t *testing.T) {
	svc := NewService(nil, NewSampler(), "")
	for _, q := range []string{"", "?since=0&until=0", "?since=99999999999", "?since=1"} {
		rec := httptest.NewRecorder()
		svc.handleClaudeUsage(rec, httptest.NewRequest("GET", "/claude-usage"+q, nil))
		if rec.Code != http.StatusOK {
			t.Fatalf("%q: want 200 got %d (%s)", q, rec.Code, rec.Body.String())
		}
		var rep ClaudeUsageReport
		if err := json.Unmarshal(rec.Body.Bytes(), &rep); err != nil {
			t.Fatalf("%q: bad json: %v", q, err)
		}
		if rep.Since >= rep.Until {
			t.Fatalf("%q: empty window %d..%d", q, rep.Since, rep.Until)
		}
		if rep.Until-rep.Since > 365*86400 {
			t.Fatalf("%q: span %ds exceeds the scan bound", q, rep.Until-rep.Since)
		}
	}
}

// More than the old 2000-file cap: every in-window file must be scanned, or a
// heavy user's totals silently shrink. Breaking this means the cap is back.
func TestScanHasNoFileCap(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	const n = 2001
	for i := 0; i < n; i++ {
		writeTranscript(t, root, "proj", fmt.Sprintf("s%d", i), []string{
			assistantLine(now.Add(-time.Minute), "s", "/p", "claude-opus-5", 1, 1, 0, 0, 0, false, "standard"),
		}, now)
	}
	rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.ScannedFiles != n || rep.Totals.Calls != n {
		t.Fatalf("scanned=%d calls=%d, want %d/%d", rep.ScannedFiles, rep.Totals.Calls, n, n)
	}
}

// Pruned files stay in skipped_files even past 2000 files; only the cap went.
func TestScanCountsEveryFileInWindow(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	old := now.Add(-72 * time.Hour)
	const n = 2001
	for i := 0; i < n; i++ {
		mt := now
		if i%2 == 1 {
			mt = old
		}
		writeTranscript(t, root, "proj", fmt.Sprintf("s%d", i), []string{
			assistantLine(now.Add(-time.Minute), "s", "/p", "claude-opus-5", 1, 1, 0, 0, 0, false, "standard"),
		}, mt)
	}
	rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.ScannedFiles != 1001 || rep.SkippedFiles != 1000 || rep.Totals.Calls != 1001 {
		t.Fatalf("scanned=%d skipped=%d calls=%d", rep.ScannedFiles, rep.SkippedFiles, rep.Totals.Calls)
	}
}

// A line past the old 8 MiB Scanner limit must not hide the usage line after
// it; the Rust scanner has no limit, so both sides must count the same call.
func TestScanKeepsLinesAfterOversizedLine(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	big := `{"type":"user","content":"` + strings.Repeat("x", 9*1024*1024) + `"}`
	writeTranscript(t, root, "proj", "s1", []string{
		big,
		assistantLine(now.Add(-time.Minute), "s1", "/p", "claude-opus-5", 5, 6, 0, 0, 0, false, "standard"),
	}, now)
	rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.Totals.Calls != 1 || rep.Totals.In != 5 || rep.ParseErrors != 0 {
		t.Fatalf("calls=%d in=%d parse_errors=%d, want 1/5/0", rep.Totals.Calls, rep.Totals.In, rep.ParseErrors)
	}
}

// A line with invalid UTF-8 is one parse error and is dropped, matching Rust's
// lines(); the usage line after it must still count.
func TestScanInvalidUTF8LineIsOneParseError(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	writeTranscript(t, root, "proj", "s1", []string{
		"{\"type\":\"assistant\",\"usage\":\"\xff\xfe\"}",
		assistantLine(now.Add(-time.Minute), "s1", "/p", "claude-opus-5", 5, 6, 0, 0, 0, false, "standard"),
	}, now)
	rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.ParseErrors != 1 || rep.Totals.Calls != 1 {
		t.Fatalf("parse_errors=%d calls=%d, want 1/1", rep.ParseErrors, rep.Totals.Calls)
	}
}

// A token field must be absent, null, or a non-negative integer; anything else
// makes the whole line one parse error (D4, mirrors Rust as_u64). Breaking this
// means Go and Rust would total the same transcript differently.
func TestScanRejectsNonUintTokenFields(t *testing.T) {
	now := time.Now()
	ts := now.Add(-time.Minute).UTC().Format(time.RFC3339)
	mk := func(usage string) string {
		return `{"type":"assistant","timestamp":"` + ts + `","sessionId":"s","cwd":"/p","message":{"model":"claude-opus-5","usage":` + usage + `}}`
	}
	bad := []string{
		`{"input_tokens":1.5}`,
		`{"output_tokens":-1}`,
		`{"cache_read_input_tokens":"7"}`,
		`{"cache_creation_input_tokens":true}`,
		`{"cache_creation":{"ephemeral_5m_input_tokens":1.5}}`,
		`{"cache_creation":{"ephemeral_1h_input_tokens":-1}}`,
		`{"input_tokens":1e3}`,
		`{"cache_creation":"x"}`,
		`"x"`,
	}
	for _, u := range bad {
		root := t.TempDir()
		writeTranscript(t, root, "proj", "s1", []string{mk(u)}, now)
		rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
		if err != nil {
			t.Fatalf("scan: %v", err)
		}
		if rep.ParseErrors != 1 || rep.Totals.Calls != 0 {
			t.Fatalf("%s: parse_errors=%d calls=%d, want 1/0", u, rep.ParseErrors, rep.Totals.Calls)
		}
	}
	// null and absent are valid zeros
	root := t.TempDir()
	writeTranscript(t, root, "proj", "s1", []string{
		mk(`{"input_tokens":null,"cache_creation":null}`),
		mk(`{"output_tokens":4}`),
	}, now)
	rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.ParseErrors != 0 || rep.Totals.Calls != 2 || rep.Totals.Out != 4 {
		t.Fatalf("parse_errors=%d calls=%d out=%d, want 0/2/4", rep.ParseErrors, rep.Totals.Calls, rep.Totals.Out)
	}
}

// D5 parity with Rust: an assistant line whose usage is null (or only named in
// content) is skipped silently — no call, no parse error.
func TestScanSkipsAssistantWithoutUsage(t *testing.T) {
	now := time.Now()
	ts := now.Add(-time.Minute).UTC().Format(time.RFC3339)
	root := t.TempDir()
	writeTranscript(t, root, "proj", "s1", []string{
		`{"type":"assistant","timestamp":"` + ts + `","sessionId":"s","cwd":"/p","message":{"model":"claude-opus-5","usage":null}}`,
		`{"type":"assistant","timestamp":"` + ts + `","sessionId":"s","cwd":"/p","message":{"model":"claude-opus-5","content":"mentions \"usage\""}}`,
	}, now)
	rep, err := scanClaudeUsage(root, now.Add(-time.Hour).Unix(), now.Unix())
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if rep.Totals.Calls != 0 || rep.ParseErrors != 0 {
		t.Fatalf("calls=%d parse_errors=%d, want 0/0", rep.Totals.Calls, rep.ParseErrors)
	}
}
