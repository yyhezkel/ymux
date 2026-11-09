<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: ymux-2026-09-14-manual -->

**Recommendation: remove the header button everywhere and add a default-off `smart_bidi` to the RTL profiles. Fix three defects first.** Everything below comes from reading the code; nothing was run.

**1. Cursor-addressed TUIs: improved, not correct.**
- Isolates now cost no cell when `x>0`, including after SGR (`unicodeIsolates.ts:145`, `InputHandler.ts:605`).
- At column 0 the join is skipped and `x++` still runs (`InputHandler.ts:631`). The width-0 cell isn't painted (`DomRendererRowFactory.ts:105`), so the row paints one cell left of its buffer columns.
- `docs/vault/frontend-lib.md:251` is wrong about this: there is no zero-width span.
- The trigger exists because RTL context resets only on LF (`bidi_filter.rs:136`). A CUP or CR to column 0 after Hebrew, followed by Latin, emits FSI there.
- After a CUP at `x>0`, FSI joins an unrelated cell.

Unproven:
- whether Claude, tmux and zellij hit that path
- whether FSI shares a span with its token: FSI rides the preceding cell, and spans split on letter-spacing (`DomRendererRowFactory.ts:169,196`), so it depends on the font
- any `InputHandler`-level test: the tests pin only the provider

**2. What it uniquely does.** The other modes pick paragraph direction or reorder lines. Only smart bidi makes each token atomic, which fixes edge punctuation in RTL rows (`./run.sh`, `--flag`, `/opt/x`, by UAX #9 reasoning). It should show no effect in `off`, which has no bidi pass.

It has no real per-pane use. It self-gates on nearby RTL (`bidi_filter.rs:304`), so a pane without Hebrew is untouched. RTL behaviour is keyed on pane class (`frontend-lib.md:383`). The only per-pane case left is a kill switch.

**3. Reported defects.**
- **`ESC ( B`: real, desktop and web.** `(` is outside `0x30..=0x7E` (`bidi_filter.rs:154,159`; `bidiFilter.ts:80`), so `B` becomes text and gets wrapped near Hebrew. xterm then errors on FSI to GROUND (`EscapeSequenceParser.ts:96,191-195`) and prints a literal `B`. It needs fixing before any wider rollout, but doesn't change the recommendation.
- **U+FFFD: not real.** Decode runs first (`lib.rs:2719`, `pty.ts:64`), pinned by `bidi_filter.rs:514-543`. Only a cap-flush edge remains.
- **New defect: copy pollution.** Combined isolates enter selections (`BufferLine.ts:537`) and nothing strips them (`terminalInstance.ts:760`). Copying `main.rs` yields `main.rs` plus a trailing PDI.

**4. Options.**

| Option | Gain | Cost |
|---|---|---|
| Keep button | No work | Experimental stream switch in the header; tabs mode has no control |
| Profile setting only | Matches the class rule; tabs mode gets control | Wider reach, so fixes must land first |
| Profile default + button | Full granularity | Keeps a control with no demonstrated use |
| Remove feature | Deletes about 700 lines | Loses the edge-punctuation fix |

**Files:**
- Filter fixes: `bidi_filter.rs`, `web/bidiFilter.ts`. Reset context on CR and cursor-positioning CSI (H, f, G); handle `0x20-0x2F` intermediates.
- Copy strip: `terminalInstance.ts`.
- Setting: `settings.rs`, the `RtlProfile` binding, `SettingsModal.tsx`, `settings.ts`, `backend/web.ts`, `lib.rs` (`pane_connect` takes the resolved value), i18n ×4.
- Button: `PaneView.tsx`.
- Vault: six pages.

Saved leaf `smart_bidi` values are ignored, so no pane gets stuck on.

**Size:** about 200-250 lines, one PR, about 9 source files.

**Test live:**
- Hebrew plus `./x --flag` in `auto_per_line` and `force_rtl`, on and off
- Claude Code under tmux and zellij: no row shift, no stray `B`
- a font whose Hebrew advance differs from Latin
- copy/paste of a token into a shell
- a local Claude pane (visual-order stream)

**Would invalidate it:**
- a real workflow needing the filter on in only some panes of one class
- live A/B showing no visible gain over row-dir alone, in which case remove the feature
- Claude's stream already carrying its own isolates, which shrinks the use case

**Versus `knowledge/research/ymux-2026-09-14-manual.md`: partly different.**
- **Agreed:** a profile-level default, gated on fixes, and no global key.
- **Different:** it keeps the button as a per-pane override; I drop it, because the self-gate removes the granularity argument it relies on.
- **Stale:** its blocker, the width bug, has largely landed. Column 0 and copy pollution are what remain.
- **Adopted:** its finding that Rust never reads `terminal.rtl`, so the frontend must push the resolved value, and its file count.
- **`FOLLOWUPS.md` is stale:** the "filter runs before `pty_decode`" entry is out of date.
- **Bears on the third invalidator:** `docs/DECISIONS.md:407` records that modern Claude emits visual-order Hebrew plus its own isolates.
