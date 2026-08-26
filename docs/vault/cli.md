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
- **Notes / settings** — `note`, `settings` (`show` / `set` / `preset`)
- **Ops** — `port-watch`, `session-meta`, `doctor`, `check-updates`
- **Dev** — `dev get-state`, `console-tail`, `debug-log-tail`, `report-bug`

## `hooks.rs` (730) — `ymux setup-hooks`

Registers agent hooks that point at the local `ymux` binary, so Claude Code (and codex,
gemini) pipe permission requests and lifecycle events back through the tunnel into the
desktop UI.

**`Notification` is registered again as of hooks v1.5.0.** v0.4.4 had dropped it as
observability-only; it is back with a different job — the CLI filters on
`notification_type` and pushes the type alone, and the desktop turns that into per-pane
agent state (see `backend-rpc.md`). Bumping `manifest.json`'s
`hooks.claude-code.version` is what tells existing installs to re-sync.

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

The hook spec used to be a hardcoded `&[(event, subcmd, matcher)]` slice. It now ships as
JSON at the repo root — `hooks/claude-code.json`, `hooks/codex.json`,
`hooks/gemini.json` — which the CLI fetches from raw.githubusercontent.com at install
time, with `~/.ymux/cache/hooks/` as a fallback and the bundled spec as the final
fallback. So a hook change reaches existing installs without a CLI rebuild.

## `port_watch.rs` — the LISTEN watcher

Runs on the remote. Every **1s** it reads `/proc/net/tcp` + `/proc/net/tcp6`; if the raw
bytes are identical to the previous tick it does nothing, otherwise it extracts LISTEN
sockets and diffs against the previous set. A new port sends `port.opened`, a vanished
one `port.closed`. **Both are detection-only on the desktop** — `rpc_server.rs` records
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

## `session_meta.rs` (818) — multi-machine session labels

`~/.ymux/session-meta.json` on the **server** maps a tmux session name to the Claude
session running inside it plus display metadata, so any ymux desktop connecting to that
server — home, office, laptop — labels the same sessions identically.

Written from two directions: the Claude `stop` hook inside the pane writes
`claude_session_id` + `claude_title` extracted from the transcript every turn, and the
desktop writes labels and origin. `origin` carries the desktop's `machine_id`
(`lib.rs::machine_id`) so the picker can say which machine created a session. Labels cross
the SSH exec **hex-encoded** (`--label-hex`, `lib.rs::hex_utf8`) so Hebrew never meets
shell quoting.

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
- The legacy `winmux-` pipe name is still answered by the app, so an old CLI on a PATH
  keeps working. Do not "finish the rename" here in isolation.

## Read the source when

You need a verb's exact flags or output shape (or read `docs/CLI.md`, which is the
user-facing contract), the browser-automation selector semantics, or the hook JSON
schema in `hooks/*.json`.
