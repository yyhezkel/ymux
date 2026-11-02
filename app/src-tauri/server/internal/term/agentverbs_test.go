package term

// Phase 103 (WEB-DESIGN B5): browser workspaces, the layout leaf ops, and the
// agent verbs with their workspace fence.

import (
	"encoding/json"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
)

func TestSplitLeafKeepsUnknownFields(t *testing.T) {
	root, _ := parseLayout(json.RawMessage(`{"kind":"pane","pane_id":"a","color":"#f00","future_field":{"x":1}}`))
	out, err := splitLeaf(root, "a", "b", "vertical")
	if err != nil {
		t.Fatal(err)
	}
	b, _ := json.Marshal(out)
	s := string(b)
	for _, want := range []string{`"kind":"split"`, `"direction":"vertical"`, `"ratio":0.5`,
		`"color":"#f00"`, `"future_field":{"x":1}`, `"pane_id":"b"`, `"split_id":"sp_`} {
		if !strings.Contains(s, want) {
			t.Errorf("split result lacks %s: %s", want, s)
		}
	}
	// Nested: split the second leaf of a split.
	out, err = splitLeaf(out, "b", "c", "horizontal")
	if err != nil || findLeaf(out, "c") == nil || findLeaf(out, "a") == nil {
		t.Fatalf("nested split: %v %v", out, err)
	}
	if ids := paneIDs(out); len(ids) != 3 || ids[0]["pane_id"] != "a" || ids[2]["pane_id"] != "c" {
		t.Errorf("depth-first leaves = %v", ids)
	}
	if _, err := splitLeaf(out, "zzz", "d", "horizontal"); err != errNoPane {
		t.Errorf("missing leaf → %v, want errNoPane", err)
	}
}

func TestSplitDirectionSpellings(t *testing.T) {
	for in, want := range map[string]string{"": "horizontal", "right": "horizontal", "h": "horizontal",
		"down": "vertical", "v": "vertical", "vertical": "vertical"} {
		if got, ok := splitDirection(in); !ok || got != want {
			t.Errorf("splitDirection(%q) = %q %v", in, got, ok)
		}
	}
	if _, ok := splitDirection("diagonal"); ok {
		t.Error("diagonal accepted")
	}
}

func TestWebWorkspaceVersionGuardAndPersistence(t *testing.T) {
	path := filepath.Join(t.TempDir(), "web-workspaces.json")
	st := newWebWSStore(path)
	w, err := st.create(workspaceCreate{Name: "api", Meta: json.RawMessage(`{}`)})
	if err != nil || w.Version != 1 || !strings.HasPrefix(w.ID, "w_") {
		t.Fatalf("create = %+v %v", w, err)
	}
	layout := json.RawMessage(`{"kind":"pane","pane_id":"term_1"}`)
	w2, err := st.put(w.ID, workspacePut{Version: 1, Layout: layout})
	if err != nil || w2.Version != 2 {
		t.Fatalf("put v1 = %+v %v", w2, err)
	}
	cur, err := st.put(w.ID, workspacePut{Version: 1, Layout: json.RawMessage(`null`)})
	if err != errVersion || cur.Version != 2 || string(cur.Layout) != string(layout) {
		t.Fatalf("stale put must conflict and return the current doc: %+v %v", cur, err)
	}
	if again, ok := newWebWSStore(path).get(w.ID); !ok || again.Version != 2 || string(again.Layout) != string(layout) {
		t.Fatalf("reload = %+v", again)
	}
}

