---
vault: cli
covers:
  - app/src-tauri/cli/src/main.rs
  - app/src-tauri/cli/src/hooks.rs
  - app/src-tauri/cli/src/port_watch.rs
  - app/src-tauri/cli/src/session_meta.rs
  - app/src-tauri/cli/build.rs
  - app/src-tauri/cli/Cargo.toml
---

# The `ymux` CLI

4,868 lines. One binary that runs in **two very different places**: on the user's Windows
desktop next to the app, and cross-compiled as `ymux-linux-{x64,arm64}` inside a pane on a
remote server, reaching the desktop back through the reverse tunnel.

Everything it does is a JSON-RPC call into `rpc_server.rs`. It holds no state of its own
except the files noted below.

## Transport selection — the first thing to check when it "can't connect"

| Situation | Endpoint |
|---|---|
| Windows, default | named pipe `\\.\pipe\ymux-<USER>` |
| Any platform, `YMUX_PIPE_PATH` set | that pipe |
| Linux/Unix, or Windows fallback | TCP from `YMUX_SOCKET_ADDR`, e.g. `127.0.0.1:8765` |
| Linux, `YMUX_SOCKET_ADDR` unset | **exit code 2** |

The TCP path is the remote case: `ymux-tunnel` forwards that local port back to the
desktop's RPC endpoint. The address reaches the remote three ways — `set_env` on the shell
channel, `tmux set-environment -g`, and `~/.ymux/run/last.env` — and **none of the three
reaches an already-running process**, which is the whole reason sticky ports exist (see
`backend-sessions.md` § tunnel_registry).

`claude-hook` exits 0 right after it reads stdin when `YMUX_HEADLESS` is set. ymux sets it on its own
one-shot `claude -p` calls, which have no pane of their own. A plain `claude` started by hand on the same
host does not set it, so `last.env` still lends that run a pane.

**Reply deadline (`reply_deadline`, main.rs).** `rpc_via` waits for the reply line under a
`tokio::time::timeout`: 300 s (`RPC_REPLY_FLOOR_SECS`), or the wait the request carries
(`wait_timeout_seconds`, or `timeout_ms` rounded up) plus 30 s when that is longer, so the
desktop's own timeout answer always arrives first. On expiry the error is
`no reply from the ymux app within <n>s`. It starts with neither `connect ` nor `rpc error`.
The handshake keeps its own 10 s reads. `wait` polls with short calls, so one poll against a
silent desktop can outlast `--timeout` by up to this deadline.

## Verb families

Clap subcommands, ~60 of them. `docs/CLI.md` is the user-facing reference; this is the map:

- **Workspaces** — `list-workspaces`, `select-workspace`, `new-workspace`,
  `update-workspace`, `delete-workspace`
- **Panes** — `tree`, `split`, `send`, `send-key`, `set-status`, `set-pane-title`,
  `set-pane-annotation`, `pane-disconnect`, `pane-kill-session`,
  `pane-persistence-list`
- **Browser automation** — the largest family: `browser-navigate`, `-go-back`, `-go-home`,
  `-resolve-url`, `-url`, `-history`, `-wait`, `-wait-for`, `-eval`, `-screenshot`,
  `-click`, `-find`, `-snapshot`, `-type`. This is how an agent drives the workspace
  browser.
- **Agent** — `claude-hook`, `setup-hooks`, `claude-sessions-list`
- **Driving other agents** — `wait`, `read`, `worktree create` (below)
- **Notes / settings** — `note`, `settings` (`show` / `set` / `preset`)
- **Ops** — `port-watch`, `session-meta`, `doctor`, `check-updates`
- **Dev** — `dev get-state`, `console-tail`, `debug-log-tail`, `report-bug`

**Hook card text (`derive_hook_title` / `derive_hook_summary`, main.rs).** For
`pre-tool-use` / `tool-permission`, a string `tool_name` gives title ``Allow `<tool>` ?``.
For `pre-tool-use` with `tool_name`, the summary is the first of `tool_input.command |
file_path | path | pattern | url`, clipped to 100 bytes on a char boundary, else empty
(never a JSON dump). Without `tool_name` the old chain (command / tool / title /
`agent: <sub>`, JSON-dump summary) is unchanged, as are all other subcommands.

**`pre-tool-use` and `permission_mode` (main.rs).** The CLI reads the mode before it
dials the desktop. `auto` prints nothing and exits 0, so Claude Code's own classifier
decides and the ymux policy does not run. It also spawns a detached copy of the CLI
(`report_tool_running`) that pushes notification type `tool_running`, so the pane light
leaves NeedsInput without Claude Code waiting. `acceptEdits`, `bypassPermissions` and `skip`
print an explicit `allow`. Every other mode goes to the desktop policy engine.

