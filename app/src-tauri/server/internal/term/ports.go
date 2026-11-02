package term

// ports.go — listening-port detection on the box (Phase 102, WEB-DESIGN B4).
//
// On the desktop a `ymux port-watch` that the DESKTOP starts per host reads
// /proc/net/tcp{,6} once a second and sends port.opened / port.closed back
// through the tunnel. A browser has nobody to start that process, so the
// daemon watches by itself (Yossi, 2026-10-05) — the same parse and the same
// filters as cli/src/port_watch.rs, ported with its test vectors. Detection
// only: browser v1 forwards nothing (WEB-DESIGN §10).
//
// The result rides the events socket as `port-detected` {addr, remote_port,
// family} and `port-undetected` {remote_port} — the desktop's names, minus
// workspace_id, which has no meaning on the box — and `hello` carries the
// current set. A port.opened / port.closed RPC from a hand-started watcher
// lands in the same set, so it is idempotent with this loop.

import (
	"context"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

// ListenPort is one forwardable listening socket.
type ListenPort struct {
	Addr       string `json:"addr"`
	RemotePort uint16 `json:"remote_port"`
	Family     string `json:"family"` // "v4" | "v6"
}

func (p ListenPort) key() string { return p.Family + "/" + p.Addr + "/" + strconv.Itoa(int(p.RemotePort)) }

// classifyV4 decodes a /proc/net/tcp local-address hex (8 chars, the u32 in
// little-endian memory order). ok is false for anything that is neither
// loopback nor bind-any — LAN-IP binds are not what dev servers use.
func classifyV4(hex string) (addr string, ok bool) {
	if len(hex) != 8 {
		return "", false
	}
	var o [4]uint64
	for i := range o {
		v, err := strconv.ParseUint(hex[i*2:i*2+2], 16, 8)
		if err != nil {
			return "", false
		}
		o[i] = v
	}
	switch {
	case o[0] == 0 && o[1] == 0 && o[2] == 0 && o[3] == 0:
		return "0.0.0.0", true
	case o[3] == 127:
		return "127." + strconv.FormatUint(o[2], 10) + "." + strconv.FormatUint(o[1], 10) + "." + strconv.FormatUint(o[0], 10), true
	}
	return "", false
}

// classifyV6 decodes a /proc/net/tcp6 local-address hex (32 chars).
func classifyV6(hex string) (addr string, ok bool) {
	if len(hex) != 32 {
		return "", false
	}
	up := strings.ToUpper(hex)
	switch {
	case strings.Trim(up, "0") == "":
		return "::", true
	case up == "00000000000000000000000001000000": // ::1, last word little-endian
		return "::1", true
	}
	return "", false
}

// parseProcNetTCP returns the LISTEN-state loopback / bind-any sockets of
// one /proc/net/tcp or tcp6 body.
func parseProcNetTCP(content string, v6 bool) []ListenPort {
	var out []ListenPort
	lines := strings.Split(content, "\n")
	for _, line := range lines[min(1, len(lines)):] {
		cols := strings.Fields(line)
		if len(cols) < 4 || !strings.EqualFold(cols[3], "0A") { // 0A = LISTEN
			continue
		}
		ipHex, portHex, found := strings.Cut(cols[1], ":")
		if !found {
			continue
		}
		port, err := strconv.ParseUint(portHex, 16, 16)
		if err != nil {
			continue
		}
		var addr string
		var ok bool
		family := "v4"
		if v6 {
			addr, ok = classifyV6(ipHex)
			family = "v6"
		} else {
			addr, ok = classifyV4(ipHex)
		}
		if ok {
			out = append(out, ListenPort{Addr: addr, RemotePort: uint16(port), Family: family})
		}
	}
	return out
}

// shouldReport is the CLI's should_forward: no privileged ports, no SSH, no
// excluded port.
func shouldReport(port uint16, exclude map[uint16]bool) bool {
	return port >= 1024 && port != 22 && !exclude[port]
}

// excludeEnv parses YMUX_PORTFORWARD_EXCLUDE ("8000,9000"), as the CLI does.
func excludeEnv() map[uint16]bool {
	out := map[uint16]bool{}
	for _, p := range strings.Split(os.Getenv("YMUX_PORTFORWARD_EXCLUDE"), ",") {
		if n, err := strconv.ParseUint(strings.TrimSpace(p), 10, 16); err == nil {
			out[uint16(n)] = true
		}
	}
	return out
}

// portWatch holds the detected set and announces changes on the hub.
type portWatch struct {
	mu      sync.Mutex
	current map[string]ListenPort
	hub     *eventHub
	// ownPorts returns the daemon's own ports (API + hook listener) — never
	// reported, like the CLI never reports the tunnel port.
	ownPorts func() []uint16
}

func newPortWatch(hub *eventHub, ownPorts func() []uint16) *portWatch {
	return &portWatch{current: map[string]ListenPort{}, hub: hub, ownPorts: ownPorts}
}

// list is the current set, sorted by port, for hello.
func (w *portWatch) list() []ListenPort {
	w.mu.Lock()
	defer w.mu.Unlock()
	out := make([]ListenPort, 0, len(w.current))
	for _, p := range w.current {
		out = append(out, p)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].RemotePort != out[j].RemotePort {
			return out[i].RemotePort < out[j].RemotePort
		}
		return out[i].key() < out[j].key()
	})
	return out
}

