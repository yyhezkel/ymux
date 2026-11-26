package chat

import (
	"errors"
	"fmt"
	"path/filepath"
	"testing"
)

func newTestChatAPI(t *testing.T) (*ChatAPI, *SessionManager) {
	t.Helper()
	store, err := OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("OpenChatStore: %v", err)
	}
	t.Cleanup(store.Close)
	mgr := NewSessionManager(store)
	return NewChatAPI(mgr, store, "shared-secret"), mgr
}

// issue a pending device directly in the store; returns the plaintext one-shot.
func issueTestDevice(t *testing.T, c *ChatAPI, expiresAt int64) (string, string) {
	t.Helper()
	oneShot := randHex(24)
	dev := &PairedDevice{
		ID: "dev_" + randHex(6), Name: "phone", OtsHash: hashToken(oneShot),
		Scopes: "all", CreatedAt: 1000, ExpiresAt: expiresAt,
	}
	if err := c.store.issueDevice(dev); err != nil {
		t.Fatalf("issueDevice: %v", err)
	}
	return dev.ID, oneShot
}

func TestPairingHappyPath(t *testing.T) {
	c, _ := newTestChatAPI(t)
	id, oneShot := issueTestDevice(t, c, 9_999_999_999)
	longTerm := randHex(32)
	gotID, ok := c.store.redeemDevice(hashToken(oneShot), hashToken(longTerm), 2000)
	if !ok || gotID != id {
		t.Fatalf("redeem failed: id=%q ok=%v", gotID, ok)
	}
	// long-term token now authenticates as this device (not admin).
	dev, admin, authOK := c.authDevice(longTerm)
	if !authOK || admin || dev != id {
		t.Fatalf("auth after redeem: dev=%q admin=%v ok=%v", dev, admin, authOK)
	}
}

func TestPairingExpiredOneShot(t *testing.T) {
	c, _ := newTestChatAPI(t)
	_, oneShot := issueTestDevice(t, c, 1500) // expires at 1500
	if _, ok := c.store.redeemDevice(hashToken(oneShot), hashToken(randHex(32)), 2000); ok {
		t.Fatal("expired one-shot should not redeem")
	}
}

func TestPairingSingleUse(t *testing.T) {
	c, _ := newTestChatAPI(t)
	_, oneShot := issueTestDevice(t, c, 9_999_999_999)
	if _, ok := c.store.redeemDevice(hashToken(oneShot), hashToken(randHex(32)), 2000); !ok {
		t.Fatal("first redeem should succeed")
	}
	if _, ok := c.store.redeemDevice(hashToken(oneShot), hashToken(randHex(32)), 2000); ok {
		t.Fatal("second redeem of the same one-shot must fail")
	}
}

func TestPairingRevokeStopsAuth(t *testing.T) {
	c, _ := newTestChatAPI(t)
	id, oneShot := issueTestDevice(t, c, 9_999_999_999)
	longTerm := randHex(32)
	c.store.redeemDevice(hashToken(oneShot), hashToken(longTerm), 2000)
	c.store.revokeDevice(id)
	if _, _, ok := c.authDevice(longTerm); ok {
		t.Fatal("revoked device must not authenticate")
	}
}

func TestPairingRename(t *testing.T) {
	c, _ := newTestChatAPI(t)
	id, _ := issueTestDevice(t, c, 9_999_999_999)
	c.store.renameDevice(id, "Yossi's iPhone")
	devs, _ := c.store.listDevices()
	if len(devs) != 1 || devs[0].Name != "Yossi's iPhone" {
		t.Fatalf("rename not applied: %+v", devs)
	}
}

func TestAuthRoles(t *testing.T) {
	c, _ := newTestChatAPI(t)
	id, oneShot := issueTestDevice(t, c, 9_999_999_999)
	longTerm := randHex(32)
	c.store.redeemDevice(hashToken(oneShot), hashToken(longTerm), 2000)

	if _, admin, ok := c.authDevice("shared-secret"); !ok || !admin {
		t.Fatal("shared token should be admin")
	}
	if dev, admin, ok := c.authDevice(longTerm); !ok || admin || dev != id {
		t.Fatalf("device token: dev=%q admin=%v ok=%v", dev, admin, ok)
	}
	if _, _, ok := c.authDevice("garbage"); ok {
		t.Fatal("garbage token must not authenticate")
	}
}

func TestPerDeviceRateLimit(t *testing.T) {
	c, mgr := newTestChatAPI(t)
	mgr.cfg.maxPerDevice = 2
	for i := 0; i < 2; i++ {
		_ = c.store.insertSession(&SessionRow{
			ID: fmt.Sprintf("s%d", i), DeviceID: "dev_3", Status: stActive,
		})
	}
	_, err := mgr.create(startSpec{DeviceID: "dev_3"})
	var ce *chatErr
	if !errors.As(err, &ce) || ce.kind != "rate" {
		t.Fatalf("want rate error, got %v", err)
	}
}

func TestGlobalSessionCap(t *testing.T) {
	_, mgr := newTestChatAPI(t)
	mgr.cfg.maxGlobal = 0
	if _, err := mgr.create(startSpec{}); err == nil {
		t.Fatal("global cap of 0 should reject create")
	}
}

func TestSessionScopingByDevice(t *testing.T) {
	c, _ := newTestChatAPI(t)
	_ = c.store.insertSession(&SessionRow{ID: "a", DeviceID: "dev_A", Status: stActive})
	_ = c.store.insertSession(&SessionRow{ID: "b", DeviceID: "dev_B", Status: stActive})
	rowsA, _ := c.store.listSessionsForDevice("dev_A")
	if len(rowsA) != 1 || rowsA[0].ID != "a" {
		t.Fatalf("device A should see only its session, got %+v", rowsA)
	}
	rowB, _ := c.store.getSession("b")
	if ownsSession("dev_A", rowB) {
		t.Fatal("dev_A must not own dev_B's session")
	}
	if !ownsSession("", rowB) {
		t.Fatal("admin must own any session")
	}
}

// An issued-but-never-redeemed one-shot is dead once it expires (redeem
// refuses it), so prune removes the row. A redeemed device keeps that same
// old expires_at and must survive, as must an unexpired pending row.
func TestPruneDropsExpiredPendingButNeverActive(t *testing.T) {
	c, _ := newTestChatAPI(t)
	expiredID, _ := issueTestDevice(t, c, 1500)
	liveID, _ := issueTestDevice(t, c, 9_999_999_999)
	activeID, oneShot := issueTestDevice(t, c, 1500)
	if _, ok := c.store.redeemDevice(hashToken(oneShot), hashToken(randHex(32)), 1400); !ok {
		t.Fatal("redeem before expiry failed")
	}

	c.store.pruneRequests(2000)

	devs, err := c.store.listDevices()
	if err != nil {
		t.Fatalf("listDevices: %v", err)
	}
	status := map[string]string{}
	for _, d := range devs {
		status[d.ID] = d.Status
	}
	if _, ok := status[expiredID]; ok {
		t.Errorf("an expired, never-redeemed pending row survived the prune")
	}
	if status[liveID] != "pending" {
		t.Errorf("an unexpired pending row = %q, want pending", status[liveID])
	}
	if status[activeID] != "active" {
		t.Errorf("a redeemed device with an old expires_at = %q, want active", status[activeID])
	}
}
