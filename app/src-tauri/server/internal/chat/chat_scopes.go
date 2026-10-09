package chat

// chat_scopes.go — Phase 77 §Q6. Exported access to per-device scope grants for
// the API layer: enforcement (resolve a bearer → its scopes) and the owner's
// GET/PUT scopes endpoints. The scope vocabulary + parsing live in internal/auth.

// DeviceScopes resolves a bearer token to its stored scopes. The shared/admin
// token returns admin=true (and bypasses scope checks); a paired device returns
// its stored scopes string. ok=false for an unknown/empty token.
func (c *ChatAPI) DeviceScopes(token string) (scopes string, admin, ok bool) {
	if token == "" {
		return "", false, false
	}
	if token == c.sharedToken {
		return "", true, true
	}
	if d, found := c.store.deviceByTokenHash(hashToken(token)); found {
		return d.Scopes, false, true
	}
	return "", false, false
}

// GetDeviceScopes returns a device's stored scopes string (empty ⇒ "all").
func (c *ChatAPI) GetDeviceScopes(id string) (string, bool) {
	if d, ok := c.store.deviceByID(id); ok {
		return d.Scopes, true
	}
	return "", false
}

// ActiveDeviceScopes is GetDeviceScopes for a device that is still active:
// ok=false once it is revoked. Web Push (term/webpush.go) asks it before
// every send, so a revoke stops a browser's notifications.
func (c *ChatAPI) ActiveDeviceScopes(id string) (string, bool) {
	if d, ok := c.store.deviceByID(id); ok && d.Status == "active" {
		return d.Scopes, true
	}
	return "", false
}

// SetDeviceScopes stores a device's grants (caller passes the canonical form
// from auth.NormalizeScopes). Returns false if the device isn't active.
func (c *ChatAPI) SetDeviceScopes(id, scopes string) bool {
	return c.store.setDeviceScopes(id, scopes)
}
