package term

// hookreg.go — hook tokens for the tmux sessions a browser creates
// (Phase 100, WEB-DESIGN B2).
//
// A session made through POST /api/v2/term/sessions is started with three
// SESSION-scoped variables (`tmux new-session -e`), which beat the global ones
// the desktop sets with `set-environment -g`:
//
//	YMUX_SOCKET_ADDR   the daemon's hook listener (internal/hooks)
//	YMUX_TUNNEL_TOKEN  a random per-session token, the HMAC key
//	YMUX_PANE_ID       term_<hex>, the id hooks report back
//
// so `ymux claude-hook` inside that session dials the DAEMON instead of the
// desktop. This registry remembers which token belongs to which session and
// answers the listener's "whose HMAC is this?" (core.HookResolver).
//
// This is the one piece of state in a package whose rule is "tmux is the
// truth", and it is deliberately the thinnest possible: in memory, keyed by
// session name, and only for sessions created through this API. A daemon
// restart starts it empty. That is accepted (Yossi, 2026-10-04): under
// systemd a restart takes a daemon-started tmux server with it anyway, and a
// session that survives (its tmux was started elsewhere) falls back to
// ~/.ymux/run/last.env — the desktop — exactly as before Phase 100.
//
// Rule #8: tokens are never logged. Rule #1: neither is anything a hook
// carries; see hookdispatch.go.

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"sync"
	"time"

	"ymux-server/internal/agent"
	"ymux-server/internal/core"
)

// hookEntry is one hook-enabled session. name follows renames; token and
// paneID are fixed for the session's life (they live in its environment).
type hookEntry struct {
	name   string
	token  string
	paneID string
	run    agent.Run
	brief  agent.BriefEntry
	// policy is what a permission request from this session gets (Phase
	// 101): policyNone answers allow at once, policyGate waits for a human.
	policy string
	// status is the pane's set-status text (Phase 102).
	status string
	// workspaceID is the browser workspace this session belongs to (Phase
	// 103) — "" for a session created outside one. It fences an agent's
	// send / title verbs to its own workspace.
	workspaceID string
}

// Hook policies (DECISIONS 2026-10-05). The desktop's auto/block are not
// offered: `none` is auto plus the light and the feed, and a browser user who
// wants "block" can deny from the gate.
const (
	policyNone = "none"
	policyGate = "gate"
)

func validPolicy(p string) bool { return p == policyNone || p == policyGate }

// HookRegistry implements core.HookResolver and core.AddrSink for term.
type HookRegistry struct {
	mu     sync.Mutex
	addr   string
	byName map[string]*hookEntry
	now    func() time.Time

	// feed and hub are B3 (Phase 101): the cards, the pending approvals and
	// the events subscribers. Each has its own lock; neither is ever taken
	// while mu is held by the same goroutine.
	feed *feedStore
	hub  *eventHub

	// B4 (Phase 102): notes (persisted once Service.SetDataDir names a
	// file), notifications (memory), and the box's listening ports (nil
	// until Service.StartPortWatch).
	notes  *noteStore
	notifs *notifStore
	ports  *portWatch

	// B5 (Phase 103): the browser workspaces, and what an agent's split and
	// send need from the service — set by Service.attachHooks.
	webws *webWSStore
	tmux  *Tmux
	spawn func(name, cwd, policy, workspaceID string) (hookEntry, error)

	// C4 (Phase 108): the browser's settings document (settings.go).
	settings *settingsStore
}

// NewHookRegistry returns an empty registry. Until SetHookAddr is called
// (hooks.Start not run, or its listen failed) sessions are created without
// hook variables and keep whatever global environment tmux has.
func NewHookRegistry() *HookRegistry {
	return &HookRegistry{byName: map[string]*hookEntry{}, now: time.Now, feed: newFeedStore(), hub: newEventHub(),
		notes: newNoteStore(""), notifs: &notifStore{}, webws: newWebWSStore(""),
		settings: newSettingsStore("")}
}

// SetHookAddr implements core.AddrSink.
func (r *HookRegistry) SetHookAddr(addr string) {
	r.mu.Lock()
	r.addr = addr
	r.mu.Unlock()
}

func (r *HookRegistry) hookAddr() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.addr
}

