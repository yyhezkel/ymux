---
vault: frontend-lib
covers:
  - app/src/terminalInstance.ts
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
  - app/src/sessionRestore.ts
  - app/src/logger.ts
  - app/src/shortcuts.ts
  - app/src/stt.ts
  - app/src/platform.ts
  - app/src/download.ts
  - app/src/fontProbe.ts
  - app/src/i18n/index.ts
---

# Frontend library modules

The non-component half of `app/src/`. Two things dominate: the terminal wrapper, and
**RTL** — four separate modules exist because Hebrew broke in four different places.

## `terminalInstance.ts` (1,935) — the xterm.js wrapper

`class TerminalInstance` owns one xterm `Terminal`, its `FitAddon`, the optional
`WebglAddon`, and the DOM container. Module-scope globals cache font family/size, theme,
and the Ctrl+C-copies-selection flag so new panes construct with the current values;
`setTerminalTheme` / `setTerminalFont` / `setRtlProfiles` push changes to live panes.

Things it does that are easy to get wrong:

- **`applyRowDirections` / `ensureDirObserver`** — a `MutationObserver` coalesces a burst
  of cell mutations into **one** `applyDir()` per animation frame, and a `WeakMap` cache
  skips any row whose text is unchanged. Without both, per-line direction is a
  per-mutation DOM write.
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

**`force_rtl` (2026-08-23)** is the mode with no heuristics. Every row gets
`dir="rtl"`, full stop: no dominance vote, no block grouping, no `stripPaneFrame`,
and `autoDirection` / `directionPolicy` / the `suppress` signal are all inert.
That inertness is a **contract**, not an oversight, and `textDirection.test.ts`
pins it across every combination of the three knobs. It was added for remote
panes on Yossi's ask — "RTL מלא, ולא שורה שורה" — where the stream is logical and
a shell reads best unconditionally right-to-left. Latin runs inside a row still
come out correct, because the browser resolves them as LTR runs inside an RTL
paragraph. The **known cost**: a POSITIONAL row — tmux's status line, a zellij
frame, vim, htop — is full-width, so UAX #9 rule L2 reverses the order of its runs
and the layout renders mirrored. `RTL_DOMINANCE` and `stripPaneFrame` exist to
prevent exactly that in `auto_per_line`; `force_rtl` trades it away deliberately.
It is opt-in, neither profile default moved, and switching back is one click.

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
first shipped gated on `tuiOwnsBidi`, and because the OSC title propagates over SSH — that
is how Claude Code is detected at all — it fired on remote panes and broke them. Yossi's
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

`rowDirections(mode, texts, {auto, suppress, dominance})` is the whole-pane
decision, and the one place `force_rtl` and `auto_per_line` diverge. It was
lifted out of `TerminalInstance.applyRowDirections` so it could be tested at
all — the method is bound to the DOM and never ran under `node --test`, and in
these modules the tests *are* the specification.

**`bidi.ts` (71)** — the `bidi_reorder` path (bidi-js, no type defs). Exports the escape
matcher so the visual→logical pass protects escapes **exactly** the way this file does —
one definition of "what an escape looks like".

**`copyBidi.ts` (185)** — visual→logical for text on its way to the **clipboard**.
Measured on Yossi's machine, 2026-08-20: plain PowerShell renders reversed on screen but
pastes correctly, while Claude Code renders correctly and pastes reversed — exactly
inverted, because the two panes hold opposite orders in the buffer.

**`mouseRtl.ts` (82)** — coordinate transform for RTL rows. xterm's `SelectionService`
maps `clientX` → buffer column assuming LTR. With `dir="rtl"` on a row the browser paints
it mirrored, so a click on what the user sees as cell 5 lands on cell `cols - 5 - 1`.
Selection and click positioning both land on the wrong side without this.

## Typed mirrors

