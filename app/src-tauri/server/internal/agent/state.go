// Package agent holds the daemon's copy of the desktop's per-pane agent
// logic: the traffic-light state machine, the [ymux-brief] parser, the
// human-readable hook copy and the send-key table.
//
// Phase 99 (WEB-DESIGN Phase B1). Everything here is a PORT of Rust that
// still runs on the desktop, kept pure so it is unit-tested on its own:
//
//	state.go    ← app/src-tauri/src/lib.rs   (PaneAgentState, AgentRunState::apply_hook)
//	brief.go    ← app/src-tauri/src/brief.rs (parse_brief, brief_from_stop)
//	humanize.go ← app/src-tauri/src/rpc_server.rs (humanize_notification, clip)
//	keys.go     ← app/src-tauri/src/rpc_server.rs (translate_key)
//
// Until the desktop stops carrying its own copy, a change to either side
// must be made to both — the Rust functions carry the same note pointing
// here. The JSON shapes match the desktop's Tauri events field for field,
// so the frontend can consume either backend without forking its types.
//
// Stdlib only, imports no sibling package — like auth, a leaf.
package agent

import "time"

// MinTurnMs is the shortest turn folded into the rolling average.
// Mirrors AGENT_RUN_MIN_TURN_MS (lib.rs): text-only turns would drag the
// mean toward zero.
const MinTurnMs = 2000

// State is a pane's effective agent state — the traffic light.
type State string

const (
	// StateUnknown: no hook has ever arrived. Renders nothing — a pane
	// running a plain shell must not sprout a status light.
	StateUnknown State = "unknown"
	// StateRunning: the agent is working on a turn.
	StateRunning State = "running"
	// StateDone: the agent finished its turn. It does NOT expire on a
	// timer; Notification/idle_prompt promotes it to NeedsInput instead.
	StateDone State = "done"
	// StateNeedsInput: the agent is blocked on the human.
	StateNeedsInput State = "needs-input"
)

// needsInputNotifications are Claude Code's documented notification_type
// values that mean "blocked on the human". Anything not listed produces NO
// transition, so an upstream addition can never strand a pane on a colour.
var needsInputNotifications = map[string]bool{
	"permission_prompt":      true,
	"idle_prompt":            true,
	"agent_needs_input":      true,
	"elicitation_dialog":     true,
	"elicitation_url_dialog": true,
}

// resumedNotifications mean the human answered and work resumed.
var resumedNotifications = map[string]bool{
	"elicitation_complete": true,
	"elicitation_response": true,
}

// Run is one pane's agent run state (AgentRunState in lib.rs). The zero
// value is a pane no hook has touched.
type Run struct {
	// TurnStartedAt is the in-flight turn's start; zero when idle.
	TurnStartedAt time.Time
	SumMs         uint64
	Count         uint32
	State         State
	// StateSince is when State last actually changed; zero if never.
	StateSince time.Time
	// Seq is a per-pane monotonic counter, bumped on every applied hook.
	// uint32 for the same reason as the Rust side: the event carries a
	// plain JSON number and the frontend compares it.
	Seq uint32
}

// CurrentState returns State, reading the zero value as StateUnknown.
func (r *Run) CurrentState() State {
	if r.State == "" {
		return StateUnknown
	}
	return r.State
}

// AvgMs is the rolling average of counted turns; ok is false until one is.
func (r *Run) AvgMs() (avg uint64, ok bool) {
	if r.Count == 0 {
		return 0, false
	}
	return r.SumMs / uint64(r.Count), true
}

// StartedAtMs is the in-flight turn's start as epoch-ms; ok is false when
// no turn is in flight.
func (r *Run) StartedAtMs() (ms int64, ok bool) {
	return epochMs(r.TurnStartedAt)
}

// StateSinceMs is StateSince as epoch-ms, for the frontend's staleness check.
func (r *Run) StateSinceMs() (ms int64, ok bool) {
	return epochMs(r.StateSince)
}

// RecordTurn folds a completed turn into the average — only when it ran at
// least MinTurnMs.
func (r *Run) RecordTurn(durMs uint64) {
	if durMs < MinTurnMs {
		return
	}
	if r.SumMs+durMs < r.SumMs { // saturating, as in Rust
		r.SumMs = ^uint64(0)
	} else {
		r.SumMs += durMs
	}
	if r.Count < ^uint32(0) {
		r.Count++
	}
}

// ApplyHook folds one hook into the state (apply_hook in lib.rs).
//
// notificationType is "" when the hook carried none. It returns true only
// when the state actually changed. Seq bumps on every MAPPED hook, even a
// no-op one, so the frontend's ordering guard still advances; StateSince is
// stamped (with now) only on a real change, or a long turn full of tool
// calls would keep resetting its own clock.
func (r *Run) ApplyHook(subkind, notificationType string, now time.Time) bool {
	var next State
	switch subkind {
	case "user-prompt-submit", "pre-tool-use":
		next = StateRunning
	case "stop":
		next = StateDone
	case "notification":
		switch {
		case needsInputNotifications[notificationType]:
			next = StateNeedsInput
		case resumedNotifications[notificationType]:
			next = StateRunning
		default:
			// Unmapped or absent: no opinion, and no seq bump either.
			return false
		}
	default:
		return false
	}
	if r.Seq < ^uint32(0) {
		r.Seq++
	}
	if r.CurrentState() == next {
		return false
	}
	r.State = next
	r.StateSince = now
	return true
}

// AgentRunEvent is the `pane:agent-run` payload, field for field
// (emit_agent_run_event in lib.rs). Optional values are pointers so they
// marshal to null exactly like the Rust Option.
type AgentRunEvent struct {
	PaneID     string `json:"pane_id"`
	StartedAt  *int64 `json:"started_at"`
	AvgMs      *int64 `json:"avg_ms"`
	Running    bool   `json:"running"`
	State      State  `json:"state"`
	StateSince *int64 `json:"state_since"`
	Seq        uint32 `json:"seq"`
}

// Event builds the `pane:agent-run` payload for this run.
func (r *Run) Event(paneID string) AgentRunEvent {
	ev := AgentRunEvent{PaneID: paneID, State: r.CurrentState(), Seq: r.Seq}
	if ms, ok := r.StartedAtMs(); ok {
		ev.StartedAt = &ms
		ev.Running = true
	}
	if avg, ok := r.AvgMs(); ok {
		v := int64(avg)
		ev.AvgMs = &v
	}
	if ms, ok := r.StateSinceMs(); ok {
		ev.StateSince = &ms
	}
	return ev
}

// epochMs mirrors `duration_since(UNIX_EPOCH).ok()`: a zero or pre-epoch
// time has no epoch-ms.
func epochMs(t time.Time) (int64, bool) {
	if t.IsZero() || t.Before(time.Unix(0, 0)) {
		return 0, false
	}
	return t.UnixMilli(), true
}