// mint creates the identity for a session about to be created and returns
// the tmux environment that carries it. Nothing is registered until add.
//
// paneID is the caller's own pane id (Phase 109: a browser layout leaf, so
// the leaf a hook reports is the leaf the UI drew); "" mints term_<hex>.
func (r *HookRegistry) mint(name, addr, paneID string) (*hookEntry, map[string]string, error) {
	tok, err := randHex(32)
	if err != nil {
		return nil, nil, err
	}
	if paneID == "" {
		id, err := randHex(8)
		if err != nil {
			return nil, nil, err
		}
		paneID = "term_" + id
	}
	e := &hookEntry{name: name, token: tok, paneID: paneID, policy: policyNone}
	env := map[string]string{
		"YMUX_SOCKET_ADDR":  addr,
		"YMUX_TUNNEL_TOKEN": tok,
		"YMUX_PANE_ID":      e.paneID,
	}
	return e, env, nil
}

// paneInUse reports whether a live session already carries paneID.
func (r *HookRegistry) paneInUse(paneID string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, e := range r.byName {
		if e.paneID == paneID {
			return true
		}
	}
	return false
}

// add registers a session that tmux has just created.
func (r *HookRegistry) add(e *hookEntry) {
	r.mu.Lock()
	r.byName[e.name] = e
	r.mu.Unlock()
}

// Rename follows a tmux rename. The session's environment (and therefore its
// token and pane id) is unchanged by a rename; only the key moves.
func (r *HookRegistry) Rename(from, to string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if e, ok := r.byName[from]; ok {
		delete(r.byName, from)
		e.name = to
		r.byName[to] = e
	}
}

// Remove forgets a killed session, denying any approval it left pending —
// the desktop's "sender dropped → deny", rather than a card that sits there
// until it times out for a claude that no longer exists.
func (r *HookRegistry) Remove(name string) {
	r.mu.Lock()
	var gone []string
	if e, ok := r.byName[name]; ok {
		gone = append(gone, e.paneID)
		delete(r.byName, name)
	}
	r.mu.Unlock()
	r.denyPendingOf(gone)
}

func (r *HookRegistry) denyPendingOf(paneIDs []string) {
	for _, pane := range paneIDs {
		for _, id := range r.feed.pendingFor(pane) {
			r.decide(id, "deny", "session-gone")
		}
	}
}

// SetPolicy changes a session's hook policy. False when the session is not
// one this registry knows (not created through the API, or already gone).
func (r *HookRegistry) SetPolicy(name, policy string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.byName[name]
	if !ok {
		return false
	}
	e.policy = policy
	logger.Info("hook policy set", "pane", e.paneID, "policy", policy)
	return true
}

// decide resolves a pending card and tells every subscriber. by is who
// answered, for the log only. False when there was nothing pending to decide.
func (r *HookRegistry) decide(requestID, decision, by string) bool {
	if !r.feed.decide(requestID, decision) {
		return false
	}
	logger.Info("feed decided", "request", requestID, "decision", decision, "by", by)
	r.hub.publish("feed:item-resolved", same(map[string]string{"request_id": requestID, "decision": decision}))
	return true
}

// Retain drops every entry whose session tmux no longer reports — a session
// that exited or was killed outside this API. Called with each list.
func (r *HookRegistry) Retain(live []Session) {
	keep := make(map[string]bool, len(live))
	for _, s := range live {
		keep[s.Name] = true
	}
	r.mu.Lock()
	var gone []string
	for name, e := range r.byName {
		if !keep[name] {
			gone = append(gone, e.paneID)
			delete(r.byName, name)
		}
	}
	r.mu.Unlock()
	r.denyPendingOf(gone)
}

// MatchHookHMAC implements core.HookResolver. O(hook-enabled sessions),
// constant-time compare.
func (r *HookRegistry) MatchHookHMAC(nonce, mac []byte) (core.HookTarget, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, e := range r.byName {
		h := hmac.New(sha256.New, []byte(e.token))
		h.Write(nonce)
		if hmac.Equal(h.Sum(nil), mac) {
			return termHookTarget{r: r, e: e}, true
		}
	}
	return nil, false
}

// PaneSnapshot is what the daemon knows about one hook-enabled pane: the
// `pane:agent-run` and `pane:brief` payloads the desktop would emit.
type PaneSnapshot struct {
	Session  string              `json:"session"`
	AgentRun agent.AgentRunEvent `json:"agent_run"`
	Brief    agent.BriefEntry    `json:"brief"`
}

// Snapshot returns every hook-enabled pane's state, keyed by pane id. The
// events socket's hello (events.go) is the served form of the same data.
func (r *HookRegistry) Snapshot() map[string]PaneSnapshot {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make(map[string]PaneSnapshot, len(r.byName))
	for _, e := range r.byName {
		out[e.paneID] = PaneSnapshot{Session: e.name, AgentRun: e.run.Event(e.paneID), Brief: e.brief}
	}
	return out
}

func randHex(n int) (string, error) {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return hex.EncodeToString(b), nil
}