**`types.ts` (582)** — the data-model types are **generated from the Rust structs by
ts-rs** and re-exported here so `from "./types"` keeps working. Regenerate after a Rust
struct change with `cd app/src-tauri && cargo test`. **Do not hand-edit
`src/bindings/*.ts`.** Note ts-rs renders `Option<T>` as `T | null` — a required,
nullable key, not `T?` — so helpers such as `effectiveIdentity` widen their params to
`T | null | undefined`. The hand-written helpers here (`paneCaps`, `profileFor`,
`describeConnection`, `isLocalConn`, `isRemoteEffective`, `collectPanes`, `findPane`)
are what components use to reason about a pane.

**Not everything here is generated.** `TmuxSessionInfo` and `ForeignScope` are
**hand-written mirrors** of structs that live in `lib.rs` rather than `ymux-types`, so
ts-rs never sees them and nothing regenerates them for you. A field added on the Rust
side is silently missing here until someone types it — update both in the same commit.
Phase 90 added two more of these: `TmuxSessionInfo.owner_cwd` (the claim-time cwd from
`session-owners.json`, a grouping key only) and `SessionSummary`, the row shape of
`sessions_overview_summarize` (`status` is a closed union ending in `unknown`, which is
what the backend emits for anything the model did not say cleanly).

**`settings.ts` (751)** — the typed settings mirror plus load/save and the CSS-variable
apply. `src-tauri/src/settings.rs` owns the canonical schema; this follows it. Also
carries the font-catalog bindings: `fontCatalog` (each item now reporting whether it is
`installed`, read from the font directory on every call rather than from any record of
past installs), `fontInstall`, and `fontUninstall`.

## Small modules

- **`logger.ts` (77)** — `createLogger(tag)`. Lines reach both devtools and the single
  local `debug.log` via the `ui_log` command, tagged `[UI:TAG]`. Level filtering is
  **double-gated**: skip the IPC below the threshold here (cheap), and the backend filters
  again — the backend is authoritative, so a popout window that never loads settings still
  behaves. **Import this before the console monkeypatch.** Rule #9.
- **`i18n/index.ts` (86)** — dictionaries statically imported (~30 KB total, no async
  loader). Active language and direction are two signals, so `t(key)` and the document
  `dir` react together. A missing key returns the key itself.
- **`platform.ts` (50)** — host OS resolved **once** from Rust (`host_platform`,
  `std::env::consts::OS`). Exists because two Windows-only assumptions were baked in as
  literals and both broke on mac: local paths joined with a hardcoded `\`, and drag-drop
  positions divided by `devicePixelRatio` (WebView2 reports physical pixels, wry's macOS
  backend reports logical points).
- **`sessionRestore.ts` (102)** — remembers which tmux session each SSH pane was attached
  to, so the next start re-attaches instead of showing [Connect]. **localStorage on
  purpose**: per-machine, high-churn session state, the same class as window rects and
  sidebar width. Losing it costs one click, never data, and it keeps Rule #7's
  atomic-write surface small.
- **`shortcuts.ts` (380)** — the accelerator registry, not just a parser. It owns
  `ShortcutsSettings`, `DEFAULT_SHORTCUTS`, `SHORTCUT_ACTION_IDS` and
  `SHORTCUT_GROUPS` (the Settings tab's row order; BRIEF added `toggle_queue`
  Ctrl+Shift+Q and `show_briefing` Ctrl+Alt+Q, both in the general group), parses
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

- **`claudePricing.ts` (247)** — **the one place ymux knows what Claude costs.** Both
  backends count tokens and refuse to price them, because token counts are facts and
  prices are a table that goes stale; keeping the table here makes a price change a
  one-file edit instead of a server rebake plus a matching edit in the Rust mirror.
  `PRICING_AS_OF` records when it was last checked. Rates are **Anthropic first-party API
  list prices** (Bedrock and Vertex are partner-priced and not modelled), and
  `ModelPrice.promo`/`until` exists so a launch rate expires instead of silently
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
  on. `local` picks desktop paths over remote ones.

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
