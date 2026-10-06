---
vault: server-go
covers:
  - app/src-tauri/server/cmd/ymux-server/main.go
  - app/src-tauri/server/internal/agent/*.go
  - app/src-tauri/server/internal/api/*.go
  - app/src-tauri/server/internal/auth/*.go
  - app/src-tauri/server/internal/chat/*.go
  - app/src-tauri/server/internal/config/*.go
  - app/src-tauri/server/internal/core/*.go
  - app/src-tauri/server/internal/desktop/*.go
  - app/src-tauri/server/internal/files/*.go
  - app/src-tauri/server/internal/hooks/*.go
  - app/src-tauri/server/internal/insights/*.go
  - app/src-tauri/server/internal/logging/*.go
  - app/src-tauri/server/internal/logs/*.go
  - app/src-tauri/server/internal/push/*.go
  - app/src-tauri/server/internal/term/*.go
  - app/src-tauri/server/internal/workspace/*.go
  - app/src-tauri/server/go.mod
---

# `ymux-server` — the Go control-plane daemon

Runs **on the user's remote Linux box**, not on the desktop. ~12,100 lines across 13
`internal/` packages. Formerly `ymux-insights`; Phase 77 restructured it into subsystems
behind interfaces, and Phase 95 added the first piece of "ymux in the browser"
(`docs/WEB-DESIGN.md`): a real terminal, in `internal/term`.

## Read this first: the two blobs

`app/src-tauri/resources/ymux-server-linux-{x64,arm64}` are **committed binaries**, not
build output. `src/addons.rs` pulls them in with `include_bytes!`, so nothing in the
desktop build reads this Go source. **A Go change that skips the rebake is green in every
job and ships the OLD server to every remote.** `ci-windows.yml` has a gate that fails on
exactly that; the rebake path is "download the `ymux-server-linux` artifact from the CI
run, drop both files into `app/src-tauri/resources/`, commit them in the same change".
Manual build commands: `docs/ymux-server/README.md` § Build.

## Architecture: `core` is a leaf

```
main.go  →  wires config, auth, api, chat, hooks, insights, logs, files, push, term, workspace
            (and injects desktop.AskApproval into chat as a closure, so chat
             never imports desktop — same shape as SetPushLister)
core     ←  every subsystem imports it; it imports no sibling
api      →  imports subsystems; subsystems NEVER import api
```

`internal/core` (110 lines) holds the cross-subsystem interfaces and value types —
`AddrSink`, `HookResolver` / `HookTarget` / `RPCError`, `NotificationSender` and friends. It is a **leaf package
by rule**, and that rule is the concrete fix for the Phase-69 WS↔session↔hookRPC import
cycle that forced the old daemon into one flat `package main`. Same for `api`: the
dependency arrow points one way, so there is no cycle to break later.

## The packages

| Package | Lines | What |
|---|---|---|
| `agent` | 608 | Phase 99 (WEB-DESIGN B1): pure Go ports of the desktop's per-pane agent logic — traffic light, `[ymux-brief]` parser, hook copy, send-key table. Stdlib only, nothing imports it yet (see below) |
| `api` | 838 | HTTP front door: the mux, unauthenticated liveness + version negotiation, and each subsystem mounted behind auth middleware. `huma.go` holds the typed client-SDK surface |
| `auth` | 195 | `Bearer` middleware plus per-device scope grants (`scopes.go`) — a leaf, imports no sibling |
| `chat` | 2,953 | the biggest: Claude session runner, the engine↔substrate bridge, hook RPC, pairing, transcript parser, push, scopes, store |
| `config` | 469 | API token, filesystem paths, the log janitor (size cap + age prune), and the one-time data-dir migration |
| `core` | 110 | the leaf interface package |
| `desktop` | 290 | the daemon's only OUTBOUND client — dials the ymux desktop through the reverse tunnel (Phase 96) |
| `files` | 682 | the Files API (`/api/v2/files/*`) |
| `hooks` | 178 | the hook-RPC endpoint: localhost listener + the Phase-66 challenge/response, asking each `core.HookResolver` (chat, term) whose token signed it (Phase 100) |
| `insights` | 2,653 | sampler, store, Docker, the hygiene reaper, and the two Phase-84 rollups |
| `logging` | 599 | the unified `log/slog` handler |
| `logs` | 475 | per-client log storage and the SSE tail |
| `push` | 433 | self-hosted push over a long-lived WebSocket |
| `term` | 4,240 | tmux sessions + a binary WebSocket carrying a real PTY (95), the embedded diagnostic page (97), hook routing for browser-created sessions (100), their feed, gate and events socket (101), the small verbs, notes and port detection (102), browser workspaces with the agent layout verbs (103), session history (104), and serving the web bundle + the browser settings document (108) |
| `workspace` | 1,613 | the workspace pub/sub substrate and its WebSocket frame contract |

## `agent/` — the desktop's agent logic, ported (Phase 99)

WEB-DESIGN Phase B moves the brain to the daemon for browser clients, and B1 is the
part with no IO: four files, each a **port of Rust that still runs on the desktop**,
with the Rust tests translated under the same names.

- `state.go` ← `lib.rs` `PaneAgentState` / `AgentRunState::apply_hook`. `Run.ApplyHook(subkind,
  notificationType, now)` is the traffic-light table (incl. `stop-failure` → `failed`, mirroring Rust); `seq` bumps on every *mapped* hook,
  `StateSince` only on a real change, an unmapped notification is a full no-op.
  `Run.Event(paneID)` is the `pane:agent-run` payload with identical JSON keys (nil
  pointers → `null`, like the Rust `Option`). `now` is a parameter so tests pin it.
- `brief.go` ← `brief.rs`: `ParseBrief`, `BriefFromStop`, `PreBriefText`. `ClipChars`
  counts **runes** (Rust counts chars) so a Hebrew value is never cut mid-letter; key and
  marker matching are ASCII-only folds (`asciiLower` / `asciiEqualFold`), not
  `strings.EqualFold`, to match `eq_ignore_ascii_case`. `BriefEntry` is the `pane:brief`
  entry shape.
- `humanize.go` ← `rpc_server.rs` `humanize_notification`: the English/Hebrew copy
  character for character, pinned by golden tests (the Rust side has none — these are the
  cross-check). `session_duration_seconds` must be a whole non-negative number, as
  serde's `as_u64` demands; `json.Number` is accepted.
- `keys.go` ← `translate_key`; an unknown name passes through **lowercased**, as in Rust.

Nothing imports the package in B1 — that is deliberate: B2 (the hook listener for
browser-created tmux sessions) and B3 (`feed.push`/`feed.decide` + events) build on
tested logic. Until the desktop drops its own copy, **a change to either side must be
made to both**; the Rust functions carry the same note pointing here.

## Things worth knowing before you edit

**`workspace/frames.go`** — the WebSocket frame contract is **typed Go values** so
producers cannot drift from the published schema
(`internal/api/frames.schema.json` + `internal/api/asyncapi.json`). Discriminator is `"type"`,
chosen because kotlinx `@JsonClassDiscriminator`, TS tagged unions, and
AsyncAPI/JSON-Schema all default to it. Locked in S4.3 and canonical — no client is
pinned to anything else yet.

**`chat/bridge.go`** — the engine↔substrate bridge. The Claude runner was built for the
retired `/api/claude/*` WebSocket; the new workspace API is a **pure pub/sub substrate**.
For a `claude_chat` session the bridge lazily spawns a Claude process on the first
`user_input`, feeds stdin, and republishes its output (assistant text, tool use/result,
hooks, status) into the substrate.

**`chat/chat_hookrpc.go`** — chat's `core.HookResolver`: `MatchHookHMAC` finds the
mobile session whose token signed the nonce, `dispatchHook` answers its `feed.push`
(policy `auto`/`block`/`gate`, the phone approves). Since Phase 100 the handshake itself
lives in `hooks` (below); `hooks.ChallengeTag` emits
`YMUX-CHALLENGE` (WINMUX responses still accepted); the Rust half is `CHALLENGE_TAG` in
`ymux-tunnel`. **Flip both together.**

**`term/` (Phase 95) — the server-side terminal, and it owns no state.** This is the
package that lets a browser have a shell, and the two rules that shape it are worth
reading before you touch any of it:

- **Attach, never spawn a bare shell.** Every terminal handed out is `tmux attach`
  against a named session, so the session outlives every client and a dropped
  connection loses nothing. Same "attach means attach" rule the desktop follows
  (`docs/DECISIONS.md`, 2026-08-23).
- **tmux is the truth.** There is no session table here. A session exists because
  tmux says so, and its identity is its tmux name, not an id the daemon mints. That
  is what lets a desktop and a browser see one reality (`docs/DECISIONS.md` Q1).

The consequence people expect to find and do not: **no fan-out, no ring buffer, no
shared state.** Several clients on one session is tmux's own multi-client case, so each
WebSocket simply owns one `tmux attach` process and tmux does the mirroring. The
`workspace` package's `KindTerminal` constant is reserved and deliberately unimplemented
— PTY bytes must never enter that package's append-only SQLite event log.

When the PTY ends (the session was killed, or tmux detached this client) `attach.go`
sends `{"type":"exit"}` **and then a close frame with 1000**. Without that frame the
browser saw 1006, the same code as a dropped network — found live 2026-10-05.

The four REST ops (list/create/rename/kill) are **huma operations** (`term/huma.go`,
`RegisterHuma`; ids `term-list|create|rename|kill`), so they are in the generated OpenAPI
and `sdk-gen/ci-check.mjs` guards them. Auth is ONE point: `api/huma.go` `bearerMiddleware`
accepts the shared token or a device token (`tokenOK`), then `opScopes` requires
`shell:attach` of a device (the owner token bypasses scopes). Statuses: no/unknown token
401, device without the grant 403, create 201, name clash 409. The token is read from
the `Authorization` header only — no query-string token on these ops (that stays on the
raw attach/events WebSockets, which are out of OpenAPI; WS is described by `asyncapi.json`).
**Body caveat:** huma rejects unknown fields and a missing body by default, but the old raw
handlers ignored both (the browser also sends `pane_id`/`cmd`). So the create/rename bodies
are `required:"false"` with `additionalProperties:"true"` — do not tighten them, 422s would
break the page and web clients. The other term routes (feed, events, history, webapp, ...)
are still raw `gate`-guarded handlers; `service.go`'s `gate` **fails closed** (a Service with
neither shared token nor scope resolver rejects everything), unlike the workspace subsystem's
"no auth configured => open".

**`auth.ScopeShellAttach` is not in `AllScopes`, and that is the security design, not an
oversight.** `ParseScopes` fails open to `AllScopes` for `""`, `"all"` and anything
malformed, so every scope in that list is reachable by a device nobody ever restricted.
A leaked device token must not become a shell over the internet, so this one grant is
only ever held by a device whose stored scopes name it explicitly — which also means
every phone paired before Phase 95 keeps working and none of them gained a terminal.
`GrantableScopes` (what `ValidScope` reads) is the union; `AllScopes` (what the
fail-open default reads) is not. Keeping those two lists apart is the whole mechanism,
and `NormalizeScopes` will not collapse a list containing an opt-in scope to `"all"`.

`pty_linux.go` opens `/dev/ptmx` directly through `golang.org/x/sys/unix` rather than
pulling in `creack/pty`. Not style: a new dependency means new `go.sum` lines, and this
repo has no Go toolchain on the dev box (Rule #17) — `x/sys` was already in the module
graph via gopsutil. The ioctl numbers are asm-generic, identical on the only two targets
that ship. `pty_other.go` is a stub so the package still builds on a mac or Windows dev
box. Every tmux call is an argv array (Rule #3) and targets use tmux's `=name` exact-match
prefix, so killing `api` can never hit `api-staging`. The tests inject a fake runner —
the CI `go` job's ubuntu image has no tmux, and a test that shelled out to a real
multiplexer would be a flake generator anyway.

**Rule #1 is absolute here**: a PTY carries the user's shell content. Nothing in this
package logs bytes — the attach/detach lines carry the session name, two byte COUNTS and
a duration. `meta.go` reads `~/.ymux/session-meta.json` (the CLI owns writing it; the
daemon never writes, so there is no second writer racing the CLI's atomic tmp+rename) and
joins labels on with the `label > auto_name > claude_title > raw name` precedence.

**`term/hookreg.go` + `hookdispatch.go` (Phase 100, WEB-DESIGN B2) — the one piece of
state, and why it is allowed.** A session created through `POST /api/v2/term/sessions`
gets three SESSION-scoped variables (`tmux new-session -e`, which beat the desktop's
`set-environment -g`): `YMUX_SOCKET_ADDR` (the daemon's hook listener),
`YMUX_TUNNEL_TOKEN` (32 random bytes, the HMAC key) and `YMUX_PANE_ID` (`term_<16 hex>`).
(Chat `spawnEnv` likewise sets only the `YMUX_*` trio; the `WINMUX_*` duplicates are gone.)
So `ymux claude-hook` in that session dials the **daemon**, not the desktop. The
`HookRegistry` remembers token → session (in memory, keyed by name, following
rename/kill and pruned against every `list`) and is term's `core.HookResolver`. It is
the only state in a package whose rule is "tmux is the truth", kept as thin as possible:
a daemon restart starts it empty, which is accepted (DECISIONS 2026-10-04) because under
systemd a restart kills a daemon-started tmux server anyway, and a surviving session
falls back to `last.env` (the desktop) exactly as before.
- **That pruning trusts `Tmux.List()` completely** — an empty list empties the registry.
  `List()` therefore uses a TAB-separated `-F` template. It used `\x1f` until 2.4.2, and
  tmux 3.4 escapes a non-printable byte to the literal text `\037`, so on a real box every
  list was `[]` and each list call (the diagnostic page lists on load) silently cut every
  browser session's hooks (`auth denied: unknown-session`). A test pins "no control byte
  but TAB in `listFormat`"; `ValidName` rejects control characters, so a name cannot hold
  a tab, and `SplitN(…, 5)` keeps a tab inside the path.
- `new-session -e` needs **tmux ≥ 3.2**; `SupportsSessionEnv` asks `tmux -V` once.
  Older, or no listener address → the session is created exactly as before, no hooks.
- `hookdispatch.go` is the daemon's counterpart of the desktop's `feed.push` arms,
  folding each hook into the pane's `agent.Run` + `agent.BriefEntry` (the Phase-99 port):
  `pre-tool-use`/`notification` → `ApplyHook`; `user-prompt-submit` → turn start + clipped
  prompt; `stop` → `RecordTurn` + `BriefFromStop`; `stop-failure` → `StateFailed` (timer cleared, NO `RecordTurn`, state-only: early passive return like `notification`); `session-end` → run reset (seq+1) +
  `session_ended`. A hook whose `pane_id` or `tmux_session` is not the matched session's
  is denied. A permission request follows the session's **policy** (Phase 101, below).
  `ping` answers; any other method is a JSON-RPC error.
- `Snapshot()` is the test-facing form of the state; the events socket's `hello` is the
  served one.
- Rule #8: tokens never logged. Rule #1: hook logs carry pane id, subkind, state and
  seq — never the prompt, the reply or tool input.

**`term/feed.go` + `events.go` (Phase 101, WEB-DESIGN B3) — the feed, the gate, the
live channel.** `feedPush` now does what the desktop's `feed.push` does after folding:
- **Policy per session** (`hookEntry.policy`, DECISIONS 2026-10-05): `none` — the default,
  set at create (`{"policy":"gate"}` to change it) or later with
  `POST /api/v2/term/sessions/{name}/policy` — answers a permission request `allow` at
  once and makes **no card**, as the desktop's Auto does. `gate` makes a blocking card and
  waits for a decision: `wait_timeout_seconds` default 120, clamped 1–600; timeout →
  `"timeout"` (the CLI denies); killing or losing the session → `deny`. The wait runs
  with **no lock held** — `feedPush` copies what it needs under `r.mu` and releases it.
- **Cards** follow the desktop's rules: `user-prompt-submit` and `notification` never make
  one; the lifecycle subkinds (`stop`, `session-*`, `post-tool-use`, `subagent-stop`,
  `pre-compact`) are humanized, and a stop with a non-degraded brief shows `ask · rec`
  (else `delta`), clipped to 160. Text is rendered in **both languages at creation** and
  each subscriber gets its own (`?lang=he`), because there is no settings store to read
  one language from. `feedStore` keeps the last 50 in memory; the browser keeps history
  (IndexedDB, Phase C). First decision wins; a second `decide` returns false.
- **`GET /api/v2/events`** — one WebSocket for the box, same gate as the terminal
  (owner token or explicit `shell:attach`). First frame `hello` = the hydration the
  desktop does with `pane_agent_states` / `pane_briefs` / the feed list, plus each pane's
  session and policy. Then `feed:item-added`, `feed:item-resolved`, `pane:agent-run`,
  `pane:brief` — the desktop's event names and JSON, wrapped `{"type","data"}`. Client →
  server: `{"type":"feed.decide","request_id","decision"}`; the same as REST is
  `POST /api/v2/feed/{request_id}/decide`. The subscriber is registered before the
  snapshot is taken (an event can arrive twice, never zero times — clients dedupe by
  seq / request_id), and one that falls 256 frames behind is **dropped, not waited for**:
  it reconnects and re-hydrates. Lock order is hub → feed, never the reverse.
- **`api` `hooks/forward` drops `term_` panes.** The CLI forwards every pre-tool-use to it
  regardless of where the RPC went; for a browser session that made a second, dead card
  on the phone (FOLLOWUPS P2, closed).
- These routes are raw stdlib handlers, so they are not in the OpenAPI spec (only the four
  session ops above are).

**`term/verbs.go` + `notes.go` + `ports.go` (Phase 102, WEB-DESIGN B4) — the small
verbs.** `DispatchHook` falls through to `verb()` for the desktop's `dispatch()` arms a
CLI inside a browser session can call, each answering the JSON the CLI expects and
announcing the desktop's event on the events socket:
- `set-status` → `pane:status` {pane_id, text}; **only for the caller's own pane** (the
  feed.push defense). The text is in `hello.pane_status`.
- `notify` → `notification:new` with the desktop's `NotificationItem` plus `session`.
  In memory, capped at 200 (the desktop's list is unbounded — a leak on a daemon that
  runs for months). `DELETE /api/v2/notifications` clears → `notifications:cleared`.
- `note-add/list/update/done/delete` → `notes:changed` (no data; clients re-list). The
  desktop's `Note` JSON and `n_<hex>_<hex>` ids, in `<data dir>/notes.json`, tmp + fsync
  + rename (Rule #7); a file that will not parse is **moved aside, never overwritten**.
  `tag` is a `json.RawMessage` on purpose: `null` must mean "clear", and a pointer field
  would collapse it into "absent". REST for the UI: `GET/POST /api/v2/notes`,
  `PATCH/DELETE /api/v2/notes/{id}`.
- **Ports: the daemon detects them itself** (Yossi, 2026-10-05) — nobody starts a
  `ymux port-watch` for a browser. `ports.go` is `cli/src/port_watch.rs` ported with its
  test vectors: `/proc/net/tcp{,6}` once a second, parsed only when the raw bodies
  changed, LISTEN + loopback/bind-any only, ≥1024, not 22, not `YMUX_PORTFORWARD_EXCLUDE`,
  and never the daemon's own API or hook-listener port. Events `port-detected` {addr,
  remote_port, family} / `port-undetected` {remote_port} — the desktop's, minus
  workspace_id; `hello.ports` is the current set. A `port.opened`/`port.closed` RPC lands
  in the same set. Detection only — v1 forwards nothing.
- Wiring: `main.go` calls `SetDataDir` and `StartPortWatch` after `hooks.Start`, so the
  listener's port is known before the first scan.

**`term/webws.go` + `layout.go` + `agentverbs.go` (Phase 103, WEB-DESIGN B5) — the
browser's workspaces and agent automation.**
- **Where they live (DECISIONS 2026-10-05):** `<data dir>/web-workspaces.json`, owned by
  term — not `internal/workspace`'s SQLite. The verbs that change a layout run here, and
  writing into a sibling subsystem's store would break the import rule; and every field
  added to the huma-described `Workspace` there is an SDK regeneration for the phone.
  Shape `{id, name, version, layout, tabs_mode, intent, is_project_root}` — the desktop's
  workspaces.json fields. REST: `GET/POST /api/v2/web/workspaces`,
  `GET/PUT/DELETE /api/v2/web/workspaces/{id}`. **PUT carries `version`**; a stale one gets
  **409 with the current document** (last-writer-wins with a guard, no CRDT — §10).
  Every change → `workspaces:changed` {workspace_id, version}.
- **Layout is the desktop's `LayoutNode` JSON, stored opaque.** `layout.go` handles it as
  generic maps so unknown fields (connection, color, a field added next year) survive a
  daemon-side edit. Only three operations exist: find a leaf, split a leaf (the
  desktop's `split_pane_in`: `{first: leaf, second: new, ratio 0.5}`, `sp_<hex>_<hex>`
  ids), set/clear a leaf's title or annotation. Everything else is the browser's (§4).
- **A pane is a tmux session.** Its leaf's `pane_id` is the session's hook pane id
  (`term_<hex>`); `POST /api/v2/term/sessions` takes `workspace_id` (must exist) and
  answers `pane_id`. `hookEntry.workspaceID` is the membership.
- **Agent verbs** (`agentVerb`, after B4's `verb` in `DispatchHook`): `tree` (defaults to
  the caller's workspace; `null` when none), `ui.tree`, `split`/`action.split`,
  `send`/`send-key`/`action.send_keys`, `set-pane-title`/`set-pane-annotation`,
  `pane.scrollback`. Rules decided for the daemon (Yossi): **send and the title verbs only
  reach the caller itself or panes of its own workspace** (the desktop has no fence);
  **`pane.scrollback` stays the desktop's error stub**, text identical (Rule #1).
  `send` goes through `Tmux.SendBytes` = `send-keys -t =name: -H <hex bytes>` — exact
  bytes, no key parsing, chunked; the trailing `:` makes it a PANE target (a bare
  `=name` is rejected by pane commands — verified on tmux 3.4).
- **`split` creates the session itself** (the desktop leaves that to its frontend; a
  browser may not be connected): same workspace, same policy, the source pane's
  `pane_current_path`, via `spawnSession` — the one place a session is born, shared with
  the create route. A session that could not get hook routing is killed rather than left
  unaddressable, and a layout write that fails kills the new session too. The reply adds
  `pane_id` + `session` to the desktop's `{ok, workspace_id, split_from}`.
- `writeFileAtomic` / `loadJSON` here are shared with `notes.go` (tmp + fsync + rename;
  an unparsable file is moved aside, never overwritten).

**`term/history.go` (Phase 104, WEB-DESIGN §4.2 / B6) — session history.** Reads, never
writes, the CLI's `session-meta.json` (which now keeps ended rows with `ended_at` + `cwd`,
see the CLI vault page) and Claude Code's transcripts:
- `GET /api/v2/term/history` — rows with `ended_at` AND a `claude_session_id`, newest
  first, minus any whose tmux name is live again (the CLI may not have re-pruned yet).
- `GET /api/v2/claude/sessions/{id}/transcript?offset=&limit=` — found by globbing
  `~/.claude/projects/*/<id>.jsonl`, so the cwd is not needed. The id must be a **UUID
  before it touches a path or an argv**. Turns are the user's prompts, Claude's text and
  one `{role:"tool", tool}` marker per call; tool results, thinking, sidechain (sub-agent)
  and `isMeta` lines are left out. Page default 200, max 1000. **Rule #1: logs carry the
  id, byte count and turn count only.**
- `POST /api/v2/term/history/{name}/resume` {workspace_id?, policy?} — `spawnSession`
  with a command: `tmux new-session … -- <claude abs path> --resume <id>` (argv, no shell
  — `Tmux.Create` takes an optional `cmd`, verified on tmux 3.4 that `$(id)` arrives
  literally). The row's own name is reused when free, so the CLI's next prune flips it
  back to live; else a name is minted. When claude exits the session ends and the row is
  history again. `claude` is resolved to an absolute path by the daemon (its PATH was
  augmented at start), so the tmux server's PATH does not matter.

**Caller-chosen pane ids (Phase 109, 2.10.0).** `POST /api/v2/term/sessions` takes an
optional `pane_id`; `spawnSession` passes it to `mint`, so the session's
`YMUX_PANE_ID` (and every hook it reports) is the browser layout leaf's own id instead of
a minted `term_<hex>`. `ValidPaneID`: 1–64 of `[A-Za-z0-9_-]` (it lands in an env var
and in hook payloads), else 400; an id a live session already carries → 409. Agent
splits and resumes still mint.

**Hook routing survives a restart (Phase 111, 2.12.0, `term/recover.go` + `hooks.Start`).**
The registry is in memory, so a restart (every add-on update) used to leave browser sessions
running with dead hooks — refused as unknown, no light, feed or gate. Two halves fix it:
`hooks.Start(portFile, …)` re-binds the port recorded in `<data dir>/hook-port` (0600) before
falling back to an ephemeral one — `YMUX_SOCKET_ADDR` is frozen in the environment of every
process already running in a session, claude included, so a new port would orphan them all.
Then `Service.RecoverHooks()` (main.go, after `SetDataDir`) reads each tmux session's own
environment (`Tmux.Environment` = `show-environment`): a session whose `YMUX_SOCKET_ADDR`
is this listener, with a 64-hex token and a valid pane id not already registered, is added
back with `YMUX_POLICY` (written at create, and by `SetPolicy` via `set-environment`) and
`YMUX_WORKSPACE_ID` (dropped if that workspace is gone). **Desktop sessions carry the same
variable names pointed at the desktop's tunnel and are never claimed.** No new store and no
token on disk: tmux already holds it for the hook processes; the env is never logged (only
counts). Sessions created before 2.12.0 recover with policy `none`. Tests: `term/recover_test.go`, `hooks/port_test.go`; every test that
starts the listener passes `""` as the port file (ephemeral, nothing recorded).

**Session argv (Phase 110, 2.11.0).** The create body also takes `cmd` — an argv the
session runs instead of a shell (a browser pane opened in "claude" mode). `sessionArgv`
bounds it (≤ 32 args, ≤ 4096 bytes each, no NUL, non-empty argv[0]) and resolves a bare
`claude` to the daemon's absolute path; it lands after `--` in `tmux new-session`, never
in a shell (Rule #3).

**Browser-pairing approve validates scopes (Phase 113, 2.13.0).**
`POST /api/pairing/requests/{id}/approve` used to store the request's `scopes` list
verbatim; it now goes through `auth.NormalizeScopes` like `PUT /api/v2/devices/{id}/scopes`
(unknown names dropped, duplicates collapsed, an ordinary full set folded to "all").

**Insights auth (Phase 110).** The Insights routes (legacy `/current` … and
`/api/v2/insights/*`) were behind `auth.Bearer` — the shared token only — so a device's
`insights:read` grant existed but nothing honored it, and the browser's Monitor got 401.
`Server.insightsAuth` now lets the shared token through as before, and a valid device
token holding `insights:read` for **GET only**; docker actions and hygiene/kill (POSTs)
stay owner-only (WEB-DESIGN §7). Workspace routes still use `auth.Bearer`.

**Gate card text fallback (Phase 110).** `cardText` normally leaves a `pre-tool-use`
card alone — the CLI's title IS the approval prompt. But the CLI derives that title from
`payload.command` / `payload.tool`, while Claude Code sends `tool_name` + `tool_input`,
so it falls back to `agent: pre-tool-use` with the raw hook JSON as the summary (seen
live in the browser). Exactly that fallback is now humanized (`Claude wants to run: Bash`
/ the command); a title the CLI did derive is untouched.

**`term/webapp.go` (Phase 108, WEB-DESIGN C4) — the daemon serves the web bundle.**
`SetWebRoot(dataDir)` (main.go) points it at `<data dir>/www/current` — a directory or a
symlink to `www/<version>/`, holding the desktop's own vite build. Nothing here uploads or
versions it: the Phase D add-on will write it; until then it is copied by hand from the
ci-windows `ymux-web` artifact.
- `GET /` serves `current/index.html` (`Cache-Control: no-cache`, the app CSP:
  `'self'` scripts, `connect-src 'self' ws: wss:`, `frame-ancestors 'none'`) when it
  exists, **and the diagnostic page otherwise** — a box without a bundle is unchanged.
  `/diag` is always the diagnostic page.
- `GET /assets/{file...}` (immutable, a year — vite hashes those names) and
  `GET /fonts/{file...}` (a day). A name containing `..`, `\` or a leading-dot segment is
  a 404 before any stat.
- **No `/{path...}` catch-all, on purpose:** the shared mux carries method-less `/api/...`
  patterns, and a method-qualified catch-all beside them is a registration-time conflict
  panic in Go 1.22 routing.
- Public, like the diagnostic page: static code, no secrets, and the app's login screen is
  how a browser gets a token in the first place.

**`term/settings.go` (Phase 108, WEB-DESIGN C4) — the browser's settings.** Decided
2026-10-05: one document on the daemon, shared by every browser. `GET /api/v2/settings` →
`{version, settings}` (`settings` is `null` before the first save); `PUT` with
`{version, settings}` replaces it when `version` matches, else **409 with the current
document** (the web-workspaces guard). The document is the desktop's `Settings` JSON
stored **opaque** — it must be a JSON object, ≤ 256 KB, and no field is parsed, so a new
setting needs no Go change. `<data dir>/web-settings.json`, compact, tmp + fsync + rename.
Each save publishes `settings:changed` `{version, settings}` on the events socket (the
WebBackend unwraps it into the desktop's bare-`Settings` payload). Logs carry the version
and byte count only.

**`term/page.go` + `page.html` (Phase 97) — the diagnostic page, and it is the only
client this stack has.** A single embedded HTML file that walks the whole Phase 95 + 96
flow: request access → match the 6-digit code → approve in ymux → list tmux sessions →
attach a real terminal on xterm.js. It exists because both phases are unverified live
(Rule #14) and the alternative was answering "does the PTY work" by hand with
`websocat`.

Three decisions in it worth not undoing:

- **Embedded in the binary**, because a debugging tool with its own delivery mechanism
  is one you cannot use when delivery is what broke. This is **not** an answer to Q2
  (how the real web bundle ships) — a few KB of diagnostics and a 3 MB app are different
  questions. (Q2 was decided 2026-10-05 as the `ymux-web` add-on; the serving half is
  `webapp.go` below.)
- **xterm.js from a CDN, not embedded.** The committed server blobs are ~13 MB each and
  every rebake writes both into git history; +600 KB per rebake to save one CDN fetch is
  the wrong trade here. The page says so plainly when the CDN is blocked instead of
  showing an empty box. Since 2.4.2 it is the app's own packages (`@xterm/xterm` 6.0.0 +
  `@xterm/addon-fit` 0.11.0) from **jsdelivr, SRI-pinned**, and `page.go`'s CSP allows
  exactly `cdn.jsdelivr.net`. The original cdnjs URLs (xterm 5.3.0, xterm-addon-fit 0.8.0)
  were 404s — cdnjs has no fit addon at all — so before that the page never rendered a
  terminal. Bumping a package means a new URL **and** a new `integrity` hash.
  Two layout traps, both fixed in 2.4.2 and commented in place: xterm parks its
  char-measure span at `left:-9999em`, which in this `dir="rtl"` document made the page
  scroll 130,000 px sideways (`#termWrap` clips it); and the font is an explicit stack,
  because a bare `monospace` resolved to a proportional face in a stock headless Chrome.
- **`GET /{$}`, not `GET /`.** Exact-match for the root, so an unknown path still 404s.
  A catch-all that silently returns HTML is how a typo in an API path becomes an hour of
  confusion. `page_test.go` asserts it.

`POST /diag/log` is the other half of the point: **half the steps in this flow happen in
a browser**, so without a sink the daemon log shows a pairing request and then, minutes
later, a WebSocket, with nothing between. Browser lines land under `SRV:WEB` while the
daemon's own terminal work stays `SRV:TERM`, so the two sides of the story can be
grepped apart. It cannot require a credential — an unpaired page is exactly when its
lines matter most — so it is bounded instead: 2 KB body, 300-char detail, 120 lines a
minute process-wide (per-process rather than per-IP, because the thing being protected
is one log FILE and rotating source addresses would defeat a per-IP limit). The level is
chosen from a fixed set rather than passed through, and every string is stripped of
control characters: a newline from a browser would otherwise forge a line in the log,
which is how a log stops being evidence.

**The expected first failure is a 403, and the page says so.** A freshly approved
browser holds `"all"`, which does not include `shell:attach`, so listing sessions is
refused until an owner grants it — and there is no UI for that yet. The page detects
exactly that status and prints the `curl` that fixes it. The gate logs every refusal
with a reason (never the token, Rule #8) for the same purpose.

`page.html` is not counted by the vault gate (it hashes `.go`/`.rs`/`.ts`/`.tsx`/`.mjs`),
so the guard that it stays in step with the handlers is
`TestPageReferencesTheRoutesItCalls` — a renamed route would otherwise break the page
silently, since nothing else links the two.

**`desktop/` (Phase 96) — the direction that did not exist.** Until this package,
the daemon never dialled the desktop: every desktop→daemon call is a `curl` the
DESKTOP opens on an SSH exec channel (`pairing.rs::daemon_curl`), and the daemon's
only tunnel-facing code is a LISTENER (`chat/chat_hookrpc.go`) the CLI dials inbound.
Anyone reasoning about this system will assume the server can push; it cannot, and
this package is the narrow exception.

It speaks **exactly what the Linux CLI speaks** — same endpoint, same HMAC
challenge-response, same newline-delimited JSON-RPC — so it inherits an
already-deployed server side instead of adding a protocol. The Rust counterparts are
`cli/src/main.rs::perform_handshake` (the client half it mirrors) and
`crates/ymux-tunnel/src/lib.rs` (the server half it talks to). The desktop now
OPENS with `YMUX`; the client still mirrors whichever tag it is addressed in and
accepts either in the verdict, so a pre-flip desktop works unchanged. The tests run a Go implementation of the server half written
from the wire spec, so a drift in either direction fails in CI rather than on a box
where the only symptom is "the approval card never appears".

`Discover` reads `~/.ymux/run/last.env` — and for this caller the FILE is the primary
source, not the fallback it is for the CLI: the daemon is a service started
independently of any SSH session, so it never inherits `YMUX_SOCKET_ADDR`. It is
re-read on **every** call, never cached, because a reconnect moves the tunnel to a
different port. Rule #8: the token is a password — it is never logged and never put on
the wire, only an HMAC of the server's nonce.

**`chat/chat_browser_pairing.go` (Phase 96) — a browser asks, the desktop approves.**
The mobile flow runs desktop-first (ymux issues a one-shot, the QR carries it, the
phone redeems), which cannot work for a browser on a machine ymux is not running on.
So: the browser POSTs to a public `/api/pairing/request`, the daemon pushes a
**blocking `feed.push`** through `desktop/` — an ordinary ymux Allow/Deny card that
toasts with every panel closed — and on approval the row flips `requested` → `pending`,
after which the **unchanged** `/api/pairing/redeem` finishes the job. A device that
arrived this way is indistinguishable from one a QR produced.

Three things about it are load-bearing:

- **`redeemDevice` matches `status='pending'` only**, so a request nobody approved can
  never be exchanged for a credential. That is the whole security property, and it came
  free from the existing query. `TestRequestedRowCannotBeRedeemed` is its guard.
- **The code is a matching device, not a secret.** Six digits shown on both the browser
  and the card, so a human can tell their own browser from someone else's request
  arriving at the same moment. Authorisation is the owner token, never knowing a code.
- **Approval does not grant a shell.** An approved browser gets the ordinary `"all"`
  grant, which since Phase 95 excludes `auth.ScopeShellAttach`. "Is this browser mine?"
  and "may it run commands on my machine?" are different questions and do not share a
  button; the second is a separate act in the device list.

The request endpoint cannot require a credential, so it is rate-limited per IP with a
global cap on outstanding requests, and it **fails closed with a message** when no
desktop is reachable — a browser left polling a request no human will ever see is
worse than a refusal.

**`hooks/hooks.go`** — the hook-RPC endpoint. Phase 100 moved the protocol here from
chat, because two subsystems now mint hook tokens: it binds an ephemeral localhost port,
reports the address to every resolver that is a `core.AddrSink`, does the challenge
(`ChallengeTag`, mirroring the client's dialect), asks each `core.HookResolver` in turn
whose token produced the HMAC (`chat.SessionManager` for phone sessions, term's
`HookRegistry` for browser-created tmux sessions), and passes the one JSON-RPC request to
the matched `core.HookTarget`. A non-nil `*core.RPCError` goes out as a JSON-RPC `error`
object (code -32000, as the desktop's `rpc_server` does). `main.go` starts it after both
resolvers exist and **no longer only when chat.db opened** — a failed chat store must not
cost browser sessions their hooks. hooks → core, chat → core, term → core: still no
cycle.

**`insights/analytics.go`** (424) — `GET /analytics`, the Monitor's Analytics tab. It is a
separate endpoint from `/history` for two reasons, both of them about the transport.
Every desktop fetch is one `curl` over the workspace SSH session with `--max-time 6`
(`insights_fetch` in `addons.rs`), so **the whole screen has to come back in one
response** — N round trips for N series is not on the table. And `/history` is raw rows
with `LIMIT 2000`, which at the 5s sample interval is 2.8 hours; a "last 7 days" question
served from it would silently answer with the OLDEST 2.8 hours of the window. So the
aggregation is SQL, server-side, and the client only draws. Windows are clamped to
`[5 minutes, retentionDays]` — asking for more than the store keeps just renders a
half-empty chart. It is the first reader of `disk_samples` and `docker_samples`, which
the sampler was already writing: `AnalyticsDisk.GrowthBytes` is signed (last `used` minus
first, i.e. "/var grew 3 GB overnight") and `AnalyticsContainer.UptimePct` is the share of
samples in which the container was running, which a point-in-time `/docker` list cannot
tell you.

**`insights/claudeusage.go`** (443) — `GET /claude-usage`: what Claude Code actually spent
on this machine, read from the transcripts it already writes to
`~/.claude/projects/<encoded-cwd>/<session>.jsonl`. Every assistant line carries
`message.model` and a `message.usage` block with real token counts, **including the
5-minute/1-hour cache-write split** — kept separate because the two are priced
differently and collapsing them understates a long session. This is the only record of it
on the box: `claude -p /usage` reports subscription quota *percentages* with no history.

The rule to not break: **this endpoint counts tokens and never prices them.** The price
table lives in the desktop at `app/src/claudePricing.ts`, in one place, so a price change
is a one-file edit instead of a server rebake plus a matching edit in the Rust local
mirror. Token counts are facts; prices are a table that goes stale. Guard rails matter
here because nobody controls the size of `~/.claude/projects` — hundreds of MB is normal
— and this runs inside a 6-second curl, so lines are rejected on a `"usage"` byte scan
before the JSON decoder sees them.

**`insights/hygiene.go`** — detects the leaks Yossi hit: duplicate `ymux port-watch`
processes (one per workspace at most), **orphaned ones (ppid==1 for >60s — the SSH
channel died and nothing else ever kills an exec child; Phase 86.B)**, and orphaned
long-running `claude` sessions with no terminal. `PortWatchReaper` SIGTERMs duplicates +
orphans every 5 minutes; claude sessions are only ever flagged. `POST /hygiene/kill`
accepts only pids the daemon itself classifies `reapable`. The desktop's Monitor →
Cleanup tab is the UI. Uses gopsutil so `go test` still runs on the dev box.

**`insights/sampler.go` + `docker.go`** — the 5s sample used to take ~3s on a 23-container
host because Docker's `stats?stream=false` sleeps to compute its own CPU%. Phase 86.D:
`one-shot=true` and our own delta (`dockerCPUPrev`, package-level because `/docker`
calls `dockerList()` live too), Docker only every 6th tick and `top` every 2nd, with
the last result carried forward on the other ticks. The "sample slow" WARN stays as the
regression alarm.

**`push/push.go`** — no Firebase, no FCM, no APNs. A paired device holds a long-lived
WebSocket (`GET /api/v2/push/subscribe`) from an Android foreground service; the server
delivers events over it and **queues per-device while the socket is down**, replaying on
reconnect. Wire contract: `docs/PUSH-PROTOCOL.md`.

**`logging/logging.go`** — one line format across every subsystem: local time with UTC
offset, LEVEL padded to 5, component like `[SRV:CHAT]`, then slog attrs as trailing
`key=val` (quoted when the value holds spaces, quotes, or `=`). The process minimum level
is watched from `~/.ymux/log-level`, which the desktop pushes (see
`backend-sessions.md` § log_sync).

**huma and the OpenAPI spec.** `files/huma.go`, `logs/huma.go`, and `api/huma.go` reflect
request/response structs into the server's OpenAPI, so the spec cannot drift from the
handlers. The wire contract is byte-for-byte identical to the stdlib handlers they
replaced — same query params, status codes, headers (`X-Ymux-Truncated` only —
the pre-rename `X-Winmux-Truncated` twin is gone, `Content-Disposition`), same JSON. `sdk-gen/ci-check.mjs` regenerates the spec straight
out of the server and fails CI if the committed SDKs moved.

## Invariants

- **`core` imports no sibling. `api` is imported by nobody.** Both directions are the
  cycle fix; breaking either re-creates the flat-package problem. `auth` and `logging`
  are leaves too (neither imports a sibling), which is why `term` may import them
  without putting a cycle back.
- **`auth.ScopeShellAttach` stays out of `AllScopes`.** Adding it there would hand a
  shell to every device that was never explicitly restricted — including ones paired
  years earlier. `term/service_test.go` asserts it.
- **`core.Version` and `ymux-addons`' `INSIGHTS_VERSION` are bumped together.** They had
  already drifted once (2.2.1 vs 2.2.0), and the effect was that the desktop stopped
  offering the update at all. See `crates.md`.
- **Rebake the two Linux blobs in the same commit as any shipping Go change.** Test files
  are excluded from the gate; they do not reach the binary.
- The wire contract lives in `frames.go` + the committed schemas. Change the Go type and
  the schema together, or the SDK drift-guard will say so.
- CGO-free (`modernc.org/sqlite`), which is why the linux/amd64 + linux/arm64 cross-build
  takes ~30s on a Windows runner with no cross toolchain.

## Read the source when

You need an endpoint's exact path and payload, the chat session state machine, or the
sampler's metric names. The API surface is generated into `sdk/typescript` and
`sdk/kotlin`; the frame schema is `internal/api/frames.schema.json`, the push
contract `docs/PUSH-PROTOCOL.md`, and the design rationale `docs/PHASE-77-DESIGN.md`.
