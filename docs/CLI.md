# `ymux` CLI

A small client that talks to the running ymux app over JSON-RPC. The same
binary works on Windows (over a per-user named pipe) and on a remote Linux server
(over a reverse SSH tunnel + HMAC handshake — see [PROTOCOLS.md](./PROTOCOLS.md)).

## Synopsis

```
ymux [--raw] [--quiet] <command> [args...]
```

| Global flag | Default | Effect |
|---|---|---|
| `--raw` | off | Print the JSON-RPC result as a single line of compact JSON instead of pretty. |
| `--quiet` | off | On success, print nothing. Errors still go to stderr. |

## Transport selection

| Where | Default transport | Override |
|---|---|---|
| Windows (no `YMUX_SOCKET_ADDR`) | Named pipe `\\.\pipe\ymux-<USER>` | `YMUX_PIPE_PATH` |
| Anywhere with `YMUX_SOCKET_ADDR` set | TCP to that address; HMAC handshake using `YMUX_TUNNEL_TOKEN` | env vars only |
| Linux (no `YMUX_SOCKET_ADDR`) | the CLI tries to load `~/.ymux/run/last.env`; if it has the vars, TCP. Otherwise: error. | populate `last.env` (auto-done by SSH connect) or set the env vars manually |

## Exit codes (general)

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | client-side or server-side RPC error (message on stderr) |

`claude-hook` and `wait` override these — see their sections.

## Commands

### `list-workspaces`

```
ymux list-workspaces
```

