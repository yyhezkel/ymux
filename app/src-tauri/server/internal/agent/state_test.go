package agent

// Translated one-for-one from `agent_run_tests` in app/src-tauri/src/lib.rs
// (same names, same assertions). Keep the two suites in step.

import (
	"testing"
	"time"
)

var t0 = time.Unix(1_700_000_000, 0)

func TestAverageExcludesShortTurnsAndMeansTheRest(t *testing.T) {
	var r Run
	if _, ok := r.AvgMs(); ok {
		t.Fatal("no turns yet")
	}
	r.RecordTurn(1_000) // < 2s → text-only, excluded
	if r.Count != 0 {
		t.Fatalf("count = %d, want 0", r.Count)
	}
	if _, ok := r.AvgMs(); ok {
		t.Fatal("a short turn must not produce an average")
	}
	r.RecordTurn(40_000)
	r.RecordTurn(20_000)
	if r.Count != 2 {
		t.Fatalf("count = %d, want 2", r.Count)
	}
	if avg, ok := r.AvgMs(); !ok || avg != 30_000 {
		t.Fatalf("avg = %d,%v want 30000,true", avg, ok)
	}
}

func TestStartedAtMsNoneWhenIdle(t *testing.T) {
	var r Run
	if _, ok := r.StartedAtMs(); ok {
		t.Fatal("idle run must have no start")
	}
	r.TurnStartedAt = time.Unix(0, 0)
	if ms, ok := r.StartedAtMs(); !ok || ms != 0 {
		t.Fatalf("started_at = %d,%v want 0,true", ms, ok)
	}
}

func TestAPaneWithNoHooksHasNoState(t *testing.T) {
	var r Run
	if r.CurrentState() != StateUnknown {
		t.Fatalf("state = %q, want unknown", r.CurrentState())
	}
}

func TestTheTurnCycleWalksRunningThenDone(t *testing.T) {
	var r Run
	if !r.ApplyHook("user-prompt-submit", "", t0) || r.CurrentState() != StateRunning {
		t.Fatalf("after prompt: %q", r.CurrentState())
	}
	if !r.ApplyHook("stop", "", t0) || r.CurrentState() != StateDone {
		t.Fatalf("after stop: %q", r.CurrentState())
	}
}

func TestAStopFailureTurnsTheLightFailed(t *testing.T) {
	// A turn that died on an API error must not read as a clean Done.
	var r Run
	r.ApplyHook("user-prompt-submit", "", t0)
	if !r.ApplyHook("stop-failure", "", t0) || r.CurrentState() != StateFailed {
		t.Fatalf("after stop-failure: %q", r.CurrentState())
	}
	if string(r.CurrentState()) != "failed" {
		t.Fatalf("wire value %q", r.CurrentState())
	}
	if r.ApplyHook("stop-failure", "", t0) {
		t.Fatal("already Failed must report no change")
	}
}

func TestAPromptAfterAFailureResumesRunning(t *testing.T) {
	// Failed is not sticky: a retried turn must not keep showing red.
	var r Run
	r.ApplyHook("stop-failure", "", t0)
	if !r.ApplyHook("user-prompt-submit", "", t0) || r.CurrentState() != StateRunning {
		t.Fatalf("after prompt: %q", r.CurrentState())
	}
}

func TestIdlePromptPromotesDoneToNeedsInput(t *testing.T) {
	var r Run
	r.ApplyHook("stop", "", t0)
	if !r.ApplyHook("notification", "idle_prompt", t0) || r.CurrentState() != StateNeedsInput {
		t.Fatalf("state = %q, want needs-input", r.CurrentState())
	}
}

func TestEveryDocumentedBlockingNotificationMeansNeedsInput(t *testing.T) {
	for _, nt := range []string{
		"permission_prompt",
		"idle_prompt",
		"agent_needs_input",
		"elicitation_dialog",
		"elicitation_url_dialog",
	} {
		var r Run
		r.ApplyHook("user-prompt-submit", "", t0)
		if !r.ApplyHook("notification", nt, t0) {
			t.Errorf("%s must transition", nt)
		}
		if r.CurrentState() != StateNeedsInput {
			t.Errorf("%s: state = %q", nt, r.CurrentState())
		}
	}
}

func TestAnsweringAnElicitationResumesRunning(t *testing.T) {
	var r Run
	r.ApplyHook("notification", "elicitation_dialog", t0)
	if !r.ApplyHook("notification", "elicitation_response", t0) || r.CurrentState() != StateRunning {
		t.Fatalf("state = %q, want running", r.CurrentState())
	}
}

func TestAnUnmappedNotificationChangesNothing(t *testing.T) {
	var r Run
	r.ApplyHook("stop", "", t0)
	seqBefore := r.Seq
	for _, nt := range []string{"auth_success", "something_invented_in_2027", ""} {
		if r.ApplyHook("notification", nt, t0) {
			t.Errorf("%q must not transition", nt)
		}
	}
	if r.CurrentState() != StateDone {
		t.Fatalf("state must be untouched, got %q", r.CurrentState())
	}
	if r.Seq != seqBefore {
		t.Fatalf("a no-op must not bump seq: %d → %d", seqBefore, r.Seq)
	}
}

func TestAStopArrivingAfterANotificationStillWins(t *testing.T) {
	var r Run
	r.ApplyHook("notification", "permission_prompt", t0)
	r.ApplyHook("stop", "", t0)
	if r.CurrentState() != StateDone || r.Seq != 2 {
		t.Fatalf("state=%q seq=%d, want done,2", r.CurrentState(), r.Seq)
	}
}

func TestALongTurnDoesNotKeepResettingItsOwnClock(t *testing.T) {
	var r Run
	if !r.ApplyHook("user-prompt-submit", "", t0) {
		t.Fatal("prompt must transition")
	}
	first := r.StateSince
	if first.IsZero() {
		t.Fatal("state_since must be stamped")
	}
	for i := 1; i <= 5; i++ {
		if r.ApplyHook("pre-tool-use", "", t0.Add(time.Duration(i)*time.Second)) {
			t.Fatal("already running")
		}
	}
	if !r.StateSince.Equal(first) {
		t.Fatal("state_since must not move")
	}
	if r.Seq != 6 {
		t.Fatalf("every applied hook still advances seq: got %d", r.Seq)
	}
}

func TestUnrelatedSubkindsAreIgnored(t *testing.T) {
	var r Run
	r.ApplyHook("user-prompt-submit", "", t0)
	if r.ApplyHook("session-start", "", t0) || r.ApplyHook("post-tool-use", "", t0) {
		t.Fatal("unrelated subkinds must be ignored")
	}
	if r.CurrentState() != StateRunning {
		t.Fatalf("state = %q, want running", r.CurrentState())
	}
}

// Go-only: the event payload marshals exactly like emit_agent_run_event.
func TestEventShape(t *testing.T) {
	var r Run
	ev := r.Event("p_1")
	if ev.State != StateUnknown || ev.Running || ev.StartedAt != nil || ev.AvgMs != nil || ev.StateSince != nil {
		t.Fatalf("idle event = %+v", ev)
	}
	r.TurnStartedAt = t0
	r.ApplyHook("user-prompt-submit", "", t0)
	r.RecordTurn(4_000)
	ev = r.Event("p_1")
	if !ev.Running || ev.StartedAt == nil || *ev.StartedAt != t0.UnixMilli() {
		t.Fatalf("running event = %+v", ev)
	}
	if ev.AvgMs == nil || *ev.AvgMs != 4_000 || ev.StateSince == nil || ev.Seq != 1 {
		t.Fatalf("event = %+v", ev)
	}
}
