//go:build !linux

package term

// pty_other.go — the daemon ships on linux only (resources/ymux-server-linux-
// {x64,arm64} are the entire server artifact). This stub exists so the package
// still builds on a macOS or Windows dev box: `go vet ./...` and `go test
// ./...` stay usable there, and the failure — if anyone ever runs the daemon
// off-target — is one clean error instead of a compile break in an unrelated
// package.

import "errors"

// ErrUnsupported reports that PTY allocation is linux-only.
var ErrUnsupported = errors.New("term: PTY sessions are supported on linux only")

// PTY is the non-linux placeholder. It is never constructed.
type PTY struct{}

// StartPTY always fails off linux.
func StartPTY(string, []string, []string, uint16, uint16) (*PTY, error) {
	return nil, ErrUnsupported
}

// Read always fails off linux.
func (p *PTY) Read([]byte) (int, error) { return 0, ErrUnsupported }

// Write always fails off linux.
func (p *PTY) Write([]byte) (int, error) { return 0, ErrUnsupported }

// Resize is a no-op off linux.
func (p *PTY) Resize(uint16, uint16) error { return ErrUnsupported }

// Close is a no-op off linux.
func (p *PTY) Close() error { return nil }