// F1: meta round-trips opaque, a put without meta keeps it, and a pre-F1
// flat row (layout, no meta) becomes a header + itself as the screen, once.
func TestWebWorkspaceMetaAndFlatMigration(t *testing.T) {
	path := filepath.Join(t.TempDir(), "web-workspaces.json")
	st := newWebWSStore(path)
	meta := json.RawMessage(`{"parent_id":"w_h","cwd":"/srv/app","sort_order":2}`)
	w, err := st.create(workspaceCreate{Name: "shell", Meta: meta, Layout: json.RawMessage(`{"kind":"pane"}`)})
	if err != nil || string(w.Meta) != string(meta) || len(w.Layout) == 0 {
		t.Fatalf("create = %+v %v", w, err)
	}
	w2, err := st.put(w.ID, workspacePut{Version: 1, Name: ptr("renamed")})
	if err != nil || string(w2.Meta) != string(meta) {
		t.Fatalf("a put without meta must keep it: %+v %v", w2, err)
	}
	flat, _ := st.create(workspaceCreate{Name: "old", Layout: json.RawMessage(`{"kind":"pane","pane_id":"p1"}`)})
	header, _ := st.create(workspaceCreate{Name: "empty"}) // no layout: not a screen, left alone
	re := newWebWSStore(path)
	all := re.list()
	if len(all) != 4 {
		t.Fatalf("after migration: %d rows, want 4: %+v", len(all), all)
	}
	var h, moved WebWorkspace
	for i, x := range all {
		if x.ID == flat.ID {
			moved, h = x, all[i-1]
		}
	}
	if h.Name != "old" || string(h.Meta) != `{}` || len(h.Layout) != 0 {
		t.Fatalf("header = %+v", h)
	}
	if string(moved.Meta) != `{"parent_id":"`+h.ID+`"}` || string(moved.Layout) != string(flat.Layout) || moved.Version != 2 {
		t.Fatalf("moved = %+v", moved)
	}
	if e, _ := re.get(header.ID); len(e.Meta) != 0 {
		t.Fatalf("a paneless row was migrated: %+v", e)
	}
	if n := len(newWebWSStore(path).list()); n != 4 {
		t.Fatalf("migration ran twice: %d rows", n)
	}
}

func TestWebGroupsVersionGuard(t *testing.T) {
	path := filepath.Join(t.TempDir(), "web-workspaces.json")
	st := newWebWSStore(path)
	if g := st.getGroups(); g.Version != 0 || string(g.Groups) != `[]` {
		t.Fatalf("empty = %+v", g)
	}
	g, err := st.putGroups(WebGroups{Version: 0, Groups: json.RawMessage(`[{"id":"g_1","name":"work"}]`)})
	if err != nil || g.Version != 1 {
		t.Fatalf("put = %+v %v", g, err)
	}
	if cur, err := st.putGroups(WebGroups{Version: 0, Groups: json.RawMessage(`[]`)}); err != errVersion || cur.Version != 1 {
		t.Fatalf("stale put = %+v %v", cur, err)
	}
	if again := newWebWSStore(path).getGroups(); again.Version != 1 || string(again.Groups) != `[{"id":"g_1","name":"work"}]` {
		t.Fatalf("reload = %+v", again)
	}
}

func ptr[T any](v T) *T { return &v }

func TestWebWorkspacesREST(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	if w := do(s, "POST", "/api/v2/web/workspaces", "owner-token", `{}`); w.Code != http.StatusBadRequest {
		t.Errorf("no name → %d", w.Code)
	}
	w := do(s, "POST", "/api/v2/web/workspaces", "owner-token", `{"name":"api"}`)
	var ws WebWorkspace
	_ = json.Unmarshal(w.Body.Bytes(), &ws)
	if w.Code != http.StatusCreated || ws.ID == "" {
		t.Fatalf("create → %d %s", w.Code, w.Body)
	}
	if w := do(s, "PUT", "/api/v2/web/workspaces/"+ws.ID, "owner-token", `{"version":1,"layout":[1,2]}`); w.Code != http.StatusBadRequest {
		t.Errorf("non-object layout → %d", w.Code)
	}
	if w := do(s, "PUT", "/api/v2/web/workspaces/"+ws.ID, "owner-token", `{"version":1,"tabs_mode":true}`); w.Code != http.StatusOK {
		t.Errorf("put → %d %s", w.Code, w.Body)
	}
	if w := do(s, "PUT", "/api/v2/web/workspaces/"+ws.ID, "owner-token", `{"version":1,"tabs_mode":false}`); w.Code != http.StatusConflict ||
		!strings.Contains(w.Body.String(), `"version":2`) {
		t.Errorf("stale put → %d %s, want 409 with the current doc", w.Code, w.Body)
	}
	if w := do(s, "GET", "/api/v2/web/workspaces", "owner-token", ""); !strings.Contains(w.Body.String(), ws.ID) {
		t.Errorf("list → %s", w.Body)
	}
	// F1: meta must be an object; groups are a versioned array.
	if w := do(s, "PUT", "/api/v2/web/workspaces/"+ws.ID, "owner-token", `{"version":2,"meta":"x"}`); w.Code != http.StatusBadRequest {
		t.Errorf("non-object meta → %d", w.Code)
	}
	if w := do(s, "POST", "/api/v2/web/workspaces", "owner-token", `{"name":"s","meta":{"parent_id":"`+ws.ID+`"},"layout":{"kind":"pane"}}`); w.Code != http.StatusCreated ||
		!strings.Contains(w.Body.String(), `"meta":{"parent_id":"`+ws.ID+`"}`) {
		t.Errorf("create with meta → %d %s", w.Code, w.Body)
	}
	if w := do(s, "PUT", "/api/v2/web/groups", "owner-token", `{"version":0,"groups":{}}`); w.Code != http.StatusBadRequest {
		t.Errorf("non-array groups → %d", w.Code)
	}
	if w := do(s, "PUT", "/api/v2/web/groups", "owner-token", `{"version":0,"groups":[{"id":"g_1"}]}`); w.Code != http.StatusOK {
		t.Errorf("groups put → %d %s", w.Code, w.Body)
	}
	if w := do(s, "GET", "/api/v2/web/groups", "owner-token", ""); !strings.Contains(w.Body.String(), `"version":1`) {
		t.Errorf("groups get → %s", w.Body)
	}
	if w := do(s, "DELETE", "/api/v2/web/workspaces/"+ws.ID, "owner-token", ""); w.Code != http.StatusNoContent {
		t.Errorf("delete → %d", w.Code)
	}
	if w := do(s, "GET", "/api/v2/web/workspaces/"+ws.ID, "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("get deleted → %d", w.Code)
	}
}

