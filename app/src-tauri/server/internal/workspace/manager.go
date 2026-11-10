package workspace

// manager.go — the live layer over the Store: server-authoritative IDs, the
// per-session subscriber fan-out (8a), and winner-takes-all hook resolution
// with broadcast (8b). Persistence goes through Store; live WS channels live
// here in memory.

import (
	"encoding/json"
	"sync"
	"time"

	"github.com/google/uuid"

	"ymux-server/internal/core"
	"ymux-server/internal/logging"
)

// logger is the workspace subsystem's component logger (Phase 79.D).
var logger = logging.New("SRV:WS")

// liveSession holds the in-memory fan-out channels for a session's subscribers.
type liveSession struct {
	mu      sync.Mutex
	subs    map[string]chan []byte // client_id → buffered frame channel
	dropped int64                  // live frames dropped for full buffers (log rate-limit)
}

// PushLister yields the paired-device IDs eligible for an out-of-band push
// (active + registered FCM token). Implemented by the chat device store and
// injected via SetPushLister so the workspace package stays decoupled from chat.
type PushLister interface {
	ActiveDeviceIDs() []string
}

// Manager is the workspace subsystem entry point.
type Manager struct {
	store    *Store
	notifier core.NotificationSender
	pusher   PushLister    // nil = no device store wired (no out-of-band push)
	driver   SessionDriver // optional engine hook (claude_chat); nil = pure pub/sub

	mu   sync.Mutex
	live map[string]*liveSession // session_id → live subscribers

	// Test seam, nil in production: runs between the two steps of Subscribe
	// (register the channel, read the replay) so a test can publish exactly
	// there and prove the order loses nothing.
	betweenSubscribeSteps func()
}

// SetPushLister wires the device store used to find out-of-band push targets.
func (m *Manager) SetPushLister(p PushLister) { m.pusher = p }

// shouldNotify reports whether an event type warrants a push when no live WS
// subscriber is attached: a hook needing a decision, an assistant reply, or an
// explicit notification. High-frequency stream frames (tool_use/tool_result/
// status) never push.
func shouldNotify(typ string) bool {
	switch typ {
	case FrameHookRequest, FrameAssistantText, FrameNotification:
		return true
	}
	return false
}

// NewManager wires the manager over a store + a notification sender (NoopSender
// until FCM lands).
func NewManager(store *Store, notifier core.NotificationSender) *Manager {
	if notifier == nil {
		notifier = core.NoopSender{}
	}
	return &Manager{store: store, notifier: notifier, live: map[string]*liveSession{}}
}

func now() int64 { return time.Now().Unix() }

// ─── workspaces ──────────────────────────────────────────────────────────────

// CreateWorkspace mints a server-authoritative UUID (Q5).
// SessionDriver is an optional consumer of client→server commands on a session
// (Phase 77 §16). The chat WorkspaceBridge implements it to run Claude for a
// `claude_chat` session: the manager stays a pure pub/sub substrate; the driver
// is notified out-of-band so it can spawn/feed the engine. nil = no engine.
type SessionDriver interface {
	OnUserInput(sessionID, content, clientID string)
	OnHookDecision(sessionID, reqID, clientID, decision string)
	OnInterrupt(sessionID, clientID string)
}

// SetDriver registers the (single) session driver. Wired in cmd after both the
// chat engine + workspace manager exist.
func (m *Manager) SetDriver(d SessionDriver) { m.driver = d }

// Driver returns the registered driver (nil if none).
func (m *Manager) Driver() SessionDriver { return m.driver }

// DefaultID is the always-present workspace every client falls into when it
// hasn't chosen one (ensured at startup). Returned by pairing/redeem as
// default_workspace_id so a freshly paired phone can connect without a
// workspace-list round-trip.
const DefaultID = "ws_default"

func (m *Manager) CreateWorkspace(name string) (Workspace, error) {
	w := Workspace{ID: "ws_" + uuid.NewString(), Name: name, CreatedAt: now()}
	return w, m.store.CreateWorkspace(w)
}

