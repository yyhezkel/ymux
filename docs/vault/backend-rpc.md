---
vault: backend-rpc
covers:
  - app/src-tauri/src/rpc_server.rs
  - app/src-tauri/mcp/src/main.rs
  - app/src-tauri/mcp/Cargo.toml
---

# Local RPC endpoint + MCP bridge

Two files. `rpc_server.rs` (~2,560 lines) is the app's local control surface — a
newline-delimited JSON-RPC v2 server that the CLI, agent hooks, the reverse tunnel, and
the MCP bridge all speak to. `mcp/src/main.rs` (~460 lines) is a standalone stdio MCP
server that forwards a handful of tools onto that same endpoint.

Everything that reaches this endpoint is already **on the user's machine as the user** —
there is no auth layer here, and the transport is what provides isolation.

## Transport

| | Windows | macOS / Unix |
|---|---|---|
| Endpoint | named pipe `\\.\pipe\ymux-<user>` | Unix domain socket |
| Concurrency | **pool of 8 listeners** | one listener per path |
| Legacy name | pool of 2 on the pre-rename `winmux-` pipe | legacy path in the candidate list |

`pipe_name()` / `pipe_names()` live in `ymux-core` (shared with `ymux-tunnel`, which
must resolve the same path from the other side).

- **Windows needs a pool** because a named-pipe listener serves one client; a single
  listener means every concurrent connect gets ERROR_PIPE_BUSY. Each of the 8 slots
  loops: `make_listener` → `connect().await` → hand the connection to a **separate
  task** → immediately recreate its listener, so a slow handler never blocks the slot.
  `PIPE_MAX_INSTANCES = 254`.
- **Unix binds every candidate path**, not the first that works. macOS caps `sun_path`
  at 104 bytes and a long `$TMPDIR` can push the primary name over it, so `ymux-core`
  offers a list — and `ymux-tunnel` walks the same list in the same order. Binding all
  of them means the two ends cannot split.
- **`BIND_ERROR`** ([rpc_server.rs:37](../../app/src-tauri/src/rpc_server.rs)) records why
  the endpoint isn't listening. It exists because a failed bind used to be one
  `log_warn` and a bare `return` — indistinguishable from "no ports detected yet", with
  `PortsWindow` spinning forever. `doctor` reads it. Zero binds is a `log_error`; a
  partial bind is a `log_warn`, and it is the early warning for the `sun_path` cap.
- `handle_client_with_telemetry` wraps every connection with a `conn_id` and
  START/END + elapsed-ms lines, so slow handlers surface in `debug.log` without a
  profiler. `HANDLER_SEQ` is monotonic per process and shows up in `doctor`.
  Read timeout: `HANDLER_READ_TIMEOUT` = 30s.

## The method catalog

`dispatch()` ([rpc_server.rs:646](../../app/src-tauri/src/rpc_server.rs)) is one big match
and **is** the canonical list — nothing else enumerates these:

- **Workspaces** — `ping`, `list-workspaces`, `select-workspace`, `new-workspace`,
  `update-workspace`, `delete-workspace`, `reset-layout`
- **Panes** — `tree`, `ui.tree`, `split`, `action.split`, `action.connect`,
  `pane.scrollback`, `pane.screenshot`, `set-pane-title`, `set-pane-annotation`,
  `set-status`, `pane.persistence.get`, `pane.persistence.list`, `pane.kill-session`
- **Input** — `send`, `send-key` (via `translate_key`: `cr`, `tab`, `escape`, `bs`,
  `arrow-*`, `home`, `end`, and `ctrl-x` forms)
- **Agent surface** — `notify`, `feed.push`, `feed.decide`, and the hook verbs
  `session-start`, `session-end`, `stop`, `user-prompt-submit`, `pre-tool-use`,
  `post-tool-use`, `subagent-stop`, `pre-compact`
- **Notes** — `note-add`, `note-list`, `note-update`, `note-done`, `note-delete`
- **Settings / updates** — `settings.load|save|set|preset|get-presets`, `updates.check`
- **Claude** — `claude.sessions.list`
- **Ports** — `port.opened`, `port.closed` (the remote `/proc/net/tcp` watcher calls
  these through the reverse tunnel). Detection-only: record in `detected_ports` + emit
  `port-detected` / `port-undetected`; no forward is opened here. Since Phase 86.C one
  watcher serves every workspace on the same host, so both handlers fan out to all
  `port_watcher_subscribers` of the event's host, and a port is "internal" if it is any
  subscriber's tunnel port.
- **Diagnostics** — `doctor`, `dev.get-state`, `dev.console-tail`,
  `dev.debug-log-tail`, `dev.report-bug`

