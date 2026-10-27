# ymux-2026-08-03-manual — research report

## Question
BACKLOG.md parked a command-palette polish: cherry-pick commit `5413ef9` from branch `design-pass-01` once the palette outgrows ~23 commands. This report checks the current command count, whether that commit and branch still exist, whether the commit would cherry-pick cleanly onto `main`, and whether the trigger condition holds today.

### Q1 — Does the command palette currently register more than ~23 commands (counted at file:line in the palette source)
### Q2 — Does commit `5413ef9` exist on `design-pass-01`, and which files and command entries does it change
### Q3 — Does `5413ef9` cherry-pick onto current `main` without conflicts (dry-run, no commit)
### Q4 — Based on Q1 and Q3, is the trigger condition met today (yes/no, with the reason)

## Findings

### Q1
**Yes. The palette registers 29 commands, 6 more than the ~23 the backlog cited.** At the time the backlog entry was written it registered 24.

- `CommandPalette.tsx` defines no commands of its own. It receives them through the `commands` prop (`app/src/CommandPalette.tsx:5-7`, `:20`).
- The only caller is `App.tsx`: `<CommandPalette … commands={paletteCommands()}` (`app/src/App.tsx:5533-5535`).
- `paletteCommands()` returns one literal array, `app/src/App.tsx:1191-1226`. It holds 29 `{ id: … }` entries, from `workspace.new` (`:1192`) to `fm.open` (`:1224`).
  - `sed -n '1186,1228p' app/src/App.tsx | grep -c '{ id: "'` → `29`
- By id prefix: workspace 5, queue 1, contextRail 1, briefing 1, pane 12, ssh 3, insights 1, settings 3, view 3, fm 1.
- Upper bound vs what a user sees:
  - 29 is the most commands that can appear at once.
  - `PALETTE_CAPS` (`app/src/App.tsx:146-149`) drops `ssh.provision` and `pane.openDiff` on hosts that lack the `ssh` or `diffPane` capability (`:1227-1231`).
  - Per-command `enabled()` guards hide the workspace and pane commands when no workspace or pane is active (`app/src/CommandPalette.tsx:29-31`).
- Count at the time the backlog entry was written: `git show 1fb4f354:app/src/App.tsx | awk '/const paletteCommands = /,/^  };/' | grep -c '{ id: "'` → `24`. The "~23" was approximately right. Growth since then: +5 (~21%).
- Matching is still plain substring: `app/src/CommandPalette.tsx:33` says "Case-insensitive substring match on the label. v1 — no fuzzy lib.", and `:38` uses `includes(q)`. `CommandPalette.tsx` was last touched in `bddc0b02` (2026-05-28): `git log -1 --format='%h %ad' --date=short -- app/src/CommandPalette.tsx` → `bddc0b02 2026-05-28`.

Confidence: high. The array is static, so a line count is exact.

### Q2
**No. Neither the commit nor the branch exists anymore, either locally or on `origin`.** Both were deleted on 2026-08-23 without being archived.

- `git rev-parse --verify 5413ef9^{commit}` → `fatal: Needed a single revision`
- `git cat-file -t 5413ef9` → `fatal: Not a valid object name 5413ef9`
- `git rev-list --all | grep ^5413ef9` → no output
- `git branch --list design-pass-01` → empty; `git rev-parse --verify design-pass-01` → `fatal: Needed a single revision`
- `git ls-remote origin refs/heads/design-pass-01 | wc -l` → `0`. `git ls-remote --heads origin` lists only `main` and `fix/sessions-overview-header`.
- No other git clone under `/home/runner/*/` contains the object: a `git cat-file -t 5413ef9` loop over every directory with a `.git` found nothing.
- Recorded deletion:
  - `docs/DECISIONS.md:906`: "`browser-dev-mode-tickets` and `design-pass-01` — local-only, never pushed, deleted on Yossi's call without archiving."
  - `PROGRESS.txt:4817-4819`: "design-pass-01 (5) went without archiving on Yossi's explicit call -- local-only, never pushed."
  - This contradicts the instruction to keep the branch alive as the archive (`BACKLOG.md:80`). The 2026-08-23 decision came later and wins.
- What the commit changed, known only from secondary records. The diff itself cannot be inspected.
  - Commit `1fb4f354` (2026-08-03), the commit that added the backlog entry, lists these files:
    - `app/src/CommandPalette.tsx` (246 lines of change)
    - the 4 i18n files (he/en/ar/ru): `cmd.cat.*` plus palette hints and a result count
    - `app/src/App.css` (109 lines)
  - Features it describes:
    - an fzy-style fuzzy scorer with `<mark>` highlighting
    - category grouping derived from the dotted id prefix, with sticky headers
    - per-category icons and right-aligned keybinding hints
    - a Recent section (localStorage, last 5 commands)
    - a footer with navigation hints and a result count
  - Command entries: none. The entry states "no churn on the command definitions" (`git show 1fb4f354`, BACKLOG hunk; mirrored at `BACKLOG.md:48-80`). The command list in `App.tsx` was not part of the commit.

Confidence: high that the commit is gone. Medium for the file list, which comes from the backlog author's description rather than a diff.