// EnsureWorkspace creates a workspace with a fixed id if it doesn't exist (used
// for the backward-compat "default" workspace, S3.d).
func (m *Manager) EnsureWorkspace(id, name string) (Workspace, error) {
	if w, err := m.store.GetWorkspace(id); err == nil {
		return w, nil
	}
	w := Workspace{ID: id, Name: name, CreatedAt: now()}
	return w, m.store.CreateWorkspace(w)
}

func (m *Manager) ListWorkspaces() ([]Workspace, error)     { return m.store.ListWorkspaces() }
func (m *Manager) GetWorkspace(id string) (Workspace, error) { return m.store.GetWorkspace(id) }

// WorkspaceNameByID resolves a workspace id → display name (beta.3 Fix 3).
// Returns ("", false) when the workspace was deleted or the id is unknown so
// the caller can fall back to an empty prefix rather than surfacing a stale
// name. Cheap Store lookup — no lock beyond what the store already holds.
func (m *Manager) WorkspaceNameByID(id string) (string, bool) {
	if id == "" {
		return "", false
	}
	w, err := m.store.GetWorkspace(id)
	if err != nil {
		return "", false
	}
	return w.Name, true
}

// DeleteWorkspace cascades (kills sessions + their state) and drops live subs.
func (m *Manager) DeleteWorkspace(id string) error {
	sessions, _ := m.store.ListSessions(id)
	for _, se := range sessions {
		m.dropLive(se.ID)
	}
	return m.store.DeleteWorkspace(id)
}

// ─── sessions ────────────────────────────────────────────────────────────────

// CreateSession mints a session UUID under an existing workspace.
func (m *Manager) CreateSession(wsID, kind string) (Session, error) {
	if _, err := m.store.GetWorkspace(wsID); err != nil {
		return Session{}, ErrNotFound
	}
	if kind == "" {
		kind = KindClaudeChat
	}
	se := Session{ID: "sess_" + uuid.NewString(), WorkspaceID: wsID, Kind: kind, CreatedAt: now(), LastActivity: now()}
	if err := m.store.CreateSession(se); err != nil {
		return Session{}, err
	}
	return se, nil
}

func (m *Manager) GetSession(id string) (Session, error)          { return m.store.GetSession(id) }
func (m *Manager) ListSessions(wsID string) ([]Session, error)    { return m.store.ListSessions(wsID) }
func (m *Manager) ListPending(id string) ([]PendingRequest, error) { return m.store.ListPending(id) }
func (m *Manager) EventCount(sessionID string) int64              { return m.store.EventCount(sessionID) }

// SubscriberCount reports how many clients are live on a session.
func (m *Manager) SubscriberCount(sessionID string) int {
	ls := m.liveOf(sessionID, false)
	if ls == nil {
		return 0
	}
	ls.mu.Lock()
	defer ls.mu.Unlock()
	return len(ls.subs)
}

// ─── event log + fan-out (8a) ────────────────────────────────────────────────

// Publish appends an event to the session log and fans it out to live
// subscribers. Returns the stored event (with its assigned seq).
func (m *Manager) Publish(sessionID, typ string, payload json.RawMessage) (Event, error) {
	ev, err := m.store.AppendEvent(sessionID, typ, payload)
	if err != nil {
		return ev, err
	}
	m.store.TouchSession(sessionID, ev.Timestamp)
	m.fanout(ev)
	m.maybePush(ev)
	return ev, nil
}