## Hooks → toasts

An agent hook arrives as one of the hook verbs and turns into a notification:

1. `humanize_notification(subkind, payload, ws_name, lang)` produces `(title, body)` —
   it is bilingual, driven by the settings language. For a Stop it reads
   `response_summary` with `last_assistant_message` (what current Claude Code actually
   sends) as the fallback, so the body shows how the turn ended. **Feed cards for the
   passive lifecycle subkinds (`stop`, `session-start/end`, `post-tool-use`,
   `subagent-stop`, `pre-compact`) go through the same function**: `feed.push` overrides
   the CLI-derived `title`/`summary` desktop-side before building the `FeedItem` — the
   CLI's fallbacks produced "agent: stop" titles and a raw payload dump (or SessionEnd's
   bare `reason`) as the card body, and fixing it here also covers stale remote CLIs.
   The `ws_name` passed there is empty on purpose (the card's meta row already carries
   the workspace chip); `pre-tool-use` is excluded because its Gate-card title is the
   approval prompt itself.
2. `hook_toast_enabled(notifications, hook_settings, subkind)` decides whether a native
   toast fires at all; `hook_toast_should_sound` decides whether it makes noise.
3. `show_toast_with_sound` spawns a thread and uses `notify_rust`.
4. `push_policy_audit` records policy decisions (see `ymux-policy` in `crates.md`).

**`Notification` is a registered hook again**, reversing half of the v0.4.4 decision
that dropped it as observability-only noise. It now has a different job: `dispatch`
reads `payload.notification_type` and folds it into the per-pane agent state
(`AgentRunState::apply_hook` in `lib.rs`), then emits `pane:agent-run`. `pre-tool-use`
and `notification` are the two subkinds that carry no turn timing, so they fold and emit
on their own path; the rest also move the timer and emit further down. If you add a hook
subkind that should affect the traffic light, it goes through `apply_hook`, not through a
second state machine here.

`feed.push` reads `settings::load_from_disk()` once per call (it used to re-read for
the policy, the Block branch and Stop separately). With `blocking: true` it parks the caller on a
`tokio::sync::oneshot::Sender` held in `FeedStore.pending`, and `decide_feed` (shared
with the Tauri `feed_decide` command, defined in `lib.rs`) is what wakes it. That is the
allow/deny prompt loop. `FEED_MAX_ITEMS_LIMIT = 50` — `lib.rs` has its own copy of the
constant.

## The MCP bridge (`mcp/`)

A separate binary, `ymux-mcp`. Wire:
`agent ⇄ stdio JSON-RPC ⇄ ymux-mcp ⇄ named pipe / socket ⇄ app`.

**Stateless per call** — each `tools/call` opens a fresh connection. The app must
already be running; an unreachable pipe becomes an MCP error carrying that message.
`default_pipe_name()` / `default_socket_paths()` mirror the server's path logic, and
`ymux_config_dir()` finds the config dir independently (this binary does not link
`ymux-core`).

Tools exposed: `list_workspaces`, `tree`, `list_panes`, `read_pane`, `take_screenshot`,
`split_pane`, `connect_workspace`, `send_keys`, `notify`, `note_add`. Each maps to one
`dispatch` method; `tool_definitions()` builds the JSON schemas with the `obj()` / `s()`
helpers.

## Invariants

- **Rule #1** — pane content crosses this endpoint (`pane.scrollback`, `read_pane`) but
  is **never logged**. Log the byte count.
- **Rule #8** — the tunnel HMAC token reaches `port.opened` handlers; never log it.
- **Rule #6** — a handler error becomes a JSON-RPC error object, never a panic. One bad
  request must not take down a pool slot.
- Adding a method means: `dispatch` arm + `docs/PROTOCOLS.md` + (if agents should see
  it) an MCP tool definition. Nothing generates these from each other.

## Gotchas

- The legacy `winmux-` pipe/socket names are **load-bearing**, not leftovers — a
  `winmux-cli` still on someone's PATH, or an MCP host config written against the old
  name, reaches the app through them. Removal is scheduled as a set (CLAUDE.md § rename).
- A pool slot that fails `make_listener` retries every 500ms forever rather than dying.
  A permanently broken pipe therefore shows up as a repeating `log_warn`, not silence.
- `dev.get-state` / `build_dev_state` embeds `CARGO_PKG_VERSION` and the optional
  `YMUX_GIT_HASH` build-time env var.

## Read the source when

You need a method's exact params/result shape, the `translate_key` table, or the
`humanize_notification` copy. The wire contract as documented lives in
`docs/PROTOCOLS.md`; the CLI verbs that call these methods are in `docs/CLI.md`.
