// Package push is ymux's self-hosted push subsystem (no Firebase/FCM/APNs).
// A paired device holds a long-lived WebSocket (GET /api/v2/push/subscribe) via
// an Android foreground service; the server delivers notification-worthy events
// over it, and queues them per-device when the socket is down for replay on
// reconnect. See docs/PUSH-PROTOCOL.md for the wire contract.
//
// Server implements core.NotificationSender structurally (Notify), so the
// workspace manager fans out to it with no import of core here. It stays
// decoupled from the chat store via the injected Deps closures.
package push

import (
	"encoding/json"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"

	"ymux-server/internal/logging"
)

// logger is the push subsystem's component logger (Phase 79.D).
var logger = logging.New("SRV:PUSH")

const (
	envelopeVersion = 1
	queueCap        = 1000
	heartbeatSec    = 30
	writeWait       = 10 * time.Second
	pongWait        = 2 * heartbeatSec * time.Second
)

// QueuedEvent is a stored envelope awaiting delivery to a device.
type QueuedEvent struct {
	PushSeq int64
	Ts      int64
	Event   string // JSON of the §4.4 event object
}

// Deps injects the device/token/queue operations (backed by the chat store),
// keeping this package free of a chat import.
type Deps struct {
	ResolveToken func(token string) (deviceID string, admin, ok bool)
	Enqueue      func(deviceID, eventJSON string, capN int) (int64, error)
	PendingAfter func(deviceID string, cursor int64) []QueuedEvent
	Ack          func(deviceID string, upto int64)
}

// client is one live push connection. mu serializes all writes to conn.
type client struct {
	deviceID string
	conn     *websocket.Conn
	mu       sync.Mutex

	// flushMu serializes flush and guards sent: the highest push_seq written
	// on this connection (it starts at the client's cursor).
	flushMu sync.Mutex
	sent    int64
}

func (c *client) writeJSON(v any) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	_ = c.conn.SetWriteDeadline(time.Now().Add(writeWait))
	return c.conn.WriteJSON(v)
}

// Server is the push registry + NotificationSender.
type Server struct {
	deps Deps
	up   websocket.Upgrader
	mu   sync.Mutex
	live map[string]*client // device_id → the single live connection
}

// New builds a push Server over the injected store deps.
func New(deps Deps) *Server {
	return &Server{
		deps: deps,
		up:   websocket.Upgrader{CheckOrigin: func(*http.Request) bool { return true }},
		live: map[string]*client{},
	}
}

func (s *Server) register(c *client) {
	s.mu.Lock()
	if old := s.live[c.deviceID]; old != nil {
		_ = old.conn.WriteControl(websocket.CloseMessage,
			websocket.FormatCloseMessage(4409, "replaced"), time.Now().Add(time.Second))
		_ = old.conn.Close()
	}
	s.live[c.deviceID] = c
	s.mu.Unlock()
}

func (s *Server) unregister(c *client) {
	s.mu.Lock()
	if s.live[c.deviceID] == c {
		delete(s.live, c.deviceID)
	}
	s.mu.Unlock()
}

func (s *Server) get(deviceID string) *client {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.live[deviceID]
}

// flush writes every queued event after the last one this connection sent,
// in push_seq order, and reports how many. It is the ONLY place an event
// envelope is written, for the reconnect replay and live delivery alike, so a
// connection's push_seq never goes backwards: the client's dedup drops any
// envelope <= the highest it processed (PUSH-PROTOCOL §5) and its ack is
// cumulative (§3.4), so an out-of-order envelope would be lost for good.
func (s *Server) flush(c *client) (int, error) {
	c.flushMu.Lock()
	defer c.flushMu.Unlock()
	n := 0
	for _, pe := range s.deps.PendingAfter(c.deviceID, c.sent) {
		var ev any
		_ = json.Unmarshal([]byte(pe.Event), &ev)
		if err := c.writeJSON(envelope(c.deviceID, pe.PushSeq, ev)); err != nil {
			return n, err
		}
		c.sent = pe.PushSeq
		n++
	}
	return n, nil
}

// Notify implements core.NotificationSender: persist the event to the device's
// queue (assigning its push_seq), then deliver live if the socket is up. If not
// connected it waits in the queue for the reconnect replay — at-least-once.
// Live delivery is a flush of the queue, not a direct write, so it can never
// overtake a replay in progress or another Notify's lower push_seq.
func (s *Server) Notify(deviceID string, payload map[string]any) error {
	eventJSON, err := json.Marshal(payload)
	if err != nil {
		return err
	}
	seq, err := s.deps.Enqueue(deviceID, string(eventJSON), queueCap)
	if err != nil {
		logger.Error("enqueue failed", "device", deviceID, "err", err)
		return err
	}
	live := s.get(deviceID) != nil
	logger.Debug("notify", "device", deviceID, "seq", seq, "type", payload["type"], "live", live)
	if c := s.get(deviceID); c != nil {
		_, _ = s.flush(c)
	}
	return nil
}

