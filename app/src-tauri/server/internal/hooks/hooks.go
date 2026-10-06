// Package hooks is the hook-RPC endpoint: the localhost TCP listener that the
// ymux CLI's `claude-hook` dials, and the Phase-66 wire protocol spoken on it.
//
// Phase 100 (WEB-DESIGN B2) moved the protocol here from chat. Until then
// chat owned the handshake because its per-session tokens were the only ones;
// now two subsystems mint tokens — chat for the claude children it spawns for
// the phone, term for the tmux sessions a browser creates — so the listener
// does the challenge/response once and asks each core.HookResolver whose token
// produced the HMAC. The matched core.HookTarget answers the one request.
// hooks → core, chat → core, term → core; cmd wires them.
//
// Wire format (cli/src/main.rs perform_handshake + rpc_via):
//
//	S->C  "WINMUX-CHALLENGE <nonce-hex>\n"
//	C->S  "YMUX-RESPONSE <hmac_sha256(token, nonce_bytes)-hex>\n"   (or WINMUX-)
//	S->C  "YMUX-OK\n"  |  "YMUX-DENIED <reason>\n"                  (client's dialect)
//	C->S  {"jsonrpc":"2.0","id":1,"method":"feed.push","params":{…}}\n
//	S->C  {"jsonrpc":"2.0","id":1,"result":{…}}\n  |  {…,"error":{"code","message"}}\n
//
// Rule #8: tokens and MACs are never logged. Rule #1: nor are params — a hook
// payload carries prompts and tool input.
package hooks

import (
	"bufio"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"time"

	"ymux-server/internal/core"
	"ymux-server/internal/logging"
)

// logger is the hook-RPC listener's component logger (Phase 79.D).
var logger = logging.New("SRV:HOOKRPC")

// Handshake wire tags (winmux → ymux rename). The challenge we EMIT stays
// on the legacy tag for one release because a pre-rename `winmux` CLI does
// a literal prefix match and hangs up on anything else; both ends read
// either dialect and mirror whatever they were spoken to. Mirrors the same
// constants in crates/ymux-tunnel/src/lib.rs — flip both together.
//
// FOLLOWUPS P1: set ChallengeTag = TagYmux in the release after 0.5.0,
// once every provisioned remote has been re-bootstrapped.
const (
	TagYmux      = "YMUX"
	TagLegacy    = "WINMUX"
	ChallengeTag = TagLegacy
)

// Start binds a localhost port and serves hook RPC connections for the life
// of the process, matching callers against resolvers in order. Every
// resolver that is also a core.AddrSink learns the bound address.
// Best-effort: if the listen fails, hooks simply won't reach the daemon
// (logged) and the rest of the server is unaffected.
//
// portFile (Phase 111), when non-empty, makes the port survive a restart: a
// session's hooks find the daemon through YMUX_SOCKET_ADDR, fixed in the
// environment of every process already running in it (claude included), so
// a daemon that came back on a new port would leave all of them dialing a
// dead one. The previous port is tried first; only if it is taken does the
// listener fall back to an ephemeral one (and those sessions lose hooks).
func Start(portFile string, resolvers ...core.HookResolver) {
	ln := listenPreferred(portFile)
	if ln == nil {
		return
	}
	addr := ln.Addr().String()
	for _, r := range resolvers {
		if sink, ok := r.(core.AddrSink); ok {
			sink.SetHookAddr(addr)
		}
	}
	logger.Info("RPC listening", "addr", addr, "resolvers", len(resolvers))
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go handleConn(conn, resolvers)
		}
	}()
}

