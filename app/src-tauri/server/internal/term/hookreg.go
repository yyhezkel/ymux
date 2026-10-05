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
}

// HookRegistry implements core.HookResolver and core.AddrSink for term.
type HookRegistry struct {
	mu     sync.Mutex
	addr   string
	byName map[string]*hookEntry
	now    func() time.Time
}

// NewHookRegistry returns an empty registry. Until SetHookAddr is called
// (hooks.Start not run, or its listen failed) sessions are created without
// hook variables and keep whatever global environment tmux has.
func NewHookRegistry() *HookRegistry {
	return &HookRegistry{byName: map[string]*hookEntry{}, now: time.Now}
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
func (r *HookRegistry) mint(name, addr string) (*hookEntry, map[string]string, error) {
	tok, err := randHex(32)
	if err != nil {
		return nil, nil, err
	}
	id, err := randHex(8)
	if err != nil {
		return nil, nil, err
	}
	e := &hookEntry{name: name, token: tok, paneID: "term_" + id}
	env := map[string]string{
		"YMUX_SOCKET_ADDR":  addr,
		"YMUX_TUNNEL_TOKEN": tok,
		"YMUX_PANE_ID":      e.paneID,
	}
	return e, env, nil
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

// Remove forgets a killed session.
func (r *HookRegistry) Remove(name string) {
	r.mu.Lock()
	delete(r.byName, name)
	r.mu.Unlock()
}

// Retain drops every entry whose session tmux no longer reports — a session
// that exited or was killed outside this API. Called with each list.
func (r *HookRegistry) Retain(live []Session) {
	keep := make(map[string]bool, len(live))
	for _, s := range live {
		keep[s.Name] = true
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	for name := range r.byName {
		if !keep[name] {
			delete(r.byName, name)
		}
	}
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

// Snapshot returns every hook-enabled pane's state, keyed by pane id. Nothing
// serves it yet — B3 adds the route and the live events (WEB-DESIGN §8).
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
