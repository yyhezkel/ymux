package term

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ymux-server/internal/agent"
)

func TestContextStoreModel(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "context", "sessions")
	st := newContextStore(dir)
	clock := time.UnixMilli(1_000_000)
	st.now = func() time.Time { clock = clock.Add(time.Second); return clock }

	if c, _ := st.apply(contextEvent{sessionID: "../etc", prompt: "x"}); c != nil {
		t.Fatal("a path-shaped id was accepted")
	}
	c, err := st.apply(contextEvent{sessionID: "s1", wsID: "w1", paneID: "p1", cwd: "/srv", prompt: "fix the bug"})
	if err != nil || c == nil || *c.FirstPrompt != "fix the bug" || c.Version != 1 {
		t.Fatalf("first prompt = %+v %v", c, err)
	}
	if b, _ := json.Marshal(c); !strings.Contains(string(b), `"log":[]`) {
		t.Fatalf("a fresh record's log must be [] not null: %s", b)
	}
	if c, _ = st.apply(contextEvent{sessionID: "s1", prompt: "second"}); c != nil {
		t.Fatalf("a later prompt wrote: %+v", c)
	}
	goal := agent.BriefFromStop(ptr("work\n[ymux-brief]\ntask: t1\nstatus: working\ndelta: d1\ngoal: ship it\ndone: tests pass"), nil, 0)
	if goal.Goal == nil || *goal.Goal != "ship it" || *goal.Done != "tests pass" {
		t.Fatalf("brief goal/done not parsed: %+v", goal)
	}
	c, _ = st.apply(contextEvent{sessionID: "s1", stop: &goal})
	plain := agent.BriefFromStop(ptr("no brief here"), nil, 0)
	c, _ = st.apply(contextEvent{sessionID: "s1", stop: &plain, wsID: ""})
	if *c.Goal != "ship it" || *c.DoneWhen != "tests pass" || len(c.Log) != 2 || !c.Log[1].Degraded || *c.Log[1].Delta != "no brief here" {
		t.Fatalf("after stops = %+v", c)
	}
	if *c.WsID != "w1" {
		t.Fatal("an empty ws_id erased the known one")
	}
	c, _ = st.apply(contextEvent{sessionID: "s1", ended: true, reason: "prompt_input_exit"})
	last := c.Log[len(c.Log)-1]
	if last.Kind != "closed" || *last.Delta != "prompt_input_exit" || *last.Task != "t1" || c.Version != 4 {
		t.Fatalf("closed = %+v v%d", last, c.Version)
	}
	_, _ = st.apply(contextEvent{sessionID: "s2", wsID: "w1", prompt: "newer"})
	_, _ = st.apply(contextEvent{sessionID: "s3", wsID: "w2", prompt: "elsewhere"})
	if l := st.list("w1"); len(l) != 2 || l[0].SessionID != "s2" {
		t.Fatalf("list = %+v", l)
	}

	// Persistence, the 200-line cap, and a corrupt file left alone.
	for i := 0; i < 250; i++ {
		_, _ = st.apply(contextEvent{sessionID: "s2", stop: &plain})
	}
	_ = os.WriteFile(filepath.Join(dir, "bad.json"), []byte("{not json"), 0o600)
	re := newContextStore(dir)
	if l := re.list("w1"); len(l) != 2 || len(l[0].Log) != contextLogMax {
		t.Fatalf("reloaded = %d sessions, log %d", len(l), len(l[0].Log))
	}
	if _, err := re.apply(contextEvent{sessionID: "bad", prompt: "x"}); err == nil {
		t.Fatal("a corrupt session was overwritten")
	}
	if b, _ := os.ReadFile(filepath.Join(dir, "bad.json")); !strings.HasPrefix(string(b), "{not") {
		t.Fatal("corrupt file changed")
	}
}
