# CLI hook gate card: payload fields vs title/summary

- Claude PreToolUse payload keys: `tool_name`, `tool_input` (`.command` for Bash), `permission_mode`, `session_id` — `app/src-tauri/cli/src/main.rs:249-253`, `:2305`
- `derive_hook_title` (`main.rs:1560`) reads top-level `command` / `tool` / `title` — NOT `tool_name`/`tool_input` → real payloads miss → fallback `agent: pre-tool-use`
- `derive_hook_summary` (`main.rs:1587`) reads `summary`/`description`/`body`/`reason`, else raw payload JSON truncated to 280 bytes via `truncate_at_char_boundary`
- Daemon only humanizes titles starting `agent: ` for subkind `pre-tool-use` — `app/src-tauri/server/internal/term/feed.go:178-181`
- `docs/vault/server-go.md` Phase 110 note saying CLI reads command/tool is stale vs live Claude payload
- Payloads without `tool_name` must keep old fallback; other subcommand titles unchanged
- `ymux_policy::evaluate` in PreToolUse handler: not part of card text
- Builds/tests CI-only: `CLAUDE.md` Rule #17
