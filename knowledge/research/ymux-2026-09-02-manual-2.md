# ymux-2026-09-02-manual-2 — research report

## Question
BACKLOG P2 "BRIEF ⇄ sessions-overview unification, half 2": the active-sessions overview pays for a `claude -p` call even when a pane already has a free, self-reported brief in `AppState.briefs`. Should it prefer the brief and fall back to the LLM only for sessions with no brief? And can BRIEF's deferred "refresh the picture 🤖" button reuse the overview's summarize pipeline instead of getting its own plumbing? This report covers the current code only. Nothing in ymux was changed.

### Q1 — Does the sessions overview call `claude -p` for every row, including panes with a fresh brief in `AppState.briefs`
### Q2 — What defines a "fresh" brief and which code path reads `AppState.briefs`
### Q3 — Which pipeline does the BRIEF "refresh the picture 🤖" button use today, and is it deferred or absent
### Q4 — Can sessions_overview's summarize pipeline be called from the BRIEF refresh button without new plumbing, and what would have to change

## Findings

### Q1
**Answer:** The request says "a `claude -p` call per row", but the code does not work that way. Each call covers a batch of rows, and only rows the user ticked are summarized. Still, the core of the complaint is right: **the overview never looks at `AppState.briefs`**. A ticked row whose pane has a brief still goes into the LLM batch. Confidence: **high**.

Evidence:
- Summaries are pulled, not run automatically. Nothing is summarized until the user ticks rows and presses "Summarize selected" (`app/src/SessionsOverviewWindow.tsx:16-22`, `:152-158`; decided in `docs/DECISIONS.md:807`).
- The frontend splits the ticked names into chunks of 10 (`app/src/SessionsOverviewWindow.tsx:29`). It sends one `sessions_overview_summarize` invoke per chunk (`:97-103`).
- The backend de-duplicates the names, caps them at 25 (`app/src-tauri/src/sessions_overview.rs:39`, `:418-424`) and runs **one** `claude -p` over all of them. Over SSH this is a single remote pipeline (`:437-444`, `build_ssh_summary_script` `:193-209`). Locally the captures are framed into one stdin and passed to `run_local_claude` (`:448-456`). So the cost is one LLM call per chunk of up to 10 ticked sessions, not one per row.
- The command's inputs are only `workspace_id, names, lang` (`sessions_overview.rs:412-417`). Its only state reads are `state.workspaces` (`workspace_connection`, `:395-405`) and the SSH handle / claude path (`:439-440`). `briefs` appears nowhere in the module. `grep -rn briefs app/src-tauri/src/*.rs` finds hits only in `brief.rs`, `lib.rs:182,2902-2909`, `rpc_server.rs:1486,1565,1614` and `context_store.rs` comments.
- The frontend overview gets no brief data either. `SessionsOverviewWindow` takes no brief-related prop (`SessionsOverviewWindow.tsx:33-48`; mounted at `app/src/App.tsx:5478-5500`).
- The browser/daemon path behaves the same way. `web.ts:922-926` posts names and lang to `/api/v2/term/sessions/summarize`. The Go handler (`app/src-tauri/server/internal/term/claudetools.go:493-530`) does the same de-dup and cap, adds a `TryLock` that returns 409 when a summary is already running, and also has no brief lookup.
- `docs/DECISIONS.md:807` already names this gap: "Showing a fresh self-reported BRIEF for free instead of an LLM call is the BACKLOG unification item, deferred."

### Q2
**Answer:** **The code has no definition of a "fresh" brief.** A brief has a timestamp (`updated_ms`) and a `session_ended` flag, but no TTL and no staleness check. The one staleness constant in the frontend (`STALE_AFTER_MS`, 6 h) applies to the traffic light, not to briefs. Three paths read `AppState.briefs`, all through the Queue/Briefing data flow, and none of them is in the overview. Confidence: **high** that no definition exists. The right definition is a design choice (see Open questions).

