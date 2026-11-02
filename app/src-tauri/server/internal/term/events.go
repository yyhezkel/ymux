package term

// events.go — GET /api/v2/events, the live channel a browser runs on
// (Phase 101, WEB-DESIGN B3).
//
// One WebSocket for the whole box, carrying the events the desktop's
// frontend listens to today, under the same names and with the same JSON:
//
//	server → client  {"type":"hello",              "data":{…hydration…}}
//	                 {"type":"feed:item-added",    "data":FeedItem}
//	                 {"type":"feed:item-resolved", "data":{"request_id","decision"}}
//	                 {"type":"pane:agent-run",     "data":agent.AgentRunEvent}
//	                 {"type":"pane:brief",         "data":{"pane_id","entry"}}
//	client → server  {"type":"feed.decide", "request_id":…, "decision":"allow"|"deny"}
//
// `hello` is the hydration the desktop does with pane_agent_states /
// pane_briefs / feed_list after a webview reload, sent first on every
// connect so a client never has to stitch a REST snapshot to a live stream.
// The subscriber is registered BEFORE the snapshot is taken, so an event in
// between arrives twice rather than never; clients already drop a brief or a
// light whose seq is not newer, and a feed item by request_id.
//
// It sits behind Service.gate like every terminal route: owner token, or a
// device explicitly granted shell:attach (Yossi 2026-10-05). The feed shows
// the prompts and tool input of sessions that token can already open.
//
// A subscriber that cannot keep up is dropped, not waited for: one slow phone
// must not stall the hook path. It reconnects and gets a fresh hello.

import (
	"encoding/json"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"

	"ymux-server/internal/agent"
)

// subBuffer is how many frames may queue for one subscriber before it is
// considered stuck and dropped.
const subBuffer = 256

type subscriber struct {
	lang string
	ch   chan []byte
}

// eventHub fans events out to every connected events socket.
type eventHub struct {
	mu   sync.Mutex
	subs map[*subscriber]struct{}
}

func newEventHub() *eventHub { return &eventHub{subs: map[*subscriber]struct{}{}} }

func (h *eventHub) add(lang string) *subscriber {
	s := &subscriber{lang: lang, ch: make(chan []byte, subBuffer)}
	h.mu.Lock()
	h.subs[s] = struct{}{}
	h.mu.Unlock()
	return s
}

// remove unregisters s and closes its channel. Safe to call twice.
func (h *eventHub) remove(s *subscriber) {
	h.mu.Lock()
	if _, ok := h.subs[s]; ok {
		delete(h.subs, s)
		close(s.ch)
	}
	h.mu.Unlock()
}

func (h *eventHub) count() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	return len(h.subs)
}

// publish sends one event to everyone. render builds the data for a
// language; it is called at most once per language present.
func (h *eventHub) publish(typ string, render func(lang string) any) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if len(h.subs) == 0 {
		return
	}
	frames := map[string][]byte{}
	for s := range h.subs {
		f, ok := frames[s.lang]
		if !ok {
			b, err := json.Marshal(map[string]any{"type": typ, "data": render(s.lang)})
			if err != nil {
				logger.Error("event encode failed", "type", typ, "err", err)
				return
			}
			f, frames[s.lang] = b, b
		}
		select {
		case s.ch <- f:
		default:
			// Stuck. Dropping it closes its channel, which ends its socket;
			// the client reconnects and re-hydrates from hello.
			delete(h.subs, s)
			close(s.ch)
			logger.Warn("events subscriber dropped: not keeping up", "type", typ)
		}
	}
}

// same renders a language-independent payload.
func same(v any) func(string) any { return func(string) any { return v } }

// helloData is the hydration frame.
type helloData struct {
	// Keyed by pane id; the values are the pane:agent-run payload.
	PaneAgentStates map[string]agent.AgentRunEvent `json:"pane_agent_states"`
	// Keyed by pane id; the values are the pane:brief entry.
	PaneBriefs map[string]agent.BriefEntry `json:"pane_briefs"`
	// Keyed by pane id: which tmux session it is and its hook policy.
	Panes map[string]paneInfo `json:"panes"`
	// Oldest first, in the subscriber's language.
	Feed []FeedItem `json:"feed"`
	// Phase 102 (B4): set-status text by pane, notifications oldest first,
	// and the box's listening ports.
	PaneStatus    map[string]string  `json:"pane_status"`
	Notifications []NotificationItem `json:"notifications"`
	Ports         []ListenPort       `json:"ports"`
}

type paneInfo struct {
	Session string `json:"session"`
	Policy  string `json:"policy"`
}

