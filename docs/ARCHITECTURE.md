# Architecture

## The model

ymux organizes terminal work into a tree:

```
window  →  workspace  →  layout (binary tree of splits)  →  pane (leaf)  →  session (PTY or SSH channel)
```

A **workspace** is the persistent unit (named, color-coded, with an SSH/local connection
template). Its **layout** is a binary tree where each node is either a `pane` (leaf — has
its own connection + session) or a `split` (horizontal/vertical divider with a ratio and
two children). A **pane** is the visible cell on screen; its **session** is the running
PTY (local) or SSH channel (remote) that backs it. Sessions are ephemeral — only
workspaces and their layouts are persisted.

## Top-level processes

| Process | Path | Role |
|---|---|---|
| `app.exe` | `src-tauri/target/debug/app.exe` | The Tauri app: WebView2 frontend (SolidJS) + Rust backend (PTY/SSH/RPC). Long-running. |
| `ymux.exe` | same dir | Windows-side CLI client. Talks to the running app over a named pipe. |
| `ymux-linux-x64` | bundled in resources, copied to `~/.ymux/bin/` on the SSH server | Linux CLI client. Talks to the Windows app via a reverse SSH tunnel. |

## Communication channels

```
┌──────────────────────────────────────────┐
│             Windows host                 │
│                                          │
│  ┌────────────┐   Tauri commands +       │
│  │  SolidJS   │   events (in-process)    │
│  │  Frontend  │ ◄══════════════════════╗ │
│  └─────┬──────┘                        ║ │
│        │ xterm.js writes               ║ │
│        ▼                               ║ │
│  ┌────────────┐ pty:data / pty:exit    ║ │
│  │            │ feed:item-added /      ║ │
│  │   Rust     │ resolved / pane:status ║ │
│  │  backend   │ ──events──►            ║ │
│  │ (lib.rs)   │                        ║ │
│  └─┬─┬─┬─┬────┘                        ║ │
│    │ │ │ │                             ║ │
│    │ │ │ └─ portable-pty ──► local shell processes (ConPTY)
│    │ │ │                                 │
│    │ │ └─ local RPC endpoint ◄── ymux (local CLI)
│    │ │      win:  \\.\pipe\ymux-<user>   │
│    │ │      unix: $TMPDIR/ymux-<user>.sock
│    │ │            (fallback <config>/rpc.sock)
│    │ │                                   │
│    │ └─ russh ──── SSH ──┐                │
│    │                    │                │
│    └─ tunnel.rs bridge ◄┤                │
│       (forwarded-tcpip) │                │
└─────────────────────────┼────────────────┘
                          │
                          │ SSH
                          │
┌─────────────────────────┼────────────────┐
│            Remote Linux server           │
│                         │                │
│        sshd ────────────┘                │
│         │                                │
│         ├── shell (with YMUX_*         │
│         │    env vars + last.env file)   │
│         │                                │
│         ├── 127.0.0.1:<remote_port>      │
│         │   (reverse-forwarded to the    │
│         │    desktop app via russh)      │
│         │     ▲                          │
│         │     │  TCP                     │
│         └─────┴── ymux-linux-x64       │
│                   (~/.ymux/bin/)       │
└──────────────────────────────────────────┘
```

- **Frontend ⇄ backend (in-process):** SolidJS calls Rust via `invoke('cmd_name', {...})`
  for synchronous-style RPC. Backend pushes async updates via `app.emit('event:name', ...)`
  which the frontend subscribes to with `listen()`. PTY data flows backend → frontend
  exclusively as events; user keystrokes flow frontend → backend as `pty_write` invokes.
- **CLI ⇄ app on Windows:** newline-delimited JSON-RPC v2 over a per-user Named Pipe at
  `\\.\pipe\ymux-<user>`. ACL is whatever Windows assigns by default to a pipe
  created by the user's process — effectively user-only. No HMAC needed (the pipe
  ACL is the auth boundary).
