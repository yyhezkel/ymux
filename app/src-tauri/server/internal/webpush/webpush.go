// Package webpush sends Web Push messages (RFC 8030) to a browser's push
// service: the payload encrypted for the subscriber (RFC 8291, aes128gcm),
// the request signed with this daemon's VAPID key (RFC 8292). Stdlib only.
//
// Phase 114 (WEB-DESIGN E, Yossi 2026-10-06: "E1+E2 together"). This is the
// one place the daemon talks to a third party: the browser chooses the push
// service (FCM for Chrome, Mozilla's autopush for Firefox, Apple's for
// Safari), and the daemon POSTs to the endpoint it was handed. The service
// sees the endpoint, the size and the timing — never the content, which only
// the subscribing browser can decrypt. internal/push (the phone's own
// WebSocket) stays the self-hosted channel; this one exists because a
// closed browser tab cannot hold a socket.
package webpush

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ecdh"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/hkdf"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/asn1"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Subscription is the browser's PushSubscription.toJSON(), flattened.
type Subscription struct {
	Endpoint string `json:"endpoint"`
	P256dh   string `json:"p256dh"` // base64url, the UA's uncompressed P-256 point
	Auth     string `json:"auth"`   // base64url, 16 bytes
}

// MaxPayload is what fits one aes128gcm record of the 4096-byte size every
// push service accepts: 4096 − 86 header − 16 tag − 1 delimiter.
const MaxPayload = 3993

// recordSize is the rs field. One record carries the whole message.
const recordSize = 4096

// Contact is the VAPID `sub` claim. Push services want a way to reach the
// sender; the project page is the honest one for a self-hosted daemon.
const Contact = "https://github.com/yyhezkel/winmux"

// Keys is the daemon's VAPID key pair.
type Keys struct {
	priv *ecdsa.PrivateKey
	pub  []byte // uncompressed point, 65 bytes
}

// LoadOrCreateKeys reads the PEM key at path, or makes one and writes it
// (0600, tmp + rename). The public half is what a browser subscribes with,
// so it must survive restarts: a new key orphans every subscription.
func LoadOrCreateKeys(path string) (*Keys, error) {
	if b, err := os.ReadFile(path); err == nil {
		blk, _ := pem.Decode(b)
		if blk == nil {
			return nil, fmt.Errorf("%s: not PEM", path)
		}
		priv, err := x509.ParseECPrivateKey(blk.Bytes)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", path, err)
		}
		return keysFrom(priv)
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, err
	}
	der, err := x509.MarshalECPrivateKey(priv)
	if err != nil {
		return nil, err
	}
	if err := writeAtomic(path, pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: der})); err != nil {
		return nil, err
	}
	return keysFrom(priv)
}

func keysFrom(priv *ecdsa.PrivateKey) (*Keys, error) {
	if priv.Curve != elliptic.P256() {
		return nil, errors.New("VAPID key is not P-256")
	}
	e, err := priv.ECDH()
	if err != nil {
		return nil, err
	}
	return &Keys{priv: priv, pub: e.PublicKey().Bytes()}, nil
}

// PublicKey is the applicationServerKey a browser subscribes with.
func (k *Keys) PublicKey() string { return b64(k.pub) }

// Validate checks a subscription before it is stored: an https endpoint and
// keys of the right shape. Anything else could never be delivered to.
func Validate(s Subscription) error {
	u, err := url.Parse(s.Endpoint)
	if err != nil || u.Scheme != "https" || u.Host == "" || len(s.Endpoint) > 2048 {
		return errors.New("endpoint must be an https URL")
	}
	if p, err := unb64(s.P256dh); err != nil || len(p) != 65 || p[0] != 4 {
		return errors.New("p256dh must be an uncompressed P-256 point")
	}
	if a, err := unb64(s.Auth); err != nil || len(a) != 16 {
		return errors.New("auth must be 16 bytes")
	}
	return nil
}

// Options are the per-message RFC 8030 headers.
type Options struct {
	TTL     time.Duration // how long the service may hold it for an offline device
	Urgency string        // very-low | low | normal | high
	Topic   string        // a newer message with the same topic replaces a queued one
}

// Send encrypts payload for sub and POSTs it. It returns the push service's
// status: 201 is delivered-to-the-service; 404/410 mean the subscription is
// gone and should be dropped (the caller decides).
func (k *Keys) Send(ctx context.Context, c *http.Client, sub Subscription, payload []byte, o Options) (int, error) {
	if len(payload) > MaxPayload {
		return 0, fmt.Errorf("payload %d bytes > %d", len(payload), MaxPayload)
	}
	asPriv, err := ecdh.P256().GenerateKey(rand.Reader)
	if err != nil {
		return 0, err
	}
	salt := make([]byte, 16)
	if _, err := rand.Read(salt); err != nil {
		return 0, err
	}
	body, err := encrypt(sub, payload, asPriv, salt)
	if err != nil {
		return 0, err
	}
	auth, err := k.authorization(sub.Endpoint, time.Now())
	if err != nil {
		return 0, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, sub.Endpoint, bytes.NewReader(body))
	if err != nil {
		return 0, err
	}
	ttl := int(o.TTL / time.Second)
	if ttl < 0 {
		ttl = 0
	}
	req.Header.Set("TTL", fmt.Sprint(ttl))
	if o.Urgency != "" {
		req.Header.Set("Urgency", o.Urgency)
	}
	if o.Topic != "" {
		req.Header.Set("Topic", o.Topic)
	}
	req.Header.Set("Content-Encoding", "aes128gcm")
	req.Header.Set("Content-Type", "application/octet-stream")
	req.Header.Set("Authorization", auth)
	resp, err := c.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, 4096))
	return resp.StatusCode, nil
}

