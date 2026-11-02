# ymux-322655e-app-src-terminalinstance — research report

## Question
Commit `322655e feat(rtl): normalise Claude's visual output to logical on the way in` (2026-08-20) made local Claude panes show Hebrew with correct letters *and* right-edge placement, and Yossi's live test the same day tied a typing lag to the same commit (FOLLOWUPS.md, the P1 entry starting "322655e — app/src/terminalInstance.ts (flushPending, …"). This report explains why logical order is required, what the conversion costs and where that cost lands, whether the latency is measured or inferred, which alternatives were rejected, and what is still unknown. Everything here is from the code and the repo's records. Nothing was run live (goal `## Out of scope`).

### Q1 — What makes LOGICAL order necessary for correct display, and why did VISUAL order fail?
### Q2 — Is the typing latency real (reproduced), inferred, or a design risk?
### Q3 — Why does the early-exit pattern (RTL_RE.test, all-Latin short-circuit) prevent latency in the common case?
### Q4 — What design alternatives were considered and rejected, and why?
### Q5 — What remain unknown unknowns (caret behavior, Claude's box-drawing preservation, fragment base direction)?

## Findings

### Q1
**Answer:** Two different mechanisms produce correct letters and right-edge placement, and only one buffer order lets both work at once. Right-edge placement comes only from the browser laying out a row under `dir="rtl"`. Under `dir="rtl"` the browser runs UAX #9 and reverses every RTL run. So the letters come out right only if the buffer already holds logical order. Visual order fails in both ways it can be rendered:

| buffer | render | letters | placement | why |
|---|---|---|---|---|
| visual | `dir="rtl"` | wrong | right | browser bidi reverses runs Claude already reversed (double reorder); neutrals such as `?` resolve against the RTL paragraph and land at the wrong end |
| visual | `dir="ltr"` + `unicode-bidi: bidi-override` | right | **left** | the override paints bytes verbatim, which suits pre-reversed bytes, but it needs `dir="ltr"`, and that forbids right alignment |
| **logical** | `dir="rtl"` | right | right | the browser does the one and only reorder, which is what remote panes have always received |

