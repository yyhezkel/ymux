# Protocols

Wire formats. Anyone implementing a compatible client should be able to read this
and not need to crack open the source.

## JSON-RPC v2

All RPC requests and responses follow [JSON-RPC v2](https://www.jsonrpc.org/specification).
Messages are **newline-delimited** — exactly one JSON object per line, terminated
with `\n`. The server parses one line at a time; it does not support batching.

### Request shape

```json
{ "jsonrpc": "2.0", "id": 1, "method": "<name>", "params": { ... } }
```

`id` is echoed in the response. Notification-style requests (no `id`) are not used
in ymux.

### Response shape

Success:
```json
{ "jsonrpc": "2.0", "id": 1, "result": { ... } }
```

Error:
```json
{ "jsonrpc": "2.0", "id": 1, "error": { "code": -32700, "message": "parse error: ..." } }
```

### Error codes

| Code | Meaning |
|---|---|
| `-32700` | Parse error (malformed JSON line). |
| `-32000` | Method-specific error. The `message` carries the detail (e.g. `"no such session s17"`, `"pane p_xxx not connected"`, `"unknown method: foo"`). |

We don't currently emit `-32600/-32601/-32602` codes; everything that isn't a
parse error is `-32000`.

## Method catalog

The methods exposed by the JSON-RPC server (`rpc_server.rs::dispatch`).

### `list-workspaces`

Return the persisted workspaces file.

**Params:** `{}`
**Result:** the full `WorkspacesFile` (see [CONFIG.md](./CONFIG.md#workspacesjson)).

### `select-workspace`

Set the active workspace.

**Params:** `{ "id": "<workspace_id>" }`
**Result:** `{ "ok": true, "active": "<id>" }` and a `workspaces:changed` event
fires for the frontend.
**Errors:** `"no workspace <id>"`.

### `new-workspace`

Create a workspace with a single-pane layout.

**Params:**
```json
{
  "name": "Local PowerShell",
  "connection": { "type": "local", "shell": "pwsh.exe" },
  "color": "#7aa2f7",
  "cwd": "C:\\Users\\me\\projects"
}
```
The `connection` object follows the same schema as in `workspaces.json`.
**Result:** the newly created `Workspace` object (with its generated `id` + a fresh
single-pane layout). Side effect: the new workspace becomes active.
**Errors:** `"bad params: <serde error>"`.

### `delete-workspace`

Remove a workspace and kill any of its sessions.

**Params:** `{ "id": "<workspace_id>" }`
**Result:** `{ "ok": true }`.

### `send`

Write raw text to a connected pane's session (no key translation, just bytes).

**Params:** `{ "pane_id": "<pid>", "data": "<utf-8 string>" }` (alias `"pane"` for `"pane_id"`)
**Result:** `{ "ok": true, "bytes": <count> }`
**Errors:** `"pane <id> not connected"`, `"no such session <sid>"`.

### `send-key`

Translate a named key into the right escape bytes and write it.

**Params:** `{ "pane_id": "<pid>", "key": "enter" }`
**Result:** `{ "ok": true, "bytes": <count> }`
**Recognized keys:** `enter` / `return` / `cr`, `tab`, `ctrl-c` / `^c`, `ctrl-d` / `^d`,
`ctrl-z` / `^z`, `ctrl-l` / `^l`, `esc` / `escape`, `backspace` / `bs`,
`up` / `arrow-up`, `down`, `left`, `right`, `home`, `end`. Unknown keys are sent
verbatim as bytes.

### `notify`

Show a Windows toast and append to the in-memory notification list.

**Params:** `{ "title": "...", "body": "...", "workspace_id": "..." (optional) }`
**Result:** `{ "ok": true, "id": <numeric_id> }`. Frontend receives a
`notification:new` event.

### `tree`

Inspect the layout of one workspace (the active one if `workspace_id` omitted).

**Params:** `{ "workspace_id": "<id>" (optional) }`
**Result:** `{ "workspace_id": "<id>", "name": "<n>", "layout": <LayoutNode> }` or `null`.

### `set-status`

Set a transient status text on a pane's header.

**Params:** `{ "pane_id": "<pid>", "text": "<text>" }`
**Result:** `{ "ok": true }`. Side effect: a `pane:status` event with the new text.

### `feed.push`

Push an item onto the agent feed. Used by `ymux claude-hook ...`.

**Params:**
```json
{
  "request_id": "req_<unique>",
  "kind": "permission_request" | "passive",
  "subkind": "tool-permission" | "session-idle" | ...,
  "pane_id": "<from YMUX_PANE_ID>",
  "workspace_id": "<optional>",
  "title": "Run `npm test` ?",
  "summary": "<long description>",
  "payload": { ... },
  "wait_timeout_seconds": 5
}
```

**Behavior:**
- `wait_timeout_seconds` is clamped server-side to `1..=600`. Default 120.
- For `kind == "permission_request"`: the call **blocks** for up to that many seconds
  while a oneshot waits on a UI decision.
- For other kinds: returns immediately with `decision: "passive"`.
- Side effects: `feed:item-added` event, Windows toast, and for blocking items a
  `feed:item-resolved` event when the verdict comes in.

**Result:** `{ "request_id": "<id>", "decision": "allow" | "deny" | "timeout" | "passive" }`.

### `feed.decide`

Provide a decision for a pending feed item. Same logic as the Tauri command
`feed_decide` (used by the frontend Allow/Deny buttons) — exposed over RPC so a
remote tool can also automate decisions.

**Params:** `{ "request_id": "<id>", "decision": "allow" | "deny" | "timeout" }`
**Result:** `{ "ok": true }`
**Errors:** `"unknown decision: <x>"`, `"missing request_id"`.

### `context.inject` (Phase 105.C)

Asked by `ymux claude-hook session-start`. The desktop answers with the context
to hand back to Claude Code as `hookSpecificOutput.additionalContext`. It is a
separate request/response method, not a `feed.push` reply field. SessionStart
never creates a feed item or a toast, so it has no reason to go through the
feed path.

**Params:** `{ "pane_id"?: "<YMUX_PANE_ID>", "tmux_session"?: "<name>",
"session_id"?: "<claude session id>", "source"?: "startup" | "resume" | "compact" | "clear" }`.
The pane is resolved with the same `resolve_hook_pane` rule `feed.push` uses.

**Result:** `{ "additional_context": "<text or empty>" }`. The text is empty in
any of these cases:
- Settings → General → *Inject context into sessions* is off.
- The source is `clear` or an unknown value.
- There is nothing to say.

When there is text, it starts with `[ymux-context]` and is at most 1536 bytes.
Its content depends on the source:
- `compact` / `resume`: the same shape as the Context Rail card. The lines are
  `Goal`, `Done when`, `Now: [status] task` and `Next`. Then come the last 5 ✔
  deltas, and finally `Original request` (the first prompt, ≤ 400 chars). A
  missing field is omitted.
- `startup`: the workspace intent plus one line per other open session in the
  workspace (task + status, ≤ 8).

The builder is `context_store::build_injection`, and `docs/CONTEXT.md` has the
details. Unknown to the Go daemon, which answers it with a JSON-RPC error. The
CLI treats that like a timeout: it fails open.

## Named Pipe transport (Windows local)

- **Name:** `\\.\pipe\ymux-<USERNAME>`. The user is `$env:USERNAME` if set, else
  `whoami::username()`.
- **Mode:** byte mode, `max_instances = 8`, `first_pipe_instance = false`.
- **Framing:** newline-delimited JSON-RPC v2 (above).
- **Auth:** none on the protocol level. The pipe ACL — the default Windows ACL of
  a pipe created by a user-mode process — restricts access to the same user. There
  is no HMAC or token because the pipe namespace is the auth boundary.
- **Override:** the CLI honors `YMUX_PIPE_PATH` if set.

## TCP transport (remote, via reverse SSH tunnel)

When an SSH workspace connects, the Windows app asks the server to forward a port
back via russh's `tcpip_forward("127.0.0.1", 0)`. The server picks a free port and
returns it. Each TCP connection that lands on that port is wrapped in a forwarded
SSH channel and delivered to our `Handler::server_channel_open_forwarded_tcpip`,
which hands it off to `tunnel.rs::bridge_to_pipe`.

- **Address:** `127.0.0.1:<remote_port>`. The CLI on the remote reads it from
  `YMUX_SOCKET_ADDR` (with a fallback `~/.ymux/run/last.env` file).
- **Auth:** HMAC-SHA256 challenge-response on the **first lines** of every TCP
  connection.

### Handshake

```
server → client : YMUX-CHALLENGE <hex 32-byte nonce>\n
client → server : YMUX-RESPONSE <hex HMAC-SHA256(token, nonce)>\n
server → client : YMUX-OK\n               # success
                  ─ or ─
                  YMUX-DENIED <reason>\n  # failure (channel closed)
```

- **Token:** 32 alphanumeric chars, generated per SSH session. Lives in
  `YMUX_TUNNEL_TOKEN`. Never sent on the wire.
- **HMAC:** `HMAC-SHA256(key=token_bytes, msg=nonce_bytes)`, 32 bytes →
  64 hex chars.
- **Verification:** constant-time via `hmac::Hmac::verify_slice`.
- **Timeout:** 10 s for both the challenge read and the response read on each side.
- **`YMUX-DENIED` reasons:** `bad-format` (response line didn't match
  `YMUX-RESPONSE <hex>`), `bad-mac` (HMAC verification failed).

After `YMUX-OK`, the same TCP socket is used as a pure transport. The CLI sends
exactly one JSON-RPC request and reads exactly one JSON-RPC response per
TCP connection. The server-side bridge `tokio::io::copy_bidirectional`s between
the SSH channel and a fresh Named Pipe client connection — so the actual JSON-RPC
server is the same single server that handles local CLI calls.

## Agent hook contract

A "Claude hook" or similar agent integration invokes the CLI as:

```
echo '<json-payload>' | ~/.ymux/bin/ymux claude-hook <subcommand>
```

The CLI reads the payload from stdin, parses it as JSON, and constructs a `feed.push`.

### Stdin payload shape

There is no enforced schema. The CLI does light heuristic introspection:

| Field looked at | Used for |
|---|---|
| `command` | `title` for `tool-permission` / `pre-tool-use` (`Run `<command>` ?`) |
| `tool` | fallback `title` (`Allow `<tool>` ?`) |
| `title` | direct `title` |
| `summary`, `description`, `body`, `reason` | first non-empty becomes `summary` (in that order) |
| `wait_timeout_seconds` | passed through to `feed.push`, clamped 1..600 |

If the payload doesn't include any of those, the title is `agent: <subcommand>`
and the summary is the JSON-stringified payload (truncated to ~280 chars).

### Subcommand → kind mapping

| Subcommand | `kind` | Blocking? |
|---|---|---|
| `tool-permission`, `pre-tool-use` | `permission_request` | yes |
| anything else (`session-active`, `session-stop`, `session-idle`, `notification`, `prompt-submit`, `session-end`) | `passive` | no |
| `session-start` (Phase 105.C) | — no `feed.push`; calls `context.inject` instead (≈300 ms, fail-open) and prints the hook JSON below | no |

`session-start`'s stdout is either nothing or exactly one line:
`{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"…"}}`.
That line is built with serde, so the text cannot break out of the JSON. Nothing
else in that branch writes to stdout. The budget is
`YMUX_CONTEXT_TIMEOUT_MS` (default 300, clamped 100–3000).

### Decisions and exit codes

| `decision` | exit code | meaning |
|---|---|---|
| `allow` | 0 | user clicked Allow |
| `passive` | 0 | non-blocking item, accepted automatically |
| `deny` | 1 | user clicked Deny |
| `timeout` | 2 | server timed out waiting for a decision |
| anything else / wire error | 3 | shouldn't happen; investigate `debug.log` |

The CLI also prints `claude-hook[<sub>] decision=<...>` to stderr for visibility.

## Bootstrap protocol (Phase 6.2)

Run on every SSH workspace connect, after auth, before opening the user's shell.

1. `uname -s -m` — match against `x86_64-linux` / `aarch64-linux`.
2. Read `resources/remote-manifest.json` (BOM-stripped) to find the matching
   triple's expected SHA-256 + relative path.
3. `echo $HOME` — for the absolute paths.
4. `sha256sum ~/.ymux/bin/ymux-linux-x64` — compare hashes.
5. If equal: refresh symlink `~/.ymux/bin/ymux → ymux-linux-x64`. Done.
6. Otherwise: `mkdir -p ~/.ymux/bin`, then SFTP-upload the binary
   (russh-sftp), `chmod 0755`, refresh symlink, `sha256sum` again to verify.

Every step `dlog`s to `%APPDATA%\ymux\debug.log` for post-mortem.

## Env propagation (Phase 6.3)

When opening the SSH shell, the backend tries:
- `channel.set_env(false, "YMUX_SOCKET_ADDR", "127.0.0.1:<port>")` — best-effort.
  sshd often filters via `AcceptEnv`; failures are silent.
- `channel.set_env(false, "YMUX_TUNNEL_TOKEN", "<token>")` — same.
- `channel.set_env(false, "YMUX_PANE_ID", "<pane_id>")` — same.

And then writes a heredoc-quoted file `~/.ymux/run/last.env` (mode 0600) with
the same three values. The Linux CLI runs `load_fallback_env_file` at startup,
which reads that file and `setenv`s any missing vars before doing transport
selection. If `YMUX_SOCKET_ADDR` is set after this load, the CLI uses TCP;
otherwise (and only on Windows) it falls back to the named pipe.
