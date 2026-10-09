package term

// Phase 101 (WEB-DESIGN B3): the feed, the gate, and the events socket.

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

// pushAsync is push for a goroutine: no *testing.T (vet's testinggoroutine —
// Fatal from a non-test goroutine), a failure comes back as the result.
func pushAsync(tg termHookTarget, params map[string]any) map[string]any {
	raw, _ := json.Marshal(params)
	res, rpcErr := tg.DispatchHook("feed.push", raw)
	if rpcErr != nil {
		return map[string]any{"rpc_error": rpcErr.Message}
	}
	return res.(map[string]any)
}

// frame is one decoded event.
type frame struct {
	Type string          `json:"type"`
	Data json.RawMessage `json:"data"`
}

// next reads the next event from a hub subscriber, failing after a second.
func next(t *testing.T, sub *subscriber) frame {
	t.Helper()
	select {
	case b, ok := <-sub.ch:
		if !ok {
			t.Fatal("subscriber channel closed")
		}
		var f frame
		if err := json.Unmarshal(b, &f); err != nil {
			t.Fatalf("bad frame %s: %v", b, err)
		}
		return f
	case <-time.After(time.Second):
		t.Fatal("no event within 1s")
	}
	return frame{}
}

// nextOf skips events until one of type typ arrives.
func nextOf(t *testing.T, sub *subscriber, typ string) frame {
	t.Helper()
	for i := 0; i < 20; i++ {
		if f := next(t, sub); f.Type == typ {
			return f
		}
	}
	t.Fatalf("no %q event", typ)
	return frame{}
}

func gateEntry(t *testing.T, policy string) (*HookRegistry, *hookEntry, termHookTarget) {
	t.Helper()
	r := NewHookRegistry()
	e, _, _ := r.mint("web", "127.0.0.1:1", "")
	e.policy = policy
	r.add(e)
	return r, e, matched(t, r, e)
}

func permission(e *hookEntry, id string, extra map[string]any) map[string]any {
	p := map[string]any{
		"request_id": id, "kind": "permission_request", "subkind": "pre-tool-use",
		"pane_id": e.paneID, "title": "Run `ls`?", "summary": "Bash",
		"payload": map[string]any{"tool_name": "Bash", "tool_input": map[string]any{"command": "ls"}},
	}
	for k, v := range extra {
		p[k] = v
	}
	return p
}

func TestPolicyNoneAllowsWithoutACard(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	sub := r.hub.add("en")
	res := push(t, tg, permission(e, "p1", nil))
	if res["decision"] != "allow" || res["policy"] != "none" {
		t.Fatalf("none → %v", res)
	}
	if n := len(r.feed.list("en")); n != 0 {
		t.Errorf("policy none must not make a card (desktop Auto), got %d", n)
	}
	// The light still moves, and subscribers hear it.
	f := next(t, sub)
	if f.Type != "pane:agent-run" || !strings.Contains(string(f.Data), `"state":"running"`) {
		t.Errorf("want a running pane:agent-run, got %s %s", f.Type, f.Data)
	}
}

func TestGateWaitsForADecision(t *testing.T) {
	r, e, tg := gateEntry(t, policyGate)
	sub := r.hub.add("en")

	got := make(chan map[string]any, 1)
	go func() { got <- pushAsync(tg, permission(e, "g1", nil)) }()

	added := nextOf(t, sub, "feed:item-added")
	var item FeedItem
	_ = json.Unmarshal(added.Data, &item)
	if item.RequestID != "g1" || item.State != statePending || !item.Blocking || item.Title != "Run `ls`?" {
		t.Fatalf("pending card = %+v", item)
	}
	select {
	case res := <-got:
		t.Fatalf("a gated request answered before any decision: %v", res)
	case <-time.After(50 * time.Millisecond):
	}

	if !r.decide("g1", "allow", "test") {
		t.Fatal("decide returned false for a pending request")
	}
	select {
	case res := <-got:
		if res["decision"] != "allow" || res["policy"] != "gate" {
			t.Fatalf("gate → %v", res)
		}
	case <-time.After(time.Second):
		t.Fatal("the gated request never returned")
	}
	resolved := nextOf(t, sub, "feed:item-resolved")
	if !strings.Contains(string(resolved.Data), `"decision":"allow"`) {
		t.Errorf("resolved = %s", resolved.Data)
	}
	if st := r.feed.list("en")[0].State; st != stateAllowed {
		t.Errorf("card state after allow = %q", st)
	}
	if r.decide("g1", "deny", "late") {
		t.Error("a second decision must lose — the first answer wins")
	}
}

func TestGateTimesOut(t *testing.T) {
	r, e, tg := gateEntry(t, policyGate)
	start := time.Now()
	res := push(t, tg, permission(e, "g2", map[string]any{"wait_timeout_seconds": 1}))
	if res["decision"] != "timeout" {
		t.Fatalf("want timeout, got %v", res)
	}
	if d := time.Since(start); d < 900*time.Millisecond || d > 3*time.Second {
		t.Errorf("timeout took %v, want ~1s", d)
	}
	if st := r.feed.list("en")[0].State; st != stateTimedout {
		t.Errorf("card state after timeout = %q", st)
	}
}

