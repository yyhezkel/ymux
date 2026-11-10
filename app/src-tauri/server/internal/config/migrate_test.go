package config

import (
	"os"
	"path/filepath"
	"testing"
)

func write(t *testing.T, dir, name, content string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}

func read(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	return string(b)
}

// Legacy dir with data + absent current → atomic whole-dir migration.
func TestMigrateWholeDir(t *testing.T) {
	root := t.TempDir()
	legacy := filepath.Join(root, "insights")
	current := filepath.Join(root, "server")
	write(t, legacy, "token", "secret")
	write(t, filepath.Join(legacy, "logs", "clients", "dev1"), "a.log", "line")

	migrated, err := MigrateDataDir(legacy, current)
	if err != nil || !migrated {
		t.Fatalf("migrate: migrated=%v err=%v", migrated, err)
	}
	if got := read(t, filepath.Join(current, "token")); got != "secret" {
		t.Fatalf("token not migrated: %q", got)
	}
	if got := read(t, filepath.Join(current, "logs", "clients", "dev1", "a.log")); got != "line" {
		t.Fatalf("nested log not migrated: %q", got)
	}
	if _, err := os.Stat(legacy); !os.IsNotExist(err) {
		t.Fatalf("legacy dir should be gone, err=%v", err)
	}
}

// Installer pre-created an empty current dir → migration still runs.
func TestMigrateIntoPreCreatedEmptyDir(t *testing.T) {
	root := t.TempDir()
	legacy := filepath.Join(root, "insights")
	current := filepath.Join(root, "server")
	write(t, legacy, "token", "tok")
	if err := os.MkdirAll(current, 0o755); err != nil { // installer's mkdir -p
		t.Fatal(err)
	}
	migrated, err := MigrateDataDir(legacy, current)
	if err != nil || !migrated {
		t.Fatalf("migrate: migrated=%v err=%v", migrated, err)
	}
	if read(t, filepath.Join(current, "token")) != "tok" {
		t.Fatal("token not migrated into pre-created dir")
	}
}

// Current already initialized (has token) → never clobber.
func TestMigrateSkipsWhenCurrentInitialized(t *testing.T) {
	root := t.TempDir()
	legacy := filepath.Join(root, "insights")
	current := filepath.Join(root, "server")
	write(t, legacy, "token", "old")
	write(t, current, "token", "new")

	migrated, err := MigrateDataDir(legacy, current)
	if err != nil || migrated {
		t.Fatalf("should skip: migrated=%v err=%v", migrated, err)
	}
	if read(t, filepath.Join(current, "token")) != "new" {
		t.Fatal("current token must be untouched")
	}
}

// No legacy dir (fresh install) → no-op.
func TestMigrateNoLegacy(t *testing.T) {
	root := t.TempDir()
	migrated, err := MigrateDataDir(filepath.Join(root, "insights"), filepath.Join(root, "server"))
	if err != nil || migrated {
		t.Fatalf("fresh install should be a no-op: migrated=%v err=%v", migrated, err)
	}
}

// Entry-by-entry path with one entry that cannot move (a non-empty directory
// already sits at its destination): everything else still migrates — the
// token and the entries that sort after the failure included — and the
// failure is reported. Stopping at the first error used to strand them for
// good, because `token` is what the next boot's "already migrated" test reads.
func TestMigrateSlowPathContinuesPastAFailedEntry(t *testing.T) {
	root := t.TempDir()
	legacy := filepath.Join(root, "insights")
	current := filepath.Join(root, "server")
	write(t, legacy, "chat.db", "devices")
	write(t, filepath.Join(legacy, "logs"), "old.log", "old")
	write(t, legacy, "token", "secret")
	write(t, legacy, "workspace.db", "ws")
	// Pre-created by the installer: `logs` sorts before `token`.
	write(t, filepath.Join(current, "logs"), "new.log", "new")

	migrated, err := MigrateDataDir(legacy, current)
	if !migrated {
		t.Fatal("a partial migration must still report that it happened")
	}
	if err == nil {
		t.Fatal("the entry that could not move must be reported")
	}
	for name, want := range map[string]string{"chat.db": "devices", "token": "secret", "workspace.db": "ws"} {
		if got := read(t, filepath.Join(current, name)); got != want {
			t.Errorf("%s = %q, want %q", name, got, want)
		}
	}
	if got := read(t, filepath.Join(current, "logs", "new.log")); got != "new" {
		t.Errorf("the existing destination was touched: %q", got)
	}
	if got := read(t, filepath.Join(legacy, "logs", "old.log")); got != "old" {
		t.Errorf("the entry that failed must stay where it was: %q", got)
	}
	// Idempotent afterwards: the token is across, nothing more to do.
	if again, err := MigrateDataDir(legacy, current); again || err != nil {
		t.Fatalf("second run: migrated=%v err=%v", again, err)
	}
}
