# RTL per-line direction — test matrix (v0.4.4, Approach C)

ymux gives every **visible** terminal row an explicit `dir` computed from its
text by `detectDirection()` (`app/src/textDirection.ts`), replacing xterm.js's
`dir="auto"` ("first strong directional character wins"), which mis-rendered a
mixed Hebrew+Latin line that happened to start with Latin.

**Rule (Yossi):** a line with **any** Hebrew/Arabic char → **RTL** (mixed OR
pure); a **pure-Latin** line → **LTR**; digits / symbols / whitespace only →
**LTR** (safe default).

Only affects the `auto_per_line` RTL mode (the default). Gated by
**Settings → Terminal → "Auto-direction per line"** (default ON).

## Unit tests

`app/src/textDirection.test.ts` — 41 cases (`node:test`), of which 5 cover
`rowDirections` / `force_rtl`. Run:

```
cd app && node --experimental-strip-types --test src/textDirection.test.ts
```

## Detection matrix

| Input | Expected | Why |
|-------|----------|-----|
| `1. Hello world` | **LTR** | pure Latin |
| `1. שלום עולם` | **RTL** | pure Hebrew |
| `1. שלום world` | **RTL** | mixed → RTL |
| `/opt/wa/.shared.env` | **LTR** | pure ASCII path |
| `שרת רץ על port 4200` | **RTL** | mixed Hebrew + latin/digits |
| `` (empty) | **LTR** | safe default |
| `12345` | **LTR** | digits only |
| `→ ← ↑ ↓` | **LTR** | arrows/symbols only |
| `$ ls -la /home` | **LTR** | shell prompt + command |
| `git commit -m 'תיקון'` | **RTL** | Latin command with Hebrew arg |
| `ERROR: קובץ לא נמצא` | **RTL** | Latin word then Hebrew |
| `مرحبا بالعالم` | **RTL** | pure Arabic |
| `run مرحبا now` | **RTL** | mixed Arabic + Latin |
| `הפורט 4200 פתוח` | **RTL** | Hebrew wrapping a number |

Within an RTL line, embedded Latin runs (paths, `port 4200`) keep their natural
LTR order via the browser's BiDi algorithm — the row's paragraph direction is
RTL, the runs are not reversed.

## Visual smoke test (real-world)

1. RTL mode ON (Hebrew UI). Connect a shell; `printf` a numbered list where one
   item is a pure ASCII path and the others start with Hebrew → the path line
   sits LTR, the Hebrew/mixed lines sit RTL, list markers align on the right.
2. `echo "שרת רץ על port 4200"` → whole line RTL; "port 4200" reads L-to-R
   inside it.
3. `cat` a source file (pure Latin) → unchanged LTR, no flips.
4. Toggle **Settings → Terminal → Auto-direction per line = OFF** → every row
   renders LTR (classic terminal). Toggle back ON → per-line detection resumes
   live (no reconnect).

## TUI-owns-bidi smoke (Claude Code visual-order RTL)

Covers `tuiOwnsBidi` and the hook-driven `claudeActive` signal (`setTuiSignal`). **Run this whole section after any
merge that touches `terminalInstance.ts` or `textDirection.ts`** — the feature
was silently lost in merge `bcaa330` (2026-07-31) and stayed gone for 18 days
with a green test suite, because the merge deleted the code and its tests
together. See `docs/DECISIONS.md`, the TUI-owns-bidi entry.

1. Start Claude Code in a pane and print Hebrew → renders correctly: no
   reversed letters, no left-aligned scramble, no clipped glyphs.
2. `%APPDATA%\winmux\debug.log` shows `[TERM] tui-owns-bidi on pane=<id>` at
   Claude start (the ymux Claude hook reports it — no OSC title involved). **If this line never appears, the feature is inert** — for
   tmux panes that is expected to be the open question (see follow-up 3 in
   DECISIONS); for a local pane it is a bug.
3. Type a Hebrew sentence ending in `?` into Claude's input box → the `?` stays
   at the **end** of the line, not the start. This is the exact A/B symptom
   from the round-6 diagnosis and the fastest single check.
4. Exit Claude → `tui-owns-bidi off` in the log → Hebrew shell scrollback flips
   back to per-line RTL.
5. **Repeat steps 1–4 twice in the same pane.** This is what catches the
   latch-ON failure: if the off-signal never arrives, every later shell screen
   renders forced-LTR with no recovery short of a new pane.
