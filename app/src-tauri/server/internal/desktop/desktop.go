// Package desktop is the daemon's OUTBOUND client to the ymux desktop, over
// the reverse SSH tunnel (Phase 96, docs/WEB-DESIGN.md §7).
//
// Read this first: until now the daemon never dialled the desktop. Every
// desktop→daemon call is a `curl` the DESKTOP opens on an SSH exec channel
// (`pairing.rs::daemon_curl`), and the daemon's only tunnel-facing code is a
// LISTENER (`chat/chat_hookrpc.go`) that the CLI dials inbound. This package is
// the missing direction, and it exists for one reason: a browser asking for
// access should reach Yossi as an ordinary ymux Allow/Deny card that toasts
// with every panel closed, rather than as a row he has to go and find in a tab.
//
// It speaks exactly what the Linux CLI speaks — same endpoint, same HMAC
// challenge-response, same newline-delimited JSON-RPC — so it inherits an
// already-deployed server side rather than adding a protocol. The Rust
// counterparts are `cli/src/main.rs::perform_handshake` (the client half this
// mirrors) and `crates/ymux-tunnel/src/lib.rs` (the server half it talks to).
//
// Rule #8: the tunnel token is a password. It never appears in a log line, and
// it never travels on the wire — only the server's nonce and an HMAC of it do.
package desktop

import (
	"bufio"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"time"

	"ymux-server/internal/logging"
)

var logger = logging.New("SRV:DESK")

// ErrNoEndpoint means no live tunnel was found: the desktop is closed, or it
// has no SSH session to this host. Callers treat it as "not available right
// now", never as a failure of the thing they were doing.
var ErrNoEndpoint = errors.New("no ymux desktop tunnel on this host")

// handshakeBudget bounds the challenge-response. The RPC that follows gets the
// caller's own, much longer, deadline — a blocking approval card is allowed to
// take minutes, an authentication is not.
const handshakeBudget = 10 * time.Second

// Endpoint is a reachable desktop: the tunnel's localhost address and the HMAC
// token that authenticates to it.
type Endpoint struct {
	Addr  string
	Token string // Rule #8 — never log this field
}

// envFile is where the desktop writes the endpoint for each SSH workspace.
func envFile(home string) string {
	return filepath.Join(home, ".ymux", "run", "last.env")
}

// parseEnvFile reads KEY=VALUE lines, skipping blanks and `#` comments —
// byte-for-byte the CLI's `parse_env_file`, because it reads the same file.
func parseEnvFile(content string) map[string]string {
	out := map[string]string{}
	for _, line := range strings.Split(content, "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		if k, v, ok := strings.Cut(line, "="); ok {
			out[strings.TrimSpace(k)] = strings.TrimSpace(v)
		}
	}
	return out
}

// pick returns the first key present, honouring both spellings. A remote
// provisioned before the winmux→ymux rename still writes the old names until
// its next bootstrap, and those installs are exactly the ones least likely to
// be updated soon.
func pick(m map[string]string, keys ...string) string {
	for _, k := range keys {
		if v := m[k]; v != "" {
			return v
		}
	}
	return ""
}

// Discover locates the tunnel.
//
// The file is the primary source here, not the fallback it is for the CLI: the
// daemon is a long-running service started independently of any SSH session,
// so it never inherits YMUX_SOCKET_ADDR in its environment. The environment is
// still checked first, because a daemon started by hand from inside a workspace
// pane would have the fresher value.
//
// Re-read on EVERY call, never cached: a reconnect moves the tunnel to a
// different port, and a cached address would leave the daemon talking to
// nothing until it restarted.
func Discover(home string) (Endpoint, bool) {
	env := map[string]string{}
	for _, k := range []string{
		"YMUX_SOCKET_ADDR", "WINMUX_SOCKET_ADDR",
		"YMUX_TUNNEL_TOKEN", "WINMUX_TUNNEL_TOKEN",
	} {
		if v := os.Getenv(k); v != "" {
			env[k] = v
		}
	}
	if b, err := os.ReadFile(envFile(home)); err == nil {
		for k, v := range parseEnvFile(string(b)) {
			if env[k] == "" {
				env[k] = v
			}
		}
	}
	ep := Endpoint{
		Addr:  pick(env, "YMUX_SOCKET_ADDR", "WINMUX_SOCKET_ADDR"),
		Token: pick(env, "YMUX_TUNNEL_TOKEN", "WINMUX_TUNNEL_TOKEN"),
	}
	if ep.Addr == "" || ep.Token == "" {
		return Endpoint{}, false
	}
	return ep, true
}

