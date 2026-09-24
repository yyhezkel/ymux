---
vault: server-go
covers:
  - app/src-tauri/server/cmd/ymux-server/main.go
  - app/src-tauri/server/internal/api/*.go
  - app/src-tauri/server/internal/auth/*.go
  - app/src-tauri/server/internal/chat/*.go
  - app/src-tauri/server/internal/config/*.go
  - app/src-tauri/server/internal/core/*.go
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
core     ←  every subsystem imports it; it imports no sibling
api      →  imports subsystems; subsystems NEVER import api
```

`internal/core` (110 lines) holds the cross-subsystem interfaces and value types —
`AddrSink`, `HookConnHandler`, `NotificationSender` and friends. It is a **leaf package
by rule**, and that rule is the concrete fix for the Phase-69 WS↔session↔hookRPC import
cycle that forced the old daemon into one flat `package main`. Same for `api`: the
dependency arrow points one way, so there is no cycle to break later.

## The packages

| Package | Lines | What |
|---|---|---|
| `api` | 838 | HTTP front door: the mux, unauthenticated liveness + version negotiation, and each subsystem mounted behind auth middleware. `huma.go` holds the typed client-SDK surface |
| `auth` | 195 | `Bearer` middleware plus per-device scope grants (`scopes.go`) — a leaf, imports no sibling |
| `chat` | 2,953 | the biggest: Claude session runner, the engine↔substrate bridge, hook RPC, pairing, transcript parser, push, scopes, store |
| `config` | 469 | API token, filesystem paths, the log janitor (size cap + age prune), and the one-time data-dir migration |
| `core` | 110 | the leaf interface package |
| `files` | 682 | the Files API (`/api/v2/files/*`) |
| `hooks` | 42 | a thin TCP listener — owns none of the protocol |
| `insights` | 2,653 | sampler, store, Docker, the hygiene reaper, and the two Phase-84 rollups |
| `logging` | 599 | the unified `log/slog` handler |
| `logs` | 475 | per-client log storage and the SSE tail |
| `push` | 433 | self-hosted push over a long-lived WebSocket |
| `term` | 870 | tmux session management + a binary WebSocket carrying a real PTY (Phase 95) |
| `workspace` | 1,613 | the workspace pub/sub substrate and its WebSocket frame contract |

## Things worth knowing before you edit

**`workspace/frames.go`** — the WebSocket frame contract is **typed Go values** so
producers cannot drift from the published schema
(`docs/ymux-server/frames.schema.json` + `asyncapi.json`). Discriminator is `"type"`,
chosen because kotlinx `@JsonClassDiscriminator`, TS tagged unions, and
AsyncAPI/JSON-Schema all default to it. Locked in S4.3 and canonical — no client is
pinned to anything else yet.

**`chat/bridge.go`** — the engine↔substrate bridge. The Claude runner was built for the
retired `/api/claude/*` WebSocket; the new workspace API is a **pure pub/sub substrate**.
For a `claude_chat` session the bridge lazily spawns a Claude process on the first
`user_input`, feeds stdin, and republishes its output (assistant text, tool use/result,
hooks, status) into the substrate.

**`chat/chat_hookrpc.go`** — holds `challengeTag`, the Go half of the tunnel handshake.
It still speaks the legacy `WINMUX-CHALLENGE` dialect on purpose; the Rust half is
`CHALLENGE_TAG` in `ymux-tunnel`. **Flip both together.**

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

`/api/v2/term/*` mounts **raw**, not behind `auth.Bearer`, for the same reason `push`
does: that middleware only knows the shared token, and a paired device's token has to
work too. `service.go`'s `gate` does both checks and **fails closed** — a Service with
neither a shared token nor a scope resolver rejects everything, which is the opposite of
the workspace subsystem's "no auth configured ⇒ open" convenience and deliberately so.

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

Known gap, logged in FOLLOWUPS: the four REST ops are stdlib handlers, not huma ops, so
they are **not** in the generated OpenAPI and the SDK drift-guard does not cover them.
Phase B moves them.

**`hooks/hooks.go`** (42 lines) is deliberately tiny: bind a localhost port, report the
bound address through `core.AddrSink` so spawned claude children can be pointed at it,
hand each connection to a `core.HookConnHandler` — which is `chat.SessionManager`, the
thing that actually owns per-session HMAC tokens and pending-hook state. That
indirection is what breaks the import cycle.

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
replaced — same query params, status codes, headers (`X-Ymux-Truncated`,
`Content-Disposition`), same JSON. `sdk-gen/ci-check.mjs` regenerates the spec straight
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
`sdk/kotlin`; the frame schema is `docs/ymux-server/frames.schema.json`, the push
contract `docs/PUSH-PROTOCOL.md`, and the design rationale `docs/PHASE-77-DESIGN.md`.
