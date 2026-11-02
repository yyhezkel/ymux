# ymux-app-src-sidebar-tsx — research report

## Question
FOLLOWUPS.md (the P1 entry dated 2026-08-18, "NOT VERIFIED LIVE: project folders v4 … and the pane-header overflow menu") asks for a 9-step live smoke test of the project-folder tree and the pane-header overflow menu. This headless research stage cannot run the desktop app. Nothing can be launched here: no display, no Windows/macOS build, and CLAUDE.md Rule #17 bans local builds. So each question is answered by a **static audit of the code path** at HEAD `23f2870`. The audit checks whether the code implements each scenario's expected behaviour as written, and flags any place where the scenario no longer matches the code. Code paths that were changed after the 2026-08-18 entry was written are the most important finding. **Live pass/fail is still an Open question for every Q.**

### Q1 — Pane-header overflow menu — when window shrinks to 6-8 panes, do buttons move into chevron menu and does menu open correctly? Verify Hebrew text rendering.
### Q2 — SSH folder pinning — can folders be pinned from SSH workspaces and do they appear as child workspace rows with folder glyph and correct nesting?
### Q3 — SSH folder session isolation — when opening a pane from a pinned SSH folder, is the wizard read-only, does it show only that folder's sessions, and does `pwd` reflect the folder path?
### Q4 — Worktree nesting — when expanding a folder's worktrees and opening one, does it nest under the folder, is `pwd` the worktree path, and does the repo root NOT appear as a stub?
### Q5 — New worktree creation — when creating a new worktree from the + button, does the workspace open and survive the session (no hung cmux#5032 trap)?
### Q6 — Folder deletion cascade — when deleting a parent folder, does the confirm dialog name all descendants and show their count, and are all descendants deleted without affecting host directories?
### Q7 — Error states — for disconnected host (clear error, no spinner), nonexistent path (clear error, nothing created), and non-repo path (pins as folder + "no git" toast, promotes after git init), are all three handled correctly?
### Q8 — Persistence — after restarting the app, are folder nesting structure and collapse state preserved?
### Q9 — Legacy migration — when opening workspaces.json carrying the old `project_folders` key, do the pins reappear as child workspaces and worktree workspaces re-parent under them correctly?

## Findings

**Headline:** the smoke script is out of date. Phase 91.F moved worktrees out of the sidebar and into the Diff pane. Phase 92 made folders into "headers", which hold rows and have no panes of their own. As a result, scenarios (3), (4) and (5) describe UI that no longer exists in the form the script assumes. In the current code, scenario (5)'s expectation that "its workspace opens" is **not implemented**. The other scenarios have matching code paths.

### Q1
**Answer:** Implemented, and an earlier "menu does not open" bug was already fixed in code. Hebrew strings exist. RTL measurement is designed to work. The fix commit claims a Chromium check but there is no live desktop proof. Confidence: medium.
- The header actions live in one array. `visibleCount()` decides how many render inline, and the rest go into a chevron menu. Close is kept out of that array so it never moves (`app/src/PaneView.tsx:1033-1040`).
- The fitter `fit()` shrinks `visibleCount` until the summed child `offsetWidth` fits within `clientWidth` minus inline padding. Summing widths is used instead of `scrollWidth` specifically so it works in RTL (`app/src/PaneView.tsx:1184-1226`). It is re-run by a `ResizeObserver` on the header (`:1237-1246`) and by an effect on the action set (`:1252-1258`).
- The chevron button carries `aria-expanded` and toggles `showOverflow` (`app/src/PaneView.tsx:1483-1494`). The menu closes on an outside mousedown or Escape (`:1260-1279`).
- The earlier "chevron appears but click does nothing" bug came from `.pane-header { overflow:hidden }` clipping the absolutely positioned menu. It was removed in commit `eafd907` (2026-08-17), and the CSS comment documents this (`app/src/App.css:974-975`). That commit says it was re-verified in Chromium at 7 widths × LTR/RTL, and that the 130px-min-width menu still spills past a 130px pane.
- The menu is placed with `inset-inline-end: 0`, a logical property, so it mirrors correctly in RTL (`app/src/App.css:4072-4083`). The Hebrew tooltip is `"pane.tooltip.more_actions": "פעולות נוספות"` (`app/src/i18n/he.json:134`).

