package term

// The gate is the security boundary of this package: everything behind it is a
// shell on the machine. These tests are the executable statement of that rule.

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"ymux-server/internal/auth"
)

// testService builds a Service with a fake tmux and a two-device resolver.
func testService(respond func(args []string) ([]byte, error)) (*Service, *[][]string) {
	tm, calls := fake(respond)
	s := &Service{tmux: tm, token: "owner-token", home: "/nonexistent"}
	s.SetScopeResolver(func(tok string) (string, bool, bool) {
		switch tok {
		case "device-all":
			return "all", false, true // the fail-open default a plain pairing gets
		case "device-shell":
			return `["shell:attach"]`, false, true
		case "device-narrow":
			return `["insights:read"]`, false, true
		}
		return "", false, false
	})
	return s, calls
}

func do(s *Service, method, path, token string, body string) *httptest.ResponseRecorder {
	mux := http.NewServeMux()
	s.RegisterRoutes(mux)
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

func TestGateRequiresShellAttach(t *testing.T) {
	s, _ := testService(ok(""))

	cases := []struct {
		token string
		want  int
		why   string
	}{
		{"owner-token", http.StatusOK, "the owner/desktop token always passes"},
		{"device-shell", http.StatusOK, "a device granted shell:attach explicitly"},
		{"device-all", http.StatusForbidden,
			`"all" must NOT imply shell:attach — that is the whole point of keeping it out of AllScopes`},
		{"device-narrow", http.StatusForbidden, "a device with other grants but not this one"},
		{"nonsense", http.StatusUnauthorized, "an unknown token"},
		{"", http.StatusUnauthorized, "no token at all"},
	}
	for _, c := range cases {
		w := do(s, "GET", "/api/v2/term/sessions", c.token, "")
		if w.Code != c.want {
			t.Errorf("token %q: got %d, want %d — %s", c.token, w.Code, c.want, c.why)
		}
	}
}

func TestGateFailsClosedWithoutConfig(t *testing.T) {
	// A Service with neither a shared token nor a resolver rejects everything.
	// The workspace subsystem treats that state as "no auth configured ⇒ open";
	// here it must not, because the thing behind the door is a shell.
	tm, _ := fake(ok(""))
	s := &Service{tmux: tm}
	if w := do(s, "GET", "/api/v2/term/sessions", "anything", ""); w.Code != http.StatusUnauthorized {
		t.Errorf("unconfigured service returned %d, want 401", w.Code)
	}
}

func TestGateCoversEveryRoute(t *testing.T) {
	// Listing is gated too: session names carry project and branch names, and a
	// token that cannot open a terminal has no reason to enumerate them.
	s, calls := testService(ok(""))
	routes := []struct{ method, path string }{
		{"GET", "/api/v2/term/sessions"},
		{"POST", "/api/v2/term/sessions"},
		{"POST", "/api/v2/term/sessions/api/rename"},
		{"DELETE", "/api/v2/term/sessions/api"},
		{"GET", "/api/v2/term/sessions/api/attach"},
	}
	for _, rt := range routes {
		w := do(s, rt.method, rt.path, "device-all", "")
		if w.Code != http.StatusForbidden {
			t.Errorf("%s %s with an ungranted token returned %d, want 403",
				rt.method, rt.path, w.Code)
		}
	}
	if len(*calls) != 0 {
		t.Errorf("a rejected request still reached tmux: %v", *calls)
	}
}

func TestListReturnsAnnotatedSessions(t *testing.T) {
	s, _ := testService(ok("api\x1f2\x1f1700000000\x1f0\x1f/srv\n"))
	w := do(s, "GET", "/api/v2/term/sessions", "owner-token", "")
	if w.Code != http.StatusOK {
		t.Fatalf("got %d, want 200", w.Code)
	}
	var got []Annotated
	if err := json.Unmarshal(w.Body.Bytes(), &got); err != nil {
		t.Fatalf("decode: %v (body %s)", err, w.Body.String())
	}
	if len(got) != 1 || got[0].Name != "api" || got[0].Display != "api" {
		t.Errorf("unexpected body: %+v", got)
	}
}

func TestCreateRejectsDuplicate(t *testing.T) {
	// has-session succeeds ⇒ the name is taken ⇒ 409, not tmux's 500.
	s, _ := testService(ok(""))
	w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"api"}`)
	if w.Code != http.StatusConflict {
		t.Errorf("got %d, want 409", w.Code)
	}
}

func TestCreateGeneratesAName(t *testing.T) {
	s, calls := testService(func(args []string) ([]byte, error) {
		if args[0] == "has-session" {
			return nil, exitErr() // free
		}
		return nil, nil
	})
	w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{}`)
	if w.Code != http.StatusCreated {
		t.Fatalf("got %d, want 201 (body %s)", w.Code, w.Body.String())
	}
	var body struct{ Name string }
	_ = json.Unmarshal(w.Body.Bytes(), &body)
	if !strings.HasPrefix(body.Name, "ymux-") {
		t.Errorf("generated name %q, want a ymux- prefix", body.Name)
	}
	if !ValidName(body.Name) {
		t.Errorf("generated name %q is not addressable by tmux", body.Name)
	}
	last := (*calls)[len(*calls)-1]
	if last[1] != "new-session" {
		t.Errorf("called %v, want new-session", last)
	}
}

func TestKillMissingSessionIs404(t *testing.T) {
	s, _ := testService(func(args []string) ([]byte, error) {
		if args[0] == "has-session" {
			return nil, exitErr()
		}
		return nil, nil
	})
	if w := do(s, "DELETE", "/api/v2/term/sessions/gone", "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("got %d, want 404", w.Code)
	}
}

func TestAttachRejectsUnknownSessionBeforeUpgrading(t *testing.T) {
	// A 404 is honest; a socket that opens and instantly dies is not.
	s, _ := testService(func(args []string) ([]byte, error) {
		if args[0] == "has-session" {
			return nil, exitErr()
		}
		return nil, nil
	})
	if w := do(s, "GET", "/api/v2/term/sessions/gone/attach", "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("got %d, want 404", w.Code)
	}
}

func TestBearerAcceptsQueryToken(t *testing.T) {
	// A browser cannot set headers on a WebSocket handshake, so ?token= has to
	// work — for the attach route at least.
	s, _ := testService(ok(""))
	mux := http.NewServeMux()
	s.RegisterRoutes(mux)
	r := httptest.NewRequest("GET", "/api/v2/term/sessions?token=owner-token", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, r)
	if w.Code != http.StatusOK {
		t.Errorf("query-string token returned %d, want 200", w.Code)
	}
}

func TestShellAttachIsNotInAllScopes(t *testing.T) {
	// Belt and braces at the package that defines the vocabulary: if someone
	// ever adds ScopeShellAttach to AllScopes, ParseScopes' fail-open default
	// would hand a shell to every device paired before this existed.
	for _, s := range auth.AllScopes {
		if s == auth.ScopeShellAttach {
			t.Fatal("shell:attach is in AllScopes — every unrestricted device would get a shell")
		}
	}
	if auth.HasScope("all", auth.ScopeShellAttach) {
		t.Error(`HasScope("all", shell:attach) = true, want false`)
	}
	if auth.HasScope("", auth.ScopeShellAttach) {
		t.Error(`HasScope("", shell:attach) = true, want false`)
	}
	if !auth.HasScope(`["shell:attach"]`, auth.ScopeShellAttach) {
		t.Error("an explicit grant was not honoured")
	}
}
