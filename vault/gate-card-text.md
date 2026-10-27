# Gate card text — who writes title/summary, per path

- CLI derives both, for every agent hook: `app/src-tauri/cli/src/main.rs:2637-2638` → `derive_hook_title` (`:1560`) / `derive_hook_summary` (`:1587`)
- `derive_hook_title` pre-tool-use: `payload.command` → "Run `cmd` ?"; `payload.tool` → "Allow `tool` ?"; `payload.title`; else `agent: <subcommand>`
- Claude Code PreToolUse sends `tool_name` + `tool_input` (never `command`/`tool`) → title always falls back to `agent: pre-tool-use`
- `derive_hook_summary` no summary/description/body/reason → raw `serde_json::to_string(payload)`, clipped 280 bytes + `…` → summary starts with `{`
- Desktop path: `app/src-tauri/src/rpc_server.rs` `feed.push` (`:1306`); card humanize block excludes pre-tool-use (`:1773-1803`) → desktop gate card = CLI text verbatim (FOLLOWUPS P3 "derive_hook_title / derive_hook_summary")
- Daemon path (browser sessions): `app/src-tauri/server/internal/term/hookdispatch.go` `addCard` → empty title → `(no title)` → `cardText` in `term/feed.go` (en + he)
- `cardText` pre-tool-use → `gateText` (`term/feed.go`): fallback title (`agent: ` prefix or `(no title)`) → `agent.Humanize` if `tool_name` else `Claude wants to use a tool` + clipped command; derived title kept, summary starting `{` → clipped command
- `agent.Humanize` (`server/internal/agent/humanize.go`) = golden port of Rust `humanize_notification` (`rpc_server.rs:419`) → copy changes in both; gate-only fallbacks belong in `term/feed.go`
- Frontend renders as-is: `app/src/FeedPanel.tsx` `feed-title` / `feed-summary` (no payload parsing) → card text fix = daemon or CLI, never UI
- Go source change (non-test) → `ci-windows.yml` "Assert the embedded server binaries were rebaked" fails unless `resources/ymux-server-linux-{x64,arm64}` move too; rebake = CI `ymux-server-linux` artifact
- Pipeline host: no `go` binary → Go tests prove on CI only
- Vault page: `docs/vault/server-go.md` § "Gate card text fallback (Phase 110)"
