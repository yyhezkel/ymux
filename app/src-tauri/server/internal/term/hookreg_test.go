package term

// Phase 100 (WEB-DESIGN B2): hook routing for browser-created sessions —
// the create-time env injection, the registry's bookkeeping, and the
// dispatch that folds a hook into the pane's light and brief.

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/json"
	"net/http"
	"regexp"
	"strings"
	"testing"
	"time"

	"ymux-server/internal/agent"
)

// hookService is a Service whose fake tmux reports `version` for -V, says
// every session name is free, and accepts every create.
func hookService(version string) (*Service, *[][]string) {
	s, calls := testService(func(args []string) ([]byte, error) {
		switch args[0] {
		case "-V":
			return []byte(version + "\n"), nil
		case "has-session":
			return nil, exitErr()
		}
		return nil, nil
	})
	s.attachHooks(NewHookRegistry())
	return s, calls
}

func envArgs(argv []string) map[string]string {
	out := map[string]string{}
	for i := 0; i+1 < len(argv); i++ {
		if argv[i] == "-e" {
			k, v, _ := strings.Cut(argv[i+1], "=")
			out[k] = v
		}
	}
	return out
}

func lastCreate(t *testing.T, calls *[][]string) []string {
	t.Helper()
	for i := len(*calls) - 1; i >= 0; i-- {
		if (*calls)[i][1] == "new-session" {
			return (*calls)[i]
		}
	}
	t.Fatal("no new-session call")
	return nil
}

var paneIDShape = regexp.MustCompile(`^term_[0-9a-f]{16}$`)

func TestCreateInjectsHookEnv(t *testing.T) {
	s, calls := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:4321")
	w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"web1"}`)
	if w.Code != http.StatusCreated {
		t.Fatalf("got %d: %s", w.Code, w.Body.String())
	}
	env := envArgs(lastCreate(t, calls))
	if env["YMUX_SOCKET_ADDR"] != "127.0.0.1:4321" {
		t.Errorf("socket addr = %q", env["YMUX_SOCKET_ADDR"])
	}
	if !paneIDShape.MatchString(env["YMUX_PANE_ID"]) {
		t.Errorf("pane id = %q", env["YMUX_PANE_ID"])
	}
	if len(env["YMUX_TUNNEL_TOKEN"]) != 64 {
		t.Errorf("token should be 32 random bytes in hex, got %d chars", len(env["YMUX_TUNNEL_TOKEN"]))
	}
	snap := s.hooks.Snapshot()
	got, ok := snap[env["YMUX_PANE_ID"]]
	if !ok || got.Session != "web1" {
		t.Fatalf("registry = %+v", snap)
	}
	if got.AgentRun.State != agent.StateUnknown {
		t.Errorf("a fresh pane has no light, got %q", got.AgentRun.State)
	}
	var body map[string]any
	_ = json.Unmarshal(w.Body.Bytes(), &body)
	if body["hooks"] != true {
		t.Errorf("response hooks = %v", body["hooks"])
	}
}

func TestNoHookEnvWithoutAListenerOrOnOldTmux(t *testing.T) {
	for _, c := range []struct {
		name, version, addr string
	}{
		{"no listener", "tmux 3.4", ""},
		{"tmux 3.1", "tmux 3.1c", "127.0.0.1:1"},
	} {
		s, calls := hookService(c.version)
		if c.addr != "" {
			s.hooks.SetHookAddr(c.addr)
		}
		if w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"x"}`); w.Code != http.StatusCreated {
			t.Fatalf("%s: got %d", c.name, w.Code)
		}
		if env := envArgs(lastCreate(t, calls)); len(env) != 0 {
			t.Errorf("%s: injected %v", c.name, env)
		}
		if n := len(s.hooks.Snapshot()); n != 0 {
			t.Errorf("%s: registered %d entries", c.name, n)
		}
	}
}

func TestVersionAtLeast(t *testing.T) {
	for v, want := range map[string]bool{
		"tmux 3.2": true, "tmux 3.2a": true, "tmux 3.4": true, "tmux 4.0": true,
		"tmux next-3.5": true, "tmux master": true,
		"tmux 3.1c": false, "tmux 3.0a": false, "tmux 2.9": false, "garbage": false,
	} {
		if got := versionAtLeast(v, 3, 2); got != want {
			t.Errorf("versionAtLeast(%q) = %v, want %v", v, got, want)
		}
	}
}

func TestRegistryFollowsRenameKillAndList(t *testing.T) {
	r := NewHookRegistry()
	e, _, _ := r.mint("a", "127.0.0.1:1")
	r.add(e)
	r.Rename("a", "b")
	if snap := r.Snapshot(); snap[e.paneID].Session != "b" {
		t.Fatalf("rename not followed: %+v", snap)
	}
	r.Retain([]Session{{Name: "other"}})
	if len(r.Snapshot()) != 0 {
		t.Fatal("a session tmux no longer lists must be dropped")
	}
	r.add(e)
	r.Remove("b")
	if len(r.Snapshot()) != 0 {
		t.Fatal("kill must forget the session")
	}
}

// matched returns the dispatch target for e, through the real HMAC match.
func matched(t *testing.T, r *HookRegistry, e *hookEntry) termHookTarget {
	t.Helper()
	nonce := []byte("0123456789abcdef")
	h := hmac.New(sha256.New, []byte(e.token))
	h.Write(nonce)
	target, ok := r.MatchHookHMAC(nonce, h.Sum(nil))
	if !ok {
		t.Fatal("registry did not match its own token")
	}
	if _, ok := r.MatchHookHMAC(nonce, []byte("forged")); ok {
		t.Fatal("a forged MAC matched")
	}
	return target.(termHookTarget)
}

