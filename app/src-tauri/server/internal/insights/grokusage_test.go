package insights

import (
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

func TestMain(m *testing.M) {
	// A real ~/.grok on the machine running the tests must not change
	// Claude-only counts. A test that wants the walk sets the dir itself.
	grokUsageDir = ""
	os.Exit(m.Run())
}

func writeGrokUsage(t *testing.T, root, id, body string) {
	t.Helper()
	dir := filepath.Join(root, "grp", id)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "updates.jsonl"), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	summary := `{"info":{"cwd":"/work"}}`
	if err := os.WriteFile(filepath.Join(dir, "summary.json"), []byte(summary), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestGrokUsageDeltasCumulativeTurns(t *testing.T) {
	root := t.TempDir()
	now := time.Now().Unix()
	old := (now - 10*86400) * 1000
	cur := now * 1000
	body := `{"params":{"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":3,"outputTokens":1}}}}
{"params":{"_meta":{"agentTimestampMs":` + itoa(old) + `},"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":10,"outputTokens":4}}}}
{"params":{"_meta":{"agentTimestampMs":` + itoa(cur) + `,"modelId":"grok-2"},"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":25,"outputTokens":9}}}}
turn_completed {
`
	writeGrokUsage(t, root, "sess1", body)
	model := `{"params":{"_meta":{"agentTimestampMs":` + itoa(cur) + `,"modelId":"grok-top"},"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":100,"outputTokens":10,"cachedReadTokens":40,"modelUsage":{"grok-4":{"inputTokens":100,"outputTokens":10,"cachedReadTokens":40}}}}}}
`
	writeGrokUsage(t, root, "sess2", model)

	rep := &ClaudeUsageReport{Series: []ClaudeBucket{}, ByModel: []ClaudeRow{}, ByProject: []ClaudeRow{}, BySession: []ClaudeRow{}}
	buckets := map[int64]*ClaudeTokens{}
	byModel, byProject, bySession := newAgg(), newAgg(), newAgg()
	scanGrokUsage(root, now-3600, now+60, rep, buckets, byModel, byProject, bySession)
	if rep.Totals.Calls != 2 || rep.Totals.In != 75 || rep.Totals.Out != 15 || rep.Totals.CacheRead != 40 {
		t.Fatalf("totals = %+v, want calls 2 in 75 out 15 cache 40", rep.Totals)
	}
	if rep.ParseErrors < 1 {
		t.Fatal("a broken turn_completed line should be a parse error")
	}
	if _, ok := byModel.tok["grok-2 standard"]; !ok {
		t.Fatalf("models = %v", keysOf(byModel))
	}
	if _, ok := byModel.tok["grok-4 standard"]; !ok {
		t.Fatalf("modelUsage row missing: %v", keysOf(byModel))
	}
	if _, ok := byModel.tok["grok-top standard"]; ok {
		t.Fatal("top-level totals were counted beside modelUsage")
	}
}

func TestGrokUsageMissingDirIsEmpty(t *testing.T) {
	rep := &ClaudeUsageReport{}
	buckets := map[int64]*ClaudeTokens{}
	scanGrokUsage(filepath.Join(t.TempDir(), "nope"), 0, time.Now().Unix(), rep, buckets, newAgg(), newAgg(), newAgg())
	if rep.Totals.Calls != 0 || rep.ScannedFiles != 0 {
		t.Fatalf("missing dir counted: %+v files=%d", rep.Totals, rep.ScannedFiles)
	}
}

func TestScanClaudeUsageFoldsGrokOnlyWhenRootSet(t *testing.T) {
	claude := t.TempDir()
	grok := t.TempDir()
	now := time.Now().Unix()
	writeGrokUsage(t, grok, "sess1", `{"params":{"_meta":{"agentTimestampMs":`+itoa(now*1000)+`,"modelId":"grok-2"},"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":7,"outputTokens":1}}}}
`)
	rep, err := scanClaudeUsage(claude, now-3600, now+60)
	if err != nil || rep.Totals.Calls != 0 {
		t.Fatalf("unset grok root changed the scan: err=%v totals=%+v", err, rep.Totals)
	}
	old := grokUsageDir
	grokUsageDir = grok
	t.Cleanup(func() { grokUsageDir = old })
	rep, err = scanClaudeUsage(claude, now-3600, now+60)
	if err != nil {
		t.Fatal(err)
	}
	if rep.Totals.Calls != 1 || rep.Totals.In != 7 {
		t.Fatalf("grok root not folded: %+v", rep.Totals)
	}
}

func itoa(n int64) string {
	return strconv.FormatInt(n, 10)
}

func keysOf(a *claudeAgg) []string {
	out := make([]string, 0, len(a.tok))
	for k := range a.tok {
		out = append(out, k)
	}
	return out
}
