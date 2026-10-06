# ymux in the browser — design

Status: **draft, 2026-09-10**. Decision thread in `docs/DECISIONS.md` (Open,
"ymux in the browser"). No phase number yet — assign one at implementation time
(`git log --all --grep` first; numbers race across sessions).

## 0. The decision this document rests on

Yossi, 2026-09-10: **"ymux in the browser", not "a terminal in the browser".**
The point is full control of the presentation — the same shell, sidebar, panes,
feed, briefs and hook gates the desktop has — served from our own server over
HTTPS, so any browser (laptop, phone, tablet) is a ymux client. A raw
terminal-in-a-tab (ttyd behind the existing nginx) was considered and rejected as
the deliverable; it may still be a useful spike.

## 1. What this actually changes

Today the **desktop Rust backend is the brain** (`lib.rs`, 14k lines): it owns
`workspaces.json`, the layout tree, every SSH session, the reverse tunnel, and the
JSON-RPC endpoint the CLI and agent hooks call back into. The Go daemon on the
remote (`ymux-server`, 11k lines) is a **remote agent**: insights, chat, files,
logs, pairing, push, and a workspace pub/sub substrate.

A browser client has no Rust. So in browser mode **the Go daemon becomes the
brain** for the box it runs on. Concretely it must absorb two Rust surfaces:

1. The subset of the **184 Tauri commands** the frontend calls that make sense
   when the server is the host (§4).
2. The subset of the **`rpc_server.rs` dispatch catalog** that the remote CLI
   and hooks call *back* — `feed.push`, `feed.decide`, `port.opened`,
   `set-status`, `set-pane-title`, the hook verbs, `tree`/`split`/`send` for
   agent automation (§6). Today those ride the reverse tunnel to the desktop;
   with no desktop they must land on the daemon.

The second surface is the one that is easy to forget and the one that makes the
product ymux rather than a terminal: without it there are no hook gates, no feed,
no traffic lights, no briefs.

## 2. What already exists and is reused as-is

| Piece | Where | Reuse |
|---|---|---|
| HTTPS front door | `nginx-proxy` add-on: nginx + Let's Encrypt (Cloudflare DNS-01) | unchanged; add a `location /` for the web bundle |
| Auth | bearer token + per-device tokens with scopes, QR pairing (`internal/auth`, `chat/pairing`) | unchanged; one new scope (§7) |
| Event streaming | typed frames in `workspace/frames.go`, `seq` replay over the **per-session** `GET /api/v2/workspace/{id}/session/{sid}/subscribe` | **Correction 2026-10-04:** there is no workspace-wide `/api/v2/workspace/events` WS yet — it was a PHASE-77 proposal. Phase B3 builds it |
| Hook allow/deny loop | `PendingRequest` winner-takes-all, `hook_request` / `hook_decision` / `hook_resolved` frames | this **is** the desktop's `feed.push` blocking loop, already server-side |
| Hook listener | `hooks/hooks.go` + `chat/chat_hookrpc.go` (speaks the `WINMUX-CHALLENGE` dialect) | extend the method set (§6) |
| Files | `/api/v2/files/*` | replaces `file_*_remote` |
| Insights / hygiene / Claude usage / analytics | `internal/insights` | unchanged |
| Session model | `workspace/model.go` already declares `KindTerminal = "terminal"` and never implements it | the hook for §3 |
| Multi-machine session labels | `~/.ymux/session-meta.json` (CLI `session-meta`) | the browser reads it the same way the desktop does |
| Terminal rendering | xterm.js 6 + the four RTL modules + `pty_decode` semantics | unchanged; only the byte source moves |
| Push | self-hosted WebSocket push with per-device queueing (`internal/push`) | becomes Web Push / the PWA's channel |

## 3. Server: the `terminal` session kind

New in `internal/workspace` (or a sibling `internal/term` package that imports
`core` only — keep the leaf rule):

- **Attach, never spawn a bare shell.** A terminal session is `tmux attach -t
  <name>` on a PTY (`creack/pty`). This matches the desktop's "attach means
  attach" rule (DECISIONS 2026-08-23) and means every browser pane is
  persistent by construction. Create = `tmux new-session -d -s <name> -c <cwd>`
  then attach.
- **`GET /api/v2/term/sessions/{name}/attach`** — a **separate binary WebSocket**,
  not the JSON workspace stream. (Keyed by tmux NAME, not by a minted session id:
  tmux is the truth, Q1.) Raw bytes both ways; a single text frame
  `{"type":"resize","cols":N,"rows":N}` for resize. Reason: base64-in-JSON
  doubles PTY traffic and puts a hot path through the typed-frame codec that was
  designed for chat events. One WS per pane, so per-pane backpressure is free.
- Scrollback for a late joiner: replay the last N KB from a ring buffer on
  attach (tmux itself repaints on attach; the ring buffer is for the
  `pane.scrollback` RPC agents use).
- List / rename / kill map to `tmux list-sessions -F`, `rename-session`,
  `kill-session` — the same commands `lib.rs` runs over SSH today, run locally.
  `session-meta.json` annotation (`owned` / `in_cwd` / `foreign`) is ported from
  `backend-core.md` § "Which folder a session belongs to".
- **Rule #1 applies verbatim**: log pane id + byte counts, never content.

Sizing: ~600 lines Go including tests. `creack/pty` is CGO-free; the blob stays
cross-buildable on the Windows runner.

## 4. Server: workspaces, layout, and the small stores

