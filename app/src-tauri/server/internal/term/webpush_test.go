package term

// Phase 114 (E): which hooks become a browser notification, who gets it,
// and the subscription routes.

import (
	"context"
	"encoding/json"
	"net/http"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"ymux-server/internal/webpush"
)

const testP256dh = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"
const testAuth = "BTBZMqHH6r4Tts7J_aSIgg"

type sentPush struct {
	endpoint string
	payload  map[string]any
	opts     webpush.Options
}

// fakePusher records sends; status answers per endpoint (default 201).
type fakePusher struct {
	mu     sync.Mutex
	sent   []sentPush
	status map[string]int
}

func (f *fakePusher) send(_ context.Context, sub webpush.Subscription, p []byte, o webpush.Options) (int, error) {
	var m map[string]any
	_ = json.Unmarshal(p, &m)
	f.mu.Lock()
	defer f.mu.Unlock()
	f.sent = append(f.sent, sentPush{sub.Endpoint, m, o})
	if c, ok := f.status[sub.Endpoint]; ok {
		return c, nil
	}
	return http.StatusCreated, nil
}

// waitSent waits until n sends were recorded and returns them.
func (f *fakePusher) waitSent(t *testing.T, n int) []sentPush {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		f.mu.Lock()
		if len(f.sent) >= n {
			out := append([]sentPush(nil), f.sent...)
			f.mu.Unlock()
			return out
		}
		f.mu.Unlock()
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("want %d sends, got %d", n, f.count())
	return nil
}

func (f *fakePusher) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.sent)
}

func record(dev, lang, ep string) webpush.Record {
	return webpush.Record{DeviceID: dev, Lang: lang,
		Sub: webpush.Subscription{Endpoint: ep, P256dh: testP256dh, Auth: testAuth}}
}

// pushService is hookService with Web Push on: dev_ok holds shell:attach,
// dev_noshell does not, anything else is revoked.
func pushService(t *testing.T) (*Service, *fakePusher) {
	t.Helper()
	s, _ := hookService("tmux 3.4")
	keys, err := webpush.LoadOrCreateKeys(filepath.Join(t.TempDir(), "vapid.pem"))
	if err != nil {
		t.Fatal(err)
	}
	f := &fakePusher{status: map[string]int{}}
	s.scopes = func(tok string) (string, bool, bool) {
		switch tok {
		case "tok_ok":
			return `["shell:attach"]`, false, true
		case "tok_noshell":
			return `["insights:read"]`, false, true
		}
		return "", false, false
	}
	s.enableWebPush(&webPusher{
		keys:  keys,
		store: webpush.NewStore(filepath.Join(t.TempDir(), "webpush.json")),
		deviceOf: func(tok string) (string, bool, bool) {
			switch tok {
			case "tok_ok":
				return "dev_ok", false, true
			case "tok_noshell":
				return "dev_noshell", false, true
			}
			return "", false, false
		},
		scopesByID: func(id string) (string, bool) {
			switch id {
			case "dev_ok":
				return `["shell:attach"]`, true
			case "dev_noshell":
				return `["insights:read"]`, true
			}
			return "", false
		},
		send: f.send,
	})
	return s, f
}

func TestGateNotifiesAllowedBrowsersOnly(t *testing.T) {
	s, f := pushService(t)
	st := s.webpush.store
	_ = st.Put(record("dev_ok", "he", "https://p/ok"))
	_ = st.Put(record(ownerDevice, "en", "https://p/owner"))
	_ = st.Put(record("dev_revoked", "en", "https://p/revoked"))
	_ = st.Put(record("dev_noshell", "en", "https://p/noshell"))

	e, _, _ := s.hooks.mint("web", "127.0.0.1:1", "")
	e.policy = policyGate
	s.hooks.add(e)
	tg := matched(t, s.hooks, e)
	got := make(chan map[string]any, 1)
	go func() { got <- pushAsync(tg, permission(e, "g1", nil)) }()

	sent := f.waitSent(t, 2)
	// The drops come after the sends in the same pass; wait for them.
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if recs, _ := st.All(); len(recs) == 2 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if n := f.count(); n != 2 {
		t.Fatalf("sent %d, want 2 (dev_ok + owner)", n)
	}
	for _, p := range sent {
		if p.payload["kind"] != noteGate || p.payload["request_id"] != "g1" || p.payload["pane_id"] != e.paneID {
			t.Errorf("payload %v", p.payload)
		}
		if p.opts.Urgency != "high" || p.opts.Topic != "g1" || p.opts.TTL != 10*time.Minute {
			t.Errorf("options %+v", p.opts)
		}
	}
	if recs, _ := st.All(); len(recs) != 2 {
		t.Errorf("revoked + no-shell records must be dropped, %d left", len(recs))
	}
	s.hooks.decide("g1", "allow", "test")
	<-got
}

