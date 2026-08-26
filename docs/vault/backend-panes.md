---
vault: backend-panes
covers:
  - app/src-tauri/src/diff_pane.rs
  - app/src-tauri/src/file_manager.rs
  - app/src-tauri/src/workspace_browser.rs
  - app/src-tauri/src/browser_diag.js
  - app/src-tauri/src/worktrees.rs
  - app/src-tauri/src/workspaces_merge.rs
  - app/src-tauri/src/notes.rs
  - app/src-tauri/src/tickets.rs
  - app/src-tauri/src/skills.rs
  - app/src-tauri/src/stt.rs
  - app/src-tauri/src/fonts.rs
  - app/src-tauri/src/tray.rs
  - app/src-tauri/src/dev.rs
---

# Non-terminal panes and app features

Twelve modules, ~8,650 lines, plus one injected JS probe. A pane in ymux is not necessarily a shell — Diff, File
Manager, and Browser are panes too, and they share the layout tree with terminals.

## Panes

**`diff_pane.rs` (322)** — one background tokio task per Diff pane, polling `git diff`
every `POLL_INTERVAL_MS` and emitting `diff-pane-updated` when the output hash changes.
Duplicate suppression uses a cheap fnv-style `u64` rather than a string compare, so an
unchanged large diff costs a read and zero allocations. Each tick re-reads the
workspace's cwd + `DiffSource` under a **short** lock — the cwd can change (worktree
re-anchor) and later polls must see it. The task self-terminates when the workspace is
gone or its layout no longer contains the pane id. Handles live in
`CoreState.diff_pane_watchers`.

**`file_manager.rs` (1,741)** — dual-column file manager: lists, transfers, mutations on
both sides. Local ops use `std::fs`; remote ops piggy-back on the workspace's
already-authenticated SSH session and open **a fresh SFTP channel per call**. Sessions
are deliberately *not* cached: a new SFTP subsystem on an existing handle is cheap, and
caching would mean chasing teardown semantics when the terminal pane disconnects.

**`workspace_browser.rs` (1,063)** — **at most one child Webview per workspace**, attached
to the main window via `Window::add_child` (this is what pins `tauri = "=2.10.3"` with
`features = ["unstable"]`). `workspace_browser_show(workspace_id, url, x, y, w, h)`
creates or reveals it. All browser webviews share the **process-default WebView2
environment**; a per-workspace `--user-data-dir` forces a separate environment per
workspace and WebView2 does not support multiple environments in one process — that
surfaced as `0x8007139F`. Creation is serialized by `AppState.browser_create_lock` for
the same reason. Runtime-only, never persisted; `workspace_delete` calls
`cleanup_workspace_sessions` to remove `browser-sessions/<workspace_id>/`.

**Pop-out: a child Webview CANNOT be re-parented.** `add_child` binds it to its host
window for life, so `browser_popout_open` (Phase 85.C) does destroy-and-respawn, not
move: it drops the child under `main`, builds a `browser-popout-<ws>` OS window loading
ymux's own `index.html`, and that window's frontend calls `workspace_browser_show` with
its own label to spawn a fresh child underneath itself. **The page reloads.** That is
inherent to the shape, not an oversight. `workspace_browser_show` therefore takes a
`window_label` (default `"main"`) and it is read only on the SLOW path — the fast path
just repositions whatever already exists.

Two details that are load-bearing, both inherited from `popout_pane`: the command MUST
stay `async` (on Windows `WebviewWindowBuilder` deadlocks when driven from a synchronous
command — the shell appears, the webview never initializes, blank white), and the URL
must be a CLEAN `index.html`, because the built app's asset protocol treats any suffix
as a literal path and serves a blank page. The workspace id rides the window LABEL.

**The devtools split is deliberate.** The popout WINDOW gets `.devtools(false)` — it
renders ymux's own UI, and per the Cargo.toml:44-51 warning the `devtools` feature flips
wry's runtime default to `true` for every webview, so the explicit opt-out is the only
thing holding Rule #1. The CHILD webview inside it keeps `.devtools(true)`: that one
shows a tunneled third-party page, nothing of ours.