// handshake performs the client half of the HMAC challenge-response.
//
// Wire, as the Rust server implements it:
//
//	S→C  "<TAG>-CHALLENGE <nonce-hex>\n"
//	C→S  "<TAG>-RESPONSE <hmac-hex>\n"     HMAC-SHA256(token, nonce-bytes)
//	S→C  "<TAG>-OK\n" | "<TAG>-DENIED <reason>\n"
//
// TAG is YMUX or WINMUX. We MIRROR whichever the server opened with, and then
// accept either in the verdict — a mixed-version server may answer on the tag
// it prefers rather than the one it was addressed in. The desktop still opens
// with the legacy tag on purpose (`CHALLENGE_TAG` in ymux-tunnel), so in
// practice this speaks WINMUX today and will speak YMUX the day that flips,
// with no change here.
func handshake(rw *bufio.ReadWriter, token string) error {
	line, err := rw.ReadString('\n')
	if err != nil {
		return fmt.Errorf("read challenge: %w", err)
	}
	trimmed := strings.TrimSpace(line)

	tag := ""
	nonceHex := ""
	for _, t := range []string{"YMUX", "WINMUX"} {
		if rest, ok := strings.CutPrefix(trimmed, t+"-CHALLENGE "); ok {
			tag, nonceHex = t, rest
			break
		}
	}
	if tag == "" {
		return fmt.Errorf("expected a CHALLENGE line, got %q", trimmed)
	}
	nonce, err := hex.DecodeString(strings.TrimSpace(nonceHex))
	if err != nil {
		return fmt.Errorf("bad nonce: %w", err)
	}

	mac := hmac.New(sha256.New, []byte(token))
	mac.Write(nonce)
	if _, err := fmt.Fprintf(rw, "%s-RESPONSE %s\n", tag, hex.EncodeToString(mac.Sum(nil))); err != nil {
		return fmt.Errorf("write response: %w", err)
	}
	if err := rw.Flush(); err != nil {
		return fmt.Errorf("flush response: %w", err)
	}

	verdict, err := rw.ReadString('\n')
	if err != nil {
		return fmt.Errorf("read verdict: %w", err)
	}
	switch v := strings.TrimSpace(verdict); {
	case v == "YMUX-OK" || v == "WINMUX-OK":
		return nil
	case strings.HasPrefix(v, "YMUX-DENIED") || strings.HasPrefix(v, "WINMUX-DENIED"):
		return fmt.Errorf("auth denied by the desktop: %s", v)
	default:
		return fmt.Errorf("unexpected handshake verdict: %q", v)
	}
}

// rpcResponse is the JSON-RPC reply envelope.
type rpcResponse struct {
	Result json.RawMessage `json:"result"`
	Error  json.RawMessage `json:"error"`
}

