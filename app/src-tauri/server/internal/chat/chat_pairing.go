package chat

// Phase 70.B — mobile pairing endpoints. The desktop (admin token) ISSUES a
// pending device with a one-shot token (shown as a QR). The phone REDEEMS the
// one-shot for a long-term bearer, which then authenticates all REST/WS calls
// (Insights + chat) via the existing auth, now backed by paired_devices.
//
// Tokens are returned in plaintext exactly once; only sha256 hashes persist
// (Rule #2). One-shots are single-use + short-TTL.

import (
	"encoding/json"
	"net/http"
	"strings"
	"time"
)

// pairingTTL — how long a one-shot pairing token is valid.
const pairingTTL = 5 * time.Minute

func nowUnix() int64 { return time.Now().Unix() }

func (c *ChatAPI) registerPairingRoutes(mux *http.ServeMux) {
	mux.HandleFunc("/api/pairing/issue", c.adminGuard(c.handlePairIssue))
	// Phase 77 S6: /api/pairing/redeem is served as a typed huma op in the api
	// package (so it lands in the generated OpenAPI + SDKs) via ChatAPI.Redeem.
	mux.HandleFunc("/api/pairing/devices", c.adminGuard(c.handlePairDevices))
	mux.HandleFunc("/api/pairing/devices/", c.adminGuard(c.handlePairDeviceItem))
}

// POST /api/pairing/issue (admin) → { device_id, one_shot_token, expires_at }
func (c *ChatAPI) handlePairIssue(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "POST only", http.StatusMethodNotAllowed)
		return
	}
	var body struct {
		DeviceName string   `json:"device_name"`
		Scopes     []string `json:"scopes"`
	}
	if r.Body != nil {
		_ = json.NewDecoder(r.Body).Decode(&body)
	}
	scopes := "all" // decision #4: all scopes always
	if len(body.Scopes) > 0 {
		if b, err := json.Marshal(body.Scopes); err == nil {
			scopes = string(b)
		}
	}
	now := nowUnix()
	oneShot := randHex(24)
	dev := &PairedDevice{
		ID:        "dev_" + randHex(6),
		Name:      body.DeviceName,
		OtsHash:   hashToken(oneShot),
		Scopes:    scopes,
		CreatedAt: now,
		ExpiresAt: now + int64(pairingTTL.Seconds()),
	}
	if err := c.store.issueDevice(dev); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, map[string]any{
		"device_id":      dev.ID,
		"one_shot_token": oneShot, // shown once (becomes the QR)
		"expires_at":     dev.ExpiresAt,
	})
}

// Redeem exchanges a one-shot pairing token for a device id + a long-term
// device token (the credential the phone stores). The one-shot itself is the
// auth, so this is safe to expose unauthenticated. Returns ok=false for an
// invalid/expired token. Backs the /api/pairing/redeem huma op (api package).
func (c *ChatAPI) Redeem(oneShotToken, clientIP string) (deviceID, longTermToken string, ok bool) {
	if oneShotToken == "" {
		return "", "", false
	}
	longTerm := randHex(32)
	id, ok := c.store.redeemDevice(hashToken(oneShotToken), hashToken(longTerm), nowUnix())
	if !ok {
		return "", "", false
	}
	c.store.touchDevice(id, clientIP)
	return id, longTerm, true
}

// TokenValid reports whether a bearer token is accepted: the shared (desktop)
// token OR a registered device's long-term token. Lets the api-layer auth admit
// paired phones to the /api/v2/* surface with the token they got from Redeem.
func (c *ChatAPI) TokenValid(token string) bool {
	_, _, ok := c.authDevice(token)
	return ok
}

// GET /api/pairing/devices (admin) → { devices: [...] }
func (c *ChatAPI) handlePairDevices(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "GET only", http.StatusMethodNotAllowed)
		return
	}
	// A box paired only by QR never reaches the browser-pairing routes that
	// prune, so drop dead one-shots here too, before listing.
	c.store.pruneRequests(nowUnix())
	devs, err := c.store.listDevices()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	out := make([]map[string]any, 0, len(devs))
	for _, d := range devs {
		out = append(out, map[string]any{
			"device_id":   d.ID,
			"device_name": d.Name,
			"status":      d.Status,
			"scopes":      d.Scopes,
			"created_at":  d.CreatedAt,
			"expires_at":  d.ExpiresAt,
			"last_seen":   d.LastSeen,
			"last_ip":     d.LastIP,
		})
	}
	writeJSON(w, map[string]any{"devices": out})
}

// DELETE /api/pairing/devices/{id}            (revoke)
// PUT    /api/pairing/devices/{id}/name       (rename, body {name})
func (c *ChatAPI) handlePairDeviceItem(w http.ResponseWriter, r *http.Request) {
	rest := strings.Trim(strings.TrimPrefix(r.URL.Path, "/api/pairing/devices/"), "/")
	parts := strings.Split(rest, "/")
	id := parts[0]
	if id == "" {
		http.Error(w, "missing device id", http.StatusBadRequest)
		return
	}
	switch {
	case r.Method == http.MethodDelete && len(parts) == 1:
		c.store.revokeDevice(id)
		writeJSON(w, map[string]any{"ok": true})
	case r.Method == http.MethodPut && len(parts) == 2 && parts[1] == "name":
		var body struct {
			Name string `json:"name"`
		}
		if r.Body != nil {
			_ = json.NewDecoder(r.Body).Decode(&body)
		}
		c.store.renameDevice(id, body.Name)
		writeJSON(w, map[string]any{"ok": true})
	default:
		http.Error(w, "DELETE {id} or PUT {id}/name", http.StatusMethodNotAllowed)
	}
}
