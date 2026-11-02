# ymux-2026-09-14-manual — research report

## Question

Phase 93 hid the smart-bidi header button in tabs mode, arguing that RTL is a Settings matter (`terminal.rtl.*`). The same argument would apply to split mode, where the button still shows. The question is whether the per-pane `smart_bidi` byte-stream filter (`pane_set_smart_bidi`) should keep a pane-header button in split mode or anywhere else, or should move to a Settings toggle. Research only, no code change.

### Q1 — Does the split-mode smart-bidi header button stay, and is per-pane `smart_bidi` state needed from the header?
### Q2 — Can the per-pane `smart_bidi` filter (`pane_set_smart_bidi`) be expressed as one global Settings key (`terminal.rtl.*`) without losing behaviour?
### Q3 — Which is the right home for the toggle: a pane-header button, a Settings toggle, or both?
### Q4 — What does each option cost in files and vault pages touched?

## Findings

### Q1

**Answer.** Yes, keep it for now. In both backends the split-mode header button is the **only** UI that writes `smart_bidi`. Removing it without a replacement would leave the filter impossible to switch on, while panes that already have `smart_bidi: true` saved would keep filtering with no visible way to turn it off. Per-pane state from the header is needed only because no other control exists yet. The Phase 93 premise that "bidi is a Settings matter" is **not true of this filter**: no `terminal.rtl.*` key controls it.

**Evidence.**
- The header action `id: "bidi"` is added only after the tabs-mode early return. Tabs mode returns with just `popout` (`app/src/PaneView.tsx:1001-1004`). The bidi action is at `app/src/PaneView.tsx:1048-1064` and calls `backend.call("pane_set_smart_bidi", …)` at `:1058`.
- It is the only caller. `grep -rn "pane_set_smart_bidi\|smart_bidi" app/src` finds the command name only in `PaneView.tsx:1058`, the web backend's implementation of it (`app/src/backend/web.ts:877-883`), comments, and the null default literals (`app/src/layoutOps.ts:40`, `app/src/tabsHarness.tsx:29`). There is no RPC, CLI or Settings writer: grep finds no `smart_bidi` in `rpc_server.rs`.
- `terminal.rtl.*` has no smart-bidi field. `RtlProfile` holds only `rtl_mode`, `auto_direction`, `mirror_arrows_rtl`, `tui_owns_bidi` and `direction_policy` (`app/src-tauri/src/settings.rs:381-416`). Grepping `app/src-tauri/src/*.rs` outside `settings.rs` for `rtl_mode` / `.rtl` returns nothing, so the Rust side never reads `terminal.rtl` at all; those profiles are applied in the frontend (`app/src/settings.ts:633` → `setRtlProfiles`, `app/src/terminalInstance.ts:333`).
- A saved flag still applies in tabs mode, even though the button is hidden there. `pane_connect` seeds the runtime filter from the leaf whatever the workspace mode (`app/src-tauri/src/lib.rs:9430`, `:9443`; `find_pane_smart_bidi` `:8881-8891`). The web backend does the same (`app/src/backend/web.ts:608-609`). So a tabs-mode pane with `smart_bidi: true` filters and has no visible indicator or way off, unless the user switches the workspace back to split mode (`tabs_mode` is a per-workspace flag, `app/src-tauri/crates/ymux-types/src/lib.rs:433`).
- The original design chose a header button on purpose. DECISIONS 2026-06-02, Phase 52: "Default OFF per pane. Toggle is a ⇆ button in the pane header" (`docs/DECISIONS.md:1690`).

**Confidence:** high for the code facts. Whether anyone uses the button is unmeasured (see Open questions).

### Q2

**Answer.** Not as a **single global key** without losing behaviour. It can be done without loss as a **per-profile default plus the existing per-pane override**, because the leaf field is already tri-state (`Option<bool>`). There are four losses or gaps a single global key would bring:

