package term

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestParseStatusZAndBranch(t *testing.T) {
	out := "## main...origin/main [ahead 1]\x00 M a.go\x00R  new.go\x00old.go\x00?? n\x00x\x00"
	files, br := parseStatusZ(out)
	if br == nil || *br != "main" {
		t.Fatalf("branch = %v", br)
	}
	if len(files) != 3 || files[1].XY != "R " || *files[1].OrigPath != "old.go" || files[2].Path != "n" {
		t.Fatalf("files = %+v", files)
	}
	for in, want := range map[string]string{"HEAD (no branch)": "", "No commits yet on dev": "dev", "feat [gone]": "feat", "solo": "solo"} {
		got := parseBranchHeader(in)
		if (want == "" && got != nil) || (want != "" && (got == nil || *got != want)) {
			t.Errorf("%q → %v", in, got)
		}
	}
}

func TestBranchTargetClip(t *testing.T) {
	if s, ok := sanitizeBranch("feat/x y"); !ok || s != "feat/x-y" {
		t.Errorf("sanitize = %q %v", s, ok)
	}
	for _, bad := range []string{"", "-x", "a..b"} {
		if _, ok := sanitizeBranch(bad); ok {
			t.Errorf("%q accepted", bad)
		}
	}
	if got := defaultWorktreeTarget("/srv/app/", "feat/x"); got != "/srv/app-feat-x" {
		t.Errorf("target = %q", got)
	}
	if got := clipBytes("aא", 2); got != "a" {
		t.Errorf("clip mid-rune = %q", got)
	}
	if !optionLike("--upload-pack=x") || !optionLike("a\nb") || optionLike("main~2") {
		t.Error("optionLike")
	}
}

// A real repository, when git is present (CI has it).
func TestDiffBundleRealGit(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("no git")
	}
	dir := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	run("init", "-q")
	_ = os.WriteFile(filepath.Join(dir, "a.txt"), []byte("one\n"), 0o644)
	run("add", ".")
	run("commit", "-qm", "c1")
	_ = os.WriteFile(filepath.Join(dir, "a.txt"), []byte("two\n"), 0o644)
	_ = os.WriteFile(filepath.Join(dir, "new.txt"), []byte("fresh\n"), 0o644)
	b := diffBundle(context.Background(), dir, DiffSource{Kind: "head"})
	if b.Error != nil {
		t.Fatalf("error = %s", *b.Error)
	}
	if b.Branch == nil || *b.Branch != "main" || len(b.Files) != 2 {
		t.Fatalf("bundle = %+v", b)
	}
	if !strings.Contains(b.DiffText, "+two") || !strings.Contains(b.DiffText, "+fresh") {
		t.Fatalf("diff text missing changes:\n%s", b.DiffText)
	}
	if e := diffBundle(context.Background(), dir, DiffSource{Kind: "ref", GitRef: "--output=/tmp/x"}); e.Error == nil {
		t.Fatal("an option-shaped ref was run")
	}
	if e := diffBundle(context.Background(), t.TempDir(), DiffSource{Kind: "working"}); e.Error == nil || !strings.HasPrefix(*e.Error, "git: ") {
		t.Fatalf("not a repo → %+v", e)
	}
}
