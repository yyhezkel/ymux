---
vault: backend-rpc
covers:
  - app/src-tauri/src/rpc_server.rs
  - app/src-tauri/src/brief.rs
  - app/src-tauri/src/context_store.rs
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
- **`BIND_ERROR`** ([BIND_ERROR@rpc_server.rs:37](../../app/src-tauri/src/rpc_server.rs)) records why
  the endpoint isn't listening. It exists because a failed bind used to be one
  `log_warn` and a bare `return` — indistinguishable from "no ports detected yet", with
  `PortsWindow` spinning forever. `doctor` reads it. Zero binds is a `log_error`; a
  partial bind is a `log_warn`, and it is the early warning for the `sun_path` cap.
- `handle_client_with_telemetry` wraps every connection with a `conn_id` and
  START/END + elapsed-ms lines, so slow handlers surface in `debug.log` without a
  profiler. `HANDLER_SEQ` is monotonic per process and shows up in `doctor`.
  Read timeout: `HANDLER_READ_TIMEOUT` = 30s.

## The method catalog

`dispatch()` ([dispatch@rpc_server.rs:635](../../app/src-tauri/src/rpc_server.rs)) is one big match
and **is** the canonical list — nothing else enumerates these:

- **Workspaces** — `ping`, `list-workspaces`, `select-workspace`, `new-workspace`,
  `update-workspace`, `delete-workspace`, `reset-layout`
  — `list-workspaces`, `new-workspace` and `update-workspace` replies run `secret_env::redact` (secret env rows `value: ""`), a second layer over `persist()`; `tree` / `ui.tree` emit no env
- **Panes** — `tree`, `ui.tree`, `split`, `action.split`, `action.connect`,
  `pane.scrollback`, `pane.screenshot`, `set-pane-title`, `set-pane-annotation`,
  `set-status`, `pane.persistence.get`, `pane.persistence.list`, `pane.kill-session`
  <!-- Phase 91.F: the `LayoutNode::Pane` literal `split` builds also sets the new
  `diff_cwd: None` field (see crates.md) — mechanical, no protocol change. -->

- **Input** — `send`, `send-key` (via `translate_key`: `cr`, `tab`, `escape`, `bs`,
  `arrow-*`, `home`, `end`, and `ctrl-x` forms)
- **Agent surface** — `notify`, `feed.push`, `feed.decide`, `context.inject` (Phase
  105.C, below), and the hook verbs
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
   it is bilingual (ported to the daemon with golden tests in Phase 99:
   `server/internal/agent/humanize.go`, as are `translate_key` → `keys.go` and
   `brief.rs` → `brief.go` — change both sides), driven by the settings language. For a Stop it reads
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

## Briefs (`brief.rs`)

The data layer behind the Queue panel / Briefing card. An agent may end its final
assistant message with a plain-text `[ymux-brief]` block (`task:` / `status:` /
`ask:` / `rec:` / `next:` / `delta:`, one per line, plus the Phase 105 **sticky**
`goal:` / `done:` — aliases `done when` / `done-when` / `done_when` — written once
and repeated only on change; `PaneBrief` carries what this turn said, the session
store keeps the last non-empty value. The Go port does NOT parse these two yet —
BACKLOG P2). Because the CLI forwards the
Stop hook payload verbatim, the desktop parses `last_assistant_message` with **no
CLI cooperation**: `parse_brief` is pure string ops (last marker line wins via a
full-line scan, so a self-quoting agent doesn't truncate its brief; keys are ASCII
split on the first `:`, so fully-RTL values are safe; markdown decoration and
fences are stripped; unknown keys ignored). `status` is
`working|waiting-for-you|stuck|done` with `waiting`/`blocked` aliases, defaulting
to `done`. No marker → a **degraded** brief (`degraded: true`, task from
`claude_title`, delta = first line of the message, never a fabricated `ask`).

