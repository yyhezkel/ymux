package files

// Phase 116 (WEB-DESIGN F2): mkdir / rename / copy / recursive delete /
// archive / unzip — provider rules and the HTTP codes.

import (
	"bytes"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func newLF(t *testing.T) (*LocalFiles, string) {
	t.Helper()
	lf, err := NewLocalFiles(t.TempDir(), 0)
	if err != nil {
		t.Fatal(err)
	}
	return lf, lf.Root()
}

func TestMkdirRenameCopyDeleteTree(t *testing.T) {
	lf, root := newLF(t)
	if err := lf.Mkdir("/a"); err != nil {
		t.Fatal(err)
	}
	if err := lf.Mkdir("/a"); err != ErrExists {
		t.Fatalf("mkdir twice = %v", err)
	}
	if err := lf.Mkdir("/x/y"); err != ErrNotFound {
		t.Fatalf("mkdir without parent = %v", err)
	}
	_ = os.WriteFile(filepath.Join(root, "a", "f.txt"), []byte("hi"), 0o644)
	_ = os.Symlink("/etc/passwd", filepath.Join(root, "a", "link"))
	if err := lf.Copy("/a", "/b"); err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(filepath.Join(root, "b", "f.txt")); string(b) != "hi" {
		t.Fatalf("copied content = %q", b)
	}
	if _, err := os.Lstat(filepath.Join(root, "b", "link")); err == nil {
		t.Fatal("a symlink was copied")
	}
	if err := lf.Copy("/a", "/a/inner"); err == nil {
		t.Fatal("copied a folder into itself")
	}
	if err := lf.Copy("/a", "/b"); err != ErrExists {
		t.Fatalf("copy onto existing = %v", err)
	}
	if err := lf.Rename("/b", "/a"); err != ErrExists {
		t.Fatalf("rename onto existing = %v", err)
	}
	if err := lf.Rename("/b", "/c"); err != nil {
		t.Fatal(err)
	}
	if err := lf.Rename("/../../etc", "/d"); err == nil {
		t.Fatal("renamed from outside")
	}
	if err := lf.DeleteTree("/"); err != ErrOutsideSandbox {
		t.Fatalf("delete root = %v", err)
	}
	if err := lf.DeleteTree("/c"); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(root, "c")); !os.IsNotExist(err) {
		t.Fatal("tree still there")
	}
	if err := lf.Delete("/a"); err == nil {
		t.Fatal("plain Delete removed a non-empty dir")
	}
}

func TestArchiveAndUnzip(t *testing.T) {
	lf, root := newLF(t)
	_ = os.MkdirAll(filepath.Join(root, "p", "dir"), 0o755)
	_ = os.WriteFile(filepath.Join(root, "p", "dir", "f.txt"), []byte("x"), 0o644)
	_ = os.WriteFile(filepath.Join(root, "p", "-rf"), []byte("flag-shaped"), 0o644)
	if _, err := lf.Archive("/p", []string{"../.."}, "o.zip", "zip"); err != ErrOutsideSandbox {
		t.Fatalf("archive outside = %v", err)
	}
	if _, err := lf.Archive("/p", []string{"dir"}, "a/b.zip", "zip"); err == nil {
		t.Fatal("output path accepted")
	}
	if _, err := exec.LookPath("tar"); err == nil {
		out, err := lf.Archive("/p", []string{"dir", "-rf"}, "o.tar.gz", "targz")
		if err != nil || filepath.Base(out) != "o.tar.gz" {
			t.Fatalf("targz = %q %v", out, err)
		}
		if _, err := lf.Archive("/p", []string{"dir"}, "o.tar.gz", "targz"); err != ErrExists {
			t.Fatalf("archive onto existing = %v", err)
		}
	}
	_, zipErr := exec.LookPath("zip")
	_, unzipErr := exec.LookPath("unzip")
	if zipErr != nil {
		if _, err := lf.Archive("/p", []string{"dir"}, "o.zip", "zip"); err == nil || !strings.Contains(err.Error(), "exit 127") {
			t.Fatalf("missing zip = %v, want the exit-127 message", err)
		}
		return
	}
	if _, err := lf.Archive("/p", []string{"dir"}, "o.zip", "zip"); err != nil {
		t.Fatal(err)
	}
	if unzipErr != nil {
		return
	}
	dest, err := lf.Unzip("/p/o.zip")
	if err != nil || dest != filepath.Join(root, "p", "o") {
		t.Fatalf("unzip = %q %v", dest, err)
	}
	if b, _ := os.ReadFile(filepath.Join(dest, "dir", "f.txt")); string(b) != "x" {
		t.Fatalf("extracted = %q", b)
	}
}

func TestMutationsHTTP(t *testing.T) {
	h := mount(t)
	post := func(path, body string) int {
		return do(t, h, "POST", path, bytes.NewBufferString(body), "application/json").Code
	}
	if c := post("/api/v2/files/mkdir", `{"path":"/n"}`); c != 200 {
		t.Fatalf("mkdir → %d", c)
	}
	if c := post("/api/v2/files/mkdir", `{"path":"/n"}`); c != 409 {
		t.Fatalf("mkdir again → %d", c)
	}
	if c := post("/api/v2/files/rename", `{"from":"/n","to":"/m"}`); c != 200 {
		t.Fatalf("rename → %d", c)
	}
	if c := post("/api/v2/files/copy", `{"from":"/nope","to":"/z"}`); c != 404 {
		t.Fatalf("copy missing → %d", c)
	}
	if c := post("/api/v2/files/archive", `{"cwd":"/","names":["m"],"output":"m.zip","format":"rar"}`); c != 422 {
		t.Fatalf("bad format → %d", c)
	}
	rec := do(t, h, "POST", "/api/v2/files/archive", bytes.NewBufferString(`{"cwd":"/","names":["m"],"output":"m.tar.gz","format":"targz"}`), "application/json")
	if _, err := exec.LookPath("tar"); err == nil {
		var out PathBody
		_ = json.Unmarshal(rec.Body.Bytes(), &out)
		if rec.Code != 200 || out.Path != "/m.tar.gz" {
			t.Fatalf("archive → %d %s", rec.Code, rec.Body)
		}
	}
	_ = post("/api/v2/files/mkdir", `{"path":"/m/sub"}`)
	if c := do(t, h, "DELETE", "/api/v2/files/delete?path=/m", nil, "").Code; c == 200 {
		t.Fatal("non-recursive delete removed a non-empty dir")
	}
	if c := do(t, h, "DELETE", "/api/v2/files/delete?path=/m&recursive=true", nil, "").Code; c != 200 {
		t.Fatalf("recursive delete → %d", c)
	}
}
