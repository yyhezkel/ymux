# ymux-app-src-sidebar-css — research report

## Question
FOLLOWUPS.md has a P3 entry dated 2026-08-19 ("SIDEBAR SURFACES STILL UNEXERCISED after the design pass"). It lists five sidebar surfaces that have never been run live since the px → `--w-fs-*` sweep and the move of the rail CSS out of `App.css` into `app/src/sidebar.css` (`b16537d`, 2026-08-19).

This headless research stage **cannot run the app**. It has no display. The researcher role forbids starting servers. ymux's CLAUDE.md Rule #17 bans local `npm run tauri dev` / builds. So every question is answered by a **static audit of the CSS and the Sidebar.tsx code path** at HEAD `1046f77`. The audit says what the code will do and flags the places where the code (or the FOLLOWUPS script) is wrong. **A live pass/fail is still an Open question for every Q.** The FOLLOWUPS entry must not be closed on this report alone.

The sidebar markup has changed a lot since the entry was written: Phase 91.C/91.E/91.F and 92 (`git log a0ce0d0..HEAD -- app/src/sidebar.css app/src/Sidebar.tsx` → 12 non-merge commits, including `e5d6586` cards, `e554fe2` worktrees leave the sidebar, `0a0fe32` headers are not screens). Parts of the script are out of date as a result.

### Q1 — Does the UI size slider (11pt–16pt) correctly scale group headers, counts, and the bottom action row
### Q2 — Can workspaces and groups be drag-reordered successfully
### Q3 — Do context menus, inline group rename, and color picker function as expected
### Q4 — Do all 13 preset themes render correctly, including those outside the test fixture
### Q5 — Is the prefers-reduced-motion CSS properly applied when the user preference is enabled

## Findings

### Q1
**Answer:** Yes for the text. Every surface the entry names reads an em-based `--w-fs-*` token, and those tokens hang off the root font-size that the setting writes. So text in group headers, counts and the action row scales linearly with the setting. Three corrections and two caveats apply. Confidence: high that the text scales (it follows from the CSS cascade). Medium on how it looks.

- **The control is not a slider, and it does not live under Appearance.** It is `<input type="number" min="8" max="32">` (`app/src/SettingsModal.tsx:951-963`). It sits in the `textLocale` tab (`app/src/SettingsModal.tsx:897`), labelled "text & locale" (`app/src/i18n/en.json:1290`). A tester needs to know this: they will type or step 11…16, not drag.
- The chain: `applyTheme()` writes `--w-font-size-ui: <n>pt` (`app/src/settings.ts:622`). `html { font-size: var(--w-font-size-ui) }` (`app/src/App.css:75-77`). The tokens are em ratios: xs 0.77 / sm 0.85 / md 0.92 / lg 1 (`app/src/App.css:36-39`). `.sidebar` sets `var(--w-fs-lg)` (`app/src/sidebar.css:160`).
- Group header: `.group-header` sm (`sidebar.css:831`). `.group-header-name` sm (`:859`). `.group-header-count` xs (`:865`). Action row: `.ws-action-half` md (`:805`).
- **Caveat 1: em compounds.** The name and the count are children of `.group-header`, so they come out at 0.85×0.85 = 0.72em and 0.85×0.77 = 0.65em of the rail. At 11pt that is ≈ 7.9pt / 7.2pt (≈ 10.6px / 9.6px). Below the slider's floor, at 8pt, the count is ≈ 5.2pt. This still scales, but the bottom of the range should be checked for legibility. (Arithmetic from the cited ratios. Not measured.)
- **Caveat 2: some things do not scale, by construction.** The group chevron is a Lucide SVG at `size={12}` (`app/src/Sidebar.tsx:767`; default 16 in `app/src/icons.tsx:88`), so its CSS `font-size: 10px` (`sidebar.css:870`) has no effect and the glyph stays 12px. The drag ghost is fixed at `font-size: 12px` (`sidebar.css:975`). The empty-state icon is fixed at 22px (`:683`). The icons-mode emoji is fixed at 16px (`:82`). None of these is one of the three surfaces the entry names.

### Q2
**Answer:** Implemented at the code level, for both workspaces and groups. The CSS the move relocated is wired to the class names the TSX emits. Confidence: medium. No live drag has been run.