Geometry is saved in LOGICAL pixels on `WindowEvent::Destroyed` (`Destroyed`, not
`CloseRequested` — it also covers a programmatic close). `outer_position` / `inner_size`
report PHYSICAL, and the builder takes logical, so a straight round-trip would rescale
the window by the scale factor on every open/close cycle on any HiDPI display. The same
handler drops the child Webview — it belongs to a window that no longer exists, and
leaving it in the map would send the next `workspace_browser_show` down the fast path
onto a dead handle — clears the persisted mode, and emits `browser-popout:closed`.
`teardown_workspace_runtime` calls `close_popout_window` so the window cannot outlive
the workspace it belongs to.

**`workspace_browser_open_devtools` calls `open_devtools()` with no `#[cfg]` around it,
and that is deliberate.** The method is gated on the **tauri crate's**
`any(debug_assertions, feature = "devtools")`, and `Cargo.toml:52` turns that feature on
for every build, so the call compiles in release — and stops compiling loudly if anyone
ever strips the feature. Phase 85.A removed a gate that *looked* like it mirrored tauri's
but did not: inside this crate `feature = "devtools"` resolves against **`app`'s own**
feature set, and `app` declares no `[features]` section at all. In release, where
`debug_assertions` is off, the condition was therefore always false and the
`#[cfg(not(...))]` arm compiled instead — the command returned `Err("this build was
compiled without devtools support")` in 100% of shipped builds. Right-click → Inspect was
unaffected the whole time, because `.devtools(true)` on the builder is not gated; that
mismatch is what made it look like a UI bug. **Do not write `feature = "…"` in a `#[cfg]`
inside `app/src-tauri/src/`** — the crate has no features, so every such cfg is silently
false. These two were the only ones.

**Who can call what, in this webview.** The old comment here said no IPC is injected into
the child, and that was wrong; Phase 82.E corrected it in place. `tauri::manager::webview`
prepends `__TAURI_INTERNALS__` and the invoke bootstrap to **every** webview's init
scripts, external URLs included, so an `invoke` function does reach the tunneled page.
What actually denies it is the capability layer: a remote page's origin is
`Origin::Remote`, every capability in `capabilities/` declares a `Local` execution
context, so `Origin::matches` fails and each command is refused. **`capabilities/default.json`
is scoped to `windows: ["main"]` and this webview lives in the `main` window** — adding a
`remote` context there would hand a third-party service the app's whole command surface.
Don't.

The Dev-Mode inspect script still talks back through navigation
(`location.href = "ymux-ticket:<base64url>"`, decoded by `handle_ticket_navigation`,
re-emitted as `browser:ticket-captured`) because `WebviewBuilder` has no `on_ipc` in 2.10.

**Phase 82.E — the macOS "site JS is dead" probe.** On macOS a page renders in this
webview but the site's own JavaScript never runs; the identical code path is fine on
Windows. Rule #17 rules out building or attaching a debugger locally, so the diagnosis
ships inside the binary and reports through the unified logger under tag `BROWSERDIAG`.
Three independent channels, chosen so that silence is still evidence:

- **`browser_diag.js` (241)** — a document-start `initialization_script` that reports by
  setting `document.title` to `ymux-diag:<base64 json>`. That is the one channel that
  behaves identically on both platforms (wry hooks `add_DocumentTitleChanged` on Windows
  and a KVO observer on the `title` keypath on macOS) and it needs no navigation, so it
  cannot perturb the page load being measured. `fetch`, an iframe, and `location.href`
  were all considered and rejected — the file's header says why.
- **`DIAG_EVAL_JS`**, pushed from Rust 300 ms after `PageLoadEvent::Finished`. `eval` is
  `evaluateJavaScript` on macOS, a *different* injection mechanism from the `WKUserScript`
  that carries the document-start probe, and it reports whether that probe left its global
  behind. One beacon therefore separates "JS engine is dead" (nothing arrives) from
  "user-script injection is broken" (this arrives, `us` is 0).
