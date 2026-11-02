package chat

// Phase 69.A — REST + WebSocket surface for the mobile Claude chat client.
// Registered on the same localhost mux as the metrics API; same bearer model
// (device tokens layered in 69.D). All paths localhost-only behind the tunnel.

import (
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/gorilla/websocket"
)

type ChatAPI struct {
	mgr         *SessionManager
	store       *ChatStore
	sharedToken string // insights bearer; 69.D adds per-device tokens

	// Phase 96, browser-initiated pairing. askApproval pushes a blocking
	// Allow/Deny card to the desktop (injected from cmd so this package stays
	// clear of the outbound-tunnel client); nil means no desktop is reachable
	// and a pairing request fails closed. pairLimiter bounds an endpoint that
	// by its nature cannot require a credential.
	askApproval ApprovalAsker
	pairLimiter *ipLimiter
}

func NewChatAPI(mgr *SessionManager, store *ChatStore, sharedToken string) *ChatAPI {
	return &ChatAPI{
		mgr: mgr, store: store, sharedToken: sharedToken,
		pairLimiter: newIPLimiter(),
	}
}

var wsUpgrader = websocket.Upgrader{
	ReadBufferSize:  4096,
	WriteBufferSize: 4096,
	// Localhost-only behind the tunnel; the bearer token is the auth, so we
	// don't gate on Origin (mobile clients have no meaningful Origin).
	CheckOrigin: func(_ *http.Request) bool { return true },
}

func (c *ChatAPI) RegisterRoutes(mux *http.ServeMux) {
	// Phase 77 (S3): the legacy Claude-chat HTTP surface is RETIRED. No mobile
	// devices existed in production, so this is a clean break (not a migration):
	// clients drive Claude sessions through /api/v2/workspace/* now. The old
	// paths return 410 Gone. (The chat SessionManager + hook bridge stay as
	// internal machinery for the workspace-driven spawn wiring.)
	gone := func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "gone: this endpoint was retired in Phase 77 — use /api/v2/workspace/*", http.StatusGone)
	}
	mux.HandleFunc("/api/claude/session", gone)
	mux.HandleFunc("/api/claude/sessions", gone)
	mux.HandleFunc("/api/claude/session/", gone)
	mux.HandleFunc("/ws/claude/session/", gone)
	// Pairing STAYS — it's desktop-facing (the Monitor issues QR + device
	// tokens), and Yossi lives on the desktop. Backward compat preserved.
	c.registerPairingRoutes(mux)
	// Phase 96: the other direction — a browser asks, the desktop approves.
	c.registerBrowserPairingRoutes(mux)
}

// clientIP prefers nginx's forwarded headers (the daemon is behind the proxy,
// so RemoteAddr is always 127.0.0.1).
func clientIP(r *http.Request) string {
	if v := r.Header.Get("X-Real-IP"); v != "" {
		return v
	}
	if v := r.Header.Get("X-Forwarded-For"); v != "" {
		return strings.TrimSpace(strings.Split(v, ",")[0])
	}
	return r.RemoteAddr
}

// ─── auth ────────────────────────────────────────────────────────────────

// authDevice validates the bearer token. admin is true for the shared
// (insights/desktop) token; for a registered device token, deviceID is its id.
func (c *ChatAPI) authDevice(token string) (deviceID string, admin, ok bool) {
	if token == "" {
		return "", false, false
	}
	if token == c.sharedToken {
		return "", true, true
	}
	if d, found := c.store.deviceByTokenHash(hashToken(token)); found {
		return d.ID, false, true
	}
	return "", false, false
}

func bearer(r *http.Request) string {
	return strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
}

// guard wraps a session handler with bearer auth, passing the resolved device
// id ("" for admin/shared) to the handler.
func (c *ChatAPI) guard(h func(http.ResponseWriter, *http.Request, string)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		dev, _, ok := c.authDevice(bearer(r))
		if !ok {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		h(w, r, dev)
	}
}

// adminGuard restricts device-management endpoints to the shared (desktop)
// token — a device token can never mint or revoke other devices.
func (c *ChatAPI) adminGuard(h http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		_, admin, ok := c.authDevice(bearer(r))
		if !ok || !admin {
			http.Error(w, "admin token required", http.StatusForbidden)
			return
		}
		h(w, r)
	}
}

// ownsSession reports whether the caller may touch this session: admin
// (deviceID=="") sees all; a device sees only its own.
func ownsSession(callerDeviceID string, row *SessionRow) bool {
	return callerDeviceID == "" || row.DeviceID == callerDeviceID
}

// ─── REST handlers ───────────────────────────────────────────────────────

func (c *ChatAPI) handleCreate(w http.ResponseWriter, r *http.Request, deviceID string) {
	if r.Method != http.MethodPost {
		http.Error(w, "POST only", http.StatusMethodNotAllowed)
		return
	}
	var spec startSpec
	if r.Body != nil {
		_ = json.NewDecoder(r.Body).Decode(&spec) // empty body is fine (all optional)
	}
	spec.DeviceID = deviceID
	s, err := c.mgr.create(spec)
	if err != nil {
		writeChatErr(w, err)
		return
	}
	writeJSON(w, map[string]any{
		"session_id":        s.id,
		"claude_session_id": s.claudeSessionID,
		"status":            s.getStatus(),
	})
}

