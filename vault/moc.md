# ymux curated vault — map of content (pipeline index)

- Canonical vault: `docs/vault/INDEX.md` (hash-gated by `scripts/vault-check.mjs`); this file only routes into it
- Build glue: `docs/vault/build-glue.md` § `app/src-tauri/Cargo.toml` and `build.rs`

## Build facts
- `app/src-tauri/build.rs:1-11` `main()` = `emit_build_metadata()` → rerun-if-changed `../dist` → `tauri_build::build()`
- Staged CLI gitignored: `.gitignore:42` `resources/ymux-cli.exe`, `.gitignore:44` `resources/ymux-cli` (mac)
- Bundled via `app/src-tauri/tauri.conf.json:38` resources list
- Staging command: `app/package.json:12` `build:linux-cli` (PowerShell `scripts/build-linux-cli.ps1`)
- Prerequisite doc: `CLAUDE.md:130` (CI section) — run before any cargo step on a fresh checkout
- Builds/tests CI-only: `CLAUDE.md` Rule #17