- **`on_page_load`**, which fires from the navigation delegate with no JS involved at all
   — proof the page committed even when nothing else reports in. Plus an 8-second
  watchdog that logs `NO BEACON` affirmatively, so an empty log can never be confused
  with "the user never opened a page".

It is deliberately **not** `cfg`-gated to macOS: if the probe first ran on the one machine
nobody can debug, "no beacon" would be ambiguous between a dead JS engine and a broken
probe. The Windows build is the control experiment.

**The beacon channel is attacker-influenced and treated that way.** The page controls its
own title. `handle_diag_title` spends from a per-webview budget of `DIAG_MAX_BEACONS = 24`
and then goes quiet; `decode_diag` re-serializes through `serde_json` rather than logging
the page's bytes (raw newlines are legal JSON whitespace, so a hostile page could
otherwise forge `[ERROR]` lines in `debug.log`) and truncates at `DIAG_MAX_LOG = 900` on a
char boundary. **Any other title is ignored, not logged** — Rule #1's spirit is that page
content stays out of the log. `mod diag_title_tests` pins all of it, including that `btoa`'s
padded standard-alphabet output round-trips through `tickets::base64_decode`, which was
written for the URL-safe unpadded dialect.

**`workspace_browser_open_devtools(workspace_id)`** opens the inspector on that webview:
one click instead of Safari → Develop → machine → webview, and the *only* way in on
Windows, where WebView2's F12 is not wired up for a child webview. `.devtools(true)` is set
on this webview alone; the command always exists so the frontend needs no build-shape
knowledge, but its body is `#[cfg(any(debug_assertions, feature = "devtools"))]` and the
other arm returns a clean error.

## Git

**`worktrees.rs` (638)** — enumerate and create git worktrees for a workspace flagged
`is_project_root` whose cwd is a repo. Every git call dispatches on that workspace's
connection:

| connection | transport |
|---|---|
| `Local` / none | `tokio::process::Command`, arg array |
| `Wsl` | `local_setup::wsl_exec` (an `sh -ls` script) |
| `Ssh` | an exec channel on a **live** handle to that host |

The SSH path matches on `user@host:port`, **not on workspace id** — a repo is reachable
from any session to the same host.

**`workspaces_merge.rs` (415)** — the three-way merge `save_to_disk` calls. A save is not
"dump what I have": it re-reads the file and, if it changed since we last touched it,
applies only *our* delta onto *theirs*. Needed because a stable build and a dev build
share `%APPDATA%\ymux` unless someone sets `YMUX_CONFIG_DIR`, and the pre-rename
`%APPDATA%\winmux` + `WINMUX_CONFIG_DIR` are still honoured — so an old and a new binary
can land on the same directory from either side of the rename. `reconcile(ours, base,
theirs)` is a **pure function with unit tests**, which is the point: an idle app never
saves, so the interesting path cannot be reached by launching one and waiting.

## Capture and content

**`notes.rs` (341)** — `%APPDATA%\ymux\notes.json`, atomic write + poison gate, emits
`notes:changed`. Exposed as **both** Tauri commands and JSON-RPC methods, so the CLI on
a remote pane can drop a note through the tunnel.

**`tickets.rs` (1,867)** — dev-mode element capture. Right-clicking in a workspace
browser with Dev Mode on records xpath + CSS selector + bounded HTML + computed-style
summary + optional screenshot + a description. Tickets belong to a **project**, not a
workspace — the point is handing one to Claude Code inside the right repo — so they are
written to `<project>/.ymux-tickets/` when the project is reachable from this machine,
and fall back to `<config_dir>/tickets/<workspace_id>/` while still recording the project
path, so nothing is orphaned.

**`skills.rs` (288)** — installs a skill folder (`SKILL.md` + scripts) from the local
registry at `config_dir()/ymux-tools/skills/<name>/` onto a workspace's
`~/.claude/skills/<name>/`: filesystem copy for local, SFTP over the existing session for
remote. Populating the source registry is out of scope for this module.

