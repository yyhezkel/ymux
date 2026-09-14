---
vault: backend-core
covers:
  - app/src-tauri/src/lib.rs
  - app/src-tauri/src/main.rs
  - app/src-tauri/src/sessions_overview.rs
---

# Backend core — `lib.rs`

**Windows are built programmatically, not in `tauri.conf.json`** — its `windows` array is
empty on purpose. `main` is built in `.setup()` with `.devtools(false)`; `popout_pane`
builds `popout-<sid>` terminal windows and `workspace_browser::browser_popout_open`
builds `browser-popout-<ws>`. All three share the same three non-negotiables: the
builder call must be in an **`async`** command (on Windows `WebviewWindowBuilder`
deadlocks from a synchronous one — the shell appears, the webview stays blank white), the
URL must be a **clean `index.html`** with the id carried by the window LABEL (the built
app's asset protocol serves a blank page for any suffixed path), and lifecycle is wired
through `on_window_event` on `Destroyed`, never `CloseRequested`. Each label prefix needs
its own file in `capabilities/`; the globs are prefix-anchored, so `browser-popout-*` is
not covered by `popout-*`.

`teardown_workspace_runtime` is the single place a workspace's runtime state dies: the
Browser child Webview, its pop-out OS window (`close_popout_window` — otherwise the
window outlives the workspace), the browser session dir, the bootstrap verdict, and the
reverse-tunnel state.

**13,966 lines, and about 1,700 of them are `#[cfg(test)]` at the bottom.** It is the
"everything else" module: app state, the workspace data model and its persistence, the
PTY and SSH spawn paths, the multiplexer (zellij / tmux) plumbing, and ~70 Tauri
commands. `main.rs` is 6 lines — the Windows-subsystem flag and `app_lib::run()`. Never
put logic there.

## Shape of the file, in order

| Lines | What |
|---|---|
| 1–40 | `mod` declarations for the 34 sibling modules; `use ymux_tunnel as tunnel` |
| 40–280 | `AppState`, `AgentRunState`, `FeedItem`/`FeedStore`, `NotificationItem`, `LoadState` |
| 305–830 | `WorkspacesFile`, config paths, `machine_id`, `save_to_disk`/`load_from_disk`, migrations |
| 1190–1570 | Pure layout-tree walkers (`close_pane_in`, `set_split_ratio_in`, …) |
| 1570–2220 | Shell detection, UTF-8 arg/env quoting, Smart Connect script builder, `emit_data` |
| 2224–3100 | `spawn_local_pty`, the zellij verb wrappers, tmux attach script, `spawn_wsl_pty` |
| 3100–4600 | SSH: key offer/generate, `connect_and_authenticate`, `spawn_ssh`, `TunnelLease` |
| 4600–5000 | Port watcher, workspace↔ssh-handle lookup |
| 4986–7270 | Workspace/group/layout Tauri commands |
| 7274–9500 | Connect path: `workspace_ensure_connected`, `pane_connect`, session listing/labels/owners |
| 9798–10180 | `pane_kill_session`, `pane_disconnect`, pty write/resize, feed, `doctor`, `popout_pane` |
| 10185–10760 | `run()` — the Tauri builder |
| 10759+ | Unit tests, one `mod` per concern |

## Key types

- **`AppState`** ([lib.rs:146](../../app/src-tauri/src/lib.rs)) — the single managed Tauri
  state, `Clone` because every field is an `Arc<Mutex<…>>` and the RPC server task needs
  its own handle. It **wraps** `ymux_core::CoreState` at `state.core`: `sessions`,
  `pane_sessions`, `forwards`, `port_watchers`, `detected_ports`, `port_watcher_tasks`,
  `diff_pane_watchers`. Next to those, `port_watcher_hosts` (Phase 86.C, on `AppState`
  itself): one remote `port-watch` per (host, port, user) — the first workspace to connect
  is the *owner*, siblings are *subscribers*, and the owner slot is released when the
  watcher's exec channel ends or the lease drops so the next `try_ensure_port_watcher`
  from any sibling re-spawns. Taken alone, never nested under another lock. Everything else — `workspaces`, `load_state`, `notifications`,
  `pane_status`, `agent_runs`, `feed`, `notes`, `settings`, `recent_paths`,
  `console_buffer`, `claude_paths`, `bidi_filters`, `workspace_browsers`,
  `browser_create_lock`, `bootstrap_guard`, `tunnel_registry` — is app-shell concern and
  lives on the outer struct. **Reach russh state through `state.core.<field>`.**
- **`Session` / `LocalSession` / `SshSession` / `SshCmd`** — defined in
  `ymux-core`, re-exported here so `crate::Session` still resolves. See `crates.md`.
- **`Connection`, `LayoutNode`, `Workspace`** — `ymux-types`. `LayoutNode::Pane` carries
  its own optional `connection`, so one workspace's leaves can target different hosts.
- **`LoadState`** — `Loaded | Failed`. A poison flag: if `load_from_disk` hit a real
  read/parse error, `persist` refuses to write, because saving in-memory state over a
  file we failed to understand destroys the user's workspaces.
- **`PaneAgentState` / `AgentRunState` / `PaneAgentSnapshot`** — per-pane Claude state,
  in `AppState.agent_runs`. `apply_hook(subkind, notification_type)` is the transition
  table and it is the **single owner** of the state machine; the frontend only paints
  what it is handed. `NEEDS_INPUT_NOTIFICATIONS` and `RESUMED_NOTIFICATIONS` list the
  `notification_type` values that mean "blocked on the user" and "unblocked". A `stop`
  arriving after a notification still wins, an unmapped notification changes nothing,
  and a long turn does not keep resetting its own clock — all of that is pinned by unit
  tests in the same file. Transitions reach the UI as the **`pane:agent-run`** event via
  `emit_agent_run_event`, which carries `(started, avg, state, since, seq)`; `seq` bumps
  only on an applied transition, so a no-op skips the emit. In-memory and
  session-scoped — never persisted. Its sibling store is
  **`AppState.briefs`** (`HashMap<pane_id, PaneBriefEntry>` from `brief.rs`, covered in
  `backend-rpc.md`): per-pane agent briefs + last user prompt, same in-memory-only
  rationale, emitted as `pane:brief` via `emit_brief_event` and hydrated by the
  `pane_briefs` command — the `pane_agent_states` pattern verbatim.
- **`workspace_set_intent`** (BRIEF) — sets/clears `Workspace.intent` (trimmed;
  empty clears), persists atomically, emits `workspaces:changed`, returns the
  updated `Workspace`. The log line carries the intent's LENGTH only — it is user
  content. Adding the field bumped `WORKSPACES_SCHEMA_VERSION` 2→3.
- **`workspace_set_tabs_mode`** — flips `Workspace.tabs_mode` and emits
  `workspaces:changed`. The layout tree is not touched; see `crates.md` for why this is
  a flag and not a `LayoutNode` variant.
- **`workspace_pin_project_folder`** — persist a folder as a child workspace (CLONE of
  the parent's connection, `single_terminal_layout`). It only persists; validation is
  the caller's `project_folder_probe` (`backend-panes.md` § Git), and since the no-git
  fix the command takes **`is_project_root` as a parameter** — the probe's verdict —
  instead of hardcoding `true`. A directory without a repo therefore pins as a plain
  demoted folder (the same state `workspace_set_project_root(false)` produces, no
  worktree scan) rather than being refused; the sidebar's "Check for a git repository"
  promotes it after a later `git init`. Refusals that remain: empty path, unknown
  parent, a parent already inside a project folder, and the same path already pinned
  under the same parent.

## Persistence — the part to get right

`%APPDATA%\ymux\workspaces.json`, via `save_to_disk` ([lib.rs:838](../../app/src-tauri/src/lib.rs)).

1. Serialize to pretty JSON.
2. **Three-way merge before writing.** `LAST_KNOWN` (a `static Mutex<Option<String>>`)
   holds the file text as this process last read or wrote it. `save_to_disk` re-reads
   the file and hands `(ours, base, theirs)` to `workspaces_merge::reconcile`. Reason:
   a stable build and a dev build share `%APPDATA%` unless someone sets
   `WINMUX_CONFIG_DIR`, and a plain dump is last-write-wins across the whole document —
   the older binary silently drops every field its structs don't know.
3. **The schema gate**, between reading the file and merging onto it.
   `WORKSPACES_SCHEMA_VERSION` (currently 2) is stamped on every write through
   `serialize_with`, not by assigning the field — the invariant is "what we WRITE is
   current", and serialization is the one place that cannot be bypassed.
   `schema_gate(on_disk, last_written)` is a pure function (extracted for the same
   reason `resolve_effective_session_name` was: the cases that matter need two binaries
   sharing a config dir, which no test can arrange) returning one of three things:
   **Refuse** when the file on disk is NEWER than this build — a hard `Err`, because
   merging would mean understanding keys we have never seen; **WarnDowngrade** when an
   older build got there first — write anyway, since refusing would leave the user
   unable to save for as long as that build is open, and the merge in step 2 is the
   real repair; **Write** otherwise. `schema_version_of` is deliberately tolerant —
   absent, empty, malformed and version-less all read as "carry on" — because a
   refusal hangs off it and a stray byte must never lock a user out of saving.
   **Know the limit:** this cannot stop an already-shipped 0.4.x build, which never
   reads the field and will write v1 back over a v2 file. Step 2 is what covers that
   case; the gate adds the log line that was missing when it cost hours in 2026-08.
4. Write `workspaces.<pid>.tmp`, `write_all`, **`sync_all`**, then `rename`. Rule #7.
5. `remember_file_text` updates the merge base.
6. Log line records `N workspaces: R root / N-R nested / P repo` — the tree *shape*,
   not just a count, because two pinned folders once lost `parent_id` with nothing in
   the log to bracket when.

`load_from_disk` repairs on the way in and each repair is logged: WSL→Local connection
rewrite (`migrate_wsl_workspaces`), `backfill_sort_orders`, `normalize_parents`,
`migrate_legacy_project_folders`. Other files in the same dir, each with the same
tmp+rename discipline: `machine-id` (stable per-install id, deliberately **not** in
settings.json so "Reset all settings" can't change this machine's identity),
tmux labels, session owners.

## Spawning a shell

`pane_connect` ([lib.rs:7949](../../app/src-tauri/src/lib.rs)) is the front door and takes
a wide argument list because every connection mode funnels through it: `persistent`,
`mode` (`default | tmux | plain | cmd | claude`), `cwd_override`, `cmd`, `claude_args`,
`tmux_session_name`, plus the credential arguments.

- Connection resolution prefers **the pane's own** `connection`, falling back to the
  workspace's. That is what stops an SSH workspace from quietly spawning a local shell
  in a pane that was split off a FileManager or Browser pane.
- **Local** → `spawn_local_pty`: ConPTY pair via `portable_pty`, shell from
  `pick_default_shell`, a reader thread that emits `pty:data` and cleans itself out of
  the session maps on child exit. `persist_session: Option<String>` picks the
  multiplexer by `cfg`: **zellij on Windows, tmux on macOS/Unix**. This is the one place
  the platforms genuinely differ, and it is deliberate (CLAUDE.md § Platforms).
- **SSH** → `connect_and_authenticate` then `spawn_ssh`: auth chain is ssh-agent
  (OpenSSH + Pageant, each wrapped in `catch_unwind` to absorb upstream panics) →
  explicit key file (optional passphrase) → default `~/.ssh/id_*` → password. Then
  best-effort bootstrap, `tcpip_forward(0)` for the reverse tunnel, env file via
  `ymux-tunnel`, shell channel with `set_env` for the `YMUX_*` vars, `request_pty`,
  `request_shell`, channel-pump task.
- `emit_data` ([lib.rs:2370](../../app/src-tauri/src/lib.rs)) is UTF-8 **boundary-safe** —
  it buffers a partial multibyte sequence rather than emitting a broken string. Do not
  "simplify" it.

## Multiplexer wrappers

Zellij verbs are built as argument vectors (`zellij_args_list`,
`zellij_args_delete_force`, `zellij_args_write_chars`) and run through
`zellij_try`/`zellij_run`, which classify spawn errors into a `ZellijOutcome` rather
than bubbling an `io::Error`. Never build these by string concatenation (Rule #3), and
check `docs/ZELLIJ.md` for what our pinned 0.44.3 binary actually supports before adding
a verb — zellij.dev documents a different version.

tmux is the SSH-side equivalent: `TMUX_LIST_FORMAT` + the `<<<YMUX_META>>>` marker frame
the listing output so `parse_tmux_sessions` can read it back unambiguously.
`session-meta` labels cross the wire **hex-encoded** (`hex_utf8`) so Hebrew/RTL labels
never meet shell quoting.

**A session NAME is a security boundary, because one path types it into a shell.**
`build_zellij_attach_command` produces a line that is typed verbatim into the user's
cmd.exe or PowerShell 900ms after spawn, and a session name can come from a user's pane
title. Until 2026-08-23 the sanitizer replaced only `.`, `:` and whitespace — tmux's own
blockers — so `;`, `&`, `|`, `$`, backticks, quotes and `>` rode a title straight into
that line, and a pane titled `work; calc` ran `calc`. Two things now hold:

- `session_name_char_is_safe` is a **whitelist at the source**: ASCII alphanumerics,
  `_`, `-`, and any non-ASCII character that is neither control nor separator. The
  whitelist lives here rather than as quoting at each use site because **there is no
  quoting that is correct in cmd.exe and PowerShell at once** — they disagree about
  `^`, `%`, backticks and single quotes — and that line has to parse in both. The
  non-ASCII clause is what preserves Phase 23.I's promise that a Hebrew or CJK title
  becomes a session of the same name: every metacharacter in all three shells is ASCII.
- `build_zellij_attach_command` returns `Option` and **refuses** a name it cannot type.
  Only a picker-supplied name (a session made outside ymux by hand) can reach that.
  Refusing beats mangling, which would silently attach to a session other than the one
  chosen; the pane is left in a plain working shell, the same failure mode the function
  already accepts when zellij is missing.

The tmux/SSH side was already correct — `build_tmux_attach_script` and
`tmux kill-session` use `shell_quote`, and the zellij verbs above are argv-only. The
Windows zellij line was the single hole. Note the historical trap: the comment at that
call site asserted a sanitizer named `sanitize_session_name` **that has never existed in
this tree**, and the test guarding it only ever fed it `ymux-p_1a2b_0` — a name that
could not fail.

## Which folder a session belongs to

`pane_list_tmux_sessions` **annotates and never filters** — the picker's
*This folder / Whole server* toggle is a client-side view of one response, so a
session outside the scope stays one click away instead of vanishing. The verdicts are
stamped by `annotate_session_scope`, which loads `session-owners.json` and delegates to
`annotate_scope_with` — the disk-free core where every rule actually lives, and the one
the tests drive.

Two independent signals feed it, and it needs both:

- **`#{session_path}`** — the sixth field of `TMUX_LIST_FORMAT`, giving `in_cwd` via
  `path_is_within`, which insists on a separator boundary so `/srv/app2` is not "inside"
  `/srv/app`.
- **`session-owners.json`** (`%APPDATA%\ymux`, host → session name → `SessionOwner`),
  giving `owned`. This half exists because `zellij list-sessions` reports **no directory
  at all**, so on Windows ownership is the only workspace signal there is.

`foreign: Option<ForeignScope>` (2026-08-24) is the third verdict and the "Whole server"
view's mess-guard: a row nobody can place is free to attach, a row we *can* place already
belongs to someone. It is `Some` when another workspace claimed the session
(`ForeignKind::Workspace`, labelled with that workspace's name from `workspaces.json`) or
when a known `cwd` sits outside the caller's folder (`ForeignKind::Folder`). Two rules
govern it, both asserted in `tmux_list_parse_tests`:

- **Never `Some` while `owned || in_cwd`.** The workspace view is exactly the complement
  of this field, so the badge cannot appear there and the frontend needs no scope check.
- **An unknown `cwd` with no ownership row is never foreign.** `cwd: None` means we do
  not know, never "somewhere else" — silence beats a fabricated warning.

`project_path` is optional and **`None` means unscoped, which is load-bearing**: session
restore and `pane_probe_tmux_sessions` share these paths and must see everything.

`owner_cwd` (Phase 90) is a fourth field stamped in the same pass: the cwd recorded in
`session-owners.json` at claim time, whichever workspace claimed it. It exists only so the
active-sessions overview has something to group a zellij row under (zellij reports no live
cwd). It is a snapshot that can go stale and it feeds **no** verdict — `owned` / `in_cwd` /
`foreign` never read it, and the picker ignores it.

## Active-sessions overview — `sessions_overview.rs` (Phase 90)

674 lines. The sidebar's right-click **Active sessions…** dialog: every multiplexer
session on the workspace's machine, grouped by directory, with a one-line agent summary
and a status (`idle | working | waiting_input | error | unknown`) per row, and three row
actions. The **list** is `pane_list_tmux_sessions` with `project_path: None` — no new
list command. The module owns what the picker never needed:

- **`sessions_overview_summarize(workspace_id, names, lang)`** — capture the last 40 lines of
  each named session (240 chars per line) and run **one** `claude -p … --output-format json`
  over all of them, **on the machine that holds the sessions**. Over SSH the capture loop and
  the model call are a single remote pipeline (`build_ssh_summary_script`: `tmux capture-pane
  -p -t "=$s:" -S -40 | cut … | bash -lc '<claude> -p …'`), so screen bytes never cross to
  the desktop. macOS captures through `local_tmux_output` and Windows through
  `zellij -s <name> action dump-screen` (`zellij_args_dump_screen`, argv), both framed in
  memory and piped into the local `claude` on stdin (`run_local_claude`, the twin of
  `claude_usage::run_local_usage_probe`; `resolve_claude_binary` grew a `claude.cmd`
  fallback for npm installs on Windows). Legacy WSL workspaces get an `Err` and no summaries.
  Sessions are **indexed, not named** in the prompt so the model never echoes a name back,
  and the prompt is one line of ASCII with no `"` or `%` because a `.cmd` on Windows is
  spawned through `cmd.exe /c`. `parse_summary_envelope` is lenient by design: a bad row is
  dropped, a missing row is `unknown`, a body that is not JSON is `unknown` for everyone —
  never an `Err`, because the list is still worth showing. **Rule #1:** a capture is PTY
  content and the answer is derived from it; the log line carries counts, byte totals, the
  envelope's `subtype` / `is_error` and a duration, nothing else.
- **`sessions_kill_by_name(workspace_id, name)`** — a session one of our panes holds goes
  through `kill_pane_session_inner` (PTY + maps torn down the tested way); anything else goes
  straight to `kill_target` and releases the ownership claim on `killed | already_gone`.
  `KillTarget` + `kill_target` were lifted out of `kill_pane_session_inner` for exactly this —
  a pure move, so there is still one implementation of "kill".
- **Open is `workspace_open_session` in lib.rs (Phase 90.B)** — the third child-creating
  command beside `workspace_pin_project_folder` / `workspace_open_worktree`, same construction
  (a CLONE of the root's connection, `single_terminal_layout`, no `sort_order`). It walks up
  to the root first (the dialog may have been opened from a project-folder child; sessions
  belong to the host), then is **idempotent on `Workspace.tmux_session`**: a row already opened
  for that name anywhere under the root is activated, never duplicated. Placement is
  `pick_session_parent`: the deepest `is_project_root` descendant whose `cwd` contains the
  session cwd (`path_is_within`, boundary-aware, `session_workspace_tests`), else the root —
  it never pins a folder on the user's behalf. The frontend then attaches the row's single
  pane; the current screen is never touched. `tmux_rename_session` also moves
  `tmux_session` on every row of the same host (`conn_same_host`) and re-persists.
- **Rename is `tmux_rename_session` in lib.rs**, registered since 23.G and unused until now.
  Phase 90 gave it `validate_tmux_rename_target` (ASCII letters, digits, `_`, `-`, ≤64 —
  stricter than `session_name_char_is_safe`, because the 23.I Hebrew rename crash was never
  root-caused), the local-tmux and WSL arms, `=`-pinned exact targets, and the migration of
  everything keyed by the old name: live `Session.tmux_session` fields, `session-owners.json`
  (`rename_session_owner`), `tmux-labels.json` (`rename_tmux_label`) and, over SSH, the
  session-meta label re-set under the new name. **zellij is refused** — `docs/ZELLIJ.md` §1
  keeps `action rename-session` unsent on purpose. This lifts the 2026-07-16 "NO tmux rename
  anywhere" line for an explicit user action only; `pane_set_title` still never renames.

## Invariants

- **Rule #7** — every config write is tmp + fsync + rename. No exceptions in this file.
- **Rule #6** — every `#[tauri::command]` returns `Result<_, String>`; no `panic!`.
- **Rule #4** — no `unwrap`/`expect` outside tests and the `run()` boot path. The
  `state.workspaces.lock().unwrap()` calls are the known exception and predate the rule.
- **Rule #1** — PTY bytes are never logged. `log_debug` lines carry byte counts and
  pane ids only. It is also why every terminal-bearing window is built `.devtools(false)`
  — see Gotchas.
- `persist` gates on `LoadState::Loaded`. Anything that writes workspaces must go
  through it.

## Gotchas

- `run()` installs a **panic hook before anything else** and forces `RUST_BACKTRACE=1`,
  then calls `ymux_core::flush_log()` from inside the hook — log writes are queued, and
  a panic on its way to an abort loses them otherwise. This exists because a Hebrew-title
  crash (Phase 23.I) was a `STATUS_STACK_BUFFER_OVERRUN` with no Rust trace at all.
- **`.devtools(false)` on the main window and on every popout is mandatory, not
  cosmetic.** Phase 82.E turned on tauri's `devtools` feature so the workspace Browser
  child webview can be inspected (see `build-glue.md` and `backend-panes.md`), and
  `tauri-runtime-wry` reads that setting as `devtools.unwrap_or(true)` — the feature
  flips the default to *inspectable* for **every** webview in the process. These two
  windows render live PTY output, so an inspector on them is a Rule #1 leak. The opt-outs
  in `run()` and in `popout_pane` are the only thing standing between the feature and
  that. Do not "clean them up".
- `WEBVIEW2_USER_DATA_FOLDER` is set at the very top of `run()`, before any webview
  exists, to **one app-wide** profile dir. Per-workspace profiles reintroduce
  `0x8007139F` — WebView2 allows one environment per process. Windows-only by
  construction; WKWebView and WebKitGTK ignore the variable.
- `AGENT_RUN_MIN_TURN_MS = 2000` mirrors a gate in
  `ymux-tools/statuslines/hooks/turn-state.js`. Change one, change both.
- `FEED_MAX_ITEMS` here is `#[allow(dead_code)]` documentation — `rpc_server.rs` has
  its own copy.
- **tmux targets: `=name` is a SESSION target, `=name:` is a pane target.** `=` pins an
  exact match (a bare name prefix-matches — `tax` hits `tax-contine`), and it works as-is
  for `rename-session` / `kill-session`. `capture-pane` takes a pane target, and tmux 3.4
  reads a bare `=name` there as a pane name: `can't find pane`. Phase 90 shipped with the
  bare form and every capture came back empty until a live run on 2026-09-02 caught it —
  `sessions_overview.rs` uses `=name:` and its test asserts the colon.

## Read the source when

You need the exact `invoke_handler` list, the body of a specific command, the precise
russh channel-message match in `spawn_ssh`, or the Smart Connect script text. This file
tells you where those live; it does not reproduce them.

Design rationale lives in `docs/ARCHITECTURE.md`; the wire formats in
`docs/PROTOCOLS.md`; zellij's real CLI surface in `docs/ZELLIJ.md`.