**Truth model (decision, §9 Q1): server-native workspaces; tmux sessions are the
shared reality.** The daemon does *not* mirror the desktop's `workspaces.json`.
A browser workspace is `{id, name, project_root, layout, tabs_mode, intent}` in
the daemon's SQLite (the `Workspace` row in `model.go` grows those columns).
Both clients discover the same tmux sessions through `session-meta.json`; the
desktop's Phase-90 "a session is a workspace row" is exactly the bridge. What
does not sync: which panes are open where. That is presentation, per client.

**Layout tree ops move to TypeScript.** `split`, `close`, `swap`, `set_ratio`,
`distribute_evenly`, `reset_layout` are pure tree operations (~400 lines in
`lib.rs`, unit-tested there). In browser mode the frontend mutates the tree and
`PUT`s the document to the daemon (`/api/v2/workspace/state` with an optimistic
`version` — **correction 2026-10-04: it does not exist yet**, only in PHASE-77-DESIGN;
Phase B5 builds it). The daemon stores it opaque. The desktop keeps its Rust
implementation for now — **logged debt**: two implementations of one tree
algorithm; the follow-up is to make the desktop use the TS ops too and reduce
Rust to persistence.

Small stores the daemon gains, each a table + a handful of huma ops, all with the
same shapes the Tauri commands return today so the frontend types do not fork:

| Store | Tauri commands replaced | Notes |
|---|---|---|
| settings | `settings_load/save/reset/apply_preset/get_presets` | per-device? No — per server, one settings doc; UI prefs stay in localStorage as today |
| notes | `notes_*` | trivial |
| tickets | `tickets_*` | the project-on-agent-machine model (DECISIONS 2026-08-12) is *simpler* here: the daemon is on that machine |
| feed + notifications | `feed_list/decide`, `notifications_*` | feed items are `PendingRequest` + the event log; `humanize_notification` ports to Go (bilingual copy) |
| skills | `skills_list/installed` | reads `~/.claude` locally |
| worktrees / project probe | `git_probe_worktrees`, `project_folder_probe`, `workspace_list_worktrees`, `workspace_create_worktree`, `workspace_open_worktree` | `git` run locally, argv arrays (Rule #3) |
| exec | `ssh_exec_in_workspace` | `POST /api/v2/exec` — **owner scope only**, argv array body, never a string |
| diff | `diff_pane_*` | `git diff` locally |
| claude | `claude_summarize`, `claude_usage_fetch`, `pane_list_claude_sessions` | `claudeusage.go` already exists; summaries read `~/.claude/projects` locally |

### 4.2 Session history: an ended session stays openable and resumable

Decided with Q1 (Yossi, 2026-09-10). Deleting a session row kills the tmux
session on the server, as Phase 90 does today — but the conversation should not
vanish with it.

What the code does now: the transcript is Claude's own
`~/.claude/projects/<encoded-cwd>/<session>.jsonl` and **survives the kill**.
What is lost is the *mapping*: `cli/src/session_meta.rs::prune` deletes every
entry with no live `tmux ls` match on every write, so nothing remembers which
transcript belonged to the row. The desktop's resume picker
(`pane_list_claude_sessions`) already scans the jsonl files and offers
`claude --resume`, so half the feature exists, unlinked from the row.

The change, small and shared by both clients:

- `prune` marks `ended_at` instead of deleting. Retention: 90 days or the N
  latest, whichever is smaller; a real prune only past that.
- The daemon's session list returns ended rows with `ended_at`,
  `claude_session_id`, `auto_name`, `cwd`. The sidebar keeps the row with an
  "ended" glyph and two actions:
  - **open** — a read-only transcript viewer. The daemon already parses this
    format (`insights/claudeusage.go`); a `GET /api/v2/claude/sessions/{id}/transcript`
    returns the user/assistant turns, paged. **Rule #1:** rendered, never
    logged — the handler logs the session id and byte count only.
  - **resume** — `tmux new-session -d -s <name> -c <cwd> -- claude --resume <id>`
    (argv array, Rule #3), then attach; the row flips back to live and
    `session-meta` is rewritten by the hooks as usual.
- Desktop parity is free: the same `ended_at` field reaches
  `pane_list_tmux_sessions` through the existing SSH read of `session-meta.json`.

### 4.1 Commands that do NOT exist in browser mode (by design)

Local-machine and desktop-shell affordances: `file_*_local`, `file_manager_*_local`,
`font_*`, `list_system_fonts`, `updater_*`, `check_for_updates_now`,
`download_and_install_update`, `stt_transcribe_local`, `local_setup_*`,
`detect_local_shells`, `provisioning_*`, `ssh_*`, `test_ssh_connect`,
`parse_ssh_config`, `list_ssh_keys`, `check/fix_key_permissions`,
`connect_existing_*`, `browser_pane_*`, `browser_popout_open`,
`workspace_browser_*`, `pane_browser_*`, `popout_pane`, `restart_windows`,
`set_tray_badge`, `clipboard_read_text`, `zellij_delete_session`,
`pane_upload_dropped` (becomes the Files upload), `addon_*`, `mobile_pairing_*`
(the daemon *is* that; device management is a daemon page), port forwarding
(`forward_port_start`, `port_forward_stop`, `list_detected_ports` — an SSH
local-forward has no browser equivalent; v2 could expose detected ports as nginx
sub-paths). The in-app Browser is a real browser tab now.

**The frontend must hide these, not fail on them.** `types.ts` already has the
pattern: `paneCaps()` / `profileFor()` decide what a pane can offer from its
effective connection. Extend it with a backend capability set (§5) so the
sidebar, menus, palette and Settings tabs render only what the backend can do.

## 5. Frontend: one `Backend` interface, two implementations

178 `invoke`/`listen` occurrences across 42 files, 184 distinct commands, 29
events. The change is mechanical but wide:

- **`app/src/backend/backend.ts`** — `interface Backend { call<T>(cmd, args):
  Promise<T>; on(event, handler): Unlisten; caps: Set<Capability>; term(sessionId):
  TermStream }`. `TermStream` is `{ write(bytes), resize(c, r), onData(cb),
  onExit(cb), close() }`.
- **`TauriBackend`** — `call` = `invoke`, `on` = `listen`, `term` = the existing
  `pty_write` / `pty_resize` + `pty:data` / `pty:exit` plumbing, `caps` = all.
- **`WebBackend`** — `call` = `fetch` to a command→route table (one file, one
  line per command, generated from the OpenAPI spec via `sdk/typescript` so the
  drift-guard covers it), `on` = a demux over the single workspace-events WS,
  `term` = the binary WS from §3, `caps` = what `GET /api/version` reports.
- **`index.tsx` picks the backend once**: `"__TAURI_INTERNALS__" in window` →
  Tauri, else Web. The `platform.ts` init pattern (resolve once before first
  render, sync reads after) is the template.
- Every `invoke("x", …)` becomes `backend.call("x", …)`. A codemod plus `tsc`
  does most of it; the 29 `listen` sites in `App.tsx` lines ~2778–3200 are done
  by hand. Rule #5 holds: `call<T>` keeps the explicit return type at each site.
- ~~`TerminalInstance` (`terminalInstance.ts`) takes a `TermStream` instead of a
  session id + global listeners.~~ **Superseded 2026-10-05 (§8.2, C2):** no
  `TermStream`; the WebBackend speaks the desktop's own `pty_write` / `pty_resize` /
  `pty:data` / `pty:exit` contract over the attach WS, so the PTY path is untouched. The RTL modules and `pty_decode`-equivalent
  chunk reassembly are unchanged; **UTF-8 chunk reassembly moves into
  `WebBackend.term`** because the daemon sends raw bytes (the desktop does it in
  `pty_decode.rs`).
- **Layout ops** land in `app/src/layoutOps.ts`, pure and Solid-free like
  `paneAgentState.ts`, ported from the `lib.rs` functions with their tests
  translated (`swap_two_leaves_in_a_split` and friends).
- Popouts: browser tabs. `index.tsx`'s label router gets a `?popout=<sid>` arm
  for the web build only (the desktop asset protocol cannot serve suffixed
  paths; a real HTTP server can).

Sizing: ~2–3k TS touched, of which ~1.5k is the mechanical rename. This is the
largest and least glamorous phase, and it is where the risk lives.

## 6. Server: the CLI / hook bridge

On the box, `ymux claude-hook`, `port-watch`, `session-meta` and the agent verbs
dial `YMUX_SOCKET_ADDR`. Today that is the reverse-tunnel port. **Correction
2026-10-04:** `setup-hooks` writes no address at all — only `<exe> claude-hook <sub>`
entries in `~/.claude/settings.json`. The CLI finds its listener through the process
env (`YMUX_SOCKET_ADDR`, `YMUX_TUNNEL_TOKEN`, `YMUX_PANE_ID`), falling back to
`~/.ymux/run/last.env`. So in browser mode the daemon must **inject that env into each
tmux session it creates** (`tmux new-session -e …`, session-scoped so it beats the
desktop's `set-environment -g`), pointing at its own hook listener — `hooks/hooks.go`
binds one and reports it through `core.AddrSink`. Same challenge dialect, same CLI
binary. And the hook "verbs" below are not methods: they are `subkind` values of a
single `feed.push` (`rpc_server.rs` `feed.push` arm).

**Done in B2 (Phase 100):** `internal/hooks` owns the handshake and asks each
`core.HookResolver` (chat, term) whose token signed it; `term.HookRegistry` injects the
three variables with `new-session -e` (tmux ≥ 3.2) and folds `feed.push` into the pane's
traffic light and brief via `internal/agent`. Permission requests are allowed
(`policy:"none"`) until B3 adds the events WS and real gating. State is in memory and
not yet served.

The daemon's `HookConnHandler` grows the JSON-RPC subset the desktop's
`dispatch()` answers for remote callers:

- `feed.push` (blocking and not) → `PendingRequest` + event log; `feed.decide` →
  resolution + `hook_resolved` fan-out. The desktop's `apply_hook` traffic-light
  transition table ports to Go and emits `pane:agent-run`-shaped frames.
- The passive hook verbs (`stop`, `session-start/end`, `user-prompt-submit`,
  `post-tool-use`, `subagent-stop`, `pre-compact`, `notification`) → feed cards
  via a Go port of `humanize_notification`, plus the `[ymux-brief]` parser
  (`brief.rs` is pure string ops; port with its tests).
- `port.opened` / `port.closed` → detection only, as today.
- `set-status`, `set-pane-title`, `set-pane-annotation`, `notify`, `note-*`.
- Agent automation: `tree`, `split`, `send`, `send-key`, `pane.scrollback`.
  `split` mutates the stored layout document server-side — the one place the
  daemon must understand the tree. Keep it to "split leaf X" and let the
  frontend re-derive; do not port the whole op set to Go.

**When both a desktop and a browser are attached to the same box**, whose hook
gate fires? The tmux session's `YMUX_SOCKET_ADDR` decides: a session created
from the desktop dials the tunnel, one created from the browser dials the
daemon. The hook-forward path (`POST /api/v2/hooks/forward`, `mobile.go`)
already lets the desktop mirror its gates to the daemon so phones see them; the
browser gets those for free. The reverse (daemon → desktop) is out of scope.

## 7. Delivery, auth, and the security line

### 7.1 Web bundle delivery (Q2 — decided 2026-10-05: (b))

The bundle is vite's output — `index.html`, hashed JS/CSS chunks, fonts, ~3 MB.
Something has to serve it at `https://<domain>/`. Three ways:

| | Mechanism | For | Against |
|---|---|---|---|
| (a) `//go:embed` | `app/dist` compiled into `ymux-server` | simplest to serve; one version, one artifact | the daemon is a **committed 13 MB blob per arch**; every frontend fix trips the rebake gate and adds ~26 MB of git history per PR; the frontend's release cadence is chained to the daemon's |
| (b) **`ymux-web` add-on** | `ymux-web-<ver>.tar.gz` bundled in `app.exe` with `include_bytes!` like the CLI; the add-on (registry in `crates/ymux-addons`, actions in `addons.rs`) uploads it over the workspace SSH session to `~/.ymux/server/www/<ver>/`; the daemon serves the newest at `/` | offline-friendly; **version-aligned by construction** (app version = web version); upload is sha256-gated and idempotent exactly like the CLI bootstrap; detect / install / update reuse the add-on UI that exists | updating needs a desktop — a phone-only user cannot update (the desktop is the admin; acceptable); `app.exe` grows ~3 MB |
| (c) daemon self-download | `ymux-server web update` fetches the release asset from GitHub | headless; no desktop in the loop | adds an outbound network dependency the daemon does not have today; **requires signature verification** — a fetched bundle served to the user's browser is code execution in their session, so a sha256 from an unauthenticated manifest is not enough |

**Recommendation: (b), with (c) as a later opt-in** once there is a signed
manifest to verify against.

Serving details, whichever wins: vite emits content-hashed asset names, so
`/assets/*` gets `Cache-Control: public, max-age=31536000, immutable` and
`index.html` gets `no-cache`. There is no client-side router today (the desktop
routes by window label), so `/` serves `index.html` and the only extra path is
`?popout=<sid>` for a terminal in its own tab. `GET /api/version` reports the
served web version so the add-on's detect step can compare.

**Login — approved from inside ymux, on the mobile-pairing rails.** Raised by
Yossi 2026-09-24; the decision thread has the detail. The browser opens
`https://<domain>/` and gets in only when ymux says so, reusing the pairing
machinery that already exists: single-use token, 5-minute TTL, public
`POST /api/pairing/redeem`, device list with rename / revoke, and an `issue`
endpoint that **already accepts a `scopes` array** the desktop has never sent.
The device token lands in `localStorage` and rides as `Authorization: Bearer`,
or as `?token=` on the two WebSocket kinds (a browser cannot set headers on a
WS handshake). No cookie, so no CSRF surface.

**Browser-initiated, decided 2026-09-24.** The page requests access and shows a
short code; ymux shows an approval card carrying **the same code plus the
requesting IP and User-Agent**; the human matches them and approves. The
standard device-authorization shape. Needs rate limiting, a cap on pending
requests, and `shell:attach` as its own checkbox defaulting off.

Reusing the existing store rather than inventing a second credential path:
`PairedDevice.Status` already runs `pending | active | revoked`, so a request
adds `requested`. The public request endpoint mints the **existing** one-shot
token plus a display code; approving flips the row to `pending` with the chosen
scopes; the polling browser then calls the existing `/api/pairing/redeem` with
the one-shot it already holds. New: the status, a `code` column, and the
request / list / approve / deny endpoints.

**The desktop must be open and SSH-connected for a first pairing**, because the
daemon has no way to reach it otherwise — and that is a feature as much as a
cost: approval is bound to whoever holds SSH access to the box.

**How the request reaches the desktop is still open.** The daemon never dials
the desktop today (every desktop→daemon call is a `curl` the desktop opens over
SSH). So either the desktop polls while the Devices tab is open, or the daemon
gains an outbound client and pushes the request through the reverse tunnel as a
blocking `feed.push` — which turns it into an ordinary ymux approval card that
toasts with every panel closed, reusing the agent hook-gate path. The second is
recommended; the decision thread has the trade.

**Sending scopes at issue time is required, not optional.** Since Phase 95
`"all"` deliberately excludes `shell:attach`, so a browser paired through
today's flow would 403 on every terminal route.

**The security line, stated plainly.** Today a leaked device token exposes
metrics, files under the shared root, and a Claude chat. After this change a
leaked token with the wrong scope is **a shell on the box, over the internet**.
Non-negotiable for v1:

1. New scope **`shell:attach`**, and it is **not** in `AllScopes`. `ParseScopes`
   fails open to "all" for backward compatibility; `shell:attach` must be
   granted explicitly on the device, never implied by `all`. Phones paired
   before this change get no shell.
2. `exec` and `hygiene/kill` stay owner-token only.
3. Redeem endpoint rate-limited; device tokens get an idle expiry the owner
   can set; a revoke tears down live WS attachments immediately.
4. Recommended in the docs, not enforced: Cloudflare Access (or an IP
   allowlist in nginx) in front of `/`. The add-on installer offers to write
   the nginx `allow` block.
5. The daemon never logs PTY bytes (Rule #1) and never logs tokens (Rule #8);
   the WS handlers get the same telemetry wrapper `handle_client_with_telemetry`
   gives the pipe: conn id, start/end, byte counts.

## 8. Phasing

**Phase A landed 2026-09-24 (Phase 95)** — `internal/term`, ~870 lines plus tests.
What shipped differs from the sketch below in one way worth reading: there is **no
per-session object and no ring buffer**. Several clients on one tmux session is
tmux's own multi-client case, so each WebSocket owns one `tmux attach` process and
tmux does the mirroring. That removed the fan-out this document originally
assumed. `workspace.KindTerminal` stays reserved and unimplemented: PTY bytes must
never enter that package's SQLite event log. Live verification is open (Rule #14)
— the smoke list is in FOLLOWUPS.


| Phase | Scope | Size | Verifiable how |
|---|---|---|---|
| A | Go: `terminal` kind, `/term` WS, tmux list/rename/kill, session-meta annotation, `shell:attach` scope | ~870 Go | `go test` + `websocat` into a real box |
| B | Go: workspaces/layout/settings/notes/tickets/feed stores, `humanize` + brief port, hook-bridge method subset, `setup-hooks` env target, session history (§4.2: `ended_at` in `session_meta.rs`, transcript endpoint, resume) | ~1.7k Go + ~100 Rust (CLI) | `go test`; a `claude` run inside an attached tmux fires a gate visible on the events WS |
| C | TS: `Backend` interface, `TauriBackend`, codemod, `WebBackend`, `layoutOps.ts`, capability gating (no `TermStream` — §8.2) | ~2–3k TS | desktop unchanged in behaviour (the regression risk); web build renders against a Phase A/B daemon over plain HTTP on localhost |
| D | `ymux-web` add-on, nginx `location /`, pairing page, Mobile tab → "Web & devices" | ~500 Rust + Go | full path over HTTPS from a phone |
| E | PWA: manifest, service worker, push subscription over the existing WS channel | ~300 TS | "Add to Home Screen" on Android; a hook gate arrives as a notification |

**Phase B is split into six PRs (decided 2026-10-04, DECISIONS):** B1 pure Go ports of
the agent logic (`internal/agent`, Phase 99) → B2 hook listener for browser-created
tmux sessions → B3 `feed.push`/`feed.decide` + the workspace events WS → B4 small verbs
→ B5 workspace state + layout verbs → B6 session history (§4.2, a CLI change). Policy:
the daemon's per-session `auto/block/gate/none`, no `ymux-policy` port.

A and B ship without touching the desktop. C is the merge-risk phase: land it
behind the backend interface with `TauriBackend` first, ship a desktop release on
it, and only then add `WebBackend`. D and E are small once C exists.

Phase E is the answer to the parked Android port (DECISIONS 2026-08-23, "Fork
scan", options A/B/C): **option D — a PWA on this stack**. It closes that
thread; no third platform to keep green.

### 8.1 Phase B — status and handoff (2026-10-05)

Written when Phase B moved from a cloud session to a session on a real machine. Read
this, `docs/DECISIONS.md` (Decided, 2026-10-04 "WEB-DESIGN Phase B split…") and the
FOLLOWUPS P1 entries for Phase 99/100 before touching Phase B. Everything below was
checked against the code on that date; line numbers drift, the function names do not.

| Part | State |
|---|---|
| B1 — `internal/agent` (Phase 99) | **In main** (PR #54). Pure Go ports + translated tests; nothing imported it until B2. |
| B2 — hook listener for browser sessions (Phase 100) + daemon **2.4.2** | **Merged (PR #55), verified live 2026-10-05** on a real box over HTTPS — browser pairing approved on the desktop, `claude` typed into the browser xterm, all hooks folded; the chat (phone) path still answers. The live test found and fixed four bugs (term `List()` always empty on real tmux → hooks wiped on every list; diag page xterm 404; RTL overflow; close 1006). Not verified: the add-on update path, tmux < 3.2. |
| B3 — feed, gate, events WS (Phase 101) + daemon **2.5.0** | **Merged (PR #56), verified live 2026-10-05** — none / gate allow / deny / 120 s timeout / kill-while-pending, Hebrew cards, no phone forward for `term_`. `term/feed.go` + `events.go`; see the vault (`server-go.md`). |
| B4 — small verbs, notes, ports (Phase 102) + daemon **2.6.0** | **Merged (PR #57), verified live 2026-10-05** with the real CLI — set-status / notify / note-* / port detection, notes survive a restart. `term/verbs.go`, `notes.go`, `ports.go`. |
| B5 — browser workspaces + agent layout verbs (Phase 103) + daemon **2.7.0** | **Merged (PR #60), verified live 2026-10-05** with the real CLI — workspaces + version guard, tree, split (creates the session), send fenced to the workspace, titles. Workspaces in a term-owned JSON store (DECISIONS 2026-10-05), REST at `/api/v2/web/workspaces`, not `/api/v2/workspace/state`. |
| B6 — session history (Phase 104) + daemon **2.8.0** + CLI | **Merged (PR #61), verified live 2026-10-05** — a killed claude session stays as history with its cwd, its transcript opens, resume brings it back under the same name. CLI `prune` marks `ended_at` (retention 90 days / 100 rows), keeps unknown fields. **Phase B is complete.** |

**B3 — `feed.push` / `feed.decide` + the events WS.**
- Already ported (`term/hookdispatch.go`): the traffic light and brief folding of the
  desktop's `feed.push` arms (`rpc_server.rs` `feed.push`, ~:1289–1865, the
  `user-prompt-submit` / `stop` / `session-end` / `pre-tool-use` / `notification` arms).
- Missing, desktop behaviour to mirror:
  - a feed store (desktop `FeedStore`, cap 50 items; `FeedItem` in `lib.rs` ~:114 =
    `{request_id, kind, subkind, pane_id?, workspace_id?, title, summary, payload,
    state: pending|allowed|denied|timedout|passive, created_ms, blocking}`);
  - card text via `agent.Humanize` for `stop` / `session-*` / `post-tool-use` /
    `subagent-stop` / `pre-compact` (a stop with a non-degraded brief uses
    `agent.PreBriefText` and `ask · rec` else `delta`, clipped to 160);
  - `user-prompt-submit` returns early (no card); `notification` never makes a card;
  - the blocking wait: `wait_timeout_seconds` default 120, clamped 1–600; a dropped
    channel → deny, an expired timer → `"timeout"` (the CLI treats it as deny);
  - events `feed:item-added`, `feed:item-resolved` `{request_id, decision}`,
    `pane:agent-run` (`agent.Run.Event`), `pane:brief` `{pane_id, entry}`
    (`agent.BriefEntry`); hydration commands `pane_agent_states` (`lib.rs` ~:2599)
    and `pane_briefs` (~:2637) — `term.HookRegistry.Snapshot()` already holds both;
  - **there is no workspace-wide events WS** (§2 correction) — B3 builds it; the
    per-session `…/subscribe` and `workspace/frames.go` are the patterns to follow.
- **Decided 2026-10-05 (DECISIONS):** a browser session defaults to `none`
  (observability only: folded, permission answered `allow`, no card — the desktop's
  Auto); `gate` is opt-in per session, with the desktop's timeout semantics. Events on
  `GET /api/v2/events` behind the terminal gate; feed history in the browser
  (IndexedDB); card language per subscriber.
- De-duplicate: every `pre-tool-use` also arrives via the CLI's fire-and-forget
  `POST /api/v2/hooks/forward` (FOLLOWUPS P2) — ignore `term_` pane ids there, or key on
  `request_id`.

**B4 — small verbs** (desktop `dispatch()` in `rpc_server.rs`): `set-status`
(`pane_id`, `text` → event `pane:status`), `notify` (`title`, `body`, `kind` →
`notification:new`), `note-add/list/update/done/delete` (delegates to `notes.rs`),
`port.opened` / `port.closed` (`workspace_id`, `port`, `addr`, `family` →
`port-detected` / `port-undetected`; detection only in browser mode). B2 answers all of
these with JSON-RPC error -32000 today.

**B5 — workspace state + layout verbs.** `/api/v2/workspace/state` does not exist (§4
correction); the `Workspace` row is only `{ID, Name, CreatedAt}` and the workspace
store has no ALTER yet (copy `chat/chat_store.go`'s `ALTER TABLE … ADD COLUMN` pattern).
Verbs: `tree`, `split` (daemon understands only "split leaf X"), `send`, `send-key`
(`agent.TranslateKey` is ready), `set-pane-title`, `set-pane-annotation`.
`pane.scrollback` is a deliberate error stub on the desktop (Rule #1) — a daemon
`tmux capture-pane` would be new behaviour, not a port; decide explicitly.

**B6 — session history (§4.2), the one CLI change.** `cli/src/session_meta.rs`
`prune` (~:132) DELETES every entry with no live tmux session, and treats a failed
`tmux` (non-zero exit) as "no sessions" — wiping the file. The entry has no `cwd`
field. `SessionMetaEntry` has no catch-all for unknown fields, so an old CLI on a remote
re-saves the file and silently drops `ended_at` — mixed-version remotes lose history
until re-bootstrapped. Readers to update together: the CLI, the desktop's copy of the
struct (`lib.rs` ~:9491) and `server/internal/term/meta.go`.

**Working notes for the session that picks this up.**
- Rebake = download the `ymux-server-linux` artifact of the PR's ci-windows run
  (`gh run download <run> -n ymux-server-linux -D <tmp>`, then copy — `gh run
  download` will not overwrite existing files). The two local rebakes in Phase 100 were
  a one-off cloud-session exception to Rule #17 (that session could not download
  artifacts); not a precedent.
- Rule #17 holds: builds and tests run on CI. Live checks on the machine (a real
  daemon, tmux, `claude`) are exactly what Rule #14 asks for — do them.
- The desktop offers a server add-on update only when the version string changes
  (`addons.rs` ~:752). Bump `core.Version` + `INSIGHTS_VERSION` + the four sdk-gen
  version strings together whenever a server change must reach already-updated hosts.
- A daemon restart under systemd can kill the shared tmux server, desktop sessions
  included — accepted, documented in `docs/ymux-server/DEPLOYMENT.md`.

### 8.2 Phase C — plan (2026-10-05)

Approved by Yossi 2026-10-05 (DECISIONS, "Phase C plan"). Phase numbers are allocated at PR time
(`git log --all --grep=Phase`; 105 is taken by the Context Rail PRs #58/#59).

#### Survey (main = b3aec42)
- 42 files import `@tauri-apps/*`: api/core 39, api/event 9, plugin-opener 6,
  plugin-dialog 5, api/window 4, api/webview 3, api/app 1.
- 192 distinct `invoke` names, ~266 sites (App.tsx 91, FileManagerPane 36, PaneView 14,
  BrowserPane 11, settings.ts 10, terminalInstance 7, MobilePairing 7, …). Two dynamic
  sites (`invoke(cmd, …)` in YmuxToolsTab, AddonsTab).
- 29 `listen` events, 24 of them in App.tsx's onMount.
- PTY path: `pane_connect` → sessionId → `TerminalInstance.attach(sid)` (onData →
  `pty_write`, `fitAndResize` → `pty_resize`); output through ONE global `listen("pty:data")`
  in App.tsx:3846 demuxed via `sessionToPane`, plus a copy in PopoutTerminal.tsx:98.
  `writeData` already coalesces per rAF, so a WebBackend only needs a streaming TextDecoder.
- Rust does three things to output the browser will not get in v1: UTF-8 reassembly
  (pty_decode — redone in TS), the opt-in bidi filter, the OSC 9/99/777 notification parser.
- Tests: `node --test src/*.test.ts` (15 files) — pure modules only.

#### Shape
```
app/src/backend/
  types.ts    Backend, Capability, UnsupportedError
  tauri.ts    TauriBackend   (invoke / listen / emit, caps = all)
  web.ts      WebBackend     (C5)
  webRoutes.ts command → handler table (C5)
  index.ts    `backend` singleton, picked synchronously at module load
```
`interface Backend { kind: "tauri"|"web"; call<T>(cmd, args?): Promise<T>;
on<T>(event, cb): Promise<UnlistenFn>; emit(event, payload?); caps: ReadonlySet<Capability> }`
(C1 shipped `call` / `on` / `emit`; `caps` lands in C3. No `term()` — see C2.)
`on` stays async so every `await listen(…)` ordering in App.tsx is preserved 1:1.

#### PRs

C1 is **Phase 106**.

##### C1 — seam + codemod (desktop, no behaviour change)
- `backend/types.ts`, `tauri.ts`, `index.ts`. `call` = `invoke`, `on` = `listen`.
- One-off codemod script (scratch, not committed): `invoke<T>("x", a)` →
  `backend.call<T>("x", a)`, `listen<T>(` → `backend.on<T>(`, imports rewritten.
  Generic parameters kept verbatim (Rule #5). The two dynamic sites by hand.
- Guard: `src/backendSeam.test.ts` fails when any file outside `src/backend/` imports
  `@tauri-apps/api/core` or `@tauri-apps/api/event`.
- Vault: frontend-lib.md (new "backend seam" section) + every covering page whose
  files the codemod touched (re-stamp; prose only where it names `invoke`).
- Size: ~150 new + ~300 mechanical line edits across 39 files.

##### C2 — dropped (2026-10-05, while writing C1)
The `TermStream` refactor is not needed, and it was the one change in C1–C3 that
touched the desktop's PTY hot path. The WebBackend can speak the desktop's own PTY
contract instead: `pane_connect` opens the session's attach WS and returns the
session id, `pty_write` / `pty_resize` become frames on that WS, and incoming frames
are emitted locally as `pty:data` / `pty:exit` with the same payloads Rust sends
(`TextDecoder("utf-8", {stream:true})` per session does `pty_decode`'s job). App.tsx,
`TerminalInstance` and PopoutTerminal stay exactly as they are. The release that
precedes `WebBackend` is therefore C1 + C3.

##### C3 — capabilities + import-safety (desktop, no behaviour change)
**Phase 107.** Shipped as `backend.host` (window / dialog / opener / drag-drop) +
`backend.can(cap)`; the list of gated entry points is in vault frontend-lib.md.
- `Capability` set (~17): localPanes, ssh, wsl, browserPane, popout, fileManagerLocal,
  fileManagerRemote, diffPane, worktrees, tickets, skills, addons, mobilePairingAdmin,
  updater, fonts, stt, tray, portForward, provisioning, insights.
  TauriBackend = all, so the desktop renders exactly what it does today.
- Gate the sidebar menus, command palette, Settings tabs, pane-kind pickers, and
  `paneCaps()` (intersect with backend caps).
- window / webview / dialog / opener: wrap the call sites that run at import or boot
  (index.tsx `getCurrentWindow().label`, logger, platform `host_platform`) so a
  browser without `__TAURI_INTERNALS__` loads the bundle. `opener` → `window.open`,
  `dialog` → gated off in web.

→ **Desktop release (0.5.x) carrying C1 + C3.** Yossi smokes on Windows + Mac against a
checklist (connect local + SSH, split/close/swap, popout + reattach, kill, restart →
restore, feed gate allow/deny, file manager both sides, Browser pane, Settings, update
check). WebBackend does not start before that release is green.

##### C4 — the daemon serves the bundle (Go, small)
- `ymux-server` serves `~/.ymux/server/www/current/` at `/` when it exists
  (`index.html` no-cache, `/assets/*` immutable, CSP for the app); the diagnostic page
  stays at `/diag`. `/api/version` gains `web_version` + `web_caps`.
- ci-windows frontend job uploads the vite `dist/` as a `ymux-web` artifact, so a
  box can be loaded by hand (`gh run download` → copy into `www/<ver>/`, symlink
  `current`). This is the serving half of Q2 option (b); the add-on upload is Phase D.
- Settings store (decided 2026-10-05: on the daemon, shared by every browser):
  `GET/PUT /api/v2/settings` → `<dir>/web-settings.json`, an opaque JSON document +
  `version` (same 409 guard as web workspaces, atomic write), `settings:changed` on the
  events WS. The daemon never parses the fields — the desktop `Settings` type stays the
  only schema.
- Version bump (2.9.0) with the usual five places; rebake.

##### C5 — WebBackend: auth, workspaces, terminal
**Phase 109**, daemon 2.10.0 — started before the C1 + C3 smoke (Yossi, 2026-10-05).
As built (vault frontend-lib § The browser arm): a leaf's pane id is also its session's
hook pane id (the daemon takes `pane_id` on create), so no id ever has to be rewritten;
`WebLogin.tsx` is the sign-in; the browser's "new workspace" skips the wizard. Not yet:
`mode: "claude"` / `cmd` on `pane_connect` (the pane opens a shell), colour / emoji /
groups / order of a browser workspace, `backend:resync` re-seeding the lights.
- Auth: token in localStorage; none → a small Solid login screen running the Phase 96
  request-access flow (code shown, desktop approves, poll, redeem). 401 → back to it.
- PTY (the desktop contract, see C2): `pane_connect` → POST term/sessions (workspace_id,
  policy) or an existing name, then opens WS `/api/v2/term/sessions/{name}/attach` and
  returns a session id; binary frames → per-session `TextDecoder("utf-8",{stream:true})`
  → local `pty:data`; `{"type":"exit"}` / close → `pty:exit`; `pty_write` / `pty_resize`
  → WS frames; `pane_disconnect` → close WS; `pane_kill_session` → DELETE.
- Workspaces: `/api/v2/web/workspaces` mapped to the desktop `Workspace` shape with a
  synthesized ssh-shaped connection (host = location.hostname) — the panes ARE remote
  tmux, so RTL profile and paneCaps answer "remote". Layout ops run client-side in
  `layoutOps.ts` (ported from lib.rs split_pane_in / close_pane_in / set_split_ratio_in
  / swap_two_panes_in_layout / reset_all_split_ratios, with their tests), then PUT with
  `version`; 409 → take the returned doc, re-apply the op once, else surface.
- Events: one WS `/api/v2/events?lang=…`; `initBackend` awaits `hello` and serves the
  boot reads (`pane_agent_states`, `pane_briefs`, `feed_list`, `notifications_list`,
  `list_detected_ports`) from it. Reconnect with backoff → new hello → a synthetic
  `backend:resync` event App.tsx re-seeds on (TauriBackend never emits it).
- Settings: `settings_load` / `settings_save` → the daemon store from C4 (defaults when
  empty); `settings:changed` from another browser re-applies live.
- Any command not in the route table rejects with `UnsupportedError` (logged once per
  name) — a hidden-by-caps button should never reach it; if one does, it is a C3 bug.

##### C6 — WebBackend: the ymux surfaces
**Phase 110**, daemon 2.11.0. As built (vault frontend-lib § The browser arm): Monitor via
same-origin insights fetches; the File Manager's remote side over the Files API (no
rename / mkdir / copy / zip / upload-from-machine yet — FOLLOWUPS); "claude" mode panes
run claude as the session's argv; the gate card's fallback title is humanized on the
daemon. Not in it: claude quota (`claude_usage_fetch` is a CLI probe over SSH on the
desktop), session history UI (none on the desktop either yet), IndexedDB feed history.
- feed_decide (WS frame), feed history persisted in IndexedDB, notes CRUD,
  notifications clear, history + transcript + resume (Sessions panel), files via
  `/api/v2/files/*`, insights / claude usage via the existing daemon routes,
  pane title / annotation via the layout leaf.

#### Verification
- C1 + C3: CI (tsc + node tests + vite + both platforms), then the desktop release smoke.
  "Compiles" is not "verified" — the release smoke is the gate.
- C4–C6: live on 111.yossiyehezkel.com over HTTPS — chronoscope headless + Yossi's
  phone/laptop: pair → create workspace → split → real `claude` with gate → approve the
  card in the browser → reload → layout and feed restored → kill → history → resume.

#### Not in Phase C
Popouts as tabs, the bidi filter, OSC notifications from the web PTY, tickets / skills / diff pane in web mode, the ymux-web add-on (D), PWA (E).

### 8.3 Phase D — plan (2026-10-06)

Decided with Yossi (DECISIONS 2026-10-06). Phase 111 first made hook routing survive a
daemon restart, since every add-on update restarts it.

- **D1 — Phase 112, the `ymux-web` add-on.** Ships the frontend embedded in the desktop
  binary (no tarball, no second build); install/update upload it to
  `~/.ymux/server/www/<ver>-<hash8>/` and swap `current`; **automatic on connect** when
  a host has the add-on and its label differs. nginx-proxy already proxies `/` to the
  daemon, so nothing changes there. Vault backend-remote § web_addon.rs.
- **D2 — Phase 113, Web & devices.** The device list shows each device's scopes and a
  "Terminal access (shell:attach)" checkbox (default off) → owner
  `PUT /api/v2/devices/{id}/scopes` through `pairing.rs daemon_curl` (whose path allow-list
  grows by exactly that route). The daemon's browser-approve endpoint gets the
  `NormalizeScopes` validation the PUT already has.
- Then one desktop release carrying C1 + C3 + D, smoked once on Windows + Mac.

## 9. Questions (tracked in `docs/DECISIONS.md`)

- **Q1 truth model — DECIDED 2026-09-10:** server-native workspaces; tmux
  sessions are the shared reality (§4).
- **Q2 bundle delivery — DECIDED 2026-10-05: (b), the `ymux-web` add-on** (§7.1);
  the daemon's serving half lands in Phase C4, the upload in Phase D.
- **Q3 "local parallel" — DEFERRED 2026-09-10** until the remote path is proven
  end-to-end. It would mean the Rust backend implementing the same HTTP/WS API
  — two implementations of one contract in two languages, the macOS-branch
  lesson again. When it returns, the candidate is the same CGO-free Go daemon
  running locally, with the desktop as one more client.
- **Q4 shared session, two clients** — desktop and browser attached to one tmux
  session both see output (tmux does that); who owns the hook gate is decided
  by §6. Recommendation: accept for v1, documented.
- **Q5 session history — recommended, lands in Phase B:** §4.2.

## 10. What v1 deliberately does not do

No SSH from the browser to a *third* host (the daemon is the host). No port
forwarding. No local panes. No in-app browser webview (use a tab). No in-browser
provisioning wizard. No sync of pane layouts between desktop and browser. No
CRDT — last-writer-wins with the existing `version` guard.