// encrypt is RFC 8291 §3–4 with a single aes128gcm record (RFC 8188):
//
//	salt(16) | rs(4) | idlen(1)=65 | as_public(65) | AES-GCM(plaintext ‖ 0x02)
func encrypt(sub Subscription, plaintext []byte, asPriv *ecdh.PrivateKey, salt []byte) ([]byte, error) {
	uaPubRaw, err := unb64(sub.P256dh)
	if err != nil {
		return nil, fmt.Errorf("p256dh: %w", err)
	}
	authSecret, err := unb64(sub.Auth)
	if err != nil {
		return nil, fmt.Errorf("auth: %w", err)
	}
	uaPub, err := ecdh.P256().NewPublicKey(uaPubRaw)
	if err != nil {
		return nil, fmt.Errorf("p256dh: %w", err)
	}
	shared, err := asPriv.ECDH(uaPub)
	if err != nil {
		return nil, err
	}
	asPub := asPriv.PublicKey().Bytes()
	cek, nonce, err := deriveKeys(shared, authSecret, salt, uaPubRaw, asPub)
	if err != nil {
		return nil, err
	}
	block, err := aes.NewCipher(cek)
	if err != nil {
		return nil, err
	}
	gcm, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	rec := make([]byte, 0, len(plaintext)+1)
	rec = append(rec, plaintext...)
	rec = append(rec, 0x02) // last (and only) record, no padding

	out := make([]byte, 0, 16+4+1+len(asPub)+len(rec)+gcm.Overhead())
	out = append(out, salt...)
	out = binary.BigEndian.AppendUint32(out, recordSize)
	out = append(out, byte(len(asPub)))
	out = append(out, asPub...)
	return gcm.Seal(out, nonce, rec, nil), nil
}

// deriveKeys is the RFC 8291 §3.4 key schedule: the auth secret folds the
// ECDH secret into the IKM, then RFC 8188 derives the content key and nonce.
func deriveKeys(shared, authSecret, salt, uaPub, asPub []byte) (cek, nonce []byte, err error) {
	keyInfo := append(append([]byte("WebPush: info\x00"), uaPub...), asPub...)
	ikm, err := hkdf.Key(sha256.New, shared, authSecret, string(keyInfo), 32)
	if err != nil {
		return nil, nil, err
	}
	cek, err = hkdf.Key(sha256.New, ikm, salt, "Content-Encoding: aes128gcm\x00", 16)
	if err != nil {
		return nil, nil, err
	}
	nonce, err = hkdf.Key(sha256.New, ikm, salt, "Content-Encoding: nonce\x00", 12)
	if err != nil {
		return nil, nil, err
	}
	return cek, nonce, nil
}

// authorization is the RFC 8292 header: an ES256 JWT for the endpoint's
// origin, valid 12 hours (the services cap it at 24).
func (k *Keys) authorization(endpoint string, now time.Time) (string, error) {
	u, err := url.Parse(endpoint)
	if err != nil {
		return "", err
	}
	hdr := b64([]byte(`{"typ":"JWT","alg":"ES256"}`))
	claims, err := json.Marshal(map[string]any{
		"aud": u.Scheme + "://" + u.Host,
		"exp": now.Add(12 * time.Hour).Unix(),
		"sub": Contact,
	})
	if err != nil {
		return "", err
	}
	signing := hdr + "." + b64(claims)
	digest := sha256.Sum256([]byte(signing))
	der, err := ecdsa.SignASN1(rand.Reader, k.priv, digest[:])
	if err != nil {
		return "", err
	}
	var rs struct{ R, S *big.Int }
	if _, err := asn1.Unmarshal(der, &rs); err != nil {
		return "", err
	}
	sig := make([]byte, 64) // JWS wants raw r ‖ s, 32 bytes each
	rs.R.FillBytes(sig[:32])
	rs.S.FillBytes(sig[32:])
	return "vapid t=" + signing + "." + b64(sig) + ", k=" + k.PublicKey(), nil
}

func b64(b []byte) string { return base64.RawURLEncoding.EncodeToString(b) }

// unb64 takes base64url with or without padding (browsers differ), and
// tolerates the standard alphabet.
func unb64(s string) ([]byte, error) {
	s = strings.TrimRight(s, "=")
	s = strings.NewReplacer("+", "-", "/", "_").Replace(s)
	return base64.RawURLEncoding.DecodeString(s)
}

func writeAtomic(path string, b []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
