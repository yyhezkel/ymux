// Package term is the server-side terminal: tmux session management plus a
// binary WebSocket that attaches a real PTY to one (Phase 95, the first slice
// of "ymux in the browser" — docs/WEB-DESIGN.md §3).
//
// Two rules shape everything here:
//
//   - ATTACH, NEVER SPAWN A BARE SHELL. Every terminal this package hands out
//     is `tmux attach` against a named session, so a browser pane is
//     persistent by construction and a dropped connection loses nothing. It is
//     the same "attach means attach" rule the desktop follows
//     (docs/DECISIONS.md, 2026-08-23).
//   - TMUX IS THE TRUTH. This package keeps no session table. A session exists
//     because tmux says so; its identity is its tmux name, not a UUID we mint.
//     That is what lets the desktop and a browser see one reality
//     (docs/DECISIONS.md, Q1).
//
// Consequence worth stating: multiple clients attached to one session are
// tmux's own multi-client case, so there is no fan-out, no ring buffer and no
// shared state in this package. Each WebSocket owns one `tmux attach` process.
//
// Rule #1: a PTY carries the user's shell content. Nothing here logs bytes —
// only session names, byte COUNTS and error kinds.
// Rule #3: every tmux invocation is an argv array. No string concatenation.
package term

import (
	"context"
	"errors"
	"os/exec"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"ymux-server/internal/logging"
)

var logger = logging.New("SRV:TERM")

// ErrNoSession is returned when tmux has no session by that name.
var ErrNoSession = errors.New("no such tmux session")

// ErrBadName is returned for a session name tmux could not address.
var ErrBadName = errors.New("invalid session name")

// runner executes a command and returns its combined stdout. Injectable so the
// tests never need a tmux binary on the CI runner (the `go` job is a bare
// ubuntu image).
type runner func(ctx context.Context, name string, args ...string) ([]byte, error)

func execRunner(ctx context.Context, name string, args ...string) ([]byte, error) {
	return exec.CommandContext(ctx, name, args...).Output()
}

// Tmux wraps the tmux CLI.
type Tmux struct {
	bin string
	run runner

	// envOnce caches whether `new-session -e` is available (tmux >= 3.2).
	envOnce sync.Once
	envOK   bool
}

// NewTmux returns a Tmux driving the real binary.
func NewTmux() *Tmux { return &Tmux{bin: "tmux", run: execRunner} }

// cmdTimeout bounds every tmux call. These are local process spawns that
// normally return in milliseconds; a hang here would wedge an HTTP handler.
const cmdTimeout = 5 * time.Second

func (t *Tmux) exec(args ...string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), cmdTimeout)
	defer cancel()
	return t.run(ctx, t.bin, args...)
}

