package desktop

// The tests run a FAKE DESKTOP: a Go implementation of the Rust server half
// (`crates/ymux-tunnel/src/lib.rs`) written straight from the wire spec. That
// is the point — two independent implementations of the handshake have to
// agree, so a drift in either direction shows up here rather than on a live
// box where the only symptom is "the approval card never appears".

import (
	"bufio"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// fakeDesktop speaks the server half on 127.0.0.1. tag is the dialect it opens
// with ("YMUX" or "WINMUX"); reply is called with the decoded JSON-RPC method
// and returns the `result` object. A nil reply answers nothing and closes.
func fakeDesktop(t *testing.T, tag, token string, reply func(method string) any) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { _ = ln.Close() })

	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		r := bufio.NewReader(conn)

		nonce := []byte{0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04}
		fmt.Fprintf(conn, "%s-CHALLENGE %s\n", tag, hex.EncodeToString(nonce))

		line, err := r.ReadString('\n')
		if err != nil {
			return
		}
		want := hmac.New(sha256.New, []byte(token))
		want.Write(nonce)
		expect := fmt.Sprintf("%s-RESPONSE %s", tag, hex.EncodeToString(want.Sum(nil)))
		if strings.TrimSpace(line) != expect {
			fmt.Fprintf(conn, "%s-DENIED bad-hmac\n", tag)
			return
		}
		fmt.Fprintf(conn, "%s-OK\n", tag)

		if reply == nil {
			return
		}
		reqLine, err := r.ReadString('\n')
		if err != nil {
			return
		}
		var req struct {
			Method string `json:"method"`
		}
		_ = json.Unmarshal([]byte(reqLine), &req)
		out, _ := json.Marshal(map[string]any{
			"jsonrpc": "2.0", "id": 1, "result": reply(req.Method),
		})
		conn.Write(append(out, '\n'))
	}()
	return ln.Addr().String()
}

