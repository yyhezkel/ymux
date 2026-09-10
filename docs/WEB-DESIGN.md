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
| Event streaming | `WS /api/v2/workspace/events`, typed frames in `workspace/frames.go`, `seq` replay | carries every non-PTY event (§5) |
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
- **`WS /api/v2/term/{session_id}`** — a **separate binary WebSocket**, not the
  JSON workspace stream. Raw bytes both ways; a single text frame
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
`PUT`s the document to the daemon (`/api/v2/workspace/state` already exists with
an optimistic `version`). The daemon stores it opaque. The desktop keeps its Rust
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
- `TerminalInstance` (`terminalInstance.ts`) takes a `TermStream` instead of a
  session id + global listeners. The RTL modules and `pty_decode`-equivalent
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
dial `YMUX_SOCKET_ADDR`. Today that is the reverse-tunnel port. In browser mode
`setup-hooks` (already run by the daemon's install path) writes the **daemon's
hook listener** address instead — `hooks/hooks.go` already binds one and reports
it through `core.AddrSink`. Same challenge dialect, same CLI binary, no CLI
change beyond reading one more env source.

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

### 7.1 Web bundle delivery (Q2, open)

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

**Login.** Open `https://<domain>/` → pairing page → paste the code / scan the
QR the desktop's Mobile tab already generates → `POST /api/pairing/redeem` →
device token stored in `localStorage`, sent as `Authorization: Bearer` and as
`Sec-WebSocket-Protocol` on the two WS kinds. Origin-checked WS upgrade. No
cookie, so no CSRF surface. The device shows up in the existing device list with
rename / revoke.

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

| Phase | Scope | Size | Verifiable how |
|---|---|---|---|
| A | Go: `terminal` kind, `/term` WS, tmux list/rename/kill, session-meta annotation, `shell:attach` scope | ~800 Go | `go test` + `websocat` into a real box |
| B | Go: workspaces/layout/settings/notes/tickets/feed stores, `humanize` + brief port, hook-bridge method subset, `setup-hooks` env target, session history (§4.2: `ended_at` in `session_meta.rs`, transcript endpoint, resume) | ~1.7k Go + ~100 Rust (CLI) | `go test`; a `claude` run inside an attached tmux fires a gate visible on the events WS |
| C | TS: `Backend` interface, `TauriBackend`, codemod, `WebBackend`, `layoutOps.ts`, capability gating, `TerminalInstance` on `TermStream` | ~2–3k TS | desktop unchanged in behaviour (the regression risk); web build renders against a Phase A/B daemon over plain HTTP on localhost |
| D | `ymux-web` add-on, nginx `location /`, pairing page, Mobile tab → "Web & devices" | ~500 Rust + Go | full path over HTTPS from a phone |
| E | PWA: manifest, service worker, push subscription over the existing WS channel | ~300 TS | "Add to Home Screen" on Android; a hook gate arrives as a notification |

A and B ship without touching the desktop. C is the merge-risk phase: land it
behind the backend interface with `TauriBackend` first, ship a desktop release on
it, and only then add `WebBackend`. D and E are small once C exists.

Phase E is the answer to the parked Android port (DECISIONS 2026-08-23, "Fork
scan", options A/B/C): **option D — a PWA on this stack**. It closes that
thread; no third platform to keep green.

## 9. Questions (tracked in `docs/DECISIONS.md`)

- **Q1 truth model — DECIDED 2026-09-10:** server-native workspaces; tmux
  sessions are the shared reality (§4).
- **Q2 bundle delivery — OPEN:** three options in §7.1. Recommendation: add-on.
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
