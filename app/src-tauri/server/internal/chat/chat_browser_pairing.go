package chat

// chat_browser_pairing.go — Phase 96, browser-initiated pairing
// (docs/DECISIONS.md, option B2).
//
// The existing mobile flow runs desktop-first: ymux issues a one-shot, the QR
// carries it, the phone redeems. That cannot work for a browser on a machine
// ymux is not running on — the one-shot is 48 characters and there is nothing
// to scan. So the browser asks instead:
//
//	browser  →  POST /api/pairing/request        (public)
//	            ← { request_id, code, one_shot_token }
//	daemon   →  feed.push (blocking) through the reverse tunnel
//	            → an ordinary ymux Allow/Deny card, with the SAME code
//	human    →  matches the two codes, presses Allow
//	daemon   →  the row flips `requested` → `pending`
//	browser  →  POST /api/pairing/redeem with the one-shot it already holds
//
// The last step is the UNCHANGED existing endpoint. Once approved, a row from
// this road is indistinguishable from one a QR produced, so nothing downstream
// needs to know about any of this.
//
// **The code is a matching device, not a secret.** Its job is to let a human
// tell their own browser apart from someone else's request arriving at the same
// moment. Approval is authorised by the owner token, never by knowing a code.
//
// **Approval does NOT grant a shell.** An approved browser gets the ordinary
// device grant, and `"all"` deliberately excludes `auth.ScopeShellAttach`
// (Phase 95). Turning on a terminal is a second, separate act by the owner in
// the device list. Two decisions rather than one is the point: "is this browser
// mine?" and "may it run commands on my machine?" are different questions and
// should not share a button.

import (
	"crypto/rand"
	"encoding/json"
	"math/big"
	"net/http"
	"strings"
	"sync"
	"time"
)

// requestTTL is how long an unanswered request lives. Same 5 minutes as a
// one-shot: the browser is sitting on the page, waiting.
const requestTTL = 5 * time.Minute

// Limits on an endpoint that, by its nature, cannot require a credential.
const (
	maxRequestsPerIP     = 5
	requestWindow        = 10 * time.Minute
	maxOutstandingGlobal = 20
)

// ApprovalAsker pushes a blocking approval to the desktop and returns its
// decision ("allow" | "deny" | "timeout").
//
// Injected from cmd rather than imported, so this package stays clear of the
// outbound-tunnel client — the same shape as SetPushLister / SetDeviceAuth.
// nil means no desktop reachable, and a request then fails closed.
type ApprovalAsker func(requestID, title, summary string, payload map[string]any, waitSeconds int) (string, error)

// SetApprovalAsker wires the desktop push used to approve browser pairings.
func (c *ChatAPI) SetApprovalAsker(fn ApprovalAsker) { c.askApproval = fn }

// ipLimiter is a fixed-window counter per client IP.
type ipLimiter struct {
	mu   sync.Mutex
	hits map[string][]time.Time
}

func newIPLimiter() *ipLimiter { return &ipLimiter{hits: map[string][]time.Time{}} }

// allow records an attempt and reports whether it is within the window budget.
func (l *ipLimiter) allow(ip string, now time.Time) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	kept := l.hits[ip][:0]
	for _, t := range l.hits[ip] {
		if now.Sub(t) < requestWindow {
			kept = append(kept, t)
		}
	}
	if len(kept) >= maxRequestsPerIP {
		l.hits[ip] = kept
		return false
	}
	l.hits[ip] = append(kept, now)
	// Bound the map itself: without this, one attacker cycling source
	// addresses grows it without limit.
	if len(l.hits) > 1000 {
		for k, v := range l.hits {
			if len(v) == 0 || now.Sub(v[len(v)-1]) >= requestWindow {
				delete(l.hits, k)
			}
		}
	}
	return true
}

// randCode returns a 6-digit display code, grouped as "482 193".
//
// Digits only, on purpose: it is read aloud off one screen and compared on
// another, sometimes in a Hebrew interface, and letters bring O/0 and I/1
// confusions that a matching check cannot afford.
func randCode() string {
	var sb strings.Builder
	for i := 0; i < 6; i++ {
		n, err := rand.Int(rand.Reader, big.NewInt(10))
		if err != nil {
			return "" // caller treats an empty code as a failure
		}
		if i == 3 {
			sb.WriteByte(' ')
		}
		sb.WriteByte(byte('0' + n.Int64()))
	}
	return sb.String()
}