- The drag is pointer-based, with a 5px threshold (`app/src/Sidebar.tsx:289`, `:404-418`). It never starts from an interactive child (`:454`). `didDrag` swallows the trailing click, so a drag never also activates or collapses (`:427`, `:719-720`). Escape aborts the drag (`:466`).
- Workspace drops are allowed within one level only, by `parent_id`. A drop onto a group header is accepted for root rows only (`:332-363`). Group drops hit only other real group headers (`:365-376`). Index math corrects for a source above its target (`:380-402`).
- Classes emitted: `dragging` / `drop-above|below` / `drop-into` on `.group-header` (`Sidebar.tsx:713-717`) and on `.ws-item` (`:913-914`). CSS: `.ws-item` is `position: relative` (`sidebar.css:271-272`). `.group-header` is given `position: relative` for the pseudo-elements (`:1003`). The 2px accent bars are `::before` / `::after` (`:985-1001`). Drop-into is a tint plus a dashed outline (`:1006-1010`). The ghost is in `:965-980`. `body.ymux-dragging` turns off selection (`:960-963`).
- Pseudo-element collision check: the only other `::after` on `.ws-item` is the hook pulse, which is written `:not(.drop-below)` so it gives way to the indicator (`sidebar.css:739`). A grep for `ws-item|group-header|ws-card ... ::before|::after` across `app/src/*.css` found no other rule. The card's colour stripe is a real `<span>` (`Sidebar.tsx:1272`), not a pseudo-element, so it cannot collide.

### Q3
**Answer:** All three surfaces have code and CSS. The audit found one probable defect and one gap in behaviour. Confidence: medium. Both need a live click to confirm.

- **Probable defect: inline group rename does not take focus.** The rename `<input>` relies on the `autofocus` attribute (`app/src/Sidebar.tsx:741-759`). The same file states, 460 lines later, that "`autofocus` doesn't fire on elements inserted into an already-loaded document". The new-group input therefore focuses itself explicitly with `ref={(el) => queueMicrotask(() => el.focus())}` (`Sidebar.tsx:1207-1209`). The solid-primitives docs agree that native `autofocus` "only works on page load, which makes it incompatible with SolidJS" (jsr.io, see Sources). What follows: Rename → the input appears unfocused. Typing goes nowhere until the user clicks into it. Enter/Escape do nothing until then. `onBlur` cancel (`:758`) cannot fire for a field that was never focused, so clicking away can leave the input open.
- **Gap: no outside-click / Escape dismissal for the menus.** The workspace menu, the group menu and the colour picker close only when an item or swatch is picked, or when the same row is right-clicked again (`Sidebar.tsx:723-731`, `:936-948`, `:805-818`). The only window listeners are the drag listeners and an Escape key that aborts a drag (`:459-468`). No other file references `.ws-menu` / `.group-menu` / `.group-swatch-picker` (a grep across `app/src` returned nothing outside `Sidebar.tsx`). The two menus use separate signals (`:174`, `:181`), so both can be open at once. Possibly deliberate. It needs a live check.
- Workspace menu: `position: fixed` at the click point, clamped with fixed 200×260 margins rather than the measured size (`Sidebar.tsx:475-480`, `:1095-1110`). Its CSS is in `sidebar.css:611-642`. Header-only items are guarded by `isHeaderRow` (Phase 92, `:1121`). The menu is rendered **inside** the `.ws-item` (the row closes at `:1240`), so its buttons sit at md-of-md = 0.92×0.92 ≈ 0.85em. The group menu is a sibling of its header, so it renders at 0.92em (`sidebar.css:288`, `:636`, `:900`). The two menus therefore come out at slightly different text sizes.
- Colour picker: 18px round swatches from `GROUP_PICKER_COLORS` (`Sidebar.tsx:38`, `:805-818`; `sidebar.css:906-922`). Clicking one calls `onGroupSetColor` and closes the picker. The header swatch reads `--group-color` (`Sidebar.tsx:734-737`; `sidebar.css:842-849`).

### Q4
**Answer:** Not verifiable statically as "renders correctly". The audit found three concrete cross-preset defects in the CSS, and found that the fixture no longer covers the sidebar as it exists. Confidence: medium-high that the cascade findings are real (selector specificity and file order). Low on visual severity.

