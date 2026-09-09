---
vault: frontend-shell
covers:
  - app/src/index.tsx
  - app/src/App.tsx
  - app/src/Sidebar.tsx
  - app/src/LayoutView.tsx
  - app/src/PaneView.tsx
  - app/src/PaneTabs.tsx
  - app/src/AgentLight.tsx
  - app/src/paneAgentState.ts
  - app/src/queueModel.ts
  - app/src/paneTitle.ts
  - app/src/cwdShort.ts
  - app/src/BriefingCard.tsx
  - app/src/Divider.tsx
  - app/src/PanelChrome.tsx
  - app/src/PanelFloat.tsx
  - app/src/PanelSurface.tsx
  - app/src/SideDrawer.tsx
  - app/src/floatingWindow.tsx
  - app/src/paneDrag.ts
  - app/src/panels.ts
  - app/src/CommandPalette.tsx
  - app/src/WelcomeScreen.tsx
  - app/src/useNarrow.ts
  - app/src/icons.tsx
  - app/src/TechText.tsx
---

# Frontend shell — App, sidebar, layout, panes, panel chrome

**SolidJS**, not React. Signals and `createEffect`, no virtual DOM, no hooks rules.
`index.tsx` (121 lines) mounts `<App/>` — **unless the window label says otherwise**.
It is the whole router, and there is no other one: no query params, no `location.search`
anywhere in the tree. A built app's asset protocol serves a blank page for any suffixed
path (`index.html?x`, `index.html#x`), so every pop-out URL is a clean `index.html` and
the id rides the LABEL instead. Two prefixes bail before `<App>` mounts, so none of the
workspace/settings bootstrap runs in those windows:

| label | renders | id |
|---|---|---|
| `popout-<sid>` | `<PopoutTerminal>` | terminal session |
| `browser-popout-<ws>` | `<PopoutBrowser>` | workspace |

The browser prefix deliberately does NOT start with `popout-`, so the two checks cannot
collide — and neither can their capability globs, which are prefix-anchored too. The
xterm CSS and `App.css` imports at the top are global on purpose: a popout that skipped
them rendered unstyled, which read as a blank white window.

## `App.tsx` (5,566) — one component, ~50 signals

There is a single `function App()` starting at line 142 and it holds essentially all
application state as `createSignal` pairs: `file` (the whole `WorkspacesFile`),
`activePaneId`, `maximizedPaneId`, `panels`, `notifications`, `feedItems`, `settings`,
`notes`, `paneStatus`, `agentRuns`, `portForwards`, `detectedPorts`, `sidebarWidth`,
`zoomFactor`, the pending-credential signals (`pendingPwFor`, `pendingPassphraseFor`,
`pendingHostTrust`), and the various modal/window toggles.

**`refreshPersistence()` is the one place the wheel proxy is armed (Phase 91.D).** Every
refresh of `pane_persistence_list` fans out `ti.setTmuxScroll(!!m[pid])` over `terms`,
so a pane the backend lists as holding a tmux/zellij session gets the Shift+Up/Down wheel
proxy and every other pane keeps xterm's native wheel; `pty:exit` disarms synchronously
before the async refresh confirms it. See `frontend-lib.md` § mouse contract.

**Keyboard dispatch is one ordered table, not a chain of `if`s.** `keyBindings`
is a `KeyBinding[]` of `{ id, when?, run }` built once; `handleKey` walks it and the
first entry whose accelerator matches wins. Two rules in it are load-bearing and easy
to break:

- **`when` returning false means "skip and keep scanning", never "swallow".** That is
  how plain `Ctrl+B` (`toggle_sidebar_soft`) toggles the sidebar outside a terminal but
  reaches the PTY inside one — `Ctrl+b` is tmux's prefix, and stealing it would break
  every tmux binding.
