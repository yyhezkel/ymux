# ymux-2026-10-05-manual-2 — research report

## Question
How far is the Go daemon (`app/src-tauri/server/`, the browser-session backend) from parity with the desktop's per-Claude-session context store (`context_store.rs`, Context Rail, Phase 105), and what would a port need? The BACKLOG line was written on 2026-10-05. **The main finding is that it is now mostly stale.** Phase 117 (commit `1b7f6f1a`, 2026-10-07) ported the store, its persistence and the rail's list endpoint to `server/internal/term/context.go`. Only the 105.C injection (`context.inject`) is still missing.

Path note: the goal says `server/internal/agent`. In this repo the daemon lives at `app/src-tauri/server/`. A repo-root `server/` does not exist (`ls /home/runner/ymux` shows no `server` dir).

### Q1 — Where does `server/internal/agent` stop short of a per-session context store, and which parts exist (brief parser)
### Q2 — What does the Rust `context_store.rs` persist per session (first prompt, brief log) and in what format
### Q3 — What happens today when the CLI calls `context.inject` against the Go daemon (unknown-method error, CLI fail-open path)
### Q4 — Which browser-created session paths currently skip context persistence and where the SessionStart injection (105.C) would hook in
### Q5 — What must the Phase C web UI need from the context store before a port is worth scheduling; what is the minimal port surface

## Findings

### Q1
**Answer.** `server/internal/agent` was never meant to hold the store. It is the stdlib-only, IO-free port of the desktop's per-pane logic. The store itself now exists one package over, in `server/internal/term/context.go` (284 lines, Phase 117). The premise of the BACKLOG line ("no per-session context store") is out of date. The only part still missing is injection (105.C).

**What exists:**
- `agent/` contains `brief.go`, `humanize.go`, `keys.go` and `state.go`, each with tests (`ls server/internal/agent`). The brief parser is `ParseBrief` / `BriefFromStop` / `PreBriefText` (`docs/vault/server-go.md:89`).
- The brief parser now carries the sticky `goal` / `done` keys: `server/internal/agent/brief.go:75-76` (fields) and `brief.go:192` (parse). This also makes the sibling BACKLOG P2 line (BACKLOG.md:14), which says they are missing, stale.
- `term/context.go:40-66` defines `ContextLogEntry` and `SessionContext`, mirroring the Rust structs field for field, including `schema`.
- `term/context.go:130-168`: `newContextStore` loads `<dir>/*.json`, prunes files older than 30 days, and marks unparseable files `broken`.
- `term/context.go:172-242`: `apply` keeps the first prompt only while it is empty (clipped to 2000), appends a `turn` line per stop with sticky goal/done, appends a `closed` line per session-end with the reason clipped to 80, caps the log at 200, bumps `version` and writes atomically.
- `term/context.go:260-274`: `recordContext` publishes `context:changed {session_id, ws_id}` on the events hub.
- `term/context.go:278-284` plus `service.go:157`: `GET /api/v2/context/sessions?ws_id=`.
- `service.go:102` wires the store to `<data>/context/sessions` in `SetDataDir`. `hookreg.go:108-118` holds the `context *contextStore` field (memory-only by default).
- `hookdispatch.go:143-156`: `feedPush` feeds the store for `user-prompt-submit`, `stop` and `session-end`, after `r.mu` is released.
- Tests: `term/context_test.go:14` (`TestContextStoreModel`).

**Where it stops short:**
- `term/context.go:18` says it in so many words: "Not ported: injection back into the agent (context.inject, 105.C)."
- There is no `session_context_get` equivalent. Only list is served: `service.go:157` is the only context route. The frontend never calls `session_context_get` (`grep -rln session_context_get app/src` returns nothing), so this gap is harmless today.

**Confidence:** high. All of it is read directly from the live code.

### Q2
**Answer.** The store keeps one pretty-printed JSON file per Claude Code session id, at `<config_dir>/context/sessions/<session_id>.json`. Each file holds `{schema, session_id, ws_id, pane_id, cwd, first_prompt (≤2000 chars), first_prompt_ms, goal, done_when, log[≤200], version}`. Each log entry is `{ts_ms, kind: turn|closed, status, task, delta, next, ask, rec, degraded}`.

