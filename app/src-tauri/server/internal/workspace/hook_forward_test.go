package workspace

import (
	"encoding/json"
	"path/filepath"
	"testing"
)

// A desktop-forwarded hook (B path) publishes on a virtual session with no
// subscriber, so it fans out to paired devices; the desktop then polls
// HookResolution for the phone's decision.
func TestForwardHookPushesThenResolves(t *testing.T) {
	notif := &mockNotifier{ch: make(chan struct{}, 8)}
	st, err := OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	m := NewManager(st, notif)
	m.SetPushLister(mockLister{ids: []string{"dev_a"}})

	if err := m.ForwardHook(
		"desktop-ws1-pane1", "req-1",
		json.RawMessage(`{"req_id":"req-1","origin":"desktop","tool_name":"Bash"}`), 0,
	); err != nil {
		t.Fatalf("ForwardHook: %v", err)
	}
	waitFor(t, notif.ch, 1)
	if got := notif.count(); got != 1 {
		t.Fatalf("want 1 push for the forwarded hook, got %d", got)
	}

	// Unresolved until a winner-takes-all decision lands.
	if _, resolved := m.HookResolution("req-1"); resolved {
		t.Fatal("hook should be unresolved before any decision")
	}
	won, err := m.ResolveHook("req-1", "dev_a", "allow")
	if err != nil || !won {
		t.Fatalf("first resolve should win: won=%v err=%v", won, err)
	}
	dec, resolved := m.HookResolution("req-1")
	if !resolved || dec != "allow" {
		t.Fatalf("HookResolution = %q resolved=%v, want allow/true", dec, resolved)
	}
}

// timeout_at is a unix-seconds deadline (0 = none). Past it, the request is
// neither listed as pending nor answerable: a late phone answer must not win.
func TestExpiredPendingIsNotListedOrResolvable(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	payload := json.RawMessage(`{"tool_name":"Bash"}`)

	if err := m.CreateHookRequest(se.ID, "req-expired", payload, now()-5); err != nil {
		t.Fatal(err)
	}
	if err := m.CreateHookRequest(se.ID, "req-live", payload, now()+600); err != nil {
		t.Fatal(err)
	}
	if err := m.CreateHookRequest(se.ID, "req-forever", payload, 0); err != nil {
		t.Fatal(err)
	}

	pending, err := m.ListPending(se.ID)
	if err != nil {
		t.Fatal(err)
	}
	got := map[string]bool{}
	for _, p := range pending {
		got[p.ReqID] = true
	}
	if got["req-expired"] || !got["req-live"] || !got["req-forever"] || len(pending) != 2 {
		t.Fatalf("pending = %v, want req-live + req-forever only", got)
	}

	won, err := m.ResolveHook("req-expired", "dev_a", "allow")
	if err != nil || won {
		t.Fatalf("a late answer won an expired request: won=%v err=%v", won, err)
	}
	if _, resolved := m.HookResolution("req-expired"); resolved {
		t.Fatal("an expired request reports a resolution")
	}
	for _, id := range []string{"req-live", "req-forever"} {
		if won, err := m.ResolveHook(id, "dev_a", "allow"); err != nil || !won {
			t.Fatalf("%s: won=%v err=%v, want a win", id, won, err)
		}
	}
}