### Q2
**Answer:** Yes, at the code level. The SSH right-click "pin" path first arms the connection, then opens the SFTP `DirPicker`, then probes the path, then persists a child header with a folder glyph. One deviation from the script: pinning now also creates and **activates** a `shell` screen under the new folder (Phase 92). Confidence: medium-high.
- `startPinProjectFolder` handles SSH workspaces like this: it calls `workspace_ensure_connected` and then `setDirPickerFor` (`app/src/App.tsx:1888-1901`).
- The `DirPicker` lists the real remote directory through `file_list_remote` (`app/src/DirPicker.tsx:69-91`). On pick it calls `pinProjectFolder` (`app/src/App.tsx:5494-5502`).
- `workspace_pin_project_folder` pushes a folder with `parent_id = parent` and `layout: None`, then a `shell` screen, which becomes the active workspace (`app/src-tauri/src/lib.rs:6066-6142`).
- Nesting under an existing project root is refused with "already inside a project folder" (`:6094-6105`).
- Glyph: when `is_project_root` is true the header shows a folder plus a git badge (`docs/vault/frontend-shell.md` § Sidebar "Header glyphs").

### Q3
**Answer:** The read-only folder and the cwd injection are implemented. "A session list containing ONLY that folder's sessions" is **no longer the design**. The tmux picker has a *This folder* / *Whole server* toggle over a single response, and it opens on *Whole server* whenever the folder view would be empty. The script's "select it, open a pane" step is also out of date: a folder is now a header, and clicking it activates its first screen. Confidence: medium.
- Read-only directory: `folderAnchor()` = `p.workspaceCwd` (`app/src/PaneView.tsx:244-247`). When it is set, the wizard shows a locked `.nc-locked-dir` instead of the input (`:2074-2086`).
- The connect options send `cwdOverride = anchor`, and a picked session's project path never overrides the anchor (`app/src/PaneView.tsx:750-764`). The backend falls back to `cwd_override.or(ws_cwd)` (`app/src-tauri/src/lib.rs:9340`).
- Attach-only exception: when the target session is already live, the `cd` is deliberately **not** injected (`app/src/PaneView.tsx:765-772`; vault `backend-core.md` § attach-only guard). Reconnecting to a live session therefore keeps that session's own cwd, and `pwd` is not guaranteed to be the folder.
- Scope toggle: `inWorkspaceScope = s => s.owned || s.in_cwd`, and `pickScopeDefault` falls back to Whole server (`docs/vault/frontend-shell.md` § PaneView "The tmux picker's scope toggle").

### Q4
**Answer:** The sidebar step "expand its worktrees" **no longer exists**. Since Phase 91.F, worktrees are listed and opened from the Diff pane's worktree strip, and the sidebar shows no stub rows at all. So the requirement that the repo root does not appear as a stub is met by construction. Opening a worktree nests it under the folder, and its pane cwd is the worktree path. Confidence: medium.
- Vault, § Sidebar "Worktrees left the sidebar (Phase 91.F)": the `.pf-unopened` stub rows were removed.
- `openWorktree` → `workspace_open_worktree(rootWorkspaceId, worktreePath)` (`app/src/App.tsx:1935-1950`). This pushes a workspace with `parent_id = root` and `cwd = worktree_path` (`app/src-tauri/src/lib.rs:6203-6216`).
- Clicking the repo root's own entry matches the folder's `shell` screen through `(parent, cwd)` using `paths_equal` and activates that screen, so no duplicate row is created (`app/src-tauri/src/lib.rs:6179-6199`).

### Q5
**Answer:** **Gap.** Creating a worktree from the Diff strip's `+` only creates the git worktree and re-lists the strip. **No workspace is opened.** The user has to click the new entry in the strip. Once a worktree workspace *is* opened, the cmux#5032 trap is avoided: the first pane is a plain interactive shell. Confidence: high for "does not auto-open", low for "survives" (needs a live run).
- `submitWorktree` invokes `workspace_create_project_worktree` and then calls only `onDone()` and `onClose()` (`app/src/ProjectFolderModal.tsx:114-134`).
- `onDone` only calls `reloadWorkspaces()` and bumps `worktreesVersion` (`app/src/App.tsx:5525-5533`).
- On the backend, `workspace_create_project_worktree` runs `git worktree add` and returns the parsed `Vec<WorktreeEntry>`. It creates no workspace (`app/src-tauri/src/worktrees.rs:572-615`).
- The doc comment on `workspace_open_worktree` says the first pane is a plain shell because cmux#5032 spawned the setup command as PID 1 (`app/src-tauri/src/lib.rs:6144-6149`). The layout is `single_terminal_layout(conn)` (`:6210`).

