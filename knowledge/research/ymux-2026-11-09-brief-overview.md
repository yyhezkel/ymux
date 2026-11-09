<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: ymux-manual-p2-brief-sessions, ymux-2026-09-02-manual, ymux-2026-09-02-manual-2 -->

**Verdict: show one status per row, taken from the Queue's own function. Prefer the brief over the LLM, keep the LLM out of the Queue, and skip the refresh button and persisted selection.** Nothing was run. LLM accuracy and overview usage are unmeasured.

Paths are under `/home/runner/winmuxMac/winmux/app/`. `OV.rs` = `src-tauri/src/sessions_overview.rs`, `OV.tsx` = `src/SessionsOverviewWindow.tsx`, `QM` = `src/queueModel.ts`.

## 1. Vocabularies
**Facts:**
- `queueStatus` is per pane, driven by live hooks and briefs (`QM:40-53`); every other surface uses it (`src/App.tsx:1273-1276`).
- The LLM verdict is per session name, pulled by hand, and held locally (`OV.tsx:16-18,74`).
- The capture reads only the session's current window (`OV.rs:202`).
- Tests cover parsing only (`OV.rs:569-679`).
- `idle` has no Queue equivalent: `done` means an agent finished.

**Options:**
- Shared enum: forces false equivalences.
- LLM as Queue fallback: an unexpiring guess lights the sidebar and badge (`App.tsx:1357-1369`), against "a wrong light is worse than no light" (`src-tauri/src/rpc_server.rs`, `resolved_pane` comment).
- **Precedence per row (recommended).**

**Recommendation:** add a pure `overviewVerdict()` to `QM`. A row with an `inQueue` pane shows `queueStatus` and `whatsHappening`. Other rows show the LLM pill, labelled "read from screen". No mapping table is needed, because a row never carries two verdicts. Hooks are installed on provisioned remotes and local Claude (`docs/vault/backend-remote.md:62,129`), so hookless panes are mostly plain shells.

**Invalidated if:** hookless agents become common and an accuracy measurement exists.

## 2. Prefer the brief
**Facts:**
- The cost is one `claude -p` per chunk of up to 10 ticked rows, not per row (`OV.tsx:29,97`).
- `pane_holding_session` is already host-safe (`OV.rs:487`); `panePersistence` is name-only.
- A session never attached has no brief, because the hook resolves to no pane (`src-tauri/src/lib.rs:2047-2082`).

**Recommendation:**
- **Mapping:** stamp `pane_ids` on `TmuxSessionInfo` from `pane_holding_session`, changed to return all matches. Take the most urgent.
- **Fresh:** event-based, no age cap. A row counts when the pane is connected and in the Queue, and no prompt is newer than the brief unless the hook says running. A degraded brief keeps the free status, but its summary stays tickable.
- **Pane-backed rows:** render on open, free, with the checkbox disabled.
- **No pane:** tick for LLM. Optionally show the last `context_store` entry dimmed (`src-tauri/src/context_store.rs:53`, via `claude_session_id`), text only.
- **Refresh button: no.** The pipeline needs a session name (`OV.rs:415`), so plain panes get nothing, and fresh briefs don't need it.

**Files and size for 1+2:** `lib.rs`, `OV.rs`, `OV.tsx`, `App.tsx:5478`, `QM` plus test, `types.ts`, i18n, vault pages. About 120 lines.

**Invalidated if:** logs show most ticks land on paneless sessions, or degraded briefs dominate. Known risk: `find_pane_by_tmux_session` matches by name across hosts (`lib.rs:2089`), so a stale pane id can misfile a brief.

## 3. Persisted selection
**Facts:** the default is nothing selected and nothing auto-runs (`OV.tsx:192-195`), so "muted" means nothing today. After section 2, ticks apply only to paneless sessions.

**Options:**
- Host-keyed JSON file: about 70 lines of Rust, two commands, rename and kill hooks, a vault page.
- Field in `session-owners.json`: wrong, since unclaimed sessions have no entry and a kill releases the claim.
- localStorage per workspace id: about 25 lines; the workspace already implies the host.

**Recommendation:** don't build; close the entry. If wanted anyway, use localStorage:
- **Rename:** handle it in `commitRename` (`OV.tsx:289`). External renames lose a tick, which is harmless.
- **Dead sessions:** intersect with the live list on open, but never prune on an empty list, since empty can mean "could not ask" (`OV.tsx:178-183`).

**Invalidated if:** auto-summarize ships. Then a muted set matters, and it belongs in a host-keyed file.

## Is the overview earning its cost?
Yes for the list. The Queue covers only layout panes with agent signal (`App.tsx:1289-1296`), while the overview inventories every session with open, rename and kill. Only the LLM column overlaps. Count `summarize:` log lines (`OV.rs:471`) before investing further in it.

## Versus prior research
The conclusion differs on three points:
- **Queue fallback:** `ymux-manual-p2-brief-sessions.md` recommends it. I reject it: it needs a timestamped store and a wider `inQueue` to surface an unmeasured, manually refreshed guess.
- **Refresh button:** `ymux-2026-09-02-manual-2.md` recommends building it. I don't.
- **Selection file:** `ymux-2026-09-02-manual.md` recommends it. I reject it because nothing is muted today.

We agree on showing the Queue's verdict on hook-backed rows, the backend join, and the no-newer-prompt freshness rule.

They caught two things I missed, and I adopt both:
- The Hebrew `sessions.status.idle` reads "ממתין" (waiting), against `queue.status.done` (`src/i18n/he.json:1227,1349`). Fix the label.
- The web path has a Go mirror (`src-tauri/server/internal/term/claudetools.go`). Without a mirror, browser rows simply fall back to the LLM.