func push(t *testing.T, tg termHookTarget, params map[string]any) map[string]any {
	t.Helper()
	raw, _ := json.Marshal(params)
	res, rpcErr := tg.DispatchHook("feed.push", raw)
	if rpcErr != nil {
		t.Fatalf("feed.push error: %+v", rpcErr)
	}
	return res.(map[string]any)
}

func TestDispatchFoldsATurn(t *testing.T) {
	r := NewHookRegistry()
	clock := time.Unix(1_700_000_000, 0)
	r.now = func() time.Time { return clock }
	e, _, _ := r.mint("web", "127.0.0.1:1")
	r.add(e)
	tg := matched(t, r, e)

	res := push(t, tg, map[string]any{
		"request_id": "r1", "kind": "passive", "subkind": "user-prompt-submit",
		"pane_id": e.paneID, "tmux_session": "web",
		"payload": map[string]any{"prompt": "  fix the login bug  "},
	})
	if res["decision"] != "passive" {
		t.Fatalf("prompt → %v", res)
	}
	snap := r.Snapshot()[e.paneID]
	if snap.AgentRun.State != agent.StateRunning || !snap.AgentRun.Running {
		t.Errorf("after prompt: %+v", snap.AgentRun)
	}
	if snap.Brief.LastPrompt == nil || *snap.Brief.LastPrompt != "fix the login bug" {
		t.Errorf("last prompt = %v", snap.Brief.LastPrompt)
	}

	clock = clock.Add(30 * time.Second)
	push(t, tg, map[string]any{
		"request_id": "r2", "kind": "passive", "subkind": "stop", "pane_id": e.paneID,
		"claude_title": "Login fix",
		"payload":      map[string]any{"last_assistant_message": "Done.\n[ymux-brief]\nstatus: done\ndelta: fixed\n"},
	})
	snap = r.Snapshot()[e.paneID]
	if snap.AgentRun.State != agent.StateDone || snap.AgentRun.Running {
		t.Errorf("after stop: %+v", snap.AgentRun)
	}
	if snap.AgentRun.AvgMs == nil || *snap.AgentRun.AvgMs != 30_000 {
		t.Errorf("the 30 s turn must be averaged: %v", snap.AgentRun.AvgMs)
	}
	b := snap.Brief.Brief
	if b == nil || b.Degraded || b.Delta == nil || *b.Delta != "fixed" || b.Task == nil || *b.Task != "Login fix" {
		t.Errorf("brief = %+v", b)
	}

	push(t, tg, map[string]any{"request_id": "r3", "kind": "passive", "subkind": "session-end", "pane_id": e.paneID})
	snap = r.Snapshot()[e.paneID]
	if snap.AgentRun.State != agent.StateUnknown || !snap.Brief.SessionEnded {
		t.Errorf("after session-end: %+v / ended=%v", snap.AgentRun, snap.Brief.SessionEnded)
	}
}

func TestPermissionRequestIsAllowedUnderPolicyNone(t *testing.T) {
	r := NewHookRegistry()
	e, _, _ := r.mint("web", "127.0.0.1:1")
	r.add(e)
	res := push(t, matched(t, r, e), map[string]any{
		"request_id": "p1", "kind": "permission_request", "subkind": "pre-tool-use",
		"pane_id": e.paneID, "payload": map[string]any{"tool_name": "Bash"},
	})
	if res["decision"] != "allow" || res["policy"] != "none" || res["request_id"] != "p1" {
		t.Fatalf("permission request → %v", res)
	}
	if st := r.Snapshot()[e.paneID].AgentRun.State; st != agent.StateRunning {
		t.Errorf("pre-tool-use must light running, got %q", st)
	}
}

func TestMismatchedPaneOrSessionIsDenied(t *testing.T) {
	r := NewHookRegistry()
	e, _, _ := r.mint("web", "127.0.0.1:1")
	r.add(e)
	tg := matched(t, r, e)
	for _, p := range []map[string]any{
		{"request_id": "x", "kind": "permission_request", "subkind": "pre-tool-use", "pane_id": "term_someoneelse"},
		{"request_id": "x", "kind": "permission_request", "subkind": "pre-tool-use", "pane_id": e.paneID, "tmux_session": "other"},
		{"request_id": "x", "kind": "permission_request", "subkind": "pre-tool-use"}, // no pane id at all
	} {
		if res := push(t, tg, p); res["decision"] != "deny" {
			t.Errorf("%v → %v, want deny", p, res)
		}
	}
	if st := r.Snapshot()[e.paneID].AgentRun.State; st != agent.StateUnknown {
		t.Errorf("a denied hook must not touch the light, got %q", st)
	}
}

func TestPingAndUnknownMethod(t *testing.T) {
	r := NewHookRegistry()
	e, _, _ := r.mint("web", "127.0.0.1:1")
	r.add(e)
	tg := matched(t, r, e)
	if res, err := tg.DispatchHook("ping", nil); err != nil || res.(map[string]any)["ok"] != true {
		t.Errorf("ping → %v, %v", res, err)
	}
	if _, err := tg.DispatchHook("no.such.method", nil); err == nil || err.Code != -32000 {
		t.Errorf("unknown method → %+v", err)
	}
}
