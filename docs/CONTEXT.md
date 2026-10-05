# CONTEXT — per-session context and the Context Rail

BRIEF (`docs/BRIEF.md`) answers "who needs me right now". CONTEXT answers
"what is this session about, and where does it stand", and it keeps the answer
across restarts. Design history: `docs/DECISIONS.md` § 2026-10-05 — Phase 103.
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
- `version` bumps on every write. Phase 103.C uses it.

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
- **Collapsed:** a 36 px strip with a badge counting the sessions that need you.
- **Width and collapsed state:** stored in `localStorage`. Each access is wrapped
  in try/catch.

Top to bottom:

1. **Others strip:** "N sessions waiting / stuck in M other workspaces", computed
   from the existing Queue rows (no extra backend call). Clicking it activates
   the most urgent of those workspaces.
2. **🎯 Intent:** the same editor the Briefing card uses (`IntentEditor.tsx`).
3. **One card per session** of the current workspace:
   - The pane's live Queue row (`QueueRow.tsx`, shared with the Queue panel and
     the Briefing card). The newest session in a pane owns that pane's row.
   - 📝 The first prompt, clipped to 180 characters. Click to expand.
   - The log, newest first: `time · icon · ask·rec | delta → next`. Five lines
     are shown, with "show all" for the rest.

   Closed sessions are listed under "show N closed sessions". An agent pane that
   has no session record yet (an older CLI, or a non-Claude agent) still gets a
   plain row.

## Privacy

Prompts and brief text are user content (Rule #1). They live in the session
files and the UI only. Log lines carry session ids, pane ids, counts and
versions. The files sit in the user's own config dir, next to `notes.json`.
Everything is rendered as plain text with `dir="auto"`.

## Stage C (next PR)

Phase 103.C feeds this context back to the agent. The `SessionStart` hook asks
the desktop for `additional_context`:

- On `compact` / `resume`: the session's first prompt plus its last 8 log lines.
- On `startup`: the workspace intent plus one line per sibling session.

The result is capped at 1.5 KB under a `[ymux-context]` header, and Settings has
a toggle to turn it off.