func TestWaitTimeoutClamp(t *testing.T) {
	i := func(v int64) *int64 { return &v }
	for _, c := range []struct {
		in   *int64
		want time.Duration
	}{{nil, 120 * time.Second}, {i(0), time.Second}, {i(-5), time.Second}, {i(30), 30 * time.Second}, {i(9999), 600 * time.Second}} {
		if got := waitTimeout(c.in); got != c.want {
			t.Errorf("waitTimeout(%v) = %v, want %v", c.in, got, c.want)
		}
	}
}

func TestKilledSessionDeniesItsPendingRequest(t *testing.T) {
	r, e, tg := gateEntry(t, policyGate)
	sub := r.hub.add("en")
	got := make(chan map[string]any, 1)
	go func() { got <- pushAsync(tg, permission(e, "g3", nil)) }()
	nextOf(t, sub, "feed:item-added")
	r.Remove("web")
	select {
	case res := <-got:
		if res["decision"] != "deny" {
			t.Fatalf("killed session's pending request → %v, want deny", res)
		}
	case <-time.After(time.Second):
		t.Fatal("the pending request outlived its session")
	}
}

func TestPromptAndNotificationMakeNoCard(t *testing.T) {
	r, e, tg := gateEntry(t, policyGate)
	push(t, tg, map[string]any{"request_id": "u", "kind": "passive", "subkind": "user-prompt-submit",
		"pane_id": e.paneID, "payload": map[string]any{"prompt": "hi"}})
	push(t, tg, map[string]any{"request_id": "n", "kind": "passive", "subkind": "notification",
		"pane_id": e.paneID, "payload": map[string]any{"notification_type": "idle_prompt"}})
	if n := len(r.feed.list("en")); n != 0 {
		t.Errorf("prompt/notification made %d cards, want 0", n)
	}
}

func TestStopCardUsesTheBriefInBothLanguages(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	push(t, tg, map[string]any{"request_id": "s1", "kind": "passive", "subkind": "stop", "pane_id": e.paneID,
		"title": "agent: stop",
		"payload": map[string]any{"last_assistant_message": "All done.\n[ymux-brief]\nstatus: waiting-for-you\nask: merge now?\nrec: yes\n"}})
	en, he := r.feed.list("en"), r.feed.list("he")
	if len(en) != 1 || len(he) != 1 {
		t.Fatalf("want one card, got en=%d he=%d", len(en), len(he))
	}
	if en[0].Summary != "merge now? · yes" || he[0].Summary != "merge now? · yes" {
		t.Errorf("a stop with a brief must show ask · rec, got %q / %q", en[0].Summary, he[0].Summary)
	}
	if en[0].Title == "agent: stop" || en[0].Title == he[0].Title {
		t.Errorf("titles must be humanized per language: en=%q he=%q", en[0].Title, he[0].Title)
	}
	if en[0].State != statePassive || en[0].Blocking || en[0].Session != "web" {
		t.Errorf("stop card = %+v", en[0])
	}
}

func TestFeedIsCapped(t *testing.T) {
	s := newFeedStore()
	for i := 0; i < feedMaxItems+7; i++ {
		s.add(&feedEntry{item: FeedItem{RequestID: string(rune('a'+i%26)) + "x", State: statePassive}})
	}
	if n := len(s.list("en")); n != feedMaxItems {
		t.Fatalf("feed holds %d, want %d", n, feedMaxItems)
	}
}

func TestStuckSubscriberIsDropped(t *testing.T) {
	h := newEventHub()
	sub := h.add("en")
	for i := 0; i < subBuffer+1; i++ {
		h.publish("x", same(i))
	}
	if h.count() != 0 {
		t.Fatal("a subscriber that never reads must be dropped, not waited for")
	}
	drained := 0
	for range sub.ch {
		drained++
	}
	if drained != subBuffer {
		t.Errorf("drained %d, want %d then a closed channel", drained, subBuffer)
	}
	h.remove(sub) // second remove is a no-op, not a double close
}

func TestPolicyRoute(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	if w := do(s, "POST", "/api/v2/term/sessions/web/policy", "owner-token", `{"policy":"block"}`); w.Code != http.StatusBadRequest {
		t.Errorf("invalid policy → %d, want 400", w.Code)
	}
	if w := do(s, "POST", "/api/v2/term/sessions/web/policy", "owner-token", `{"policy":"gate"}`); w.Code != http.StatusNotFound {
		t.Errorf("unknown session → %d, want 404", w.Code)
	}
	e, _, _ := s.hooks.mint("web", "127.0.0.1:1", "")
	s.hooks.add(e)
	if w := do(s, "POST", "/api/v2/term/sessions/web/policy", "owner-token", `{"policy":"gate"}`); w.Code != http.StatusOK {
		t.Errorf("set policy → %d, want 200", w.Code)
	}
	if e.policy != policyGate {
		t.Errorf("policy = %q, want gate", e.policy)
	}
}

