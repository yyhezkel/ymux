// Package core holds the cross-subsystem interfaces and shared value types for
// ymux-server. It is a LEAF package: it imports no sibling internal package,
// so every subsystem depends on `core` and never on another subsystem. That is
// the concrete fix for the Phase-69 WS↔session↔hookRPC import cycle that forced
// the old daemon into one flat `package main` (see PHASE-77-DESIGN §15).
package core

import (
	"io"
	"net"
)

// Version is the ymux-server release version. Major 2 marks the API-stability
// guarantee introduced in Phase 77. One constant, shared by every package + cmd.
// 2.1.0 (v0.4.3): native self-hosted push + REST hook resolution + per-device
// scopes. Bumped so the add-on offers the update (remote pulls the new binary).
// 2.1.3 (hotfix): docker.sock connection leak in internal/insights — dockerHTTP
// used to create a fresh http.Client + Transport per poll, leaking each
// Transport's keep-alive idle pool into dockerd (RSS growth to 8 GB on a
// 34-container host in ~16 h). See app/src-tauri/server/internal/insights/docker.go.
// 2.1.4 (Phase 79): unified logging — slog with the system-wide line format
// ([ts] [LEVEL] [SRV:COMP] msg) into insights.log + a 30s watcher on
// ~/.ymux/log-level so the desktop's Settings→Logs level applies without a
// service restart. Bumped so the add-on offers the update.
// 2.2.0: the winmux -> ymux rename. The binary, the systemd unit and the
// data dir all move, so every existing remote needs this update offer to
// pick up the daemon that knows about the new paths.
// 2.2.1 (Phase 86): server load — docker stats one-shot + 30s cadence, top
// every 10s, hygiene reaper also kills orphaned (ppid 1) port-watchers.
const Version = "2.2.1"

// FrameVersion is the WebSocket frame-contract version (PHASE-77-DESIGN §4.4).
// It is sent in the WS `hello` frame; a client that refuses an unknown value
// must fail loudly rather than silently drift.
const FrameVersion = 2

// HookConnHandler consumes a freshly-accepted hook-RPC connection and speaks the
// Phase-66 challenge/response + JSON-RPC protocol on it. It is implemented by
// chat's SessionManager (which owns the per-session HMAC tokens + pending-hook
// state) and driven by the thin hooks.Listener. This indirection is the cycle
// break: hooks → core, chat → core, and cmd wires the concrete handler in.
type HookConnHandler interface {
	HandleHookConn(conn net.Conn)
}

// AddrSink receives the hook listener's bound localhost address so the session
// manager can inject it (as YMUX_SOCKET_ADDR) into spawned claude children.
type AddrSink interface {
	SetHookAddr(addr string)
}

// ─── Reserved for S3 (workspace shared state, Q3) ────────────────────────────
// Declared now to document the intended boundary; not yet consumed.

// EventBus is the fan-out surface for multi-client attach (use-case 8a) and
// notification broadcast (8b): many subscribers per session, every
// server-origin frame delivered to all of them. Implemented by
// internal/workspace in Sprint 3.
type EventBus interface {
	Publish(sessionID string, frame []byte)
	Subscribe(sessionID string) (frames <-chan []byte, cancel func())
}

// NotificationSender delivers an out-of-band push (e.g. FCM) to a paired device
// when a session needs input and no live WS subscriber is attached (use-case
// 8b). Sprint 3 ships NoopSender; a real FCM sender is a later sprint
// (PHASE-77-DESIGN §7).
type NotificationSender interface {
	// Notify pushes a minimal payload; the device fetches full detail via the
	// API after the user taps. A nil/empty return means "delivered (or dropped
	// silently)".
	Notify(deviceID string, payload map[string]any) error
}

// NoopSender is the default NotificationSender — it drops pushes (no FCM yet).
type NoopSender struct{}

// Notify does nothing (returns nil).
func (NoopSender) Notify(string, map[string]any) error { return nil }

// ─── Files API (S2, PHASE-77-DESIGN §4.2) ────────────────────────────────────

// FileEntry is one item in a directory listing.
type FileEntry struct {
	Name     string `json:"name"`
	Type     string `json:"type"` // "dir" | "file"
	Size     int64  `json:"size"`
	Modified int64  `json:"modified"` // unix seconds
}

// FilesProvider is the sandboxed filesystem surface behind /api/v2/files/*.
// Every path is interpreted relative to a provider-owned root and MUST stay
// inside it (traversal + symlink-escape rejected) — see internal/files.
// Abstracting it as an interface keeps the HTTP layer testable with a mock and
// lets a future provider (e.g. an object store) drop in.
type FilesProvider interface {
	// List returns the resolved cwd and the entries at path (depth 1 = the dir
	// itself; depth 2 = one level of children flattened, name carrying the
	// relative sub-path).
	List(path string, depth int) (cwd string, entries []FileEntry, err error)
	// Read returns up to maxBytes of a file; truncated is true if the file was
	// larger than maxBytes.
	Read(path string, maxBytes int64) (content []byte, truncated bool, err error)
	// Write creates/overwrites a file and returns its sha256 (hex) + size.
	Write(path string, data []byte) (sha256Hex string, size int64, err error)
	// Delete removes a file (not a non-empty directory).
	Delete(path string) error
	// Open streams a file for download; caller closes the ReadCloser.
	Open(path string) (rc io.ReadCloser, size int64, err error)
	// Root is the absolute sandbox root (for diagnostics).
	Root() string
}
