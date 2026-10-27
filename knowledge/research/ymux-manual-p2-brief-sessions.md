# ymux-manual-p2-brief-sessions — research report

## Question

ymux ships two vocabularies for "what is this agent session doing". The first is the Phase 90 active-sessions overview (PR #41), an LLM's read of a captured screen: `idle | working | waiting_input | error`. The second is the BRIEF/Queue family: the hook-driven `PaneAgentState` combined with the agent-written `[ymux-brief]` status, folded by `app/src/queueModel.ts` into `QueueStatus`. This report finds where each value is produced and consumed, which surface reads which source, where the surfaces disagree about the same session, and what single mapping reconciles them. It also covers whether the overview verdict can serve as a fallback for panes with no hooks without disturbing hook precedence. Code changes are out of scope; this report is the deliverable.

### Q1 — Which code emits each LLM screen-read verdict value (`idle`, `working`, `waiting_input`, `error`) and where is it consumed
### Q2 — Which code emits each BRIEF state (`working`, `waiting-for-you`, `stuck`, `done`, `ended`) and where `app/src/queueModel.ts` reads them
### Q3 — Which surfaces (Queue, Briefing card, Active-sessions table) derive their status from which source today, and where do they disagree for the same session
### Q4 — What single value-to-value mapping reconciles the two vocabularies, with each non-obvious case (e.g. `idle` vs `done`, `error` vs `stuck`, `waiting_input` vs `waiting-for-you`) justified
### Q5 — Can the overview's LLM verdict feed `queueModel` as a fallback for panes with no hooks without changing hook-driven precedence, and what must change to allow it

## Findings

### Q1

**Answer.** No code path assigns these four values deterministically. A model (`claude -p`) emits them in reply to a fixed prompt, and two parsers (Rust desktop, Go daemon) normalise its reply. Only one place consumes them: the status pill in `SessionsOverviewWindow.tsx`. They are not passed to any other surface.

**Producers (prompt + normaliser):**
- Desktop (Tauri) path: `app/src-tauri/src/sessions_overview.rs:53` `KNOWN_STATUSES = ["idle","working","waiting_input","error"]`. The prompt at `sessions_overview.rs:96-105` defines each value. `waiting_input` = "a prompt, question or permission request is waiting for the human". `working` = "a command or an agent is still running". `error` = "the last thing on screen is a failure". `idle` = "a shell prompt with nothing running".
- `normalize_status` (`sessions_overview.rs:238-245`) lowercases the value. Anything outside `KNOWN_STATUSES` becomes `unknown`, a fifth value the model never sends.
- `parse_summary_envelope` (`sessions_overview.rs:251-280`) gives every requested name a row and defaults it to `unknown` (`:260`). One row per tmux/zellij **session name**, never per pane (`SessionSummary { name, status, summary }`, `:58-62`).
- Tauri command `sessions_overview_summarize` (`sessions_overview.rs:409-475`). Over SSH it runs a single remote pipeline; on local machines it captures zellij (Windows) or tmux (mac) sessions and runs the local `claude`.
- Web/daemon path: `app/src-tauri/server/internal/term/claudetools.go:376-384` repeats the same prompt word-for-word. `parseOverview` (`claudetools.go:438-490`, switch at `:477`) applies the same normalisation. The browser backend sends `sessions_overview_summarize` to `POST /api/v2/term/sessions/summarize` (`app/src/backend/web.ts:922-926`).
- TS mirror: `app/src/types.ts:166-173`, `status: "idle" | "working" | "waiting_input" | "error" | "unknown"`.

**Consumer (only one):**
- `app/src/SessionsOverviewWindow.tsx:74` keeps a component-local `summaries` signal keyed by session name. `summarize()` (`:89-125`) fills it, and only when the user ticks rows and presses **Summarize selected** (`:153-158`; 90.C decision "summaries are pulled, not pushed", `docs/DECISIONS.md:805-807`). Every `load()` and every close clears it (`:166`, `:203`).
- `statusOf` (`:310-311`) → pill `<span class="sessions-status ${st}">{t("sessions.status.${st}")}` (`:488-489`). Styling at `app/src/App.css:7323`. Labels at `app/src/i18n/en.json:1349-1353`.
- `App.tsx:5478-5500` passes the window only a workspace id and callbacks. No `QueueRow` data goes in, and no summary data comes back out.

Unrelated value spaces: `server/internal/chat/chat_session.go:29` `waiting_input` belongs to the Phase 69 chat status set, and `"idle"` in `app/src/types.ts:101` is a sidebar `CardStatusKind`.

**Confidence:** high. Every producer and consumer was found with `git grep -n waiting_input` over `app/src app/src-tauri/src app/src-tauri/server` (main checkout; `.worktrees/` copies ignored).

### Q2

**Answer.** BRIEF states come from three separate signals. (a) The agent-written `status:` line, parsed desktop-side into `BriefStatus` (`working | waiting-for-you | stuck | done`). (b) The hook-driven `PaneAgentState` (`unknown | running | done | needs-input | failed`). (c) The `session_ended` flag set by the SessionEnd hook. **`ended` is not a wire value.** It exists only as a `QueueStatus` derived from (c). `queueModel.ts` merges all three into `QueueStatus` = `needs-input | stuck | waiting | working | done | ended`.

**Brief status (agent-written):**
- `app/src-tauri/src/brief.rs:63-68` enum `BriefStatus { Working, WaitingForYou, Stuck, Done }`, kebab-case on the wire.
- Aliases at `brief.rs:72-80`: `waiting` / `waiting for you` → `WaitingForYou`; `blocked` → `Stuck`.
- A missing `status:` → `Done` (`brief.rs:249`). No marker at all → degraded brief, also `Done` (`brief.rs:259-262`). As a result, **an agent that never writes a brief always reads `done`**.
- The Stop hook stores the brief on the pane and clears `session_ended` (`rpc_server.rs:1565-1572`). UserPromptSubmit stores `last_prompt` and also clears `session_ended` (`rpc_server.rs:1485-1497`).
- SessionEnd sets `session_ended = true`, keeps the brief, and resets the agent run to `Unknown` (`rpc_server.rs:1590-1622`).

**Hook state (desktop transition table):**
- `app/src-tauri/src/lib.rs:263-284` enum `PaneAgentState`. `Unknown` = "No hook has ever arrived for this pane" (`:264-266`).
- `apply_hook` at `lib.rs:380-398`:
  - `user-prompt-submit` / `pre-tool-use` → `Running`
  - `stop` → `Done`
  - `stop-failure` → `Failed`
  - notification `permission_prompt | idle_prompt | agent_needs_input` → `NeedsInput` (`lib.rs:304-307`)
  - `elicitation_*` → `Running` (`lib.rs:313`)
- TS mirror at `app/src/paneAgentState.ts:16-22`. It lists an extra `"idle"` that the Rust `as_str` never emits (`lib.rs:290-294`), so that value is dead.

**Where `queueModel.ts` reads them.** `queueStatus()` at `app/src/queueModel.ts:40-53`, in precedence order:
1. `waitingOnPermission || state==="needs-input"` → `needs-input` (`:43`)
2. `state==="failed"` → `stuck` (`:45`)
3. `brief.session_ended` → `ended` (`:46`)
4. `state==="running"` → `working` (`:47`)
5. brief status `stuck` → `stuck`, `waiting-for-you` → `waiting`, `working` → `working` (`:48-51`)
6. everything else → `done` (`:52`). This covers brief `done`, no brief, and `state` `done`/`unknown` with no brief.

Buckets: `QUEUE_BUCKET` (`:58-65`). Queue admission: `inQueue = light!==null || brief!==null` (`:106-108`).

**Confidence:** high (read directly from source; behaviour pinned by `brief.rs` tests `:338-384`).

### Q3

**Answer.** The Queue, the Briefing card, the Context Rail, the sidebar cards and the pane lights all derive from **one** function over **one** row source: `allPaneAgentRows()` → `queueStatus()`. They cannot disagree with each other. The Active-sessions table is the odd one out. It reads only the LLM verdict, keyed by session name. It never sees hook state or briefs, and the Queue never sees its verdict. They disagree in the six ways listed below.

**Source per surface:**

| Surface | Source | Key | Evidence |
|---|---|---|---|
| Queue panel | `queueStatus(allPaneAgentRows())` | pane id | `QueuePanel.tsx:16-17,57`; `App.tsx:5440` |
| Briefing card | same rows, filtered by workspace, painted by `QueueRowView` | pane id | `App.tsx:5515`; `BriefingCard.tsx:18,40`; `QueueRow.tsx:11-14,44` |
| Context Rail | same rows | pane id | `App.tsx:5228`; `ContextRail.tsx:68` |
| Sidebar card / attention | same rows + `queueStatus` | pane → workspace | `App.tsx:2974-3030`, `:1359-1368` |
| Active-sessions table | LLM `SessionSummary.status`, on demand | tmux/zellij session name | `SessionsOverviewWindow.tsx:74,310-311,488-489` |

`allPaneAgentRows` (`App.tsx:1281-1321`) is documented as "the single source; the Queue panel, the sidebar attention set and the active-workspace lights … cannot disagree" (`App.tsx:1272-1275`).

**Where they disagree for the same session** (one tmux/zellij session bound to a workspace via `Workspace.tmux_session`, see `App.tsx:2980-2989`):

1. **Finished turn, no brief.** A Stop with no `[ymux-brief]` gives Queue `done` (💤), because degraded → `Done` (`brief.rs:259-262`) and `queueModel.ts:52`. Claude Code's empty input box on screen could be read by the model as `idle` ("shell prompt") or `waiting_input` ("prompt … waiting for the human", `sessions_overview.rs:101`). Both are plausible readings of the prompt text, and neither matches `done`. Which one the model actually picks has not been measured (Open questions).
2. **Agent asked a question in prose.** If the brief says `done`, or there is no brief, the Queue shows `done` while the screen shows a question → `waiting_input`. The reverse also happens: brief `waiting-for-you` + screen idle prompt → Queue `waiting` vs LLM `idle` or `waiting_input`.
3. **Error vs stuck.** Queue `stuck` comes from hook `failed` (API error, `queueModel.ts:44-45`) or brief `stuck`. The LLM's `error` means "last thing on screen is a failure" (`sessions_overview.rs:102`). That can be a failed shell command in a session with no agent, or a failed tool call inside a turn that is still running. In that last case the Queue says `working` (hooks win) and the table says `error`.
4. **Ended session.** SessionEnd → Queue `ended` ✅ (`queueModel.ts:46`), while the screen shows the shell prompt the agent left behind → LLM `idle`.
5. **The same label means different things.** `en.json:1227` labels Queue `done` as **"idle"**, and `en.json:1349` labels overview `idle` as **"Idle"**. One word covers "agent finished, your move" and "plain shell, nothing running". Arabic uses the identical word `خامل` for both (`ar.json:1227,1349`). Hebrew labels overview `idle` as **"ממתין"** ("waiting", `he.json:1349`), which reads like the Queue's waiting family. A Hebrew user (the primary user, per CLAUDE.md "Communication") sees "waiting" in the table for a session the Queue calls 💤.
6. **Freshness and coverage.** The table is a manual snapshot (cleared on every load, `SessionsOverviewWindow.tsx:166`). `SessionSummary` has no timestamp (`types.ts:169-173`). The Queue is live on the 250 ms agent clock (`App.tsx:2894`). Panes with no hooks are invisible to the Queue (`inQueue` false: `light` is null for `unknown`, `paneAgentState.ts:62`; `brief` is null), yet they get a status in the table.

**Confidence:** high for the source table and for disagreements 3–6 (structural, from code). Medium for 1–2: the code makes these mismatches possible, but how often they happen depends on what the model answers for Claude Code's idle input box, which nobody has measured.

### Q4

**Answer.** Make **`QueueStatus` the single vocabulary**. It is the richer set (6 values against 4), three surfaces already speak it, and it carries the hook-precedence rules. Express the LLM verdict as a *weaker source* into it. Do not try to merge the two sets symmetrically.

**LLM → QueueStatus** (forward mapping, used by the Q5 fallback and to restate overview pills):

| LLM verdict | QueueStatus | Justification |
|---|---|---|
| `working` | `working` | Same meaning: "a command or an agent is still running" (`sessions_overview.rs:102`) vs live `running` (`queueModel.ts:47`). |
| `waiting_input` | `waiting` (bucket 1), **not** `needs-input` (bucket 0) | `needs-input` is reserved for live, synchronous evidence: our permission card or a `permission_prompt`/`idle_prompt` notification (`queueModel.ts:41-43`, `lib.rs:304-307`). A screen read is an inferred, possibly stale snapshot with no timestamp (`types.ts:169-173`), the same epistemic class as a brief's `waiting-for-you`, which maps to `waiting` (`queueModel.ts:50`). Both labels already render as "waiting for you" (`en.json:1223,1225`), so the user-facing word does not change. Only the sort bucket stays honest. |
| `error` | `stuck` | Precedent: hook `failed` → `stuck` with the reasoning "needs a look, not an answer" (`queueModel.ts:44-45`). The LLM's `error` means "the last thing on screen is a failure" (`sessions_overview.rs:102`), which is also a look, not a question. |
| `idle` | **no agent signal**: the row stays out of the Queue (`inQueue` false). It is not `done`, and not `ended` unless `session_ended` is already set. | The prompt defines `idle` as "a shell prompt with nothing running" (`sessions_overview.rs:103`). That is the absence of an agent, which the hook model calls `Unknown` and paints nothing (`lib.rs:264-266`, `paneAgentState.ts:62`). Queue `done` means "the agent finished its turn — your move" (`lib.rs:269-276`); a plain shell asks nothing of the user. Mapping `idle`→`done` would pull every shell into bucket 2. |
| `unknown` | no signal | Parse failure or missing row (`sessions_overview.rs:251-260`). |

**QueueStatus → overview pill** (reverse mapping, for a session that a hook-backed pane holds):

| QueueStatus | Overview pill | Note |
|---|---|---|
| `needs-input` | `waiting_input` | Permission card / needs-input notification |
| `waiting` | `waiting_input` | |
| `stuck` | `error` | Lossy: a brief `stuck` is not necessarily an on-screen failure |
| `working` | `working` | |
| `done` | *no equivalent* | The overview set has no "turn finished, your move" value. Recommend that the unified pill show `done` (💤) when hooks are present rather than force `idle` or `waiting_input`. |
| `ended` | `idle` | Shell prompt left behind |

**The three non-obvious pairs, stated plainly:**
- **`idle` vs `done`.** These are different states. `done` = the agent exists and finished a turn. `idle` = no agent is running. They only look alike on a screen showing Claude Code's empty input box. There, hooks (if present) decide `done`, and the screen read should not override them.
- **`error` vs `stuck`.** Map `error`→`stuck` (same bucket 0, same "needs a look" meaning). Hook `running` still wins: a failure on screen in the middle of a turn is not stuck.
- **`waiting_input` vs `waiting-for-you`.** These are equivalent in meaning. Both map to `waiting`, never `needs-input`, because neither is live evidence.

Separately from the mapping, the Q3 §5 label collision (`queue.status.done` = "idle"/"רגוע"/"خامل" vs `sessions.status.idle` = "Idle"/"ממתין"/"خامل") must be fixed in the UI half (half 2) so that equal words mean equal states.

**Confidence:** medium-high. The forward mapping follows from the prompt's own definitions and existing `queueModel` precedents. The `done` row of the reverse mapping is a design choice, not a derivation.

### Q5

**Answer.** **Yes.** A fallback can sit **below** every existing branch of `queueStatus()` and fire only when `state === "unknown"` and `brief === null`, i.e. no hook and no brief has ever arrived for the pane (`lib.rs:264-266`). Every hook-driven and brief-driven branch (`queueModel.ts:43-51`) would run first and stay unchanged, so precedence cannot change. It needs six supporting changes, none of which exist today:

1. **Join session name to pane.** Verdicts are keyed by session name (`sessions_overview.rs:58-62`). Queue rows are keyed by pane id (`queueModel.ts:15-27`). Two join paths exist: the frontend's `Workspace.tmux_session` (`App.tsx:2980-2989`, field in `app/src/bindings/Workspace.ts:7`), or the backend's `pane_holding_session` (`sessions_overview.rs:487-506`, which already checks host plus name). `LayoutNode` panes carry no session name (`app/src/bindings/LayoutNode.ts:8`), so the frontend join has to go through the workspace.
2. **Lift the verdict out of the dialog.** Today it lives in a component-local signal that is wiped on load and on close (`SessionsOverviewWindow.tsx:74,166,203`). It needs an App-level store keyed by (host, session) that `allPaneAgentRows` (`App.tsx:1281-1321`) can read into a new optional `QueueRow` field, e.g. `screen: { status, atMs } | null`.
3. **Add a timestamp and staleness.** `SessionSummary` has no time (`types.ts:169-173`; Rust `:58-62`; Go `claudetools.go:358-362`). A screen read must expire the same way hook state does (`STALE_AFTER_MS`, `paneAgentState.ts:42`), and much faster, because it was a single snapshot.
4. **Widen `inQueue`.** It currently admits only `light !== null || brief !== null` (`queueModel.ts:106-108`). Without a change, screen-only rows never reach the Queue. Per the Q4 mapping, admit only non-`idle`, non-`unknown` verdicts.
5. **Keep the light honest.** `trafficLight` returns null for `unknown` (`paneAgentState.ts:62`). Leave it that way: the light is "hook evidence only" by design (`DECISIONS.md:814`). Mark fallback rows as inferred in the UI, the way degraded briefs are rendered dimmer (`brief.rs:85`).
6. **Freshness has a cost.** Summaries are pulled by explicit user choice (90.C, `DECISIONS.md:805-807`), and each one is a real `claude -p` (10–30 s per chunk, `docs/vault/frontend-panes.md:294-296`). A fallback that refreshes on its own would reverse that decision, which needs Yossi's call. With no auto-refresh, the fallback is only as fresh as the last manual summarize.

Rule #1 is not an obstacle. The verdict is already screen-derived display data and is never logged (`sessions_overview.rs:20-23`, `:462-475`). Keeping the status word in memory, the same way briefs are kept, adds no new exposure.

**Confidence:** high that precedence is preserved (the guard is strictly below the existing branches). Medium on the size of the change list: it was derived from reading the code, not from a prototype.

## Options

- **A — QueueStatus as the one vocabulary; LLM verdict as the lowest-priority source (recommended).** Cost: the 6 Q5 changes plus i18n relabels. Risk: low, because hook and brief branches are untouched. Evidence: Q4, Q5.
- **B — A new neutral third vocabulary that both sides map into.** Cost: rewrites `QueueStatus`, `STATUS_EMOJI` (`QueueRow.tsx:17-24`), buckets and every i18n key. Risk: churn across 5 surfaces that already agree (Q3). No evidence that it buys anything over A.
- **C — Mapping only, no fallback.** Restate overview pills through the reverse mapping when a hook-backed pane holds the session, and fix labels. Cost: smallest. Leaves panes with no hooks out of the Queue, which is the gap the request names.

## Recommendation

Adopt **Option A**. `QueueStatus` becomes the single vocabulary, and the screen read becomes a lower-trust source into it:

- `working`→`working`
- `waiting_input`→`waiting`
- `error`→`stuck`
- `idle` and `unknown` → no agent signal

On any session a hook-backed pane holds, the overview's pill should show the Queue's verdict instead of the LLM's. The `done` state, which has no overview equivalent, should be shown as 💤 `done`.

For the fallback, add a branch at the very bottom of `queueStatus()` (`app/src/queueModel.ts:52`), guarded by `state==="unknown" && brief===null && screen fresh`. Widen `inQueue` (`:106-108`) to admit screen-only rows. Lift the summaries out of `SessionsOverviewWindow.tsx:74` into an App-level, timestamped store that `allPaneAgentRows` (`App.tsx:1281`) joins through `Workspace.tmux_session`.

Independently, and cheaply: fix the label collision in half 2. `queue.status.done` and `sessions.status.idle` must not share a word, and Hebrew `sessions.status.idle` must not say "ממתין".

The accepted trade-off: a fallback row is only as fresh as the user's last manual summarize, because the 90.C pull-only decision stands.

What would change this recommendation:
- Yossi deciding the overview should auto-refresh. Then staleness matters less and the cost question moves to the front.
- A live measurement showing the model reads Claude Code's idle input box as `waiting_input` in most cases. Then the reverse mapping for `done` needs a screen-side rule.

## Open questions

- **Which verdict does the model give for Claude Code's empty input box** (`idle` vs `waiting_input`)? This decides how often disagreement 1 in Q3 occurs. To answer it: run the Phase 90 FOLLOWUPS P1 live smoke (step 1) on a host with an idle Claude pane and record the pills. This pipeline cannot run it (Linux host, no live SSH desktop).
- **Should the fallback auto-refresh**, which reverses 90.C's pull-only rule? This is a product decision for Yossi (`docs/DECISIONS.md:805-807`).
- **How many real panes have no hooks** (non-Claude agents, remotes without the ymux CLI)? This sets the value of Q5. It is unmeasured: there is no telemetry for it in the repo. Answering it would take a count of `agent_runs` entries vs agent-looking panes on a live desktop.
- **Is the `"idle"` member of `PaneAgentState` in TS (`paneAgentState.ts:18`) intentional?** The Rust `as_str` never emits it (`lib.rs:290-294`). It is harmless dead code today, so check with `git log -S'"idle"' -- app/src/paneAgentState.ts`.

## Sources

- `app/src-tauri/src/sessions_overview.rs:53,58-62,96-105,238-245,251-280,409-475,487-505`: LLM verdict set, prompt definitions, normaliser, command, session→pane join
- `app/src-tauri/server/internal/term/claudetools.go:357-362,376-384,438-490`: Go daemon mirror of prompt + parser
- `app/src/backend/web.ts:922-926`: web route for summarize
- `app/src/types.ts:166-173`: TS `SessionSummary`, no timestamp
- `app/src/SessionsOverviewWindow.tsx:74,89-125,153-158,166,203,310-311,488-489`: only consumer; local, pulled, cleared
- `app/src/queueModel.ts:6-10,32-53,58-65,106-108`: `QueueStatus`, precedence, buckets, admission
- `app/src-tauri/src/brief.rs:63-80,83-85,249,259-262`: `BriefStatus`, aliases, default `Done`, degraded
- `app/src-tauri/src/rpc_server.rs:1485-1497,1565-1572,1590-1622`: prompt/stop/session-end mutate briefs + `session_ended`
- `app/src-tauri/src/lib.rs:263-294,304-313,380-398`: `PaneAgentState` + hook transition table
- `app/src/paneAgentState.ts:16-22,42,62`: TS state mirror, staleness, null light for unknown/idle
- `app/src/App.tsx:1272-1321,1359-1368,2894,2974-3030,5228,5440,5478-5500,5515`: single row source, surface wiring, overview gets no rows
- `app/src/QueuePanel.tsx:16-17,57`; `app/src/QueueRow.tsx:11-24,44`; `app/src/BriefingCard.tsx:18,40`; `app/src/ContextRail.tsx:68`: surfaces share `queueStatus`
- `app/src/i18n/{en,he,ar,ru}.json:1223-1228,1349-1353`: label collision (`git grep` of `queue.status.done` / `sessions.status.idle` across 4 locales)
- `docs/DECISIONS.md:791-807,810-818`: Phase 90 + 90.C pull-only decision; BRIEF "live hook state outranks the brief"
- `docs/vault/frontend-panes.md:285-296`: overview summarize flow, chunk cost
- `git grep -n waiting_input -- app/src app/src-tauri/src app/src-tauri/server` → producers only in `sessions_overview.rs`, `claudetools.go`; consumers only `types.ts`, `SessionsOverviewWindow.tsx`, `App.css`, i18n (chat `waiting_input` is a separate Phase 69 set)
- `git log --oneline -- app/src/SessionsOverviewWindow.tsx` / `--grep=#41` → `233d4f36 Merge pull request #41`, `5d681167 Phase 90.C`
- No web sources used: the question is fully internal to this repo.
