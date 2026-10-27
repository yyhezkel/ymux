# ymux-2026-08-23-manual-2 — research report

## Question

BACKLOG P3 (2026-08-23) says every Claude hook runs as its own process and pays for a TCP connect, a forwarded SSH channel and an HMAC handshake on every `rpc_call`. It proposes pooling those calls through a resident agent on the remote, with the Go daemon acting as a proxy, but only if the per-call latency is actually noticeable. This report checks whether that cost is real and measured, what Phase 86.E already changed, and what a daemon proxy would need. It is research only, with no code changes.

### Q1 — Does each Claude hook invocation open its own process and connection path (TCP + forwarded SSH channel + HMAC) in `cli/src/main.rs` `rpc_call`
### Q2 — What did Phase 86.E change so pre-tool-use uses 1 connection instead of 3, and how many connections does each remaining hook event open today
### Q3 — Is per-call hook latency measured or recorded anywhere, and does any evidence show it is a user-visible cost
### Q4 — Would a resident agent on the remote (Go daemon proxy: hook → unix socket → one long-lived tunnel connection) remove the TCP/SSH/HMAC setup per call, and what would it need (auth, lifecycle, reconnect, protocol)

## Findings

### Q1

**Answer: yes.** Every hook event is a separate `ymux claude-hook <event>` process, and every `rpc_call` it makes opens a new connection. That connection pays for a TCP connect, a new forwarded SSH channel, an HMAC handshake, a new connection to the desktop's local pipe or socket, and exactly one JSON-RPC request. Nothing is reused within a process or between processes. **Confidence: high.**

Evidence, following one call from the remote to the desktop:

- **Process per event.** The hook spec registers each Claude Code event as its own shell command, `${YMUX_BIN} claude-hook <event>` (`hooks/claude-code.json` `events`). Claude Code starts one process per event.
- **TCP connect per call.** `rpc_call` → `rpc_call_with` (`app/src-tauri/cli/src/main.rs:1273`, `:1285`) runs `TcpStream::connect(YMUX_SOCKET_ADDR)` on every call (`main.rs:1292-1297`). If that address is stale, it re-reads `last.env` and dials a second time (`main.rs:1298-1331`, Phase 80).
- **Forwarded SSH channel per call.** `YMUX_SOCKET_ADDR` is a loopback port that the remote sshd listens on because the desktop asked for a reverse forward (`request_reverse_forward` → `handle.tcpip_forward`, `app/src-tauri/src/lib.rs:5320`). sshd turns each accepted TCP connection into a new `forwarded-tcpip` channel, which the desktop receives in `server_channel_open_forwarded_tcpip` (`app/src-tauri/crates/ymux-core/src/lib.rs:662`) and passes to `spawn_bridge` (`app/src-tauri/crates/ymux-tunnel/src/lib.rs:400`).
- **HMAC per call.** `bridge_stream_to_pipe` runs `perform_handshake` first on every channel (`ymux-tunnel/src/lib.rs:175-188`): it sends a 32-byte nonce challenge, reads the response, verifies with `verify_slice`, then sends OK (`ymux-tunnel/src/lib.rs:92-160`). The CLI side is `perform_handshake` (`main.rs:1394-1490`), called from `rpc_via` whenever a token is present (`main.rs:1529-1532`).
- **Pipe/socket connection and one request per call.** After the handshake the bridge opens a new client connection to the desktop's named pipe or Unix socket (`ymux-tunnel/src/lib.rs:191-245`). `rpc_server::handle_client` deliberately serves exactly one request and returns. This is the v0.3.1 pipe-leak fix ("the pipe RPC protocol is one request per connection", `app/src-tauri/src/rpc_server.rs:281-296`, `:298-338`; root cause in `docs/DECISIONS.md:1225`).

**Round trips per call (inferred from the code order above, not measured).** The CLI waits for the challenge before replying, and waits for the verdict before sending the request (`main.rs:1418-1460`, `:1529-1545`). Each call therefore needs about three desktop↔remote round trips:
1. channel-open → confirm + challenge
2. response → OK
3. request → reply

