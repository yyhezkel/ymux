package hooks

import (
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func TestListenPreferredReclaimsThePort(t *testing.T) {
	// Phase 111: a restarted daemon must come back on the same port, or every
	// running claude keeps dialing the old YMUX_SOCKET_ADDR.
	f := filepath.Join(t.TempDir(), "hook-port")
	ln := listenPreferred(f)
	if ln == nil {
		t.Fatal("no listener")
	}
	_, first, _ := net.SplitHostPort(ln.Addr().String())
	b, _ := os.ReadFile(f)
	if strings.TrimSpace(string(b)) != first {
		t.Fatalf("recorded %q, bound %s", b, first)
	}
	ln.Close()
	again := listenPreferred(f)
	if again == nil {
		t.Fatal("no listener on restart")
	}
	defer again.Close()
	if _, p, _ := net.SplitHostPort(again.Addr().String()); p != first {
		t.Errorf("restart bound %s, want the previous %s", p, first)
	}
}

func TestListenPreferredFallsBackWhenTaken(t *testing.T) {
	f := filepath.Join(t.TempDir(), "hook-port")
	busy, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer busy.Close()
	_, p, _ := net.SplitHostPort(busy.Addr().String())
	if err := os.WriteFile(f, []byte(p+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	ln := listenPreferred(f)
	if ln == nil {
		t.Fatal("no fallback listener")
	}
	defer ln.Close()
	_, got, _ := net.SplitHostPort(ln.Addr().String())
	if got == p {
		t.Fatal("bound a port that was taken?")
	}
	b, _ := os.ReadFile(f)
	if n, _ := strconv.Atoi(strings.TrimSpace(string(b))); strconv.Itoa(n) != got {
		t.Errorf("file not updated to the new port: %q vs %s", b, got)
	}
}
