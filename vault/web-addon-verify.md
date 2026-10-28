# ymux-web add-on (Phase 112) — verify anchors
- prose owner: `docs/vault/backend-remote.md` § web_addon.rs; this note = file:line anchors + live-verify gotchas only

## Desktop side (`app/src-tauri/src/`)
- `web_addon.rs:92` `init` ← `lib.rs:12807` (setup); log `web_addon.rs:115-118` (format string `:117`) info `embedded web bundle {label}: {n} files, {bytes} bytes` (suffix `, B bytes` beyond the goal wording → match as prefix, never `$`-anchored)
- dev build on `devUrl` → `web_addon.rs:110` info `no embedded index.html — ... unavailable` → live check needs a Tauri-CLI build (CLAUDE.md Rule #13)
- label = `<CARGO_PKG_VERSION>-<sha256 8 hex>` `web_addon.rs:79` `label_for`
- registry label swap: `addons.rs:754` replaces placeholder `"embedded"` (`crates/ymux-addons/src/lib.rs:135`); add-on name `"ymux in the browser"` `crates/ymux-addons/src/lib.rs:264`
- routine dispatch: `addons.rs:249-251` → `detect` `web_addon.rs:127` / `install` `:140` (log `installed {label} ({n} files)` `:180`) / `uninstall` `:185` (`rm -rf ~/.ymux/server/www`)
- auto-update: `lib.rs:4846` `spawn_ssh` → `web_addon.rs:229` `spawn_auto_update`; key `host|want` inserted into `SYNCED` at `:235` BEFORE detect; empty/equal `have` → return `:246`; log `auto-update {have} → {want}` `:249`; failure `auto-update failed:` `:251` clears the key
- default log threshold Info: `crates/ymux-core/src/lib.rs:153` → all WEBADDON lines visible without changing Settings → Logs

## Daemon side (`app/src-tauri/server/`)
- web root: `cmd/ymux-server/main.go:326` `SetWebRoot(*base)` → `internal/term/webapp.go:44` `<dir>/www/current`
- `/` = `current/index.html` when present, else diagnostic page; `/diag` always diagnostic (`docs/vault/server-go.md` § term/webapp.go)
- markers: diagnostic `<title>ymux · דף בדיקה</title>` `internal/term/page.html:6`; app `<title>YMUX</title>` `app/index.html:10`
- committed blob reports `ymux-server 2.19.0` (`--version`, 2026-10-27) ≥ 2.9.0 gate; API binds `127.0.0.1:<port>` only (`cmd/ymux-server/main.go:344`), `--dir` default `~/.ymux/server` → always pass a temp `--dir`
- side-port proxy run without a build: [howto/run-committed-server-blob.md](howto/run-committed-server-blob.md) (`--dir $D`, stage `$D/www/current/index.html`)

## Live-verify gotchas
- once per host+label per RUN: a forced relabel on the server (`mv www/<label> www/<other>; ln -sfn <other> www/current`) needs a desktop RESTART before reconnect, else `SYNCED` short-circuits at `web_addon.rs:235`
- no add-on (`www/current/index.html` absent) → `detect` prints nothing → no log, no write (`web_addon.rs:246`)
- `https://<domain>/` requires the `nginx-proxy` add-on domain (`pairing.rs`, marker `~/.ymux/server/mobile-domain`)