func TestAttentionAndDoneNotify(t *testing.T) {
	s, f := pushService(t)
	_ = s.webpush.store.Put(record(ownerDevice, "en", "https://p/owner"))
	e, _, _ := s.hooks.mint("web", "127.0.0.1:1", "")
	e.policy = policyNone
	s.hooks.add(e)
	tg := matched(t, s.hooks, e)

	push(t, tg, map[string]any{"request_id": "n0", "kind": "passive", "subkind": "notification",
		"pane_id": e.paneID, "payload": map[string]any{"notification_type": "idle_prompt"}})
	push(t, tg, map[string]any{"request_id": "n1", "kind": "passive", "subkind": "notification",
		"pane_id": e.paneID, "payload": map[string]any{"notification_type": "permission_prompt",
			"message": "Claude needs your permission to use Bash"}})
	first := f.waitSent(t, 1)[0]
	if first.payload["kind"] != noteAttention || first.payload["body"] != "Claude needs your permission to use Bash" {
		t.Fatalf("attention payload %v (idle_prompt must not notify)", first.payload)
	}
	// A prompt never notifies; a stop does.
	push(t, tg, map[string]any{"request_id": "u", "kind": "passive", "subkind": "user-prompt-submit",
		"pane_id": e.paneID, "payload": map[string]any{"prompt": "hi"}})
	push(t, tg, map[string]any{"request_id": "s1", "kind": "passive", "subkind": "stop",
		"pane_id": e.paneID, "payload": map[string]any{"cwd": "/srv/app"}})
	sent := f.waitSent(t, 2)
	if sent[1].payload["kind"] != noteDone || sent[1].opts.Urgency != "normal" {
		t.Fatalf("done payload %v %+v", sent[1].payload, sent[1].opts)
	}
	if len(sent) != 2 {
		t.Fatalf("sent %d, want 2", len(sent))
	}
}

func TestExpiredSubscriptionIsDropped(t *testing.T) {
	s, f := pushService(t)
	f.status["https://p/gone"] = http.StatusGone
	_ = s.webpush.store.Put(record("dev_ok", "en", "https://p/gone"))
	if n := s.webpush.deliver(webNote{Kind: "test"}, ""); n != 0 {
		t.Fatalf("a 410 counted as sent")
	}
	if recs, _ := s.webpush.store.All(); len(recs) != 0 {
		t.Fatalf("a 410 subscription was kept")
	}
}

func TestWebPushRoutes(t *testing.T) {
	s, f := pushService(t)
	if w := do(s, "GET", "/api/v2/webpush/key", "tok_ok", ""); w.Code != http.StatusOK {
		t.Fatalf("key → %d", w.Code)
	} else {
		var b map[string]string
		_ = json.Unmarshal(w.Body.Bytes(), &b)
		if b["public_key"] != s.webpush.keys.PublicKey() {
			t.Errorf("key body %v", b)
		}
	}
	if w := do(s, "GET", "/api/v2/webpush/key", "tok_noshell", ""); w.Code != http.StatusForbidden {
		t.Errorf("no shell:attach → %d, want 403", w.Code)
	}
	sub := `{"endpoint":"https://p/1","keys":{"p256dh":"` + testP256dh + `","auth":"` + testAuth + `"},"lang":"he"}`
	if w := do(s, "POST", "/api/v2/webpush/subscriptions", "tok_ok", sub); w.Code != http.StatusOK {
		t.Fatalf("subscribe → %d %s", w.Code, w.Body)
	}
	bad := `{"endpoint":"http://p/1","keys":{"p256dh":"` + testP256dh + `","auth":"` + testAuth + `"}}`
	if w := do(s, "POST", "/api/v2/webpush/subscriptions", "tok_ok", bad); w.Code != http.StatusBadRequest {
		t.Errorf("http endpoint → %d, want 400", w.Code)
	}
	recs, _ := s.webpush.store.All()
	if len(recs) != 1 || recs[0].DeviceID != "dev_ok" || recs[0].Lang != "he" {
		t.Fatalf("stored %+v", recs)
	}
	if w := do(s, "POST", "/api/v2/webpush/test", "tok_ok", ""); w.Code != http.StatusOK {
		t.Fatalf("test → %d", w.Code)
	}
	if sent := f.waitSent(t, 1); sent[0].payload["body"] != "ההתראות עובדות." {
		t.Errorf("test push in the subscriber's language: %v", sent[0].payload)
	}
	// Another caller cannot remove dev_ok's subscription; dev_ok can.
	if w := do(s, "DELETE", "/api/v2/webpush/subscriptions", "owner-token", `{"endpoint":"https://p/1"}`); w.Code != http.StatusOK {
		t.Fatalf("owner delete → %d", w.Code)
	}
	if recs, _ := s.webpush.store.All(); len(recs) != 1 {
		t.Fatal("the owner token removed a device's subscription")
	}
	if w := do(s, "DELETE", "/api/v2/webpush/subscriptions", "tok_ok", `{"endpoint":"https://p/1"}`); w.Code != http.StatusOK {
		t.Fatalf("delete → %d", w.Code)
	}
	if recs, _ := s.webpush.store.All(); len(recs) != 0 {
		t.Fatal("unsubscribe kept the record")
	}
}

func TestWebPushOffIs503(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	if w := do(s, "GET", "/api/v2/webpush/key", "owner-token", ""); w.Code != http.StatusServiceUnavailable {
		t.Errorf("no VAPID key → %d, want 503", w.Code)
	}
}