// homeWith writes a last.env naming addr/token and returns the fake home.
func homeWith(t *testing.T, body string) string {
	t.Helper()
	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, ".ymux", "run"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(envFile(home), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return home
}

// clearEnv makes Discover deterministic — the real process environment must not
// leak into a test that is about the FILE.
func clearEnv(t *testing.T) {
	t.Helper()
	for _, k := range []string{
		"YMUX_SOCKET_ADDR", "WINMUX_SOCKET_ADDR",
		"YMUX_TUNNEL_TOKEN", "WINMUX_TUNNEL_TOKEN",
	} {
		t.Setenv(k, "")
	}
}

func TestParseEnvFile(t *testing.T) {
	got := parseEnvFile("# comment\n\nYMUX_SOCKET_ADDR=127.0.0.1:8765\n  \nYMUX_TUNNEL_TOKEN = abc \nBROKEN\n")
	if got["YMUX_SOCKET_ADDR"] != "127.0.0.1:8765" {
		t.Errorf("addr = %q", got["YMUX_SOCKET_ADDR"])
	}
	if got["YMUX_TUNNEL_TOKEN"] != "abc" {
		t.Errorf("token = %q (surrounding spaces must be trimmed)", got["YMUX_TUNNEL_TOKEN"])
	}
	if _, ok := got["BROKEN"]; ok {
		t.Error("a line with no = should be skipped")
	}
}

func TestDiscoverReadsTheFile(t *testing.T) {
	// The daemon is a long-running service started independently of any SSH
	// session, so the FILE — not the environment — is its normal source.
	clearEnv(t)
	home := homeWith(t, "YMUX_SOCKET_ADDR=127.0.0.1:9\nYMUX_TUNNEL_TOKEN=tok\n")
	ep, ok := Discover(home)
	if !ok || ep.Addr != "127.0.0.1:9" || ep.Token != "tok" {
		t.Fatalf("Discover = %+v, %v", ep, ok)
	}
}

func TestDiscoverAcceptsLegacySpelling(t *testing.T) {
	// A remote provisioned before the winmux→ymux rename still writes the old
	// names until its next bootstrap — and those are the installs least likely
	// to be updated soon.
	clearEnv(t)
	home := homeWith(t, "WINMUX_SOCKET_ADDR=127.0.0.1:10\nWINMUX_TUNNEL_TOKEN=old\n")
	ep, ok := Discover(home)
	if !ok || ep.Addr != "127.0.0.1:10" || ep.Token != "old" {
		t.Fatalf("Discover = %+v, %v", ep, ok)
	}
}

func TestDiscoverEnvWinsOverFile(t *testing.T) {
	clearEnv(t)
	t.Setenv("YMUX_SOCKET_ADDR", "127.0.0.1:11")
	t.Setenv("YMUX_TUNNEL_TOKEN", "fresh")
	home := homeWith(t, "YMUX_SOCKET_ADDR=127.0.0.1:12\nYMUX_TUNNEL_TOKEN=stale\n")
	ep, _ := Discover(home)
	if ep.Addr != "127.0.0.1:11" || ep.Token != "fresh" {
		t.Errorf("env should win, got %+v", ep)
	}
}

func TestDiscoverMissingIsNotAnError(t *testing.T) {
	clearEnv(t)
	if _, ok := Discover(t.TempDir()); ok {
		t.Error("Discover succeeded with no env and no file")
	}
	// Half a config is no config: dialling with an empty token would fail the
	// handshake and look like a rejection rather than a missing desktop.
	half := homeWith(t, "YMUX_SOCKET_ADDR=127.0.0.1:13\n")
	if _, ok := Discover(half); ok {
		t.Error("Discover succeeded with an address but no token")
	}
}

func TestCallCompletesTheHandshake(t *testing.T) {
	addr := fakeDesktop(t, "YMUX", "s3cret", func(method string) any {
		return map[string]any{"method_seen": method, "ok": true}
	})
	raw, err := Call(Endpoint{Addr: addr, Token: "s3cret"}, "ping", map[string]any{}, 5*time.Second)
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var got struct {
		MethodSeen string `json:"method_seen"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if got.MethodSeen != "ping" {
		t.Errorf("server saw method %q, want ping", got.MethodSeen)
	}
}

func TestCallMirrorsTheLegacyTag(t *testing.T) {
	// The desktop still OPENS with the legacy tag on purpose (CHALLENGE_TAG in
	// ymux-tunnel), so this is the path that actually runs in production today.
	// The client must answer on the tag it was addressed in.
	addr := fakeDesktop(t, "WINMUX", "s3cret", func(string) any { return map[string]any{"ok": true} })
	if _, err := Call(Endpoint{Addr: addr, Token: "s3cret"}, "ping", nil, 5*time.Second); err != nil {
		t.Fatalf("legacy-tag handshake failed: %v", err)
	}
}

func TestCallRejectsAWrongToken(t *testing.T) {
	addr := fakeDesktop(t, "YMUX", "right", func(string) any { return nil })
	_, err := Call(Endpoint{Addr: addr, Token: "wrong"}, "ping", nil, 5*time.Second)
	if err == nil {
		t.Fatal("Call succeeded with the wrong token")
	}
	if !strings.Contains(err.Error(), "denied") {
		t.Errorf("error %q should say the desktop denied it", err)
	}
}

func TestCallOnADeadEndpoint(t *testing.T) {
	// The desktop being closed is the normal case, not an exceptional one.
	_, err := Call(Endpoint{Addr: "127.0.0.1:1", Token: "t"}, "ping", nil, time.Second)
	if err == nil {
		t.Fatal("Call succeeded against a dead address")
	}
}

func TestAskApprovalReturnsTheDecision(t *testing.T) {
	clearEnv(t)
	addr := fakeDesktop(t, "WINMUX", "tok", func(method string) any {
		if method != "feed.push" {
			t.Errorf("method = %q, want feed.push", method)
		}
		return map[string]any{"request_id": "req1", "decision": "allow"}
	})
	home := homeWith(t, "YMUX_SOCKET_ADDR="+addr+"\nYMUX_TUNNEL_TOKEN=tok\n")

	got, err := AskApproval(home, "req1", "A browser wants access", "code 4821 from 1.2.3.4", nil, 5)
	if err != nil {
		t.Fatalf("AskApproval: %v", err)
	}
	if got != Allow {
		t.Errorf("decision = %q, want allow", got)
	}
}

func TestAskApprovalPassesADenial(t *testing.T) {
	clearEnv(t)
	addr := fakeDesktop(t, "YMUX", "tok", func(string) any {
		return map[string]any{"decision": "deny"}
	})
	home := homeWith(t, "YMUX_SOCKET_ADDR="+addr+"\nYMUX_TUNNEL_TOKEN=tok\n")
	got, err := AskApproval(home, "req1", "t", "s", nil, 5)
	if err != nil || got != Deny {
		t.Fatalf("got (%q, %v), want (deny, nil)", got, err)
	}
}

func TestAskApprovalRefusesAPassiveReply(t *testing.T) {
	// "passive" means the desktop did not treat the push as blocking — an
	// older build that does not know the kind. Anything but a real decision
	// must be an error, because the only other option is guessing, and the
	// guess would be "allow".
	clearEnv(t)
	addr := fakeDesktop(t, "YMUX", "tok", func(string) any {
		return map[string]any{"decision": "passive"}
	})
	home := homeWith(t, "YMUX_SOCKET_ADDR="+addr+"\nYMUX_TUNNEL_TOKEN=tok\n")
	if _, err := AskApproval(home, "req1", "t", "s", nil, 5); err == nil {
		t.Fatal("a passive reply was accepted; it must never read as approval")
	}
}

func TestAskApprovalWithNoDesktop(t *testing.T) {
	clearEnv(t)
	_, err := AskApproval(t.TempDir(), "req1", "t", "s", nil, 5)
	if err != ErrNoEndpoint {
		t.Fatalf("err = %v, want ErrNoEndpoint", err)
	}
}

func TestApprovalIsSentAsABlockingPermissionRequest(t *testing.T) {
	// `kind: "permission_request"` is the exact string the desktop keys on to
	// make a card blocking, and the subkind must NOT be "pre-tool-use" (that
	// one is routed through the agent policy engine, which is the wrong judge
	// of a pairing request). Both are load-bearing, so assert the params.
	clearEnv(t)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	seen := make(chan map[string]any, 1)
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		r := bufio.NewReader(conn)
		nonce := []byte{1, 2, 3}
		fmt.Fprintf(conn, "YMUX-CHALLENGE %s\n", hex.EncodeToString(nonce))
		_, _ = r.ReadString('\n')
		fmt.Fprint(conn, "YMUX-OK\n")
		line, _ := r.ReadString('\n')
		var req struct {
			Params map[string]any `json:"params"`
		}
		_ = json.Unmarshal([]byte(line), &req)
		seen <- req.Params
		fmt.Fprint(conn, `{"jsonrpc":"2.0","id":1,"result":{"decision":"allow"}}`+"\n")
	}()

	home := homeWith(t, "YMUX_SOCKET_ADDR="+ln.Addr().String()+"\nYMUX_TUNNEL_TOKEN=tok\n")
	if _, err := AskApproval(home, "req9", "title", "summary", map[string]any{"ip": "1.2.3.4"}, 30); err != nil {
		t.Fatalf("AskApproval: %v", err)
	}
	params := <-seen
	if params["kind"] != "permission_request" {
		t.Errorf("kind = %v, want permission_request (the card would not block)", params["kind"])
	}
	if params["subkind"] == "pre-tool-use" {
		t.Error("subkind must not be pre-tool-use — that goes through the policy engine")
	}
	if params["request_id"] != "req9" {
		t.Errorf("request_id = %v", params["request_id"])
	}
}

func TestClampWaitMatchesTheDesktop(t *testing.T) {
	// The desktop clamps wait_timeout_seconds to 1..600 whatever we send, so
	// our local deadline must be computed from the clamped value — otherwise a
	// request for 9999s would give up its socket at 600 and read as a timeout
	// we caused.
	for _, c := range []struct{ in, want int }{{0, 120}, {-5, 120}, {9999, 600}, {600, 600}, {30, 30}, {1, 1}} {
		if got := clampWait(c.in); got != c.want {
			t.Errorf("clampWait(%d) = %d, want %d", c.in, got, c.want)
		}
	}
}