1. **Granularity.** The filter is opt-in per pane and off by default (`docs/DECISIONS.md:1690`, `docs/vault/backend-sessions.md:36-41`). One global key turns it on for every pane, shell panes included, where Phase 52 meant it for selected mixed RTL/Latin panes (hint text: "Experimental; recommended for Claude Code TUI panes", `app/src/i18n/en.json:1162`).
2. **Class split.** Every other RTL knob is split into `local` and `remote` profiles, because the two pane classes need opposite treatment (`app/src-tauri/src/settings.rs:438-465`). The split axis is computed only in the frontend (`profileFor`, `app/src/types.ts:578-580`, via `posixExec`). A `terminal.rtl.{local,remote}.smart_bidi` key would need Rust to repeat that classification at `pane_connect` (`lib.rs:9395-9443`), or the frontend to push the resolved value in.
3. **Live flip.** Today the command persists the leaf **and** flips the runtime map right away (`app/src-tauri/src/lib.rs:8956-8988`, flip at `:8981`). A Settings key would need the settings-save path to go over every live pane in `AppState.bidi_filters` (`lib.rs:207`). Rust can reach the settings, since `AppState.settings` exists at `lib.rs:189`, but nothing reads RTL settings there now.
4. **Browser backend.** The web backend seeds per attach from the leaf (`app/src/backend/web.ts:608-609`) and flips per session id (`:880-881`). It would need settings access at attach time as well.

Why the tri-state route loses nothing: `LayoutNode::Pane.smart_bidi` is `Option<bool>` (`app/src-tauri/crates/ymux-types/src/lib.rs:268`), and the reader collapses `None` to off (`lib.rs:8887`, `smart_bidi.unwrap_or(false)`). Changing that fallback to the profile default turns `None` into "inherit" with no schema change and no `WORKSPACES_SCHEMA_VERSION` bump. Existing `Some(true)` / `Some(false)` leaves keep their meaning. One gap stays: the header toggle always writes `Some(next)` (`PaneView.tsx:1057-1060`), and there is no "reset to inherit" UI.

**A blocker for any wider default, regardless of where the toggle lives.** An open P2 in FOLLOWUPS ("Smart bidi garbles cursor-addressed TUIs (Claude Code), desktop and browser alike") says the inserted FSI/PDI (U+2068/2069) are one column wide in xterm's default Unicode 6 table. That shifts rows, so Claude Code repaints over the wrong cells. It was seen live, and it is linked to `intake-ymux-app-src-terminalinstance-ts-5` (`FOLLOWUPS.md` line beginning "- [~] P2 | 2026-10-07 | app/src/terminalInstance.ts (xterm unicode)"). That ticket is still in planning: `sqlite3 -readonly pipeline.db "select name,status,stage … like '%terminalinstance-ts-5%'"` → `ymux-app-src-terminalinstance-ts-5|running|PLAN`. Making the filter a profile-wide switch now would bring a known-broken experimental filter to every pane of a class, which is exactly the class of pane the hint recommends it for.

**Confidence:** high on the structure (`Option<bool>`, no Rust reader of `terminal.rtl`). Medium on "without loss" for the tri-state design, because it is not built and the width bug can change what "behaviour" is worth keeping.

### Q3

**Answer.** **Both, in stages.** Now: keep the split-mode header button as the only control (status quo). After the xterm width fix lands: add a per-profile Settings default (`terminal.rtl.{local,remote}.smart_bidi`, default off) that leaves with `smart_bidi: None` inherit, and keep the header button as the per-pane override. Bringing the button back in tabs mode is out of scope (Phase 93 stands). The Settings default is what gives tabs-mode panes a way to change the filter, and so it is what makes Phase 93's stated reason true.

**Evidence.**
- The header is where the per-pane state lives and is the only writer (Q1 evidence).
- Phase 93's reason ("bidi is a Settings matter", `app/src/PaneView.tsx:990-992`, `docs/vault/frontend-shell.md:416-421`) only becomes true once a `terminal.rtl.*` key exists. Today a tabs-mode user has no control at all (Q1).
- Settings already has a per-profile RTL block for exactly this kind of knob: radios plus booleans written as a complete profile through `setRtlField` (`app/src/SettingsModal.tsx:267`, `:488`; `docs/vault/frontend-flows.md:123-129`).
- The P2 width bug (Q2) is why the Settings default should wait. A wider default doubles the reach of a known garbling bug.

**Confidence:** medium. This is a judgement call between the cost in Q4 and how much the filter is used, which is unmeasured.

### Q4

