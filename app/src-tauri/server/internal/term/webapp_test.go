package term

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// installBundle writes a fake vite build into <dir>/www/current.
func installBundle(t *testing.T, dir string) {
	t.Helper()
	cur := filepath.Join(dir, "www", "current")
	for name, body := range map[string]string{
		"index.html":           "<!doctype html><title>ymux app</title>",
		"assets/index-abc1.js": "console.log(1)",
		"fonts/x.woff2":        "font",
		"assets/.hidden":       "secret",
	} {
		p := filepath.Join(cur, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func TestRootServesDiagPageWithoutABundle(t *testing.T) {
	// A box with no bundle installed behaves exactly as before Phase 108.
	s, _ := testService(ok(""))
	s.SetWebRoot(t.TempDir())
	w := do(s, "GET", "/", "", "")
	if w.Code != http.StatusOK || w.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("GET / = %d cache=%q, want the diagnostic page", w.Code, w.Header().Get("Cache-Control"))
	}
	if w := do(s, "GET", "/assets/index-abc1.js", "", ""); w.Code != http.StatusNotFound {
		t.Errorf("asset without a bundle = %d, want 404", w.Code)
	}
}

func TestRootServesTheAppWhenInstalled(t *testing.T) {
	dir := t.TempDir()
	installBundle(t, dir)
	s, _ := testService(ok(""))
	s.SetWebRoot(dir)

	w := do(s, "GET", "/", "", "")
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), "ymux app") {
		t.Fatalf("GET / = %d %q, want the app", w.Code, w.Body.String())
	}
	if got := w.Header().Get("Cache-Control"); got != "no-cache" {
		t.Errorf("index cache = %q, want no-cache (it names the hashed chunks)", got)
	}
	if !strings.Contains(w.Header().Get("Content-Security-Policy"), "frame-ancestors 'none'") {
		t.Errorf("app served without its CSP")
	}
	// The diagnostic page stays reachable.
	if w := do(s, "GET", "/diag", "", ""); w.Code != http.StatusOK || !strings.Contains(w.Body.String(), "ymux") {
		t.Errorf("GET /diag = %d", w.Code)
	}

	w = do(s, "GET", "/assets/index-abc1.js", "", "")
	if w.Code != http.StatusOK || !strings.Contains(w.Header().Get("Cache-Control"), "immutable") {
		t.Errorf("asset = %d cache=%q, want 200 immutable", w.Code, w.Header().Get("Cache-Control"))
	}
	if w := do(s, "GET", "/fonts/x.woff2", "", ""); w.Code != http.StatusOK {
		t.Errorf("font = %d, want 200", w.Code)
	}
	for _, p := range []string{"/assets/.hidden", "/assets/missing.js", "/assets/%2e%2e/index.html", "/assets/"} {
		if w := do(s, "GET", p, "", ""); w.Code != http.StatusNotFound {
			t.Errorf("GET %s = %d, want 404", p, w.Code)
		}
	}
	// Still not a catch-all.
	if w := do(s, "GET", "/nope", "", ""); w.Code != http.StatusNotFound {
		t.Errorf("GET /nope = %d, want 404", w.Code)
	}
}

func TestSettingsStoreVersionGuardAndPersistence(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "web-settings.json")
	st := newSettingsStore(path)
	if d := st.get(); d.Version != 0 || string(d.Settings) != "null" {
		t.Fatalf("fresh store = %+v, want version 0 and null", d)
	}
	d, err := st.put(0, json.RawMessage(`{"theme":"dark","future_field":[1,2]}`))
	if err != nil || d.Version != 1 {
		t.Fatalf("first put = %+v %v", d, err)
	}
	if cur, err := st.put(0, json.RawMessage(`{"theme":"light"}`)); err != errVersion || cur.Version != 1 {
		t.Fatalf("stale put = %+v %v, want errVersion with the current doc", cur, err)
	}
	// Reload: unknown fields survive byte-for-byte (the daemon never parses them).
	again := newSettingsStore(path).get()
	if again.Version != 1 || string(again.Settings) != `{"theme":"dark","future_field":[1,2]}` {
		t.Fatalf("reloaded = %+v", again)
	}
}

