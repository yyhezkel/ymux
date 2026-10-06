package term

// webpush.go — Phase 114 (WEB-DESIGN E): notifications for the browser app,
// including when it is closed (Web Push; internal/webpush does the crypto).
//
// Three moments are worth a notification, chosen so the default `none`
// policy is not silent:
//
//   - gate      — a blocking card waits for a decision (policy gate). The
//                 notification carries Approve / Deny; the service worker
//                 answers through POST /api/v2/feed/{id}/decide.
//   - attention — Claude's own Notification hook (it is asking for a
//                 permission in the terminal). idle_prompt is skipped: the
//                 `done` that preceded it already said so.
//   - done      — the stop card ("Claude finished — your turn").
//
// Who gets them: every browser that subscribed, as long as its device still
// holds shell:attach (checked at send time, so a revoke or a removed grant
// stops them; that browser's record is dropped). A 404/410 from the push
// service drops the record too — the browser unsubscribed or was reset.
//
// Rule #1: the payload carries the card text — the user's own content, to
// the user's own browser, encrypted end to end. Logs carry the kind, the
// device, the status and the push service's host only.

import (
	"context"
	"encoding/json"
	"net/http"
	"net/url"
	"path/filepath"
	"strings"
	"time"

	"ymux-server/internal/agent"
	"ymux-server/internal/auth"
	"ymux-server/internal/webpush"
)

// ownerDevice is the record owner for the daemon's own token (no device id).
const ownerDevice = "owner"

// Notification kinds, the payload's `kind`.
const (
	noteGate      = "gate"
	noteAttention = "attention"
	noteDone      = "done"
)

// webNote is one notification before it is rendered per subscriber language.
type webNote struct {
	Kind      string
	RequestID string
	PaneID    string
	Session   string
	TitleEn   string
	BodyEn    string
	TitleHe   string
	BodyHe    string
}

// webPusher is the send side; nil on the Service until SetWebPush succeeds.
type webPusher struct {
	keys  *webpush.Keys
	store *webpush.Store
	// deviceOf maps a bearer to its device id; scopesByID answers an ACTIVE
	// device's grants (ok=false once revoked).
	deviceOf   func(token string) (id string, admin, ok bool)
	scopesByID func(id string) (scopes string, ok bool)
	// send is the network call; tests replace it.
	send func(ctx context.Context, sub webpush.Subscription, payload []byte, o webpush.Options) (int, error)
	// label names a session's workspace for the notification title.
	label func(session string) string
}

// SetWebPush turns Web Push on: the VAPID key and the subscriptions live in
// the data dir. deviceOf / scopesByID come from the chat store (nil when it
// did not open — then only the owner token can subscribe).
func (s *Service) SetWebPush(dataDir string,
	deviceOf func(token string) (string, bool, bool),
	scopesByID func(id string) (string, bool)) {
	keys, err := webpush.LoadOrCreateKeys(filepath.Join(dataDir, "vapid.pem"))
	if err != nil {
		logger.Warn("web push disabled: VAPID key unavailable", "err", err)
		return
	}
	client := &http.Client{Timeout: 15 * time.Second}
	wp := &webPusher{
		keys:       keys,
		store:      webpush.NewStore(filepath.Join(dataDir, "webpush.json")),
		deviceOf:   deviceOf,
		scopesByID: scopesByID,
		send: func(ctx context.Context, sub webpush.Subscription, p []byte, o webpush.Options) (int, error) {
			return keys.Send(ctx, client, sub, p, o)
		},
	}
	s.enableWebPush(wp)
	logger.Info("web push enabled")
}

func (s *Service) enableWebPush(wp *webPusher) {
	s.webpush = wp
	if s.hooks != nil {
		wp.label = s.hooks.workspaceLabel
		s.hooks.notify = wp.notify
	}
}

// deviceFor is the record owner for a request already past s.gate.
func (s *Service) deviceFor(r *http.Request) string {
	tok := bearer(r)
	if s.token != "" && tok == s.token {
		return ownerDevice
	}
	if s.webpush == nil || s.webpush.deviceOf == nil {
		return ""
	}
	if id, admin, ok := s.webpush.deviceOf(tok); ok {
		if admin {
			return ownerDevice
		}
		return id
	}
	return ""
}