State lives in `AppState.briefs: HashMap<pane_id, PaneBriefEntry>` — entry =
`{ brief, last_prompt, prompt_ms, session_ended, seq }`, keyed by the **resolved**
pane (same `resolve_hook_pane` rule as `agent_runs`), in-memory only (same
rationale), hydrated by the `pane_briefs` command and pushed as the `pane:brief`
event (whole entry, `seq`-guarded). Three `feed.push` arms feed it:
`user-prompt-submit` stores the clipped last user prompt (the queue's "got from
you: …" line), `stop` stores the parsed/degraded brief and clears `session_ended`,
`session-end` sets `session_ended` but **keeps** the brief — it summarizes
finished work. A non-degraded brief also rewrites the stop feed card: humanize
sees the message with the block stripped (`pre_brief_text`), and the summary
becomes `ask · rec` (else `delta`). Rule #1: brief/prompt content never reaches a
log line — log lines carry pane id + flags, never text. `BriefStatus` also derives
`Deserialize` since Phase 105, because the session context files store it.

## Session context (`context_store.rs`, Phase 105)

The persisted counterpart of `AppState.briefs`: one record per Claude Code
**session id**, not per pane — `SessionContext { session_id, ws_id, pane_id, cwd,
first_prompt (clip 2000), first_prompt_ms, goal, done_when, log ≤ 200 LogEntry,
version }` (`goal` / `done_when`: last non-empty brief value wins, in
`append_turn`), a
`LogEntry` being `{ ts_ms, kind: turn|closed, status: BriefStatus, task, delta,
next, ask, rec, degraded }`. Spec: `docs/CONTEXT.md`.

The same three `feed.push` arms feed it, through `context_store::on_hook(state,
app, HookOrigin, HookEvent)`. The origin is read once above the `match`:
`payload.session_id` and `payload.cwd` (the CLI forwards the hook payload
verbatim, so these are Claude Code's own fields; a hook with no session id is a
no-op), the **resolved** pane, and `find_workspace_for_pane` on it (only for the
three subkinds). `user-prompt-submit` → `Prompt` sets `first_prompt` **only while
empty** and writes only when that or the placement changed (every prompt passes
through here); `stop` → `Stop(&brief)` appends a `turn` line from the same
`PaneBrief` the Queue gets, degraded included; `session-end` → `End(reason)`
appends `closed` with Claude Code's `reason` enum as delta and the last task.
Each write emits `context:changed {session_id, ws_id}`. Failure is a `log_warn`
with ids only and never touches the hook's own handling.

Storage: `ContextState` (on `AppState.context`) caches every file of
`<config>/context/sessions/` behind one mutex, loaded once (`ensure_loaded`);
`mutate_at` is clone → apply → bump `version` → atomic write → swap, so a failed
write leaves memory and disk as they were, and a closure returning `false` writes
nothing. Session ids are validated as `[A-Za-z0-9_-]{1,128}` before any path is
built. A file that fails to parse is **poisoned**: never written over, mutations
refused. `startup()` (called from `run()`'s setup) prunes `*.json`/`*.tmp` with
mtime older than 30 days (`prune_dir`) and warms the cache on a background
thread. Tauri commands: `session_context_list(ws_id)` (most recent activity
first) and `session_context_get(session_id)`.

**Injection (Phase 105.C).** RPC `context.inject` (its own arm, NOT a feed.push
reply — SessionStart never touches the feed) resolves the pane with
`resolve_hook_pane` and calls `injection_for_hook`: off when
`settings.brief.inject_context` is false; otherwise it reads this session's
record, the pane's workspace (falling back to the record's `ws_id`) and that
workspace's `intent`, and the workspace's sessions for `startup`. The text comes
from the pure `build_injection(source, this, intent, siblings)`: `compact`/`resume`
→ the Context Rail card's shape: `Goal` / `Done when` (sticky store fields) /
`Now: [status] task` / `Next` (latest turn), the last `INJECT_LOG_LINES` = 5 ✔
deltas (`delta_line`, oldest → newest) and a tail `Original request` (first
prompt ≤ 400 chars); `assemble(head, items, tail)` drops items first; `startup` →
intent + ≤ 8 other OPEN sessions (task + status); `clear`/unknown → "". Every field
is flattened to one line, control chars dropped; `[ymux-context]` header;
`INJECT_MAX_BYTES` = 1536 with the oldest items dropped first (`assemble`), then a
char-boundary byte clip. Logged: source, ids, byte count. Every path takes a `dir`
parameter so the unit tests run against a tempdir: first prompt set once, the
200-line cap, degraded turns, round-trip + version, the poison gate, id
validation, prune.

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

- The app no longer listens on the pre-rename `winmux-` pipe/socket: a `winmux-cli` still
  on someone's PATH, or an MCP host config written against the old name, now fails at connect.
- A pool slot that fails `make_listener` retries every 500ms forever rather than dying.
  A permanently broken pipe therefore shows up as a repeating `log_warn`, not silence.
- `dev.get-state` / `build_dev_state` embeds `CARGO_PKG_VERSION` and the optional
  `YMUX_GIT_HASH` build-time env var.

## Read the source when

You need a method's exact params/result shape, the `translate_key` table, or the
`humanize_notification` copy. The wire contract as documented lives in
`docs/PROTOCOLS.md`; the CLI verbs that call these methods are in `docs/CLI.md`.
