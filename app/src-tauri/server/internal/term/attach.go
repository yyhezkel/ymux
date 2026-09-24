package term

// attach.go — GET /api/v2/term/sessions/{name}/attach, the binary WebSocket
// that carries a real terminal.
//
// Why a SEPARATE socket from the workspace events WS rather than another frame
// type on it: PTY traffic is bytes, and base64-in-JSON would inflate every
// keystroke and every screen repaint by a third while pushing the hottest path
// in the product through a codec built for chat events. One socket per pane
// also makes backpressure per-pane for free — a client that stops reading
// stalls its own terminal and nothing else.
//
// The wire is deliberately tiny:
//
//	client → server   binary  = keystrokes, written straight to the PTY
//	client → server   text    = {"type":"resize","cols":N,"rows":N}
//	server → client   binary  = PTY output, unmodified
//	server → client   text    = {"type":"exit"} once, when the PTY ends
//
// Output is passed through BYTE FOR BYTE. No UTF-8 reassembly happens here:
// a multi-byte rune split across two reads is reassembled by the client (the
// desktop does the same job in pty_decode.rs), and doing it twice would be the
// bug, not the fix.
//
// Rule #1: not one byte of PTY traffic is logged. The close line carries the
// session name and two counts.

import (
	"encoding/json"
	"net/http"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
)

var upgrader = websocket.Upgrader{
	ReadBufferSize:  4096,
	WriteBufferSize: 32 * 1024,
	// Origin is not the boundary here — the bearer token is (see Service.gate).
	// The page that legitimately opens this socket is served from the same
	// daemon, but a desktop client dialing through the SSH tunnel has no Origin
	// header at all, and rejecting that would break the case this exists for.
	CheckOrigin: func(*http.Request) bool { return true },
}

const (
	// ptyReadChunk bounds one read off the master fd. A full-screen repaint of
	// a large terminal is a few KB; 32K means a `cat` of a big file becomes a
	// handful of frames rather than hundreds.
	ptyReadChunk = 32 * 1024
	// wsReadLimit caps an inbound frame. Keystrokes are bytes and a paste is
	// kilobytes; anything past this is not a terminal client.
	wsReadLimit = 1 << 20
	// pingEvery keeps idle sockets alive through nginx (default proxy read
	// timeout is 60s) and is how a dead peer is noticed: the write fails.
	pingEvery = 30 * time.Second
	// writeWait bounds a single frame write to a stalled client.
	writeWait = 10 * time.Second
)

// controlFrame is the client→server text frame.
type controlFrame struct {
	Type string `json:"type"`
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
}

// childEnv is the environment for the `tmux attach` child: the daemon's own
// (which config.AugmentUserPath has already fixed up) with TERM forced.
// systemd hands the daemon a minimal environment where TERM is usually absent,
// and tmux with no TERM falls back to a dumb terminal that renders nothing.
func childEnv() []string {
	env := make([]string, 0, len(os.Environ())+1)
	for _, kv := range os.Environ() {
		if strings.HasPrefix(kv, "TERM=") {
			continue
		}
		env = append(env, kv)
	}
	return append(env, "TERM=xterm-256color")
}

