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

## Notes
- [phase92-headers-screens](phase92-headers-screens.md) — Phase 92 header/screen symbols, RPC select, delete landing: file:line anchors
- [macos-signing](macos-signing.md) — macOS signing / hardened runtime / notarisation: where config + CI live, what blocks live proof