// NB: `clientIP` lives in chat_api.go and is reused here. It reads nginx's
// forwarded headers, which is right for display and rate limiting — and is
// exactly why neither may ever be used for authorisation: a header is
// client-controlled unless the proxy overwrites it.

// clip bounds an untrusted string before it is stored and later displayed.
func clip(s string, n int) string {
	s = strings.TrimSpace(s)
	s = strings.Map(func(r rune) rune {
		if r < 0x20 || r == 0x7f {
			return -1 // control characters would corrupt the approval card
		}
		return r
	}, s)
	if len(s) > n {
		return s[:n]
	}
	return s
}

func (c *ChatAPI) registerBrowserPairingRoutes(mux *http.ServeMux) {
	mux.HandleFunc("POST /api/pairing/request", c.handlePairRequest)
	mux.HandleFunc("GET /api/pairing/request/status", c.handlePairRequestStatus)
	mux.HandleFunc("GET /api/pairing/requests", c.adminGuard(c.handleListRequests))
	mux.HandleFunc("POST /api/pairing/requests/{id}/approve", c.adminGuard(c.handleApproveRequest))
	mux.HandleFunc("POST /api/pairing/requests/{id}/deny", c.adminGuard(c.handleDenyRequest))
}

// POST /api/pairing/request — public. Creates a pending approval and pushes it
// to the desktop, returning immediately so the browser can start polling.
func (c *ChatAPI) handlePairRequest(w http.ResponseWriter, r *http.Request) {
	var body struct {
		DeviceName string `json:"device_name"`
	}
	if r.Body != nil {
		_ = json.NewDecoder(r.Body).Decode(&body)
	}
	ip := clientIP(r)
	now := time.Now()

	if !c.pairLimiter.allow(ip, now) {
		logger.Warn("pairing request rate-limited", "ip", ip)
		http.Error(w, "too many pairing requests", http.StatusTooManyRequests)
		return
	}
	c.store.pruneRequests(now.Unix())
	if c.store.countRequests(now.Unix()) >= maxOutstandingGlobal {
		logger.Warn("pairing request refused: too many outstanding", "ip", ip)
		http.Error(w, "too many pending requests", http.StatusTooManyRequests)
		return
	}
	// Fail closed and SAY SO: without a desktop nobody can approve, and a
	// browser left polling a request no human will ever see is the worst of
	// both worlds. This is the "ymux must be open" constraint surfacing as a
	// message rather than as a hang.
	if c.askApproval == nil {
		http.Error(w, "ymux is not connected to this server — open it and try again",
			http.StatusServiceUnavailable)
		return
	}

	code := randCode()
	if code == "" {
		http.Error(w, "could not generate a code", http.StatusInternalServerError)
		return
	}
	oneShot := randHex(24)
	dev := &PairedDevice{
		ID:        "dev_" + randHex(6),
		Name:      clip(body.DeviceName, 64),
		OtsHash:   hashToken(oneShot),
		Scopes:    "all", // NB: excludes shell:attach — see the file header
		CreatedAt: now.Unix(),
		ExpiresAt: now.Add(requestTTL).Unix(),
	}
	ua := clip(r.Header.Get("User-Agent"), 200)
	if err := c.store.requestDevice(dev, code, ip, ua); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	logger.Info("browser pairing requested", "request", dev.ID, "ip", ip)

	// Ask the human out-of-band. The HTTP response must not wait on a person:
	// the browser needs its one-shot now so it can poll, and an approval that
	// takes two minutes would otherwise hold a request open for two minutes.
	go c.awaitApproval(dev.ID, code, ip, ua)

	writeJSON(w, map[string]any{
		"request_id":     dev.ID,
		"code":           code,
		"one_shot_token": oneShot, // shown once; the browser keeps it to redeem
		"expires_at":     dev.ExpiresAt,
	})
}