// handleAttach upgrades the request and bridges the socket to a PTY running
// `tmux attach` against the named session.
func (s *Service) handleAttach(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	if !ValidName(name) {
		logger.Warn("attach refused: invalid session name", "ip", clientIP(r))
		http.Error(w, "invalid session name", http.StatusBadRequest)
		return
	}
	// Checked BEFORE the upgrade so a missing session is an honest 404 rather
	// than a socket that opens and immediately dies.
	if !s.tmux.Has(name) {
		logger.Warn("attach refused: no such session", "session", name)
		http.Error(w, "no such session", http.StatusNotFound)
		return
	}
	cols, rows := querySize(r)
	logger.Debug("attach upgrading", "session", name, "cols", cols, "rows", rows, "ip", clientIP(r))

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		// Upgrade has already written its own error, but it is invisible
		// otherwise — and a failed upgrade looks identical to a client that
		// never dialled, which is the wrong thing to be guessing about.
		logger.Warn("websocket upgrade failed", "session", name, "err", err)
		return
	}
	defer conn.Close()
	conn.SetReadLimit(wsReadLimit)

	pty, err := StartPTY(s.tmux.bin, AttachArgs(name), childEnv(), cols, rows)
	if err != nil {
		logger.Error("pty start failed", "session", name, "err", err)
		_ = conn.WriteControl(websocket.CloseMessage,
			websocket.FormatCloseMessage(websocket.CloseInternalServerErr, "pty start failed"),
			time.Now().Add(writeWait))
		return
	}
	logger.Info("terminal attached", "session", name, "cols", cols, "rows", rows)

	// Atomic because the two counters are written from different goroutines
	// (the socket reader counts in, the writer loop counts out) and read by the
	// deferred log line on a third path.
	var inBytes, outBytes atomic.Int64
	started := time.Now()
	defer func() {
		_ = pty.Close()
		// Rule #1: counts and duration, never content.
		logger.Info("terminal detached", "session", name,
			"in_bytes", inBytes.Load(), "out_bytes", outBytes.Load(),
			"seconds", int64(time.Since(started).Seconds()))
	}()

	done := make(chan struct{})
	var once sync.Once
	closeOnce := func() { once.Do(func() { close(done) }) }

	// PTY → chan. Owns no socket, so it can block on a read for hours.
	out := make(chan []byte, 64)
	go func() {
		defer close(out)
		buf := make([]byte, ptyReadChunk)
		for {
			n, err := pty.Read(buf)
			if n > 0 {
				b := make([]byte, n)
				copy(b, buf[:n])
				select {
				case out <- b:
				case <-done:
					return
				}
			}
			if err != nil {
				return // EIO when the child exits; any error ends the pump
			}
		}
	}()

	// Socket → PTY. This goroutine NEVER writes to the socket: the loop below
	// is the single writer, which is what gorilla requires.
	go func() {
		defer closeOnce()
		for {
			typ, data, err := conn.ReadMessage()
			if err != nil {
				return
			}
			switch typ {
			case websocket.BinaryMessage:
				if _, err := pty.Write(data); err != nil {
					return
				}
				inBytes.Add(int64(len(data)))
			case websocket.TextMessage:
				var f controlFrame
				if json.Unmarshal(data, &f) != nil {
					continue // a frame we don't understand is not fatal
				}
				if f.Type == "resize" {
					_ = pty.Resize(f.Cols, f.Rows)
				}
			}
		}
	}()

	ping := time.NewTicker(pingEvery)
	defer ping.Stop()
	for {
		select {
		case b, ok := <-out:
			if !ok {
				// The PTY ended: the session was killed, or another client
				// detached this one. Tell the UI before the socket closes so
				// it can say so instead of showing a frozen screen.
				_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
				_ = conn.WriteMessage(websocket.TextMessage, []byte(`{"type":"exit"}`))
				return
			}
			_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
			if conn.WriteMessage(websocket.BinaryMessage, b) != nil {
				return
			}
			outBytes.Add(int64(len(b)))
		case <-ping.C:
			_ = conn.SetWriteDeadline(time.Now().Add(writeWait))
			if conn.WriteMessage(websocket.PingMessage, nil) != nil {
				return
			}
		case <-done:
			return
		}
	}
}

// querySize reads the initial terminal size from the query string. Absent or
// unparseable falls through to StartPTY's 80x24.
func querySize(r *http.Request) (cols, rows uint16) {
	return parseDim(r.URL.Query().Get("cols")), parseDim(r.URL.Query().Get("rows"))
}

// parseDim parses one dimension, clamping to what a terminal can actually be.
// The upper bound is not paranoia: cols/rows land in a uint16 ioctl field, and
// a client asking for 100000 columns would otherwise wrap to something small
// and absurd rather than being rejected.
func parseDim(s string) uint16 {
	var n int
	for _, c := range s {
		if c < '0' || c > '9' {
			return 0
		}
		n = n*10 + int(c-'0')
		if n > 9999 {
			return 0
		}
	}
	return uint16(n)
}
