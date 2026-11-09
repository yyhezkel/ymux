package agent

// Golden tests over testdata/*.json. The same files are read by the Rust
// tests through include_str! (brief.rs, lib.rs, rpc_server.rs,
// context_store.rs), so a behavior change on one side fails on that side.
//
// state_since is not in state.json: Rust stamps the wall clock inside
// apply_hook, Go takes it as a parameter, so the two cannot be compared.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"
)

func loadFixture(t *testing.T, name string, v any) {
	t.Helper()
	b, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(b, v); err != nil {
		t.Fatalf("%s: %v", name, err)
	}
}

func samePtr(a, b *string) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

func TestGoldenParser(t *testing.T) {
	var cases []struct {
		Name   string `json:"name"`
		Msg    string `json:"msg"`
		Pre    string `json:"pre"`
		Parsed *struct {
			Task, Status, Ask, Rec, Next, Delta, Goal, Done *string
		} `json:"parsed"`
	}
	loadFixture(t, "parser.json", &cases)
	for _, c := range cases {
		if got := PreBriefText(c.Msg); got != c.Pre {
			t.Errorf("%s: pre = %q, want %q", c.Name, got, c.Pre)
		}
		got, ok := ParseBrief(c.Msg)
		if ok != (c.Parsed != nil) {
			t.Errorf("%s: parsed = %v", c.Name, ok)
			continue
		}
		if !ok {
			continue
		}
		var status *string
		if got.Status != nil {
			s := string(*got.Status)
			status = &s
		}
		w := c.Parsed
		if !samePtr(got.Task, w.Task) || !samePtr(status, w.Status) || !samePtr(got.Ask, w.Ask) || !samePtr(got.Rec, w.Rec) ||
			!samePtr(got.Next, w.Next) || !samePtr(got.Delta, w.Delta) || !samePtr(got.Goal, w.Goal) || !samePtr(got.Done, w.Done) {
			t.Errorf("%s: got %+v", c.Name, got)
		}
	}
}

func asJSON(t *testing.T, v any) any {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out any
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func TestGoldenJSONShape(t *testing.T) {
	var f struct {
		Briefs []struct {
			Name      string  `json:"name"`
			Message   *string `json:"message"`
			Title     *string `json:"title"`
			UpdatedMs int64   `json:"updated_ms"`
			Expect    any     `json:"expect"`
		} `json:"briefs"`
		EmptyEntry any `json:"empty_entry"`
	}
	loadFixture(t, "brief_json.json", &f)
	for _, c := range f.Briefs {
		if got := asJSON(t, BriefFromStop(c.Message, c.Title, c.UpdatedMs)); !reflect.DeepEqual(got, c.Expect) {
			t.Errorf("%s:\n got %v\nwant %v", c.Name, got, c.Expect)
		}
	}
	if got := asJSON(t, BriefEntry{}); !reflect.DeepEqual(got, f.EmptyEntry) {
		t.Errorf("empty entry = %v", got)
	}
}

func TestGoldenStateTransitions(t *testing.T) {
	var cases []struct {
		Name  string `json:"name"`
		Steps []struct {
			Subkind          string `json:"subkind"`
			NotificationType string `json:"notification_type"`
			Changed          bool   `json:"changed"`
			State            string `json:"state"`
			Seq              uint32 `json:"seq"`
		} `json:"steps"`
	}
	loadFixture(t, "state.json", &cases)
	for _, c := range cases {
		var r Run
		for i, s := range c.Steps {
			changed := r.ApplyHook(s.Subkind, s.NotificationType, time.Unix(int64(i+1), 0))
			if changed != s.Changed || string(r.CurrentState()) != s.State || r.Seq != s.Seq {
				t.Errorf("%s step %d: changed=%v state=%s seq=%d", c.Name, i, changed, r.CurrentState(), r.Seq)
			}
		}
	}
}

func TestGoldenHumanize(t *testing.T) {
	var cases []struct {
		Subkind string         `json:"subkind"`
		Lang    string         `json:"lang"`
		WsName  string         `json:"ws_name"`
		Payload map[string]any `json:"payload"`
		Title   string         `json:"title"`
		Body    string         `json:"body"`
	}
	loadFixture(t, "humanize.json", &cases)
	for _, c := range cases {
		title, body := Humanize(c.Subkind, c.Payload, c.WsName, c.Lang)
		if title != c.Title || body != c.Body {
			t.Errorf("%s/%s = (%q, %q), want (%q, %q)", c.Subkind, c.Lang, title, body, c.Title, c.Body)
		}
	}
}

func TestGoldenKeys(t *testing.T) {
	var cases []struct {
		Name  string `json:"name"`
		Bytes []int  `json:"bytes"`
	}
	loadFixture(t, "keys.json", &cases)
	for _, c := range cases {
		want := make([]byte, len(c.Bytes))
		for i, b := range c.Bytes {
			want[i] = byte(b)
		}
		if got := TranslateKey(c.Name); string(got) != string(want) {
			t.Errorf("%q = %v, want %v", c.Name, got, want)
		}
	}
}

func TestGoldenPolicy(t *testing.T) {
	var cases []struct {
		Name        string   `json:"name"`
		Tool        string   `json:"tool"`
		Command     string   `json:"command"`
		CustomBlock []string `json:"custom_block"`
		Blocked     bool     `json:"blocked"`
	}
	loadFixture(t, "policy.json", &cases)
	for _, c := range cases {
		if got := BlockedBy(c.Tool, c.Command, c.CustomBlock); (got != "") != c.Blocked {
			t.Errorf("%s: matched %q, want blocked=%v", c.Name, got, c.Blocked)
		}
	}
}

// TestEveryBuiltinPatternBlocks walks the table itself, so a pattern added on
// the Go side cannot go untested.
func TestEveryBuiltinPatternBlocks(t *testing.T) {
	for _, pat := range blockPatterns {
		if got := BlockedBy("Bash", pat+"x", nil); got == "" {
			t.Errorf("%q does not block", pat)
		}
	}
}