**Evidence:**
- The truth table appears in the commit body (`git show 322655e`) and in the getter's doc comment at `app/src/terminalInstance.ts:686-718`. It was measured from "Yossi's screen" according to that comment and the PROGRESS entry (`PROGRESS.txt:4528-4537`).
- Why `dir` alone cannot stop the reorder (UAX #9 L2 reverses level ≥ 1 runs whichever way the paragraph faces): `app/src/terminalInstance.ts:1491-1500`.
- Claude Code (≥ 2.1.74) writes RTL to the PTY already in visual order, and the browser bidi on top puts a trailing `?` at the start of the line: `app/src/textDirection.ts:404-412`.
- Before the commit, the pairing was `suppress` → every row `"ltr"` (`app/src/textDirection.ts:398`) plus `bidi-override`. The commit removed it for normalised panes: `suppress = bidiOwnedByTui && !normaliseIncomingToLogical` (`app/src/terminalInstance.ts:1480`), and `override = suppress && mode === "auto_per_line"` (`:1512`).
- After the stream is normalised, a local Claude pane takes the remote path: the buffer is logical, `detectRowDirections` decides each row's `dir`, and the browser does the bidi (`app/src/terminalInstance.ts:1475-1479`).
- `settings.rs` predicted this same constraint before the commit existed: "you cannot have correct letters AND right alignment … Getting both requires un-reversing the stream to logical first and then rendering RTL" (`app/src-tauri/src/settings.rs:510-515`).

**Confidence:** high for the mechanism, because the code, the commit, PROGRESS and the settings comment all agree. The table's rows were observed by Yossi. This session reproduced none of them.

### Q2
**Answer:** The lag was **reproduced live, but not measured.** On 2026-08-20 Yossi ran three builds on real panes. Only the build containing 322655e lagged, and switching `rtl_mode` back to `off` removed the lag. No timing figure exists: no before/after milliseconds and no profile. "The cost is the `visualToLogicalStream` pass" is an **inference** from the code. This research found two other costs the same commit switches on, so that attribution is incomplete.

**Evidence (what is observed):**
- FOLLOWUPS.md, the 322655e P1 entry (`FOLLOWUPS.md:76` at HEAD 23f2870) records: "(1) merged 0.5.0 WITH 322655e — … noticeably better … and typing lagged; (2) main-only 7382de9 — no lag …; (3) branch build `ymux-rtl-test` … predates 322655e — no lag." The current workaround is `terminal.rtl.local.rtl_mode = "off"`, with no lag and worse display.
- The commit itself says "Not run live" (`git show 322655e`), and its PROGRESS entry lists the caret and box-drawing checks as NOT VERIFIED (`PROGRESS.txt:4553-4555`). So the lag was found after the commit, not by it.
- On defaults: local is `auto_per_line` with `tui_owns_bidi: true` (`app/src-tauri/src/settings.rs:519-533`, `:673-675`). That makes `normaliseIncomingToLogical` true for every local pane once Claude is detected in front (`app/src/terminalInstance.ts:682-684`, `:716-718`).

**Evidence (what weakens the "spread thinly, not blocking" claim):**
- The stall detectors fire only on a heartbeat gap above **300 ms** or a `longtask` above **200 ms** (`app/src/App.tsx:3782-3784`). Typing lag that a person notices starts around 50–100 ms per keystroke. So "zero `UI stall` warnings and one `longtask`" only shows that no single task reached 200 ms. It does not show that each flush was cheap. These instruments could not see the lag that was reported.

**Evidence (two other costs the same commit turns on, not named in FOLLOWUPS):**
1. **The row-direction pass stopped short-circuiting.** Before the commit, `suppress=true` meant `rowDirections` returned `"ltr"` for every row without looking at them (`app/src/textDirection.ts:398`). After it, every frame in which any row's text changed runs the full block-aware `detectRowDirections` (`app/src/textDirection.ts:221-235`: `stripPaneFrame`, `classifyRow` per row, two regex tests per row). That pass is triggered by a `MutationObserver` on the xterm rows, coalesced to one per rAF (`app/src/terminalInstance.ts:1378`, `:1424-1430`). Rows then get `dir="rtl"`, which makes the browser do real bidi layout instead of painting verbatim.
2. **The per-frame log + IPC.** At 322655e, `logDirections` logged whenever the direction vector changed (`git show 322655e:app/src/terminalInstance.ts`, lines 1420-1423). Every log line was one `invoke("ui_log")` IPC call (`git show 322655e:app/src/logger.ts`, line 60). Under the old `suppress` path the vector was all-`ltr` and stayed constant, so this was silent. Once rows resolve to mixed `rtl`/`ltr`, Hebrew output changes the vector frame after frame. `faa0a4c` (2026-09-23) later named this explicitly ("title-seen / rtl-dirs no longer log per frame"), batched the logger to one `ui_log_batch` invoke per second, and rate-limited `rtl-dirs` to one line per 2 s (`app/src/terminalInstance.ts:1561-1569`). **Yossi's lag observation (2026-08-20) predates that fix.** Nobody has re-tested the lag on a build containing `faa0a4c`, according to every record searched (FOLLOWUPS, PROGRESS, DECISIONS).

**Confidence:** high that the lag was reproduced (first-hand record). Low that `visualToLogicalStream` is the main cost. This research could not measure how the three costs split.

### Q3
**Answer:** The early-out keeps an **all-Latin chunk** cheap. It does nothing for the pane this trade-off is about: a Claude pane with Hebrew on screen. The work happens per rAF flush, at two levels:

1. **Per flush:** `writeData` queues chunks and schedules one `requestAnimationFrame` (`app/src/terminalInstance.ts:1897-1906`). `flushPending` joins them (`:1911`) and calls `visualToLogicalStream(merged)` (`:1945`).
2. **Chunk gate:** `if (!chunk || !RTL_RE.test(chunk)) return chunk;` (`app/src/copyBidi.ts:174`) is one regex scan with no escape walk and no allocation. An all-Latin flush pays only this.
3. **Escape walk:** otherwise `chunk.matchAll(ANSI_RE)` goes over the whole merged string (`app/src/copyBidi.ts:177-183`). `ANSI_RE` covers ESC-Fe, CSI and OSC (`app/src/bidi.ts:9`). Every escape is pushed through unchanged, and every text span between escapes is passed to `unreorderTextSegment`.
4. **Segment gate:** `unreorderTextSegment` runs `RTL_RE` again per segment (`app/src/copyBidi.ts:150`), and `unreorderLine` runs it per line (`:80`). So Latin spans are cheap even inside a Hebrew chunk.
5. **Paid path:** only a line containing Hebrew reaches bidi-js: `strongCounts` majority vote, `getEmbeddingLevels`, `getReorderSegments`, `getMirroredCharactersMap`, `split("")`, a reverse per flip and `repairSurrogates` (`app/src/copyBidi.ts:79-101`).

Why the common case is cheap: the gate is per **flush**, not per pane. A pane with no Hebrew in the bytes of a flush pays one regex test. Why it does not help the case that lags: Claude's TUI redraws its input box and surrounding lines while you type. If Hebrew is anywhere in the redrawn bytes, every keystroke's echo flush goes down the full path. The FOLLOWUPS idea "cache the RTL verdict per pane instead of re-scanning every chunk" would remove only step 2's scan. That is the cheapest step, so it would likely not help a Hebrew pane. That is an inference; it has not been measured.

**Profile data:** none exists. `copyBidi.test.ts` has 16 test cases and none is a benchmark (`grep -n "perf\|bench\|performance" app/src/copyBidi.test.ts` → no output).

**Confidence:** high for the code walk. Low for any claim about relative cost, since there is no profile.

### Q4
**Answer:** Rejected alternatives, from the records:

| alternative | status | reason | proven or stated |
|---|---|---|---|
| **Compose** `reorderRtlForDisplay(visualToLogical(x))` in `flushPending` (the plan approved first) | rejected before coding | `reorderRtlForDisplay` moves runs in place and adds no padding, so it can never move a line to the right edge; `reorder(unreorder(v)) === v` on every sample | **probed**, not unit-tested: PROGRESS records a probe against four simulated Claude lines with `right-placed=false` (`PROGRESS.txt:4494-4504`); the claim is repeated at `app/src/terminalInstance.ts:699-703`. No committed test pins it |
| Use the clipboard `visualToLogical` on the stream | rejected | it ignores escapes by design, so it would permute CSI/OSC bytes into garbage | stated (`app/src/copyBidi.ts:161-165`) and **tested**: the new stream tests check escapes and OSC byte-for-byte (`PROGRESS.txt:4547-4550`) |
| Keep `dir="ltr"` + `bidi-override` (pre-commit) | superseded | letters right, placement left (Q1 row 2) | observed by Yossi |
| `rtl_mode="off"` as the profile default | tried and reverted earlier | fixes Claude and breaks a logical shell (PowerShell Hebrew renders reversed) | stated (`app/src-tauri/src/settings.rs:529-531`, `PROGRESS.txt:4043-4051`) |
| Normalise under `bidi_reorder` / `off` | excluded | WebGL renderer, no `dir`, so there is no placement to gain | stated (`app/src/terminalInstance.ts:705-707`) |
| Normalise under `force_rtl` (2026-08-23) | excluded | remote streams are already logical, and normalising them would reverse them | stated (`app/src/terminalInstance.ts:709-715`, `docs/vault/frontend-lib.md:140-144`) |
| A new setting/mode for the trade-off | rejected | `rtl_mode="off"` is the documented fallback | stated (commit body; `PROGRESS.txt:4539-4541`) |
| Revert 322655e | warned against | it is the only one of the three rows that gets both letters and placement right | stated (FOLLOWUPS P1 entry) |
| Per-pane RTL-verdict caching; narrowing normalise to panes running Claude; incremental reorder instead of whole chunk | **proposed, not evaluated** | listed under "DIRECTIONS WORTH TRYING, none validated" | stated only (FOLLOWUPS P1 entry). Note that narrowing to Claude panes is **already** how the gate works: `bidiOwnedByTui` requires `foldTuiOwnsBidi` (the hook or title signal that Claude is in front) (`app/src/terminalInstance.ts:682-684`), so a plain shell pane does not pay. That direction may already be done, depending on whether the hook signal clears when Claude exits |

Why `visualToLogicalStream` and not a composed reorder: it is the only operation that changes the **buffer order**. Placement needs logical bytes plus `dir="rtl"`, and any transform that ends in visual bytes, which is what the compose does, gives back the visual buffer. Its base direction is a strong-character **majority vote**, chosen because a count gives the same answer for a line and its reversal, so the inverse is well defined (`app/src/copyBidi.ts:47-72`). Measured on a 12-line corpus, the majority vote recovered 11 lines and the "any RTL" rule recovered 9 (`app/src/copyBidi.ts:59-66`).

**Confidence:** high for the listed rejections (each has a record). Medium on the "no-op" claim: it was a probe at design time and no committed test pins it.

### Q5
**Answer:** These are still open. None was verified live, according to the records searched.

1. **Caret while typing Hebrew.** The commit flags this as the known risk. Mechanism (inferred): Claude edits its input line in *visual* columns and places its cursor with absolute/relative CSI moves computed against its visual layout. After normalisation the buffer holds the same characters in different columns, so the caret xterm draws can sit on the wrong glyph. `isCurrentLineRtl` deliberately still bails on `bidiOwnedByTui` (`app/src/terminalInstance.ts:1664-1671`), so arrows are not mirrored. That is correct for Claude's own model, but nothing reconciles the drawn caret with the moved text. Tracking: commit body and `PROGRESS.txt:4553-4555`. `settings.rs:514-515` calls it "the cursor/partial-repaint problem already logged in FOLLOWUPS".
2. **Partial repaints / fragment base direction.** Each text span between two escapes resolves its own base direction (`app/src/copyBidi.ts:167-171`), and `merged` is a rAF boundary, not a line boundary (`app/src/terminalInstance.ts:1940-1944`). Claude's differential redraw writes a cursor move followed by a few cells. A fragment like that is un-reversed on its own, out of context of the rest of the row, and can land in a different order than a full-line redraw would give.
3. **Box-drawing rows.** Claude's bordered input box contains `│ … │` around Hebrew. `unreorderLine` runs on any line that contains Hebrew, borders included. Whether the borders stay in place depends on the majority vote plus bidi-js treating box-drawing characters as neutrals. Untested; PROGRESS names `770013b` as "the regression to watch" (`PROGRESS.txt:4555`).
4. **Unrecoverable ambiguity.** A number next to a Latin word at the edge of an RTL line has two logical readings with the same visual form, and the code returns the first (`app/src/copyBidi.ts:131-142`). It is proven impossible to fix, and the effect on display is small but real.
5. **Cost attribution.** Which of the three costs dominates (stream un-reorder, full `detectRowDirections` plus browser bidi layout, per-frame log IPC as of 2026-08-20) is unmeasured (see Q2). Whether the lag survives `faa0a4c` is unknown.
6. **Hook-signal lifetime.** If the Claude hook signal (`tuiExplicit`) stays set after Claude exits, a later plain shell in that pane is still normalised. That would both reverse logical shell output and add cost. Not checked in this pass.

## Options
Not requested. The goal asks for documentation, and fixing the latency is out of scope.

## Recommendation
Do **not** revert 322655e. It is the only buffer/render combination that gives correct letters *and* right placement (Q1). Before anyone designs a latency fix, first **measure**, in this order:

1. Re-test the lag on a build that contains `faa0a4c`. That commit removed the per-frame `rtl-dirs` log IPC, which the same commit had started firing on Hebrew panes, so the lag Yossi saw on 2026-08-20 may already be smaller.
2. If it remains, take one DevTools performance profile of typing Hebrew into Claude with `rtl.local.rtl_mode = auto_per_line` and split the time between `visualToLogicalStream` (`app/src/copyBidi.ts:173`), `applyRowDirections`/`detectRowDirections` (`app/src/terminalInstance.ts:1446`) and style/layout of `dir="rtl"` rows.
3. Lower or add a finer threshold to the stall instrumentation (`app/src/App.tsx:3782-3784`). At 300/200 ms it cannot see keystroke-scale lag.

What this accepts: until the profile exists, the local default stays at Yossi's chosen trade-off (`off`: no lag, worse display). What would change this recommendation: a profile that shows `visualToLogicalStream` dominating. In that case the incremental or line-cached reorder from FOLLOWUPS becomes the next step.

## Open questions
- Q2 partially open: no timing measurement exists, and the split between the three costs is unknown. To answer it: a DevTools performance profile on Windows with a Claude pane and Hebrew input, before and after `faa0a4c`.
- Q3 profile data: none. To answer it: the same profile, or a node micro-benchmark of `visualToLogicalStream` against recorded (redacted) Claude repaint chunks.
- Q5 items 1, 2, 3 and 6: these need a live Windows pane (out of scope here).
- Goal "Done when" asks for FOLLOWUPS.md:67 to be edited. That line now sits at `FOLLOWUPS.md:71`, and it already carries the `→ intake-ymux-322655e-app-src-terminalinstance` marker but is still `[~]`. That edit is outside the researcher's read-only scope and was **not done**. It needs the orchestrator or a human.

## Sources
- `git show 322655e` — commit body: the three-row matrix, the rejected compose design, "Not run live"
- `app/src/terminalInstance.ts:682-684` — `bidiOwnedByTui`
- `app/src/terminalInstance.ts:686-718` — `normaliseIncomingToLogical`, truth table, mode exclusions
- `app/src/terminalInstance.ts:742-753` — `bufferIsVisualOrder` subtracts the normalised case first
- `app/src/terminalInstance.ts:1378`, `:1424-1430` — MutationObserver → per-rAF direction pass
- `app/src/terminalInstance.ts:1475-1481`, `:1491-1512` — `suppress` / `override` gating, UAX #9 L2 note
- `app/src/terminalInstance.ts:1561-1569` — `rtl-dirs` log rate limit (post-faa0a4c)
- `app/src/terminalInstance.ts:1659-1671` — `isCurrentLineRtl` still bails on `bidiOwnedByTui`
- `app/src/terminalInstance.ts:1897-1945` — `writeData` rAF coalescing, `flushPending` branch
- `app/src/copyBidi.ts:47-101`, `:131-185` — majority base, `unreorderLine`, ambiguity, `visualToLogicalStream`
- `app/src/bidi.ts:9` — exported `ANSI_RE`
- `app/src/textDirection.ts:221-235`, `:386-399`, `:404-412` — `detectRowDirections`, `rowDirections` short-circuit, Claude visual-order note
- `app/src-tauri/src/settings.rs:505-533`, `:673-675` — local profile defaults, predicted trade-off
- `app/src/App.tsx:3775-3814` — UI-stall heartbeat (300 ms) and longtask (200 ms) thresholds
- `docs/vault/frontend-lib.md:140-150` — `force_rtl` traps around `normaliseIncomingToLogical`
- `FOLLOWUPS.md:71-76` — live evidence of the three builds, current workaround, unvalidated directions
- `PROGRESS.txt:4043-4051`, `:4486-4555` — earlier `off` default, compose-design probe, NOT VERIFIED list
- `git show 322655e:app/src/logger.ts` → line 60: one `invoke("ui_log")` per log line at commit time
- `git show 322655e:app/src/terminalInstance.ts` → lines 1420-1423: `logDirections` fired on every vector change
- `git show faa0a4c` → 2026-09-23: logger batched, "title-seen / rtl-dirs no longer log per frame"
- `grep -n "perf\|bench\|performance" app/src/copyBidi.test.ts` → no output (no benchmark exists)
- `grep -n "typing lag\|lagged\|latency" PROGRESS.txt FOLLOWUPS.md docs/DECISIONS.md …` → only the FOLLOWUPS P1 entry records the lag; no re-test found
- Web: none used. The repo answered every question it could.