**When the push fails (`feed_push_with_retry`, `static_fallback_decision`, main.rs).** The
pure `desktop_answered(err)` splits an `rpc_call` error in two. A transport failure
(connect, handshake, write, read, or a 0-byte reply) means the desktop is not there: the
push retries once and then the CLI's static policy decides, Block → deny, Auto → allow,
Gate → allow, because nobody can approve a card. An error the desktop itself replied with
(`rpc error: …`, or a non-empty reply that is not JSON) means it WAS reached: no retry,
and a Gate verdict becomes `ask`, which hands the question to Claude Code's own prompt.
A reply deadline that ran out (`no reply from the ymux app within …`, `reply_timed_out`)
is a hung app: no retry either, since a second push would wait the whole deadline again,
and the static policy decides as for an unreachable desktop.
Until 2026-11-10 every error took the first path, so a reachable desktop that answered
with an error turned a gated call into an allow.

## `wait`, `read`, `worktree create`

These verbs let one agent drive others with no sleep loop. `read` and `worktree create`
go through the generic RPC path. `wait` returns its own `ExitCode` from its arm.

**`run_wait`** polls `pane.agent-state` every 2 s (`WAIT_POLL_SECS`), because the
endpoint has no subscribe path. The pure `wait_step` decides each reply. Idle is
`done | needs-input | failed`, and needs-input is included because `idle_prompt` moves
a finished agent from Done to NeedsInput. The JSON carries the actual state. Exit codes:
0 reached, 2 error, 3 timeout, 4 unknown pane (`known: false`, at once), 5 timeout with
`state: unknown`. **A pane with no hooks never reports idle**: it waits, because a hook
can still arrive, and exits 5. Three RPC failures in sequence exit 2, so a tunnel
reconnect does not end a long wait.

**`--after-seq`**: Done is sticky, so `wait` right after `send` can see the previous
turn. `send` and `send-key` return `agent_seq`, read before the write, and the wait then
requires `seq > after_seq`.