### Q6
**Answer:** Yes, at the code level. The dialog names every descendant, shows the descendant count and a live-session count, and lists by name any session rows that will be killed. The backend deletes the whole subtree and does not touch host directories. **Exception:** tmux/zellij sessions behind *session rows* (Phase 91) are killed on the host, and the dialog says so. Confidence: medium-high.
- `ConfirmDeleteWorkspace` renders `alsoRemoves {count}`, one `<li>` per descendant with a folder or branch icon and a live badge, `liveWarning {count}`, and `sessionWarning {names}` (`app/src/ConfirmDeleteWorkspace.tsx:41-120`).
- `workspace_delete` does the following (`app/src-tauri/src/lib.rs:8011-8086`):
  - collects `collect_subtree_ids`, a BFS with a visited set;
  - runs the runtime teardown for each id;
  - does one `retain` and one `persist`.
- The teardown only removes the browser webview, port watchers, PTY sessions and notes. It contains no filesystem removal of the workspace directories (`app/src-tauri/src/lib.rs:8110-8170`).

### Q7
**Answer:** All three cases are handled in code.
- **Disconnected host:** the pin is preceded by a best-effort `ensure_connected`. `DirPicker` shows any error and clears `loading` in `finally`, so the spinner cannot get stuck. The probe fails hard with "no live SSH session to …".
- **Nonexistent path:** the probe fails hard with "directory not found on the host: …" before anything is persisted.
- **Non-repo path:** pins in the demoted state with the `pf.pinned.noGit` toast. "Check for a git repository" promotes it through `workspace_set_project_root`.

Confidence: medium-high.
- `DirPicker.navigate`: `catch → setError(String(e))`, `finally → setLoading(false)` (`app/src/DirPicker.tsx:86-90`). The error renders at `:146-147`.
- `project_folder_probe` (`app/src-tauri/src/worktrees.rs:520-541`):
  - returns `Err` when the directory is missing (`:529-531`);
  - for SSH with no handle, returns `Err("no live SSH session … connect a pane there first")` (`:495-497`);
  - when git fails, returns `Ok(false)` (`:532-540`).
- `pinProjectFolder` runs the probe **before** `workspace_pin_project_folder`, so a failed probe persists nothing. Any error becomes an `err` toast (`app/src/App.tsx:1832-1852`).
- `recheckGit` → `git_probe_worktrees` → `workspace_set_project_root(true)` → `pf.checkGit.found` toast (`app/src/App.tsx:1863-1890`).
- Caveat: for SSH the user picks from a live SFTP listing, so the "NONEXISTENT path" case is only reachable through the typed-path modal (WSL) or a directory deleted between listing and pick (`app/src/App.tsx:1897-1912`).

### Q8
**Answer:** Yes, at the code level. Nesting is `parent_id` and collapse is `is_collapsed`. Both are fields on the persisted `Workspace`, and each mutation persists through the atomic `persist`. Confidence: medium.
- `workspace_set_collapsed` sets `ws.is_collapsed` and then calls `persist(&state)` (`app/src-tauri/src/lib.rs:6766-6785`).
- On load, `normalize_parents` repairs the parent links and `migrate_headers_to_screens` runs after it (`app/src-tauri/src/lib.rs:1086-1094`).
- The frontend's `onSetCollapsed` is wired from the Sidebar (`app/src/App.tsx:4649-4650`).

### Q9
**Answer:** Yes, at the code level, and unit-tested. `migrate_legacy_project_folders` reads `project_folders` off the raw JSON and does three things:
- creates an `is_project_root` workspace under the first root workspace on the same host (or keeps it as a root);
- carries over `is_collapsed`;
- re-parents every workspace whose legacy `project_folder_id` matches, back-filling `cwd` from `worktree_path`.

It runs first in the load chain, followed by `normalize_parents` and then `migrate_headers_to_screens`, which moves the migrated folder's layout into a `shell` screen. Confidence: high for the logic, medium for real-file behaviour.
- `app/src-tauri/src/lib.rs:7072-7208` (the migration), `:1083` (call order).
- Tests: `lib.rs:13505-13627`, including `assert!(folder.is_collapsed, "collapse state carries over")` at `:13551`.
- The CI run of these tests was not checked here, because Rule #17 bans running them locally.