**Evidence:**
- `app/src-tauri/src/context_store.rs:36-41`: the constants are `FIRST_PROMPT_MAX_CHARS = 2000`, `LOG_MAX_ENTRIES = 200`, `RETENTION` of 30 days and `SCHEMA_VERSION = 1`.
- `context_store.rs:43-70`: `LogKind` (`turn` | `closed`, lowercase) and `LogEntry`. The optional fields are `#[serde(default)]`.
- `context_store.rs:72-100`: `SessionContext`, described as "also the on-disk file shape". `goal` / `done_when` follow a last-non-empty-wins rule, and `version` is bumped on every mutation.
- `context_store.rs:121-129`: the id must be `[A-Za-z0-9_-]{1,128}`.
- `context_store.rs:235-237`: the directory is `config_dir/context/sessions`.
- `context_store.rs:244-256`: load strips a BOM and refuses a file whose `session_id` does not match its name.
- `context_store.rs:258-273`: save goes through tmp + fsync + rename (Rule #7) using `serde_json::to_string_pretty`.
- The spec of record is `docs/CONTEXT.md` § The model / § Persistence. It covers the first-prompt-only-while-empty rule, the degraded brief on a Stop with no brief, and the `closed` line carrying Claude Code's `reason`.

**One format difference from the Go port:** Go writes compact JSON (`json.Marshal`, `term/context.go:231`) where Rust writes pretty JSON. Both stores read either form, and the two never share a directory (the desktop uses `%APPDATA%\ymux\context`, the daemon uses `<data>/context`). This is cosmetic.

**Confidence:** high.

### Q3
**Answer.** The CLI's `ymux claude-hook session-start` sends `context.inject` over the session's `YMUX_SOCKET_ADDR`. In a browser-created session that address is the daemon's hook listener. The daemon's `term` target has no arm for the method, so it logs `hook method not handled` and returns a JSON-RPC error `{code: -32000, message: "unknown method"}`. The CLI treats that as `Ok(Err(e))`. It writes `session-start context.inject failed: …` to the hook log, prints nothing on stdout and exits 0, so Claude Code starts with no extra context. The session start is neither stalled nor broken.

**Evidence:**
- `server/internal/term/hookreg.go:153-155`: browser sessions get `YMUX_SOCKET_ADDR` = the daemon's listener, plus `YMUX_TUNNEL_TOKEN` and `YMUX_PANE_ID`.
- `cli/src/main.rs:2427-2444`: the `session-start` branch builds `{pane_id, session_id, source, tmux_session?}` and runs `tokio::time::timeout(budget, rpc_call("context.inject", req))`.
- `cli/src/main.rs:1248-1254`: the budget is 300 ms, overridable with `YMUX_CONTEXT_TIMEOUT_MS` and clamped to 100–3000.
- `cli/src/main.rs:1288-1300`: `rpc_call_with` prefers TCP to `YMUX_SOCKET_ADDR`.
- `server/internal/term/hookdispatch.go:62`: `errUnknownMethod = &core.RPCError{Code: -32000, Message: "unknown method"}`.
- `hookdispatch.go:65-80`: `DispatchHook` handles `ping` and `feed.push`, then B4 verbs and B5 agent verbs, then falls through to `logger.Warn("hook method not handled")` and returns `errUnknownMethod`.
- `server/internal/hooks/hooks.go:192`: the error is serialised as the JSON-RPC `error` object.
- `cli/src/main.rs:2445-2460`: `Ok(Err(e))` → `hook_vlog(...)` → empty text. A timeout takes the same path.
- `cli/src/main.rs:2468-2471` together with `session_start_output` (`main.rs:1260-1263`): empty text means nothing is printed, and the branch returns `ExitCode::SUCCESS`.
- The documented intent is in `docs/CONTEXT.md` § Injection: "fails open: … (including the Go daemon, which does not know the method)".

**Side cost:** each browser-session start (and each `/compact`, resume and `/clear`) leaves one `WARN hook method not handled method=context.inject` line in the daemon log (`hookdispatch.go:78`). It also costs one round trip that cannot succeed. The round trip runs on localhost, so it is far below the 300 ms budget.

**Confidence:** high for the code path. It has not been observed live on a box: no daemon log was read and no live run was made.

### Q4
**Answer.** For a browser-created session that has hooks, the first prompt, turn log and close line are now persisted (Phase 117). The paths that still skip persistence are these:
1. **No hook routing at all.** A tmux older than 3.2, or a daemon with no hook listener address, creates the session without the per-session env (`service.go:339-345`, `tmux.go:216-226`). Its hooks then go wherever the global environment points, usually the desktop's `last.env` or nowhere. The daemon store never sees them.
2. **Released carriers** (`replace_pane`, Phase 117). `hookdispatch.go:92-96` returns `passive` before any folding, so it writes no context ("no card, no light, no context"). This is intended.
3. **A daemon restart.** `RecoverHooks` (`recover.go:25-40`) rebuilds hook entries from tmux env, but it does not know `released`, so both sessions carry the pane id again. `docs/vault/server-go.md:607-610` calls this a "known edge".
4. **Hooks with no or invalid `session_id`.** `recordContext` returns early on an empty id (`context.go:261`), and `apply` drops an invalid id (`context.go:173-175`).
5. **No data dir.** Without `SetDataDir` the store is memory-only (`hookreg.go:118`, `context.go:132-134`), so it is lost on a restart.
6. **SessionStart itself.** The CLI never `feed.push`es a session-start (`cli/src/main.rs:2419-2427` returns before the shared dispatch). That is by design, but it means the daemon has no SessionStart event to react to other than the `context.inject` RPC.

**Where 105.C would hook in:** add a `case "context.inject":` to `termHookTarget.DispatchHook` (`server/internal/term/hookdispatch.go:66-71`), alongside `ping` / `feed.push`. That puts it before the `verb` / `agentVerb` fall-through and the unknown-method error at `:78-79`. The session is already authenticated by HMAC at that point and `t.e` gives the session's `hookEntry` (`paneID`, `workspaceID`). The handler would:
- read `this` from `r.context.sessions[session_id]` (under `contextStore.mu`);
- read the workspace intent from the web workspaces store (`webws.go:52` `Intent`, `webws.go:167` `get(id)`) using `e.workspaceID`;
- list siblings with `r.context.list(wsID)` (`context.go:245`);
- build the text and return `{"additional_context": text}`, which is the shape the CLI reads at `cli/src/main.rs:2446-2449`.

The Rust reference for both the dispatch arm and its body is `src/rpc_server.rs:1283-1305` → `context_store.rs:657-701` (`injection_for_hook`) → `context_store.rs:576-652` (`build_injection`).

**Confidence:** high for paths 1–2, 4–6 and for the hook point. Medium for path 3: it is documented as a known edge but not reproduced.

### Q5
**Answer.** The web UI's Context Rail already has everything it needs from the store. `app/src/backend/web.ts:893-894` maps `session_context_list` to `GET /api/v2/context/sessions?ws_id=`, and `context:changed` arrives on the events socket. The rail is a reader and does not use injection (`docs/CONTEXT.md` § The Context Rail: the rail fetches `session_context_list` and reacts to `context:changed`). **No further port is needed for the rail.** The open item is only worth scheduling for agent-side value: giving a browser-session Claude its goal and progress back after `/compact` or `resume`, and sibling sessions plus the workspace goal on `startup`. That is the gap between desktop-session and browser-session agents today.

**Minimal port surface for 105.C (all Go, `server/internal/term/`):**
1. The `context.inject` arm in `DispatchHook`, as described in Q4.
2. A pure `buildInjection(source, this, intent, siblings) string` port of `context_store.rs:485-652`. The pieces are `INJECT_MAX_BYTES = 1536` (`:488`), `INJECT_LOG_LINES = 5` (`:490`), `INJECT_MAX_SIBLINGS = 8` (`:492`), `INJECT_PROMPT_CHARS = 400` (`:495`), the `[ymux-context]` header (`:496`), and the helpers `flat`, `status_word`, `delta_line`, `clip_bytes` (char-boundary clip) and `assemble` (drop the oldest items first) at `:500-570`. Port the Rust tests under the same names, as Phase 99 did (`docs/vault/server-go.md` § agent).
3. A `get(sessionID)` accessor on `contextStore`. `list` exists, and `get` is trivial over `s.sessions`.
4. The off switch. The desktop reads `settings.brief.inject_context` (`context_store.rs:663`, `settings.rs:909/919`). The daemon's browser settings document is stored **opaque** and never parsed (`server/internal/term/settings.go:3-15`). The port therefore has to either parse that one field out of `web-settings.json`, or default to on and accept that there is no off switch in browser mode. This is the one design decision in the port.
5. The obligations from CLAUDE.md: rebake both `resources/ymux-server-linux-{x64,arm64}` blobs from the ci-windows artifact, bump the daemon version, and update `docs/vault/server-go.md` § term/context.go (whose line "Injection … is NOT ported" would become false) plus `docs/CONTEXT.md` § Injection ("including the Go daemon, which does not know the method").

**Not needed:** the CLI needs no change, because it already sends `pane_id`, `session_id`, `tmux_session` and `source` and reads `additional_context` (`cli/src/main.rs:2428-2449`). No new HTTP route is needed (injection travels over the hook RPC), and no frontend change is needed.

**Rough size (inferred):** about 150–200 Go lines plus tests. The Rust injection section is `context_store.rs:485-701`, about 215 lines including `injection_for_hook`, whose workspace lookup collapses in Go to `e.workspaceID`.

**Confidence:** high for the surface. Medium for the size, which is an estimate from the Rust line count and has not been measured.

## Options

| Option | Cost | Risk | Evidence |
|---|---|---|---|
| A — Close the BACKLOG item as mostly done; re-file a narrow "105.C context.inject on the daemon" item (P3) | none now | browser agents keep starting with no injected context; one WARN log line per session start | `context.go:18`, `hookdispatch.go:78` |
| B — Port 105.C now (minimal surface per Q5) | ~150–200 Go LOC + tests, blob rebake, version bump, 2 doc edits | settings is opaque on the daemon → off-switch decision needed; release cadence for blobs | Q5 items 1–5 |
| C — Only silence the noise: answer `context.inject` with `{"additional_context": ""}` | ~3 lines + rebake | hides that injection is missing; still needs a rebake | `hookdispatch.go:66-71`, `cli/src/main.rs:2446-2449` |

## Recommendation
**Option A.** Close this BACKLOG line as superseded by Phase 117, and re-file a narrow P3 item: "daemon: port 105.C `context.inject` (buildInjection + DispatchHook arm + inject_context off-switch decision)". Close the sibling P2 line (BACKLOG.md:14, brief `goal`/`done` keys) as done at the same time, since `agent/brief.go:75-76,192` now has them.

The trade-off is that browser-session agents get no context back after `/compact` or `resume` until someone schedules the item. Today that costs nothing visible: the CLI fails open and the rail works.

Move to B when either of these happens:
- browser sessions become a primary way of running long agent tasks, which is where `/compact` recovery matters;
- someone decides the off-switch question (parse one field from the opaque settings document, or always on).

At that point, B is a self-contained, roughly one-day Go change with a precise Rust reference to port test-for-test.

## Open questions
- **Live behaviour of Q3 on a real box.** The unknown-method error and fail-open path are proven from code only. To confirm, run `claude` in a browser-created session on a daemon ≥ 2.17.0 and grep the daemon log for `hook method not handled method=context.inject`, plus `~/.ymux` hook log for `session-start context.inject failed`.
- **Whether the deployed daemon blobs include Phase 117.** Not checked. `docs/vault/howto/redeploy-server-blob.md` says how.
- **Q4 path 3 (RecoverHooks after `replace_pane`)** is documented as a known edge but not reproduced. Its effect on context records (two sessions writing under one pane id) is unmeasured.
- **The Q5 port size** is inferred from Rust line counts, not measured.
- **The off-switch design for browser mode** (Q5 item 4) needs a human decision.

## Sources
- `app/src-tauri/server/internal/term/context.go:1-284` — the Phase 117 store; `:18` says injection is not ported
- `app/src-tauri/server/internal/term/hookdispatch.go:62,65-80` — unknown-method error; dispatch fall-through
- `app/src-tauri/server/internal/term/hookdispatch.go:92-96,143-156` — released → passive; context fed from feedPush
- `app/src-tauri/server/internal/term/hookreg.go:108-118,153-155` — store field; session env (`YMUX_SOCKET_ADDR`)
- `app/src-tauri/server/internal/term/service.go:102,157,339-345` — data dir wiring; route; hooks only with listener + tmux ≥3.2
- `app/src-tauri/server/internal/term/tmux.go:216-226` — `SupportsSessionEnv`
- `app/src-tauri/server/internal/term/recover.go:25-40` — `RecoverHooks`
- `app/src-tauri/server/internal/term/settings.go:3-15` — settings document opaque
- `app/src-tauri/server/internal/term/webws.go:52,167` — workspace `Intent`; `get`
- `app/src-tauri/server/internal/hooks/hooks.go:192` — JSON-RPC error serialisation
- `app/src-tauri/server/internal/agent/brief.go:75-76,192` — sticky goal/done ported
- `app/src-tauri/src/context_store.rs:36-129,235-273,485-701` — Rust model, persistence, injection
- `app/src-tauri/src/rpc_server.rs:1283-1305` — desktop `context.inject` arm
- `app/src-tauri/cli/src/main.rs:1248-1263,1288-1300,2419-2471` — CLI timeout, transport, fail-open branch
- `app/src/backend/web.ts:893-894` — web backend maps `session_context_list` to the daemon route
- `docs/CONTEXT.md` § The model, § Persistence, § Injection — spec of record
- `docs/vault/server-go.md:587-610` — vault note for Phase 117 context.go, "105.C … NOT ported", released edge
- `git show -s 1b7f6f1a` → `1b7f6f1a 2026-10-07 Phase 117: WEB-DESIGN F3 — … Context Rail (daemon 2.17.0)`
- `git log --oneline -- app/src-tauri/server/internal/term/context.go` → `1a4ccc2e` (log `[]` not null fix), `1b7f6f1a`
- `grep -rln session_context_get app/src` → no output (frontend never calls get)
- Web: none used — the repo answered every question
