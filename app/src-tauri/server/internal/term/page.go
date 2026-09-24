package term

// page.go — Phase 97: the diagnostic web page and the browser's log sink.
//
// The page (page.html) is the smallest client that exercises the whole
// Phase 95 + 96 stack: request access, match the code, approve in ymux, list
// tmux sessions, attach a real terminal. It exists because "does the PTY
// actually work" was otherwise a question you answered by hand with websocat,
// and because both of those phases are still unverified live (Rule #14).
//
// It is EMBEDDED in the binary on purpose: a debugging tool with its own
// delivery mechanism is a debugging tool you cannot use when delivery is what
// broke. That is not a decision about how the real web bundle ships — Q2 in
// docs/DECISIONS.md stays open, and a few KB of diagnostics is a different
// question from a 3 MB application.
//
// The log sink is the other half of the point. Half the steps in this flow
// happen in a browser, so without it the daemon log shows a pairing request
// and then, minutes later, a WebSocket, with nothing in between. Rule #1: the
// page reports step names and error strings; terminal bytes never leave the
// terminal, and the sanitiser here assumes the page might be lying about that.

import (
	_ "embed"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	"ymux-server/internal/logging"
)

// webLogger tags browser-reported steps distinctly from the daemon's own
// terminal work, so `grep SRV:WEB` reads as the client's side of the story and
// `grep SRV:TERM` as the server's.
var webLogger = logging.New("SRV:WEB")

//go:embed page.html
var diagPage []byte

// Limits on the log sink. It cannot require a credential — a page that has not
// paired yet is exactly when its log lines matter most — so it is bounded
// instead: a small body, a short line, and a ceiling on how fast the daemon
// log can be filled.
const (
	diagLogMaxBody   = 2 << 10 // 2 KB
	diagLogMaxDetail = 300
	diagLogPerMinute = 120
)

// logBudget is a coarse refilling counter. Per-process rather than per-IP on
// purpose: the thing being protected is one log FILE, and an attacker rotating
// source addresses would defeat a per-IP limit while filling it just as fast.
type logBudget struct {
	mu     sync.Mutex
	left   int
	window time.Time
}

func (b *logBudget) allow(now time.Time) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	if now.Sub(b.window) >= time.Minute {
		b.window, b.left = now, diagLogPerMinute
	}
	if b.left <= 0 {
		return false
	}
	b.left--
	return true
}

// registerPageRoutes mounts the diagnostic page and its log sink.
//
// Both are PUBLIC — deliberately. The page is static HTML holding no secrets,
// and it is currently the only way to pair a browser at all, so gating it
// behind a credential the browser does not yet have would be a closed loop.
// Everything it then calls is gated normally.
func (s *Service) registerPageRoutes(mux *http.ServeMux) {
	// A Service built as a struct literal (the tests do) has no budget, and a
	// nil one would panic on the first posted line. Filling it in here means
	// every construction path is correct rather than only NewService's.
	if s.logBudget == nil {
		s.logBudget = &logBudget{}
	}
	// `/{$}` is an EXACT match for the root in Go 1.22+ patterns, not a
	// catch-all: an unknown path still 404s honestly instead of silently
	// returning the page.
	mux.HandleFunc("GET /{$}", s.handlePage)
	mux.HandleFunc("GET /diag", s.handlePage)
	mux.HandleFunc("POST /diag/log", s.handleDiagLog)
}

func (s *Service) handlePage(w http.ResponseWriter, r *http.Request) {
	webLogger.Info("diag page served", "ip", clientIP(r), "ua", clip(r.UserAgent(), 120))
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	// Never cached: this page's whole job is to reflect the daemon in front of
	// it, and a stale copy after an upgrade would be debugging the wrong build.
	w.Header().Set("Cache-Control", "no-store")
	// It loads xterm.js from cdnjs and talks only to its own origin.
	w.Header().Set("Content-Security-Policy",
		"default-src 'self'; script-src 'self' 'unsafe-inline' https://cdnjs.cloudflare.com; "+
			"style-src 'self' 'unsafe-inline' https://cdnjs.cloudflare.com; "+
			"connect-src 'self' ws: wss:; img-src 'self' data:")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	_, _ = w.Write(diagPage)
}

// diagLine is what the page posts.
type diagLine struct {
	Level  string `json:"level"`
	Step   string `json:"step"`
	Detail string `json:"detail"`
}

func (s *Service) handleDiagLog(w http.ResponseWriter, r *http.Request) {
	if !s.logBudget.allow(time.Now()) {
		// Silent 204: a page that spams should not also get an error loop to
		// report, and dropping the line is the whole point of the budget.
		w.WriteHeader(http.StatusNoContent)
		return
	}
	body, err := io.ReadAll(io.LimitReader(r.Body, diagLogMaxBody))
	if err != nil {
		w.WriteHeader(http.StatusNoContent)
		return
	}
	var line diagLine
	if json.Unmarshal(body, &line) != nil {
		w.WriteHeader(http.StatusNoContent)
		return
	}
	step := clip(line.Step, 60)
	if step == "" {
		w.WriteHeader(http.StatusNoContent)
		return
	}
	detail := clip(line.Detail, diagLogMaxDetail)
	ip := clientIP(r)

	// The level is chosen by an untrusted client, so it selects from a fixed
	// set rather than being passed through — otherwise a page could log at any
	// level it liked, including ones the file's threshold is tuned to keep.
	switch line.Level {
	case "error":
		webLogger.Error("browser", "step", step, "detail", detail, "ip", ip)
	case "warn":
		webLogger.Warn("browser", "step", step, "detail", detail, "ip", ip)
	default:
		webLogger.Info("browser", "step", step, "detail", detail, "ip", ip)
	}
	w.WriteHeader(http.StatusNoContent)
}

// clip bounds an untrusted string and strips control characters, which would
// otherwise let a caller forge extra lines in the log file.
func clip(s string, n int) string {
	s = strings.Map(func(r rune) rune {
		if r < 0x20 || r == 0x7f {
			return -1
		}
		return r
	}, strings.TrimSpace(s))
	if len(s) > n {
		return s[:n]
	}
	return s
}

// clientIP prefers nginx's forwarded headers — the daemon sits behind the
// proxy, so RemoteAddr is always 127.0.0.1. Display and rate limiting only;
// a header is client-controlled and must never authorise anything.
func clientIP(r *http.Request) string {
	if v := r.Header.Get("X-Real-IP"); v != "" {
		return clip(v, 64)
	}
	if v := r.Header.Get("X-Forwarded-For"); v != "" {
		return clip(strings.Split(v, ",")[0], 64)
	}
	return clip(r.RemoteAddr, 64)
}