### Q3
**It cannot be tested, because the commit object no longer exists.** No dry run was possible, whether by `git cherry-pick --no-commit` or by `git merge-tree`, since both need the object (see the Q2 evidence).

- Secondary forecast from 2026-08-03 (`1fb4f354`; `BACKLOG.md` cost paragraph):
  - `CommandPalette.tsx` and the 4 i18n files were expected to apply cleanly.
  - The 109 `App.css` lines were expected to conflict.
- The `CommandPalette.tsx` half of that forecast probably still holds, since the file has not changed since 2026-05-28 (Q1). This is inferred, not tested.
- Contradiction to note: `docs/DECISIONS.md:1059` says "main absorbed a fuzzy palette separately". The live code contradicts this: `app/src/CommandPalette.tsx:33,38` still does substring match, and the code wins.

Confidence: high that it is untestable. The clean-or-conflict question itself stays open.

### Q4
**No. Today the trigger cannot lead to the planned action.**

1. Count side (Q1): 29 > ~23, so the backlog title's literal "outgrows ~23 commands" is met. The entry's own detailed trigger is stricter: "the command count roughly quadruples, or discoverability complaints come in" (`git show 1fb4f354`, BACKLOG hunk). That means roughly 92+ commands, and 29 is far short.
2. Action side (Q2/Q3): the planned action is "cherry-pick `5413ef9`", and that commit no longer exists anywhere reachable. Even with the count condition met, there is nothing to cherry-pick.

Net: the literal count condition is met, but the stricter one is not, and the planned action cannot be carried out. Any palette polish would have to be written fresh on `main`. The backlog itself named the cheap part for that case: "categories + keybinding hints (~40 of the 246 lines) … cheap to rewrite fresh on `main`".

Confidence: high.

## Options
- **A — Close the backlog line as obsolete.** Cost: none. Risk: none, because the trigger references a commit that no longer exists. Evidence: Q2.
- **B — Replace it with a fresh-rewrite backlog item.** The item would cover category grouping and keybinding hints in `app/src/CommandPalette.tsx`, triggered at roughly 90+ commands or on discoverability complaints. Cost: one backlog line. Risk: low. Evidence: Q1 shows a 29-command palette growing about 5 commands in 3 months.
- **C — Rewrite the polish now.** This is not justified by the backlog's own size argument: substring match is adequate at 29 commands. Out of scope for this ticket.

## Recommendation
Option A, plus Option B if the categories and keybinding hints are still wanted.

The original ticket can no longer be done as written: `5413ef9` and `design-pass-01` were deleted without archiving on 2026-08-23 (`docs/DECISIONS.md:906`). The count has crept from 24 to 29 (`app/src/App.tsx:1192-1224`). That passes the loose "~23" in the title, but it is nowhere near the "roughly quadruples" threshold the author actually set.

Trade-off accepted: the 246-line polish is lost and would have to be rewritten from the description in `1fb4f354`.

What would change this: a surviving copy of `5413ef9` turning up on another machine, such as Yossi's box or a collaborator's clone, or user complaints about palette discoverability.

Side note for a human: `docs/DECISIONS.md:1059` wrongly claims `main` has a fuzzy palette. `app/src/CommandPalette.tsx:33` shows it does not.

## Open questions
- Q3, the clean-apply result: unanswerable without the object. It would be answered by `git cat-file -t 5413ef9` succeeding in a clone on another machine (Yossi's Windows box or a collaborator's), followed by `git merge-tree --write-tree --merge-base 5413ef9^ main 5413ef9` there.
- Q2, the exact diff: same dependency. The current file list is second-hand, from the `1fb4f354` backlog text.

## Sources
- `app/src/CommandPalette.tsx:5-7,20` — commands come from the parent via the prop
- `app/src/CommandPalette.tsx:29-31` — `enabled()` filter
- `app/src/CommandPalette.tsx:33,38` — substring match, no fuzzy
- `app/src/App.tsx:146-149` — `PALETTE_CAPS` capability gate
- `app/src/App.tsx:1186-1232` — `paletteCommands()`, 29 entries at `:1192-1224`
- `app/src/App.tsx:5533-5535` — the only `CommandPalette` mount
- `docs/DECISIONS.md:906` — `design-pass-01` deleted without archive (2026-08-23)
- `docs/DECISIONS.md:1059` — the claim of a fuzzy palette on main, contradicted by live code
- `PROGRESS.txt:4817-4819` — branch deletion record
- `BACKLOG.md:48-80` — the parked entry, its trigger and its cost forecast
- `sed -n '1186,1228p' app/src/App.tsx | grep -c '{ id: "'` → `29`
- `git show 1fb4f354:app/src/App.tsx | awk … | grep -c '{ id: "'` → `24`
- `git show 1fb4f354` → the backlog hunk describing the `5413ef9` contents, trigger and cost
- `git rev-parse --verify 5413ef9^{commit}` → `fatal: Needed a single revision`
- `git cat-file -t 5413ef9` → `fatal: Not a valid object name`
- `git rev-list --all | grep ^5413ef9` → empty
- `git ls-remote --heads origin` → `main`, `fix/sessions-overview-header` only
- `git log -1 --format='%h %ad' --date=short -- app/src/CommandPalette.tsx` → `bddc0b02 2026-05-28`
- Web: none used; the questions are repo-local.
