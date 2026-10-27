# xterm.js 6.0.0 — cell width of zero-width/format chars (bidi isolates)

- Source of truth: `@xterm/xterm@6.0.0` package ships `src/`; locally absent (no node_modules) → `npm pack @xterm/xterm@6.0.0` into an empty /tmp dir
- Default provider = `UnicodeV6` (`src/common/input/UnicodeV6.ts`, 145 lines); only public API = `term.unicode.register(p)` / `.versions` / `.activeVersion` (`typings/xterm.d.ts:1871-1900`); no accessor for the registered V6 object; bundle mangles `_`-private props → no internal grab
- `IUnicodeVersionProvider` = `{ version; wcwidth(cp): 0|1|2; charProperties(cp, preceding): number }` — `charProperties` REQUIRED in 6.0 d.ts
- Property encoding (`UnicodeService.createPropertyValue`): `(state << 3) | (width << 1) | shouldJoin`
- V6 width of U+2066–2069 (LRI/RLI/FSI/PDI) = **1** (V6 `BMP_COMBINING` has `[0x2060,0x2063]`, `[0x206A,0x206F]`, not 2066–2069); `@xterm/addon-unicode11@0.9.0` table has `[0x2066,0x206F]` → 0, but also moves emoji/CJK widths
- `InputHandler.print` (`src/common/InputHandler.ts:544-640`): width-0 char WITHOUT `shouldJoin` still `x++` (own cell); with `shouldJoin && x > 0` → appended to cell `x-1`, cursor advances `chWidth - oldWidth`
- `EscapeSequenceParser` resets `precedingJoinState = 0` on every escape/control transition → width-0 char right after an SGR is NOT joined by V6/V11 logic → still costs a cell
- Consequence: zero-width isolates need `charProperties` = `shouldJoin=1, width=width(preceding)` (join even when preceding=0); residual: at column 0 (`x == 0`) xterm writes it into its own cell
- DOM renderer: every span `display: inline-block` (`src/browser/renderer/dom/DomRenderer.ts:144`); spans split on letter-spacing (`DomRendererRowFactory.ts:169-197`) → a cell holding only FSI (measured width 0) is its own span; a joined FSI rides the preceding cell's span
- Repo use: smart-bidi filter inserts FSI/PDI (`docs/vault/backend-sessions.md` § `bidi_filter.rs`; web port `app/src/backend/web/bidiFilter.ts`); width fix lives in the xterm setup (`app/src/terminalInstance.ts` constructor, `new Terminal(` — the only construction site; desktop, browser and popout all go through `TerminalInstance`)