- **`run` owns its own `preventDefault()`.** `copy` deliberately does not call it until
  `copyTerminalSelection()` resolves true, so a native text-selection copy still works
  in a non-terminal pane. A central `preventDefault` in the loop would kill that.

Accelerators come from `settings.shortcuts` via `shortcutTable()`, rebuilt on every
`settings:changed`, so a rebind in Settings takes effect without a relaunch. Before
Phase 87 roughly twenty of these were hardcoded `if` branches with no UI at all.

**Three things stay out of the table deliberately.** `Ctrl+1..9` (jump to tab N) is a
numeric family where 9 means *last*, and a `ParsedShortcut` holds exactly one key.
Bare `Escape` is a contextual *dismissal* — it acts only when a fullscreen panel or a
maximized pane exists and otherwise falls through so the escape sequence reaches the
PTY; making it rebindable would let a user strand themselves in a maximized pane. Both
run before the table so no rebind can shadow them. STT push-to-talk runs after it,
because it is stored in `settings.stt` rather than `settings.shortcuts`. All three are
listed read-only in Settings → Shortcuts so they are at least visible. Every
push-to-talk outcome now lands in the unified log: success as `log.info` with backend +
char count only (never the transcript — Rule #1; chars=0 catches the "pressed PTT,
nothing happened" case), and every recorder rejection as `log.error` before the 5-second
toast, so a missed toast is no longer a lost error.

**The workspace header keeps four buttons, not seven.** Browser, Files and the
notification bell (which carries the unread badge) stay visible; view mode, `+ diff`,
Insights and Tickets live behind a single `⋯` (`.ws-header-more` / `.ws-header-menu`,
sharing `.diff-pane-menu`'s CSS rather than a second dropdown component). Each item
calls the **same handler its old standalone button called** — `setTabsMode`,
`splitPane(pid, "horizontal", "diff")`, `openPanelConnected("monitor")`,
`openPanel("tickets")` — so the menu, the palette command `pane.viewMode.toggle` and
the keyboard path can never diverge. Click-away is a `createEffect` on `wsMenuOpen()`
listening for `pointerdown`, not `click`: a press landing in a terminal pane never
bubbles a click back to the header.

**State lives here and flows down as props.** Child components are mostly
presentational; when a child needs to mutate, it calls a handler App passed it. The two
deliberate exceptions are module-scope stores — `paneDrag.ts` and `transferStore.ts` —
where prop-threading through `LayoutView → SplitView → LeafPane → PaneView` was worse
than a module signal.

**The event subscriptions are the map of the backend↔frontend contract.** Around
lines 2778–3200, `App.tsx` registers `listen()` for: `pty:data`, `pty:exit`,
`ssh-disconnected`, CLI alignment, the feed (`FeedItem` + resolved), notifications,
`notes:changed`, `workspaces:changed`, `settings:changed`, `pane:agent-run` (the
per-pane Claude traffic light), hooks-outdated, and `update:available`. If you are hunting "who reacts to event X", it is almost always
here.

An `ErrorBoundary` wraps the tree — a thrown render error shows a recovery panel rather
than a white window.

**Pinning a project folder no longer requires git.** `pinProjectFolder` calls
`project_folder_probe` (hard error only for a missing directory or a dead SSH host),
then passes the verdict to `workspace_pin_project_folder` as `isProjectRoot`; a folder
without a repo lands demoted with an explanatory toast (`pf.pinned.noGit`) instead of
being refused with git's fatal message. `recheckGit` (the sidebar's "Check for a git
repository") still uses the always-fatal `git_probe_worktrees` — there, git's own
message IS the answer.

The Monitor mount passes `local={activeWs()?.connection?.type === "local"}` (Phase
84.E) — `InsightsWindow` needs it only to print the right file paths in its
"copy investigation commands" blocks; the fetch routing itself stays in Rust. The F12 /
Ctrl+Shift+I blocker near line 3184 is deliberate and survives the `devtools` Cargo
feature: the main window opts out of inspection because it renders live PTY output;
only the workspace Browser webview is inspectable (`frontend-panes.md` § Browser).

## `Sidebar.tsx` (1,597)

Workspace tree with groups, nesting, pinned project folders, and worktree children.
Drag-reorder, collapse state, the per-workspace action row (🌐 Browser, 🗂 Files,
notes, settings, add-ons), and forwarded-port rows. Reads `Workspace`,
`WorkspaceGroup`, `WorktreeEntry`, `ForwardRow` from `types.ts`.

**Headers + cards (Phase 91.E — the cmux look).** Every row is still one
`.ws-item[data-ws-id]` (drag/drop hit-tests `closest("[data-ws-id]")`, and the context menu
is shared), but there are two bodies. `isHeaderRow(w)` — a pinned folder, anything with
children, or a remote root without children (the machine itself, so its look never flips
with the sessions setting) — renders the pre-91.E row verbatim as a slim `.ws-header`
(chevron, glyph, dim small-caps-weight name, worktree chip, `+` worktree / rescan / `+`
session, the `.ws-meta` cluster). Every other row — a session row, a worktree workspace, a
local root without children — is a `.ws-card` from `renderCardBody(w)`: line 1 = the
`.ws-dot` / terminal glyph (hidden in `full`; it IS the card in icons mode), ✳ when
`cardInfo.agent`, the display name as `.ws-name.ws-card-title` (so per-string bidi and
`.ws-gone` dimming still apply), the `.ws-card-count` attention pill, the pane-count badge
only when `split` (the S/L/B/F letter is a header's business); line 2 = ONE indicator slot
(waiting > brief > activity, else the live dot — Design Pass 01 P3 kept) + the status text;
line 3 = `branchFor(w)` • `shortenCwd(cwd, sshUser)` forced LTR (a plain span — TechText
would pill the path); line 4 = `:port` links that keep the `.ws-port-badge` class because
that is the drag-start exclusion. `branchFor` reads the PARENT folder's worktree scan cache
by cwd prefix — never a round trip, never a new scan trigger; a card under a server root has
no branch, and there is no dirty `*` (git status is not known). All of it is fed by App's
`workspaceCardInfo` memo through the `cardInfo` prop (§ Sessions as rows); `cardInfoOf`
falls back to the row's own name / cwd and "idle". The workspace colour is a 3px
`.ws-card-stripe` (a real element — `::before/::after` are the drop lines), hidden on the
active card, whose look is the solid accent block with `--w-on-accent` text (computed by
`applyTheme` from the accent's luminance; themes-redesign.css keys its four per-direction
active rules on `.ws-header` only). Icons mode collapses a card to its glyph, with a
warning ring when `has-attn`; `[data-narrow]` drops the branch, not the cwd.

**Session rows (Phase 90.B / 91.C)** are cards; the ones in `goneIds` are `.ws-gone` and
their status line reads "gone — Connect resumes"; server and folder rows (headers) carry a
terminal-glyph `+` (`onNewSession`) while `sessionsAsRows` is on. See § Sessions as rows.

**"Only rows with live sessions" (Phase 91.A)** — a toggle under the wordmark
(`.sidebar-live-toggle`, `aria-pressed`, localStorage `ymux.sidebar.liveOnly`). The rule
is `isLiveTree`: a row stays if `connectedIds` has it (App's `liveWorkspaceIds()` — any
pane in `paneToSession`, no round trip), if it is the active workspace (the filter can
never hide what you are looking at), or if a descendant qualifies — so a server stays for
its live folder child. Applied to the ungrouped list, each group's members (a group with
none left is hidden, the count shows the visible number) and the children inside a
subtree; `.sidebar-live-empty` says so when nothing at all is live.

The workspace right-click menu is a fixed-position `.ws-menu` whose items all funnel
through one `onAction(id, action)` prop with a closed string union — rename, edit,
**sessions** (Phase 90, above add-ons on purpose: it is opened several times a day),
addons, pin folder, check git, move-to-group, disconnect, delete. Adding an item means
adding a union member here and a branch in `App.tsx`'s handler; the menu itself owns no
state beyond which row it is open for.

**Worktree scans are lazy, keyed by workspace id, and never polled.** A subtree's
effect runs `scanFolder` once when it is open and has no result; a scan that fails
with "no live SSH session" parks as `offline` (not an error row). The retry lives in
one effect over `liveSshHosts` — a memo that collapses `connectedIds` (a fresh Set on
every App tick) to the sorted `user@host:port` string of live SSH hosts — and rescans
only the parked folders whose own host is in that set, inside `untrack` so its own
`setScans` never re-fires it. Both constraints are load-bearing: the earlier version
retried on *any* live workspace and tracked `scans()`, so a local workspace up with the
folder's SSH host down produced a tight retry loop (eight scans in 30ms, 2026-09-08).
Local/WSL folders never park — the backend runs git directly for them.

Header glyphs: `is_project_root` → folder + git badge; **`tmux_session` (Phase 90.B) → a
terminal icon**, tooltip = the raw session name; else the colour dot. On a card the same
glyph is the icons-mode face only. A session row is otherwise a plain child — click,
collapse, drag, delete all take the same path.

**Phase 90 — the active-sessions overview's three row actions live in App, not in the
window**, because each needs App-level state. `openSessionAsWorkspace` (90.B) closes the
dialog and calls `workspace_open_session` — the session gets a **persisted child workspace
row of its own** under the machine or its project folder; the current screen is never
split or tabbed — then activates the row, and only if its single pane is not already live
(`paneToSession.has`, because `pane_connect` on a live pane kills and respawns) waits for
the mount and calls `connectPane(pid, { persistent, tmuxSession })`, the picker's shape, so
the attach-only guard guarantees nothing is typed. **Two fallbacks make the row honest after
a restart:** `connectPane` defaults `tmuxSessionName` / `persistent` to `ws.tmux_session`
for the workspace's FIRST pane (activation never auto-connects, so a plain [Connect] on the
row must attach, not spawn a pane-derived session; a split-off pane stays a plain shell),
and `restoreSessions` uses the same field when localStorage has no hint for that pane.
`newTab` still returns the new pane id from 87; nothing depends on it now.
**Deleting a session row kills its session (Phase 91):** `commitDelete` walks
`sessionRowsIn(subtree)` (every row with `tmux_session`), best-effort
`workspace_ensure_connected`, `killSessionByName`, toasts `workspace.delete.sessionKillFailed`
on anything but `killed | already_gone | no_session | attempted`, and only then calls
`workspace_delete`. `killSessionByName` routes through the existing `killSession(paneId)` when
`panePersistence()` shows one of our panes holding the name (PTY, maps and restore hint go
the tested way; `killSession` now returns the outcome for that), else
`sessions_kill_by_name`. `renameSessionByName` calls `tmux_rename_session` and then moves
the holding pane's restore hint (`rememberPaneSession`) — the backend migrates its own
maps, but the hint is frontend-owned and would otherwise name a session that no longer
exists on the next boot.

## `LayoutView.tsx` (372) + `Divider.tsx` (72)

Recursively renders `LayoutNode`: a `split` becomes two children plus a `Divider`, a
`pane` becomes a `PaneView` (or `DiffPane` / `HelpPane` by `PaneKind`). `Divider` drives
resize with `requestAnimationFrame` coalescing — `onDrag` during, `onCommit` at the end,
so only the commit hits the backend.

When the workspace has `tabs_mode` set, `PaneTabs` renders above `.layout-root` instead
of the grid. **Browser and File Manager are no longer pane kinds here.** Both moved to
workspace-level floating windows (sidebar 🌐 / 🗂). `BrowserPane.tsx` stays in the repo
as reference for its in-pane Webview wiring; `FileManagerPane.tsx` is still live, but
consumed by `FileManagerWindow.tsx`.

## Sessions as rows (Phase 91.C)

Round 1 of Phase 91 shipped a third view mode (a sessions strip above the terminal).
Yossi's first live run said the opposite of what it built — sessions belong in the sidebar
tree as rows, switched on from Settings for every server, and `+` must make a row, not a
tab — so the strip was removed the same day and this took its place. Everything keys on
the **root id, a string** — never on the workspace object, which changes identity on every
persist and would restart timers and re-fire guards (`rootIdOf`, `activeRootId` memo).

- **`refreshSessionRows(rootId, {ensure})`** (gated on `settings.sessions_as_rows`):
  `workspace_ensure_connected` when `sessionBound && ensure` → `pane_list_tmux_sessions(root,
  null)` → **`reachable = rows.length > 0 || !sessionBound`** (a cold password-auth host
  answers an EMPTY list, not an error — the `restoreSessions` precedent — so it greys
  nothing) → `sessionLists[rootId]` → `workspace_remember_sessions(root, …)` (the root's
  memory) → `workspace_mirror_sessions(root, candidates)`. **`mirrorCandidates`** drops
  `foreign.kind === "workspace"` rows and every name one of our own panes holds —
  `panePersistence()` (live) ∪ `allPaneSessions()` (restore hints) — because a session a
  plain pane holds is `owned`, not `foreign`, and a row for it would attach a second client;
  the backend applies the pane-derived-name rule too. `mirrorInFlight` coalesces overlapping
  refreshes. Triggers: (a) the active root on activation/boot, once per root
  (`lastMirrorRoot`), with `ensure` following Phase 41's auto-connect opt-out and an early
  return while `settings()` is null; (b) the post-connect 100 ms timer (a `+` or a resume
  CREATED a session); (c) a 30 s visible-only `setInterval` on `activeRootId()` — how a
  session killed elsewhere goes grey; (d) `commitDelete` after it touched session rows.
- **`goneWorkspaceIds()`**: session rows whose root's list is reachable and lacks the name
  (zellij `exited` rows are in the list = live). Sidebar prop `goneIds` → `.ws-gone` (dim,
  italic) + `ws.gone.tooltip`. Rows are never removed on their own; delete is the way out,
  and `commitDelete` skips the kill for a gone row (a password-auth host would only toast
  "kill failed") while always forgetting the name in the root's memory.
- **`boundSessions()`** — pane_id → `BoundSession { name, gone, claudeSessionId, cwd }` for
  the first NOT-live pane of a workspace with `tmux_session` (Claude id + cwd from the live
  row first, else the root's `known_sessions`). LayoutView threads it as `boundSession`;
  PaneView's `smartConnect` short-circuits: gone with a Claude id → `connectPane` with
  `mode: "claude"`, `claudeArgs: "--resume <id>"` (a STRING), `cwdOverride`, and the button
  reads "Resume"; otherwise a plain attach — `new-session -A` recreates a gone session of the
  same name. If the session reappeared between poll and click, `pane_connect`'s attach-only
  guard types nothing.
- **`workspaceCardInfo()`** (Phase 91.E) — one `createMemo` for the whole tree, workspace id →
  `WorkspaceCardInfo { title, status: {kind, text}, cwd, agent, attention }`, passed to the
  Sidebar as `cardInfo` and read by the CARD rows (the Sidebar renders inside `<For>` and must
  not create per-row memos). Title/cwd for a `tmux_session` row follow the live list of its
  root (`sessionDisplay(row)`, `cwd → owner_cwd`), else the root's `known_sessions`
  (`display`, `cwd`), else the row itself. Line-2 precedence, most urgent first: a blocking
  permission card → the text of an UNREAD notification (`notifications()` newest-first, by
  pane then by workspace; it clears itself on focus) → gone → the agent's most urgent
  `QueueRow` (`QUEUE_BUCKET` then oldest; needs-input/stuck/waiting = `agent-attn`, text =
  `whatsHappening`, else the status word) → connected → idle. `attention` counts panes that
  are waiting or unread plus needs-input/stuck rows not already counted. Re-runs on the
  250 ms agent clock; O(workspaces × panes + notifications).
- **`+` on a server / folder row** (`sessionsAsRows && !tmux_session &&
  wsCaps(w).sessionPersistence`, `IconTerminal`, `sidebar.newSession.tooltip`) →
  `newSessionRow(w)`: `<slug of w.name>`, `-2`, `-3`… past the live list, the root's memory
  and every `tmux_session` in the file → `openSessionRow(w.id, {name, display, cwd: w.cwd},
  false)` — `workspace_open_session` (placed under the folder by cwd) → `handleSetActive` →
  focus the first pane. **Phase 91.G: `autoConnect` is FALSE for `+`** — the fresh row lands
  on its disconnected overlay rather than blind-connecting. Blind-connecting spawned a bare
  shell in `$HOME` and made that creating connect the attach-only case, so the folder `cd`
  and any command the wizard then offered were both dropped (Yossi's report). Now the pane's
  own [Connect] (a plain shell in the folder) or connect wizard (a command in the folder) is
  the CREATE; the attach-only guard's reachability probe sees the name is not live and lets
  the injection through. `openSessionAsWorkspace` (Open an existing session) still passes
  `autoConnect` true — that session IS live, so attach-only is correct.
- Setting OFF: rows stay (ordinary workspaces); refreshes, greying and `+` stop. ON with
  rows already opened by hand: the host-wide dedupe skips them. Two roots to one host: rows
  land under whichever refreshed first. The live-only filter hides mirrored (and grey) rows
  until they connect — that is its contract. A row's name is fixed at creation.

## `PaneView.tsx` (2,194) — one terminal pane

Owns a `TerminalInstance` (see `frontend-lib.md`), the connect/disconnect UI, the
session picker (tmux/zellij sessions, Claude sessions), pane title and annotation
editing, the persistence toggle, and the right-click menu. `paneCaps()` /
`profileFor()` / `effectiveIdentity()` from `types.ts` decide what a pane can offer
based on its effective connection.

**The connect wizard probes for a live session before offering a command.**
`openNewConnModal` calls `pane_target_session_state` and disables the command controls
(`attachOnly()`) when the target session is already running — the client half of the
attach-only guard (`backend-core.md`). **Phase 91.G**: the probe's name is
`p.tmuxSession ?? p.boundSession?.name` — a session row whose pane is not locally attached
has no `panePersistence` entry, so without the `boundSession` fallback the probe asked
about the derived `ymux-<paneid>` name, reported "not live" for a session alive on the
host, and the wizard would have enabled a command the backend then dropped.

**The tmux picker's scope toggle owns no data.** *This folder* vs *Whole server* is a
client-side filter over one response — `inWorkspaceScope = s => s.owned || s.in_cwd`,
against rows the backend already annotated (`backend-core.md` § Which folder a session
belongs to). No second round trip, and a count line keeps the hidden ones visible as a
number so a scoped list never reads as an empty server. `pickScopeDefault` opens on
*Whole server* when the folder view would be empty.

Each row carries a `📁` badge when `s.foreign` is set — the session belongs to another
workspace or another directory. The verdict is entirely the backend's and is **never**
set inside the workspace's own scope, so the badge needs no view conditional here; this
file only picks the wording (`foreign.kind` chooses between the workspace and folder
sentences) and appends the full path to the tooltip. It marks, it does not block:
clicking still attaches.

## Tabs and the agent traffic light

**`PaneTabs.tsx` (155)** — the tab strip shown when a workspace has `tabs_mode`. **Owns
no state:** the tab list is the layout tree's leaves in DFS order, the active tab is
`activePaneId`, and selecting a tab is focusing a pane. Reordering came free — each tab
carries `data-pane-id`, which is what `paneDrag` already resolves drop targets against,
so the existing drag store, ghost and `workspace_swap_panes` apply unchanged. The mode is
a flag on `Workspace`, not a `LayoutNode` variant; `crates.md` has the reasoning.

**`paneAgentState.ts` (102)** — **pure and Solid-free on purpose.** `trafficLight()` is
the single verdict that both the pane header and the tab strip call, so the two cannot
disagree about what colour a pane is. Unit-tested in `paneAgentState.test.ts`. It only
decides how to *paint* a state; the transition table is owned by the backend
(`PaneAgentState::apply_hook` in `lib.rs`, arriving as the `pane:agent-run` event) — see
`backend-core.md`.

**`AgentLight.tsx` (45)** — paints it. Green = Claude is working, yellow = it finished and
it is your move, red = it is blocked on you, **nothing at all = unknown**, which is the
honest answer for a plain shell pane, a disconnected pane, or state old enough to be
untrustworthy. It uses **shape as well as hue** (disc / ring / triangle) so it survives
greyscale, 8px, and red-green deficiency.

**`queueModel.ts` (BRIEF)** — the pure model behind the Queue panel: `queueStatus`
(needs-input / stuck / waiting / working / done / ended — live hook state always
outranks a brief for placement), `QUEUE_BUCKET` (who-needs-you sort order),
`whatsHappening` (running rows show the user's last prompt, ended rows show
`ask · rec` → delta → next), and `groupQueueRows` (group by workspace, the
reference-table "CRM — 5" shape). Solid-free and unit-tested in
`queueModel.test.ts`, same reasoning as `paneAgentState.ts`. App.tsx builds its
input rows in `allPaneAgentRows()` — the generalization of `paneAgentLights()` to
every workspace; the active-workspace lights, the Queue panel and the sidebar
attention set (`queueAttentionWorkspaceIds`, a fifth Sidebar prop that shares the
row's one dot as `.brief-attn`, precedence blocking > brief > activity) all derive
from those rows, so they cannot disagree. Per-pane brief entries live in the
`briefs` signal, mirrored off `pane:brief` (seq-guarded like `pane:agent-run`)
and hydrated by `pane_briefs`.

**`paneTitle.ts`** — `sessionDisplay` (label → auto_name → claude_title → name, Phase 91,
shared by the strip, the overview and "Open") and the pane display-label precedence
(`title → auto_title → workspace name → connection`), lifted out of PaneTabs so
the tab strip, the Queue panel and the Briefing card call one function.

**`cwdShort.ts`** (Phase 91.E) — `shortenCwd(path, sshUser, maxLen = 34)` for the card's path
line: `/home/<u>` (the connection's user, or any user when there is none), `/root` for an
ssh root login, `/Users/<u>`, `<X>:\Users\<u>` → `~`; still too long → `…/<parent>/<leaf>`.
Import-free on purpose so `cwdShort.test.ts` runs under plain `node --test`.

**`BriefingCard.tsx` (BRIEF)** — the workspace-entry card: 🎯 intent (inline edit
→ `workspace_set_intent`, Enter/blur saves, empty clears) + this workspace's
brief rows (the Queue's row markup verbatim). Its `briefingWs` signal is **in
`anyModalOpen()`** — the native Browser webview paints over it otherwise. Three
triggers, all but the last opt-in via `settings.brief`: **return-after-absence**
lives INSIDE `handleSetActive` and reads `last_active_at` off the pre-switch
`file()` — `workspace_set_active` stamps it to "now" (in SECONDS) before
returning, so an effect running after the switch would always measure zero
absence; **idle-return** stamps `lastInputMs` from capture-phase passive
pointer/key/wheel listeners and arms on the existing 250ms `pulseTick` (no
second timer), firing on the first input after the gap; **manual** =
`show_briefing` (Ctrl+Alt+Q) + the palette, which work regardless of the
toggles.

## Panel chrome — "one body, three surfaces"

Every side panel (Notifications, Monitor, Files, Diff, Tickets) shares one lifecycle:
docked drawer → floating window → fullscreen overlay. Four small files implement it:

Panels get their workspace context as props from here rather than reading it themselves —
`<InsightsWindow>` for instance is handed `workspaceId`, `workspaceName`, and
`local={activeWs()?.connection?.type === "local"}`, because App owns `file()` and a panel
that re-derived the active workspace would be a second source of truth.

- **`panels.ts` (26)** — the state vocabulary. A per-panel `Surface`; `closed` means
  not shown. `App.tsx` drives it.
- **`PanelSurface.tsx` (99)** — given a surface, render the right chrome. `body` and
  `headerActions` are **thunks**, because `Switch` mounts one arm at a time and the body
  must be freshly created per surface. A panel that must keep fetched data across a
  surface change keeps that data outside the thunk.
- **`PanelChrome.tsx` (81)** — the shared header for the non-drawer surfaces. Which
  buttons appear is driven purely by which callbacks are passed: ⇤ dock, ⛶ fullscreen,
  ⤢ float, ✕ close. The actions cluster carries `.panel-chrome-actions` so the drag
  guard can ignore mousedowns on buttons.
- **`PanelFloat.tsx` (81)** — the floating surface: geometry (persisted per storage key)
  plus drag and 8-way resize.
- **`SideDrawer.tsx` (145)** — the docked surface: click-away backdrop, panel pinned to
  `inline-end`, its own header.

**`floatingWindow.tsx` (211)** is the shared mechanics under `PanelFloat` and the
Browser/File-Manager windows: drag + eight resize handles, with min-size clamping that
**keeps the opposite edge pinned** when dragging a top or left handle. The geometry
signal and its persistence stay owned by each window (different localStorage keys,
different min sizes); this module is pure mechanics over a passed-in signal.

## `paneDrag.ts` (212)

Module-scope pointer-drag store for pane reorder. Module scope is the point: the dragged
pane shows `.pane-dragging` and **every other** pane shows `.pane-drop-target`, so the
state has to be readable by all `PaneView` instances without threading props through
four layers.

## Smaller pieces

- **`CommandPalette.tsx` (125)** — Ctrl+Shift+P. The command list comes from `App.tsx`,
  so each command calls **the same handler the existing UI uses**. It is a second entry
  point, never a reimplementation. A command with a predicate returning false is hidden
  (e.g. pane commands with no active pane).
- **`WelcomeScreen.tsx` (54)** — the zero-workspaces state. Pure presentational; every
  action reuses an existing flow.
- **`TechText.tsx` (82)** — wraps technical tokens in `<code><bdi>…</bdi></code>` inside
  RTL contexts, so "edit ~/.ssh/config" reads correctly in a Hebrew sentence. **Does not
  touch xterm.js** — that is the PTY-side bidi filter, a different mechanism.
- **`icons.tsx` (176)**, **`useNarrow.ts` (31)** — inline SVG icons, and the narrow-
  window media query several components collapse on.

## Invariants

- **Rule #5** — no `any`. `unknown` and narrow, or define the type. `invoke` return
  types are always explicit.
- **Rule #9** — `createLogger(tag)` from `logger.ts`, never raw `console.*`.
- Per-machine, high-churn UI state (window rects, sidebar width, last directories,
  session-restore hints) goes to **localStorage**, deliberately — it keeps Rule #7's
  atomic-write surface small. `workspaces.json` stays the layout's source of truth.
- A command exposed in the palette must call the same handler as its UI entry point.

## Read the source when

You need a specific effect's dependency list, the exact props of a component, or the
CSS class names. Test files (`*.test.ts`) are intentionally **not** covered by this
vault file — a test edit should not trip the freshness gate.
