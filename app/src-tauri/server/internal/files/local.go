// Package files serves the sandboxed shared-folder / directory-picker API
// (/api/v2/files/*, PHASE-77-DESIGN §4.2). LocalFiles is a core.FilesProvider
// backed by a single root directory; every request path is confined to that
// root — `..` is collapsed against "/" before joining, and symlink targets that
// escape the root are rejected. The daemon runs as the user, so filesystem
// permissions are the final backstop.
package files

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"ymux-server/internal/core"
)

var (
	// ErrOutsideSandbox is returned for any path that would escape the root.
	ErrOutsideSandbox = errors.New("path escapes the sandbox root")
	// ErrNotFound is returned for a missing path.
	ErrNotFound = errors.New("not found")
	// ErrIsDir is returned when a file operation targets a directory.
	ErrIsDir = errors.New("path is a directory")
)

// DefaultMaxUpload caps a single upload (configurable via NewLocalFiles).
const DefaultMaxUpload int64 = 100 << 20 // 100 MB

// defaultReadCap bounds Read when the caller doesn't ask for a specific size.
const defaultReadCap int64 = 1 << 20 // 1 MB

// LocalFiles is a root-confined core.FilesProvider.
type LocalFiles struct {
	root      string // absolute, symlink-resolved
	maxUpload int64
}

// NewLocalFiles confines all operations to root (created if missing). maxUpload
// <= 0 uses DefaultMaxUpload.
func NewLocalFiles(root string, maxUpload int64) (*LocalFiles, error) {
	abs, err := filepath.Abs(root)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(abs, 0o755); err != nil {
		return nil, err
	}
	// Canonicalise the root so containment comparisons use real paths.
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		abs = resolved
	}
	if maxUpload <= 0 {
		maxUpload = DefaultMaxUpload
	}
	return &LocalFiles{root: abs, maxUpload: maxUpload}, nil
}

// Root returns the absolute sandbox root.
func (l *LocalFiles) Root() string { return l.root }

// within reports whether p is at or below the sandbox root.
func (l *LocalFiles) within(p string) bool {
	rel, err := filepath.Rel(l.root, p)
	if err != nil {
		return false
	}
	return rel == "." || (rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)))
}

// resolve maps a client-supplied path to an absolute path guaranteed inside the
// sandbox. The client path is treated as rooted at the sandbox: any `..` is
// collapsed against "/" (so it can never climb above root) before joining. For
// an existing target we also reject a symlink whose destination escapes; for a
// not-yet-existing target (upload) we check the nearest existing ancestor.
func (l *LocalFiles) resolve(p string) (string, error) {
	clean := filepath.Clean("/" + strings.ReplaceAll(p, "\\", "/"))
	full := filepath.Join(l.root, clean)
	if !l.within(full) {
		return "", ErrOutsideSandbox
	}
	// Walk up to the nearest path that exists, EvalSymlinks it, re-check.
	probe := full
	for {
		if _, err := os.Lstat(probe); err == nil {
			break
		}
		parent := filepath.Dir(probe)
		if parent == probe {
			return full, nil // nothing exists yet up to root; join already contained
		}
		probe = parent
	}
	resolved, err := filepath.EvalSymlinks(probe)
	if err == nil && !l.within(resolved) {
		return "", ErrOutsideSandbox
	}
	return full, nil
}

func entryOf(name string, fi os.FileInfo) core.FileEntry {
	t, size := "file", fi.Size()
	if fi.IsDir() {
		t, size = "dir", 0
	}
	return core.FileEntry{Name: name, Type: t, Size: size, Modified: fi.ModTime().Unix()}
}

// List returns the resolved cwd + entries. depth 2 flattens one level of
// children (name carries the "sub/child" relative path). Dirs sort before files.
func (l *LocalFiles) List(p string, depth int) (string, []core.FileEntry, error) {
	full, err := l.resolve(p)
	if err != nil {
		return "", nil, err
	}
	fi, err := os.Stat(full)
	if err != nil {
		return "", nil, ErrNotFound
	}
	if !fi.IsDir() {
		return full, []core.FileEntry{entryOf(fi.Name(), fi)}, nil
	}
	if depth < 1 {
		depth = 1
	}
	if depth > 2 {
		depth = 2
	}
	des, err := os.ReadDir(full)
	if err != nil {
		return "", nil, err
	}
	entries := []core.FileEntry{}
	for _, de := range des {
		info, e := de.Info()
		if e != nil {
			continue
		}
		entries = append(entries, entryOf(de.Name(), info))
		if depth == 2 && de.IsDir() {
			if sub, e := os.ReadDir(filepath.Join(full, de.Name())); e == nil {
				for _, se := range sub {
					if si, e := se.Info(); e == nil {
						entries = append(entries, entryOf(de.Name()+"/"+se.Name(), si))
					}
				}
			}
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		if (entries[i].Type == "dir") != (entries[j].Type == "dir") {
			return entries[i].Type == "dir"
		}
		return entries[i].Name < entries[j].Name
	})
	return full, entries, nil
}