**`read`** prints the `pane.scrollback` JSON. `rpc_via`'s "bad response" error carries
the byte count only, because a reply can hold pane text (Rule #1).

**`worktree create`** sends the name to `worktree.create`, which validates it. It passes
`$YMUX_PANE_ID` so the desktop can find the workspace; it never falls back to the active
workspace.

## `hooks.rs` (730) — `ymux setup-hooks`

Registers agent hooks that point at the local `ymux` binary, so Claude Code (and codex,
gemini) pipe permission requests and lifecycle events back through the tunnel into the
desktop UI.

**`Notification` is registered again as of hooks v1.5.0.** v0.4.4 had dropped it as
observability-only; it is back with a different job — the CLI filters on
`notification_type` and pushes the type alone, and the desktop turns that into per-pane
agent state (see `backend-rpc.md`). Bumping `manifest.json`'s
`hooks.claude-code.version` is what tells existing installs to re-sync.

**`StopFailure` is registered as of hooks v1.8.0.** The `stop-failure` branch (right after
`notification`, before the feed-item dispatch) reads `payload["error"]` (absent/empty →
`"unknown"`), logs and pushes the type alone — never `error_details` /
`last_assistant_message` — as a passive `feed.push` subkind `stop-failure` with
`tmux_session`, ignores the RPC result and exits 0. The desktop paints the pane light
`failed`; never a card or toast.

**`SessionStart` is registered again as of hooks v1.7.0 (Phase 105.C)** — also with a
new job, and still never a feed card. The `session-start` branch (after the env gate)
sends RPC `context.inject` `{pane_id, tmux_session, session_id, source}` under a
`tokio::time::timeout` of `context_timeout_ms()` (1000 ms, `YMUX_CONTEXT_TIMEOUT_MS`
clamped 100–3000) and **fails open**: an error, a timeout or an empty answer prints
nothing and exits 0. The call costs 3 tunnel round trips, so the first default of 300 ms
was below the real cost on a 125–175 ms link. A non-empty answer goes through
`session_start_output`, the only stdout write in that branch — one serde-built
`{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}` line, so
the text cannot break the JSON. One Info line records `outcome=injected|empty|timed_out|error`,
the source, the byte count, the budget and the RPC timing, never the text; the error text
stays at Debug because a malformed reply is quoted in it. `manifest.json` is bumped at
release time, not with the spec.

**Hook timing in the log.** `rpc_via` records `connect_ms`, `handshake_ms` and `reply_ms`
in process-global atomics, so the phases that finished are still readable after
`session-start` drops the future at its budget. `rpc_timing_log()` appends them to the ACK
line, the `session-start` line and every passive line (which now logs after the RPC, not
before). A phase that did not finish stays 0. Over a tunnel, `connect` is only the local
dial, so the channel open shows in `handshake`. For a blocking `pre-tool-use`, `reply`
includes the wait for the human. Durations only (Rule #1).

`setup-hooks` also carries a **dead-hook check** (`ymux_entry_is_runnable`): a hook entry
pointing at a binary that no longer exists is repaired rather than skipped forever. That
is not hypothetical — a move left an entry pointing at the old absolute path, the hook
then failed silently, and the desktop simply never heard from that agent again. Phase 86.E widened it
with `ymux_entry_points_at`: an entry of ours whose executable is not the binary being
installed is stale too. That is the winmux→ymux migration case seen live — four entries
on `~/.winmux/bin/winmux` (exists, runs, has no `last.env` fallback) and a 1.5.0 stamp.
Because the bootstrap runs `setup-hooks --source bundled` on **every connect**, this is
what makes every migrated install converge on the current CLI with no user action. The
same rule rewrites an entry missing the spec's `timeout` (`ymux_entry_has_timeout`).

**Events dropped from the spec are pruned (`prune_dropped_events`).** The install loop visits
spec events only, so `apply_to_settings` first removes our hook commands (`is_ymux_command`,
both spellings) from every other event in `settings.json`. A hook object goes only if its own
command is ours; an entry goes only if that empties its `hooks` array, an event key only if
that empties it, so a user's hook in the same event or entry stays. A spec with no events
prunes nothing. The last spec applied wins: a `--source bundled` run removes an event only a
newer github spec has. `Done.removed` / `DryRun.would_remove` report it; a prune alone takes
the full write path (backup, then write). The legacy `hooks.json` is not pruned.

**One writer for `settings.json`: `write_settings_atomic`** (tmp in `~/.claude`, then rename).
The stamp-only refresh of `ymux_meta.hooks_version` uses it too and now returns `Error` when
the write fails; it used to `fs::write` in place and ignore the result. A rename replaces a
symlinked `settings.json` with a regular file and leaves the link's target untouched, which
is what the unix-only test observes.

The hook spec used to be a hardcoded `&[(event, subcmd, matcher)]` slice. It now ships as
JSON at the repo root — `hooks/claude-code.json`, `hooks/codex.json`,
`hooks/gemini.json` — which the CLI fetches from raw.githubusercontent.com at install
time, with `~/.ymux/cache/hooks/` as a fallback and the bundled spec as the final
fallback. So a hook change reaches existing installs without a CLI rebuild.

## `port_watch.rs` — the LISTEN watcher

Runs on the remote. Every **3s** it reads `/proc/net/tcp` + `/proc/net/tcp6`, extracts
LISTEN sockets and diffs against the set the desktop knows. There is no raw-bytes skip any more:
the timer columns of established sockets change between reads, so a byte compare matched
on 0 of 59 measured ticks (2026-11-09), and the read costs more than the parse. A new
port sends `port.opened`, a vanished
one `port.closed`. The set it diffs against is what the desktop has answered for, not the
last read (`advance`): an event counts as delivered on a result or an `rpc error` answer, and
any other failure leaves it out, so the next tick sends it again. Only a dial failure
(`connect …`) counts toward the exit below; a tick that still has an undelivered event
dials again, so three failed ticks (about 9 s) end the watcher. **Both are detection-only on the desktop** — `rpc_server.rs` records
the port in `detected_ports` and emits `port-detected`; the SSH local-forward is opened
only when the user clicks (`forward_port_start`).

**It owns its own death (Phase 86.A).** The process is an SSH exec child with no PTY, so
nothing external ever killed it — on 2026-08-23 Yossi's server had 15 of them with
ppid=1, some 3 days old, each burning ~5% CPU on a 2,589-line `/proc/net/tcp`. Two
exits now: a `std::thread` reading stdin to EOF (sshd closes it when the channel dies),
and `rpc_call_pinned` — the watcher does NOT use `rpc_call`'s `last.env` fallback
(its tunnel is fixed for life; the fallback is for hooks inherited by a long-lived
`claude`), and three consecutive dial failures end it. Start/exit are logged as
metadata only.

**One per host, not per workspace** (`lib.rs` `port_watcher_hosts`, Phase 86.C) — sibling
workspaces on the same (host, port, user) subscribe to the owner's events. Duplicates
and ppid=1 orphans are the two things `insights/hygiene.go` reaps.

## `session_meta.rs` (982) — multi-machine session labels and session history

`~/.ymux/session-meta.json` on the **server** maps a tmux session name to the Claude
session running inside it plus display metadata, so any ymux desktop connecting to that
server — home, office, laptop — labels the same sessions identically.

Written from two directions: the Claude `stop` hook inside the pane writes
`claude_session_id` + `claude_title` extracted from the transcript every turn, and the
desktop writes labels and origin. `origin` carries the desktop's `machine_id`
(`lib.rs::machine_id`) so the picker can say which machine created a session. Labels cross
the SSH exec **hex-encoded** (`--label-hex`, `lib.rs::hex_utf8`) so Hebrew never meets
shell quoting.

**Session history (Phase 104, WEB-DESIGN §4.2).** `prune` (every `stop`, every
`session-end`, every `session-meta set`) no longer deletes a row whose tmux session is
gone: `prune_with` stamps **`ended_at`** once (a later pass keeps the first time), clears
it when the name is live again, and keeps the row's mapping to its transcript so the
daemon can list, open and resume it (`server/internal/term/history.go`). Retention:
an ended row with **no `claude_session_id`** is dropped at once (nothing to open — the
old behaviour), one older than `HISTORY_MAX_DAYS` (90) is dropped, and only the
`HISTORY_MAX_ENDED` (100) newest ended rows are kept; an unparsable `ended_at` counts as
"now", never as ancient. `record_live` (user-prompt-submit / stop) clears `ended_at` and
records the hook payload's **`cwd`**. `SessionMetaEntry.extra` is a `serde(flatten)`
map, so a field a newer writer adds **survives an older reader's re-save** — before
this, an older CLI silently dropped unknown fields. Mixed-version caveat: a box still on a
pre-0.5.x CLI deletes ended rows until the desktop re-bootstraps it. The desktop only
READS this file (writes go through `ymux session-meta set`), so its struct copy in
`lib.rs` needs no change; it simply ignores the new fields.

**Zellij rows are never judged.** An entry may carry optional `mux: "zellij"`
(`SessionMetaEntry.mux`, additive; absent = tmux/legacy). `prune_with` `continue`s past
those rows: `tmux ls` never lists a zellij session, so judging them stamped every live
one `ended_at`. Residue: a deleted zellij session's row is never reaped.

**Writer key order** (`pick_session`, pure): `tmux display-message #S` → `$ZELLIJ_SESSION_NAME`
(equals what `zellij list-sessions -n` prints, the picker's join key) → `ymux-<sanitized
YMUX_PANE_ID>`; empty = absent. `resolve_session_name()` keeps its `Option<String>` shape;
`handle_hook` uses `resolve_session()` and stamps `mux: "zellij"` on `user-prompt-submit` and
`stop` when the zellij env supplied the key.

## Logging

CLI-side logging goes through `hook_log(level, msg)`, which writes into the remote log
files that `log_sync.rs` pulls back to the desktop's `debug.log`. `~/.ymux/log-level` is
read once per process. **Rule #1 applies with no exception here** — this code runs inside
the user's shell session and sees everything; the v0.4.5 hook-log migration is what made
every remote writer metadata-only, and the desktop copies those lines verbatim into an
LLM-readable file.

## Build

`npm run build:linux-cli` (from `app/`) cross-builds and stages `ymux-cli.exe`,
`ymux-linux-x64`, and `remote-manifest.json` into `src-tauri/resources/`. **It must run
before any cargo step on a fresh checkout** — `ymux-cli.exe` is gitignored and Tauri's
build script hard-fails without it. Staging is **by sha256, not mtime**, because
`resources/` is pulled in with `include_bytes!` and Cargo's staleness check is mtime-based:
re-copying a byte-identical file used to force a full rebuild of the 9.4k-line lib. The
musl cross-build links with `rust-lld` (see `src-tauri/.cargo/config.toml`), so no
external linker is needed on a Windows runner. macOS stages a native `ymux-cli` in
`build-macos-intel.yml` instead, because the staging script is PowerShell-only.

`build.rs` (32 lines) bakes the version. On a release build the Linux CLI rebakes itself
from `CARGO_PKG_VERSION` — expected churn, committed as part of the release.

## Invariants

- **Rule #3** — argv arrays. This binary builds remote commands constantly.
- **Rule #1** — hook payloads carry user prompts and tool arguments. Log metadata only.
- **Rule #8** — the tunnel HMAC token passes through here; never log it.
- A new verb is: clap subcommand + a `dispatch` arm in `rpc_server.rs` + a line in
  `docs/CLI.md`. Nothing generates one from another.
- The app no longer answers on the legacy `winmux-` pipe; the CLI's `default_pipe_name_legacy`
  dial is only a client-side fallback and now finds no listener.
- `main()` no longer promotes `WINMUX_*` env to `YMUX_*` or folds `~/.winmux` into `~/.ymux`
  (`adopt_legacy_env` / `migrate_legacy_home_dir` removed); the `WINMUX_*` read fallbacks stay,
  and the remote `~/.winmux` fold is the bootstrap's `migrate_legacy_remote_dir`.

## Read the source when

You need a verb's exact flags or output shape (or read `docs/CLI.md`, which is the
user-facing contract), the browser-automation selector semantics, or the hook JSON
schema in `hooks/*.json`.