Counts come from where the touched symbols live today and from each vault page's `covers:` list.

| Option | Source files touched | Vault pages touched | Notes |
|---|---|---|---|
| **A — keep split-only button (status quo)** | 0 | 0 | Phase 93's comment (`PaneView.tsx:990-992`) and vault line (`frontend-shell.md:419-420`) stay inaccurate. Tabs-mode panes keep no control. |
| **B — Settings toggle only (remove header button)** | ~9: `settings.rs` (field + default + migration test), `lib.rs` (seed from profile at `pane_connect`, live re-apply on settings save, retire or keep `pane_set_smart_bidi` + registration `:13143`), `SettingsModal.tsx` (RTL block + `RTL_FIELD_DEFAULTS`), `terminalInstance.ts` / `settings.ts` (`RtlProfileSettings` mirror), `backend/web.ts` (seed from settings), `PaneView.tsx` (drop action `:1048-1064`), i18n × 4 (`en/he/ar/ru.json`), plus generated `bindings/RtlProfile.ts` | 6: `backend-wizards.md` (settings.rs), `backend-core.md` (lib.rs), `backend-sessions.md` (filter seeding prose `:36-41`), `frontend-flows.md` (SettingsModal), `frontend-lib.md` (web.ts, terminalInstance), `frontend-shell.md` (PaneView). Plus `crates.md` if the leaf field is removed. | Loses per-pane granularity (Q2 §1). The Rust side has to learn the local/remote class (Q2 §2). Hits the P2 width bug class-wide. `docs/DECISIONS.md` entry needed (Phase 52 decision reversed). |
| **C — both: profile default + header override (recommended, after the width fix)** | ~9, same as B except `PaneView.tsx` keeps the button (optionally a "reset to inherit" menu entry). The `lib.rs:8887` fallback changes from `false` to the profile value. No `LayoutNode` schema change (`ymux-types lib.rs:268` already `Option<bool>`). | 6, same as B minus the `crates.md` risk. | Keeps all current behaviour. Adds an inherit state. Needs an i18n string for "inherit" if a reset is offered. |
| **D — remove the button everywhere, no replacement** | 1 + i18n cleanup | 1 (`frontend-shell.md`) | Rejected: the filter becomes unreachable, and saved `true` leaves keep filtering with no way off (Q1). |

Side finding: `pane.smartBidi.label` is defined in all four locales (`app/src/i18n/en.json:1161`) but nothing in `app/src` outside `i18n/` references it (grep → no hits). It is dead today; a Settings toggle (B/C) could reuse it.

## Options

- **A — status quo.** Free. Leaves the Phase 93 rationale untrue and tabs-mode panes with no control.
- **B — Settings only.** ~9 files / 6 vault pages. Loses per-pane choice and spreads the open P2 garbling bug class-wide.
- **C — profile default + header override.** ~9 files / 6 vault pages. Keeps all behaviour (tri-state `Option<bool>` already exists) and gives tabs mode a control. Best after the width fix.
- **D — drop the button, nothing else.** Breaks the feature. Rejected.

## Recommendation

**Do A now and plan C once `ymux-app-src-terminalinstance-ts-5` (the xterm FSI/PDI width fix, in PLAN at the time of writing) has landed and been verified live.** Keep the split-mode ⇆ button, because it is the only control for a per-pane, default-off, experimental filter. Moving that filter to Settings today would widen a known P2 garbling bug from one opted-in pane to a whole pane class. After the fix, add `smart_bidi` to `RtlProfile` (`app/src-tauri/src/settings.rs:381`) with default off for both profiles. Change the `find_pane_smart_bidi` fallback (`app/src-tauri/src/lib.rs:8887`) and the web seed (`app/src/backend/web.ts:609`) so that `None` inherits the profile value, and keep the header button as the per-pane override. That makes Phase 93's "bidi is a Settings matter" true and gives tabs-mode panes a control without bringing the button back there. The trade-off accepted is ~9 files and 6 vault pages for what is today an experimental feature. Two things would change this: usage data showing nobody turns the filter on (then it is cheaper to retire the feature than to grow it), or the width fix failing (then the filter should stay strictly per-pane, or be withdrawn). Meanwhile, the Phase 93 comment at `app/src/PaneView.tsx:990-992` and `docs/vault/frontend-shell.md:419-420` should be corrected the next time that code is touched, to say that tabs mode currently has **no** smart-bidi control.