- The 13 presets: tokyo-night, dracula, solarized-dark, nord, solarized-light, and industry / broadsheet / modernist / classical, each with a `-dark` twin (`app/src-tauri/src/settings.rs:1501-1704`). The last 8 are the "redesign" family (`app/src/settings.ts:609-611`).
- **The fixture is stale, not just narrow.** `app/dev/sidebar-fixture.html` renders only the default palette (tokyo-night) and modernist (`:107-117`). Its last change was `a0ce0d0` (2026-08-19), before the 91.E cards. Its markup carries no `ws-card*` / `ws-header` classes. It still emits `.ws-worktree-chip` (class census of the file), which belongs to the worktree rows 91.F removed. Today's dominant row type is never exercised by it.
- **Radius flattening misses sidebar classes.** The square presets (industry*, modernist*) set `--w-r-*: 0` (`themes-redesign.css:58`, `:108`). Var-driven radii (`.ws-item`, `.ws-card`, `.ws-menu`, `.group-header`, `.ws-badge`) go square correctly. Hardcoded radii escape the explicit list (`themes-redesign.css:400-412`): `.ws-card-stripe` 2px (`sidebar.css:380`), `.ws-card-count` pill 999px (`:396`), `.sidebar-empty-icon` 12px (`:684`), `.ws-ghost` 6px (`:970`), and the drop bar 1px (`:994`). The pill may be intended. The 12px empty icon inside a square card is the visible one.
- **Specificity defeat:** `.sidebar.icons .ws-add { border-radius: 6px }` (`sidebar.css:139`) has specificity (0,3,0). The flattening rule `[data-theme-preset^=…] :is(button,…)` is (0,2,0). So in icons mode the "+" button stays rounded under industry and modernist, even though `button` is in the list.
- **Redesign presets fill the hollow "activity" dot.** `.ws-waiting-dot.activity` makes the dot hollow (`background: transparent`, inset ring; `sidebar.css:722-726`). `[data-theme-family="redesign"] .ws-waiting-dot` sets `background` and `box-shadow` (`themes-redesign.css:340-343`). Both selectors are (0,2,0). themes-redesign.css loads later (`app/src/App.tsx:141-143`), so it wins. In all 8 redesign presets the "a pane rang its bell" dot therefore renders **filled with a glow**, the same as the blocking dot except for the pulse. That undoes the "two intensities" contract the CSS comment describes (`sidebar.css:702-708`).
- Not audited: per-preset contrast of card text on the active accent block. FOLLOWUPS' P1 91.E entry already tracks that as its item (9).

### Q5
**Answer:** Yes. Reduced motion is covered by a global catch-all plus a sidebar-specific override, and nothing in the sidebar escapes it. One animation is mis-specified, and it shows up in the normal-motion case. Confidence: high on the cascade. Medium that the Tauri webviews report the OS setting (WebView2 / WKWebView, not tested).

- Global: `@media (prefers-reduced-motion: reduce) { *, *::before, *::after { animation-duration: 0.01ms !important; animation-iteration-count: 1 !important; transition-duration: 0.01ms !important } }` (`app/src/tokens.css:104-110`). It loads after sidebar.css (`App.tsx:141-142`). Because of `!important` it beats every sidebar animation: `pulse-live` (`sidebar.css:604`), `pop-in` (`:624`), `welcome-in` (`:676`), `ymux-attn-dot` (`:719`), `ymux-attn-breathe` (`:746`), and all the 80–120ms transitions (`:1014-1017`).
- Sidebar-specific: the hook pulse goes to `animation: none; opacity: 1` (`sidebar.css:749-754`), so a pulsing row shows a static ring rather than vanishing.
- **Mis-specified keyframes:** `ymux-attn-dot` animates `transform: translateY(-50%) scale(…)` at every keyframe (`App.css:4695-4704`). That made sense for the old absolutely-positioned `::after` dot. Since the dot became a static flex child of `.ws-meta` / `.ws-card-status` (`sidebar.css:563-568`, `:405-411`, `:712-721`), the -50% shifts the **blocking** dot up by 4px (half its 8px height) for as long as it pulses. Under reduced motion the one 0.01ms iteration ends with no fill-mode, so the dot sits centred. The bug is visible only with motion **on**. Inferred from CSS. Not seen live.

## Recommendation
**Do not close the FOLLOWUPS entry from this report.** Keep it open, rewrite its script to match the code, and run the live sweep on a real machine.

The corrected script: (1) Settings → *text & locale* → UI size, type 11 then 16 (it is a number field, 8–32). (2) Drag a card and a group header. (3) Right-click a group → Rename, and check whether the field has focus without a click. (4) Flip all 13 presets, including icons mode and a row with a hollow activity dot. (5) Turn on OS reduced motion.