func (r *HookRegistry) hello(lang string) helloData {
	d := helloData{
		PaneAgentStates: map[string]agent.AgentRunEvent{},
		PaneBriefs:      map[string]agent.BriefEntry{},
		Panes:           map[string]paneInfo{},
		PaneStatus:      map[string]string{},
		Ports:           []ListenPort{},
	}
	r.mu.Lock()
	for _, e := range r.byName {
		if e.released {
			continue
		}
		d.PaneAgentStates[e.paneID] = e.run.Event(e.paneID)
		d.PaneBriefs[e.paneID] = e.brief
		d.Panes[e.paneID] = paneInfo{Session: e.name, Policy: e.policy}
		if e.status != "" {
			d.PaneStatus[e.paneID] = e.status
		}
	}
	r.mu.Unlock()
	d.Feed = r.feed.list(lang)
	d.Notifications = r.notifs.list()
	if r.ports != nil {
		d.Ports = r.ports.list()
	}
	return d
}

// eventLang is the card language a subscriber asked for; anything but "he"
// is English, as in agent.Humanize.
func eventLang(r *http.Request) string {
	if r.URL.Query().Get("lang") == "he" {
		return "he"
	}
	return "en"
}

// clientEvent is the client→server frame.
type clientEvent struct {
	Type      string `json:"type"`
	RequestID string `json:"request_id"`
	Decision  string `json:"decision"`
}

// handleEvents upgrades and serves one events socket.
func (s *Service) handleEvents(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "events unavailable", http.StatusServiceUnavailable)
		return
	}
	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		logger.Warn("events upgrade failed", "err", err)
		return
	}
	defer conn.Close()
	conn.SetReadLimit(64 * 1024)

	lang := eventLang(r)
	hub := s.hooks.hub
	sub := hub.add(lang)
	defer hub.remove(sub)
	logger.Info("events connected", "lang", lang, "subscribers", hub.count(), "ip", clientIP(r))
	started := time.Now()

	hello, err := json.Marshal(map[string]any{"type": "hello", "data": s.hooks.hello(lang)})
	if err != nil {
		logger.Error("hello encode failed", "err", err)
		return
	}
	_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
	if conn.WriteMessage(websocket.TextMessage, hello) != nil {
		return
	}

	// Reader: feed.decide. Never writes to the socket (gorilla: one writer).
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			_, data, err := conn.ReadMessage()
			if err != nil {
				return
			}
			var ev clientEvent
			if json.Unmarshal(data, &ev) != nil || ev.Type != "feed.decide" {
				continue // a frame we don't understand is not fatal
			}
			s.hooks.decide(ev.RequestID, ev.Decision, "events")
		}
	}()

	ping := time.NewTicker(pingEvery)
	defer ping.Stop()
	for {
		select {
		case b, ok := <-sub.ch:
			if !ok {
				logger.Info("events disconnected", "why", "dropped", "seconds", int(time.Since(started).Seconds()))
				return
			}
			_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
			if conn.WriteMessage(websocket.TextMessage, b) != nil {
				return
			}
		case <-ping.C:
			_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
			if conn.WriteMessage(websocket.PingMessage, nil) != nil {
				return
			}
		case <-done:
			logger.Info("events disconnected", "why", "closed", "seconds", int(time.Since(started).Seconds()))
			return
		}
	}
}

// handleDecide is POST /api/v2/feed/{request_id}/decide — the same decision
// as the events frame, for a client that is not holding the socket.
func (s *Service) handleDecide(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "feed unavailable", http.StatusServiceUnavailable)
		return
	}
	var body struct {
		Decision string `json:"decision"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.Decision != "allow" && body.Decision != "deny" {
		http.Error(w, `decision must be "allow" or "deny"`, http.StatusBadRequest)
		return
	}
	id := r.PathValue("request_id")
	if !s.hooks.decide(id, body.Decision, "rest") {
		// Unknown, already decided, or timed out — the first answer won.
		http.Error(w, "no pending request with that id", http.StatusNotFound)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"request_id": id, "decision": body.Decision})
}

// handlePolicy is POST /api/v2/term/sessions/{name}/policy {"policy":"none"|"gate"}.
func (s *Service) handlePolicy(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil {
		http.Error(w, "hooks unavailable", http.StatusServiceUnavailable)
		return
	}
	var body struct {
		Policy string `json:"policy"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if !validPolicy(body.Policy) {
		http.Error(w, `policy must be "none" or "gate"`, http.StatusBadRequest)
		return
	}
	name := r.PathValue("name")
	if !s.hooks.SetPolicy(name, body.Policy) {
		http.Error(w, "no hook-enabled session with that name", http.StatusNotFound)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"name": name, "policy": body.Policy})
}