## Open questions

- **Usage.** How many saved leaves have `smart_bidi: true`? Answer: `grep -o '"smart_bidi":true' %APPDATA%\ymux\workspaces.json | wc -l` on Yossi's machine(s). There is no workspaces.json on this host (`ls ~/.config/ymux` → absent).
- **Width fix outcome.** Will a zero-width provider for U+2066–2069 keep the DOM renderer's isolation? Answer: the live smoke in `ymux-app-src-terminalinstance-ts-5` (FOLLOWUPS P2 "Smart bidi garbles cursor-addressed TUIs").
- **Interaction with `tui_owns_bidi` / visual→logical normalisation for local Claude panes.** Does FSI/PDI wrapping still help once the stream is normalised to logical order? Not researched (filter internals are out of scope). Answer: a live A/B on a local Claude pane with the filter on and off after the width fix.

## Sources

- `app/src/PaneView.tsx:987-1004` — tabs-mode early return; Phase 93 comment claiming bidi is a Settings matter
- `app/src/PaneView.tsx:1048-1064` — split-mode `bidi` action, the only `pane_set_smart_bidi` caller
- `app/src-tauri/src/lib.rs:8956-8988` — `pane_set_smart_bidi`: persist leaf + flip runtime filter
- `app/src-tauri/src/lib.rs:8862-8891` — leaf setter/reader; `unwrap_or(false)` at `:8887`
- `app/src-tauri/src/lib.rs:9395-9447` — `pane_connect` seeds the filter from the leaf before spawn
- `app/src-tauri/src/lib.rs:2660-2723` — `emit_data`: decode → OSC → `apply_to_pane`
- `app/src-tauri/src/lib.rs:189`, `:207` — `AppState.settings`, `AppState.bidi_filters`
- `app/src-tauri/src/settings.rs:381-465` — `RtlProfile` fields (no smart-bidi), `RtlProfiles` local/remote rationale
- `app/src-tauri/crates/ymux-types/src/lib.rs:268`, `:433` — `smart_bidi: Option<bool>`, `tabs_mode: bool`
- `app/src/backend/web.ts:607-609`, `:877-883` — browser backend seed + toggle
- `app/src/types.ts:578-580` — `profileFor` (posixExec axis, frontend only)
- `app/src/SettingsModal.tsx:267`, `:488` — `RTL_FIELD_DEFAULTS`, `setRtlField`
- `app/src/settings.ts:633`, `app/src/terminalInstance.ts:333` — RTL profiles applied in the frontend
- `app/src/i18n/en.json:1161-1164` — smart-bidi strings ("Experimental")
- `docs/DECISIONS.md:1688-1694` — Phase 52 decision: default off per pane, header ⇆ button
- `docs/vault/frontend-shell.md:416-425`, `docs/vault/backend-sessions.md:34-52`, `docs/vault/frontend-lib.md:144-154`, `docs/vault/frontend-flows.md:123-129`, `docs/vault/backend-wizards.md:76-101` — vault coverage
- `FOLLOWUPS.md` entry "Smart bidi garbles cursor-addressed TUIs (Claude Code), desktop and browser alike" — open P2, FSI/PDI width bug
- `grep -rn "pane_set_smart_bidi\|smart_bidi" app/src app/src-tauri/src` → single UI writer (PaneView.tsx:1058), no RPC/CLI writer
- `grep -rn "rtl_mode\|\.rtl" app/src-tauri/src/*.rs | grep -v settings.rs` → no hits (Rust never reads `terminal.rtl`)
- `grep -rn "smartBidi.label" app/src | grep -v i18n` → no hits (dead key)
- `sqlite3 -readonly pipeline.db "select name,status,stage from tickets where name like '%terminalinstance-ts-5%'"` → `ymux-app-src-terminalinstance-ts-5|running|PLAN`
- `git log --oneline --all -i --grep="smart.bidi\|smart_bidi"` → 4166eb1e (Phase 52), 14622c04 (seed fix), 1b7f6f1a (Phase 117 web), 1a4ccc2e (P2 width followup)
