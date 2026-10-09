package term

import (
	"strings"
	"testing"
)

func TestRecoverHooksFromSessionEnvironment(t *testing.T) {
	// Phase 111: after a restart the registry is empty; a session this
	// daemon made (its YMUX_SOCKET_ADDR is our listener) comes back with its
	// token, pane id, policy and workspace. A desktop session — same variable
	// names, pointed at the desktop's tunnel — is never claimed.
	tok := strings.Repeat("ab", 32)
	envs := map[string]string{
		"=web":   "YMUX_SOCKET_ADDR=127.0.0.1:4321\nYMUX_TUNNEL_TOKEN=" + tok + "\nYMUX_PANE_ID=p_leaf_1\nYMUX_POLICY=gate\nYMUX_WORKSPACE_ID=w_gone\n-REMOVED\n",
		"=old":   "YMUX_SOCKET_ADDR=127.0.0.1:4321\nYMUX_TUNNEL_TOKEN=" + strings.Repeat("cd", 32) + "\nYMUX_PANE_ID=term_0123456789abcdef\n",
		"=desk":  "YMUX_SOCKET_ADDR=127.0.0.1:9999\nYMUX_TUNNEL_TOKEN=" + strings.Repeat("ef", 32) + "\nYMUX_PANE_ID=p_desk\n",
		"=plain": "TERM=xterm\n",
	}
	s, _ := testService(func(args []string) ([]byte, error) {
		switch args[0] {
		case "-V":
			return []byte("tmux 3.4\n"), nil
		case "list-sessions":
			return []byte("web\t1\t1\t0\t/x\nold\t1\t1\t0\t/x\ndesk\t1\t1\t0\t/x\nplain\t1\t1\t0\t/x\n"), nil
		case "show-environment":
			return []byte(envs[args[2]]), nil
		}
		return nil, nil
	})
	s.attachHooks(NewHookRegistry())
	s.hooks.SetHookAddr("127.0.0.1:4321")

	s.RecoverHooks()

	snap := s.hooks.Snapshot()
	if len(snap) != 2 {
		t.Fatalf("recovered %d sessions, want 2 (web, old): %+v", len(snap), snap)
	}
	if snap["p_leaf_1"].Session != "web" || snap["term_0123456789abcdef"].Session != "old" {
		t.Fatalf("wrong panes: %+v", snap)
	}
	if _, ok := snap["p_desk"]; ok {
		t.Fatal("a desktop session was claimed")
	}
	e := s.hooks.byName["web"]
	if e.policy != policyGate || e.token != tok {
		t.Errorf("web: policy %q, token kept %v", e.policy, e.token == tok)
	}
	if e.workspaceID != "" {
		t.Errorf("a workspace that no longer exists must not come back: %q", e.workspaceID)
	}
	if s.hooks.byName["old"].policy != policyNone {
		t.Errorf("a pre-111 session (no YMUX_POLICY) recovers as none")
	}
	// Idempotent: a second pass adds nothing.
	s.RecoverHooks()
	if len(s.hooks.Snapshot()) != 2 {
		t.Error("second recovery duplicated entries")
	}
}

func TestCreateRecordsPolicyAndWorkspaceInTheSession(t *testing.T) {
	s, calls := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:4321")
	if w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"g","policy":"gate"}`); w.Code != 201 {
		t.Fatalf("create = %d %s", w.Code, w.Body.String())
	}
	if env := envArgs(lastCreate(t, calls)); env["YMUX_POLICY"] != "gate" {
		t.Errorf("YMUX_POLICY = %q", env["YMUX_POLICY"])
	}
	// A later policy change is written into the session too.
	do(s, "POST", "/api/v2/term/sessions/g/policy", "owner-token", `{"policy":"none"}`)
	last := (*calls)[len(*calls)-1]
	if strings.Join(last[1:], " ") != "set-environment -t =g YMUX_POLICY none" {
		t.Errorf("last call = %v", last)
	}
}
