package chat

// The session status state machine: transition() is the one writer, and a
// terminal status (killed / stopped / error) is never left.

import (
	"encoding/json"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
)

// waitForEvent blocks until the subscriber receives an event of the given
// type, failing the test after 5 s.
func waitForEvent(t *testing.T, sub *subscriber, typ string) map[string]any {
	t.Helper()
	deadline := time.After(5 * time.Second)
	for {
		select {
		case ev, ok := <-sub.ch:
			if !ok {
				t.Fatalf("subscriber closed before a %q event", typ)
			}
			var m map[string]any
			_ = json.Unmarshal(ev, &m)
			if m["type"] == typ {
				return m
			}
		case <-deadline:
			t.Fatalf("no %q event within 5s", typ)
		}
	}
}

// A hook waiter woken because the process died must not write the pre-hook
// status back over the terminal one.
func TestHookRestoreDoesNotReviveDeadSession(t *testing.T) {
	s, sub := newTestSession(t)
	done := make(chan string, 1)
	go func() {
		done <- s.awaitHookDecision(&feedPushParams{RequestID: "req_dead", WaitTimeoutSeconds: 30}, true)
	}()
	waitForEvent(t, sub, "hook_request")
	if got := s.getStatus(); got != stWaitingHook {
		t.Fatalf("status while parked = %q, want waiting_hook", got)
	}

	// What waitExit does when the process dies.
	s.setStatus(stError)
	s.failPendingHooks()

	select {
	case d := <-done:
		if d != "deny" {
			t.Fatalf("decision = %q, want deny", d)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("hook waiter never returned")
	}
	if got := s.getStatus(); got != stError {
		t.Fatalf("status after the woken waiter = %q, want error", got)
	}
	row, err := s.mgr.store.getSession(s.id)
	if err != nil {
		t.Fatalf("getSession: %v", err)
	}
	if row.Status != stError {
		t.Fatalf("row status = %q, want error", row.Status)
	}
}

// The restore still happens when nothing else touched the status.
func TestHookRestoreAfterDecision(t *testing.T) {
	s, sub := newTestSession(t)
	done := make(chan string, 1)
	go func() {
		done <- s.awaitHookDecision(&feedPushParams{RequestID: "req_ok", WaitTimeoutSeconds: 30}, true)
	}()
	waitForEvent(t, sub, "hook_request")
	s.resolveHook("req_ok", "allow")
	select {
	case d := <-done:
		if d != "allow" {
			t.Fatalf("decision = %q, want allow", d)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("hook waiter never returned")
	}
	if got := s.getStatus(); got != stActive {
		t.Fatalf("status after the decision = %q, want active", got)
	}
}

// Output that arrives after the session was stopped must not mark it alive.
func TestLateOutputDoesNotLeaveTerminal(t *testing.T) {
	for _, line := range []string{
		`{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"late"}]}}`,
		`{"type":"result","subtype":"success","is_error":false,"result":"late"}`,
	} {
		s, _ := newTestSession(t)
		s.setStatus(stKilled)
		s.handleClaudeLine([]byte(line))
		if got := s.getStatus(); got != stKilled {
			t.Fatalf("status after a late line = %q, want killed", got)
		}
		row, err := s.mgr.store.getSession(s.id)
		if err != nil {
			t.Fatalf("getSession: %v", err)
		}
		if row.Status != stKilled {
			t.Fatalf("row status = %q, want killed", row.Status)
		}
	}
}

func TestTransitionGuards(t *testing.T) {
	s, _ := newTestSession(t)
	if s.transition(stWaitingInput, stWaitingHook) {
		t.Fatal("a from-guarded transition applied from the wrong status")
	}
	if !s.transition(stStopped, "") {
		t.Fatal("active → stopped refused")
	}
	for _, to := range []string{stActive, stWaitingInput, stWaitingHook, stKilled, stError} {
		if s.transition(to, "") {
			t.Fatalf("left terminal status for %q", to)
		}
	}
	if got := s.getStatus(); got != stStopped {
		t.Fatalf("status = %q, want stopped", got)
	}
}

// create() must store the row with the status the session really has, not
// a `starting` that nothing ever updates.
func TestCreateRowCarriesLiveStatus(t *testing.T) {
	cat, err := exec.LookPath("cat")
	if err != nil {
		t.Skip("no cat on this machine")
	}
	store, err := OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("OpenChatStore: %v", err)
	}
	t.Cleanup(store.Close)
	m := NewSessionManager(store)
	m.cfg.claudeBin = cat // reads stdin until EOF, like `claude -p`
	m.cfg.baseArgs = nil

	s, err := m.create(startSpec{})
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	sub := s.addSubscriber()
	row, err := store.getSession(s.id)
	if err != nil {
		t.Fatalf("getSession: %v", err)
	}
	if row.Status != stActive {
		t.Fatalf("row status after create = %q, want active", row.Status)
	}

	s.stop("test")
	if ev := waitForEvent(t, sub, "status"); ev["status"] != stKilled {
		t.Fatalf("exit status event = %v, want killed", ev["status"])
	}
	if got := s.getStatus(); got != stKilled {
		t.Fatalf("status after exit = %q, want killed", got)
	}
}