func TestSettingsRoutes(t *testing.T) {
	s, _ := hookService("")
	s.SetDataDir(t.TempDir())
	if w := do(s, "GET", "/api/v2/settings", "owner-token", ""); w.Code != http.StatusOK ||
		!strings.Contains(w.Body.String(), `"settings":null`) {
		t.Fatalf("GET = %d %s", w.Code, w.Body.String())
	}
	for _, bad := range []string{`{"settings":{}}`, `{"version":0,"settings":[1]}`, `{"version":0}`, `junk`} {
		if w := do(s, "PUT", "/api/v2/settings", "owner-token", bad); w.Code != http.StatusBadRequest {
			t.Errorf("PUT %s = %d, want 400", bad, w.Code)
		}
	}
	w := do(s, "PUT", "/api/v2/settings", "owner-token", `{"version":0,"settings":{"theme":"dark"}}`)
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), `"version":1`) {
		t.Fatalf("PUT = %d %s", w.Code, w.Body.String())
	}
	w = do(s, "PUT", "/api/v2/settings", "owner-token", `{"version":0,"settings":{"theme":"light"}}`)
	if w.Code != http.StatusConflict || !strings.Contains(w.Body.String(), `"theme":"dark"`) {
		t.Fatalf("stale PUT = %d %s, want 409 + current", w.Code, w.Body.String())
	}
	big := `{"version":1,"settings":{"x":"` + strings.Repeat("a", settingsMaxBody) + `"}}`
	if w := do(s, "PUT", "/api/v2/settings", "owner-token", big); w.Code != http.StatusRequestEntityTooLarge {
		t.Errorf("oversized PUT = %d, want 413", w.Code)
	}
}

// Phase 114 (E): the PWA files, each on its own named route.
func TestPWAFilesAreServed(t *testing.T) {
	dir := t.TempDir()
	installBundle(t, dir)
	cur := filepath.Join(dir, "www", "current")
	_ = os.MkdirAll(filepath.Join(cur, "icons"), 0o755)
	for name, body := range map[string]string{
		"sw.js":                "self.addEventListener('push', () => {})",
		"manifest.webmanifest": `{"name":"YMUX"}`,
		"icons/icon-192.png":   "png",
	} {
		if err := os.WriteFile(filepath.Join(cur, filepath.FromSlash(name)), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	s, _ := testService(ok(""))
	s.SetWebRoot(dir)

	w := do(s, "GET", "/sw.js", "", "")
	if w.Code != http.StatusOK || w.Header().Get("Cache-Control") != "no-cache" ||
		!strings.Contains(w.Header().Get("Content-Type"), "javascript") {
		t.Errorf("sw.js = %d cache=%q type=%q", w.Code, w.Header().Get("Cache-Control"), w.Header().Get("Content-Type"))
	}
	if !strings.Contains(w.Header().Get("Content-Security-Policy"), "worker-src 'self'") {
		t.Errorf("CSP lacks worker-src: %q", w.Header().Get("Content-Security-Policy"))
	}
	w = do(s, "GET", "/manifest.webmanifest", "", "")
	if w.Code != http.StatusOK || w.Header().Get("Content-Type") != "application/manifest+json" {
		t.Errorf("manifest = %d type=%q", w.Code, w.Header().Get("Content-Type"))
	}
	if w := do(s, "GET", "/icons/icon-192.png", "", ""); w.Code != http.StatusOK {
		t.Errorf("icon = %d", w.Code)
	}
	// Without a bundle they do not exist.
	s2, _ := testService(ok(""))
	s2.SetWebRoot(t.TempDir())
	if w := do(s2, "GET", "/sw.js", "", ""); w.Code != http.StatusNotFound {
		t.Errorf("sw.js without a bundle = %d, want 404", w.Code)
	}
}
