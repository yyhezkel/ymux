package term

// Phase 102 (WEB-DESIGN B4): set-status, notify, note-*, ports.

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ymux-server/internal/core"
)

func call(t *testing.T, tg termHookTarget, method string, params map[string]any) (any, *core.RPCError) {
	t.Helper()
	raw, _ := json.Marshal(params)
	return tg.DispatchHook(method, raw)
}

func asMap(t *testing.T, v any) map[string]any {
	t.Helper()
	b, _ := json.Marshal(v)
	var m map[string]any
	if err := json.Unmarshal(b, &m); err != nil {
		t.Fatalf("not an object: %s", b)
	}
	return m
}

func TestSetStatusOwnPaneOnly(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	sub := r.hub.add("en")
	if _, err := call(t, tg, "set-status", map[string]any{"pane_id": e.paneID, "text": "building"}); err != nil {
		t.Fatalf("own pane: %+v", err)
	}
	f := nextOf(t, sub, "pane:status")
	if !strings.Contains(string(f.Data), `"text":"building"`) {
		t.Errorf("pane:status = %s", f.Data)
	}
	if got := r.hello("en").PaneStatus[e.paneID]; got != "building" {
		t.Errorf("hello pane_status = %q", got)
	}
	if _, err := call(t, tg, "set-status", map[string]any{"pane_id": "term_other", "text": "x"}); err == nil {
		t.Error("set-status for another pane must be refused")
	}
	// The older `pane` spelling still works.
	if _, err := call(t, tg, "set-status", map[string]any{"pane": e.paneID, "text": ""}); err != nil {
		t.Errorf("pane spelling: %+v", err)
	}
}

func TestNotify(t *testing.T) {
	r, _, tg := gateEntry(t, policyNone)
	sub := r.hub.add("en")
	res, err := call(t, tg, "notify", map[string]any{"title": "Build", "body": "green"})
	if err != nil || asMap(t, res)["ok"] != true {
		t.Fatalf("notify → %v %+v", res, err)
	}
	f := nextOf(t, sub, "notification:new")
	var it NotificationItem
	_ = json.Unmarshal(f.Data, &it)
	if it.Title != "Build" || it.Body != "green" || it.Kind != "agent" || it.Session != "web" || it.ID == 0 {
		t.Errorf("notification = %+v", it)
	}
	call(t, tg, "notify", map[string]any{})
	if n := r.hello("en").Notifications; len(n) != 2 || n[1].Title != "(no title)" {
		t.Errorf("hello notifications = %+v", n)
	}
}

func TestNotesThroughTheRPC(t *testing.T) {
	r, e, tg := gateEntry(t, policyNone)
	r.notes = newNoteStore(filepath.Join(t.TempDir(), "notes.json"))
	sub := r.hub.add("en")

	res, err := call(t, tg, "note-add", map[string]any{"text": "check the cache", "tag": "todo"})
	if err != nil {
		t.Fatalf("note-add: %+v", err)
	}
	n := asMap(t, res)
	id, _ := n["id"].(string)
	if !strings.HasPrefix(id, "n_") || n["status"] != "open" || n["pane_id"] != e.paneID || n["tag"] != "todo" {
		t.Fatalf("added = %v", n)
	}
	nextOf(t, sub, "notes:changed")

	if _, err := call(t, tg, "note-add", map[string]any{}); err == nil {
		t.Error("note-add without text must fail")
	}
	// tag: null clears (presence vs absence), as the desktop's note-update.
	res, _ = call(t, tg, "note-update", map[string]any{"id": id, "tag": nil, "text": "check the CDN cache"})
	if m := asMap(t, res); m["tag"] != nil || m["text"] != "check the CDN cache" {
		t.Errorf("update with tag:null = %v", m)
	}
	res, _ = call(t, tg, "note-done", map[string]any{"id": id})
	if asMap(t, res)["status"] != "done" {
		t.Errorf("note-done = %v", res)
	}
	res, _ = call(t, tg, "note-list", map[string]any{"status": "done"})
	if b, _ := json.Marshal(res); !strings.Contains(string(b), id) {
		t.Errorf("note-list done = %s", b)
	}
	if _, err := call(t, tg, "note-delete", map[string]any{"id": "n_nope"}); err == nil || !strings.Contains(err.Message, "no note") {
		t.Errorf("delete unknown → %+v", err)
	}
	if _, err := call(t, tg, "note-delete", map[string]any{"id": id}); err != nil {
		t.Errorf("delete: %+v", err)
	}
}

func TestNotesPersistAtomically(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "notes.json")
	s := newNoteStore(path)
	n, err := s.add("keep me", "", "", "")
	if err != nil {
		t.Fatal(err)
	}
	if again := newNoteStore(path).list("", "", "", 0); len(again) != 1 || again[0].ID != n.ID {
		t.Fatalf("reload = %+v", again)
	}
	entries, _ := os.ReadDir(dir)
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".tmp") {
			t.Errorf("a tmp file was left behind: %s", e.Name())
		}
	}
	// A corrupt file is moved aside, never overwritten.
	_ = os.WriteFile(path, []byte("{not json"), 0o600)
	if got := newNoteStore(path).list("", "", "", 0); len(got) != 0 {
		t.Errorf("corrupt file should load empty, got %d", len(got))
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Error("the corrupt file must have been moved aside")
	}
}