// opened records one port (from the scan or a port.opened RPC).
func (w *portWatch) opened(p ListenPort) {
	w.mu.Lock()
	_, had := w.current[p.key()]
	w.current[p.key()] = p
	w.mu.Unlock()
	if !had {
		logger.Info("port detected", "port", p.RemotePort, "addr", p.Addr, "family", p.Family)
		w.hub.publish("port-detected", same(p))
	}
}

// closed drops every record of a port number.
func (w *portWatch) closed(port uint16) {
	w.mu.Lock()
	dropped := false
	for k, p := range w.current {
		if p.RemotePort == port {
			delete(w.current, k)
			dropped = true
		}
	}
	w.mu.Unlock()
	if dropped {
		logger.Info("port gone", "port", port)
		w.hub.publish("port-undetected", same(map[string]uint16{"remote_port": port}))
	}
}

// apply diffs a fresh scan against the current set.
func (w *portWatch) apply(scan []ListenPort) {
	seen := map[string]bool{}
	for _, p := range scan {
		seen[p.key()] = true
		w.opened(p)
	}
	w.mu.Lock()
	var gone []ListenPort
	for k, p := range w.current {
		if !seen[k] {
			gone = append(gone, p)
			delete(w.current, k)
		}
	}
	w.mu.Unlock()
	for _, p := range gone {
		logger.Info("port gone", "port", p.RemotePort)
		w.hub.publish("port-undetected", same(map[string]uint16{"remote_port": p.RemotePort}))
	}
}

// scan reads both files and returns the reportable set. A missing file (a
// non-Linux dev box, or no IPv6) reads as empty.
func (w *portWatch) scan(v4, v6 string) []ListenPort {
	exclude := excludeEnv()
	for _, p := range w.ownPorts() {
		exclude[p] = true
	}
	var out []ListenPort
	for _, body := range []struct {
		s  string
		v6 bool
	}{{v4, false}, {v6, true}} {
		for _, p := range parseProcNetTCP(body.s, body.v6) {
			if shouldReport(p.RemotePort, exclude) {
				out = append(out, p)
			}
		}
	}
	return out
}

// run polls once a second until ctx ends, parsing only when the raw files
// changed — the common case, forever, is "nothing moved".
func (w *portWatch) run(ctx context.Context) {
	t := time.NewTicker(time.Second)
	defer t.Stop()
	var lastV4, lastV6 string
	first := true
	for {
		v4b, _ := os.ReadFile("/proc/net/tcp")
		v6b, _ := os.ReadFile("/proc/net/tcp6")
		v4, v6 := string(v4b), string(v6b)
		if first || v4 != lastV4 || v6 != lastV6 {
			w.apply(w.scan(v4, v6))
			lastV4, lastV6, first = v4, v6, false
		}
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
	}
}
