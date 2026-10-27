# ymux-2026-08-23-manual-3 — research report

## Question
The BACKLOG P3 entry (BACKLOG.md:24) asks to collapse the private byte-size formatters in the frontend onto `insightsFmt.fmtBytes` and rename `app/src/insightsFmt.ts` to something less Monitor-specific. This report establishes how the formatters differ, who calls them, what the rename touches, which vault pages are gated, and whether the change fits one commit. No code was changed.

### Q1 — Where does each private formatter (`fmtSize` in FileEditor and FileManagerPane, `formatBytes` in SettingsModal) differ from `fmtBytes` in rounding and unit table
### Q2 — Which call sites use each of the four formatters, and would switching them change any rendered size string
### Q3 — What does the Monitor-specific naming of `app/src/insightsFmt.ts` affect (importers, module name, related tests)
### Q4 — Which vault pages cover the four touched files and would need updating under the vault gate
### Q5 — Is the change confined to the four named files plus the module rename, so it fits one commit

## Findings

Preliminary correction: the BACKLOG entry's line numbers have drifted. `FileManagerPane.tsx`'s `fmtSize` is now at line 465 (entry says 448) and `SettingsModal.tsx`'s `formatBytes` at line 80 (entry says 74); `FileEditor.tsx:346` is still correct. The entry's title says "FIVE byte-formatters" but names only four; the fifth is a second exported `fmtBytes` in `app/src/transferStore.ts:171`. It is included below because it is the one most likely to be missed.

### Q1

**Answer.** Every private formatter uses 1024-based arithmetic, like `fmtBytes`, but each has a different unit table, a different decimal rule, and different handling of zero. `transferStore.fmtBytes` is the closest: it matches the shared one everywhere except at 0 and on negatives.

| formatter | location | units | decimals | 0 | caps at |
|---|---|---|---|---|---|
| `fmtBytes` (shared) | `app/src/insightsFmt.ts:6-17` | `B KB MB GB TB`, with a space | one decimal when value < 10 and unit > B, otherwise integral (`toFixed(0)`) | `"0"` (no unit, `:7`) | TB; also handles negatives with a `-` sign (`:8`, `:16`) |
| `fmtSize` (editor) | `app/src/FileEditor.tsx:346-350` | `B KB MB`, with a space | always one decimal above B | `"0 B"` | MB, so 1 GiB → `1024.0 MB` |
| `fmtSize` (file manager) | `app/src/FileManagerPane.tsx:465-470` | `B K M G`, no space, single letter | always one decimal above B | `"0B"` | G, so 2 TiB → `2048.0G` |
| `formatBytes` (settings) | `app/src/SettingsModal.tsx:80-84` | `KB MB` only | below 1 MiB: integral KB via `Math.round`, minimum `1 KB`; from 1 MiB: always one decimal MB | `"1 KB"` (because of `Math.max(1, …)`) | MB, so 1 GiB → `1024.0 MB`; never shows bytes |
| `fmtBytes` (transfers) | `app/src/transferStore.ts:171-182` | `B` then `KB MB GB TB` | one decimal when < 10, otherwise `Math.round` | `"0 B"` | TB |

**Evidence (measured).** The five bodies were transcribed into a scratch script (`/tmp/fmtcmp/c.mjs`, outside the repo) and run with `node`:

```
n | insightsFmt.fmtBytes | FileEditor.fmtSize | FileManagerPane.fmtSize | SettingsModal.formatBytes | transferStore.fmtBytes
0 | 0 | 0 B | 0B | 1 KB | 0 B
500 | 500 B | 500 B | 500B | 1 KB | 500 B
1536 | 1.5 KB | 1.5 KB | 1.5K | 2 KB | 1.5 KB
10240 | 10 KB | 10.0 KB | 10.0K | 10 KB | 10 KB
1048575 | 1024 KB | 1024.0 KB | 1024.0K | 1024 KB | 1024 KB
5662310 | 5.4 MB | 5.4 MB | 5.4M | 5.4 MB | 5.4 MB
52428800 | 50 MB | 50.0 MB | 50.0M | 50.0 MB | 50 MB
1073741824 | 1.0 GB | 1024.0 MB | 1.0G | 1024.0 MB | 1.0 GB
2199023255552 | 2.0 TB | 2097152.0 MB | 2048.0G | 2097152.0 MB | 2.0 TB
```

