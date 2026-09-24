package term

// The tmux binary is NOT installed on the CI `go` job's ubuntu image, and a
// test that shells out to a real multiplexer would be a flake generator
// anyway. Every test here drives an injected runner, so what is under test is
// our argv construction and our parsing — which is where the bugs live.

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

// fake builds a Tmux whose runner records calls and replies from respond.
func fake(respond func(args []string) ([]byte, error)) (*Tmux, *[][]string) {
	calls := &[][]string{}
	t := &Tmux{
		bin: "tmux",
		run: func(_ context.Context, name string, args ...string) ([]byte, error) {
			*calls = append(*calls, append([]string{name}, args...))
			return respond(args)
		},
	}
	return t, calls
}

func ok(out string) func([]string) ([]byte, error) {
	return func([]string) ([]byte, error) { return []byte(out), nil }
}

// exitErr is what exec returns for a command that ran and failed — the "no
// server running" case, which must NOT surface as an API error.
func exitErr() error { return &exec.ExitError{ProcessState: &os.ProcessState{}} }

func TestValidName(t *testing.T) {
	good := []string{"api", "my session", "עברית", "feat-123", "a"}
	for _, s := range good {
		if !ValidName(s) {
			t.Errorf("ValidName(%q) = false, want true", s)
		}
	}
	bad := map[string]string{
		"":       "empty",
		"a:b":    "colon is a tmux target separator",
		"a.b":    "dot is a tmux target separator",
		"-x":     "leading dash parses as a flag",
		"a\nb":   "newline breaks the list-sessions record",
		"a\x00b": "NUL",
		"a\x7fb": "DEL",
	}
	for s, why := range bad {
		if ValidName(s) {
			t.Errorf("ValidName(%q) = true, want false (%s)", s, why)
		}
	}
	long := make([]byte, 129)
	for i := range long {
		long[i] = 'a'
	}
	if ValidName(string(long)) {
		t.Error("129-char name accepted, want rejected")
	}
}

func TestListParsesRecords(t *testing.T) {
	// A name containing a space is the reason the format uses \x1f rather than
	// whitespace as the field separator.
	out := "api\x1f3\x1f1700000000\x1f1\x1f/srv/api\n" +
		"my session\x1f1\x1f1700000100\x1f0\x1f/home/y\n"
	tm, calls := fake(ok(out))

	got, err := tm.List()
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("got %d sessions, want 2", len(got))
	}
	if got[0].Name != "api" || got[0].Windows != 3 || got[0].Created != 1700000000 ||
		got[0].Attached != 1 || got[0].Path != "/srv/api" {
		t.Errorf("first session parsed wrong: %+v", got[0])
	}
	if got[1].Name != "my session" || got[1].Attached != 0 {
		t.Errorf("second session parsed wrong: %+v", got[1])
	}
	if (*calls)[0][1] != "list-sessions" {
		t.Errorf("called %v, want list-sessions", (*calls)[0])
	}
}

func TestListNoServerIsEmptyNotError(t *testing.T) {
	// `tmux list-sessions` with no server exits non-zero. That is an accurate
	// empty list, not a failure — reporting it as one would put a red banner on
	// every machine that simply has no sessions yet.
	tm, _ := fake(func([]string) ([]byte, error) { return nil, exitErr() })
	got, err := tm.List()
	if err != nil {
		t.Fatalf("List returned error for a stopped server: %v", err)
	}
	if len(got) != 0 {
		t.Fatalf("got %d sessions, want 0", len(got))
	}
}

func TestListSpawnFailureIsAnError(t *testing.T) {
	// No tmux binary at all is a real error: the caller cannot tell "not
	// installed" from "nothing running" otherwise.
	tm, _ := fake(func([]string) ([]byte, error) { return nil, errors.New("exec: not found") })
	if _, err := tm.List(); err == nil {
		t.Fatal("List returned nil error when tmux could not be spawned")
	}
}

func TestKillUsesExactTarget(t *testing.T) {
	// `-t =api` must not match "api-staging". Without the `=` prefix tmux does
	// prefix matching and this would kill the wrong session.
	tm, calls := fake(ok(""))
	if err := tm.Kill("api"); err != nil {
		t.Fatalf("Kill: %v", err)
	}
	last := (*calls)[len(*calls)-1]
	if last[1] != "kill-session" {
		t.Fatalf("called %v, want kill-session", last)
	}
	if last[3] != "=api" {
		t.Errorf("target %q, want %q (exact match)", last[3], "=api")
	}
}

func TestKillMissingSession(t *testing.T) {
	tm, _ := fake(func(args []string) ([]byte, error) {
		if args[0] == "has-session" {
			return nil, exitErr()
		}
		return nil, nil
	})
	if err := tm.Kill("gone"); !errors.Is(err, ErrNoSession) {
		t.Fatalf("Kill(missing) = %v, want ErrNoSession", err)
	}
}