- **Local RPC endpoint on macOS/Linux:** the same newline-delimited JSON-RPC, over a
  **Unix domain socket** instead (`winmux_core::pipe_name()`). Path is
  `$TMPDIR/winmux-<user>.sock`; `$TMPDIR` is a per-user private directory on macOS, so
  it carries the same user isolation the per-user pipe name gives on Windows. If that
  path can't be bound — `sockaddr_un.sun_path` is capped at 104 bytes on macOS — the
  server falls back to `<config_dir>/rpc.sock` and `winmux-tunnel` tries the same two
  paths in the same order (`winmux_core::pipe_names()`). A total bind failure is
  reported via `doctor`'s `rpc_server.bind_error`, because it silently disables every
  feature that rides this transport (port detection, CLI hooks, tunnel RPC) while SSH
  panes keep working and hide the cause. The server is a single `UnixListener` — the
  254-instance / ERROR-231 pool machinery is Windows-only.
- **CLI on remote ⇄ desktop app:** TCP via a russh **reverse tunnel** opened on
  every SSH workspace connection. The CLI dials `127.0.0.1:<remote_port>`; the SSH
  session forwards that to a russh client channel; our `tunnel.rs` bridges the channel
  to a fresh client connection on the local RPC endpoint (pipe or socket, per above).
  Auth is **HMAC-SHA256 challenge-response** (Phase 6.4) — the shared token never
  travels in cleartext.
- **Agent hooks:** Claude Code and Grok Build both invoke the Linux CLI's `claude-hook <subcommand>`, which builds a `feed.push`
  JSON-RPC request from stdin, sends it through the tunnel, and (for blocking kinds)
  waits for a server-side decision before returning an exit code.

## Persistence

| File | Path | Written by |
|---|---|---|
| `workspaces.json` | `%APPDATA%\ymux\` | `lib.rs` on every mutation; loaded once at `setup()`. |
| `known_hosts.json` | `%APPDATA%\ymux\` | `lib.rs::SshClient::check_server_key` on first/match/replace. |
| `debug.log` | `%APPDATA%\ymux\` | `dlog()` everywhere. Append-only. |
| `last.env` | `~/.ymux/run/` (remote) | Written by tunnel bootstrap before the shell starts; CLI on Linux loads it as a fallback when sshd strips per-channel env vars. |
| `session-meta.json` | `~/.ymux/` (remote) | Phase 81 multi-machine sync: tmux session → `{claude_session_id, claude_title, auto_name, label, origin, updated_at}`. Written by the Linux CLI (`session-meta` subcommand + the Claude `stop`/`session-end`/`user-prompt-submit` hooks); read by the desktop in the same SSH roundtrip as `tmux list-sessions`. Atomic tmp+rename, last-writer-wins, lazy-pruned against `tmux ls` on every write. **`claude_title` vs `auto_name`:** the title churns (Claude rewrites its own summary, re-read every turn); `auto_name` is the stable identity — `"<two words> · <YYYY-MM-DD HH:MM>"` derived once from the session's first real prompt by the `user-prompt-submit` hook, re-derived only when a NEW Claude session takes over the same tmux key. Display precedence: `label > auto_name > claude_title > raw name`. |
| `machine-id` | `%APPDATA%\ymux\` | Phase 81: stable per-install id (`<COMPUTERNAME>-<4hex>`), the `origin` value for sessions this machine creates. Deliberately outside settings.json so "Reset settings" keeps identity. |

## Bundled resources

| Resource | Origin |
|---|---|
| `resources/ymux-linux-x64` | Cross-compiled by `scripts/build-linux-cli.ps1` (musl, static-PIE ELF). Bundled into `app.exe` via `tauri.conf.json` `bundle.resources`. Auto-uploaded to remote on first SSH connect by `remote_bootstrap.rs`. |
| `resources/remote-manifest.json` | Generated next to the binary. Carries the SHA-256 + size + build timestamp. The manifest path/hash schema is keyed by triple (`x86_64-linux`; `aarch64-linux` reserved for later). UTF-8 **without** BOM (writer hardened in 6.2). |

## Phase history

- 1: Local PTY pipeline.
- 1.5: BiDi via bidi-js (UAX #9).
- 2: SSH workspaces via russh.
- 3: Multi-workspace sidebar + persistence.
- 4: Splits (binary tree).
- 5: CLI + JSON-RPC over Named Pipe + agent hook stub.
- 6.1: CLI cross-compile to Linux musl.
- 6.2: Remote-bootstrap of the Linux CLI via SFTP.
- 6.3: Reverse SSH tunnel + per-pane TCP↔Pipe relay.
- 6.4: HMAC-SHA256 challenge-response (token never on wire).
- 6.5: Blocking agent hooks with `feed.push` + UI Allow/Deny.