Evidence — the fields available for a freshness rule:
- `PaneBrief.updated_ms`: epoch ms of the Stop that produced the brief (`app/src-tauri/src/brief.rs:103-106`). It is stamped at `rpc_server.rs:1554` (`brief_from_stop(last_msg, auto_title, now_ms)`).
- `PaneBrief.degraded`: true when the agent wrote no `[ymux-brief]` block. The brief is then inferred from the title plus the first line of the message (`brief.rs:84-85`; `docs/vault/backend-rpc.md` § Briefs).
- `PaneBriefEntry.session_ended`: set on SessionEnd. The brief is **kept** (`brief.rs:122-124`; `rpc_server.rs:1614-1615`).
- `PaneBriefEntry.prompt_ms` / `last_prompt`: stamped on UserPromptSubmit (`rpc_server.rs:1486-1487`; `brief.rs:118-121`). A prompt newer than `updated_ms` means the agent is working on a new turn, so the last brief describes the previous turn.
- Briefs are not dropped when a pane closes. They live in memory only and are keyed by the resolved pane id (`vault/pane-close-cleanup.md:7`; `brief.rs:110-113`). A pane id therefore keeps its brief after its session dies.

Existing staleness logic, which is not about briefs:
- `STALE_AFTER_MS = 6 h` (`app/src/paneAgentState.ts:42`) is checked against the agent state's `stateSince` (`:63`) and only gates the traffic-light colour.
- `rowSinceMs` (`app/src/queueModel.ts:99-101`) uses `brief.updated_ms` only to show and sort by age.

Code paths that read `AppState.briefs`:
1. `pane_briefs` Tauri command: clones the whole map (`app/src-tauri/src/lib.rs:2902-2909`). It hydrates the frontend `briefs` signal (`App.tsx:3984`).
2. The `pane:brief` event: `emit_brief_event` after each mutation (`rpc_server.rs:1565-1572`), mirrored with a seq guard at `App.tsx:4422-4426`.
3. `rpc_server.rs` hook arms (user-prompt-submit `:1486`, stop `:1565`, session-end `:1614`): read-modify-write.
- In the frontend, the map feeds `allPaneAgentRows()` (`App.tsx:1281-1300`), which builds `QueueRow.brief` (`queueModel.ts:14-27`) for QueuePanel and BriefingCard. The daemon/browser path has a parallel copy in the hello payload (`server/internal/term/events.go:121`, `PaneBriefs map[string]agent.BriefEntry`; `web.ts:1124`).

### Q3
**Answer:** **The button does not exist.** It is a deferred idea with no code, so it has no pipeline. Confidence: **high**.