// Call runs one JSON-RPC request against the desktop and returns its result.
//
// timeout covers the RPC only; the handshake has its own, shorter budget. That
// split matters for the caller this exists for: a blocking approval card is
// allowed to sit for minutes waiting on a human, while an authentication that
// stalls for minutes is a dead tunnel and should say so quickly.
func Call(ep Endpoint, method string, params any, timeout time.Duration) (json.RawMessage, error) {
	conn, err := net.DialTimeout("tcp", ep.Addr, handshakeBudget)
	if err != nil {
		return nil, fmt.Errorf("dial desktop: %w", err)
	}
	defer conn.Close()

	rw := bufio.NewReadWriter(bufio.NewReader(conn), bufio.NewWriter(conn))

	_ = conn.SetDeadline(time.Now().Add(handshakeBudget))
	if err := handshake(rw, ep.Token); err != nil {
		return nil, err
	}

	_ = conn.SetDeadline(time.Now().Add(timeout))
	req := map[string]any{"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("marshal request: %w", err)
	}
	if _, err := rw.Write(append(body, '\n')); err != nil {
		return nil, fmt.Errorf("write request: %w", err)
	}
	if err := rw.Flush(); err != nil {
		return nil, fmt.Errorf("flush request: %w", err)
	}

	line, err := rw.ReadString('\n')
	if err != nil {
		return nil, fmt.Errorf("read response: %w", err)
	}
	var resp rpcResponse
	if err := json.Unmarshal([]byte(line), &resp); err != nil {
		return nil, fmt.Errorf("bad response: %w", err)
	}
	if len(resp.Error) > 0 && string(resp.Error) != "null" {
		return nil, fmt.Errorf("desktop rpc error: %s", resp.Error)
	}
	return resp.Result, nil
}

// Available reports whether a desktop tunnel looks reachable, without sending
// anything. Used to tell a browser "ymux is not open" up front instead of
// leaving it polling a request nobody will ever see.
func Available(home string) bool {
	ep, ok := Discover(home)
	if !ok {
		return false
	}
	conn, err := net.DialTimeout("tcp", ep.Addr, 2*time.Second)
	if err != nil {
		return false
	}
	_ = conn.Close()
	return true
}

// clampWait mirrors the desktop's own 1..600 clamp on wait_timeout_seconds, so
// our local deadline is computed from the value the other side will actually
// honour rather than the one we asked for. A non-positive input means "no
// preference" and gets the desktop's default.
func clampWait(s int) int {
	switch {
	case s < 1:
		return 120
	case s > 600:
		return 600
	default:
		return s
	}
}

// Decision is the outcome of a blocking approval card.
type Decision string

// The three outcomes the desktop can return for a blocking feed.push.
const (
	Allow   Decision = "allow"
	Deny    Decision = "deny"
	Timeout Decision = "timeout"
)

// AskApproval pushes a BLOCKING approval card to the desktop feed and waits for
// the human's answer.
//
// `kind: "permission_request"` is what makes the card blocking — the desktop
// keys on exactly that string (`rpc_server.rs`), parks the caller on a oneshot
// and wakes it when the button is pressed. The subkind is deliberately NOT
// "pre-tool-use": that one is routed through the agent policy engine, which has
// opinions about tool names and would be the wrong judge of a pairing request.
//
// waitSeconds is clamped by the desktop to 1..600 whatever we send. The local
// deadline is given a margin on top so a desktop that answers at the very edge
// of its own timeout is still heard, rather than racing us to the close.
func AskApproval(home, requestID, title, summary string, payload map[string]any, waitSeconds int) (Decision, error) {
	ep, ok := Discover(home)
	if !ok {
		return "", ErrNoEndpoint
	}
	waitSeconds = clampWait(waitSeconds)
	params := map[string]any{
		"kind":                 "permission_request",
		"subkind":              "browser-pairing",
		"request_id":           requestID,
		"title":                title,
		"summary":              summary,
		"payload":              payload,
		"wait_timeout_seconds": waitSeconds,
	}
	logger.Info("asking the desktop to approve a browser", "request", requestID, "wait_s", waitSeconds)

	raw, err := Call(ep, "feed.push", params, time.Duration(waitSeconds+15)*time.Second)
	if err != nil {
		return "", err
	}
	var out struct {
		Decision string `json:"decision"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return "", fmt.Errorf("bad decision payload: %w", err)
	}
	switch Decision(out.Decision) {
	case Allow, Deny, Timeout:
		logger.Info("desktop answered", "request", requestID, "decision", out.Decision)
		return Decision(out.Decision), nil
	default:
		// "passive" means the desktop did not treat this as blocking at all —
		// an older build that does not know the kind. Report it rather than
		// guessing, because guessing here means guessing "allow".
		return "", fmt.Errorf("desktop returned a non-decision %q (too old to gate a browser?)", out.Decision)
	}
}