Print the full `WorkspacesFile` (see [CONFIG.md](./CONFIG.md#workspacesjson)).

```
$ ymux list-workspaces
{
  "version": 1,
  "active_workspace_id": "w_aaa",
  "workspaces": [ … ]
}
```

### `select-workspace`

```
ymux select-workspace --id <workspace_id>
```

Sets the active workspace. The app's sidebar and main view update live.

```
$ ymux select-workspace --id w_18abc
{ "ok": true, "active": "w_18abc" }
```

### `new-workspace`

```
ymux new-workspace --name <name>
                     [--type local|ssh] [--shell <path>]
                     [--host <host>] [--user <user>] [--port <port>]
                     [--key-path <path>]
                     [--cwd <path>] [--color <#hex>]
```

For `--type local` (default): pass `--shell` if you don't want the auto-detected
default (pwsh → powershell → cmd).

For `--type ssh`: `--host` and `--user` are required. `--port` defaults to 22.
`--key-path` is optional (the auth chain falls back to ssh-agent / `~/.ssh/id_*`
/ password).

```
$ ymux new-workspace --name "Local CMD" --type local --shell cmd.exe --color "#5cd87f"
{ "id": "w_18ac1...", "name": "Local CMD", … }
```

### `delete-workspace`

```
ymux delete-workspace --id <workspace_id>
```

Kills any running sessions in that workspace and removes it.

### `send`

```
ymux send --pane <pane_id> --data '<text>'
```

Writes raw bytes to a connected pane. The pane must already be connected
(via the GUI Connect button or by wiring up your own automation). Find pane IDs
via `ymux tree` or `ymux list-workspaces`.

```
$ ymux send --pane p_18abc_0 --data 'echo hello'
$ ymux send-key --pane p_18abc_0 --key enter
```

### `send-key`

```
ymux send-key --pane <pane_id> --key <name>
```

Translates a named key to the right escape bytes and writes them. Recognized:
`enter`, `tab`, `ctrl-c`, `ctrl-d`, `ctrl-z`, `ctrl-l`, `esc`, `backspace`, `up`,
`down`, `left`, `right`, `home`, `end`. Unknown keys are sent verbatim.

`send` and `send-key` return `agent_seq`, the pane's hook counter before the
write. Pass it to `wait --after-seq`.

### `wait`

```
ymux wait --for idle [--pane <id>] [--timeout <s>] [--after-seq <n>]
```

Blocks until the pane's agent ends its turn. Polls every 2 s. `--pane` defaults
to `$YMUX_PANE_ID`. `--timeout` defaults to 600 (range 1 to 86400).

Idle means `done`, `needs-input` or `failed`. The `state` field gives the actual
one, so a driver can tell a finished turn from a permission prompt.

The state comes from agent hooks. A pane no hook has reached has the state
`unknown` and is **never** reported as idle: the wait continues (the hook can
arrive later) and ends with exit code 5.

`done` stays lit after a turn. A `wait` right after `send` can see the previous
turn, so wait for idle before you send and pass the value that `send` returned:

```
s=$(ymux send --pane p --data $'run the tests\r' | jq .agent_seq)
ymux wait --for idle --pane p --after-seq "$s"
ymux read --pane p --lines 100
```

Output: `{ "pane_id", "outcome", "state", "seq", "waited_secs" }`.

| Code | `outcome` | Meaning |
|---|---|---|
| 0 | `reached` | the turn ended |
| 2 | (none, message on stderr) | no pane given, or three RPC failures in sequence |
| 3 | `timeout` | time ran out, the agent is still working |
| 4 | `unknown-pane` | no workspace holds this pane |
| 5 | `no-agent-state` | time ran out, no hook ever reached the pane |

### `read`

```
ymux read [--pane <id>] [--lines <n>]
```

Prints `{ "pane_id", "lines", "text" }` with the last `n` lines (default 200,
range 1 to 10000) as plain text. The text comes from the pane's tmux or zellij
session, so a pane with no multiplexer session is an error (exit 2). The text
goes to stdout only and is never logged.

### `worktree create`

```
ymux worktree create <name> [--workspace <id>] [--base <rev>] [--path <dir>]
```

Creates a git worktree on the new branch `<name>`, on the workspace's host.
`--workspace` defaults to the workspace of `$YMUX_PANE_ID`, and there is no
fallback to the active workspace. `--base` defaults to `HEAD`. `--path` defaults
to a sibling of the repository. The name allows letters, digits and `- _ . /`
and must be a legal git ref. Prints `{ "ok", "branch", "path", "worktrees" }`.

### `schedule`

```
ymux schedule add --name <n> --cron "<5 fields>" --prompt "<text>" [--cwd <dir>]
                  [--max-per-day <1-24>] [--precheck-timeout <1-600>] [--disabled]
                  [-- <precheck argv...>]
ymux schedule list
ymux schedule rm|enable|disable|history <id>
ymux schedule [--port <1-65535>] <verb> ...
```

Scheduled prompts on the host's `ymux-server` (daemon 2.21.0 and later). The
daemon starts `claude <prompt>` in a new tmux session on the cron schedule, also
when the desktop is off. Run these verbs on the remote host. They call the local
daemon on `127.0.0.1:<port>` with `~/.ymux/server/token`, not the desktop.
`--port` is the daemon's `--port`, default 7879. The daemon does not write its
port to a file, so set `--port` when the daemon runs on another port.

- `--cron` is 5 fields in the host's local time. The daemon does not catch up
  firings it missed while it was down.
- `--cwd` defaults to the current directory. The prompt must not start with `-`.
- The precheck argv after `--` runs in `--cwd` before each run. A non-zero exit
  or a timeout skips the run. The daemon discards its output. On a timeout the
  daemon kills the precheck's whole process group.
- A run gets policy `gate`: each permission request waits for a human, and a
  request that nobody answers is denied. The daemon never auto-approves a run.
- The daemon skips a firing while the previous run is still working: its tmux
  session exists and its claude has not finished its turn (hook state `done`,
  `failed`, or `needs-input` after the turn). A run with no hook state yet counts
  as working. A finished session stays open for you to read; the daemon does not
  close it. The daemon also skips after `--max-per-day` runs on that local day
  (default 4).
- `history` prints the last 50 firings, newest first: `start`, `outcome`
  (`ran`, `skipped-precheck`, `skipped-overlap`, `skipped-cap`, `failed`),
  `session`, `exit_code` (of the precheck) and `reason`.

Example: a nightly triage that runs only when the remote has new commits.

```
ymux schedule add --name triage --cron "0 3 * * *" --cwd ~/repo \
  --prompt "Triage the new issues and write a summary" \
  -- sh -c 'git fetch -q && test -n "$(git log HEAD..@{u} --oneline)"'
```

### `notify`

```
ymux notify --title <title> [--body <body>] [--workspace-id <id>]
```

Show a Windows toast and add to the in-memory notifications list.

```
$ ymux notify --title "build done" --body "all green"
{ "ok": true, "id": 0 }
```

### `tree`

```
ymux tree [--workspace-id <id>]
```

Print the layout tree of one workspace (active by default).

```
$ ymux tree
{
  "workspace_id": "w_aaa",
  "name": "runner1",
  "layout": { "kind": "pane", "pane_id": "p_…", "connection": { … } }
}
```

### `set-status`

```
ymux set-status --pane <pane_id> --text <text>
```

Set a transient status text on the pane's header in the UI.

### `claude-hook`

```
ymux claude-hook <subcommand> [< stdin-json]
```

Designed for use as a hook command from an AI agent (Claude Code, etc).
Reads JSON from stdin, sends a `feed.push` over the tunnel, and waits for the
user's decision (for blocking subcommands) or returns immediately
(for passive ones).

#### Subcommands

| Subcommand | Behavior |
|---|---|
| `tool-permission` | **Blocks** until Allow / Deny / timeout. |
| `pre-tool-use` | **Blocks** until Allow / Deny / timeout. In `auto` mode it prints nothing and exits 0, so Claude Code's own classifier decides. |
| `session-active`, `session-stop`, `session-idle`, `notification`, `prompt-submit`, `session-end` | Passive: emits a feed card + toast; returns immediately. |
| `session-start` | Phase 105.C. Makes no feed card. It asks the app for this session's context over `context.inject`, waiting `YMUX_CONTEXT_TIMEOUT_MS` (default 1000 ms). If the answer is non-empty, it prints one `hookSpecificOutput.additionalContext` JSON line on stdout. On any error it fails open: prints nothing and exits 0. |
| Any other name | Treated as passive. |

#### stdin payload (suggested fields — all optional)

- `tool` (e.g. `"Bash"`)
- `command` (e.g. `"rm -rf /tmp/test"`) — used as the title for tool-permission
- `summary` / `description` / `body` / `reason` — first non-empty becomes the
  card's body
- `wait_timeout_seconds` (1..600) — server-side wait before timing out

If the payload doesn't have any of those, the title is `agent: <subcommand>` and
the summary is the JSON-stringified payload truncated to ~280 chars.

#### Exit codes

| `decision` | Exit | Meaning |
|---|---|---|
| `allow` / `passive` | 0 | proceed |
| `deny` | 1 | abort |
| `timeout` | 2 | timed out without decision |
| anything else / wire error | 3 | look at stderr + `%APPDATA%\ymux\debug.log` |

#### Examples

Block on tool permission with a 5 s timeout:

```bash
echo '{"tool":"Bash","command":"rm -rf /tmp/test","wait_timeout_seconds":5}' \
  | ~/.ymux/bin/ymux claude-hook tool-permission
echo "exit=$?"
```

Passive notification:

```bash
echo '{"summary":"Claude finished thinking"}' \
  | ~/.ymux/bin/ymux claude-hook session-idle
```

## Where each command works

| Command | Local (Windows pipe) | Remote (TCP via tunnel) | Requires connected session? |
|---|---|---|---|
| `list-workspaces`, `tree`, `select-workspace`, `new-workspace`, `delete-workspace`, `notify`, `set-status` | yes | yes | no |
| `send`, `send-key`, `read` | yes | yes | **yes** — pane must be connected |
| `wait`, `worktree create` | yes | yes | `wait`: no. `worktree create`: an SSH workspace needs a live session to its host |
| `schedule` | no | yes, on the host itself | no: it calls the host's `ymux-server`, not the desktop |
| `claude-hook` | yes (will use the local pipe) | yes (the typical use) | no for passive; for blocking, the app must be running and the user must respond |

## `ymux dev` — introspection (Phase 8.E)

A small developer-facing subcommand tree for inspecting the running app and
producing bug reports. Useful for debugging your own setup and for handing
support a complete state snapshot in one file.

### `ymux dev get-state [--text]`

Snapshot of the app's current in-memory + on-disk state. JSON by default,
suitable for piping to `jq`. Pass `--text` for a short human summary.

The JSON has these top-level fields:

- `version`, `git_hash`, `build_time_unix` — what binary you're talking to.
- `appdata_dir` — where state files live.
- `workspaces` — `{ count, active_id, by_id: { ws_id: { name, pane_count,
  kind_breakdown: { terminal, browser } } } }`.
- `sessions.active` — every connected pane: `{ pane_id, kind, connection_type,
  workspace_id }`.
- `tunnels.forwards` — open SSH local-port forwards (Phase 8.B): `{
  workspace_id, remote_port, local_port }`.
- `feed` — `{ open, done, by_kind }`.
- `notes` — `{ open, done, by_tag }`.
- `log_tail` — last 50 lines of `<appdata>/ymux/debug.log`.
- `console_tail` — last 50 captured frontend console events (errors + warns).

### `ymux dev console-tail [-n N]`

Last N (default 50) frontend console events. Each entry is `{ level, message,
ts }`. The frontend wraps `console.error` and `console.warn` to forward into
this ring buffer (capped at 200) without breaking original console output.

### `ymux dev debug-log-tail [-n N]`

Last N (default 50) lines of `<appdata>/ymux/debug.log`. Same as
`Get-Content -Tail N` against the log, but works through the running app's
RPC so you don't need to know the path.

### `ymux dev check-updates [--pretty]`  *(Phase 9.B)*

Polls the manifest URL configured in `settings.updates.manifest_url`,
returns the parsed `UpdateInfo` with `current_version`, `latest_version`,
`available`, `notes_url`, `msi_url`, `released_at`, and `last_check_iso`.
Persists `last_check_iso` and `last_seen_version` into `settings.json`. If
the manifest URL is unreachable, returns `error` rather than failing.

### `ymux dev report-bug [--description "..."] [--repro-steps "..."]`

Captures a bug report at `<appdata>/ymux/bug-reports/bug-<unix>.json`
containing `{ description, repro_steps, captured_at_unix, state }` where
`state` is the full `dev get-state` output (with larger log + console tails).

If `--description` is omitted, reads from stdin until EOF (Ctrl-Z + Enter on
Windows, Ctrl-D on Unix). Prints the saved file path on success.

## `ymux settings` — read/modify persisted settings  *(Phase 9.A)*

Wrapper around the `settings.*` RPC methods. Useful for theming from a
script or for CI snapshots of a known-good config.

| Subcommand | What it does |
|---|---|
| `ymux settings show [--json]` | Print the full settings JSON (pretty by default). |
| `ymux settings set --key <dotted.path> --value <v>` | Patch a single field, e.g. `--key theme.preset --value dracula` or `--key terminal.scrollback_lines --value 8000`. |
| `ymux settings preset <name>` | Apply a built-in theme preset (`tokyo-night`, `dracula`, `solarized-dark`, `nord`, `solarized-light`). |
| `ymux settings presets [--json]` | List available presets with their swatch colors. |
| `ymux settings export --output <file>` | Write the current settings to a file. |
| `ymux settings import --input <file>` | Replace settings with the file contents (full overwrite). |

All mutations emit `settings:changed` so the running app re-applies the
theme live.

## `ymux-mcp` — MCP server for agents (Phase 8.F.4)

A standalone stdio MCP server that lets MCP-aware agents (Claude Code,
Cursor, Cline, etc.) drive ymux's browser panes natively. Each tool call
becomes a JSON-RPC request through the local named pipe to the running
ymux app.

### Setup

After building or installing ymux, register it with your agent. For
Claude Code, edit `~/.claude/mcp.json` (or the project-local equivalent):

```json
{
  "mcpServers": {
    "ymux": {
      "command": "C:\Users\<user>\Documents\programing\ymux\app\src-tauri\target\release\ymux-mcp.exe"
    }
  }
}
```

If installed via the MSI: `C:\Program Files\ymux\ymux-mcp.exe`.

**Upgrading from winmux:** an existing `mcp.json` still names
`winmux-mcp.exe` under `C:\Program Files\winmux\`. That keeps working
without an edit — the old exe is still on disk (the rename installs
side by side, see docs/RELEASING.md) and the app answers on the
pre-rename pipe name too. Point it at `ymux-mcp.exe` when convenient;
the compatibility listener goes away one release after 0.5.0.

The ymux desktop app must be running. Each tool call opens a fresh
named-pipe connection to `\.\pipe\ymux-<USER>` and closes after the
response. Set `YMUX_PIPE_PATH` to override the pipe path.

### Tools exposed

Discovery: `list_workspaces`, `tree`.

Browser navigation: `browser_navigate`, `browser_url`, `browser_history`,
`browser_go_back`, `browser_go_home`.

Browser automation (via the postMessage iframe bridge): `browser_eval`,
`browser_click`, `browser_type`, `browser_find`, `browser_snapshot`,
`browser_wait_for`.

Agent affordances: `notify`, `note_add`.

Each tool's `inputSchema` mirrors the matching CLI subcommand's flags. See
`ymux dev` and the per-subcommand `--help` output for argument shapes.

### Manual protocol probe

The server speaks newline-delimited JSON-RPC 2.0 over stdio. Test without
an agent:

```pwsh
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | ymux-mcp.exe
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | ymux-mcp.exe
echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_workspaces","arguments":{}}}' | ymux-mcp.exe
```

The first response carries `serverInfo` + `capabilities.tools`. The
second lists 15 tool definitions. The third returns the live
`WorkspacesFile` from the running app.
