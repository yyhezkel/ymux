package api

// Terminal ops auth proof: the four huma term ops accept the owner token and a
// device holding shell:attach, and refuse everything else with 401/403 — the
// status codes page.html branches on.

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ymux-server/internal/chat"
	"ymux-server/internal/term"
)

const termOwner = "owner-token"

type termOp struct{ method, path, body string }

// Bodies are deliberately invalid/absent-target so nothing past auth can
// create or kill a real tmux session on a box that has tmux.
var termOps = []termOp{
	{"GET", "/api/v2/term/sessions", ""},
	{"POST", "/api/v2/term/sessions", `{"policy":"bogus"}`},
	{"POST", "/api/v2/term/sessions/nope/rename", `{}`},
	{"DELETE", "/api/v2/term/sessions/nope", ""},
}

func termHandler(t *testing.T) (http.Handler, *chat.ChatAPI) {
	t.Helper()
	store, err := chat.OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(store.Close)
	c := chat.NewChatAPI(nil, store, termOwner)
	c.SetApprovalAsker(func(string, string, string, map[string]any, int) (string, error) {
		return "allow", nil
	})
	h := NewServer(termOwner, 0, Deps{Chat: c, Term: term.NewService(termOwner, t.TempDir())}).Handler()
	return h, c
}

func termDo(h http.Handler, method, path, token, body string) *httptest.ResponseRecorder {
	var r *http.Request
	if body == "" {
		r = httptest.NewRequest(method, path, nil)
	} else {
		r = httptest.NewRequest(method, path, strings.NewReader(body))
		r.Header.Set("Content-Type", "application/json")
	}
	if token != "" {
		r.Header.Set("Authorization", "Bearer "+token)
	}
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	return w
}

// pairDevice walks request → approve → redeem and returns the device id + token.
func pairDevice(t *testing.T, h http.Handler) (id, token string) {
	t.Helper()
	w := termDo(h, "POST", "/api/pairing/request", "", `{"device_name":"t"}`)
	var req map[string]any
	if err := json.Unmarshal(w.Body.Bytes(), &req); err != nil || w.Code != 200 {
		t.Fatalf("pairing request %d: %s", w.Code, w.Body.String())
	}
	ots, _ := req["one_shot_token"].(string)
	for i := 0; ; i++ { // approval is async
		sw := termDo(h, "GET", "/api/pairing/request/status?one_shot_token="+ots, "", "")
		if strings.Contains(sw.Body.String(), `"pending"`) {
			break
		}
		if i > 200 {
			t.Fatalf("never approved: %s", sw.Body.String())
		}
		time.Sleep(10 * time.Millisecond)
	}
	rw := termDo(h, "POST", "/api/pairing/redeem", "", `{"one_shot_token":"`+ots+`"}`)
	var red struct {
		DeviceID      string `json:"device_id"`
		LongTermToken string `json:"long_term_token"`
	}
	if err := json.Unmarshal(rw.Body.Bytes(), &red); err != nil || red.LongTermToken == "" {
		t.Fatalf("redeem %d: %s", rw.Code, rw.Body.String())
	}
	return red.DeviceID, red.LongTermToken
}

func TestTermOpsAcceptDeviceToken(t *testing.T) {
	// Pins: a device the owner granted shell:attach reaches the term handlers
	// (anything but 401/403; no tmux on CI means 500 past auth is fine). Breaking
	// it means paired browsers can no longer open a terminal.
	h, _ := termHandler(t)
	id, tok := pairDevice(t, h)
	pw := termDo(h, "PUT", "/api/v2/devices/"+id+"/scopes", termOwner, `{"scopes":["all","shell:attach"]}`)
	if pw.Code != 200 {
		t.Fatalf("owner set scopes: %d %s", pw.Code, pw.Body.String())
	}
	for _, op := range termOps {
		for _, who := range []string{termOwner, tok} {
			if w := termDo(h, op.method, op.path, who, op.body); w.Code == http.StatusUnauthorized || w.Code == http.StatusForbidden {
				t.Errorf("%s %s with granted token: %d", op.method, op.path, w.Code)
			}
		}
	}
}

func TestTermOpsRejectBadToken(t *testing.T) {
	// Pins: no/unknown token → 401 on every op; a paired device with the
	// default "all" grant (which excludes shell:attach) → 403 on every op.
	h, _ := termHandler(t)
	_, tok := pairDevice(t, h)
	for _, op := range termOps {
		for _, bad := range []string{"", "wrong"} {
			if w := termDo(h, op.method, op.path, bad, op.body); w.Code != http.StatusUnauthorized {
				t.Errorf("%s %s token %q: want 401 got %d", op.method, op.path, bad, w.Code)
			}
		}
		if w := termDo(h, op.method, op.path, tok, op.body); w.Code != 403 {
			t.Errorf("%s %s ungranted device: want 403 got %d", op.method, op.path, w.Code)
		}
	}
}
