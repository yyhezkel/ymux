package term

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestDiagPageIsServedPublicly(t *testing.T) {
	// Public on purpose: the page is currently the only way to pair a browser
	// at all, so gating it behind a credential the browser does not yet have
	// would be a closed loop.
	s, _ := testService(ok(""))
	for _, path := range []string{"/", "/diag"} {
		w := do(s, "GET", path, "", "")
		if w.Code != http.StatusOK {
			t.Errorf("GET %s = %d, want 200", path, w.Code)
		}
		if ct := w.Header().Get("Content-Type"); !strings.HasPrefix(ct, "text/html") {
			t.Errorf("GET %s content-type = %q", path, ct)
		}
		if !strings.Contains(w.Body.String(), "ymux") {
			t.Errorf("GET %s did not return the page", path)
		}
		if w.Header().Get("Cache-Control") != "no-store" {
			t.Errorf("GET %s must not be cacheable — a stale copy debugs the wrong build", path)
		}
	}
}

func TestRootIsExactNotACatchAll(t *testing.T) {
	// `/{$}` must not swallow unknown paths: a 404 that silently returns HTML
	// is how a typo in an API path becomes an hour of confusion.
	s, _ := testService(ok(""))
	if w := do(s, "GET", "/nope", "", ""); w.Code != http.StatusNotFound {
		t.Errorf("GET /nope = %d, want 404", w.Code)
	}
}

func TestDiagLogAcceptsABrowserLine(t *testing.T) {
	s, _ := testService(ok(""))
	w := do(s, "POST", "/diag/log", "", `{"level":"info","step":"pair.request","detail":"code=1 2"}`)
	if w.Code != http.StatusNoContent {
		t.Errorf("got %d, want 204", w.Code)
	}
}

func TestDiagLogIgnoresJunk(t *testing.T) {
	// The sink must never be a source of errors for the page: a malformed line
	// is dropped, not reported, or a logging failure becomes its own log storm.
	s, _ := testService(ok(""))
	for _, body := range []string{``, `not json`, `{}`, `{"level":"info","step":""}`} {
		if w := do(s, "POST", "/diag/log", "", body); w.Code != http.StatusNoContent {
			t.Errorf("body %q returned %d, want 204", body, w.Code)
		}
	}
}

func TestDiagLogBudget(t *testing.T) {
	b := &logBudget{}
	now := time.Now()
	for i := 0; i < diagLogPerMinute; i++ {
		if !b.allow(now) {
			t.Fatalf("line %d refused inside the budget", i)
		}
	}
	if b.allow(now) {
		t.Error("the budget was exceeded without a refusal")
	}
	if !b.allow(now.Add(time.Minute + time.Second)) {
		t.Error("the window never refilled")
	}
}

func TestClipStripsControlCharacters(t *testing.T) {
	// A browser-supplied step or detail lands in the log FILE. A newline in it
	// would forge an extra line, which is how a log stops being evidence.
	if got := clip("pair\nrequest\r\x00", 60); strings.ContainsAny(got, "\n\r\x00") {
		t.Errorf("clip left control characters in %q", got)
	}
	if got := clip(strings.Repeat("x", 500), 60); len(got) != 60 {
		t.Errorf("clip length = %d, want 60", len(got))
	}
	if got := clip("  spaced  ", 60); got != "spaced" {
		t.Errorf("clip = %q, want trimmed", got)
	}
}

func TestClientIPPrefersTheProxyHeader(t *testing.T) {
	// The daemon sits behind nginx, so RemoteAddr is always 127.0.0.1 and the
	// header is the only useful value — for DISPLAY and rate limiting, which
	// is exactly why it must never authorise anything.
	r := httptest.NewRequest("GET", "/", nil)
	r.Header.Set("X-Real-IP", "203.0.113.9")
	if got := clientIP(r); got != "203.0.113.9" {
		t.Errorf("clientIP = %q", got)
	}

	r2 := httptest.NewRequest("GET", "/", nil)
	r2.Header.Set("X-Forwarded-For", "203.0.113.9, 10.0.0.1")
	if got := clientIP(r2); got != "203.0.113.9" {
		t.Errorf("clientIP from X-Forwarded-For = %q, want the first hop", got)
	}

	r3 := httptest.NewRequest("GET", "/", nil)
	if clientIP(r3) == "" {
		t.Error("clientIP returned empty with no headers")
	}
}

func TestPageReferencesTheRoutesItCalls(t *testing.T) {
	// The page is embedded, so a renamed route breaks it silently — nothing
	// else links the two. Cheap guard: the HTML must mention every endpoint it
	// depends on, and these strings are the ones the handlers register.
	page := string(diagPage)
	for _, route := range []string{
		"/api/pairing/request",
		"/api/pairing/request/status",
		"/api/pairing/redeem",
		"/api/v2/term/sessions",
		"/attach?token=",
		"/diag/log",
	} {
		if !strings.Contains(page, route) {
			t.Errorf("the diagnostic page no longer references %q", route)
		}
	}
}
