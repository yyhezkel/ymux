# ymux-2026-09-02-manual — research report

## Question
The active-sessions overview (Phase 90, `SessionsOverviewWindow.tsx`) lets the user tick which multiplexer sessions get an LLM summary, but that selection lives only for one open of the dialog (90.C, PR #41). The BACKLOG item asks how to persist it per host → session name, following the `session-owners.json` keying precedent, so the choice survives reopening. This report maps where the state lives now, how the precedent store is keyed and written, what a new store would look like, which surfaces would change, and how renames/removals would leave stale keys.

### Q1 — Where is the summarize-selection state held today, and what resets it on each open
### Q2 — How is `session-owners.json` keyed and persisted (file, key shape, atomic write), and does it match the host → session name key needed
### Q3 — What storage location and format would a per-host selection store need (reuse of the owners file vs. a new file)
### Q4 — Which frontend and backend surfaces must change to load and save the selection on open and on toggle
### Q5 — How does a session rename or removal affect a stored selection (stale-key handling)

## Findings

### Q1
**Answer.** The selection is a component-local SolidJS signal, `selected: Set<string>` of raw session names, in `SessionsOverviewWindow.tsx`. It is cleared to an empty set every time the dialog closes, so each open starts with nothing ticked. Nothing is written to disk, to localStorage, or sent to the backend.

**Evidence.**
- `app/src/SessionsOverviewWindow.tsx:84`: `const [selected, setSelected] = createSignal<Set<string>>(new Set<string>());`. The selection is keyed by `s.name`, the raw multiplexer name and not the display name (`:131-147`, `toggleSelected(s.name)` at `:419`).
- `app/src/SessionsOverviewWindow.tsx:195-209`: an `on([p.open, p.workspaceId])` effect. When the dialog opens it calls `load()`. On `!open` it calls `setSelected(new Set<string>())` (`:204`). **This is the reset.**
- `app/src/SessionsOverviewWindow.tsx:180-182`: within one open, `load()` (a refresh, or the reload after a rename at `:290`) keeps only the names that still appear in the fresh list: `setSelected((prev) => new Set([...prev].filter((n) => live.has(n))))`.
- Design intent is stated in three places: the header comment at `app/src/SessionsOverviewWindow.tsx:16-21` ("The choice is per open (v1); persisting it per host is a BACKLOG item"), `docs/DECISIONS.md:805-807` (90.C addendum), and `docs/vault/frontend-panes.md:289-298`. The phase landed in commit `5d681167` (`git log -S"Summarize selected"`).
- The component is mounted once in `app/src/App.tsx:5478-5500` with `open={!!sessionsWin()}`. It stays mounted while closed, so the reset comes from the `!open` branch above and not from an unmount.

**Confidence.** High. Single file, read directly.

### Q2
**Answer.** `session-owners.json` sits in the app config dir (`%APPDATA%\ymux` on Windows). Its shape is `{ version: 1, owners: { <host_key>: { <session_name>: SessionOwner } } }`. Writes are atomic: tmp file, then fsync, then rename (Rule #7). The host key comes from the workspace's `Connection`:
- SSH → `user@host:port`
- WSL → `wsl:<distro|default>`
- Local or no connection → `"local"`

That is exactly the **host → session name** shape this item needs, and the key is derived server-side from a workspace id, so callers never build it themselves.

**Evidence.**
- Schema comment and rationale ("Keyed by host FIRST because a session name is only unique per tmux/zellij server"): `app/src-tauri/src/lib.rs:10948-10967`.
- Structs: `SessionOwner { workspace_id, cwd?, ts }` at `lib.rs:10969-10980`. `SessionOwnersFile { version, owners: HashMap<String, HashMap<String, SessionOwner>> }` at `lib.rs:10982-10988`.
- Path: `config_dir()?.join("session-owners.json")` at `lib.rs:10990-10992`.
- Load: a missing, unreadable or corrupt file falls back to an empty default and logs a warning (`lib.rs:10994-11015`).
- Atomic save: a `session-owners.<pid>.tmp` file, then `write_all`, `sync_all` and `rename` (`lib.rs:11017-11042`). The comment says it is deliberately the same shape as `save_tmux_labels` (`lib.rs:10868-10890`).
- Host key: `session_owner_host_key(conn)` at `lib.rs:11044-11058`. The SSH arm delegates to `bootstrap_guard::host_key`, which is `format!("{user}@{host}:{port}")` (`app/src-tauri/src/bootstrap_guard.rs:152-156`). Its test asserts that different users and different ports give different keys (`bootstrap_guard.rs:163-167`).
- Zellij deletes release under the literal `"local"` key (`lib.rs:10150-10153`), which confirms that every Windows local pane maps to one host key.
- The other config-dir store, `tmux-labels.json`, is keyed by **workspace id** → session name (`lib.rs:10832-10833`), not by host. Owners is the right precedent for "per host"; labels is not.

**Match.** Yes. The overview is already scoped to one workspace's machine (`pane_list_tmux_sessions` with `projectPath: null`, `SessionsOverviewWindow.tsx:172-175`). That command resolves the same `session_owner_host_key` from the workspace id (`lib.rs:10338-10345`). One caveat: the key includes the SSH user. Two workspaces that reach the same box as different users therefore get separate selections, which is consistent with how ownership already behaves.

**Confidence.** High.

### Q3
**Answer.** Use **a new file** (for example `session-selection.json`, or a name chosen by PLAN) in the same `config_dir()`. Give it the owners schema with a set in the leaf: `{ version: 1, selected: { <host_key>: [<session_name>, …] } }`, or a `{name: ts}` map if age-based pruning is wanted. Load it with the same fallback-to-default behaviour and save it with the same tmp + fsync + rename. **Do not reuse `session-owners.json`**, for these reasons:
- **Different lifecycle.** An owner row exists only for sessions ymux claimed: it is created by `pane_connect` / `workspace_open_session` and pruned per workspace on workspace delete (`lib.rs:10962-10966`, `:11104-11117`, `:8424`). The overview, by contrast, lists *every* session on the host, including ones nobody claimed. A selection entry would therefore need rows with no `workspace_id`, which breaks the `SessionOwner` invariant. `prune_owners_of_workspace` keys on `workspace_id` (`lib.rs:11107-11110`), so a selection stored there would be destroyed whenever any workspace on the host was deleted.
- **Different readers.** `annotate_session_scope` loads owners on every picker list (`lib.rs:11191-11200`). Adding a field there means every picker list deserialises UI selection state it does not use. It also couples owner verdicts (`owned`/`foreign`) to a cosmetic preference, and Phase 91 had to untangle exactly that kind of coupling.
- **Copying the pattern is the established practice.** `save_session_owners` was written as a copy of `save_tmux_labels` "so the two files cannot acquire different durability behaviour by accident" (`lib.rs:11017-11019`). A third copy, or a small shared generic helper, follows the same pattern.

**Alternative considered: frontend localStorage.** App keeps per-machine UI geometry there (`app/src/App.tsx:156-158`, `:237-245`). It is a weaker fit:
- The host key is derived in Rust (`session_owner_host_key`), and the frontend would have to re-implement it, as a TS twin that can drift.
- The data would not get Rule #7 atomicity.
- The browser-served build (`ymux-web` artifact, CLAUDE.md CI section) has its own origin, so the selection would split by surface.

These are reasons to prefer the config-dir file. They are not measurements.

**Confidence.** Medium-high. The format is a design choice. The reasons against reuse are observed in code.

### Q4
**Answer.**

**Backend (`app/src-tauri/src/`):**
1. Add a new store: struct, path, load and atomic save next to the owners block (`lib.rs:10948-11160`), or inside `sessions_overview.rs`, which already holds the overview's commands (`sessions_overview.rs:411-412`, `:512-513`).
2. Add two commands, `sessions_selection_get(workspace_id) -> Vec<String>` and `sessions_selection_set(workspace_id, names: Vec<String>) -> Result<(), String>`. Each resolves the host key from the workspace's `connection` exactly as `tmux_rename_session` does (`lib.rs:11339-11347`). Register both in the handler list next to `sessions_overview_summarize` / `sessions_kill_by_name` (`lib.rs:13204-13206`). Tauri commands return `Result<_, String>` (Rule #6).
3. Add migration hooks (Q5) in `tmux_rename_session` beside `rename_session_owner` (`lib.rs:11436`), and in the kill paths beside each `release_session_owner` call (`lib.rs:10153`, `:12308`, `sessions_overview.rs:551`).

**Frontend (`app/src/SessionsOverviewWindow.tsx`):**
1. **Load on open.** In `load()` (`:156-191`), after `pane_list_tmux_sessions`, call `sessions_selection_get`. Seed `selected` with the stored names, intersected with the live list (the intersection at `:180-182` already does this).
2. **Save on toggle.** `toggleSelected` (`:132-138`) and `setManySelected` (`:139-147`) are the only writers apart from the resets. Either have them call `sessions_selection_set` (debounced, so that ticking a whole group is one write), or save once on close.
3. **The close reset at `:204` stays.** It clears the in-memory copy only, and the next open reloads from disk. The out-of-scope rule "do not change v1 per-open behaviour" conflicts with this whole item; see Open questions.
4. Update the header comment (`:16-21`) and the stale "per open (v1)" text.

**App.tsx:** no change is needed. The window already receives `workspaceId` (`App.tsx:5480`), which is everything the backend needs to derive the host key.

**Docs and gate:** `docs/vault/backend-core.md` (the owners and overview sections at `:383-392` and `:415-425`) and `docs/vault/frontend-panes.md:285-305` cover these files. The vault-drift gate (CLAUDE.md Rules #18/#19) needs both updated in the same change. `docs/DECISIONS.md:805-807` should get a follow-up line.

**Confidence.** High for the surfaces. The command names are proposals.

### Q5
**Answer.** Session names are the key, so three things leave a stored entry stale or wrong:
- **Rename through ymux.** This is handled today for owners and labels: `tmux_rename_session` calls `rename_session_owner` and `rename_tmux_label` (`lib.rs:11436-11437`). A selection store needs a matching `rename_session_selection(host_key, old, new)` at the same point, or the renamed session silently loses its tick. The frontend reloads after a rename (`SessionsOverviewWindow.tsx:290`), so a backend-side migration is enough.
- **Kill through ymux.** Owners are released on `killed | already_gone` in three places: `kill_pane_session_inner` (`lib.rs:12293-12309`), `sessions_kill_by_name` (`sessions_overview.rs:550-552`) and `zellij_delete_session` (`lib.rs:10153`). The selection should be released at the same three sites. Otherwise a later session that reuses the name (for example `dev`) inherits a stale tick. With pull-not-push, that costs one unwanted `claude -p` only when the user presses "Summarize selected", because nothing is automatic (`SessionsOverviewWindow.tsx:153-158`).
- **Rename or kill outside ymux** (`tmux rename-session` / `kill-session` in another terminal). No hook fires. Owners have the same gap: nothing prunes them except the explicit paths above and workspace delete (`lib.rs:10962-10966`). The cheap mitigation is an existing pattern, intersect-with-live on load (`SessionsOverviewWindow.tsx:180-182`). Stale names are then never shown or summarised, but they stay on disk. If on-disk growth matters, `sessions_selection_set` can **overwrite** the host's set with the intersected list on each save, which prunes on use. Do not prune on a failed or empty list: `load()` sets `rows([])` on error (`:185-187`), and pruning against that would wipe the selection.
- **Workspace delete.** This does not apply. The selection is per host and not per workspace, so the owners' `prune_owners_of_workspace` model (`lib.rs:11104-11127`) should **not** be copied. Deleting one workspace must not clear the selection that another workspace on the same host sees.
- **Zellij `exited` rows.** These cannot be ticked (`SessionsOverviewWindow.tsx:129-130`), and `summarizeSelected` filters through `selectableNames()` (`:148`, `:154`). A stored name for an exited session is therefore harmless, and it comes back into effect if the session resurrects under the same name.

**Confidence.** Medium-high. The paths are observed. The claim about out-of-band renames rests on there being no watcher; no grep found one, but this was not exhaustively proven.

## Options

| Option | Store | Cost | Risk | Evidence |
|---|---|---|---|---|
| A — new config-dir file, owners-shaped, host-keyed | `session-selection.json` | Copy of load/save (~60 lines), 2 commands, 4 hook calls (rename + 3 kills), frontend load/save | A third copy of the atomic-save code | `lib.rs:10994-11042`, `:11436`, `:12308`, `sessions_overview.rs:551` |
| B — add a field to `session-owners.json` | Reuse | Smaller diff | Breaks the `workspace_id` invariant, workspace delete wipes the selection, every picker read pays for it | `lib.rs:10969-10980`, `:11107-11110`, `:11191-11200` |
| C — frontend localStorage keyed by a TS-derived host key | localStorage | No backend change | Host-key derivation is duplicated in TS, not Rule #7 atomic, desktop and web origins split | `lib.rs:11044-11058`, `App.tsx:156-158` |

## Recommendation
Take **Option A**. Add a new `config_dir()/session-selection.json` with schema `{version, selected: {host_key: [session_name]}}`. Resolve the host key in Rust through `session_owner_host_key` from the workspace id, and save with the same tmp + fsync + rename as `save_session_owners` (`lib.rs:11020-11042`). Wire `rename_session_selection` next to `rename_session_owner` (`lib.rs:11436`) and a release at the three kill sites. In `SessionsOverviewWindow.tsx`, seed `selected` from the store inside `load()`, intersected with the live list, and save on toggle. Keep the close-time reset, because it only clears memory. Do not prune on workspace delete or on an errored list.

The trade-off accepted is a third copy of the atomic-JSON-store boilerplate. Extracting a shared generic helper for the three stores is a reasonable refactor, but it is not required here.

Two things would change this recommendation:
- Yossi says "muted" means an opt-out list. Then store a `muted` set instead and flip the default, which is a behaviour change to 90.C (see Open questions).
- The web bundle needs to run without the desktop's config dir. Then the store would have to live behind the daemon instead.

## Open questions
- **What does "muted" mean?** The request says "a muted noisy session stays muted", but in v1 the default is *nothing selected* (`SessionsOverviewWindow.tsx:16-20`, `docs/DECISIONS.md:806`). An unticked session is already "muted" on every open, so persisting only matters for the **ticked** set. If the intent is an opt-out list (auto-summarize everything except muted rows), that reverses the 90.C decision and is out of scope as written. To resolve: ask Yossi which set to persist.
- **The scope rule conflicts with the item.** goal.md says "do not change the v1 per-open behaviour", yet persisting the selection necessarily changes what an open starts with. Reading assumed here: the close reset stays and only the initial seed changes. PLAN should confirm.
- **Out-of-band renames and kills.** That nothing watches for them is inferred from grep, not proven. A grep for a session-rename watcher in `server/` and the CLI `session_meta.rs` would settle it.
- **Web bundle.** Whether `ymux-web` (served by the daemon) can reach `config_dir()`-backed commands is not verified here. Check `docs/vault/server-go.md` and `WEB-DESIGN §7.1`.

## Sources
- `app/src/SessionsOverviewWindow.tsx:16-21` — header: per-open v1, persistence in BACKLOG
- `app/src/SessionsOverviewWindow.tsx:84` — the `selected` signal
- `app/src/SessionsOverviewWindow.tsx:129-158` — summarizable filter, toggle/setMany writers, summarizeSelected
- `app/src/SessionsOverviewWindow.tsx:156-191` — `load()`, live-intersection at `:180-182`, `rows([])` on error
- `app/src/SessionsOverviewWindow.tsx:195-209` — open/close effect; reset at `:204`
- `app/src/SessionsOverviewWindow.tsx:290` — reload after rename
- `app/src/App.tsx:5478-5500` — mount site, `workspaceId` prop
- `app/src/App.tsx:156-158`, `:237-245` — localStorage per-machine UI pref precedent
- `app/src/App.tsx:3117-3122` — `renameSessionByName` → `tmux_rename_session`
- `app/src-tauri/src/lib.rs:10832-10933` — `tmux-labels.json` (workspace-keyed), atomic save, `rename_tmux_label`
- `app/src-tauri/src/lib.rs:10948-11042` — `session-owners.json` schema, structs, load, atomic save
- `app/src-tauri/src/lib.rs:11044-11058` — `session_owner_host_key`
- `app/src-tauri/src/lib.rs:11060-11145` — claim / release / prune-by-workspace / rename
- `app/src-tauri/src/lib.rs:8415-8424` — workspace delete → `release_session_owners_of_workspace`
- `app/src-tauri/src/lib.rs:10150-10153` — zellij delete releases under `"local"`
- `app/src-tauri/src/lib.rs:10315-10345` — `pane_list_tmux_sessions` resolves the host key from the workspace id
- `app/src-tauri/src/lib.rs:11325-11437` — `tmux_rename_session` and its migrations
- `app/src-tauri/src/lib.rs:12293-12309` — `kill_pane_session_inner` release
- `app/src-tauri/src/lib.rs:13204-13206` — command registration
- `app/src-tauri/src/sessions_overview.rs:411-412`, `:512-552` — summarize command, kill-by-name + release
- `app/src-tauri/src/bootstrap_guard.rs:152-167` — `host_key` format + test
- `docs/DECISIONS.md:805-808` — 90.C decision
- `docs/vault/frontend-panes.md:285-305`, `docs/vault/backend-core.md:383-392`, `:415-425` — covering vault pages
- `git log --all --oneline -S"Summarize selected" -- app/src` → `5d681167 feat(sessions): Phase 90.C …`
- `git grep -n "session-owners\|SessionOwners"` → all call sites listed above; no session-selection store exists yet
- Web: none used (repo evidence sufficient)