// listenPreferred binds the port remembered in portFile, else an ephemeral
// one, and records whichever it got. nil when nothing could be bound.
func listenPreferred(portFile string) net.Listener {
	if portFile != "" {
		if b, err := os.ReadFile(portFile); err == nil {
			if p, err := strconv.Atoi(strings.TrimSpace(string(b))); err == nil && p > 0 && p < 65536 {
				ln, err := net.Listen("tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(p)))
				if err == nil {
					return ln
				}
				logger.Warn("previous hook port unavailable; sessions started before this restart lose their hooks",
					"port", p, "err", err)
			}
		}
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		logger.Warn("RPC listen failed, hooks won't reach the daemon", "err", err)
		return nil
	}
	if portFile != "" {
		_, port, _ := net.SplitHostPort(ln.Addr().String())
		tmp := portFile + ".tmp"
		if err := os.WriteFile(tmp, []byte(port+"\n"), 0o600); err == nil {
			_ = os.Rename(tmp, portFile)
		} else {
			logger.Warn("could not record the hook port", "err", err)
		}
	}
	return ln
}

func handleConn(conn net.Conn, resolvers []core.HookResolver) {
	defer conn.Close()
	br := bufio.NewReader(conn)

	// 1. Challenge.
	nonce := make([]byte, 16)
	if _, err := rand.Read(nonce); err != nil {
		return
	}
	_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
	if _, err := fmt.Fprintf(conn, "%s-CHALLENGE %s\n", ChallengeTag, hex.EncodeToString(nonce)); err != nil {
		return
	}

	// 2. Response → whichever resolver's token validates the HMAC.
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	respLine, err := br.ReadString('\n')
	if err != nil {
		return
	}
	// Accept either dialect and answer in the one we were spoken to, so a
	// pre-rename `winmux` CLI never sees a verdict tag it can't parse.
	trimmed := strings.TrimSpace(respLine)
	replyTag := ChallengeTag
	respHex := ""
	switch {
	case strings.HasPrefix(trimmed, TagYmux+"-RESPONSE "):
		replyTag = TagYmux
		respHex = strings.TrimSpace(strings.TrimPrefix(trimmed, TagYmux+"-RESPONSE "))
	case strings.HasPrefix(trimmed, TagLegacy+"-RESPONSE "):
		replyTag = TagLegacy
		respHex = strings.TrimSpace(strings.TrimPrefix(trimmed, TagLegacy+"-RESPONSE "))
	}
	respMAC, err := hex.DecodeString(respHex)
	if err != nil || respHex == "" {
		_, _ = conn.Write([]byte(replyTag + "-DENIED bad-response\n"))
		return
	}
	target := match(resolvers, nonce, respMAC)
	if target == nil {
		_, _ = conn.Write([]byte(replyTag + "-DENIED unknown-session\n"))
		return
	}
	if _, err := conn.Write([]byte(replyTag + "-OK\n")); err != nil {
		return
	}

	// 3. One JSON-RPC request.
	_ = conn.SetReadDeadline(time.Now().Add(15 * time.Second))
	reqLine, err := br.ReadString('\n')
	if err != nil {
		return
	}
	var req struct {
		ID     json.RawMessage `json:"id"`
		Method string          `json:"method"`
		Params json.RawMessage `json:"params"`
	}
	if json.Unmarshal([]byte(strings.TrimSpace(reqLine)), &req) != nil {
		return
	}

	result, rpcErr := target.DispatchHook(req.Method, req.Params)

	// 4. Reply. No write deadline beyond the OS — a blocking gate may
	// legitimately hold for up to wait_timeout_seconds.
	id := req.ID
	if len(id) == 0 {
		id = json.RawMessage("1")
	}
	resp := map[string]any{"jsonrpc": "2.0", "id": id}
	if rpcErr != nil {
		resp["error"] = rpcErr
	} else {
		resp["result"] = result
	}
	out, err := json.Marshal(resp)
	if err != nil {
		logger.Error("hook reply encode failed", "method", req.Method, "err", err)
		return
	}
	_ = conn.SetWriteDeadline(time.Time{})
	_, _ = conn.Write(append(out, '\n'))
}

// match asks each resolver in turn; the first hit wins. Tokens are random
// 32-byte values, so two resolvers can never both match one MAC.
func match(resolvers []core.HookResolver, nonce, mac []byte) core.HookTarget {
	for _, r := range resolvers {
		if t, ok := r.MatchHookHMAC(nonce, mac); ok {
			return t
		}
	}
	return nil
}