func TestNotesREST(t *testing.T) {
	s, _ := hookService("tmux 3.4")
	w := do(s, "POST", "/api/v2/notes", "owner-token", `{"text":"from the browser"}`)
	if w.Code != http.StatusCreated {
		t.Fatalf("create → %d %s", w.Code, w.Body)
	}
	var n Note
	_ = json.Unmarshal(w.Body.Bytes(), &n)
	if w := do(s, "PATCH", "/api/v2/notes/"+n.ID, "owner-token", `{"status":"done"}`); w.Code != http.StatusOK {
		t.Errorf("patch → %d", w.Code)
	}
	if w := do(s, "PATCH", "/api/v2/notes/"+n.ID, "owner-token", `{"status":"later"}`); w.Code != http.StatusBadRequest {
		t.Errorf("bad status → %d", w.Code)
	}
	if w := do(s, "GET", "/api/v2/notes?status=done", "owner-token", ""); !strings.Contains(w.Body.String(), n.ID) {
		t.Errorf("list → %s", w.Body)
	}
	if w := do(s, "DELETE", "/api/v2/notes/"+n.ID, "owner-token", ""); w.Code != http.StatusNoContent {
		t.Errorf("delete → %d", w.Code)
	}
	if w := do(s, "DELETE", "/api/v2/notes/"+n.ID, "owner-token", ""); w.Code != http.StatusNotFound {
		t.Errorf("delete again → %d", w.Code)
	}
	if w := do(s, "DELETE", "/api/v2/notifications", "owner-token", ""); w.Code != http.StatusNoContent {
		t.Errorf("clear notifications → %d", w.Code)
	}
}

// The CLI's port_watch.rs test vectors, verbatim.
const procHeader = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"

func TestParseProcNetTCP(t *testing.T) {
	cases := []struct {
		name, line string
		v6         bool
		want       []ListenPort
	}{
		{"v4 loopback", "   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 12345 1", false,
			[]ListenPort{{"127.0.0.1", 3000, "v4"}}},
		{"v6 loopback", "   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 99 1", true,
			[]ListenPort{{"::1", 8080, "v6"}}},
		{"established skipped", "   0: 0100007F:0BB8 0100007F:C001 01 00000000:00000000 00:00000000 00000000  1000 0 12345 1", false, nil},
		{"bind any", "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 777 1", false,
			[]ListenPort{{"0.0.0.0", 8080, "v4"}}},
		{"LAN ip skipped", "   0: 0101A8C0:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 777 1", false, nil},
	}
	for _, c := range cases {
		got := parseProcNetTCP(procHeader+"\n"+c.line, c.v6)
		if len(got) != len(c.want) || (len(got) == 1 && got[0] != c.want[0]) {
			t.Errorf("%s: got %+v want %+v", c.name, got, c.want)
		}
	}
	if got := parseProcNetTCP("", false); len(got) != 0 {
		t.Errorf("empty body → %+v", got)
	}
}

func TestShouldReport(t *testing.T) {
	ex := map[uint16]bool{9000: true}
	for port, want := range map[uint16]bool{80: false, 22: false, 9000: false, 3000: true} {
		if got := shouldReport(port, ex); got != want {
			t.Errorf("shouldReport(%d) = %v", port, got)
		}
	}
}

func TestPortWatchDiffsAndSkipsOwnPorts(t *testing.T) {
	hub := newEventHub()
	sub := hub.add("en")
	w := newPortWatch(hub, func() []uint16 { return []uint16{7879} })
	line := func(hexPort string) string {
		return "   0: 0100007F:" + hexPort + " 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 1 1"
	}
	// 3000 (0BB8) and the daemon's own 7879 (1EC7) listening.
	w.apply(w.scan(procHeader+"\n"+line("0BB8")+"\n"+line("1EC7"), ""))
	if l := w.list(); len(l) != 1 || l[0].RemotePort != 3000 {
		t.Fatalf("after first scan: %+v (the daemon's own port must be skipped)", l)
	}
	if f := nextOf(t, sub, "port-detected"); !strings.Contains(string(f.Data), `"remote_port":3000`) {
		t.Errorf("port-detected = %s", f.Data)
	}
	// Same scan again: nothing new announced.
	w.apply(w.scan(procHeader+"\n"+line("0BB8"), ""))
	select {
	case b := <-sub.ch:
		t.Errorf("an unchanged port was announced again: %s", b)
	default:
	}
	// 3000 gone.
	w.apply(w.scan(procHeader+"\n", ""))
	if f := nextOf(t, sub, "port-undetected"); !strings.Contains(string(f.Data), `"remote_port":3000`) {
		t.Errorf("port-undetected = %s", f.Data)
	}
}

func TestPortRPCFeedsTheSameSet(t *testing.T) {
	r, _, tg := gateEntry(t, policyNone)
	r.ports = newPortWatch(r.hub, func() []uint16 { return nil })
	if _, err := call(t, tg, "port.opened", map[string]any{"workspace_id": "w", "port": 5173}); err != nil {
		t.Fatalf("port.opened: %+v", err)
	}
	if l := r.hello("en").Ports; len(l) != 1 || l[0].RemotePort != 5173 || l[0].Addr != "127.0.0.1" {
		t.Errorf("hello ports = %+v", l)
	}
	call(t, tg, "port.closed", map[string]any{"workspace_id": "w", "port": 5173})
	if l := r.hello("en").Ports; len(l) != 0 {
		t.Errorf("after port.closed = %+v", l)
	}
	if _, err := call(t, tg, "port.opened", map[string]any{}); err == nil {
		t.Error("port.opened without a port must fail")
	}
}
