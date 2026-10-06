---
vault: frontend-lib
covers:
  - app/src/terminalInstance.ts
  - app/src/termMenuCopy.ts
  - app/src/types.ts
  - app/src/settings.ts
  - app/src/claudePricing.ts
  - app/src/insightsFmt.ts
  - app/src/insightsReport.ts
  - app/src/insightsCommands.ts
  - app/src/clipboardText.ts
  - app/src/textDirection.ts
  - app/src/bidi.ts
  - app/src/copyBidi.ts
  - app/src/mouseRtl.ts
  - app/src/popoutProfile.ts
  - app/src/wheelSteps.ts
  - app/src/sessionRestore.ts
  - app/src/claudeRunning.ts
  - app/src/logger.ts
  - app/src/shortcuts.ts
  - app/src/stt.ts
  - app/src/platform.ts
  - app/src/download.ts
  - app/src/fontProbe.ts
  - app/src/i18n/index.ts
  - app/src/backend/types.ts
  - app/src/backend/tauri.ts
  - app/src/backend/index.ts
  - app/src/backend/web.ts
  - app/src/backend/web/*.ts
  - app/src/layoutOps.ts
  - app/src/WebLogin.tsx
unowned:
  - app/src/bindings/*.ts   # ts-rs generated
  - app/src/*.test.ts   # tests are the spec, deliberately uncovered
  - app/src/vite-env.d.ts   # vite type shim
---

# Frontend library modules

The non-component half of `app/src/`. Two things dominate: the terminal wrapper, and
**RTL** — four separate modules exist because Hebrew broke in four different places.

## The backend seam — `src/backend/` (Phase 106, WEB-DESIGN §5)

Every host call goes through the `backend` singleton from `src/backend/index.ts`:
`backend.call<T>(cmd, args)` (was `invoke`), `await backend.on<T>(event, cb)` (was
`listen`), `backend.emit(event, payload)` (cross-window, popouts only). `TauriBackend`
is a pass-through to Tauri IPC, so the desktop behaves exactly as before. The point is
the second implementation: a browser build (Phase C5) swaps in a `WebBackend` that
answers the same command names from the daemon's HTTP/WS API.

- **`backend.host`** (Phase 107) — the window / OS affordances the components used to
  take from `@tauri-apps/api/window|webview|app` and the dialog / opener plugins:
  `windowLabel()` (index.tsx's popout router; "" when unknown), `setTitle`, `setZoom`,
  `closeWindow`, `appVersion`, `openUrl`, `revealInDir`, `pickPaths` (native open dialog),
  `savePath`, `onDragDrop` (OS file drops, Tauri's payload and positions unchanged).
- **`backend.can(cap)`** (Phase 107) — the host's capability set (`ALL_CAPABILITIES` in
  `types.ts`: localPanes, ssh, browserPane, popout, fileManagerLocal, diffPane, worktrees,
  tickets, skills, addons, mobilePairingAdmin, updater, fonts, stt, portForward).
  `TauriBackend` has them all, so the desktop renders exactly what it did. A browser
  host lacks the local-machine ones and the UI hides their entry points rather than
  failing on them (WEB-DESIGN §4.1). Gated today: the wizard's Server / Local cards,
  the Welcome cards, palette `ssh.provision` / `pane.openDiff` (`PALETTE_CAPS` in
  App.tsx) and the `open_diff` key, the header Browser button, ⋯ "+ diff" and Tickets,
  pane pop-out, the File Manager local column + toggle (`localVisible()`), the sidebar
  Add-ons item and Ports button, Settings → updates + VersionManager, AddonsTab,
  YmuxToolsTab, the font installer, the local STT option, Monitor's Mobile tab.
- **The rule is enforced:** `src/backendSeam.test.ts` fails when any file outside
  `src/backend/` imports anything from `@tauri-apps/*`.

### The browser arm — `backend/web.ts` + `backend/web/` (Phase 109, WEB-DESIGN C5)

`index.ts` picks `WebBackend` when `__TAURI_INTERNALS__` is absent (a tab served by the
daemon, vault server-go § webapp.go). `index.tsx` awaits `initBackend()` before the first
render: `ready` → `<App>`, `login` / `no-shell` → `<WebLogin>` (the Phase 96 pairing flow:
request → code → approve on the desktop → redeem; `no-shell` = signed in without
`shell:attach`). The same vite bundle serves both hosts.

- **Commands** — `WebBackend.handlers`, one entry per desktop command name, answered from
  the daemon. A name with no handler rejects (warned once): reaching one means a missing
  `can()` gate. Rejections are strings, like Tauri's.
- **Events** — `web/events.ts`: `EventBus` behind `backend.on`, fed by the daemon events
  socket (`{type,data}` frames already carry the desktop's names) and by the backend.
  `settings:changed` `{version, settings}` is unwrapped to the bare `Settings`;
  `workspaces:changed` triggers a re-read and is re-emitted with the desktop's `()`
  payload. The first `hello` answers `pane_agent_states` / `pane_briefs` / `feed_list` /
  `notifications_list`; a reconnect's hello re-reads workspaces and emits
  `backend:resync` (nothing listens yet).
- **PTY** — `web/pty.ts` speaks the desktop's own contract (WEB-DESIGN §8.2 dropped C2):
  `pane_connect` opens `/api/v2/term/sessions/{name}/attach` and returns a sid;
  `pty_write` / `pty_resize` are frames on it; binary frames go through a streaming
  `TextDecoder` (the job `pty_decode.rs` does on the desktop) into `pty:data`; the
  server's `{"type":"exit"}` or a close becomes `pty:exit`. `pane_disconnect` closes
  without a `pty:exit`, as on the desktop.
- **Panes and sessions** — a leaf's `pane_id` IS its tmux session's hook pane id (daemon
  2.10.0 takes `pane_id` on create). `pane_connect` reuses the session the hello or a
  create named for that leaf, else a live session with the leaf's derived name
  (`<ws-slug>-<pane suffix>`), else creates that name with the leaf's id. So reload →
  restore (on by default in browser settings) → the same session. `init()` also seeds
  `sessionRestore`'s per-pane hints from the daemon (hello map, else the derived name),
  so a second browser — or one whose storage was cleared — re-attaches too.
- **Workspaces** — the daemon's `/api/v2/web/workspaces` documents mapped to `Workspace`
  with a synthesized ssh-shaped connection (the panes ARE remote tmux: RTL profile and
  `paneCaps` answer "remote"). Layout gestures run in `layoutOps.ts` (pure ports of
  lib.rs `split_pane_in` / `close_pane_in` / `set_split_ratio_in` /
  `swap_two_panes_in_layout` / `reset_all_split_ratios`, pinned by `layoutOps.test.ts`)
  and PUT with the version; a 409 re-applies the op once on the returned document. The
  active workspace is per browser (localStorage). Colour, emoji, groups and order are
  not stored on the daemon yet.
- **Settings** — `GET/PUT /api/v2/settings` over `web/defaults.ts` (Rust's defaults for
  the required groups, merged one level deep; restore-on-start ON, update checks OFF).
  A stored non-object where the default is a group is **ignored**, not merged — found
  live: `{"theme":"dark"}` replaced the theme object and `applyTheme` crashed
  (`webDefaults.test.ts`). `log_dir_path` answers `""` (the console is the log).
  A 409 on save re-saves on the newer version: the whole document wins, as on the desktop.
- **New workspace** — App's `openNewWorkspace()`: the desktop opens the wizard; the
  browser (no wizard targets) creates `workspace N` directly.
- `web/` cannot import `logger.ts` (cycle through `backend`), so it uses `console.*` — the
  browser console is the sink there. No PTY bytes, no token.
- `types.ts` imports nothing from the app — `logger.ts` calls through the backend, so a
  logger import there would be a cycle.
- `on` stays async on purpose: App.tsx awaits each registration so the "listeners
  before session restore" ordering holds.
- The singleton is picked synchronously at module load, so any module may call it from
  its first line.

## `terminalInstance.ts` (1,954) — the xterm.js wrapper

**Logging:** every diagnostic goes through the module-level `termLog = createLogger("TERM")` (Rule #9), never raw `console.*`; messages carry labels and error objects only, never PTY or clipboard content (Rule #1).

**The mouse contract (Phase 91.B + 91.D):** tmux's mouse is off since the conf lock, so
xterm.js owns every button — native selection, ymux's own right-click menu. The wheel is
the one thing proxied, and only on a **multiplexer pane**: `setTmuxScroll(on)` is set by
App from `pane_persistence_list` (the backend's answer, never a title guess — Phase
65.O's proxy fired in a plain shell and walked bash history), and the constructor's
`attachCustomWheelEventHandler` turns a wheel event into Shift+Up/Down via
`term.input` (through `onData` → `pty_write`) **only when** armed, no Shift/Ctrl held,
the ALT buffer is active (tmux attached) and `term.modes.mouseTrackingMode === "none"`;
every other case returns `true` and xterm keeps its own behaviour. Why it exists: with
mouse off, xterm.js 6.0 converts a wheel event on the alt buffer into one `\e[A`/`\e[B`,
which at a shell prompt inside tmux is HISTORY. xterm consults the hook before that
conversion and never when the app requested wheel reports (zellij, vim `mouse=a`), so
those step aside by themselves. **One key per notch (Phase 98)**: `wheelSteps.ts` (pure,
unit-tested) turns `deltaY`/`deltaMode` into whole steps — a mouse notch (~100px, or 3
lines) is one step, anything from half a notch counts, a touchpad's small deltas
accumulate in the instance's `wheelCarry`, a reversal drops the carry, ≤10 per event.
91.D sent a fixed 3 keys per event; tmux's paste detection then skipped the bindings of
all but the first (dropped in copy-mode, typed into the program as `^[[1;2B` once `-e`
had left it). The conf side (`ymux-tmux.conf`) binds `S-Up`/`S-Down` to a 3-line
`send-keys -X -N 3 scroll-up/-down` on the main screen and in copy-mode (the pre-91
per-notch rate, as one operation), passes them through as 3× plain Up/Down under
`#{alternate_on}`, and sets `assume-paste-time 0` so a burst of keys still runs its
bindings. `installRtlMouseCapture` gates on row `dir`, not on tracking, so it
always feeds native selection inside tmux. `resetMouseModes()` (connect + pty:exit) is
leak cleanup for the display and is unrelated to tmux's option; `pty:exit` also disarms
the proxy.

**Right-click Copy under an app-owned mouse:** the `contextmenu` listener is registered in the capture phase so xterm never forwards the click to zellij, and `showTerminalContextMenu` builds Copy from `menuCopyText` (`termMenuCopy.ts`, pure, unit-tested): xterm selection, else the raw last OSC 52 write (private `lastOsc52`, set in the write-only provider, read via `getLastOsc52()`) when `mouseTrackingMode !== "none"`, else empty (plain shell, Copy stays disabled).

`class TerminalInstance` owns one xterm `Terminal`, its `FitAddon`, the optional
`WebglAddon`, and the DOM container. Module-scope globals cache font family/size, theme,
and the Ctrl+C-copies-selection flag so new panes construct with the current values;
`setTerminalTheme` / `setTerminalFont` / `setRtlProfiles` push changes to live panes.

Things it does that are easy to get wrong:

- **`applyRowDirections` / `ensureDirObserver`** — a `MutationObserver` coalesces a burst
  of cell mutations into **one** `applyDir()` per animation frame, and a `WeakMap` cache
  skips any row whose text is unchanged. Without both, per-line direction is a
  per-mutation DOM write. If `.xterm-rows` is missing it retries on a 250 ms timer, at
  most 40 times — never an unbounded rAF chain (same for `scheduleInitialFontMeasure`
  waiting for the container to be attached).
- **Diagnostic log lines on per-frame paths are rate-gated.** `rtl-dirs` speaks when the
  direction vector changes, at most once per 2 s per pane. There is no title log line:
  the OSC-title Claude detector was removed 2026-10-06 (it never fired in practice).
- **`fitAndResize`** is rAF-throttled — the `ResizeObserver` fires per pixel during a
  divider drag, and every call sends a SIGWINCH down the SSH channel. tmux cannot keep up
  and the renderer thrashes.
- **WebGL glyph atlas is flushed on resize.** Without that the GPU canvas keeps painting
  the previous viewport's grid metrics — visible as lines that do not reflow.
- **Link handling** — OSC 8 hyperlinks and a plain-text `[file]` link provider (Claude
  Code prints produced files as plain text, not OSC 8). Each has a one-shot diagnostic
  flag, metadata only per Rule #1, so "the regex never matched" is distinguishable from
  "the click path is broken". `workspaceId` is set on connect so a `file://` click can
  SFTP-download from the right remote.
- **`writeData` buffers and flushes**, and the custom right-click menu lives here too.

### RTL profiles — read this before touching direction

`RtlProfileSettings` mirrors `RtlProfile` in `settings.rs`: `rtlMode`
(`auto_per_line | force_rtl | bidi_reorder | off`), `autoDirection`,
`mirrorArrowsRtl`, `tuiOwnsBidi`, and `directionPolicy`.

Two of those modes paint rows with a `dir` attribute and therefore need the DOM
renderer; the other two run WebGL. `usesRowDir(mode)` is the single predicate
for that question, and it is a **type guard** narrowing `RtlMode` to `RowDirMode`.
It exists because the same question was asked in six places — the renderer choice
in the constructor, `applyRenderer`, `staleRenderer`, the mouse capture, the dir
observer and the row pass — and `applyRenderer`'s own comment records the cost of
letting them disagree: the pane lands in a combination that is none of the modes
and does **no bidi at all**. Yossi reported Hebrew broken "in all 3 options"; two
of the three were that hole, so only one mode was ever really under test.

**`force_rtl` (2026-08-23, narrowed 2026-09-15 / Phase 94)** is the mode with no
auto_per_line heuristics: no dominance vote, no block grouping, no
`stripPaneFrame`, and `autoDirection` / `directionPolicy` / the `suppress` signal
are all inert — `textDirection.test.ts` pins that across every combination of the
three knobs. It was added for remote panes on Yossi's ask — "RTL מלא, ולא שורה
שורה". What it PAINTS changed in Phase 94, after Claude Code's split-screen diff
view came out scrambled on every Windows install running it. The mechanism is
xterm's DOM renderer: every style run is an inline-block `<span>`, an atomic
inline is a neutral to UAX #9, and a row of neutrals inside an RTL paragraph is
laid out **right-to-left** — so a multi-run Latin row (a diff's gutter, line
number, two columns) came out with its fragments mirrored, while a single-run
shell row merely sat at the right edge, which is why it went unnoticed for three
weeks. The rule now, per row: Hebrew/Arabic present → `rtl` exactly as before; no
RTL text and **Claude Code holds the pane** (the hook-driven `claudeActive` flag
set by `setTuiSignal(on: boolean)` from the `YMUX_PANE_ID` Claude hooks, so it works over
SSH, zellij and tmux — NOT the profile's `tui_owns_bidi` switch) → plain `ltr`, so a
two-column TUI keeps both halves where it drew them; no RTL text in a shell →
**`ltr-end`**, a third `RowDir` value meaning `dir="ltr"` plus
`text-align: right` and `data-ymux-align="end"`: reading order kept, the run
packed against the right edge ("לטינית נשארת בימין, בלי היפוך"). The DOM renderer
trims trailing no-background cells, which is what gives `text-align` room on a
shell row and makes it a no-op on a full-width TUI row. `mouseRtl.findRow` reads
the data attribute and reports the gap after the last span as `shift`, and
`transformMouseX` subtracts it, so clicks on a packed row still land on the right
column. The `rtl-dirs` log line gained `end=N`. Opt-in, neither profile default
moved, one click back.

The same Phase 94 closed the matching hole in `auto_per_line`: step 4 of
`detectRowDirections` used to let a block with **zero** RTL text inherit the
direction of the row above it, so Claude's bordered diff under a Hebrew prompt
became an RTL block and mirrored the same way. A pure-ASCII block is now LTR no
matter what surrounds it.

Two traps around `force_rtl`, both of which produce reversed letters if missed:

- **`normaliseIncomingToLogical` stays pinned to `auto_per_line`.** It asks a
  different question from `usesRowDir`: not "does this mode paint a `dir`" but
  "is the incoming BUFFER visual order". Visual order is a local/ConPTY condition;
  `force_rtl` targets remote panes, whose stream is logical already, and
  normalising a logical stream reverses it.
- **`unicode-bidi: bidi-override` is gated on the mode, not on `suppress` alone.**
  The override paints bytes verbatim, which is right only while the buffer is
  still visual. Pairing it with `dir="rtl"` on a logical buffer reverses every
  Hebrew word, so `applyRowDirections` reads `suppress && mode === "auto_per_line"`.

`directionPolicy` is the field to understand:

- **`any_rtl`** — any Hebrew/Arabic on the row takes it RTL. What every version before
  2026-08-19 shipped, and what remote panes are **known** to render correctly.
- **`tui_dominance`** — the `RTL_DOMINANCE` vote (in `textDirection.ts`): Hebrew wins
  unless massively outnumbered by Latin, which stops a TUI status bar from mirroring its
  own layout.

**It is keyed on the pane class, never on what is running inside the pane.** The vote
first shipped gated on `tuiOwnsBidi`, and because the OSC-title detector (since removed;
detection is now the Claude hook signal) fired on remote panes and broke them. Yossi's
instruction afterwards was a total separation between local and remote, so a change aimed
at local panes cannot reach remote ones. A per-profile field is that separation, and
`remote_direction_policy_is_the_pre_2026_08_19_rule` in `settings.rs` plus the parity
tests in `textDirection.test.ts` enforce it. The same reasoning is why the four knobs
stopped being scalar globals.

## The four RTL modules

**`textDirection.ts` (427)** — per-line direction. xterm's DOM renderer with `dir="auto"`
uses "first strong directional character wins", which mis-renders a mixed line that
happens to *start* with Latin: `2. /opt/wa/.shared.env - הערה` laid out LTR because the
first strong char is Latin, though the line is mostly Hebrew. Yossi's rule instead: a
line containing **any** Hebrew/Arabic is RTL. `RTL_DOMINANCE` is the `tui_dominance`
refinement on top.

`rowDirections(mode, texts, {auto, suppress, dominance, tui})` is the whole-pane
decision, and the one place `force_rtl` and `auto_per_line` diverge; it returns
`RowDir[]` (`ltr` | `rtl` | `ltr-end`, see the profiles section). It was lifted
out of `TerminalInstance.applyRowDirections` so it could be tested at all — the
method is bound to the DOM and never ran under `node --test`, and in these
modules the tests *are* the specification.

**`bidi.ts` (71)** — the `bidi_reorder` path (bidi-js, no type defs). Exports the escape
matcher so the visual→logical pass protects escapes **exactly** the way this file does —
one definition of "what an escape looks like".

**Known limit (DEFERRED):** `bidi_reorder` has the terminal-wg "cursed cursor" — caret stays pinned right and a partial repaint can leave a line half-reordered, because the transform runs per rAF chunk while the TUI addresses untransformed columns. Fix needs whole-line reassembly + cursor tracking; see `docs/DECISIONS.md` and `docs/RTL-TEST.md` § `bidi_reorder` — known limits.

**`copyBidi.ts` (185)** — visual→logical for text on its way to the **clipboard**.
Measured on Yossi's machine, 2026-08-20: plain PowerShell renders reversed on screen but
pastes correctly, while Claude Code renders correctly and pastes reversed — exactly
inverted, because the two panes hold opposite orders in the buffer.

**`mouseRtl.ts`** — coordinate transform for RTL rows. xterm's `SelectionService`
maps `clientX` → buffer column assuming LTR. With `dir="rtl"` on a row the browser paints
it mirrored, so a click on what the user sees as cell 5 lands on cell `cols - 5 - 1`.
Selection and click positioning both land on the wrong side without this. Phase 94 added
the second transform: an `ltr-end` row is in reading order but moved right by the gap
after its last span (`RowRect.shift`, measured in `findRow` from `data-ymux-align`), and
`transformMouseX` subtracts it. The capture handler in `terminalInstance` therefore gates
on "did the transform move the point", not on the row being rtl.

## Typed mirrors

**`types.ts` (618)** — the data-model types are **generated from the Rust structs by
ts-rs** and re-exported here so `from "./types"` keeps working. Regenerate after a Rust
struct change with `cd app/src-tauri && cargo test`. **Do not hand-edit
`src/bindings/*.ts`.** Note ts-rs renders `Option<T>` as `T | null` — a required,
nullable key, not `T?` — so helpers such as `effectiveIdentity` widen their params to
`T | null | undefined`. The hand-written helpers here (`paneCaps`, `profileFor`,
`describeConnection`, `isLocalConn`, `isRemoteEffective`, `collectPanes`, `findPane`)
are what components use to reason about a pane.

**Not everything here is generated.** `BoundSession` (Phase 91.C — what a pane's
[Connect] attaches to, or resumes) and `WorkspaceCardInfo` / `CardStatusKind` (Phase 91.E,
+ `branch` in 91.F —
what a sidebar card prints, built by App's `workspaceCardInfo` memo, see frontend-shell) are
hand-written in `types.ts`, as are
`TmuxSessionInfo` and `ForeignScope`, which are
**hand-written mirrors** of structs that live in `lib.rs` rather than `ymux-types`, so
ts-rs never sees them and nothing regenerates them for you. A field added on the Rust
side is silently missing here until someone types it — update both in the same commit.
Phase 90 added two more of these: `TmuxSessionInfo.owner_cwd` (the claim-time cwd from
`session-owners.json`, a grouping key only) and `SessionSummary`, the row shape of
`sessions_overview_summarize` (`status` is a closed union ending in `unknown`, which is
what the backend emits for anything the model did not say cleanly).

**`settings.ts` (751)** — the typed settings mirror plus load/save and the CSS-variable
apply. `src-tauri/src/settings.rs` owns the canonical schema; this follows it
(`BriefSettings` gained `inject_context`, default true, in Phase 105.C). Also
carries the font-catalog bindings: `fontCatalog` (each item now reporting whether it is
`installed`, read from the font directory on every call rather than from any record of
past installs), `fontInstall`, and `fontUninstall`.

## Small modules

- **`logger.ts` (~115)** — `createLogger(tag)`. Lines reach both devtools and the single
  local `debug.log`, tagged `[UI:TAG]`. **Batched**: `enqueueLog` queues, and one
  `ui_log_batch` invoke ships the queue at most once a second, capped at 100 lines (the
  overflow is counted into a `[UI:LOG] dropped N` line); `pagehide` flushes. One invoke
  per line was how a chatty call site became a steady IPC stream. `index.tsx`'s
  console.warn/error forwarder uses the same `enqueueLog`. Level filtering is
  **double-gated**: skip below the threshold here (cheap), and the backend filters
  again — the backend is authoritative, so a popout window (which loads settings and runs `applyTheme`) still
  behaves. **Import this before the console monkeypatch.** Rule #9.
- **`popoutProfile.ts` (12)** — pure `popoutProfileKey(sid)` (`ymux.popout.profile.<sid>`) and
  `parsePopoutProfile(raw)` (only exact `"remote"` is remote, else local): the localStorage
  hand-off of the origin pane's RTL profile to its popout window.
- **`i18n/index.ts` (86)** — dictionaries statically imported (~30 KB total, no async
  loader). Active language and direction are two signals, so `t(key)` and the document
  `dir` react together. A missing key returns the key itself.
- **`platform.ts` (50)** — host OS resolved **once** from Rust (`host_platform`,
  `std::env::consts::OS`). Exists because two Windows-only assumptions were baked in as
  literals and both broke on mac: local paths joined with a hardcoded `\`, and drag-drop
  positions divided by `devicePixelRatio` (WebView2 reports physical pixels, wry's macOS
  backend reports logical points).
- **`claudeRunning.ts` (26)** — pure decisions for the persisted `claude_running` flag:
  `tuiSignalOnConnect(mode, restoring, persisted)` (claude→true; non-restoring→false; restoring
  with persisted true→true; else null = untouched) and `claudeRunningWrite(persisted, on)`
  (null when no transition). App.tsx owns the invokes.
- **`sessionRestore.ts` (102)** — remembers which tmux session each SSH pane was attached
  to, so the next start re-attaches instead of showing [Connect]. **localStorage on
  purpose**: per-machine, high-churn session state, the same class as window rects and
  sidebar width. Losing it costs one click, never data, and it keeps Rule #7's
  atomic-write surface small.
- **`shortcuts.ts` (380)** — the accelerator registry, not just a parser. It owns
  `ShortcutsSettings`, `DEFAULT_SHORTCUTS`, `SHORTCUT_ACTION_IDS` and
  `SHORTCUT_GROUPS` (the Settings tab's row order; BRIEF added `toggle_queue`
  Ctrl+Shift+Q and `show_briefing` Ctrl+Alt+Q in the general group, Phase 105 added
  `toggle_context_rail` Ctrl+Shift+K there too, Phase 91.F added
  `open_diff` Ctrl+Shift+G in the panes group), `DEPRECATED_SHORTCUT_IDS` (`find`:
  kept in the schema and defaults so old `settings.json` loads, but filtered out of
  `SHORTCUT_ACTION_IDS`, `ShortcutActionId` and the groups, so it has no row, table
  entry or conflict check), parses
  `settings.shortcuts.<name>` into a table on settings load, and exposes
  `matches(event, accelerator)`. Same vocabulary in the hand-editable JSON and the
  click-to-record picker. **Phase 87: the defaults live HERE, not in `settings.ts`,
  on purpose** — this module has zero imports, so `shortcuts.test.ts` can run under
  bare `node --test`; `settings.ts` pulls in the Tauri bridge and the terminal and
  would drag them into the test. `settings.ts` re-exports them for old call sites.
  Every event-reading function takes a structural `KeyLike`, not `KeyboardEvent`, for
  the same reason. `matches()` compares the logical `event.key` OR the physical
  `event.code` (`physicalKey`), which is what makes letter and punctuation bindings
  fire on a Hebrew layout — and why the dispatcher needs no `event.code` special
  cases. `conflictingAccels()` reports accelerators claimed by more than one action:
  dispatch is first-match-wins, so a duplicate leaves the loser silently dead. It
  takes an `extra` map because the STT push-to-talk hotkey lives under
  `settings.stt`, not `settings.shortcuts`, and a clash across those two schemas is
  exactly the bug that made Focus/Zoom move off `Ctrl+Shift+M`.
- **`shortcuts.test.ts`** — and it actually runs now: `npm test` (a plain
  `node --test` over `src/*.test.ts`) is a ci-windows step as of Phase 87. The nine
  test files that predate it had never been executed by anything.
- **`stt.ts` (262)** — one recorder interface over two backends: `webspeech` uses
  `window.SpeechRecognition` directly (WebView2 ships it, but Chrome streams to Google's
  servers behind the scenes — which is exactly why the Local option exists), and `local`
  records with MediaRecorder and POSTs through `stt_transcribe_local`.
- **`download.ts` (55)**, **`fontProbe.ts` (121)** — OSC 8 / file-link downloads, and
  probing whether a font family is actually installed.
- **`clipboardText.ts` (32)** — `copyText`. `navigator.clipboard.writeText` is the path
  that works: Tauri 2 exposes the browser API and WebView2 grants clipboard **write** (it
  denies **read**, which is why reading goes through the Rust `readClipboardText`
  command). Older WebView2 builds don't grant even write, hence the off-screen
  `<textarea>` + `execCommand` fallback. `FileManagerPane.copyPathOf` still carries its
  own copy of the pair; folding it in is logged in BACKLOG, not done in passing.

## Monitor support modules

Four pure modules behind the Monitor's Analytics and Claude tabs
(`frontend-panes.md`). They are **DOM-free and i18n-free on purpose** — callers pass
already-translated strings in — which is what makes `insightsReport.test.ts` and
`claudePricing.test.ts` runnable as plain node tests.

- **`claudePricing.ts` (245)** — **the one place ymux knows what Claude costs.** Both
  backends count tokens and refuse to price them, because token counts are facts and
  prices are a table that goes stale; keeping the table here makes a price change a
  one-file edit instead of a server rebake plus a matching edit in the Rust mirror.
  `PRICING_AS_OF` records when it was last checked (2026-10-06). Opus 5.5 and Sonnet 5.5 have their own rows because the `claude-opus-5` / `claude-sonnet-5` prefixes misprice them. The cache-read multiple is per model: `ModelPrice.cacheReadMult` (0.025x Fable 5.1 / Mythos 5.1, 0.05x Opus 5.5), absent = flat `CACHE_READ_MULT` 0.1x. Sonnet 5 is a flat $2/$10. Rates are **Anthropic first-party API
  list prices** (Bedrock and Vertex are partner-priced and not modelled), and
  `ModelPrice.intro` (`{in,out,until}`) is the generic launch-rate mechanism (no row uses it now), so a launch rate expires instead of silently
  under-reporting forever. ⚠️ **Claude Code on a Pro/Max subscription is not billed per
  token.** Everything here is the API-*equivalent* cost — right for "where is my quota
  going, in money terms", wrong for "what will my card be charged". **The UI must never
  label it a bill.**
- **`insightsFmt.ts` (34)** — `fmtBytes` / `fmtBps` / `fmtPct` / `fmtSpan`, lifted out of
  `InsightsWindow.tsx` so the Analytics tab can use them without importing its own parent
  (that import would be a cycle).
- **`insightsReport.ts` (428)** — the wire types for `/analytics` and `/claude-usage`,
  plus the one thing you can do with them outside the panel: flatten the screen into a
  plain-text report to paste into Claude, an email, or an incident ticket. Column
  alignment is exactly the kind of thing that stays quietly wrong forever if nothing
  asserts it, which is what the test is for.
- **`insightsCommands.ts` (123)** — "Copy investigation commands". The report answers the
  questions we thought to ask; this hands over the paths, the schema, and a few working
  queries so an assistant with shell access can slice the data itself. **No URL in it on
  purpose**: neither store is exposed over HTTP outside `127.0.0.1`, and nothing here
  suggests changing that — these are local reads on a box the user already has a session
  on. `local` picks desktop paths over remote ones; the metrics block keeps the same schema and queries for local, pointed at `insights-local.db` (`%APPDATA%\ymux` / `~/Library/Application Support/ymux`) — there is no "no history" variant any more.

## Invariants

- **Rule #5** — no `any`. `XtermInternals` in `terminalInstance.ts` is the pattern: a
  minimal typed view into a private API rather than a cast.
- **Rule #9** — `createLogger`, never `console.*`.
- **Rule #1** — the diagnostic flags around links log *that* something matched, never the
  matched text.
- Local and remote RTL behaviour are separated by profile and must stay that way.
- `src/bindings/` is generated. Edit the Rust struct.
- **Prices live in `claudePricing.ts` and nowhere else.** If you find yourself adding a
  rate to Go or Rust, that is the bug.

## Read the source when

You need an xterm addon's exact wiring, the full RTL decision table, or a specific
accelerator's parse rules. All four RTL modules have unit tests
(`textDirection.test.ts`, `bidi.test.ts`, `copyBidi.test.ts`, `mouseRtl.test.ts`) —
those tests are the specification and are deliberately not covered by this vault file.
