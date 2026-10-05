# CONTEXT — per-session context and the Context Rail

BRIEF (`docs/BRIEF.md`) answers "who needs me right now". CONTEXT answers
"what is this session about, and where does it stand", and it keeps the answer
across restarts. Design history: `docs/DECISIONS.md` § 2026-10-05 — Phase 104.
Implementation notes: `docs/vault/backend-rpc.md` § Session context and
`docs/vault/frontend-shell.md` § ContextRail.

## The model

One record per **Claude Code session id** (`payload.session_id` on every hook):

```
SessionContext { session_id, ws_id, pane_id, cwd,
                 first_prompt (≤ 2000 chars), first_prompt_ms,
                 log: [LogEntry] (≤ 200, oldest dropped first), version }
LogEntry       { ts_ms, kind: turn | closed, status,
                 task, delta, next, ask?, rec?, degraded }
```

- **First prompt.** Set from `UserPromptSubmit`'s `prompt`, **only while it is
  empty**. Later prompts never overwrite it. An app restart mid-session leaves it
  empty until the next new session.
- **The log.** Every `Stop` appends one `turn` line built from the parsed
  `[ymux-brief]`. With no brief, the degraded brief is used (status `done`, delta =
  first line of the message, `degraded: true`). `SessionEnd` appends a `closed`
  line, with Claude Code's `reason` enum as its delta. **Nothing calls an LLM.**
  The log costs zero tokens.
- **Placement.** `ws_id` / `pane_id` / `cwd` follow the latest hook. `ws_id` is
  the *screen* workspace holding the pane (`find_workspace_for_pane` on the
  resolved pane). A value that is missing never erases a known one.
- `version` bumps on every write. Phase 104.C uses it.

## Persistence

`<config_dir>/context/sessions/<session_id>.json`:

- Written atomically (tmp + rename, Rule #7).
- The session id must match `[A-Za-z0-9_-]{1,128}`. Anything else is refused,
  so no path traversal.
- A file that fails to parse is **never overwritten**. That session is refused
  for the rest of the process and a warning is logged.
- At startup, files whose mtime is older than **30 days** are deleted. The
  remaining files are loaded into memory on a background thread.

This **reverses** BRIEF's "briefs live in memory only" for the per-session log.
`AppState.briefs`, which drives the Queue, is still in memory only.

## Events and commands

- `context:changed` `{ session_id, ws_id }`: emitted after every write.
- `session_context_list(ws_id) -> SessionContext[]`: one workspace's sessions,
  most recent activity first.
- `session_context_get(session_id) -> SessionContext | null`.

## The Context Rail

The rail is a docked column at the **inline-end** of the main layout: the third
`.app` grid column. It is on the right in LTR and on the left in RTL. It is not a
drawer: there is no backdrop and it never covers the panes.

- **Toggle:** Ctrl+Shift+K (`toggle_context_rail`, rebindable) or the palette
  entry "Context Rail: Toggle".
- **Collapsed:** a 36 px strip.
- **Width and collapsed state:** stored in `localStorage`. Each access is wrapped
  in try/catch.

**Each window shows only its own context.** The rail follows the **focused pane**:
App's `activePaneId`, the same signal keyboard and focus routing use. Changing
focus changes the card. There is no list of the workspace's other sessions and
no strip for other workspaces. Both were in the first cut and were removed at
Yossi's call (DECISIONS 2026-10-05 follow-up).

Top to bottom:

1. **🎯 Intent:** the workspace's one-liner. It uses the same editor as the
   Briefing card (`IntentEditor.tsx`).
2. **The focused pane's current session:** the session with the newest activity
   whose `pane_id` is the focused pane. Its card shows:
   - The pane's live Queue row (`QueueRow.tsx`, shared with the Queue panel and
     the Briefing card).
   - 📝 The first prompt, clipped to 180 characters. Click to expand.
   - The log, newest first: `time · icon · ask·rec | delta → next`. Five lines
     are shown, with "show all" for the rest.
3. **Earlier sessions in this pane (N):** a collapsed toggle listing the pane's
   older sessions, for example after a restarted `claude` or a `/clear`. It
   resets whenever focus moves to another pane.

**Empty states.** Each is a one-line hint, never the whole workspace:
- No focused pane: "focus a pane".
- No session record for the pane: "no Claude session in this pane yet".

An agent pane on an older CLI, which has a live row but no record, still shows
that row.

The rail fetches `session_context_list(ws_id)` once per workspace and on
`context:changed`. Picking the focused pane's sessions out of that list is
client-side (`contextModel.sessionsForPane`), so moving focus costs no IPC.

## Privacy

Prompts and brief text are user content (Rule #1). They live in the session
files and the UI only. Log lines carry session ids, pane ids, counts and
versions. The files sit in the user's own config dir, next to `notes.json`.
Everything is rendered as plain text with `dir="auto"`.

## Injection back into the agent (Phase 104.C)

Claude Code's `SessionStart` hook is registered again (hook spec v1.7.0;
existing machines re-run `setup-hooks`). Its only job is
`ymux claude-hook session-start` → RPC `context.inject` → print
`{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}`.
It never creates a feed card and never fires a toast.

| `source` | What the agent gets |
|---|---|
| `compact`, `resume` | `This session's first prompt: …` (≤ 500 chars), then `Where this session stands (oldest → newest):` and the last 8 log lines (`- [status] task — delta → next: … (asked: … · rec: …)`) |
| `startup` | `Workspace goal: <intent>`, then `Other agent sessions in this workspace:` with one `- task — status` line per other OPEN session, most recent first, at most 8 |
| `clear`, anything else | nothing |

Rules:

- Everything sits under a `[ymux-context]` header line.
- Each field is flattened to one line with control characters stripped.
- The total is capped at **1536 bytes**. When the text doesn't fit, the oldest
  log/sibling lines are dropped first, then the result is hard-clipped on a char
  boundary.
- Nothing to say → empty → the hook prints nothing.

The budget is **~300 ms** and the hook **fails open**: a missing app, a slow
tunnel or an error (including the Go daemon, which does not know the method)
means no context, never a stalled session start. Set
`YMUX_CONTEXT_TIMEOUT_MS` (100–3000) on a remote whose tunnel round trip is
slower than that.

**Off switch:** Settings → General → Briefing → *Inject context into sessions*
(`settings.brief.inject_context`, on by default).

**Privacy:** the injected text is the user's own prompt and the agent's own
briefs, returned to that same agent or to an agent working in the same
workspace. Log lines record the source, the ids and the byte count only.
