package chat

// Phase 69.C acceptance — exercise the hook RPC server with a Go client that
// replicates the ymux CLI wire format byte-for-byte (HMAC over the RAW
// nonce bytes + JSON-RPC framing). This stands in for a real
// `ymux claude-hook` round-trip, which should also be run on Linux during
// the 67.C integration.

import (
	"bufio"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ymux-server/internal/hooks"
)

func newTestManagerWithRPC(t *testing.T) *SessionManager {
	t.Helper()
	store, err := OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("OpenChatStore: %v", err)
	}
	t.Cleanup(store.Close)
	m := NewSessionManager(store)
	hooks.Start("", m) // listener → SetHookAddr + MatchHookHMAC (Phase 100)
	if m.rpcAddr == "" {
		t.Fatal("rpcAddr not set after hooks.Start")
	}
	return m
}

// registerFakeSession inserts a session into the manager without spawning a
// real claude process.
func registerFakeSession(m *SessionManager, policy string) *Session {
	s := &Session{
		id:           "mob_" + randHex(4),
		mgr:          m,
		policy:       policy,
		rpcToken:     randHex(32),
		status:       stActive,
		subs:         map[int64]*subscriber{},
		pendingHooks: map[string]chan string{},
	}
	m.mu.Lock()
	m.sessions[s.id] = s
	m.paneIndex["mob_"+s.id] = s.id
	m.mu.Unlock()
	_ = m.store.insertSession(&SessionRow{ID: s.id, Status: stActive, Policy: policy})
	return s
}

// cliHookCall performs the full handshake + one feed.push as the CLI would.
func cliHookCall(t *testing.T, addr, token, paneID, kind string, reqID string) (string, error) {
	t.Helper()
	conn, err := net.DialTimeout("tcp", addr, 3*time.Second)
	if err != nil {
		return "", err
	}
	defer conn.Close()
	br := bufio.NewReader(conn)

	line, err := br.ReadString('\n')
	if err != nil {
		return "", err
	}
	// Mirror whichever dialect the server opened with, exactly as the real
	// CLI does — this keeps the test honest across the winmux → ymux tag
	// flip instead of pinning it to today's hooks.ChallengeTag.
	trimmed := strings.TrimSpace(line)
	tag := ""
	for _, candidate := range []string{hooks.TagYmux, hooks.TagLegacy} {
		if strings.HasPrefix(trimmed, candidate+"-CHALLENGE ") {
			tag = candidate
			break
		}
	}
	if tag == "" {
		return "", fmt.Errorf("unrecognised challenge: %q", trimmed)
	}
	nonceHex := strings.TrimSpace(strings.TrimPrefix(trimmed, tag+"-CHALLENGE "))
	nonce, err := hex.DecodeString(nonceHex)
	if err != nil {
		return "", err
	}
	h := hmac.New(sha256.New, []byte(token))
	h.Write(nonce)
	fmt.Fprintf(conn, "%s-RESPONSE %s\n", tag, hex.EncodeToString(h.Sum(nil)))

	ok, err := br.ReadString('\n')
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(ok) != tag+"-OK" {
		return "", fmt.Errorf("handshake verdict: %q", strings.TrimSpace(ok))
	}

	params := map[string]any{
		"request_id": reqID, "kind": kind, "subkind": "pre-tool-use",
		"pane_id": paneID, "title": "Run `ls`?",
		"payload": map[string]any{
			"tool_name":  "Bash",
			"tool_input": map[string]any{"command": "ls"},
		},
		"wait_timeout_seconds": 3,
	}
	req := map[string]any{"jsonrpc": "2.0", "id": 1, "method": "feed.push", "params": params}
	b, _ := json.Marshal(req)
	if _, err := conn.Write(append(b, '\n')); err != nil {
		return "", err
	}
	respLine, err := br.ReadString('\n')
	if err != nil {
		return "", err
	}
	var resp struct {
		Result struct {
			Decision string `json:"decision"`
		} `json:"result"`
	}
	if err := json.Unmarshal([]byte(strings.TrimSpace(respLine)), &resp); err != nil {
		return "", err
	}
	return resp.Result.Decision, nil
}

func TestHookHandshakeRejectsBadToken(t *testing.T) {
	m := newTestManagerWithRPC(t)
	registerFakeSession(m, "gate")
	if _, err := cliHookCall(t, m.rpcAddr, "wrong-token", "mob_x", "permission_request", "req_1"); err == nil {
		t.Fatal("expected handshake failure with wrong token")
	}
}

func TestHookAutoPolicyAllows(t *testing.T) {
	m := newTestManagerWithRPC(t)
	s := registerFakeSession(m, "auto")
	d, err := cliHookCall(t, m.rpcAddr, s.rpcToken, "mob_"+s.id, "permission_request", "req_a")
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	if d != "allow" {
		t.Fatalf("auto policy → %q, want allow", d)
	}
}

func TestHookBlockPolicyDenies(t *testing.T) {
	m := newTestManagerWithRPC(t)
	s := registerFakeSession(m, "block")
	d, _ := cliHookCall(t, m.rpcAddr, s.rpcToken, "mob_"+s.id, "permission_request", "req_b")
	if d != "deny" {
		t.Fatalf("block policy → %q, want deny", d)
	}
}