## Recommendation
Do **not** close the FOLLOWUPS entry from this report. Static evidence does not count as "verified live" (CLAUDE.md: "Verified = real run, not compile"). Instead:

1. **Rewrite the smoke script before anyone runs it.** Steps (3)–(5) describe the pre-91.F/92 UI:
   - (3) Select the folder's `shell` screen. Expect a locked directory and the *This folder* scope, which falls back to *Whole server* when empty.
   - (4) Open the Diff pane (Ctrl+Shift+G), then click a worktree in the strip.
   - (5) Use the Diff strip's `+`.
2. **Decide Q5:** should creating a worktree auto-open its workspace, as the 2026-08-18 script expected? If yes, it is a small frontend change. After `workspace_create_project_worktree` resolves in `submitWorktree` (`app/src/ProjectFolderModal.tsx:122-128`), call `openWorktree(root, newEntry)`, which needs an `onCreated(entry)` prop next to `onDone`. If no, amend the script's expectation.
3. Then have Yossi run the revised 9 steps on a real build against an SSH box, and close the entry with the per-step result.

Trade-off accepted: this report gives a corrected checklist instead of a verdict. A live run that disagrees with any "implemented" claim above would override it.

## Open questions
- **Q1–Q9 live verdict:** every step still needs a run of a real build on a Windows (or macOS) desktop against a live SSH host. Only Yossi or a collaborator with the desktop can provide this. Nothing here was run.
- **Q1:** Does the overflow menu get clipped by the ancestor `.pane` (`overflow:hidden`) at narrow heights or widths? Commit `eafd907` reports a spill at 130px. Answered by: the live shrink test in LTR and in Hebrew.
- **Q3:** Does `pwd` equal the folder when attaching to an *already live* session? By design, the attach-only guard skips the `cd`. Answered by: live connect to a new session vs. an existing one.
- **Q5:** Is "creating a worktree does not open its workspace" intended (Phase 91.F) or a regression? Answered by: a decision from Yossi.
- **Q9:** Do the migration unit tests pass on CI at HEAD? Answered by: `gh run view` on the latest ci-windows run.
- Out of scope, noted: `state.workspaces.lock().unwrap()` in non-test commands (`app/src-tauri/src/lib.rs:6141`, `:8026`, `:8085`) conflicts with Absolute Rule #4.

## Sources
- `/home/runner/ymux/FOLLOWUPS.md:104` — the P1 entry and the 9-step script (grep "project folders v4")
- `app/src/PaneView.tsx:244-247, 750-772, 1033-1060, 1180-1279, 1483-1530, 2074-2086` — overflow fitter/menu, folder anchor, cwd override
- `app/src/App.css:963-975, 4060-4083` — header no-clip note, menu positioning
- `app/src/i18n/he.json:134` — Hebrew "more actions"
- `app/src/App.tsx:1832-1912, 1935-1950, 4630-4650, 5494-5533` — pin, recheck, openWorktree, DirPicker/modal wiring
- `app/src/DirPicker.tsx:66-91, 143-147` — SFTP listing, error/loading
- `app/src/ProjectFolderModal.tsx:114-134` — create worktree does not open a workspace
- `app/src/ConfirmDeleteWorkspace.tsx:1-120` — delete dialog content
- `app/src-tauri/src/lib.rs:1074-1094, 6066-6216, 6766-6785, 7072-7208, 8011-8170, 9335-9341, 13505-13627` — pin, open worktree, collapse, migration, delete cascade, cwd fallback, tests
- `app/src-tauri/src/worktrees.rs:485-615` — dir probe, project_folder_probe, git_probe_worktrees, create worktree
- `docs/vault/frontend-shell.md` § App (pinning), § Sidebar (Phase 91.F/92), § PaneView (scope toggle)
- `docs/vault/backend-core.md` § Spawning a shell (attach-only guard)
- `git show eafd907` → "overflow menu was clipped invisible; fitter ignored padding" (2026-08-17)
- `git log --oneline -S"pane-overflow-menu"` → `6ee80f6` introduced the menu
- No web sources used, because the questions are fully repo-internal.
