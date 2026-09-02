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

## `App.tsx` (4,954) — one component, ~50 signals

There is a single `function App()` starting at line 142 and it holds essentially all
application state as `createSignal` pairs: `file` (the whole `WorkspacesFile`),
`activePaneId`, `maximizedPaneId`, `panels`, `notifications`, `feedItems`, `settings`,
`notes`, `paneStatus`, `agentRuns`, `portForwards`, `detectedPorts`, `sidebarWidth`,
`zoomFactor`, the pending-credential signals (`pendingPwFor`, `pendingPassphraseFor`,
`pendingHostTrust`), and the various modal/window toggles.

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

## `Sidebar.tsx` (1,286)

Workspace tree with groups, nesting, pinned project folders, and worktree children.
Drag-reorder, collapse state, the per-workspace action row (🌐 Browser, 🗂 Files,
notes, settings, add-ons), and forwarded-port rows. Reads `Workspace`,
`WorkspaceGroup`, `WorktreeEntry`, `ForwardRow` from `types.ts`.

The workspace right-click menu is a fixed-position `.ws-menu` whose items all funnel
through one `onAction(id, action)` prop with a closed string union — rename, edit,
**sessions** (Phase 90, above add-ons on purpose: it is opened several times a day),
addons, pin folder, check git, move-to-group, disconnect, delete. Adding an item means
adding a union member here and a branch in `App.tsx`'s handler; the menu itself owns no
state beyond which row it is open for.

Row glyphs: `is_project_root` → folder + git badge; **`tmux_session` (Phase 90.B) → a
terminal icon**, tooltip = the raw session name; else the colour dot. A session row is
otherwise a plain child — click, collapse, drag, delete all take the same path.

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
`killSessionByName` routes through the existing `killSession(paneId)` when
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

## `PaneView.tsx` (2,194) — one terminal pane

Owns a `TerminalInstance` (see `frontend-lib.md`), the connect/disconnect UI, the
session picker (tmux/zellij sessions, Claude sessions), pane title and annotation
editing, the persistence toggle, and the right-click menu. `paneCaps()` /
`profileFor()` / `effectiveIdentity()` from `types.ts` decide what a pane can offer
based on its effective connection.

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

**`paneTitle.ts`** — the pane display-label precedence
(`title → auto_title → workspace name → connection`), lifted out of PaneTabs so
the tab strip, the Queue panel and the Briefing card call one function.

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