func TestHookGateNoClientDeniesFast(t *testing.T) {
	m := newTestManagerWithRPC(t)
	s := registerFakeSession(m, "gate") // no subscribers
	start := time.Now()
	d, _ := cliHookCall(t, m.rpcAddr, s.rpcToken, "mob_"+s.id, "permission_request", "req_c")
	if d != "deny" {
		t.Fatalf("gate w/o client → %q, want deny", d)
	}
	if time.Since(start) > 2*time.Second {
		t.Fatal("should deny fast, not wait for timeout")
	}
}

func TestHookGateClientApproves(t *testing.T) {
	m := newTestManagerWithRPC(t)
	s := registerFakeSession(m, "gate")
	sub := s.addSubscriber()
	// Approver: wait for the hook_request, then allow it.
	go func() {
		for ev := range sub.ch {
			var msg map[string]any
			_ = json.Unmarshal(ev, &msg)
			if msg["type"] == "hook_request" {
				s.resolveHook(msg["req_id"].(string), "allow")
				return
			}
		}
	}()
	d, err := cliHookCall(t, m.rpcAddr, s.rpcToken, "mob_"+s.id, "permission_request", "req_d")
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	if d != "allow" {
		t.Fatalf("gate+approve → %q, want allow", d)
	}
}

func TestHookPassiveAcks(t *testing.T) {
	m := newTestManagerWithRPC(t)
	s := registerFakeSession(m, "gate")
	d, _ := cliHookCall(t, m.rpcAddr, s.rpcToken, "mob_"+s.id, "passive", "req_e")
	if d != "passive" {
		t.Fatalf("passive → %q, want passive", d)
	}
}

func TestHookPaneIdMismatchDenies(t *testing.T) {
	m := newTestManagerWithRPC(t)
	s := registerFakeSession(m, "auto")
	// Valid token (session matches) but a mismatched pane_id → deny.
	d, _ := cliHookCall(t, m.rpcAddr, s.rpcToken, "mob_someoneelse", "permission_request", "req_f")
	if d != "deny" {
		t.Fatalf("pane_id mismatch → %q, want deny", d)
	}
}

// handshakeWithTag runs just the challenge/response half, forcing the client
// to answer in `tag` regardless of which dialect the server opened with.
// Returns the verdict line.
func handshakeWithTag(t *testing.T, addr, token, tag string) string {
	t.Helper()
	conn, err := net.DialTimeout("tcp", addr, 3*time.Second)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close()
	br := bufio.NewReader(conn)

	line, err := br.ReadString('\n')
	if err != nil {
		t.Fatalf("read challenge: %v", err)
	}
	trimmed := strings.TrimSpace(line)
	idx := strings.Index(trimmed, "-CHALLENGE ")
	if idx < 0 {
		t.Fatalf("malformed challenge: %q", trimmed)
	}
	nonce, err := hex.DecodeString(trimmed[idx+len("-CHALLENGE "):])
	if err != nil {
		t.Fatalf("decode nonce: %v", err)
	}
	h := hmac.New(sha256.New, []byte(token))
	h.Write(nonce)
	fmt.Fprintf(conn, "%s-RESPONSE %s\n", tag, hex.EncodeToString(h.Sum(nil)))

	verdict, err := br.ReadString('\n')
	if err != nil {
		t.Fatalf("read verdict: %v", err)
	}
	return strings.TrimSpace(verdict)
}

// The winmux → ymux rename leaves both dialects on the wire at once: a remote
// running the pre-rename CLI answers WINMUX-RESPONSE, a current one answers
// YMUX-RESPONSE, and the same daemon serves both. Each must be accepted, and
// the verdict must come back in the dialect the client used — a CLI that only
// knows how to parse "WINMUX-OK" hangs up on anything else.
func TestHookHandshakeAcceptsBothDialects(t *testing.T) {
	m := newTestManagerWithRPC(t)
	for _, tag := range []string{hooks.TagYmux, hooks.TagLegacy} {
		s := registerFakeSession(m, "auto")
		if got, want := handshakeWithTag(t, m.rpcAddr, s.rpcToken, tag), tag+"-OK"; got != want {
			t.Fatalf("dialect %s: verdict = %q, want %q", tag, got, want)
		}
	}
}

// A bad token must be refused in the client's dialect too, so the rejection is
// legible rather than landing as an "unexpected handshake verdict" parse error.
func TestHookHandshakeDeniesInClientDialect(t *testing.T) {
	m := newTestManagerWithRPC(t)
	registerFakeSession(m, "auto")
	for _, tag := range []string{hooks.TagYmux, hooks.TagLegacy} {
		got := handshakeWithTag(t, m.rpcAddr, "not-the-token", tag)
		if !strings.HasPrefix(got, tag+"-DENIED") {
			t.Fatalf("dialect %s: verdict = %q, want %s-DENIED prefix", tag, got, tag)
		}
	}
}
