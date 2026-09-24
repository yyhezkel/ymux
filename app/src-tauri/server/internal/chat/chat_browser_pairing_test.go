package chat

// Phase 96 — browser-initiated pairing. The property these tests exist to hold
// is narrow and absolute: **a request nobody approved must never become a
// credential.** Everything else here is guard rails around that.

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// newTestAPI builds a ChatAPI over a throwaway store, with an approval asker
// that answers `decision` without a desktop anywhere.
func newTestAPI(t *testing.T, decision string) *ChatAPI {
	t.Helper()
	store, err := OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(store.Close)
	c := NewChatAPI(nil, store, "owner-token")
	if decision != "" {
		c.SetApprovalAsker(func(string, string, string, map[string]any, int) (string, error) {
			return decision, nil
		})
	}
	return c
}

func serve(c *ChatAPI, method, path, token, body string) *httptest.ResponseRecorder {
	mux := http.NewServeMux()
	c.registerBrowserPairingRoutes(mux)
	var r *http.Request
	if body == "" {
		r = httptest.NewRequest(method, path, nil)
	} else {
		r = httptest.NewRequest(method, path, strings.NewReader(body))
	}
	if token != "" {
		r.Header.Set("Authorization", "Bearer "+token)
	}
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, r)
	return w
}

// requestOnce performs a pairing request and returns its decoded body.
func requestOnce(t *testing.T, c *ChatAPI) map[string]any {
	t.Helper()
	w := serve(c, "POST", "/api/pairing/request", "", `{"device_name":"Firefox"}`)
	if w.Code != http.StatusOK {
		t.Fatalf("request returned %d: %s", w.Code, w.Body.String())
	}
	var out map[string]any
	if err := json.Unmarshal(w.Body.Bytes(), &out); err != nil {
		t.Fatalf("decode: %v", err)
	}
	return out
}

func TestRequestedRowCannotBeRedeemed(t *testing.T) {
	// THE test. A request that no human approved is sitting in `requested`,
	// and redeemDevice matches `status='pending'` only — so the one-shot the
	// browser holds is worthless until someone presses Allow. If this ever
	// goes green-to-red, an unauthenticated endpoint mints credentials.
	c := newTestAPI(t, "") // no asker: nothing can approve
	c.SetApprovalAsker(func(string, string, string, map[string]any, int) (string, error) {
		select {} // never answers, so the row stays `requested`
	})
	body := requestOnce(t, c)
	oneShot, _ := body["one_shot_token"].(string)
	if oneShot == "" {
		t.Fatal("no one-shot token issued")
	}
	if _, _, ok := c.Redeem(oneShot, "1.2.3.4"); ok {
		t.Fatal("an UNAPPROVED request was redeemed for a device credential")
	}
}

func TestApprovedRequestRedeemsThroughTheExistingPath(t *testing.T) {
	c := newTestAPI(t, "allow")
	body := requestOnce(t, c)
	oneShot := body["one_shot_token"].(string)

	waitForStatus(t, c, oneShot, "pending")

	id, longTerm, ok := c.Redeem(oneShot, "1.2.3.4")
	if !ok {
		t.Fatal("an approved request could not be redeemed")
	}
	if id == "" || longTerm == "" {
		t.Fatalf("redeem returned id=%q token=%q", id, longTerm)
	}
	if !c.TokenValid(longTerm) {
		t.Error("the long-term token from an approved browser is not accepted")
	}
}

func TestDeniedRequestIsGone(t *testing.T) {
	c := newTestAPI(t, "deny")
	body := requestOnce(t, c)
	oneShot := body["one_shot_token"].(string)

	waitForStatus(t, c, oneShot, "gone")
	if _, _, ok := c.Redeem(oneShot, "1.2.3.4"); ok {
		t.Fatal("a DENIED request was redeemed")
	}
}

func TestTimeoutIsNotApproval(t *testing.T) {
	// A human who never answers must not mean yes.
	c := newTestAPI(t, "timeout")
	body := requestOnce(t, c)
	oneShot := body["one_shot_token"].(string)
	waitForStatus(t, c, oneShot, "gone")
	if _, _, ok := c.Redeem(oneShot, "1.2.3.4"); ok {
		t.Fatal("a TIMED-OUT request was redeemed")
	}
}

func TestApprovalDoesNotGrantAShell(t *testing.T) {
	// Approving says "this browser is mine", not "it may run commands on my
	// machine". The default grant is the ordinary "all", which since Phase 95
	// excludes shell:attach — the terminal is a second, separate decision.
	c := newTestAPI(t, "allow")
	body := requestOnce(t, c)
	oneShot := body["one_shot_token"].(string)
	waitForStatus(t, c, oneShot, "pending")
	_, longTerm, ok := c.Redeem(oneShot, "1.2.3.4")
	if !ok {
		t.Fatal("redeem failed")
	}
	scopes, admin, ok := c.DeviceScopes(longTerm)
	if !ok || admin {
		t.Fatalf("DeviceScopes = (%q, admin=%v, ok=%v)", scopes, admin, ok)
	}
	if scopes != "all" {
		t.Errorf("scopes = %q, want the ordinary %q grant", scopes, "all")
	}
}

func TestRequestWithNoDesktopFailsClosed(t *testing.T) {
	// No asker wired = ymux is not connected. The browser must be told, not
	// left polling a request no human will ever see.
	c := newTestAPI(t, "")
	w := serve(c, "POST", "/api/pairing/request", "", `{}`)
	if w.Code != http.StatusServiceUnavailable {
		t.Fatalf("got %d, want 503", w.Code)
	}
	if !strings.Contains(w.Body.String(), "ymux is not connected") {
		t.Errorf("body %q should say why", w.Body.String())
	}
}