// maybePush sends an out-of-band FCM push to every active paired device when a
// notification-worthy event lands on a session with NO live WS subscriber (the
// phone is backgrounded / disconnected). Best-effort + async so a slow FCM call
// never blocks Publish; delivery is fire-and-forget (the client re-syncs the
// durable log on reconnect regardless).
func (m *Manager) maybePush(ev Event) {
	if m.pusher == nil || !shouldNotify(ev.Type) {
		return
	}
	if n := m.SubscriberCount(ev.SessionID); n > 0 {
		logger.Debug("push skip: live subscribers present", "event", ev.Type, "session", ev.SessionID, "subscribers", n)
		return
	}
	targets := m.pusher.ActiveDeviceIDs()
	if len(targets) == 0 {
		logger.Debug("push skip: no active devices", "event", ev.Type, "session", ev.SessionID)
		return
	}
	logger.Debug("push fanout", "event", ev.Type, "session", ev.SessionID, "devices", len(targets))
	frame := eventPayloadMap(ev)
	// beta.3 Fix 3: resolve the workspace name so the phone can render
	// "[<workspace>] tool needs approval" instead of just the session id.
	// Both fields are always present in the frame (empty string when the
	// lookup fails) so the mobile client can rely on the key existing.
	if s, err := m.store.GetSession(ev.SessionID); err == nil {
		frame["workspace_id"] = s.WorkspaceID
		if name, ok := m.WorkspaceNameByID(s.WorkspaceID); ok {
			frame["workspace_name"] = name
		} else {
			frame["workspace_name"] = ""
		}
	} else {
		frame["workspace_id"] = ""
		frame["workspace_name"] = ""
	}
	notifier := m.notifier
	go func() {
		for _, id := range targets {
			_ = notifier.Notify(id, frame)
		}
	}()
}

// eventPayloadMap flattens an Event into a §4.4 wire object: the type-specific
// payload with seq/type/session_id/ts merged on top.
func eventPayloadMap(ev Event) map[string]any {
	m := map[string]any{}
	if len(ev.Payload) > 0 {
		_ = json.Unmarshal(ev.Payload, &m)
	}
	m["seq"] = ev.Seq
	m["type"] = ev.Type
	m["session_id"] = ev.SessionID
	m["ts"] = ev.Timestamp
	return m
}

// eventFrame marshals the §4.4 wire object for live WS fan-out.
func eventFrame(ev Event) []byte {
	b, _ := json.Marshal(eventPayloadMap(ev))
	return b
}

func (m *Manager) fanout(ev Event) {
	ls := m.liveOf(ev.SessionID, false)
	if ls == nil {
		return
	}
	frame := eventFrame(ev)
	ls.mu.Lock()
	defer ls.mu.Unlock()
	for clientID, ch := range ls.subs {
		select {
		case ch <- frame:
		default:
			// Slow subscriber: drop the live frame. It's durable in the log, so
			// the client re-syncs via cursor replay on reconnect. Logged as
			// metadata only, the first drop and then every 100th.
			ls.dropped++
			if ls.dropped%100 == 1 {
				logger.Warn("dropped live frame for slow subscriber",
					"session", ev.SessionID, "client", clientID, "seq", ev.Seq, "dropped_total", ls.dropped)
			}
		}
	}
}

// Subscribe registers a client on a session and returns the replay (events
// after cursor) plus a live channel + a cancel func. The channel is registered
// BEFORE the replay is read, so an event published in between is in the
// channel (and possibly in the replay too) rather than in neither. The caller
// dedups live frames by seq (any frame with seq <= the last replayed seq is a
// repeat of a just-replayed event and is skipped).
//
// A client_id that is already subscribed is replaced: the old channel is
// closed (its reader ends) and the newest connection owns the id. cancel only
// ever removes the channel this call created.
func (m *Manager) Subscribe(sessionID, clientID, deviceName string, cursor int64) ([]Event, <-chan []byte, func(), error) {
	if _, err := m.store.GetSession(sessionID); err != nil {
		return nil, nil, nil, ErrNotFound
	}
	ch := make(chan []byte, 256)
	// m.mu → ls.mu, held together: a concurrent cancel that empties the
	// session must not unlink the liveSession this subscriber is joining.
	m.mu.Lock()
	ls := m.live[sessionID]
	if ls == nil {
		ls = &liveSession{subs: map[string]chan []byte{}}
		m.live[sessionID] = ls
	}
	ls.mu.Lock()
	if old, ok := ls.subs[clientID]; ok {
		close(old)
	}
	ls.subs[clientID] = ch
	ls.mu.Unlock()
	m.mu.Unlock()

	cancel := func() {
		m.mu.Lock()
		ls.mu.Lock()
		mine := ls.subs[clientID] == ch // false once replaced or dropLive'd
		if mine {
			delete(ls.subs, clientID)
			close(ch)
			if len(ls.subs) == 0 && m.live[sessionID] == ls {
				delete(m.live, sessionID)
			}
		}
		ls.mu.Unlock()
		m.mu.Unlock()
		if mine {
			_, _ = m.store.db.Exec(`DELETE FROM subscribers WHERE session_id=? AND client_id=?`, sessionID, clientID)
		}
	}

	if m.betweenSubscribeSteps != nil {
		m.betweenSubscribeSteps()
	}
	replay, err := m.store.ReplayEvents(sessionID, cursor, 0)
	if err != nil {
		cancel()
		return nil, nil, nil, err
	}
	t := now()
	_, _ = m.store.db.Exec(
		`INSERT OR REPLACE INTO subscribers(session_id,client_id,device_name,connected_at,last_seen) VALUES(?,?,?,?,?)`,
		sessionID, clientID, deviceName, t, t)
	return replay, ch, cancel, nil
}