// awaitApproval blocks on the desktop card and records the outcome.
func (c *ChatAPI) awaitApproval(id, code, ip, ua string) {
	summary := "Code " + code + " · from " + ip
	if ua != "" {
		summary += " · " + ua
	}
	decision, err := c.askApproval(
		id,
		"A browser is asking for access",
		summary,
		map[string]any{"code": code, "ip": ip, "user_agent": ua, "request_id": id},
		int(requestTTL.Seconds()),
	)
	if err != nil {
		// Unreachable desktop, dead tunnel, or a build too old to gate this.
		// Leave the row to expire rather than approving anything.
		logger.Warn("browser pairing approval failed", "request", id, "err", err)
		return
	}
	if decision != "allow" {
		logger.Info("browser pairing refused", "request", id, "decision", decision)
		c.store.denyRequest(id)
		return
	}
	if c.store.approveRequest(id, "all", time.Now().Unix()) {
		logger.Info("browser pairing approved", "request", id)
	} else {
		logger.Warn("browser pairing approved too late (expired)", "request", id)
	}
}

// GET /api/pairing/request/status?one_shot_token=… — the browser's poll.
//
// Authenticated by the one-shot the browser already holds, which is why this
// needs no other credential: only the client that made the request has it.
func (c *ChatAPI) handlePairRequestStatus(w http.ResponseWriter, r *http.Request) {
	tok := r.URL.Query().Get("one_shot_token")
	d, ok := c.store.requestByOts(hashToken(tok))
	if !ok {
		// Denied requests are deleted, so "gone" and "never existed" are the
		// same answer — deliberately, so a poller cannot probe for ids.
		writeJSON(w, map[string]any{"status": "gone"})
		return
	}
	if d.ExpiresAt < time.Now().Unix() {
		writeJSON(w, map[string]any{"status": "expired"})
		return
	}
	// "pending" here means approved-and-awaiting-redeem: the browser should now
	// call POST /api/pairing/redeem with the one-shot it already has.
	writeJSON(w, map[string]any{"status": d.Status, "expires_at": d.ExpiresAt})
}

// GET /api/pairing/requests (admin) — what is awaiting a decision. Exists for
// the desktop's device list; the approval itself arrives as a feed card.
func (c *ChatAPI) handleListRequests(w http.ResponseWriter, _ *http.Request) {
	now := time.Now().Unix()
	c.store.pruneRequests(now)
	reqs, err := c.store.listRequests(now)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	out := make([]map[string]any, 0, len(reqs))
	for _, d := range reqs {
		out = append(out, map[string]any{
			"request_id":  d.ID,
			"device_name": d.Name,
			"code":        d.Code,
			"ip":          d.RequestIP,
			"user_agent":  d.UserAgent,
			"created_at":  d.CreatedAt,
			"expires_at":  d.ExpiresAt,
		})
	}
	writeJSON(w, map[string]any{"requests": out})
}

// POST /api/pairing/requests/{id}/approve (admin).
//
// `scopes` is accepted so an owner can approve straight into a wider grant, but
// it is NOT how a browser normally gets a shell: the body is optional and the
// default is the ordinary device grant, which excludes shell:attach.
func (c *ChatAPI) handleApproveRequest(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Scopes []string `json:"scopes"`
	}
	if r.Body != nil {
		_ = json.NewDecoder(r.Body).Decode(&body)
	}
	scopes := "all"
	if len(body.Scopes) > 0 {
		if b, err := json.Marshal(body.Scopes); err == nil {
			scopes = string(b)
		}
	}
	if !c.store.approveRequest(r.PathValue("id"), scopes, time.Now().Unix()) {
		http.Error(w, "no such pending request", http.StatusNotFound)
		return
	}
	writeJSON(w, map[string]any{"ok": true})
}

// POST /api/pairing/requests/{id}/deny (admin).
func (c *ChatAPI) handleDenyRequest(w http.ResponseWriter, r *http.Request) {
	if !c.store.denyRequest(r.PathValue("id")) {
		http.Error(w, "no such pending request", http.StatusNotFound)
		return
	}
	writeJSON(w, map[string]any{"ok": true})
}