func TestCreateTakesAPolicy(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:9")
	if w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"a","policy":"nope"}`); w.Code != http.StatusBadRequest {
		t.Errorf("bad policy on create → %d, want 400", w.Code)
	}
	w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"b","policy":"gate"}`)
	if w.Code != http.StatusCreated || !strings.Contains(w.Body.String(), `"policy":"gate"`) {
		t.Fatalf("create with gate → %d %s", w.Code, w.Body.String())
	}
	w = do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"c"}`)
	if !strings.Contains(w.Body.String(), `"policy":"none"`) {
		t.Errorf("default policy must be none: %s", w.Body.String())
	}
}

func TestDecideRoute(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	if w := do(s, "POST", "/api/v2/feed/nope/decide", "owner-token", `{"decision":"allow"}`); w.Code != http.StatusNotFound {
		t.Errorf("nothing pending → %d, want 404", w.Code)
	}
	if w := do(s, "POST", "/api/v2/feed/nope/decide", "owner-token", `{"decision":"maybe"}`); w.Code != http.StatusBadRequest {
		t.Errorf("bad decision → %d, want 400", w.Code)
	}
	e, _, _ := s.hooks.mint("web", "127.0.0.1:1", "")
	e.policy = policyGate
	s.hooks.add(e)
	tg := matched(t, s.hooks, e)
	got := make(chan map[string]any, 1)
	go func() { got <- pushAsync(tg, permission(e, "g9", nil)) }()
	deadline := time.Now().Add(time.Second)
	for len(s.hooks.feed.pendingFor(e.paneID)) == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if w := do(s, "POST", "/api/v2/feed/g9/decide", "owner-token", `{"decision":"deny"}`); w.Code != http.StatusOK {
		t.Fatalf("decide → %d", w.Code)
	}
	if res := <-got; res["decision"] != "deny" {
		t.Errorf("REST deny → %v", res)
	}
}

// The socket end to end: hello hydrates, events arrive in the asked-for
// language, and a feed.decide frame answers a gate.
func TestEventsSocket(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	e, _, _ := s.hooks.mint("web", "127.0.0.1:1", "")
	e.policy = policyGate
	s.hooks.add(e)
	tg := matched(t, s.hooks, e)
	push(t, tg, map[string]any{"request_id": "s0", "kind": "passive", "subkind": "session-end", "pane_id": e.paneID})

	mux := http.NewServeMux()
	s.RegisterRoutes(mux)
	srv := httptest.NewServer(mux)
	defer srv.Close()
	url := "ws" + strings.TrimPrefix(srv.URL, "http") + "/api/v2/events?lang=he&token=owner-token"
	conn, _, err := websocket.DefaultDialer.Dial(url, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close()
	read := func() frame {
		t.Helper()
		_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
		var f frame
		if err := conn.ReadJSON(&f); err != nil {
			t.Fatalf("read: %v", err)
		}
		return f
	}

	hello := read()
	if hello.Type != "hello" {
		t.Fatalf("first frame = %q, want hello", hello.Type)
	}
	var hd helloData
	_ = json.Unmarshal(hello.Data, &hd)
	if hd.Panes[e.paneID].Session != "web" || hd.Panes[e.paneID].Policy != "gate" ||
		!hd.PaneBriefs[e.paneID].SessionEnded || len(hd.Feed) != 1 {
		t.Fatalf("hello = %s", hello.Data)
	}
	if en := s.hooks.feed.list("en")[0].Title; hd.Feed[0].Title == en {
		t.Errorf("hello feed must be in the subscriber's language (he), got the English title %q", en)
	}

	got := make(chan map[string]any, 1)
	go func() { got <- pushAsync(tg, permission(e, "g7", nil)) }()
	for {
		f := read()
		if f.Type == "feed:item-added" {
			break
		}
	}
	if err := conn.WriteJSON(map[string]string{"type": "feed.decide", "request_id": "g7", "decision": "allow"}); err != nil {
		t.Fatal(err)
	}
	select {
	case res := <-got:
		if res["decision"] != "allow" {
			t.Fatalf("socket allow → %v", res)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("feed.decide over the socket did not answer the gate")
	}
	for {
		if f := read(); f.Type == "feed:item-resolved" {
			break
		}
	}
}

func TestGateCardFallbackTitleIsHumanized(t *testing.T) {
	// Phase 110, seen live: Claude Code's PreToolUse carries tool_name +
	// tool_input, the CLI looks for command/tool, falls back to
	// "agent: pre-tool-use" and the card showed raw JSON.
	payload := map[string]any{"tool_name": "Bash", "tool_input": map[string]any{"command": "date +%s"}}
	title, summary := cardText("pre-tool-use", "agent: pre-tool-use", `{"cwd":"/x"}`, payload, nil, "en")
	if title != "Claude wants to run: Bash" || summary != "date +%s" {
		t.Errorf("got %q / %q", title, summary)
	}
	// A title the CLI did derive stays the approval prompt.
	title, summary = cardText("pre-tool-use", "Run `ls` ?", "s", payload, nil, "en")
	if title != "Run `ls` ?" || summary != "s" {
		t.Errorf("derived title changed: %q / %q", title, summary)
	}
}