**`stt.rs` (262)** — **local-endpoint** speech-to-text only. The Web Speech API backend
is entirely frontend (`window.SpeechRecognition` ships in WebView2). This posts audio to
a user-configured HTTP endpoint using a multipart shape that mirrors OpenAI's
`/v1/audio/transcriptions`, so whisper.cpp's server, faster-whisper-server, and
OpenAI-compatible proxies all work with no per-server adapter. The command is a thin
wrapper around `transcribe_inner`: every one of its failure paths (empty buffer, missing
endpoint, HTTP/transport/parse errors) funnels through a single `log_error("STT")` at
the wrapper — error strings carry no transcript content, only metadata.

## Shell chrome

**`fonts.rs` (1,195)** — download a curated font, verify it, register it **per-user**:
files land in `%LOCALAPPDATA%\Microsoft\Windows\Fonts` and register under HKCU, which
needs no elevation on Windows 10 1809+. `settings::list_system_fonts` reads that same
hive, so an install is visible in the picker immediately. Exists because flagging
unavailable families with ⚠️ was only half an answer — the user still had to go find a
`.ttf`.

**`font_uninstall` is the mirror, and how it finds the files is the interesting part.**
Which files belong to a catalog entry is derived from the CATALOG, not recorded at
install time: a zip asset wrote everything matching its `ZipFilter::name_prefix`, a bare
asset wrote exactly its `save_as`, so `entry_owns_file` re-derives the same set. A
manifest written at install time was the obvious alternative and is wrong here — it
could only know about installs made after it shipped, leaving every font already on a
user's machine unremovable, which is the case the feature was filed for.

Two orderings that look arbitrary and are not:

- **Delete, then unregister.** The reverse leaves a font still on disk and still
  loadable with no registry entry — present but invisible to the picker, which is worse
  than either end of the operation. Only paths that actually went get unregistered.
- **Match registry values by their DATA (the path), not by rebuilding the value name.**
  `register_font` builds that name from the font's internal name table with a fallback
  to the file stem; rebuilding it at uninstall would mean reproducing that decision from
  a file that has just been deleted.

A face held open by a running app is a **reported outcome** (`failed`), not an error —
that is the everyday Windows case, and the rest of the family still comes out. Accepted
and written down: a user's own hand-installed copy of the same family, in the same
per-user directory, also matches.

**`tray.rs` (91)** — tray icon, quick menu, taskbar badge. **Best-effort**: a failed
build logs and carries on. `TRAY_ACTIVE` gates close-to-tray so a failed tray can never
trap the user with a hidden window and no way back.

**`dev.rs` (174)** — shared structures and pure helpers behind `ymux dev`: state-snapshot
building, the console ring buffer (`CONSOLE_MAX = 200`, fed by frontend
`console.error`/`warn` captures, exposed as `AppState.console_buffer`), and log /
bug-report file IO. The commands and RPC handlers themselves live in `lib.rs` and
`rpc_server.rs`.

## Invariants

- **Rule #7** — `notes.json`, ticket files, and every other config write are tmp +
  fsync + rename.
- **Rule #1** — the file manager and diff pane see user content; log paths and byte
  counts, never bodies.
- A watcher task must self-terminate when its pane leaves the layout. `diff_pane` is the
  reference implementation; `port_watcher_tasks` in `CoreState` follows the same shape.
- One Webview per workspace, one WebView2 environment per process. Do not add a
  `--user-data-dir`.
- **Only the workspace Browser webview is inspectable.** The `devtools` feature flips
  wry's default to `true` process-wide; the `.devtools(false)` opt-outs on the main window
  and popouts in `lib.rs` are what keep PTY output out of an inspector. See
  `backend-core.md` § Gotchas.
- `capabilities/default.json` stays `Local`-context only. A `remote` context would expose
  the command surface to whatever the Browser is pointed at.

## Read the source when

You need a file-manager operation's exact error mapping, the ticket JSON schema, or the
curated font list. `docs/CONFIG.md` covers the user-visible settings these expose.