// ValidName reports whether s is addressable as a tmux session name.
//
// tmux itself treats `:` and `.` as target separators, so a name containing
// them cannot be addressed with `-t` at all. A leading `-` would be parsed as
// a flag. Control characters are rejected because they would corrupt the
// `list-sessions` record separator. Everything else — including spaces and
// non-ASCII, which Hebrew labels need — is allowed: the argv array (Rule #3)
// is what makes that safe, not the charset.
func ValidName(s string) bool {
	if s == "" || len(s) > 128 || strings.HasPrefix(s, "-") {
		return false
	}
	if strings.ContainsAny(s, ":.") {
		return false
	}
	for _, r := range s {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	return true
}

// target formats an EXACT-match tmux target. The `=` prefix disables tmux's
// prefix matching, so killing "api" can never hit "api-staging".
func target(name string) string { return "=" + name }

// Session is one tmux session as tmux reports it, before any session-meta
// annotation is joined on (see meta.go).
type Session struct {
	Name     string `json:"name"`
	Windows  int    `json:"windows"`
	Created  int64  `json:"created"`  // unix seconds
	Attached int    `json:"attached"` // number of attached tmux clients
	Path     string `json:"path"`     // session working directory
}

// listFormat is the tmux -F template, fields separated by TAB so a session
// name containing spaces still parses. ValidName rejects every control
// character, so a name can never contain one.
//
// NOT "\x1f": tmux 3.4 (verified live 2026-10-05) escapes a non-printable
// byte in -F output to the four literal characters `\037`, so every line came
// back as one field, List() returned [] on every real box, and Retain() then
// dropped every hook token on each list. A tab is passed through verbatim.
const listFormat = "#{session_name}\t#{session_windows}\t#{session_created}\t#{session_attached}\t#{session_path}"

// List returns every live tmux session.
//
// A non-zero exit is NOT an error here: "no server running on ..." is exactly
// what tmux says when nothing is up, and that is an accurate empty list. Only
// a failure to spawn the binary at all is reported as one — the same
// distinction the CLI's session-meta pruning makes, for the same reason
// (treating "no server" as an error would surface a red banner on a machine
// that simply has no sessions yet).
func (t *Tmux) List() ([]Session, error) {
	out, err := t.exec("list-sessions", "-F", listFormat)
	if err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			return []Session{}, nil // no server / no sessions
		}
		return nil, err
	}
	sessions := []Session{}
	for _, line := range strings.Split(strings.TrimRight(string(out), "\n"), "\n") {
		if line == "" {
			continue
		}
		// SplitN: a tab inside the path (the last field) stays in the path.
		f := strings.SplitN(line, "\t", 5)
		if len(f) < 5 {
			continue
		}
		created, _ := strconv.ParseInt(f[2], 10, 64)
		windows, _ := strconv.Atoi(f[1])
		attached, _ := strconv.Atoi(f[3])
		sessions = append(sessions, Session{
			Name: f[0], Windows: windows, Created: created, Attached: attached, Path: f[4],
		})
	}
	return sessions, nil
}

// Has reports whether a session exists.
func (t *Tmux) Has(name string) bool {
	if !ValidName(name) {
		return false
	}
	_, err := t.exec("has-session", "-t", target(name))
	return err == nil
}

// Create starts a DETACHED session. cwd may be empty (tmux inherits the
// daemon's). Creating detached and attaching separately is deliberate: the
// session outlives every client, which is the whole point.
//
// env (Phase 100) becomes SESSION-scoped environment (`-e K=V`), which tmux
// gives every pane of the session and which beats the global environment —
// the hook variables the desktop sets with `set-environment -g`. Keys are
// passed sorted so the argv is deterministic. Callers must check
// SupportsSessionEnv first; tmux < 3.2 rejects `-e` and the create fails.
//
// cmd (Phase 104), when given, is the session's program as an argv array
// after `--`: tmux execs it directly, no shell, no expansion (verified on
// tmux 3.4 with an argument containing `$(id)`) — Rule #3. Without it the
// session runs the user's shell.
func (t *Tmux) Create(name, cwd string, env map[string]string, cmd ...string) error {
	if !ValidName(name) {
		return ErrBadName
	}
	args := []string{"new-session", "-d", "-s", name}
	if cwd != "" {
		args = append(args, "-c", cwd)
	}
	keys := make([]string, 0, len(env))
	for k := range env {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		args = append(args, "-e", k+"="+env[k])
	}
	if len(cmd) > 0 {
		args = append(append(args, "--"), cmd...)
	}
	if _, err := t.exec(args...); err != nil {
		return err
	}
	logger.Info("tmux session created", "session", name, "has_cwd", cwd != "", "env_vars", len(env))
	return nil
}

// SupportsSessionEnv reports whether this tmux accepts `new-session -e`
// (added in 3.2). Asked once per daemon run; a tmux that cannot even report
// its version is treated as too old, so the create still succeeds — just
// without the hook variables.
func (t *Tmux) SupportsSessionEnv() bool {
	t.envOnce.Do(func() {
		out, err := t.exec("-V")
		if err != nil {
			logger.Warn("tmux -V failed; browser sessions will not route hooks to the daemon", "err", err)
			return
		}
		t.envOK = versionAtLeast(strings.TrimSpace(string(out)), 3, 2)
		if !t.envOK {
			logger.Warn("tmux too old for new-session -e; browser sessions will not route hooks to the daemon",
				"version", strings.TrimSpace(string(out)))
		}
	})
	return t.envOK
}