Three code findings should go to FOLLOWUPS as separate P3 items, each a one-line CSS or TSX fix for an implementer:
- (a) group rename `autofocus` → the explicit `ref` focus already used at `app/src/Sidebar.tsx:1207-1209`, applied to `:744`.
- (b) the redesign `.ws-waiting-dot` rule overriding `.activity`: add `:not(.activity)` at `app/src/themes-redesign.css:340`.
- (c) `ymux-attn-dot`'s `translateY(-50%)` on the sidebar dot: give `sidebar.css:719` its own keyframes, or drop the translate. The pane-tab users at `App.css:7192-7197` must be checked first.

Two smaller items: add the missed sidebar classes, and an icons-mode `.ws-add` selector, to the flattening list at `themes-redesign.css:400-411`. Refresh `app/dev/sidebar-fixture.html` to emit 91.E card markup and more presets.

The trade-off accepted: these are code-reading findings, and a live run may show (a) is masked by a Solid/WebView behaviour this audit could not see. A live session where Rename focuses the field unaided would overturn (a). If the activity dot renders hollow under broadsheet, that overturns (b).

## Open questions
- Q1–Q5 live pass/fail: all still open. They need a human running `npm run tauri dev` on Windows or macOS (Rule #17 forbids it here) with the corrected script above.
- Q1: legibility of the 0.65em group count at 11pt (≈ 9.6px). Needs eyes on a live rail.
- Q3: whether Solid 1.9.12 (`app/package-lock.json:2196-2197`) or the webview focuses a dynamically inserted `autofocus` input. Test: one Rename click.
- Q3: whether menus without outside-click dismissal are intended. Ask Yossi, or check `git log -S"groupMenuFor" -- app/src/Sidebar.tsx` (first introduced in `4fd7689`).
- Q4: visual severity of the unflattened radii, and per-preset contrast. Needs screenshots across the 13 presets.
- Q5: whether WebView2 and WKWebView pass through the OS reduced-motion setting. Test: toggle OS reduced motion with a waiting row visible.

## Sources
- app/src/SettingsModal.tsx:897, :951-963 — UI size is a number input 8–32 in the textLocale tab
- app/src/i18n/en.json:1290 — tab label "text & locale"
- app/src/settings.ts:609-611, :622 — redesign preset list; `--w-font-size-ui` written in pt
- app/src/App.css:36-39, :75-77 — em type tokens; root font-size from the setting
- app/src/App.css:4695-4704 — `ymux-attn-dot` keyframes with translateY(-50%)
- app/src/sidebar.css:82, :139, :160, :271-272, :380, :396, :563-568, :604-642, :683-684, :712-754, :805, :831-870, :881-932, :953-1017 — the rules cited per Q
- app/src/themes-redesign.css:58, :108, :340-343, :400-412 — square radii tokens, redesign dot override, flatten list
- app/src/tokens.css:104-110 — global reduced-motion catch-all
- app/src/App.tsx:140-143 — CSS load order
- app/src/Sidebar.tsx:38, :174-186, :289-472, :475-480, :695-818, :905-948, :1095-1240, :1207-1209, :1272 — drag, menus, rename, picker, card stripe
- app/src/icons.tsx:88 — Lucide default size 16
- app/src-tauri/src/settings.rs:1501-1704 — the 13 preset ids
- app/dev/sidebar-fixture.html:107-117 — fixture VIEWS: default + modernist only
- `git show --stat b16537d` → 2026-08-19, sidebar.css +762 / App.css −729 (the CSS move)
- `git log -1 -- app/dev/sidebar-fixture.html` → a0ce0d0 2026-08-19 (predates 91.E e5d6586)
- `grep -o 'class="[^"$]*' app/dev/sidebar-fixture.html | sort | uniq -c` → no ws-card/ws-header; one ws-worktree-chip
- `grep -rn "ws-menu|group-menu|group-swatch-picker" app/src --include=*.ts --include=*.tsx | grep -v Sidebar.tsx` → no matches (no external dismissal)
- `grep -rn "ws-item…::before|::after" app/src/*.css` → only sidebar.css:739 and :985-1001
- https://jsr.io/@solid-primitives/focus/1.0.0-next.4/README.md — retrieved 2026-10-06 — "The native `autofocus` attribute only works on page load, which makes it incompatible with SolidJS."
