package term

import (
	"encoding/json"
	"os"
	"strings"
	"testing"

	"ymux-server/internal/agent"
)

// The expected text is shared with context_store.rs's tests.
func TestGoldenInjection(t *testing.T) {
	b, err := os.ReadFile("../agent/testdata/injection.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name     string           `json:"name"`
		Source   string           `json:"source"`
		This     *SessionContext  `json:"this"`
		Intent   *string          `json:"intent"`
		Siblings []SessionContext `json:"siblings"`
		Want     string           `json:"want"`
	}
	if err := json.Unmarshal(b, &cases); err != nil {
		t.Fatal(err)
	}
	for _, c := range cases {
		intent := ""
		if c.Intent != nil {
			intent = *c.Intent
		}
		got := buildInjection(c.Source, c.This, intent, c.Siblings)
		if got != c.Want {
			t.Errorf("%s:\n got %q\nwant %q", c.Name, got, c.Want)
		}
		if len(got) > injectMaxBytes {
			t.Errorf("%s: %d bytes", c.Name, len(got))
		}
	}
}

func inject(t *testing.T, tg termHookTarget, sessionID, source string) string {
	t.Helper()
	res, rpcErr := call(t, tg, "context.inject", map[string]any{"session_id": sessionID, "source": source})
	if rpcErr != nil {
		t.Fatalf("context.inject: %+v", rpcErr)
	}
	return asMap(t, res)["additional_context"].(string)
}

func saveSettings(t *testing.T, r *HookRegistry, doc string) {
	t.Helper()
	if _, err := r.settings.put(r.settings.get().Version, json.RawMessage(doc)); err != nil {
		t.Fatal(err)
	}
}

func recordTurn(t *testing.T, r *HookRegistry, session, ws, msg string) {
	t.Helper()
	b := agent.BriefFromStop(ptr(msg), nil, 0)
	if _, err := r.context.apply(contextEvent{sessionID: session, wsID: ws, paneID: "p", stop: &b}); err != nil {
		t.Fatal(err)
	}
}

func TestContextInjectCompactReadsTheStore(t *testing.T) {
	r, _, tg := gateEntry(t, policyNone)
	recordTurn(t, r, "s1", "", "[ymux-brief]\ntask: T\ndelta: D\ngoal: G")
	got := inject(t, tg, "s1", "compact")
	if !strings.HasPrefix(got, injectHeader) || !strings.Contains(got, "\nGoal: G\n") || !strings.Contains(got, "\n- D") {
		t.Fatalf("compact = %q", got)
	}
	if got := inject(t, tg, "nosuch", "compact"); got != "" {
		t.Errorf("an unknown session = %q", got)
	}
	if got := inject(t, tg, "../etc", "compact"); got != "" {
		t.Errorf("a path-shaped id = %q", got)
	}
}

func TestContextInjectStartupUsesTheWorkspace(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	ws, err := r.webws.create(workspaceCreate{Name: "w", Intent: "ship v2"})
	if err != nil {
		t.Fatal(err)
	}
	r.mu.Lock()
	e.workspaceID = ws.ID
	r.mu.Unlock()
	recordTurn(t, r, "other", ws.ID, "[ymux-brief]\ntask: sibling work\nstatus: stuck")
	recordTurn(t, r, "elsewhere", "w_other", "[ymux-brief]\ntask: not mine")
	got := inject(t, tg, "me", "startup")
	if !strings.Contains(got, "Workspace goal: ship v2") || !strings.Contains(got, "- sibling work — stuck") || strings.Contains(got, "not mine") {
		t.Fatalf("startup = %q", got)
	}
}

func TestContextInjectFallsBackToTheRecordsWorkspace(t *testing.T) {
	r, _, tg := gateEntry(t, policyNone)
	ws, err := r.webws.create(workspaceCreate{Name: "w", Intent: "from the record"})
	if err != nil {
		t.Fatal(err)
	}
	recordTurn(t, r, "s1", ws.ID, "no brief")
	if got := inject(t, tg, "s1", "startup"); !strings.Contains(got, "Workspace goal: from the record") {
		t.Fatalf("startup = %q", got)
	}
}

func TestContextInjectOffSwitch(t *testing.T) {
	r, _, tg := gateEntry(t, policyNone)
	recordTurn(t, r, "s1", "", "[ymux-brief]\ndelta: D")
	saveSettings(t, r, `{"brief":{"inject_context":false}}`)
	if got := inject(t, tg, "s1", "compact"); got != "" {
		t.Fatalf("the off switch was ignored: %q", got)
	}
	// Only a real false turns it off.
	for _, doc := range []string{`{"brief":{"inject_context":true}}`, `{"brief":{}}`, `{"brief":{"inject_context":"no"}}`, `{"brief":7}`, `{}`} {
		saveSettings(t, r, doc)
		if got := inject(t, tg, "s1", "compact"); got == "" {
			t.Errorf("%s turned injection off", doc)
		}
	}
}