func TestRequestIsRateLimitedPerIP(t *testing.T) {
	c := newTestAPI(t, "allow")
	c.SetApprovalAsker(func(string, string, string, map[string]any, int) (string, error) {
		select {} // park, so rows stay outstanding and nothing races the count
	})
	limited := false
	for i := 0; i < maxRequestsPerIP+2; i++ {
		if serve(c, "POST", "/api/pairing/request", "", `{}`).Code == http.StatusTooManyRequests {
			limited = true
			break
		}
	}
	if !limited {
		t.Fatalf("no 429 after %d requests from one IP", maxRequestsPerIP+2)
	}
}

func TestStatusPollNeedsTheOneShot(t *testing.T) {
	// The one-shot is the browser's proof of being the client that asked.
	// A wrong or absent one must not distinguish "denied" from "never existed",
	// or the endpoint becomes an id oracle.
	c := newTestAPI(t, "allow")
	requestOnce(t, c)
	for _, q := range []string{"", "?one_shot_token=", "?one_shot_token=nonsense"} {
		w := serve(c, "GET", "/api/pairing/request/status"+q, "", "")
		var out map[string]any
		_ = json.Unmarshal(w.Body.Bytes(), &out)
		if out["status"] != "gone" {
			t.Errorf("status%q = %v, want gone", q, out["status"])
		}
	}
}

func TestAdminRoutesRejectANonOwner(t *testing.T) {
	c := newTestAPI(t, "allow")
	for _, rt := range []struct{ method, path string }{
		{"GET", "/api/pairing/requests"},
		{"POST", "/api/pairing/requests/dev_x/approve"},
		{"POST", "/api/pairing/requests/dev_x/deny"},
	} {
		if w := serve(c, rt.method, rt.path, "not-the-owner", ""); w.Code != http.StatusForbidden {
			t.Errorf("%s %s with a bad token = %d, want 403", rt.method, rt.path, w.Code)
		}
	}
}

func TestListRequestsShowsWhatAHumanJudgesBy(t *testing.T) {
	c := newTestAPI(t, "")
	c.SetApprovalAsker(func(string, string, string, map[string]any, int) (string, error) {
		select {}
	})
	requestOnce(t, c)

	w := serve(c, "GET", "/api/pairing/requests", "owner-token", "")
	if w.Code != http.StatusOK {
		t.Fatalf("got %d", w.Code)
	}
	var out struct {
		Requests []map[string]any `json:"requests"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &out); err != nil {
		t.Fatal(err)
	}
	if len(out.Requests) != 1 {
		t.Fatalf("got %d requests, want 1", len(out.Requests))
	}
	r := out.Requests[0]
	for _, k := range []string{"request_id", "code", "ip", "user_agent"} {
		if _, ok := r[k]; !ok {
			t.Errorf("request row is missing %q — the human cannot match without it", k)
		}
	}
	if code, _ := r["code"].(string); len(code) != 7 {
		t.Errorf("code = %q, want 6 digits grouped as \"482 193\"", code)
	}
}

func TestRandCodeIsSixDigits(t *testing.T) {
	// Digits only: the code is read off one screen and compared on another,
	// sometimes in a Hebrew interface, where O/0 and I/1 would be a trap.
	for i := 0; i < 50; i++ {
		got := randCode()
		if len(got) != 7 || got[3] != ' ' {
			t.Fatalf("randCode() = %q, want \"NNN NNN\"", got)
		}
		for j, ch := range got {
			if j == 3 {
				continue
			}
			if ch < '0' || ch > '9' {
				t.Fatalf("randCode() = %q contains a non-digit", got)
			}
		}
	}
}

func TestClipStripsControlCharacters(t *testing.T) {
	// The User-Agent is attacker-controlled and lands on an approval card. A
	// newline in it could forge a second line of that card.
	if got := clip("Mozilla\n\rFake: line\x00", 200); strings.ContainsAny(got, "\n\r\x00") {
		t.Errorf("clip left control characters in %q", got)
	}
	if got := clip(strings.Repeat("x", 500), 64); len(got) != 64 {
		t.Errorf("clip length = %d, want 64", len(got))
	}
}

func TestIPLimiterWindow(t *testing.T) {
	l := newIPLimiter()
	now := time.Now()
	for i := 0; i < maxRequestsPerIP; i++ {
		if !l.allow("1.2.3.4", now) {
			t.Fatalf("attempt %d denied inside the budget", i)
		}
	}
	if l.allow("1.2.3.4", now) {
		t.Error("the budget was exceeded without a refusal")
	}
	if !l.allow("5.6.7.8", now) {
		t.Error("a different IP was caught by another's budget")
	}
	if !l.allow("1.2.3.4", now.Add(requestWindow+time.Second)) {
		t.Error("the window never reopened")
	}
}

// waitForStatus polls the status endpoint until it reports want, because the
// approval runs on its own goroutine.
func waitForStatus(t *testing.T, c *ChatAPI, oneShot, want string) {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	var last string
	for time.Now().Before(deadline) {
		w := serve(c, "GET", "/api/pairing/request/status?one_shot_token="+oneShot, "", "")
		var out map[string]any
		_ = json.Unmarshal(w.Body.Bytes(), &out)
		last, _ = out["status"].(string)
		if last == want {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("status stayed %q, want %q", last, want)
}
