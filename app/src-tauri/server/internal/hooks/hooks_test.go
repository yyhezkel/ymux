package hooks

// The listener in isolation: fake resolvers, a Go client that speaks the CLI's
// wire format byte for byte. chat's own suite (chat_hookrpc_test.go) still
// drives the full chat path through this same listener.

import (
	"bufio"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"ymux-server/internal/core"
)

// fakeResolver owns one token and answers with a fixed result or error.
type fakeResolver struct {
	name  string
	token string
	err   *core.RPCError

	mu       sync.Mutex
	addr     string
	lastMeth string
}

func (f *fakeResolver) SetHookAddr(a string) { f.mu.Lock(); f.addr = a; f.mu.Unlock() }

func (f *fakeResolver) MatchHookHMAC(nonce, mac []byte) (core.HookTarget, bool) {
	h := hmac.New(sha256.New, []byte(f.token))
	h.Write(nonce)
	if hmac.Equal(h.Sum(nil), mac) {
		return f, true
	}
	return nil, false
}

func (f *fakeResolver) DispatchHook(method string, _ json.RawMessage) (any, *core.RPCError) {
	f.mu.Lock()
	f.lastMeth = method
	f.mu.Unlock()
	if f.err != nil {
		return nil, f.err
	}
	return map[string]any{"who": f.name}, nil
}

func (f *fakeResolver) listenAddr() string { f.mu.Lock(); defer f.mu.Unlock(); return f.addr }

func (f *fakeResolver) method() string { f.mu.Lock(); defer f.mu.Unlock(); return f.lastMeth }

// call runs handshake + one request in `tag`; it returns the verdict line and,
// on -OK, the decoded reply.
func call(t *testing.T, addr, token, tag, method string) (string, map[string]any) {
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
	if !strings.HasPrefix(trimmed, ChallengeTag+"-CHALLENGE ") {
		t.Fatalf("challenge = %q", trimmed)
	}
	nonce, err := hex.DecodeString(strings.TrimPrefix(trimmed, ChallengeTag+"-CHALLENGE "))
	if err != nil {
		t.Fatalf("nonce: %v", err)
	}
	h := hmac.New(sha256.New, []byte(token))
	h.Write(nonce)
	fmt.Fprintf(conn, "%s-RESPONSE %s\n", tag, hex.EncodeToString(h.Sum(nil)))
	verdict, err := br.ReadString('\n')
	if err != nil {
		t.Fatalf("read verdict: %v", err)
	}
	verdict = strings.TrimSpace(verdict)
	if verdict != tag+"-OK" {
		return verdict, nil
	}
	fmt.Fprintf(conn, `{"jsonrpc":"2.0","id":7,"method":%q,"params":{}}`+"\n", method)
	respLine, err := br.ReadString('\n')
	if err != nil {
		t.Fatalf("read reply: %v", err)
	}
	var resp map[string]any
	if err := json.Unmarshal([]byte(respLine), &resp); err != nil {
		t.Fatalf("reply %q: %v", respLine, err)
	}
	return verdict, resp
}

func TestEachTokenReachesItsOwnResolver(t *testing.T) {
	chat := &fakeResolver{name: "chat", token: "tok-chat"}
	term := &fakeResolver{name: "term", token: "tok-term"}
	Start("", term, chat)
	addr := term.listenAddr()
	if addr == "" || chat.listenAddr() != addr {
		t.Fatalf("every AddrSink must learn the address: term=%q chat=%q", addr, chat.listenAddr())
	}
	for _, c := range []struct{ token, want string }{{"tok-chat", "chat"}, {"tok-term", "term"}} {
		verdict, resp := call(t, addr, c.token, TagYmux, "feed.push")
		if verdict != "YMUX-OK" {
			t.Fatalf("%s: verdict %q", c.want, verdict)
		}
		res, _ := resp["result"].(map[string]any)
		if res["who"] != c.want {
			t.Errorf("token for %s reached %v", c.want, res["who"])
		}
		if resp["id"] != float64(7) {
			t.Errorf("id not echoed: %v", resp["id"])
		}
	}
}

func TestUnknownTokenIsDeniedInClientDialect(t *testing.T) {
	r := &fakeResolver{name: "x", token: "real"}
	Start("", r)
	for _, tag := range []string{TagYmux, TagLegacy} {
		verdict, _ := call(t, r.listenAddr(), "forged", tag, "feed.push")
		if verdict != tag+"-DENIED unknown-session" {
			t.Errorf("dialect %s: verdict %q", tag, verdict)
		}
	}
}

func TestRPCErrorBecomesAnErrorObject(t *testing.T) {
	r := &fakeResolver{name: "x", token: "t", err: &core.RPCError{Code: -32000, Message: "unknown method"}}
	Start("", r)
	_, resp := call(t, r.listenAddr(), "t", TagLegacy, "nope")
	if _, has := resp["result"]; has {
		t.Errorf("an error reply must not carry result: %v", resp)
	}
	e, _ := resp["error"].(map[string]any)
	if e["code"] != float64(-32000) || e["message"] != "unknown method" {
		t.Errorf("error = %v", resp["error"])
	}
	if m := r.method(); m != "nope" {
		t.Errorf("method not passed through: %q", m)
	}
}

func TestBadResponseHex(t *testing.T) {
	r := &fakeResolver{name: "x", token: "t"}
	Start("", r)
	conn, err := net.DialTimeout("tcp", r.listenAddr(), 3*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	br := bufio.NewReader(conn)
	if _, err := br.ReadString('\n'); err != nil {
		t.Fatal(err)
	}
	fmt.Fprintf(conn, "YMUX-RESPONSE not-hex\n")
	v, _ := br.ReadString('\n')
	if strings.TrimSpace(v) != "YMUX-DENIED bad-response" {
		t.Errorf("verdict %q", v)
	}
}