// handleWebPushKey: GET /api/v2/webpush/key → the applicationServerKey.
func (s *Service) handleWebPushKey(w http.ResponseWriter, r *http.Request) {
	if s.webpush == nil {
		http.Error(w, "web push is not available on this daemon", http.StatusServiceUnavailable)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"public_key": s.webpush.keys.PublicKey()})
}

// subscribeBody is PushSubscription.toJSON() plus the notification language.
type subscribeBody struct {
	Endpoint string `json:"endpoint"`
	Keys     struct {
		P256dh string `json:"p256dh"`
		Auth   string `json:"auth"`
	} `json:"keys"`
	Lang string `json:"lang"`
}

// handleWebPushSubs: POST subscribes (upsert by endpoint), DELETE removes
// the caller's own record for an endpoint.
func (s *Service) handleWebPushSubs(w http.ResponseWriter, r *http.Request) {
	if s.webpush == nil {
		http.Error(w, "web push is not available on this daemon", http.StatusServiceUnavailable)
		return
	}
	dev := s.deviceFor(r)
	if dev == "" {
		http.Error(w, "unknown device", http.StatusUnauthorized)
		return
	}
	var b subscribeBody
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 8<<10)).Decode(&b); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	if r.Method == http.MethodDelete {
		removed, err := s.webpush.store.Remove(b.Endpoint, dev)
		if err != nil {
			logger.Warn("web push unsubscribe failed", "device", dev, "err", err)
			http.Error(w, "could not save", http.StatusInternalServerError)
			return
		}
		logger.Info("web push unsubscribed", "device", dev, "removed", removed)
		writeJSON(w, http.StatusOK, map[string]bool{"removed": removed})
		return
	}
	sub := webpush.Subscription{Endpoint: b.Endpoint, P256dh: b.Keys.P256dh, Auth: b.Keys.Auth}
	if err := webpush.Validate(sub); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	lang := "en"
	if b.Lang == "he" {
		lang = "he"
	}
	rec := webpush.Record{DeviceID: dev, Lang: lang, Sub: sub, CreatedMs: time.Now().UnixMilli()}
	if err := s.webpush.store.Put(rec); err != nil {
		logger.Warn("web push subscribe failed", "device", dev, "err", err)
		http.Error(w, "could not save", http.StatusInternalServerError)
		return
	}
	logger.Info("web push subscribed", "device", dev, "service", endpointHost(sub.Endpoint), "lang", lang)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// handleWebPushTest: POST /api/v2/webpush/test — one notification to the
// caller's own browsers, so "did it work" has an answer without a hook.
func (s *Service) handleWebPushTest(w http.ResponseWriter, r *http.Request) {
	if s.webpush == nil {
		http.Error(w, "web push is not available on this daemon", http.StatusServiceUnavailable)
		return
	}
	dev := s.deviceFor(r)
	if dev == "" {
		http.Error(w, "unknown device", http.StatusUnauthorized)
		return
	}
	sent := s.webpush.deliver(webNote{
		Kind: "test", RequestID: "test",
		TitleEn: "YMUX", BodyEn: "Notifications work.",
		TitleHe: "YMUX", BodyHe: "ההתראות עובדות.",
	}, dev)
	writeJSON(w, http.StatusOK, map[string]int{"sent": sent})
}

// notify is the registry's hook: fan a note out without blocking the hook.
func (wp *webPusher) notify(n webNote) {
	go wp.deliver(n, "")
}