func TestContextInjectReleasedSessionGetsNothing(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	recordTurn(t, r, "s1", "", "[ymux-brief]\ndelta: D")
	r.mu.Lock()
	e.released = true
	r.mu.Unlock()
	if got := inject(t, tg, "s1", "compact"); got != "" {
		t.Fatalf("released = %q", got)
	}
}

// ── Block rules (agent.BlockedBy, read through feedPush) ──

func bash(e *hookEntry, id, command string, extra map[string]any) map[string]any {
	p := permission(e, id, map[string]any{
		"payload": map[string]any{"tool_name": "Bash", "tool_input": map[string]any{"command": command}},
	})
	for k, v := range extra {
		p[k] = v
	}
	return p
}

func wantDenied(t *testing.T, res map[string]any, id string) {
	t.Helper()
	if res["decision"] != "deny" || res["policy"] != "block" || res["request_id"] != id {
		t.Fatalf("want a policy deny, got %v", res)
	}
}

func TestBlockRuleDeniesUnderPolicyNone(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	wantDenied(t, push(t, tg, bash(e, "b1", "cd /tmp && RM  -RF /", nil)), "b1")
	cards := r.feed.list("en")
	if len(cards) != 1 {
		t.Fatalf("want one audit card, got %d", len(cards))
	}
	c := cards[0]
	if c.RequestID != "b1" || c.Kind != "passive" || c.Subkind != "policy-block" || c.Blocking || c.State != statePassive ||
		c.Title != "⛔ Blocked: Bash" || c.Summary != "blocked: command contains `rm -rf /`" {
		t.Fatalf("audit card = %+v", c)
	}
}

func TestBlockRuleDeniesBeforeTheGate(t *testing.T) {
	r, e, tg := gateEntry(t, policyGate)
	// wait_timeout_seconds bounds the test if the gate is ever reached.
	wantDenied(t, push(t, tg, bash(e, "b2", "mkfs /dev/sdb", map[string]any{"wait_timeout_seconds": 1})), "b2")
	if cards := r.feed.list("en"); len(cards) != 1 || cards[0].Blocking {
		t.Fatalf("a Block must not make a gate card: %+v", cards)
	}
}

func TestCustomBlockRuleDenies(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	saveSettings(t, r, `{"hooks":{"custom_block":["Terraform  Destroy"]}}`)
	wantDenied(t, push(t, tg, bash(e, "c1", "terraform   destroy -auto-approve", nil)), "c1")
	if res := push(t, tg, bash(e, "c2", "terraform plan", nil)); res["decision"] != "allow" {
		t.Fatalf("a non-matching command = %v", res)
	}
}

// The fail-safe rule: nothing in the settings document can remove a built-in,
// and a damaged custom list cannot deny or allow more than its good entries.
func TestDamagedSettingsKeepTheBuiltins(t *testing.T) {
	docs := []string{
		`{"hooks":{"custom_block":"garbage"}}`,
		`{"hooks":{"custom_block":{"a":1}}}`,
		`{"hooks":{"custom_block":[1,{"a":2},null,"   ",""]}}`,
		`{"hooks":"garbage"}`,
		`{"hooks":{"policy_enabled":false}}`,
		`{"hooks":{"policy_enabled":"no"}}`,
		`{}`,
	}
	for _, doc := range docs {
		r, e, tg := gateEntry(t, policyNone)
		saveSettings(t, r, doc)
		if res := push(t, tg, bash(e, "x1", "rm -rf /", nil)); res["decision"] != "deny" {
			t.Errorf("%s: rm -rf / → %v", doc, res)
		}
		if res := push(t, tg, bash(e, "x2", "ls -la", nil)); res["decision"] != "allow" {
			t.Errorf("%s: ls → %v", doc, res)
		}
	}
}

func TestBuiltinsApplyBeforeAnySettingsExist(t *testing.T) {
	_, e, tg := gateEntry(t, policyNone)
	wantDenied(t, push(t, tg, bash(e, "n1", "rm -rf /", nil)), "n1")
}

func TestGoodCustomEntriesSurviveBadOnes(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	saveSettings(t, r, `{"hooks":{"custom_block":[7,"terraform destroy","  "]}}`)
	wantDenied(t, push(t, tg, bash(e, "g1", "terraform destroy", nil)), "g1")
}

func TestBlockRuleReadsOnlyPreToolUseBash(t *testing.T) {
	_, e, tg := gateEntry(t, policyNone)
	res := push(t, tg, permission(e, "w1", map[string]any{
		"payload": map[string]any{"tool_name": "Write", "tool_input": map[string]any{"command": "rm -rf /"}},
	}))
	if res["decision"] != "allow" {
		t.Fatalf("Write = %v", res)
	}
	res = push(t, tg, bash(e, "w2", "rm -rf /", map[string]any{"subkind": "post-tool-use", "kind": "passive"}))
	if res["decision"] == "deny" {
		t.Fatalf("a passive hook was denied: %v", res)
	}
}
