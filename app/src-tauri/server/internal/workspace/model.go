// Package workspace is the heart of Phase 77: the cross-client shared state that
// powers use-case 8a (several clients on the SAME session see the same
// chat/tool progression/hook requests) and 8b (a client answers a hook /
// message, it reaches the agent, and every other subscriber sees the
// resolution). It owns an append-only per-session event log (monotonic `seq`
// for replay-from-cursor), a subscriber fan-out, and winner-takes-all pending
// requests, all persisted to SQLite. See PHASE-77-DESIGN §4.2/§4.4.
package workspace

import "encoding/json"

// SessionKind enumerates what a session drives.
//
// KindTerminal is RESERVED, not implemented here, and that is deliberate: a
// terminal's bytes must never land in this package's append-only SQLite event
// log (it would be enormous, and Rule #1 keeps shell content out of storage).
// Server-side terminals live in internal/term, keyed by their tmux session
// name rather than by an id this package mints — see docs/WEB-DESIGN.md §3.
const (
	KindClaudeChat = "claude_chat"
	KindTerminal   = "terminal"
)

// Workspace groups sessions. ID is a server-authoritative UUID (Q5).
type Workspace struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	CreatedAt int64  `json:"created_at"` // unix seconds
}

// Session is one live thing clients attach to.
type Session struct {
	ID           string `json:"id"`
	WorkspaceID  string `json:"workspace_id"`
	Kind         string `json:"kind"`
	CreatedAt    int64  `json:"created_at"`
	LastActivity int64  `json:"last_activity"`
}

// Event is one entry in a session's append-only log. Seq is monotonic per
// session, starting at 1; clients replay everything after their cursor.
type Event struct {
	Seq       int64           `json:"seq"`
	SessionID string          `json:"session_id"`
	Type      string          `json:"type"`
	Timestamp int64           `json:"ts"`
	Payload   json.RawMessage `json:"payload,omitempty"`
}

// PendingRequest is a blocking ask (hook approval / user input) awaiting the
// FIRST client answer — winner-takes-all, idempotent per ReqID.
type PendingRequest struct {
	ReqID      string `json:"req_id"`
	SessionID  string `json:"session_id"`
	Type       string `json:"type"` // "hook_approval" | "user_input"
	CreatedAt  int64  `json:"created_at"`
	TimeoutAt  int64  `json:"timeout_at"`
	ResolvedBy string `json:"resolved_by"` // client_id winner; "" while open
	Resolution string `json:"resolution"`  // "allow" | "deny" | …
}