Two details are shared by all five. First, none rolls over at the rounding boundary: 1 048 575 B prints as `1024 KB` (or `1024.0 KB`/`1024.0K`) everywhere. Second, the shared `fmtBytes` uses `toFixed(0)` where `transferStore` uses `Math.round`. These agree for the non-negative values that reach them, apart from the documented binary-float edge cases of `toFixed`, which were not exercised here.

**Confidence:** high. The bodies are short and were read in full. The table is a transcription, not an import of the `.ts` files: this host's node v20 cannot strip types (vault/moc.md "Test facts"). Re-running the comparison against the real modules on node ≥ 22.6 would make it exact.

### Q2

**Answer.** Every private formatter is used only in its own file. Switching any of them to `fmtBytes` changes at least one rendered string.

- **FileEditor `fmtSize`** has two call sites. `FileEditor.tsx:507` passes it as the `{size}` of `editor.large_file_warning` (`app/src/i18n/en.json:1046`, plus he/ar/ru at `:1055`); that warning shows only above `LARGE_FILE_THRESHOLD` = 1 MiB (`app/src-tauri/src/file_manager.rs:1113`, checked at `FileEditor.tsx:99-100`), so its value is always in the MB/GB range. `FileEditor.tsx:528` renders the status line `encoding · size`. After the switch: values from 10 KB up lose the `.0` (`10.0 KB` → `10 KB`, `50.0 MB` → `50 MB`); ≥ 1 GiB moves from `1024.0 MB` to `1.0 GB`; and an **empty file shows `0` instead of `0 B`**. The `"0 B"` fallback at `:528` only covers `meta()` being absent, not size 0.
- **FileManagerPane `fmtSize`** has three call sites. `:605` is the upload status `uploaded <name> (<size>) ✓`. `:1377` and `:1426` are the size column of the two listing panes (`.fm-size`). **Every non-zero string changes**: `1.5K` → `1.5 KB`, `10.0K` → `10 KB`, `1.0G` → `1.0 GB`. A 0-byte file goes from `0B` to `0`. The column is a fixed **60 px** grid track (`.fm-row { grid-template-columns: 22px 1fr 60px 90px }`, `app/src/App.css:3107-3109`, right-aligned with tabular numerals at `:3136-3140`). The longer form, e.g. `1023 KB` (7 chars versus `1023.0K`'s 7) or `999 MB`, may fit or clip depending on the font. Its actual pixel width was not measured (see Open questions).
- **SettingsModal `formatBytes`** has one call site, `SettingsModal.tsx:189`: the `{size}` of `settings.font.install` (`en.json:241`, "Install ({size})"). It is fed `download_bytes`, the sum of a font's asset sizes (`app/src-tauri/src/fonts.rs:198`, `:254`). After the switch: fonts under 1 MiB show `1.5 KB`/`500 B` where they used to show `2 KB`/`1 KB` (the 1 KB floor disappears), and fonts ≥ 10 MiB lose the `.0`. Values from 1 to 10 MiB are unchanged (`5.4 MB`).
- **transferStore `fmtBytes`** has four call sites: `TransferBar.tsx:62`, `:63` (×2), `:76`, plus `transferStore.ts:186` via `fmtSpeed`. Its output is identical to the shared one except at 0, where `0 B` becomes `0`. That case is reachable: `TransferBar.tsx:62` renders `fmtBytes(tx().done)` before the first progress event. `fmtSpeed` already returns `""` for `bps <= 0` (`transferStore.ts:185`), so speed is unaffected.
- **Shared `fmtBytes`** callers are unchanged by this ticket, because its rules are out of scope: `InsightsWindow.tsx:458,461,468,532,553`; `InsightsAnalytics.tsx:363,364,374,548,551,589`; `insightsReport.ts:178,180,186,192,243-245,260`; and `fmtBps` at `InsightsWindow.tsx:476`, `InsightsAnalytics.tsx:56-57,376-377`, `insightsReport.ts:184,190`.

Evidence for the call-site list: `grep -rn "fmtSize\|formatBytes\|fmtBytes\|insightsFmt" app/src` lists exactly the lines above and nothing else.

**Confidence:** high for the strings; medium for the visual effect in the 60 px column.

### Q3

**Answer.** The name `insightsFmt` reaches four importers, one test indirectly, and one vault page. Nothing outside `app/src/` imports it.

- **Importers.** `InsightsWindow.tsx:20` (`fmtBytes, fmtBps`), `InsightsAnalytics.tsx:6` (all four), `InsightsClaudeCost.tsx:6` (`fmtSpan`), and `insightsReport.ts:12`. The last one imports **with the `.ts` extension** (`"./insightsFmt.ts"`) because node's `--experimental-strip-types` runs it under test. A rename has to keep that extension style in that one file.
- **Module comment.** `insightsFmt.ts:1-3` says "Formatters shared by the Monitor's tabs". That comment would also need rewording.
- **Comment reference.** `insightsReport.ts:124` mentions "insightsFmt's fmtPct" in prose.
- **Tests.** No test imports `insightsFmt` directly (`grep -ln insightsFmt app/src/*.test.ts` → none). `insightsReport.test.ts` exercises it through `buildAnalyticsReport` and pins its output: `128 MB` (`:106`), `+3.2 GB` / `-1.0 GB` (`:124-125`), `477 MB` (`:130`). The rename does not change these strings. They only break if `fmtBytes`'s rules change, which is out of scope. The test glob `src/*.test.ts` (`app/package.json:16`) needs no change.
- **Vault.** `docs/vault/frontend-lib.md:10` (`covers:` entry) and `:546` (prose bullet `insightsFmt.ts (34)`), and the hash key in `docs/vault/.vault-lock.json:151`.
- **No other references.** `knowledge/research/ymux-app-src-insightsanalytics-tsx.md` mentions `fmtBps` but not the module path, and the `vault/` notes contain no reference (`grep insightsFmt vault knowledge` → none).

**Name candidates** (choosing one is out of scope): `formatUnits.ts`, `fmt.ts`, `numFmt.ts`, `sizeFmt.ts`. `sizeFmt` undersells the module, since it also holds `fmtPct` and `fmtSpan`. The repo's naming convention for pure modules is camelCase, e.g. `termMenuCopy.ts`, `cwdShort.ts` (vault/moc.md "Pure-module pattern").

**Confidence:** high.

### Q4

**Answer.** Three vault pages own the touched files. Under the diff check, each must appear in the same diff as its code, and the lock must be re-stamped.

| file | owning page | evidence |
|---|---|---|
| `app/src/insightsFmt.ts` (renamed) | `docs/vault/frontend-lib.md` | `covers:` at `:10`; prose at `:546` |
| `app/src/insightsReport.ts` (import line) | `docs/vault/frontend-lib.md` | `covers:` at `:11` |
| `app/src/FileEditor.tsx`, `FileManagerPane.tsx`, `InsightsWindow.tsx`, `InsightsAnalytics.tsx`, `InsightsClaudeCost.tsx`, and (if the fifth is folded in) `TransferBar.tsx`, `transferStore.ts` | `docs/vault/frontend-panes.md` | `covers:` at `:11`, `:12`, `:18`, `:19`, `:20`, `:34`, `:35` |
| `app/src/SettingsModal.tsx` | `docs/vault/frontend-flows.md` | `covers:` at `:18` |

The gate mechanics come from `scripts/vault-check.mjs`:

- **Diff check.** It fails when a covered file is in `git diff --name-only <base> HEAD` and its owning page is not (`:232-251`). CI runs it with `--diff-base` (`.github/workflows/ci-windows.yml:151`).
- **Untracked covers.** A `covers:` entry naming an untracked path is an error (`:126-127`), so the rename must also edit `frontend-lib.md:10`.
- **Line cites.** Line-cite checking (`:200-213`) is not a risk here. No `[sym@app/src/<these files>:N]` cite exists in `docs/vault/` (`grep` → none).
- **Line-count annotations.** The prose carries line counts that shrink, e.g. `insightsFmt.ts (34)` at `frontend-lib.md:546` and `transferStore.ts (207)` at `frontend-panes.md:178`. These annotations would go stale but are not machine-checked as far as the cite regex shows (`scripts/vault-cites.mjs:18-19`).

**Confidence:** high.

### Q5

**Answer.** No. "Four files" undercounts. A literal reading (three private formatters plus the rename) touches **8 source files and 4 vault files**. Folding in the fifth formatter (`transferStore`) brings it to 10 source files. It is still a mechanical, frontend-only change with no API, schema or Rust edit, so one commit is feasible.

- **Source, minimum.** The rename touches `insightsFmt.ts` → new name, plus importers `InsightsWindow.tsx`, `InsightsAnalytics.tsx`, `InsightsClaudeCost.tsx` and `insightsReport.ts`. The collapse touches `FileEditor.tsx`, `FileManagerPane.tsx` and `SettingsModal.tsx`. That is **8**.
- **Optional fifth.** `transferStore.ts` (delete `fmtBytes`, keep `fmtSpeed` by importing the shared one) and `TransferBar.tsx` (import source). That makes **10**.
- **Vault.** `frontend-lib.md`, `frontend-panes.md`, `frontend-flows.md`, `.vault-lock.json`, per Q4 and repo CLAUDE.md Rules #18/#19.
- **Bookkeeping.** `BACKLOG.md:24` closes, and `PROGRESS.txt` gets an append per CLAUDE.md "Session workflow". Both are bookkeeping and outside the commit's code scope.
- **No i18n change.** The `{size}` placeholders take whatever string is passed in (`en.json:241`, `:1046`).
- **Visible behaviour changes (Q2).** These need the human's acceptance, since the goal excludes "UI changes": empty file `0 B` → `0` in the editor; the FM size column changes from `1.5K` to `1.5 KB` in a 60 px track; the font-install button can show `500 B` instead of `1 KB`. A zero-size special case is the one place a caller might want `0 B`. Changing `fmtBytes(0)` itself is out of scope, so either a caller-side fallback or acceptance of `0` is a decision for the planner/human.

**Confidence:** high for the file list; medium on whether the human counts the visible changes as acceptable "mechanical" fallout.

## Options

1. **Literal scope.** Collapse the three named formatters and rename the module: 8 source + 4 vault files. This leaves `transferStore.fmtBytes` as a second public `fmtBytes` with the same name and nearly identical output. The risk is low, but the duplication the request complains about remains.
2. **Full scope.** As option 1, plus fold in `transferStore.fmtBytes`: 10 source + 4 vault files. This reaches the "one helper" the title asks for. The only extra visible change is the transfer bar's `0 B` → `0` before the first progress event (`TransferBar.tsx:62`).
3. **Split.** Option 1 or 2, but with the rename in its own commit first. The rename-only commit is behaviour-neutral (pure import-path edits plus a vault restamp), so the formatter commit's diff shows only the semantic changes. That costs one extra commit, against the request's "wants its own commit", which arguably refers to not riding along with a feature rather than literally one commit.

## Recommendation

Take **option 2 (full scope) in one commit**. The ticket's own title counts five formatters, and leaving a second exported `fmtBytes` in `transferStore.ts:171` defeats the point. The accepted trade-off is a handful of visible string changes, most notably the File Manager size column going from `1.5K` to `1.5 KB` inside a fixed 60 px column (`App.css:3109`), plus `0` instead of `0 B` for empty files. The plan should list these as expected output changes rather than treat them as regressions, and should state the vault pages (`frontend-lib.md`, `frontend-panes.md`, `frontend-flows.md` + lock) up front, since the diff check fails otherwise.

Two findings would change this. If a live check shows the longer strings clip in the 60 px column, the column width (CSS) joins the change, which edges into "UI change" and should go to the human. If the human wants `0 B` kept, that needs a caller-side guard, since `fmtBytes` itself is out of scope.

## Open questions

- **60 px column fit.** Does `fmtBytes` output fit the File Manager's 60 px `.fm-size` track at the app's font size? This was not measured; it needs a live render or a browser-measured text width of e.g. `1023 KB`, `999 MB`, `1.4 GB`.
- **Intended output.** Is `fmtBytes`'s output the intended canonical form, given the request does not say (goal.md Signals)? That is a human decision. This report only shows that `fmtBytes` and `transferStore.fmtBytes` already agree almost everywhere.
- **Exact equality on real modules.** Byte-exact equality against the real `.ts` modules (not transcriptions) needs node ≥ 22.6, which is unavailable on this host.

## Sources

- `app/src/insightsFmt.ts:1-17` — module comment and shared `fmtBytes` rules
- `app/src/FileEditor.tsx:346-350`, `:99-100`, `:507`, `:528` — editor `fmtSize` and its call sites
- `app/src/FileManagerPane.tsx:465-470`, `:605`, `:1377`, `:1426` — FM `fmtSize` and its call sites
- `app/src/SettingsModal.tsx:80-84`, `:189` — `formatBytes` and its font-install call site
- `app/src/transferStore.ts:171-187` — fifth formatter and `fmtSpeed`
- `app/src/TransferBar.tsx:13-22`, `:62-63`, `:76` — transferStore `fmtBytes` callers
- `app/src/InsightsWindow.tsx:20`, `app/src/InsightsAnalytics.tsx:6`, `app/src/InsightsClaudeCost.tsx:6`, `app/src/insightsReport.ts:12`, `:124` — `insightsFmt` importers and comment reference
- `app/src/insightsReport.test.ts:106`, `:124-125`, `:130` — indirect pins on `fmtBytes` output
- `app/src/App.css:3107-3109`, `:3136-3140` — 60 px size column
- `app/src/i18n/en.json:241`, `:1046` — `{size}` placeholders
- `app/src-tauri/src/file_manager.rs:1113`; `app/src-tauri/src/fonts.rs:198`, `:254` — inputs feeding the editor warning and the font size
- `docs/vault/frontend-lib.md:10-11`, `:546`; `docs/vault/frontend-panes.md:11-35`, `:178`; `docs/vault/frontend-flows.md:18`; `docs/vault/.vault-lock.json:151` — vault ownership
- `scripts/vault-check.mjs:126-127`, `:232-251`; `.github/workflows/ci-windows.yml:151` — gate mechanics
- `BACKLOG.md:24` — the original request (stale line numbers)
- `grep -rn "fmtSize\|formatBytes\|fmtBytes\|insightsFmt" app/src` → the complete call-site list in Q2
- `node /tmp/fmtcmp/c.mjs` → the comparison table in Q1
- `git log --oneline -S"formatBytes" -- app/src` → `e646383a feat(fonts): install missing fonts per-user`; `git log -- app/src/insightsFmt.ts` → `a311a95f Phase 84.C`; `-S fmtBytes -- transferStore.ts` → `3ae896f9 Phase 81.B` — origin of each formatter