func (m *Manager) liveOf(sessionID string, create bool) *liveSession {
	m.mu.Lock()
	defer m.mu.Unlock()
	ls := m.live[sessionID]
	if ls == nil && create {
		ls = &liveSession{subs: map[string]chan []byte{}}
		m.live[sessionID] = ls
	}
	return ls
}

func (m *Manager) dropLive(sessionID string) {
	m.mu.Lock()
	ls := m.live[sessionID]
	delete(m.live, sessionID)
	m.mu.Unlock()
	if ls != nil {
		ls.mu.Lock()
		for id, c := range ls.subs {
			delete(ls.subs, id)
			close(c)
		}
		ls.mu.Unlock()
	}
}

// ─── hooks: winner-takes-all + broadcast (8b) ────────────────────────────────

// CreateHookRequest records a pending hook and publishes a hook_request event so
// every subscriber sees the ask. payload should carry req_id/tool_name/etc.
func (m *Manager) CreateHookRequest(sessionID, reqID string, payload json.RawMessage, timeoutAt int64) error {
	if err := m.store.CreatePending(PendingRequest{
		ReqID: reqID, SessionID: sessionID, Type: "hook_approval",
		CreatedAt: now(), TimeoutAt: timeoutAt,
	}); err != nil {
		return err
	}
	_, err := m.Publish(sessionID, FrameHookRequest, payload)
	return err
}

// ForwardHook records + publishes a hook_request that ORIGINATED on the desktop
// (B path). It reuses CreateHookRequest against a virtual session id, so the
// no-subscriber fan-out (maybePush) reaches every paired phone. The desktop
// keeps the authoritative local hook and polls HookResolution for the phone's
// decision.
func (m *Manager) ForwardHook(virtualSession, reqID string, payload json.RawMessage, timeoutAt int64) error {
	return m.CreateHookRequest(virtualSession, reqID, payload, timeoutAt)
}

// HookResolution reports a hook's decision — resolved=false until a
// winner-takes-all resolution lands. Lets the desktop poll for a phone decision
// on a forwarded (B-path) hook.
func (m *Manager) HookResolution(reqID string) (decision string, resolved bool) {
	p, ok := m.store.GetPending(reqID)
	if !ok || p.ResolvedBy == "" {
		return "", false
	}
	return p.Resolution, true
}

// ResolveHook records the FIRST decision for reqID (winner-takes-all) and, if it
// won, broadcasts a hook_resolved event to all subscribers. Returns whether this
// caller was the winner.
func (m *Manager) ResolveHook(reqID, clientID, decision string) (bool, error) {
	if decision != "allow" {
		decision = "deny"
	}
	won, err := m.store.ResolvePending(reqID, clientID, decision)
	if err != nil || !won {
		return won, err
	}
	p, _ := m.store.GetPending(reqID)
	payload, _ := json.Marshal(hookResolvedPayload{ReqID: reqID, Decision: decision, ResolvedBy: clientID})
	_, err = m.Publish(p.SessionID, FrameHookResolved, payload)
	return true, err
}