func (c *ChatAPI) handleList(w http.ResponseWriter, r *http.Request, deviceID string) {
	var rows []SessionRow
	var err error
	if deviceID == "" {
		rows, err = c.store.listSessions() // admin sees all
	} else {
		rows, err = c.store.listSessionsForDevice(deviceID)
	}
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	out := make([]map[string]any, 0, len(rows))
	for _, r := range rows {
		out = append(out, sessionSummary(r))
	}
	writeJSON(w, map[string]any{"sessions": out})
}

func (c *ChatAPI) handleItem(w http.ResponseWriter, r *http.Request, deviceID string) {
	id := strings.TrimPrefix(r.URL.Path, "/api/claude/session/")
	id = strings.Trim(id, "/")
	if id == "" {
		http.Error(w, "missing session id", http.StatusBadRequest)
		return
	}
	row, err := c.store.getSession(id)
	if err != nil || !ownsSession(deviceID, row) {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}
	switch r.Method {
	case http.MethodGet:
		sum := sessionSummary(*row)
		if s := c.mgr.get(id); s != nil {
			s.mu.Lock()
			sum["status"] = s.status
			if s.pendingTool != "" {
				sum["pending_tool"] = s.pendingTool
			}
			s.mu.Unlock()
		}
		writeJSON(w, sum)
	case http.MethodDelete:
		if s := c.mgr.get(id); s != nil {
			s.stop("client delete")
		}
		c.store.updateSessionStatus(id, stKilled)
		c.mgr.forget(id)
		writeJSON(w, map[string]any{"ok": true})
	default:
		http.Error(w, "GET or DELETE", http.StatusMethodNotAllowed)
	}
}

func sessionSummary(r SessionRow) map[string]any {
	return map[string]any{
		"id":                r.ID,
		"device_id":         r.DeviceID,
		"claude_session_id": r.ClaudeSessionID,
		"status":            r.Status,
		"model":             r.Model,
		"cwd":               r.Cwd,
		"policy":            r.Policy,
		"started_at":        r.StartedAt,
		"last_activity_at":  r.LastActivityAt,
		"message_count":     r.MessageCount,
	}
}

func writeChatErr(w http.ResponseWriter, err error) {
	var ce *chatErr
	if errors.As(err, &ce) {
		switch ce.kind {
		case "rate":
			http.Error(w, ce.msg, http.StatusTooManyRequests)
			return
		case "notfound":
			http.Error(w, ce.msg, http.StatusNotFound)
			return
		case "state":
			http.Error(w, ce.msg, http.StatusConflict)
			return
		}
	}
	http.Error(w, err.Error(), http.StatusInternalServerError)
}

// ─── WebSocket ───────────────────────────────────────────────────────────

// clientMsg is what the mobile client sends over the WS.
type clientMsg struct {
	Type     string `json:"type"`
	Content  string `json:"content"`
	ReqID    string `json:"req_id"`
	Decision string `json:"decision"`
}

func (c *ChatAPI) handleWS(w http.ResponseWriter, r *http.Request) {
	// WS auth: bearer header or ?token= (browsers/mobile can't always set headers).
	tok := bearer(r)
	if tok == "" {
		tok = r.URL.Query().Get("token")
	}
	dev, _, ok := c.authDevice(tok)
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	if dev != "" {
		c.store.touchDevice(dev, clientIP(r)) // activity log
	}
	id := strings.TrimPrefix(r.URL.Path, "/ws/claude/session/")
	id = strings.Trim(id, "/")
	if row, err := c.store.getSession(id); err != nil || !ownsSession(dev, row) {
		http.Error(w, "no such session", http.StatusNotFound)
		return
	}
	s := c.mgr.get(id)
	if s == nil {
		http.Error(w, "no such session", http.StatusNotFound)
		return
	}

	conn, err := wsUpgrader.Upgrade(w, r, nil)
	if err != nil {
		return // Upgrade already wrote the error
	}
	defer conn.Close()
	// The metrics http.Server sets short Read/Write deadlines; a long-lived WS
	// must clear them and manage its own (per-write deadline + 30s ping below).
	_ = conn.SetReadDeadline(time.Time{})
	_ = conn.SetWriteDeadline(time.Time{})

	sub := s.addSubscriber()
	defer s.removeSubscriber(sub)

	// Replay the buffered transcript so a reconnecting client rebuilds state.
	for _, ev := range c.store.getReplay(id) {
		_ = conn.WriteMessage(websocket.TextMessage, ev)
	}

	// Writer goroutine: the only writer on the conn (gorilla requires one).
	done := make(chan struct{})
	go func() {
		ping := time.NewTicker(30 * time.Second)
		defer ping.Stop()
		for {
			select {
			case ev, ok := <-sub.ch:
				if !ok {
					return
				}
				if err := conn.WriteMessage(websocket.TextMessage, ev); err != nil {
					return
				}
			case <-ping.C:
				if err := conn.WriteControl(websocket.PingMessage, nil, time.Now().Add(5*time.Second)); err != nil {
					return
				}
			case <-done:
				return
			}
		}
	}()

	// Reader loop: client → server messages.
	conn.SetReadLimit(1 << 20)
	for {
		_, data, err := conn.ReadMessage()
		if err != nil {
			break
		}
		var m clientMsg
		if json.Unmarshal(data, &m) != nil {
			continue
		}
		switch m.Type {
		case "user_input":
			if err := s.sendUserInput(m.Content); err != nil {
				s.emit(jsonEvent(map[string]any{"type": "error", "message": err.Error()}))
			}
		case "hook_decision":
			s.resolveHook(m.ReqID, m.Decision)
		case "interrupt":
			s.interrupt()
		case "stop_session":
			s.stop("client stop")
		}
	}
	close(done)
}
