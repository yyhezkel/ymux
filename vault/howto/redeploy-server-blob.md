# Detect + redeploy a stale remote daemon
- installed path: `~/.ymux/bin/ymux-server` (+ `ymux-insights` symlink); data dir `~/.ymux/server` (token, chat.db, workspace.db, metrics.db)
- shipped version: `app/src-tauri/crates/ymux-addons/src/lib.rs` `INSIGHTS_VERSION` = Go `core.Version` (`app/src-tauri/server/internal/core/`); must move together
- detect (product): `app/src-tauri/src/addons.rs` `status_for` → `update_available` = remote `ymux-server --version` last token != `INSIGHTS_VERSION`; surfaced as Update in Settings → Add-ons. Manual only — no auto-upgrade on connect
- detect (shell): `ymux-server --version` → `ymux-server X.Y.Z`; running process: `curl -s http://127.0.0.1:7879/api/version` (unauthenticated) → `"version":"X.Y.Z"`; blob identity: `cmp ~/.ymux/bin/ymux-server app/src-tauri/resources/ymux-server-linux-x64`
- install (product): `addons.rs` `insights_install` — SFTP embedded blob → `<path>.<pid>.tmp` → chmod 0755 + `mv -f` → systemd --user unit, fallback `nohup sg docker -c "exec $DAEMON serve"`
- no systemd --user bus (e.g. `Failed to connect to bus`) → nohup arm; process tree = root `sg` parent + user `ymux-server serve` child
- headless/agent shell: start with `setsid -f` so session exit does not kill the daemon; never `&` / background tool calls
- stale symptom: 404 on routes newer than the deploy (2026-10-06: 2.2.0 deploy → 404 `/claude-usage`, `/analytics`; 2.9.0 blob → 200)
- see also: [run-committed-server-blob.md](run-committed-server-blob.md) (side-port check, no deploy)
