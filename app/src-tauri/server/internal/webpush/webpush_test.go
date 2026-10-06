package webpush

import (
	"context"
	"crypto/ecdh"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/sha256"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// RFC 8291 Appendix A, byte for byte (the vector was re-derived with Node's
// crypto before it was pinned here).
func TestEncryptMatchesRFC8291Vector(t *testing.T) {
	asRaw, _ := unb64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")
	asPriv, err := ecdh.P256().NewPrivateKey(asRaw)
	if err != nil {
		t.Fatal(err)
	}
	salt, _ := unb64("DGv6ra1nlYgDCS1FRnbzlw")
	sub := Subscription{
		P256dh: "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
		Auth:   "BTBZMqHH6r4Tts7J_aSIgg",
	}
	got, err := encrypt(sub, []byte("When I grow up, I want to be a watermelon"), asPriv, salt)
	if err != nil {
		t.Fatal(err)
	}
	want := "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
	if b64(got) != want {
		t.Fatalf("ciphertext differs from the RFC vector:\n got %s\nwant %s", b64(got), want)
	}
}

func TestKeysPersistAcrossLoads(t *testing.T) {
	p := filepath.Join(t.TempDir(), "vapid.pem")
	a, err := LoadOrCreateKeys(p)
	if err != nil {
		t.Fatal(err)
	}
	b, err := LoadOrCreateKeys(p)
	if err != nil {
		t.Fatal(err)
	}
	if a.PublicKey() != b.PublicKey() {
		t.Fatal("a reload made a new key — every subscription would be orphaned")
	}
	if raw, _ := unb64(a.PublicKey()); len(raw) != 65 || raw[0] != 4 {
		t.Fatalf("public key is not an uncompressed point: %d bytes", len(raw))
	}
}

func TestAuthorizationIsAValidES256JWT(t *testing.T) {
	k, err := LoadOrCreateKeys(filepath.Join(t.TempDir(), "k.pem"))
	if err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_800_000_000, 0)
	h, err := k.authorization("https://fcm.googleapis.com/fcm/send/abc", now)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(h, "vapid t=") || !strings.HasSuffix(h, ", k="+k.PublicKey()) {
		t.Fatalf("header shape: %q", h)
	}
	jwt := strings.TrimSuffix(strings.TrimPrefix(h, "vapid t="), ", k="+k.PublicKey())
	parts := strings.Split(jwt, ".")
	if len(parts) != 3 {
		t.Fatalf("jwt has %d parts", len(parts))
	}
	cb, _ := unb64(parts[1])
	var claims map[string]any
	if err := json.Unmarshal(cb, &claims); err != nil {
		t.Fatal(err)
	}
	if claims["aud"] != "https://fcm.googleapis.com" || claims["sub"] != Contact {
		t.Fatalf("claims: %v", claims)
	}
	if exp, _ := claims["exp"].(float64); int64(exp) != now.Add(12*time.Hour).Unix() {
		t.Fatalf("exp: %v", claims["exp"])
	}
	sig, _ := unb64(parts[2])
	if len(sig) != 64 {
		t.Fatalf("signature is %d bytes, want raw r||s (64)", len(sig))
	}
	pub, _ := unb64(k.PublicKey())
	x, y := elliptic.Unmarshal(elliptic.P256(), pub) //nolint:staticcheck // test-only point decode
	digest := sha256.Sum256([]byte(parts[0] + "." + parts[1]))
	r, s := new(big.Int).SetBytes(sig[:32]), new(big.Int).SetBytes(sig[32:])
	if !ecdsa.Verify(&ecdsa.PublicKey{Curve: elliptic.P256(), X: x, Y: y}, digest[:], r, s) {
		t.Fatal("signature does not verify")
	}
}

func TestValidate(t *testing.T) {
	good := Subscription{
		Endpoint: "https://push.example/abc",
		P256dh:   "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
		Auth:     "BTBZMqHH6r4Tts7J_aSIgg==",
	}
	if err := Validate(good); err != nil {
		t.Fatalf("good subscription refused: %v", err)
	}
	for name, mut := range map[string]func(*Subscription){
		"http endpoint": func(s *Subscription) { s.Endpoint = "http://push.example/abc" },
		"no host":       func(s *Subscription) { s.Endpoint = "https:///abc" },
		"short key":     func(s *Subscription) { s.P256dh = "BCVx" },
		"short auth":    func(s *Subscription) { s.Auth = "AAAA" },
	} {
		s := good
		mut(&s)
		if Validate(s) == nil {
			t.Errorf("%s: accepted", name)
		}
	}
}

func TestSendHeadersAndStatus(t *testing.T) {
	var got http.Header
	var n int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got = r.Header.Clone()
		n = r.ContentLength
		w.WriteHeader(http.StatusGone)
	}))
	defer srv.Close()
	k, err := LoadOrCreateKeys(filepath.Join(t.TempDir(), "k.pem"))
	if err != nil {
		t.Fatal(err)
	}
	sub := Subscription{
		Endpoint: srv.URL + "/sub",
		P256dh:   "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
		Auth:     "BTBZMqHH6r4Tts7J_aSIgg",
	}
	code, err := k.Send(context.Background(), srv.Client(), sub, []byte(`{"t":1}`),
		Options{TTL: time.Minute, Urgency: "high", Topic: "req1"})
	if err != nil {
		t.Fatal(err)
	}
	if code != http.StatusGone {
		t.Fatalf("status %d", code)
	}
	for h, want := range map[string]string{
		"Ttl": "60", "Urgency": "high", "Topic": "req1",
		"Content-Encoding": "aes128gcm", "Content-Type": "application/octet-stream",
	} {
		if got.Get(h) != want {
			t.Errorf("%s = %q, want %q", h, got.Get(h), want)
		}
	}
	if !strings.HasPrefix(got.Get("Authorization"), "vapid t=") {
		t.Errorf("Authorization = %q", got.Get("Authorization"))
	}
	// 86-byte header + 7-byte payload + delimiter + 16-byte tag.
	if n != 86+7+1+16 {
		t.Errorf("body is %d bytes", n)
	}
	if _, err := k.Send(context.Background(), srv.Client(), sub, make([]byte, MaxPayload+1), Options{}); err == nil {
		t.Error("an oversize payload was sent")
	}
}
