package config

// migrate.go — one-time data-directory migration for the ymux-insights →
// ymux-server rename (Phase 77 S5). On first boot of the 2.0 daemon the data
// dir moves from ~/.ymux/insights to ~/.ymux/server, preserving the token,
// chat.db (paired devices), workspace.db, metrics.db, and logs in place.

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

// MigrateDataDir moves a legacy data directory to the new location, once. It
// acts only when the legacy dir holds real data (a `token` file) and the new
// dir has not been initialized yet (no `token`), so it is idempotent and safe
// to call on every start. It prefers an atomic whole-directory rename (both live
// under ~/.ymux, i.e. the same filesystem); if the new dir already exists
// non-empty — e.g. the installer pre-created it — it moves entries one by one.
// Returns true if a migration happened.
//
// The `token` file is the "already migrated" test in both directions, so the
// entry-by-entry path must not stop at the first entry that will not move:
// once `token` is across, no later boot comes back for the rest (and if it is
// not across, the daemon mints a new one and no later boot comes back
// either). Every entry is attempted; the ones that failed are reported
// together and stay in the legacy dir.
func MigrateDataDir(legacy, current string) (bool, error) {
	if !hasFile(legacy, "token") {
		return false, nil // nothing legacy to migrate
	}
	if hasFile(current, "token") {
		return false, nil // current already initialized — never clobber
	}
	if err := os.MkdirAll(filepath.Dir(current), 0o755); err != nil {
		return false, err
	}

	// Fast path: current is absent or an empty dir → take its name atomically.
	if empty, _ := dirEmpty(current); empty {
		_ = os.Remove(current) // no-op if absent; drops the empty dir
		if err := os.Rename(legacy, current); err != nil {
			return false, fmt.Errorf("rename %s → %s: %w", legacy, current, err)
		}
		return true, nil
	}

	// Slow path: current exists with content (but no token) — move each entry.
	if err := os.MkdirAll(current, 0o755); err != nil {
		return false, err
	}
	entries, err := os.ReadDir(legacy)
	if err != nil {
		return false, err
	}
	var failed []error
	for _, e := range entries {
		src := filepath.Join(legacy, e.Name())
		dst := filepath.Join(current, e.Name())
		if err := os.Rename(src, dst); err != nil {
			failed = append(failed, fmt.Errorf("move %s: %w", e.Name(), err))
		}
	}
	_ = os.Remove(legacy) // best-effort: drops the legacy dir only if now empty
	return true, errors.Join(failed...)
}

func hasFile(dir, name string) bool {
	_, err := os.Stat(filepath.Join(dir, name))
	return err == nil
}

func dirEmpty(dir string) (bool, error) {
	f, err := os.Open(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return true, nil // absent counts as "nothing in the way"
		}
		return false, err
	}
	defer f.Close()
	names, err := f.Readdirnames(1)
	if err != nil && err.Error() != "EOF" {
		// Readdirnames returns io.EOF on empty; treat any read issue conservatively.
		return len(names) == 0, nil
	}
	return len(names) == 0, nil
}