// deliver sends n to every allowed record (only onlyDevice's when set) and
// returns how many the push services accepted.
func (wp *webPusher) deliver(n webNote, onlyDevice string) int {
	recs, err := wp.store.All()
	if err != nil {
		logger.Warn("web push: subscriptions unreadable", "err", err)
		return 0
	}
	ws := ""
	if wp.label != nil && n.Session != "" {
		ws = wp.label(n.Session)
	}
	sent := 0
	for _, rec := range recs {
		if onlyDevice != "" && rec.DeviceID != onlyDevice {
			continue
		}
		if !wp.allowed(rec.DeviceID) {
			_, _ = wp.store.Remove(rec.Sub.Endpoint, "")
			logger.Info("web push: dropped a subscription of a device without shell:attach", "device", rec.DeviceID)
			continue
		}
		payload, err := n.render(rec.Lang, ws)
		if err != nil {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		code, err := wp.send(ctx, rec.Sub, payload, n.options())
		cancel()
		host := endpointHost(rec.Sub.Endpoint)
		switch {
		case err != nil:
			logger.Warn("web push send failed", "kind", n.Kind, "device", rec.DeviceID, "service", host, "err", err)
		case code == http.StatusNotFound || code == http.StatusGone:
			_, _ = wp.store.Remove(rec.Sub.Endpoint, "")
			logger.Info("web push: subscription expired, dropped", "device", rec.DeviceID, "service", host, "status", code)
		case code >= 200 && code < 300:
			sent++
			logger.Debug("web push sent", "kind", n.Kind, "device", rec.DeviceID, "service", host)
		default:
			logger.Warn("web push refused", "kind", n.Kind, "device", rec.DeviceID, "service", host, "status", code)
		}
	}
	return sent
}

func (wp *webPusher) allowed(dev string) bool {
	if dev == ownerDevice {
		return true
	}
	if wp.scopesByID == nil {
		return false
	}
	scopes, ok := wp.scopesByID(dev)
	return ok && auth.HasScope(scopes, auth.ScopeShellAttach)
}

// render is the payload the service worker reads (app/public/sw.js).
func (n webNote) render(lang, workspace string) ([]byte, error) {
	title, body := n.TitleEn, n.BodyEn
	if lang == "he" {
		title, body = n.TitleHe, n.BodyHe
	}
	if workspace != "" {
		title += " · " + workspace
	}
	return json.Marshal(map[string]any{
		"v":          1,
		"kind":       n.Kind,
		"lang":       lang,
		"request_id": n.RequestID,
		"pane_id":    n.PaneID,
		"session":    n.Session,
		"title":      agent.ClipChars(title, 120),
		"body":       agent.ClipChars(body, 400),
	})
}

// options: a gate is worth waking for and is useless after its wait (600 s
// at most); a done collapses into the pane's next one.
func (n webNote) options() webpush.Options {
	switch n.Kind {
	case noteGate:
		return webpush.Options{TTL: 10 * time.Minute, Urgency: "high", Topic: topic(n.RequestID)}
	case noteAttention:
		return webpush.Options{TTL: 30 * time.Minute, Urgency: "high", Topic: topic("a" + n.PaneID)}
	default:
		return webpush.Options{TTL: time.Hour, Urgency: "normal", Topic: topic("d" + n.PaneID)}
	}
}

// topic is an RFC 8030 Topic: ≤32 characters of the base64url alphabet.
func topic(s string) string {
	b := strings.Map(func(r rune) rune {
		if r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-' || r == '_' {
			return r
		}
		return -1
	}, s)
	if len(b) > 32 {
		b = b[len(b)-32:]
	}
	return b
}

func endpointHost(ep string) string {
	if u, err := url.Parse(ep); err == nil {
		return u.Host
	}
	return ""
}

// workspaceLabel names a session's browser workspace, "" when it has none.
func (r *HookRegistry) workspaceLabel(session string) string {
	r.mu.Lock()
	e := r.byName[session]
	id := ""
	if e != nil {
		id = e.workspaceID
	}
	r.mu.Unlock()
	if id == "" || r.webws == nil {
		return ""
	}
	if w, ok := r.webws.get(id); ok {
		return w.Name
	}
	return ""
}

// noteForCard turns a stored card into a notification when it is one worth
// sending: a blocking card (gate) or a stop card (done).
func noteForCard(e *feedEntry) (webNote, bool) {
	it := e.item
	kind := ""
	switch {
	case it.Blocking:
		kind = noteGate
	case it.Subkind == "stop":
		kind = noteDone
	default:
		return webNote{}, false
	}
	return webNote{
		Kind: kind, RequestID: it.RequestID, PaneID: it.PaneID, Session: it.Session,
		TitleEn: it.Title, BodyEn: it.Summary, TitleHe: e.titleHe, BodyHe: e.summaryHe,
	}, true
}

// attentionNote is Claude's Notification hook as a notification.
func attentionNote(p feedPushParams, pane, session string) webNote {
	var payload map[string]any
	if len(p.Payload) > 0 {
		_ = json.Unmarshal(p.Payload, &payload)
	}
	tEn, bEn := agent.Humanize("notification", payload, "", "en")
	tHe, bHe := agent.Humanize("notification", payload, "", "he")
	return webNote{
		Kind: noteAttention, RequestID: p.RequestID, PaneID: pane, Session: session,
		TitleEn: tEn, BodyEn: bEn, TitleHe: tHe, BodyHe: bHe,
	}
}