// Read returns up to maxBytes of a file (default cap when maxBytes <= 0).
func (l *LocalFiles) Read(p string, maxBytes int64) ([]byte, bool, error) {
	full, err := l.resolve(p)
	if err != nil {
		return nil, false, err
	}
	fi, err := os.Stat(full)
	if err != nil {
		return nil, false, ErrNotFound
	}
	if fi.IsDir() {
		return nil, false, ErrIsDir
	}
	if maxBytes <= 0 {
		maxBytes = defaultReadCap
	}
	f, err := os.Open(full)
	if err != nil {
		return nil, false, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, maxBytes+1))
	if err != nil {
		return nil, false, err
	}
	truncated := int64(len(data)) > maxBytes
	if truncated {
		data = data[:maxBytes]
	}
	return data, truncated, nil
}

// Write creates/overwrites a file atomically and returns its sha256 + size.
func (l *LocalFiles) Write(p string, data []byte) (string, int64, error) {
	if int64(len(data)) > l.maxUpload {
		return "", 0, fmt.Errorf("upload exceeds max size (%d bytes)", l.maxUpload)
	}
	full, err := l.resolve(p)
	if err != nil {
		return "", 0, err
	}
	if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
		return "", 0, err
	}
	tmp := full + ".ymux-tmp"
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		return "", 0, err
	}
	if err := os.Rename(tmp, full); err != nil {
		_ = os.Remove(tmp)
		return "", 0, err
	}
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), int64(len(data)), nil
}

// Delete removes a file or an EMPTY directory (never recursive — safety).
func (l *LocalFiles) Delete(p string) error {
	full, err := l.resolve(p)
	if err != nil {
		return err
	}
	if _, err := os.Stat(full); err != nil {
		return ErrNotFound
	}
	return os.Remove(full) // errors on a non-empty directory
}

// Open streams a file for download; the caller closes the ReadCloser.
func (l *LocalFiles) Open(p string) (io.ReadCloser, int64, error) {
	full, err := l.resolve(p)
	if err != nil {
		return nil, 0, err
	}
	fi, err := os.Stat(full)
	if err != nil {
		return nil, 0, ErrNotFound
	}
	if fi.IsDir() {
		return nil, 0, ErrIsDir
	}
	f, err := os.Open(full)
	if err != nil {
		return nil, 0, err
	}
	return f, fi.Size(), nil
}

// ── Phase 116 (WEB-DESIGN F2): the File Manager's mutating ops ─────────

// ErrExists is returned when a create/rename/copy target is already there.
var ErrExists = errors.New("already exists")

// archiveTimeout bounds one zip / tar / unzip run.
const archiveTimeout = 10 * time.Minute

// Mkdir creates one directory (the desktop's sftp create_dir: no parents).
func (l *LocalFiles) Mkdir(p string) error {
	full, err := l.resolve(p)
	if err != nil {
		return err
	}
	if _, err := os.Lstat(full); err == nil {
		return ErrExists
	}
	if err := os.Mkdir(full, 0o755); err != nil {
		if os.IsNotExist(err) {
			return ErrNotFound
		}
		return err
	}
	return nil
}

// Rename moves within the sandbox; it never overwrites.
func (l *LocalFiles) Rename(from, to string) error {
	src, err := l.resolve(from)
	if err != nil {
		return err
	}
	dst, err := l.resolve(to)
	if err != nil {
		return err
	}
	if src == l.root {
		return ErrOutsideSandbox
	}
	if _, err := os.Lstat(src); err != nil {
		return ErrNotFound
	}
	if _, err := os.Lstat(dst); err == nil {
		return ErrExists
	}
	return os.Rename(src, dst)
}

// Copy copies a file, or a directory recursively. Symlinks are skipped, not
// followed, so a link cannot pull outside content into the copy.
func (l *LocalFiles) Copy(from, to string) error {
	src, err := l.resolve(from)
	if err != nil {
		return err
	}
	dst, err := l.resolve(to)
	if err != nil {
		return err
	}
	fi, err := os.Lstat(src)
	if err != nil {
		return ErrNotFound
	}
	if _, err := os.Lstat(dst); err == nil {
		return ErrExists
	}
	if fi.IsDir() && (dst == src || strings.HasPrefix(dst, src+string(filepath.Separator))) {
		return fmt.Errorf("cannot copy a folder into itself")
	}
	if !fi.IsDir() {
		return copyFile(src, dst, fi.Mode())
	}
	return filepath.WalkDir(src, func(p string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(src, p)
		out := filepath.Join(dst, rel)
		switch {
		case d.Type()&os.ModeSymlink != 0:
			return nil
		case d.IsDir():
			info, err := d.Info()
			if err != nil {
				return err
			}
			return os.Mkdir(out, info.Mode().Perm()|0o700)
		case d.Type().IsRegular():
			info, err := d.Info()
			if err != nil {
				return err
			}
			return copyFile(p, out, info.Mode())
		}
		return nil // devices, sockets, fifos: not copied
	})
}