// b5Fixture: workspace A with panes a1 + a2, workspace B with b1.
type b5Fixture struct {
	s          *Service
	calls      *[][]string
	wsA, wsB   string
	a1, a2, b1 hookEntry
}

func newB5(t *testing.T) b5Fixture {
	t.Helper()
	s, calls := hookService("tmux 3.4")
	s.hooks.SetHookAddr("127.0.0.1:9")
	mk := func(name string) string {
		w := do(s, "POST", "/api/v2/web/workspaces", "owner-token", `{"name":"`+name+`"}`)
		var ws WebWorkspace
		_ = json.Unmarshal(w.Body.Bytes(), &ws)
		return ws.ID
	}
	f := b5Fixture{s: s, calls: calls, wsA: mk("A"), wsB: mk("B")}
	sess := func(name, ws string) hookEntry {
		w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"name":"`+name+`","workspace_id":"`+ws+`"}`)
		var resp map[string]any
		_ = json.Unmarshal(w.Body.Bytes(), &resp)
		pane, _ := resp["pane_id"].(string)
		e, ok := s.hooks.byPane(pane)
		if w.Code != http.StatusCreated || !ok || resp["workspace_id"] != ws {
			t.Fatalf("create %s → %d %s", name, w.Code, w.Body)
		}
		return e
	}
	f.a1, f.a2, f.b1 = sess("a1", f.wsA), sess("a2", f.wsA), sess("b1", f.wsB)
	layout := `{"kind":"split","split_id":"sp_0","direction":"horizontal","ratio":0.5,` +
		`"first":{"kind":"pane","pane_id":"` + f.a1.paneID + `"},"second":{"kind":"pane","pane_id":"` + f.a2.paneID + `"}}`
	if w := do(s, "PUT", "/api/v2/web/workspaces/"+f.wsA, "owner-token", `{"version":1,"layout":`+layout+`}`); w.Code != http.StatusOK {
		t.Fatalf("seed layout → %d %s", w.Code, w.Body)
	}
	return f
}

func (f b5Fixture) as(t *testing.T, e hookEntry) termHookTarget { return matched(t, f.s.hooks, &e) }

func TestCreateRejectsUnknownWorkspace(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	if w := do(s, "POST", "/api/v2/term/sessions", "owner-token", `{"workspace_id":"w_nope"}`); w.Code != http.StatusBadRequest {
		t.Errorf("unknown workspace → %d", w.Code)
	}
}

func TestTreeVerbs(t *testing.T) {
	f := newB5(t)
	res, err := call(t, f.as(t, f.a1), "tree", map[string]any{})
	if err != nil || asMap(t, res)["workspace_id"] != f.wsA || !strings.Contains(mustJSON(res), f.a2.paneID) {
		t.Fatalf("tree → %v %+v", res, err)
	}
	res, _ = call(t, f.as(t, f.b1), "ui.tree", map[string]any{})
	m := asMap(t, res)
	if m["active_workspace_id"] != f.wsB || !strings.Contains(mustJSON(res), `"kind":"terminal"`) {
		t.Errorf("ui.tree → %s", mustJSON(res))
	}
}

func mustJSON(v any) string { b, _ := json.Marshal(v); return string(b) }

func TestSendIsFencedToTheWorkspace(t *testing.T) {
	f := newB5(t)
	res, err := call(t, f.as(t, f.a1), "send", map[string]any{"pane_id": f.a2.paneID, "data": "ls\r"})
	if err != nil || asMap(t, res)["bytes"] != float64(3) {
		t.Fatalf("send within workspace → %v %+v", res, err)
	}
	last := (*f.calls)[len(*f.calls)-1]
	if strings.Join(last[:5], " ") != "tmux send-keys -t =a2: -H" || strings.Join(last[5:], " ") != "6c 73 d" {
		t.Errorf("send argv = %v", last)
	}
	if _, err := call(t, f.as(t, f.a1), "send", map[string]any{"pane_id": f.b1.paneID, "data": "x"}); err == nil ||
		!strings.Contains(err.Message, "not in this session's workspace") {
		t.Errorf("send across workspaces → %+v, want refused", err)
	}
	if _, err := call(t, f.as(t, f.a1), "send", map[string]any{"pane_id": "term_0000000000000000", "data": "x"}); err == nil ||
		!strings.Contains(err.Message, "not connected") {
		t.Errorf("send to unknown pane → %+v", err)
	}
	res, _ = call(t, f.as(t, f.a1), "send-key", map[string]any{"pane_id": f.a2.paneID, "key": "Enter"})
	if asMap(t, res)["bytes"] != float64(1) {
		t.Errorf("send-key enter → %v", res)
	}
}

func TestSplitCreatesASessionInTheWorkspace(t *testing.T) {
	f := newB5(t)
	res, err := call(t, f.as(t, f.a2), "split", map[string]any{"direction": "down"})
	if err != nil {
		t.Fatalf("split: %+v", err)
	}
	m := asMap(t, res)
	newPane, _ := m["pane_id"].(string)
	if m["split_from"] != f.a2.paneID || !paneIDShape.MatchString(newPane) || m["workspace_id"] != f.wsA {
		t.Fatalf("split → %v", m)
	}
	ne, ok := f.s.hooks.byPane(newPane)
	if !ok || ne.workspaceID != f.wsA || ne.policy != f.a2.policy {
		t.Errorf("new session must inherit workspace + policy: %+v", ne)
	}
	ws, _ := f.s.hooks.webws.get(f.wsA)
	root, _ := parseLayout(ws.Layout)
	if findLeaf(root, newPane) == nil || len(paneIDs(root)) != 3 || ws.Version != 3 {
		t.Errorf("layout after split (v%d): %s", ws.Version, ws.Layout)
	}
	if _, err := call(t, f.as(t, f.a1), "split", map[string]any{"kind": "browser"}); err == nil {
		t.Error("a browser-kind split must be refused in browser mode")
	}
	if _, err := call(t, f.as(t, f.a1), "split", map[string]any{"pane_id": f.b1.paneID}); err == nil {
		t.Error("splitting another workspace's pane must be refused")
	}
}

func TestPaneTitleAndAnnotation(t *testing.T) {
	f := newB5(t)
	if _, err := call(t, f.as(t, f.a1), "set-pane-title", map[string]any{"pane_id": f.a2.paneID, "title": "server"}); err != nil {
		t.Fatalf("title: %+v", err)
	}
	ws, _ := f.s.hooks.webws.get(f.wsA)
	root, _ := parseLayout(ws.Layout)
	if findLeaf(root, f.a2.paneID)["title"] != "server" {
		t.Errorf("title not set: %s", ws.Layout)
	}
	call(t, f.as(t, f.a1), "set-pane-title", map[string]any{"pane_id": f.a2.paneID, "title": ""})
	ws, _ = f.s.hooks.webws.get(f.wsA)
	root, _ = parseLayout(ws.Layout)
	if _, has := findLeaf(root, f.a2.paneID)["title"]; has {
		t.Error("an empty title must clear the field, as the desktop does")
	}
	if _, err := call(t, f.as(t, f.a1), "set-pane-annotation", map[string]any{"pane_id": f.b1.paneID, "annotation": "x"}); err == nil {
		t.Error("annotating another workspace's pane must be refused")
	}
}

func TestScrollbackStaysAStub(t *testing.T) {
	f := newB5(t)
	_, err := call(t, f.as(t, f.a1), "pane.scrollback", map[string]any{"pane_id": f.a1.paneID})
	if err == nil || err.Message != scrollbackStub || !strings.HasPrefix(err.Message, "pane.scrollback: backend does not buffer PTY content (Absolute Rule #1).") {
		t.Errorf("scrollback → %+v", err)
	}
}
