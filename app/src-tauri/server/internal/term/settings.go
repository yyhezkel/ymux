package term

// settings.go — the browser's settings (Phase 108, WEB-DESIGN C4).
//
// Decided 2026-10-05: browser-mode settings live on the daemon, shared by
// every browser, not in each browser's localStorage. The document is the
// desktop's `Settings` JSON, stored OPAQUE — the desktop type stays the only
// schema and a new setting needs no Go change. It lives in
// <data dir>/web-settings.json behind the same optimistic version guard as the
// web workspaces (webws.go): a PUT carrying a stale version gets 409 with the
// current document. The daemon reads exactly three fields, for its own hooks:
// brief.inject_context, hooks.custom_block and daemon.hibernate_idle_minutes
// (hookRules below).
//
// Every write announces `settings:changed` {version, settings} on the events
// socket; the WebBackend unwraps it into the desktop's `settings:changed`
// payload (the bare Settings object).

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"sync"
)

// settingsMaxBody bounds one PUT. The desktop's settings.json is a few KB;
// 256 KB leaves room for long shortcut / palette tables without letting a
// client park megabytes on the box.
const settingsMaxBody = 256 << 10

// WebSettings is the stored document. Settings is null until the first save.
type WebSettings struct {
	Version  int64           `json:"version"`
	Settings json.RawMessage `json:"settings"`
}

type settingsStore struct {
	mu    sync.Mutex
	path  string
	doc   WebSettings
	parts hookRules // derived from doc; parsed once per write
}

// hookRules is what the daemon's hook dispatch reads from the document.
type hookRules struct {
	injectContext bool
	customBlock   []string
	// hibernateMinutes is daemon.hibernate_idle_minutes (hibernate.go):
	// 0 = off, the default. Anything but a whole number in range reads as 0.
	hibernateMinutes int
}

// parseHookRules reads the three fields. It fails safe, field by field: encoding/json
// skips a value of the wrong type and fills the rest, so a damaged entry costs
// only itself (a non-string list item arrives as "", which matches nothing).
// The built-in block patterns are not in the document, so no content here can
// remove them. Only a JSON false turns injection off.
//
// hooks.policy_enabled is deliberately not read: on the desktop it swaps Block
// for an approval card, and under `none` the same switch would mean allow.
func parseHookRules(doc json.RawMessage) hookRules {
	var d struct {
		Brief struct {
			// Raw, not *bool: encoding/json allocates the pointer before it
			// finds a wrong type, and "no" would then read as false.
			InjectContext json.RawMessage `json:"inject_context"`
		} `json:"brief"`
		Hooks struct {
			CustomBlock []string `json:"custom_block"`
		} `json:"hooks"`
		Daemon struct {
			HibernateIdleMinutes json.RawMessage `json:"hibernate_idle_minutes"`
		} `json:"daemon"`
	}
	if err := json.Unmarshal(doc, &d); err != nil {
		// The error names the field and the JSON type, never a value.
		logger.Warn("settings document has a damaged field; using the default for it", "err", err)
	}
	return hookRules{
		injectContext:    string(d.Brief.InjectContext) != "false",
		customBlock:      d.Hooks.CustomBlock,
		hibernateMinutes: hibernateMinutes(d.Daemon.HibernateIdleMinutes),
	}
}

// hibernateMinutes reads the setting. It fails to OFF: a missing value, a
// wrong type, a fraction or a negative number is 0. A large value is clamped.
func hibernateMinutes(raw json.RawMessage) int {
	var n int64
	if len(raw) == 0 || json.Unmarshal(raw, &n) != nil || n <= 0 {
		return 0
	}
	return int(min(n, hibMaxMinutes))
}

func newSettingsStore(path string) *settingsStore {
	s := &settingsStore{path: path}
	if path != "" {
		var d WebSettings
		if loadJSON(path, &d, "web settings") {
			s.doc = d
		}
	}
	if len(s.doc.Settings) == 0 {
		s.doc.Settings = json.RawMessage("null")
	}
	s.parts = parseHookRules(s.doc.Settings)
	return s
}

// rules returns the parsed hook rules.
func (s *settingsStore) rules() hookRules {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.parts
}

func (s *settingsStore) get() WebSettings {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.doc
}

// put replaces the document when version matches the stored one; otherwise
// it returns errVersion with the current document.
func (s *settingsStore) put(version int64, settings json.RawMessage) (WebSettings, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if version != s.doc.Version {
		return s.doc, errVersion
	}
	next := WebSettings{Version: s.doc.Version + 1, Settings: append(json.RawMessage{}, settings...)}
	if s.path != "" {
		b, err := json.Marshal(next) // compact: the stored bytes are what clients get back
		if err != nil {
			return WebSettings{}, err
		}
		if err := writeFileAtomic(s.path, b); err != nil {
			return WebSettings{}, err
		}
	}
	s.doc = next
	s.parts = parseHookRules(next.Settings)
	return next, nil
}

// handleSettings serves GET / PUT /api/v2/settings (behind Service.gate).
func (s *Service) handleSettings(w http.ResponseWriter, r *http.Request) {
	if s.hooks == nil || s.hooks.settings == nil {
		http.Error(w, "settings unavailable", http.StatusServiceUnavailable)
		return
	}
	st := s.hooks.settings
	if r.Method == http.MethodGet {
		writeJSON(w, http.StatusOK, st.get())
		return
	}
	raw, err := io.ReadAll(io.LimitReader(r.Body, settingsMaxBody+1))
	if err != nil {
		http.Error(w, "read failed", http.StatusBadRequest)
		return
	}
	if len(raw) > settingsMaxBody {
		http.Error(w, "settings too large", http.StatusRequestEntityTooLarge)
		return
	}
	var body struct {
		Version  *int64          `json:"version"`
		Settings json.RawMessage `json:"settings"`
	}
	if err := json.Unmarshal(raw, &body); err != nil || body.Version == nil {
		http.Error(w, "version and settings required", http.StatusBadRequest)
		return
	}
	// The document must be a JSON object: it is the desktop's Settings, and
	// anything else would hand every browser a value its loader cannot read.
	if t := bytes.TrimSpace(body.Settings); len(t) == 0 || t[0] != '{' {
		http.Error(w, "settings must be a JSON object", http.StatusBadRequest)
		return
	}
	doc, err := st.put(*body.Version, body.Settings)
	if errors.Is(err, errVersion) {
		writeJSON(w, http.StatusConflict, doc)
		return
	}
	if err != nil {
		logger.Warn("settings save failed", "err", err)
		http.Error(w, "save failed", http.StatusInternalServerError)
		return
	}
	// Rule #1 spirit: the version and size, never the contents.
	logger.Info("settings saved", "version", doc.Version, "bytes", len(doc.Settings))
	s.hooks.hub.publish("settings:changed", same(doc))
	writeJSON(w, http.StatusOK, doc)
}
