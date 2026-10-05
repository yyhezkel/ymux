package term

// webapp.go — the daemon serves the ymux web bundle (Phase 108, WEB-DESIGN
// C4 / §7.1 option (b), decided 2026-10-05).
//
// The bundle is the desktop's own vite build. It lives in
// <data dir>/www/<version>/ and <data dir>/www/current points at the one to
// serve (a symlink or a directory — the Phase D add-on writes it; until then
// it is copied by hand from the CI `ymux-web` artifact). The daemon only
// serves it: no upload path, no version logic here.
//
// When `current/index.html` exists, `/` is the app and the diagnostic page
// stays at `/diag`. When it does not, `/` keeps serving the diagnostic page,
// so a box without a bundle behaves exactly as before.
//
// Routes are explicit — `/{$}`, `/assets/…`, `/fonts/…` — never a `/{path...}`
// catch-all: the shared mux holds method-less `/api/...` patterns, and a
// catch-all GET pattern next to them is a registration-time conflict panic in
// Go 1.22+ routing. Vite emits everything hashed under /assets and the public
// dir only holds /fonts.
//
// Everything here is PUBLIC, like the diagnostic page: it is static code with
// no secrets, and the login screen inside it is how a browser gets a token.

import (
	"net/http"
	"os"
	"path"
	"path/filepath"
	"strings"
)

// webAppCSP: the bundle is self-contained (fonts and chunks from its own
// origin) and talks only to this daemon. Inline styles are Solid's `style=`
// bindings; data:/blob: images are canvases and pasted screenshots.
const webAppCSP = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; " +
	"connect-src 'self' ws: wss:; img-src 'self' data: blob:; font-src 'self' data:; " +
	"frame-ancestors 'none'; base-uri 'none'; form-action 'none'"

// SetWebRoot points the daemon at <dir>/www (called with the data dir).
func (s *Service) SetWebRoot(dataDir string) {
	s.webRoot = filepath.Join(dataDir, "www", "current")
}

// webIndex is the bundle's index.html, or "" when none is installed.
func (s *Service) webIndex() string {
	if s.webRoot == "" {
		return ""
	}
	p := filepath.Join(s.webRoot, "index.html")
	if st, err := os.Stat(p); err == nil && st.Mode().IsRegular() {
		return p
	}
	return ""
}

// handleRoot serves the app when a bundle is installed, the diagnostic page
// otherwise.
func (s *Service) handleRoot(w http.ResponseWriter, r *http.Request) {
	idx := s.webIndex()
	if idx == "" {
		s.handlePage(w, r)
		return
	}
	webLogger.Info("web app served", "ip", clientIP(r), "ua", clip(r.UserAgent(), 120))
	setWebAppHeaders(w)
	// The entry document is never cached: it names the hashed chunks, so a
	// stale copy after an update would load the previous build.
	w.Header().Set("Cache-Control", "no-cache")
	http.ServeFile(w, r, idx)
}

// handleWebAsset serves one file under /assets or /fonts from the bundle.
func (s *Service) handleWebAsset(w http.ResponseWriter, r *http.Request) {
	if s.webIndex() == "" {
		http.NotFound(w, r)
		return
	}
	rel := r.PathValue("file")
	// ServeMux has already cleaned the path; refuse anything that still
	// names a parent, a hidden file, or a backslash rather than reason about it.
	if rel == "" || strings.Contains(rel, "\\") || strings.Contains(rel, "..") ||
		strings.HasPrefix(path.Base(rel), ".") {
		http.NotFound(w, r)
		return
	}
	top := strings.SplitN(strings.TrimPrefix(r.URL.Path, "/"), "/", 2)[0] // "assets" | "fonts"
	full := filepath.Join(s.webRoot, top, filepath.FromSlash(rel))
	st, err := os.Stat(full)
	if err != nil || !st.Mode().IsRegular() {
		http.NotFound(w, r)
		return
	}
	setWebAppHeaders(w)
	if top == "assets" {
		// Vite content-hashes every name under /assets.
		w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
	} else {
		w.Header().Set("Cache-Control", "public, max-age=86400")
	}
	http.ServeFile(w, r, full)
}

func setWebAppHeaders(w http.ResponseWriter) {
	w.Header().Set("Content-Security-Policy", webAppCSP)
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.Header().Set("Referrer-Policy", "no-referrer")
}