func TestBadNamesNeverReachTmux(t *testing.T) {
	tm, calls := fake(ok(""))
	for _, op := range []struct {
		name string
		run  func() error
	}{
		{"create", func() error { return tm.Create("a:b", "") }},
		{"kill", func() error { return tm.Kill("a.b") }},
		{"rename", func() error { return tm.Rename("ok", "-flag") }},
	} {
		if err := op.run(); !errors.Is(err, ErrBadName) {
			t.Errorf("%s with a bad name = %v, want ErrBadName", op.name, err)
		}
	}
	if len(*calls) != 0 {
		t.Errorf("a rejected name still spawned tmux: %v", *calls)
	}
}

func TestCreateDetachedWithCwd(t *testing.T) {
	tm, calls := fake(ok(""))
	if err := tm.Create("work", "/srv/app"); err != nil {
		t.Fatalf("Create: %v", err)
	}
	args := (*calls)[0]
	// Detached (-d) is the contract: the session must outlive every client.
	if !contains(args, "-d") {
		t.Errorf("create args %v missing -d", args)
	}
	if !contains(args, "/srv/app") {
		t.Errorf("create args %v missing cwd", args)
	}
}

func TestAttachArgsForceUTF8(t *testing.T) {
	args := AttachArgs("api")
	if args[0] != "-u" {
		t.Errorf("attach args %v: -u must come first (systemd gives the daemon no LANG)", args)
	}
	if args[len(args)-1] != "=api" {
		t.Errorf("attach target %q, want exact match", args[len(args)-1])
	}
}

func TestParseDim(t *testing.T) {
	cases := map[string]uint16{
		"80": 80, "24": 24, "": 0, "abc": 0, "-5": 0, "1e3": 0,
		"100000": 0, // would wrap in the uint16 ioctl field
		"9999":   9999,
	}
	for in, want := range cases {
		if got := parseDim(in); got != want {
			t.Errorf("parseDim(%q) = %d, want %d", in, got, want)
		}
	}
}

func TestDisplayNamePrecedence(t *testing.T) {
	// label > auto_name > claude_title > raw. claude_title is weakest because
	// Claude rewrites it every turn; auto_name is derived once and is stable.
	full := MetaEntry{Label: "L", AutoName: "A", ClaudeTitle: "C"}
	if got := DisplayName("raw", full); got != "L" {
		t.Errorf("label should win, got %q", got)
	}
	if got := DisplayName("raw", MetaEntry{AutoName: "A", ClaudeTitle: "C"}); got != "A" {
		t.Errorf("auto_name should beat claude_title, got %q", got)
	}
	if got := DisplayName("raw", MetaEntry{ClaudeTitle: "C"}); got != "C" {
		t.Errorf("claude_title should be used, got %q", got)
	}
	if got := DisplayName("raw", MetaEntry{}); got != "raw" {
		t.Errorf("empty meta should fall back to the tmux name, got %q", got)
	}
}

func TestLoadMetaDegradesToEmpty(t *testing.T) {
	// Missing and corrupt must both degrade, never fail: a session list with no
	// labels beats a 500.
	if got := LoadMeta(t.TempDir()); len(got) != 0 {
		t.Errorf("missing file returned %d entries, want 0", len(got))
	}

	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, ".ymux"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(MetaPath(home), []byte("{not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := LoadMeta(home); len(got) != 0 {
		t.Errorf("corrupt file returned %d entries, want 0", len(got))
	}

	good := `{"version":1,"sessions":{"api":{"label":"API","origin":"box-1a2b"}}}`
	if err := os.WriteFile(MetaPath(home), []byte(good), 0o600); err != nil {
		t.Fatal(err)
	}
	got := LoadMeta(home)
	if got["api"].Label != "API" || got["api"].Origin != "box-1a2b" {
		t.Errorf("parsed meta wrong: %+v", got)
	}
}

func TestAnnotateKeepsUnlabelledSessions(t *testing.T) {
	// tmux is the truth; the meta file is decoration. A session with no entry
	// must survive the join.
	in := []Session{{Name: "api"}, {Name: "solo"}}
	got := Annotate(in, map[string]MetaEntry{"api": {Label: "API"}})
	if len(got) != 2 {
		t.Fatalf("got %d rows, want 2", len(got))
	}
	if got[0].Display != "API" {
		t.Errorf("labelled session display = %q, want API", got[0].Display)
	}
	if got[1].Display != "solo" {
		t.Errorf("unlabelled session display = %q, want its raw name", got[1].Display)
	}
}

func contains(hay []string, needle string) bool {
	for _, s := range hay {
		if s == needle {
			return true
		}
	}
	return false
}