Evidence:
- `docs/DECISIONS.md:817` (BRIEF, 2026-09-01) reads: "Deferred to BACKLOG: `ymux-brief` skill seeding, LLM "refresh the picture" button over transcripts." The intended source was **transcripts**, not screen captures.
- `grep -n -i "refresh\|summar\|claude" app/src/QueuePanel.tsx app/src/BriefingCard.tsx app/src/QueueRow.tsx` → the only hit is the CLAUDE.md adoption-snippet comment (`QueuePanel.tsx:32`). There is no refresh control, no summarize invoke and no 🤖 in any BRIEF surface.
- BACKLOG.md:20 (this ticket's source) says this entry supersedes the 2026-09-01 from-plan:brief entry for the button. Re-deciding that is out of scope.
- Two summarize pipelines exist that the button could reuse:
  - (a) `sessions_overview_summarize`: screen capture → one `claude -p` → `{name,status,summary}` rows. Ephemeral, returned to the caller. Works on SSH, macOS local tmux and Windows local zellij; WSL returns `Err` (`sessions_overview.rs:437-457`).
  - (b) `claude_summary.rs` `claude_summarize` / `summarize_session_inner` (`app/src-tauri/src/claude_summary.rs:234,323`): JSONL transcript → `claude -p`, saved as a Note tagged `summary`. **SSH only**, with no local fallback (`docs/vault/backend-claude.md:19-31`). This one matches the "over transcripts" wording at `DECISIONS.md:817`. The request asks for (a).

### Q4
**Answer:** **Partly.** The backend command and its result type can be reused unchanged. What is missing is a link from a BRIEF row (keyed by **pane id**) to the overview's input (a **multiplexer session name** within a **workspace id**). There is also a status-vocabulary mismatch. Closing the gap needs either a small backend helper or a new optional argument, so "zero new plumbing" is not achievable. Confidence: **medium-high**.

What can be reused as-is:
- `sessions_overview_summarize(workspace_id, names, lang)` → `Vec<SessionSummary{name,status,summary}>` (`sessions_overview.rs:55-62`, `:412-482`). It already handles SSH, macOS and Windows, the 25-session cap, Rule #1 logging (`:461-480`) and parsing that degrades to `unknown` instead of failing (`:251-281`). A one-name call is valid.
- The frontend wrapper pattern: one `backend.call(...)` per chunk, with a request-id guard (`SessionsOverviewWindow.tsx:90-126`).
- The web/daemon route `/api/v2/term/sessions/summarize` (`web.ts:922-926`, `claudetools.go:493`). A browser client gets the button too.

What would have to change:
1. **Pane → session name.** `QueueRow` holds `wsId` and `paneId`, but no session name (`queueModel.ts:14-27`). The frontend only knows the name for Phase 90.B session rows (`Workspace.tmux_session`, `App.tsx:2385-2392`). The backend can resolve it today: `pane_holding_session(state, ws, conn, name)` maps name → pane (`sessions_overview.rs:487-509`), and `find_pane_by_tmux_session` does the same (`lib.rs:2089`). Both run from `Session::{Ssh,Local}.tmux_session`. A reverse lookup (pane → name) is a few lines over the same maps (`core.pane_sessions` + `core.sessions`). It needs either an optional `pane_ids` argument on `sessions_overview_summarize` or a tiny `pane_session_name` command. A pane with no persistent session (plain SSH shell, non-persistent local PTY) has no name and cannot be captured, so the button has to be hidden or disabled for it.
2. **Status vocabulary.** The overview returns `idle|working|waiting_input|error|unknown` (`sessions_overview.rs:53`). BRIEF uses `working|waiting-for-you|stuck|done` (`docs/BRIEF.md` wire format). The answer needs mapping (e.g. `waiting_input→waiting-for-you`, `error→stuck`, `idle→done`) or must be shown as a separate "🤖 picture" line, never written into `AppState.briefs` as if the agent had reported it. `docs/DECISIONS.md:814` keeps live hook state as the authority for placement.
3. **Where the result lives.** The overview result is ephemeral frontend state (`summaries` signal, `SessionsOverviewWindow.tsx:74`). For BRIEF, the simplest option is a frontend-only overlay keyed by pane. Persisting it in `AppState.briefs` would mix LLM output with self-reported briefs and needs a new field (for example `source: llm`).
4. **Concurrency.** The daemon allows one summary at a time (`claudetools.go` `overviewMu.TryLock` → 409). A Queue button and an open overview could collide on the browser path. The Rust path has no such lock.

For the forward direction (overview prefers briefs), the same name→pane link is the only real blocker. The backend already has `pane_holding_session`, so `sessions_overview_summarize` could split `names` into "has a fresh brief" (answer from `state.briefs`, no capture) and "rest" (LLM). Doing the split in the backend means the Go handler needs the same change, or the browser path keeps paying for the LLM call. Doing it in the frontend needs `briefs()` plus a name→pane map passed into `SessionsOverviewWindow`, and the frontend has no such map today.

## Options

| Option | What | Cost / risk | Evidence |
|---|---|---|---|
| A — backend split | `sessions_overview_summarize` looks up each name with `pane_holding_session` → `state.briefs`. Fresh brief → row built from it (status mapped, summary = `ask · rec` / `delta` / `task`); rest → LLM. Mirror in Go `handleSessionsSummarize`. | Two implementations (Rust + Go). Needs a freshness rule. The frontend stays unchanged except a "from brief" badge if a source flag is added to `SessionSummary`. | `sessions_overview.rs:487`, `:412`; `claudetools.go:493` |
| B — frontend split | Pass `briefs()` and a name→pane map into `SessionsOverviewWindow`. Skip ticked names that have a fresh brief. | Needs a new name→pane source in the frontend (none exists). Covers Tauri and browser in one place. | `App.tsx:5478`; `queueModel.ts:14` |
| C — refresh button reuses overview | Queue/Briefing row 🤖 → resolve pane→name → `sessions_overview_summarize(ws,[name],lang)` → show as overlay. | Small new resolver; status mapping; overlay state; hide for panes with no session. | Q4 items 1-4 |

## Recommendation
Do **A + C** and share one helper between them: a backend pane↔session-name resolver over `core.pane_sessions` + `core.sessions` (`sessions_overview.rs:487-509` already does one direction). For the overview (A), split the ticked names inside `sessions_overview_summarize`. Names with a fresh, non-degraded brief are answered from `AppState.briefs`, and only the rest are captured and sent to `claude -p`. Add a `source: "brief" | "llm"` field to `SessionSummary` so the UI can mark which rows were free, and apply the same split in the Go daemon's `handleSessionsSummarize` so the browser path saves the call too. For BRIEF (C), add the 🤖 button on a Queue/Briefing row. It resolves the pane to a session name and calls the existing command with one name, or with the new optional `pane_ids` argument. The result shows as an overlay line, never as a brief, and the button is hidden when the pane has no multiplexer session.

**Trade-offs accepted:**
- The logic lives in two languages, Rust and Go.
- The 🤖 picture comes from screen captures, not transcripts as `DECISIONS.md:817` first imagined.

**What would change this recommendation:**
- If Yossi wants a transcript-based picture, the right reuse target is `claude_summary.rs`, which is SSH-only, not the overview.
- If the daemon path is being retired, drop the Go mirror.

Proposed starting rule for "fresh": `!degraded && !session_ended && updated_ms >= (prompt_ms ?? 0)`, meaning no newer prompt since the brief was written. This is a proposal, not a measured fact. A human picks the final rule (Open questions).

## Open questions
- **Freshness rule (Q2):** the code defines nothing. Two choices need a human: whether an age cap applies (e.g. the 6 h `STALE_AFTER_MS` reused), and whether a degraded brief counts. Answer: Yossi's call, then fixed in a test.
- **Real cost saved (Q1):** unmeasured. The desktop logs `summarize: … sessions=N parsed=M took=…ms` (`sessions_overview.rs:470-479`) to the user's `%APPDATA%\ymux\debug.log`, which this host cannot see. The pipeline DB does not cover desktop runtime. Answer: count `SESSIONS summarize` lines in a real debug.log, and the share of ticked sessions that hold a pane with a brief.
- **Status mapping (Q4):** `idle→done`, `error→stuck` are proposals. Answer: Yossi's call.
- **Zellij pane→name on Windows:** `Session::Local.tmux_session` holds the zellij name (`lib.rs:2085-2088`). Whether every Windows pane fills it in was not verified against a live run.

## Sources
- app/src-tauri/src/sessions_overview.rs:9-14, 39, 53, 55-62, 193-209, 251-281, 395-405, 412-482, 487-509 — overview command, batching, inputs, no brief read, name→pane helper
- app/src/SessionsOverviewWindow.tsx:16-22, 29, 33-48, 74, 90-126, 152-158 — pulled summaries, chunk 10, props, result state
- app/src/App.tsx:1281-1300, 2385-2392, 3984, 4422-4426, 5478-5500 — briefs signal, queue rows, overview mount
- app/src-tauri/src/brief.rs:84-128 — PaneBrief / PaneBriefEntry fields (updated_ms, degraded, session_ended, prompt_ms, seq)
- app/src-tauri/src/rpc_server.rs:1486, 1554, 1565-1572, 1614 — hook arms that write briefs
- app/src-tauri/src/lib.rs:182, 2085-2100, 2902-2909 — briefs field, find_pane_by_tmux_session, pane_briefs
- app/src/paneAgentState.ts:42, 63 — STALE_AFTER_MS, applied to the light only
- app/src/queueModel.ts:14-27, 99-101 — QueueRow shape, rowSinceMs
- app/src/backend/web.ts:922-926, 1124 — browser routes
- app/src-tauri/server/internal/term/claudetools.go:493-530; events.go:121 — Go summarize handler (TryLock), daemon brief map
- app/src-tauri/src/claude_summary.rs:234, 323; docs/vault/backend-claude.md:19-41 — transcript summarize pipeline, SSH-only
- docs/DECISIONS.md:807, 814, 817 — pulled summaries + deferred unification; hook state outranks brief; deferred refresh button
- docs/BRIEF.md — brief wire format and status vocabulary
- vault/pane-close-cleanup.md:7 — briefs not dropped on pane close
- `grep -rn briefs app/src-tauri/src/*.rs` → hits only in brief.rs, lib.rs, rpc_server.rs, context_store.rs (none in sessions_overview.rs)
- `grep -n -i "refresh\|summar\|claude" app/src/QueuePanel.tsx app/src/BriefingCard.tsx app/src/QueueRow.tsx` → only QueuePanel.tsx:32 (snippet comment)