// versionAtLeast parses `tmux -V` output ("tmux 3.4", "tmux 3.2a",
// "tmux next-3.5", "tmux master") against major.minor. A development build
// with no number ("master") is assumed new.
func versionAtLeast(v string, major, minor int) bool {
	v = strings.TrimPrefix(v, "tmux ")
	v = strings.TrimPrefix(v, "next-")
	if v == "master" {
		return true
	}
	maj, rest, _ := strings.Cut(v, ".")
	mj, err := strconv.Atoi(maj)
	if err != nil {
		return false
	}
	digits := strings.TrimRightFunc(rest, func(r rune) bool { return r < '0' || r > '9' })
	mn, _ := strconv.Atoi(digits)
	return mj > major || (mj == major && mn >= minor)
}

// Rename renames a session.
func (t *Tmux) Rename(from, to string) error {
	if !ValidName(from) || !ValidName(to) {
		return ErrBadName
	}
	if !t.Has(from) {
		return ErrNoSession
	}
	if _, err := t.exec("rename-session", "-t", target(from), to); err != nil {
		return err
	}
	logger.Info("tmux session renamed", "from", from, "to", to)
	return nil
}

// Kill ends a session and every process in it.
func (t *Tmux) Kill(name string) error {
	if !ValidName(name) {
		return ErrBadName
	}
	if !t.Has(name) {
		return ErrNoSession
	}
	if _, err := t.exec("kill-session", "-t", target(name)); err != nil {
		return err
	}
	logger.Info("tmux session killed", "session", name)
	return nil
}

// paneTarget is an exact-match target for a session's CURRENT pane. A bare
// "=name" is a session target and pane commands (send-keys, display-message)
// reject it with "can't find pane" — the trailing colon is what makes it a
// pane target (verified on tmux 3.4).
func paneTarget(name string) string { return target(name) + ":" }

// sendChunk bounds one send-keys call: each byte is one argv entry with -H.
const sendChunk = 1024

// SendBytes types raw bytes into a session's current pane (Phase 103, the
// agent `send` verbs). `-H` takes each byte as hex, so control bytes and
// escape sequences arrive exactly as given — no key-name parsing, no shell.
func (t *Tmux) SendBytes(name string, b []byte) error {
	if !ValidName(name) {
		return ErrBadName
	}
	for len(b) > 0 {
		n := min(len(b), sendChunk)
		args := make([]string, 0, n+4)
		args = append(args, "send-keys", "-t", paneTarget(name), "-H")
		for _, c := range b[:n] {
			args = append(args, strconv.FormatUint(uint64(c), 16))
		}
		if _, err := t.exec(args...); err != nil {
			return err
		}
		b = b[n:]
	}
	return nil
}

// CurrentPath is the working directory of a session's current pane — where a
// split of it starts.
func (t *Tmux) CurrentPath(name string) string {
	if !ValidName(name) {
		return ""
	}
	out, err := t.exec("display-message", "-p", "-t", paneTarget(name), "#{pane_current_path}")
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

// AttachArgs is the argv for attaching to a session, shared by the PTY layer.
// `-u` forces UTF-8: the daemon runs under systemd with a minimal environment
// where LANG is often unset, and without it tmux draws box characters as
// question marks in the browser.
func AttachArgs(name string) []string {
	return []string{"-u", "attach-session", "-t", target(name)}
}

// Environment reads a session's own environment (`show-environment`, the
// variables set with -e at create or set-environment later). Removed
// variables (`-NAME`) are skipped. Phase 111: RecoverHooks reads a session's
// hook identity back from here after a daemon restart — the token included,
// so the result is never logged (Rule #8).
func (t *Tmux) Environment(name string) (map[string]string, error) {
	if !ValidName(name) {
		return nil, ErrBadName
	}
	out, err := t.exec("show-environment", "-t", target(name))
	if err != nil {
		return nil, err
	}
	env := map[string]string{}
	for _, line := range strings.Split(string(out), "\n") {
		if k, v, ok := strings.Cut(line, "="); ok && !strings.HasPrefix(k, "-") {
			env[k] = v
		}
	}
	return env, nil
}

// SetEnv sets one variable in a session's environment (Phase 111).
func (t *Tmux) SetEnv(name, key, value string) error {
	if !ValidName(name) {
		return ErrBadName
	}
	_, err := t.exec("set-environment", "-t", target(name), key, value)
	return err
}