func envelope(deviceID string, seq int64, event any) map[string]any {
	env := map[string]any{
		"v":         envelopeVersion,
		"type":      "event",
		"device_id": deviceID,
		"push_seq":  seq,
		"ts":        time.Now().Unix(),
		"event":     event,
	}
	// beta.3 Fixes 3+6: hoist workspace label + set a category so Android/iOS
	// can route the push at their own priority. hook_request → "msg" (highest
	// user-attention bucket — user is being asked to decide something). Other
	// event types fall back to Android's default "recommendation" bucket.
	if evMap, ok := event.(map[string]any); ok {
		if wsID, ok := evMap["workspace_id"].(string); ok && wsID != "" {
			env["workspace_id"] = wsID
		}
		if wsName, ok := evMap["workspace_name"].(string); ok && wsName != "" {
			env["workspace_name"] = wsName
		}
		if typ, _ := evMap["type"].(string); typ == "hook_request" {
			env["category"] = "msg"
		}
	}
	return env
}

// Handler serves GET /api/v2/push/subscribe (WS upgrade). Auth: the device's
// long-term token via Authorization: Bearer or ?token=. Replays queued events
// with push_seq > ?cursor, then streams live + processes acks + heartbeats.
func (s *Server) Handler(w http.ResponseWriter, r *http.Request) {
	deviceID, _, ok := s.deps.ResolveToken(bearer(r))
	if !ok || deviceID == "" {
		logger.Warn("subscribe rejected (bad/missing token)", "remote", r.RemoteAddr)
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	cursor := parseCursor(r.URL.Query().Get("cursor"))
	conn, err := s.up.Upgrade(w, r, nil)
	if err != nil {
		logger.Warn("subscribe upgrade failed", "device", deviceID, "err", err)
		return // Upgrade already wrote the error
	}
	logger.Info("device subscribed", "device", deviceID, "cursor", cursor)
	c := &client{deviceID: deviceID, conn: conn, sent: cursor}
	defer func() {
		s.unregister(c)
		_ = conn.Close()
	}()

	// hello first, then register: once registered a concurrent Notify may
	// flush, and no event may precede the hello. An event that lands before
	// the register is in the queue, and the flush below sends it.
	_ = c.writeJSON(map[string]any{
		"v": envelopeVersion, "type": "hello", "device_id": deviceID, "heartbeat_sec": heartbeatSec,
	})
	s.register(c)
	n, err := s.flush(c)
	if n > 0 {
		logger.Info("replayed queued events", "count", n, "device", deviceID, "cursor", cursor)
	}
	if err != nil {
		return
	}

	// Heartbeat: ping every heartbeatSec; drop if no pong within pongWait.
	_ = conn.SetReadDeadline(time.Now().Add(pongWait))
	conn.SetPongHandler(func(string) error {
		return conn.SetReadDeadline(time.Now().Add(pongWait))
	})
	done := make(chan struct{})
	defer close(done)
	go func() {
		t := time.NewTicker(heartbeatSec * time.Second)
		defer t.Stop()
		for {
			select {
			case <-done:
				return
			case <-t.C:
				c.mu.Lock()
				_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
				err := conn.WriteControl(websocket.PingMessage, nil, time.Now().Add(writeWait))
				c.mu.Unlock()
				if err != nil {
					return
				}
			}
		}
	}()

	for {
		_, msg, err := conn.ReadMessage()
		if err != nil {
			return
		}
		var in struct {
			Type    string `json:"type"`
			PushSeq int64  `json:"push_seq"`
		}
		if json.Unmarshal(msg, &in) != nil {
			continue
		}
		switch in.Type {
		case "ack":
			if in.PushSeq > 0 {
				s.deps.Ack(deviceID, in.PushSeq)
			}
		case "ping":
			_ = c.writeJSON(map[string]any{"v": envelopeVersion, "type": "pong"})
		}
	}
}

// bearer pulls the token from Authorization: Bearer or the ?token= fallback.
func bearer(r *http.Request) string {
	if h := r.Header.Get("Authorization"); strings.HasPrefix(h, "Bearer ") {
		return strings.TrimSpace(strings.TrimPrefix(h, "Bearer "))
	}
	return r.URL.Query().Get("token")
}

func parseCursor(s string) int64 {
	n, err := strconv.ParseInt(s, 10, 64)
	if err != nil || n < 0 {
		return 0
	}
	return n
}