func copyFile(src, dst string, mode os.FileMode) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_CREATE|os.O_EXCL|os.O_WRONLY, mode.Perm())
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		_ = os.Remove(dst)
		return err
	}
	return out.Close()
}

// DeleteTree removes a file or a directory with everything under it — the
// desktop's recursive SFTP delete, behind the same confirm in the UI. The
// sandbox root itself is refused.
func (l *LocalFiles) DeleteTree(p string) error {
	full, err := l.resolve(p)
	if err != nil {
		return err
	}
	if full == l.root {
		return ErrOutsideSandbox
	}
	if _, err := os.Lstat(full); err != nil {
		return ErrNotFound
	}
	return os.RemoveAll(full)
}

// Archive runs `zip -r -q <out> ./name…` or `tar -czf <out> -- ./name…` in
// cwd (Rule #3: argv). Each name must sit directly or deeper under cwd; the
// output is a basename. A missing zip binary reads like the desktop's remote
// failure ("exit 127"), which is what makes the UI offer tar instead.
func (l *LocalFiles) Archive(cwd string, names []string, output, format string) (string, error) {
	if len(names) == 0 {
		return "", fmt.Errorf("%s: no items selected", format)
	}
	if output == "" || strings.ContainsAny(output, `/\`) || output == "." || output == ".." {
		return "", fmt.Errorf("output must be a file name, not a path")
	}
	dir, err := l.resolve(cwd)
	if err != nil {
		return "", err
	}
	args := make([]string, 0, len(names))
	for _, n := range names {
		if n == "" || filepath.IsAbs(n) {
			return "", fmt.Errorf("bad item %q", n)
		}
		full := filepath.Join(dir, n)
		if !l.within(full) || full == dir {
			return "", ErrOutsideSandbox
		}
		if _, err := os.Lstat(full); err != nil {
			return "", ErrNotFound
		}
		rel, _ := filepath.Rel(dir, full)
		args = append(args, "."+string(filepath.Separator)+rel) // never read as a flag
	}
	out := filepath.Join(dir, output)
	if _, err := os.Lstat(out); err == nil {
		return "", ErrExists
	}
	var bin string
	var argv []string
	switch format {
	case "zip":
		bin, argv = "zip", append([]string{"-r", "-q", out}, args...)
	case "targz":
		bin, argv = "tar", append([]string{"-czf", out, "--"}, args...)
	default:
		return "", fmt.Errorf("unknown archive format %q", format)
	}
	if err := runIn(dir, bin, argv...); err != nil {
		return "", err
	}
	return out, nil
}

// Unzip extracts into <dir>/<stem>/ (created; existing files overwritten,
// as the desktop's `unzip -o`). Info-ZIP refuses `..` entries itself.
func (l *LocalFiles) Unzip(p string) (string, error) {
	zp, err := l.resolve(p)
	if err != nil {
		return "", err
	}
	fi, err := os.Stat(zp)
	if err != nil {
		return "", ErrNotFound
	}
	if fi.IsDir() {
		return "", ErrIsDir
	}
	stem := strings.TrimSuffix(filepath.Base(zp), filepath.Ext(zp))
	if stem == "" || stem == "." {
		return "", fmt.Errorf("unzip: the archive has no name to extract into")
	}
	dest := filepath.Join(filepath.Dir(zp), stem)
	if err := os.MkdirAll(dest, 0o755); err != nil {
		return "", err
	}
	if err := runIn(filepath.Dir(zp), "unzip", "-o", "-q", zp, "-d", dest); err != nil {
		return "", err
	}
	return dest, nil
}

// runIn runs bin with argv in dir; a non-zero exit carries its output.
func runIn(dir, bin string, argv ...string) error {
	if _, err := exec.LookPath(bin); err != nil {
		return fmt.Errorf("%s failed (exit 127): %s: command not found", bin, bin)
	}
	ctx, cancel := context.WithTimeout(context.Background(), archiveTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, argv...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		code := -1
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			code = ee.ExitCode()
		}
		msg := strings.TrimSpace(string(out))
		if len(msg) > 400 {
			msg = msg[:400]
		}
		return fmt.Errorf("%s failed (exit %d): %s", bin, code, msg)
	}
	return nil
}