A pooled connection would need one. Process start (exec + tokio runtime) and the hook's own work are not affected by pooling.

### Q2

**Answer.** Before Phase 86, a pre-tool-use hook called `tunnel_healthy()` before the real request. That function sent up to two `ping` RPCs (1.8 s timeout each, 200 ms sleep between them), and every ping was a full connection of its own. Then came `feed.push`. That is up to 3 tunnel connections per tool call (2 pings + 1 push). Phase 86.E deleted the pre-flight. A connect failure now surfaces from `feed_push_with_retry` and drops into the existing static-fallback arm. In the healthy case that leaves 1 tunnel connection. **Confidence: high** for what changed; **medium** for reading "3" as the worst case (the healthy pre-86 path was ping + push = 2 tunnel connections, plus a local B-path HTTP connection to the daemon, which also makes 3 TCP connects).

Evidence:
- `git diff 5003c38c f69eb8d -- app/src-tauri/cli/src/main.rs` (PR #25 merge, 2026-08-26) → removes `async fn tunnel_healthy()` (two `rpc_call("ping")` attempts) and its call `if subcommand == "pre-tool-use" && !tunnel_healthy().await`, and adds the comment "Phase 86: no pre-flight ping … one code path, one connection attempt set", now at `main.rs:2697-2699`.
- `PROGRESS.txt:6052-6055` lists 86.E as "tunnel_healthy() pre-flight pings removed". `docs/DECISIONS.md` (Phase 86 Decided entry) defers the resident agent: "one connection per hook is the floor without a daemon in the loop."

**Connections per hook event today** (HEAD `6070488f`). "Tunnel" means a TCP + SSH channel + HMAC + pipe round as described in Q1. Every row can add one more dial if the inherited address is stale and `last.env` points somewhere else (`main.rs:1298-1331`).

| Event (`claude-hook …`) | Tunnel connections | Other | Evidence |
|---|---|---|---|
| any event, `YMUX_PANE_ID` unset (not a ymux pane) | 0 | — | env-gate return `main.rs:2350-2371` |
| `pre-tool-use`, mode `acceptEdits`/`bypassPermissions`/`skip` | 0 | — | `main.rs:2382-2406` |
| `pre-tool-use`, default mode | 1 (2 only if the first connect fails: `feed_push_with_retry(…, 2)`, 200 ms back-off) | +1 local HTTP to daemon `127.0.0.1:7879` (B path, fire-and-forget, only if `~/.ymux/server/token` exists) | `main.rs:2701-2719`, `:288-306`, `:325-370` |
| `session-start` | 1 (`context.inject`, 300 ms budget, fail-open) | — | `main.rs:2427-2473`, `:1248-1255` |
| `notification`, relevant type | 1 (`feed.push`) | — | `main.rs:2492-2555` |
| `notification`, other type | 0 | — | early return `main.rs:2511-2519` |
| `stop-failure` | 1 (`feed.push`) | — | `main.rs:2565-2598` |
| `stop`, `session-end`, `user-prompt-submit` | 1 (`feed.push`) | session-meta is a local file write, no RPC | `main.rs:2611-2623`, `:2718-2721` |

In a gated session the busiest hook remains pre-tool-use, with one tunnel connection per matched tool call (`Bash|Write|Edit|MultiEdit|NotebookEdit|Task`, `hooks/claude-code.json`). `user-prompt-submit` and `stop` each add one per turn.

### Q3

**Answer: no.** Nothing in the code or the docs records end-to-end per-call hook latency, and none of the evidence I found shows hook latency as a cost users noticed. The two related numbers that exist measure something else. **Confidence: medium.** The absence is from grep over the repo; a check of a real `debug.log` / `hook-debug.log` could change it.

Evidence:
- **Desktop side measures only the handler.** `handle_client_with_telemetry` logs `handler {id} END {elapsed_ms} ms` at debug level (`rpc_server.rs:263-279`). The timer starts after the SSH channel open and the HMAC handshake have finished, so it leaves out the setup cost this ticket is about.
- **CLI side has timestamps but no duration.** `hook_log` writes `[ts] [LEVEL] [HOOK] msg` (`main.rs:43-44`). Blocking hooks log a `REQ` line and an `ACK` line (`main.rs:2655-2666`, `:2736`). Their difference is end-to-end time, but it includes the human's wait at the Gate card. Passive hooks log a single line with no timing. `grep -n "elapsed\|Instant::now\|latency" app/src-tauri/cli/src/main.rs` → no timing code in the hook path.
- **The one latency budget is a design figure.** `context.inject` gets 300 ms with a `YMUX_CONTEXT_TIMEOUT_MS` override "for a remote whose tunnel round trips are slower than that" (`main.rs:1245-1255`; `PROGRESS.txt:7832`). No measurement of the actual time is recorded.
- **The load that was measured came from port-watch, not hooks.** Phase 86 measured `hook-debug.log` as "100% `was stale, reconnected … from last.env` (11,155 lines in 3h, bursts of 12/s)". Those lines came from orphaned **port-watch** processes using `rpc_call`, each "a fresh TCP + forwarded channel + HMAC on the desktop" (`PROGRESS.txt:6025-6031`). Phase 86.A fixed that by giving the watcher `rpc_call_pinned` with exit-after-3-failures (`main.rs:1277-1283`), not by pooling.
- **No user complaint about hook latency.** `grep -i "hook" FOLLOWUPS.md FOLLOWUPS-ARCHIVE.md | grep -i "slow|lag|delay|latenc|stall|wait"` → no hook-latency complaint. The stalls on record are failures, not slowness: the 254-call pipe leak (`docs/DECISIONS.md:1225`) and the 60 s Claude Code hook timeout cutting off a Gate (`hooks/claude-code.json` v1.6.0 note). The "latency" commits in `git log --grep=latency` (`707d63e2`, `97138051`) are about keystroke latency from the bidi filter, which is unrelated.

### Q4

**Answer: partly.** A daemon proxy would remove the per-call forwarded SSH channel and the desktop HMAC handshake, but not the hook process, and the CLI would still make a local connect and authenticate to the daemon. Most of the remote side already exists. The desktop side does not: today's RPC server and tunnel bridge are deliberately one request per connection, so a long-lived multiplexed link is a protocol change on the desktop, not just a daemon feature. **Confidence: medium** (design inference from code; nothing has been built or measured).

What already exists:
- **The daemon already ends this exact wire protocol locally.** `internal/hooks` is "the localhost TCP listener that the ymux CLI's `claude-hook` dials", speaking the same challenge / response / one-JSON-RPC format (`app/src-tauri/server/internal/hooks/hooks.go:1-21`, `Start` `:65`, `handleConn` `:120-205`). It asks each `core.HookResolver` whose token produced the HMAC. Browser-created tmux sessions already point `YMUX_SOCKET_ADDR` at it (`docs/vault/server-go.md:192-204`). Phase 111 gave that listener a stable port across restarts (`hooks.go:58-64`). A proxy would be one more resolver/target that forwards to the desktop instead of answering locally. The CLI would not change, because it only follows `YMUX_SOCKET_ADDR`.
- **The daemon already has an outbound desktop client.** `internal/desktop` reads `last.env` and speaks the CLI's handshake to the tunnel (`app/src-tauri/server/internal/desktop/desktop.go:1-20`). `Call` still dials per request (`desktop.go:201-210`), so it shows the protocol, not a pool.

What it would need:
1. **Protocol (the largest item).** The desktop has to accept many requests on one connection, matched by JSON-RPC `id`, with responses out of order. A pre-tool-use Gate can hold its reply for up to 600 s (`main.rs:2641-2646` clamp; `hooks.go` "a blocking gate may legitimately hold"), so serving requests one after another on one connection would block every other hook behind a pending Gate. Today `handle_client` returns after one request (`rpc_server.rs:298-338`), and `bridge_copy` half-closes on first EOF (`ymux-tunnel/src/lib.rs:250-279`). Both are load-bearing fixes for the v0.3.1 leak (`docs/DECISIONS.md:1225`), so this needs a new multiplexed mode next to them, not a change to them. One alternative avoids the protocol change: keep a small pool of pre-handshaked idle connections, each used once. That removes the handshake and channel-open from the critical path but still costs one channel per call.
2. **Auth.** Hook → daemon: the existing per-session token/HMAC (`hooks.go` resolvers), or the Unix-socket peer credentials the backlog line suggests. Desktop sessions do not register tokens with the daemon today, so the daemon would have to accept the desktop's tunnel token from `last.env` as a resolver. Daemon → desktop: the existing HMAC once per long-lived connection (`desktop.go` `handshake`). Rule #8 still applies: no token in logs.
3. **Lifecycle.** The daemon is an optional add-on, not always present: the CLI's B path quietly does nothing when `~/.ymux/server/token` is absent (`main.rs:310-318`, `:333-335`). So hooks need the current direct path as a permanent fallback, and the CLI needs a way to choose the proxy (a separate env var or socket path) without losing `YMUX_SOCKET_ADDR` for the direct path. The daemon must also be the right version, which ties hook behaviour to the server rebake rules.
4. **Reconnect.** The tunnel port and token change with every desktop connection, and `last.env` is one global file per host. The DECISIONS Phase 86 entry says "there is no remote record of live workspaces (last.env is a single global file)". The daemon would have to watch `last.env`, drop and re-handshake its pooled connection when the file changes or the connection hits EOF, and fail any in-flight requests in a way the CLI maps to its existing static-fallback arm (`main.rs` pre-tool-use fallback after `feed_push_with_retry`).
5. **Multiple desktops.** Two desktops connected to the same host overwrite each other's `last.env`. Direct dialling already has this issue, and a single pooled connection would make it more visible.

**Net gain if built:** per call, about 3 → about 1 desktop round trip (Q1 inference), plus no SSH channel open/close and no new pipe instance on the desktop. The hook process spawn, the local connect and the local auth remain.

## Options

| Option | Cost | Risk | Evidence |
|---|---|---|---|
| A — Keep deferred; measure first (add a CLI-side duration from connect to reply, metadata only, to the existing `REQ/ACK` and passive log lines) | small, CLI only | none to behaviour; needs Rule #1 discipline (metadata only) | Q3: no measurement exists; `rpc_server.rs:263-279` leaves setup out |
| B — Pre-handshaked idle-connection pool in the daemon (one request per connection, no desktop change) | daemon resolver + `last.env` watcher + CLI routing | medium: still one SSH channel per call; adds a daemon dependency to every hook | `hooks.go:120-205`, `desktop.go:201-210` |
| C — Full multiplexed long-lived connection (the backlog proposal) | desktop `rpc_server` multiplexed mode + tunnel bridge + daemon + CLI | high: touches the v0.3.1 leak fixes; Gate holds up to 600 s per request | `rpc_server.rs:281-338`, `DECISIONS.md:1225` |

## Recommendation

**Option A: keep the pooling deferred and add measurement first.** The backlog line's own condition ("only worth it if per-call latency shows up") is not met. No end-to-end hook latency is recorded anywhere (Q3). Phase 86.E already removed the only extra connections a hook made (Q2). The CPU and connection load that was measured came from orphaned port-watchers, which Phase 86.A fixed directly. The cheapest next step is a metadata-only timing in the CLI hook path: milliseconds from connect to handshake-OK to reply, near `rpc_via` (`app/src-tauri/cli/src/main.rs:1517-1560`), written as a field on the existing `REQ/ACK` and passive `hook_dlog` lines (`main.rs:2655-2666`, `:2736`).

**What would change this:** if live timings show the setup part (connect + handshake) regularly above about 100 ms on real remotes, or if users report pre-tool-use or stop delays. In that case choose Option C only together with a desktop multiplexed RPC mode designed next to the one-shot handler, and keep the direct path as the fallback.

**Trade-off accepted:** about 2 extra round trips per hook stay in place until there are numbers to justify a cross-component protocol change.

## Open questions

- **Q3 (partly).** Actual per-call setup latency on a real remote is unknown. To answer it: run the timing from Option A on a live remote, or time `ymux claude-hook stop </dev/null` from a pane against `debug.log` `handler … END` lines. That is a live run, which this stage cannot do (goal Out of scope; Rule #17).
- **Q1 round-trip count (3 vs 1)** is inferred from code order. Whether sshd/russh pipelines the channel-open confirm and the first data (the challenge) into one flight has not been checked on the wire.
- **Q2 "3 connections"** is read as worst case (2 pings + push) or healthy case (ping + push + B-path HTTP). The PROGRESS and DECISIONS entries don't say which; whoever wrote the backlog line would know.
- **Q4 multiple desktops.** How a pooled daemon connection should behave when two desktops share one host's `last.env` is a design question with no current answer in code.

## Sources

- `hooks/claude-code.json` (`events`, v1.6.0 note) — one command per event; PreToolUse matcher and `timeout: 600`
- `app/src-tauri/cli/src/main.rs:1273-1331` — `rpc_call` / `rpc_call_with`: TCP connect per call, stale `last.env` re-dial
- `app/src-tauri/cli/src/main.rs:1277-1283` — `rpc_call_pinned` (Phase 86.A)
- `app/src-tauri/cli/src/main.rs:1394-1490` — CLI HMAC challenge-response
- `app/src-tauri/cli/src/main.rs:1517-1560` — `rpc_via`: handshake then one request
- `app/src-tauri/cli/src/main.rs:288-306` — `feed_push_with_retry`
- `app/src-tauri/cli/src/main.rs:310-370` — B path `forward_hook_to_server` to daemon `127.0.0.1:7879`
- `app/src-tauri/cli/src/main.rs:2276-2721` — `claude-hook` dispatch: env-gate, permission-mode short cut, session-start, notification, stop-failure, session-meta, feed.push
- `app/src-tauri/cli/src/main.rs:1245-1255` — `context_timeout_ms` 300 ms budget
- `app/src-tauri/crates/ymux-tunnel/src/lib.rs:92-245`, `:250-279`, `:400` — per-channel handshake, new pipe per bridge, half-close, `spawn_bridge`
- `app/src-tauri/crates/ymux-core/src/lib.rs:662` — `server_channel_open_forwarded_tcpip`
- `app/src-tauri/src/lib.rs:5320` — reverse forward `tcpip_forward`
- `app/src-tauri/src/rpc_server.rs:263-338` — handler telemetry; one request per connection
- `app/src-tauri/server/internal/hooks/hooks.go:1-21`, `:58-90`, `:120-205` — daemon hook listener, same wire format, resolvers
- `app/src-tauri/server/internal/desktop/desktop.go:1-20`, `:201-245` — daemon's outbound desktop client, dial per call
- `docs/vault/cli.md`, `docs/vault/crates.md` § ymux-tunnel, `docs/vault/backend-rpc.md`, `docs/vault/server-go.md:192-204` — orientation
- `PROGRESS.txt:6017-6055` — Phase 86 measurements and 86.A–E summary
- `docs/DECISIONS.md` Phase 86 Decided entry; `docs/DECISIONS.md:1225` — v0.3.1 one-request-per-connection root cause
- `git diff 5003c38c f69eb8d -- app/src-tauri/cli/src/main.rs` → `tunnel_healthy()` and its pre-tool-use call removed
- `git log -1 --format=%cs f69eb8d` → `2026-08-26` (PR #25 merge)
- `grep -n "elapsed\|Instant::now\|latency" app/src-tauri/cli/src/main.rs` → no timing code in the hook path
- `grep -i hook FOLLOWUPS.md FOLLOWUPS-ARCHIVE.md | grep -i "slow|lag|delay|latenc|stall|wait"` → no hook-latency complaint
- `git log --all --oneline -i --grep=latency` → only keystroke/bidi latency commits (`707d63e2`, `169294b0`, `97138051`)
- no web sources used
