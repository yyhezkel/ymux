# BRIEF — agent briefs, the Queue, and the Briefing card

ymux runs many agent sessions in parallel (local, zellij, remote tmux). BRIEF is
the layer that answers **"who needs me right now, and what for"** without
scanning panes by eye. Design history: `docs/DECISIONS.md` § 2026-09-01;
implementation notes: `docs/vault/backend-rpc.md` § Briefs,
`docs/vault/frontend-shell.md`, `docs/vault/frontend-panes.md` § QueuePanel.

## The wire format

A cooperating agent ends **every final answer** with a plain-text block, as the
last lines of the message:

```
[ymux-brief]
task: the task in a few words
status: working | waiting-for-you | stuck | done
ask: one closed question — only when you need my decision
rec: your recommendation for that question, in one sentence
next: the immediate next step
delta: what changed since your previous brief
```

Rules the desktop parser enforces / tolerates:

- The marker is a whole line, matched case-insensitively; the **last**
  occurrence in the message wins, so quoting the format mid-answer is safe.
- Keys are ASCII, split on the first `:` — values may be fully Hebrew/Arabic.
- Markdown decoration (`- `, `> `, `**bold**`, code fences) is stripped;
  unknown keys are ignored; every field is clipped to ~200 chars.
- `status` accepts the aliases `waiting` and `blocked`; missing → `done`.
- `ask` should never appear without `rec` — a question with no recommendation
  dumps the work back on the user.

No block? The desktop synthesizes a **degraded** brief (status `done`, delta =
first line of the message) and never invents an `ask`.

## How it travels

The Claude Code Stop hook already pushes its full payload — including
`last_assistant_message` — through the ymux CLI to the desktop. The brief is
parsed **desktop-side** out of that field: no CLI changes, and agents on remote
machines with older CLIs keep working. `UserPromptSubmit` contributes the
user's last prompt (clipped, in-memory, UI-only) so a running session's queue
row can say "got from you: …". `SessionEnd` keeps the brief and marks the
session ✅ closed.

## The surfaces

- **Queue panel** (Ctrl+Shift+Q, palette "Queue: Open") — every agent pane,
  grouped by workspace with counts, sorted by who needs you: blocked/stuck →
  waiting-for-you → done/closed → running. Live hook state always outranks a
  brief for placement. Its empty state carries a copy-paste CLAUDE.md snippet —
  that is the adoption path (a bundled skill is BACKLOG).
- **Briefing card** (Ctrl+Alt+Q, palette "Briefing: Show") — on entering a
  workspace: the 🎯 intent one-liner (stored on the workspace, schema v3) plus
  that workspace's brief rows. The automatic triggers — return-after-absence
  and idle-return — are **opt-in** under Settings → General → Briefing card;
  the shortcut always works.
- **Sidebar** — a workspace holding a stuck/waiting pane carries the existing
  attention dot at a middle intensity (blocking permission > brief > activity).

## Privacy

Rule #1 applies throughout: brief text, intents and prompts live in memory and
the UI only. Log lines carry pane ids, lengths and flags — never content.
Briefs are treated as untrusted display data and rendered as plain text.