6. Devtools console: no xterm parser errors. (Round 6 saw 519 `FSI U+2068`
   errors when the pipeline double-bidi'd.)
7. Repeat **inside tmux over SSH**, not only locally — the hook signal
   does not depend on tmux passing titles through, so `tui-owns-bidi on` must appear here too.
8. Restart the app with session restore on, reattaching a pane whose Claude is
   already running (hooks fire on events, so no hook may arrive until Claude's next event) → check
   whether the state engages. New interaction; the feature predates session restore.

Accepted costs, **not** failures (`DECISIONS.md`, 2026-07-17 — display
correctness wins): the caret sits one cell forward when typing Hebrew to
Claude, and mouse selection on mixed Hebrew/English lines starts a few cells
off.

## Sidebar (chrome, not terminal — same principle)

The rail applies the same per-string rule the terminal applies per line:
`.ws-name` and the group name carry `unicode-bidi: plaintext`, so each name
resolves its OWN base direction instead of inheriting the document's. Without
it, every latin workspace name inside an RTL rail is a lone LTR run in an RTL
paragraph — it gets pushed to the inline end and, once the rail is narrowed,
ellipsizes at its **head** (`…rver-7`) instead of its tail.

1. Language **Hebrew**, at least one latin-named workspace (`server-7`) and one
   Hebrew-named one (`שרת 9`) in the list.
2. **Every name starts at the rail's start edge** — right in Hebrew, left in
   English — regardless of the name's own script. `plaintext` governs which
   direction the glyphs run and therefore which end ellipsizes; it must NOT be
   allowed to govern alignment too, or `text-align: start` resolves per string
   and latin names in a Hebrew rail jump the width of themselves away from the
   dot, leaving a ragged 0-123px gutter. Hence the explicit
   `[dir="rtl"] .ws-name { text-align: right }` / `[dir="ltr"] … { left }`.
3. Drag `.sidebar-resizer` down to ~165px so both names clip.
   - Latin name → text starts at the **left** edge, `…` at the **right**.
   - Hebrew name → text starts at the **right** edge, `…` at the **left**.
   - Hovering either row shows the full name (`title` is set unconditionally,
     not only in icons mode).
4. Switch to **English** and repeat: the two clip in the same relative
   directions, mirrored.
5. The trailing status cluster (`.ws-meta`) sits flush to the inline end in
   both languages, and the kind/pane badge forms a straight column down the
   list — it is last in the cluster and has a fixed-width slot precisely so
   that column does not go ragged when a row also has a ports or live marker.
6. Group chevron points **into** the group when collapsed in both directions
   (`[dir="rtl"] .group-header.group-collapsed .group-header-chevron`).
7. `«` on the collapse button is `Bidi_Mirrored`, so it renders as `»` under
   `dir="rtl"`. That is correct, not a bug: the rail is on the right in RTL and
   the arrow should point at the edge it collapses toward.

`app/dev/sidebar-fixture.html` (vite dev → `/dev/sidebar-fixture.html`) renders
the rail's real DOM against the real stylesheets in six configurations at once
— RTL/LTR, light/dark, 224px/165px/48px, and a redesign preset — which is the
only way to check the matrix without live workspaces.

## Performance

- Only **visible** rows carry DOM nodes, so scrollback size (up to millions of
  lines) is irrelevant — the pass touches ~24–50 rows max.
- Row mutations are coalesced to **one pass per animation frame**
  (`requestAnimationFrame`).
- A per-row text cache (`WeakMap<Element,string>`) skips any row whose text is
  unchanged since the last pass.

## Cursor interaction (PARKED "RTL caret", 2026-06-26)

`isCurrentLineRtl()` (the Left/Right arrow-mirroring gate) now uses the **same**
`detectDirection()` rule, so the caret/arrow behaviour matches the visual
direction on mixed lines. Candidate fix for the parked caret item — **verify
live** before marking it resolved.

## `force_rtl` — full RTL (2026-08-23)

A fourth `rtl_mode`, alongside `auto_per_line` / `bidi_reorder` / `off`. It uses
the same DOM renderer as `auto_per_line` but makes **no per-line decision at
all**: every visible row gets `dir="rtl"`. Added for **remote** panes on Yossi's
ask — "RTL מלא, ולא שורה שורה". Opt-in; neither profile default moved.

Everything in the matrix above is bypassed in this mode — the dominance vote,
the block/table grouping, `stripPaneFrame`, and the `auto_direction`,
`direction_policy` and `tui_owns_bidi` knobs. `rowDirections()` reads none of
them, and the unit tests assert that across every combination.

**Settings → Terminal → RTL → profile `remote` → "Full RTL (remote panes)".**
The switch is live — no new pane, no rebuild.

| What you run | Expected | Why |
|---|---|---|
| `echo "שלום עולם"` | right-aligned, letters in order | logical stream + `dir="rtl"` |
| `ls -la` (pure Latin) | right-aligned, **letters not reversed** | one LTR run inside an RTL paragraph |
| `2. /opt/wa/.shared.env - הערה` | RTL, path still readable | UAX #9 resolves the Latin run |
| drag-select a Hebrew row | selection tracks the pointer | `mouseRtl` mirrors `clientX` off the row's live `dir` |
| copy that selection | logical order | DOM child order is unchanged by `dir` |
| Left/Right arrows at a prompt | mirrored | `isCurrentLineRtl()` returns true unconditionally here |
| `htop`, `vim`, tmux status bar | **mirrored layout** | see below |

### The known cost — not a bug

A **positional** row is one where a TUI already chose the column of every glyph:
tmux's status line, a zellij frame, `vim`, `htop`, Claude Code. Those rows are
full-width, so UAX #9 rule L2 reverses the *order of runs* inside them and the
layout comes out mirrored. `RTL_DOMINANCE` and `stripPaneFrame` exist precisely
to avoid that in `auto_per_line`; `force_rtl` trades it away on purpose in
exchange for a shell that is unconditionally right-to-left. Switch back to
`auto_per_line` for TUI work — one click, takes effect immediately.

### Two traps if you change this code

Both produce *reversed letters*, which looks like the same bug from a
screenshot:

1. **`normaliseIncomingToLogical` must stay pinned to `auto_per_line`.** It asks
   whether the incoming buffer is VISUAL order (a local/ConPTY condition), not
   whether the mode paints a `dir`. A remote stream is logical already, and
   normalising it reverses it.
2. **`unicode-bidi: bidi-override` is gated on the mode, not on `suppress`.**
   The override paints bytes verbatim, which is right only while the buffer is
   still visual. Paired with `dir="rtl"` over a logical buffer it reverses every
   Hebrew word.
