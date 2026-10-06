# ymux curated vault — map of content (pipeline index)

- Canonical vault: `docs/vault/INDEX.md` (hash-gated by `scripts/vault-check.mjs`); this file only routes into it
- Build glue: `docs/vault/build-glue.md` § `app/src-tauri/Cargo.toml` and `build.rs`

## Build facts
- `app/src-tauri/build.rs` `main()` = `check_staged_resources()` → `emit_build_metadata()` → rerun-if-changed `../dist` → `tauri_build::build()`
- Staged CLI gitignored: `.gitignore:42` `resources/ymux-cli.exe`, `.gitignore:44` `resources/ymux-cli` (mac)
- Bundled via `app/src-tauri/tauri.conf.json:38` resources list
- Staging command: `app/package.json:12` `build:linux-cli` (PowerShell `scripts/build-linux-cli.ps1`)
- Prerequisite doc: `CLAUDE.md` Session workflow "Fresh worktree" bullet + CI section — run before any cargo step on a fresh checkout
- Builds/tests CI-only: `CLAUDE.md` Rule #17

## Pipeline how-tos
- [howto/run-committed-server-blob.md](howto/run-committed-server-blob.md): query the committed Go daemon on a side port without building (Rule #17)
- [howto/redeploy-server-blob.md](howto/redeploy-server-blob.md): detect a stale deployed daemon + redeploy the committed blob

## Notes
- [macos-signing](macos-signing.md) — macOS signing / hardened runtime / notarisation: where config + CI live, what blocks live proof
- [pane-close-cleanup](pane-close-cleanup.md) — what each pane-retire path drops (sessions, watchers, agent_runs, briefs)

## Test facts
- `npm test` = `node --experimental-strip-types --test "src/*.test.ts"` (`app/package.json:16`) → needs node ≥22.6; pipeline host node v20.20.2 rejects the flag → frontend unit tests prove on CI only; outcome checks = grep-structural
- Pure-module pattern: zero-import `app/src/<x>.ts` + `<x>.test.ts` (e.g. `termMenuCopy.ts`); new module must be added to a `docs/vault/*.md` `covers:` list
