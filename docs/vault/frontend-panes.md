---
vault: frontend-panes
covers:
  - app/src/BrowserWindow.tsx
  - app/src/BrowserChrome.tsx
  - app/src/components/PopoutBrowser.tsx
  - app/src/BrowserPane.tsx
  - app/src/browserDevMode.ts
  - app/src/FileManagerWindow.tsx
  - app/src/FileManagerPane.tsx
  - app/src/FileEditor.tsx
  - app/src/fmPaths.ts
  - app/src/MarkdownViewer.tsx
  - app/src/mdViewerStore.ts
  - app/src/DiffPane.tsx
  - app/src/InsightsWindow.tsx
  - app/src/InsightsAnalytics.tsx
  - app/src/InsightsClaudeCost.tsx
  - app/src/HygienePanel.tsx
  - app/src/PortsWindow.tsx
  - app/src/TicketsPanel.tsx
  - app/src/TicketModal.tsx
  - app/src/QueuePanel.tsx
  - app/src/FeedPanel.tsx
  - app/src/NotificationCenter.tsx
  - app/src/AddonsWindow.tsx
  - app/src/SessionsOverviewWindow.tsx
  - app/src/AddonsTab.tsx
  - app/src/YmuxToolsTab.tsx
  - app/src/ClaudeUsageIndicator.tsx
  - app/src/claudeUsageFmt.ts
  - app/src/TransferBar.tsx
  - app/src/transferStore.ts
  - app/src/MobilePairing.tsx
  - app/src/HelpPane.tsx
  - app/src/components/PopoutTerminal.tsx
---

# Panes, windows, and panels

Everything the user opens that is not a terminal and not a wizard. Most of these ride
the shared `PanelSurface` lifecycle from `frontend-shell.md` — drawer → float →
fullscreen — so they get that chrome for free instead of hand-rolling a variant.

## Browser

The page itself is always a **native child Webview** the Rust side mounts
(`workspace_browser.rs`), at most one per workspace. Everything in these three files is
the HTML *around* it. Since Phase 85.C there are **two hosts** for that chrome, and the
split is the thing to understand first:

**`BrowserChrome.tsx` (801)** — the chrome, and the only place the native Webview
lifecycle lives: tabs, the port+path bar, Go, Dev Mode, the Web Inspector button, the
empty state, and the show/hide/navigate effects. Exactly two things differ between hosts
and both are injected — `slotRect()` (where the Webview goes, relative to its host
window) and `windowLabel` (which OS window to attach it to).

It is a **factory plus a body component**, not one component, and that is deliberate:
the state and effects must outlive the JSX. In the floating panel the host stays mounted
while the window is closed and only the inner `<Show>` collapses, and the falling-edge
close effect is what hides the Webview — put it inside the `<Show>` and it would unmount
before it could ever fire. `makeWindowControls` in `floatingWindow.tsx` is the same
shape. Hosts therefore call `createBrowserChrome(...)` unconditionally at their top
level and render `<BrowserChromeBody api={...}/>` wherever it belongs.

**`BrowserWindow.tsx` (216)** — the floating shell only: header drag, `ResizeHandles`,
persisted geometry, the X, and the pop-out button. `windowLabel="main"`, `slotRect`
derived from the drag geometry minus the chrome constants. Both header buttons are named
in `closeGuardSelector` or a click on them would start a window drag.

**`components/PopoutBrowser.tsx` (193)** — the same chrome in its own OS window, a
sibling of `PopoutTerminal.tsx` and built the same way (`index.tsx` bails to it on the
`browser-popout-<ws>` label before `<App>` mounts). Chrome here is tabs + port bar only —
the OS window supplies the title bar, the X and the resize grips — so `slotRect` is the
viewport minus 58px, tracked on a `resize` listener because there is no drag geometry to
key off. It is a **separate JS context and cannot read App's signals**, so it self-serves
the port list and forwards by invoking the same commands App does
(`workspace_ensure_port_watcher`, `list_detected_ports`, `forward_port_start`) and
filtering the app-wide `port-*` broadcasts by workspace id — exactly how PopoutTerminal
filters `pty:data`. Its `anyModalOpen` is hardcoded `false`: ymux's modals all live in
`main` and none of them covers this window.

The 🐞 button toggles Dev Mode (ticket capture); the terminal-icon button next to it calls
`workspace_browser_open_devtools` and opens the Web Inspector on the loaded page — on
macOS that saves a trip through Safari → Develop, and on Windows it is the *only* way in,
since F12 is not wired up for a child webview. **Only this webview is inspectable**: see
`backend-panes.md` for the child-vs-shell split, the two `.devtools(false)` calls in
`backend-core.md`, and the Cargo feature they guard in `build-glue.md`.

**Popping out reloads the page, and that cannot be fixed here** — a child Webview cannot
be re-parented, so `browser_popout_open` destroys and respawns. `backend-panes.md` has
the detail. `App.tsx` closes the panel BEFORE awaiting that command: do it after and the
panel's hide races the popped-out window's own show on the same workspace id, and a slow
hide blanks a window that will never call show again.

**The `shownWsId` invariant.** The chrome must hide the Webview belonging to the
workspace that is **actually on screen**, which is *not* the same as `p.workspace?.id`.
One writer (the show effect's `.then`, after the invoke resolves), one reader
(`hideShown`), and three callers: the workspace-switch effect (hide the **outgoing**
workspace before forgetting which it was), the show effect's no-URL branch, and the
falling-edge close effect. Phase 85.B: all three used to pass the *active* id, so opening
the Browser in ws A and switching to ws B hid B — a no-op — and left A's native Webview
painted over the slot region in every workspace until the app exited. X did the same
thing, so there was no way to clear it. The close effect also no longer requires an
active workspace; that guard was a second leak.

**Chrome errors need their own strip.** `navError` renders only inside the empty state
(`<Show when={!currentUrl()}>`), so it structurally cannot report a failure of an action
taken **on a loaded page**. `chromeError` (Phase 85.A) is that channel, and it lives in
the port-bar row rather than over the slot because the native child Webview paints over
any HTML in the slot. The DevTools button spent a release cycle broken with `log.warn`
as its only failure channel; anything in the chrome that can fail while a page is up
reports here.

**`browserDevMode.ts` (448)** — right-click an element in the workspace browser to
capture it as a ticket. Kept out of the chrome on purpose: with Dev Mode in its
own module, the browser component gains one signal, one toolbar button, and one
re-inject effect, and nothing about tabs or navigation changes.

**`BrowserPane.tsx` (546)** — the pre-Phase-53 in-pane Browser. **Not imported by
`LayoutView` any more**; kept as reference for its in-pane Webview wiring. Do not wire
it back up without reading the WebView2 single-environment constraint in
`backend-panes.md`.

## Files

**`FileManagerWindow.tsx` (151)** wraps **`FileManagerPane.tsx` (1,645)** — the dual
column local + remote SFTP manager — in the same drag/resize chrome as BrowserWindow.
Pure HTML, no native Webview, so the only persistence concern is geometry.

**`fmPaths.ts` (112)** remembers the last directory each column showed, per workspace, in
localStorage — so re-opening lands where the user left off instead of snapping to
`$HOME`. localStorage for the same reason as `sessionRestore.ts`: per-machine state that
changes on every navigation click, where loss costs one click.

**`FileEditor.tsx` (552)** — a modal with a monospace `<textarea>`, Save / Cancel /
Reload, and an unsaved-changes guard. **Syntax highlighting is deliberately out of
scope**: this is "view the file, fix a typo, save", not a code editor.

**`MarkdownViewer.tsx` (118)** + **`mdViewerStore.ts` (31)** — double-clicking a `.md`
in the File Manager opens it here instead of the OS app. **Security:** `html: false`
makes markdown-it drop raw HTML at parse time, and DOMPurify scrubs the rendered output
as a second layer.

**`TransferBar.tsx` (149)** + **`transferStore.ts` (207)** — in-flight SFTP transfers.
A module signal rather than prop-threading, because transfers start from several places
(the File Manager, terminal drag-drop, OSC 8 link downloads) and one listener at the App
root feeds them all into a single list.

## Monitoring

**`InsightsWindow.tsx` (586)** — pull-based server monitor and the tab host. Fetches the
live snapshot through the `insights_fetch` Tauri command, which curls `127.0.0.1:7879`
over the workspace SSH session — **or serves it from `insights_local.rs` for a local
workspace; the routing is transparent to this component.** No mock data: if the daemon is
not installed or not running, the panel says so. Tabs: metrics, analytics, mobile, logs,
health, claude. It threads one `local` prop down from `App.tsx`
(`connection?.type === "local"`), used only by the copy-the-commands blocks so they can
name the right paths.

**`InsightsAnalytics.tsx` (623)** — the Analytics tab: what the server has *been* doing,
as opposed to Metrics' what-it-is-doing-right-now. A thin view over the `/analytics`
endpoint (`server-go.md`), which rolls up the three sampler tables the daemon was already
writing and nobody read. Range picker is 1h / 6h / 24h / 7d. Three house rules it keeps on
purpose, and the same three apply to the Claude cost panel below:

- **No polling.** This is an analysis screen. It loads when the tab opens and when Refresh
  is pressed; Metrics owns the live view. Each fetch is a curl over the workspace SSH
  session, so a background poll here would be rude.
- **All aggregation server-side, one round trip.** Never pull raw rows and sum them in the
  client — a 7-day window is ~120k samples.
- **Colour means status, not category.** Every bar is the accent colour, because a bar
  encodes magnitude; red/amber are reserved for "this is a problem" (disk filling, a
  container that keeps dying). Otherwise it turns into a rainbow where nothing stands out.

**`InsightsClaudeCost.tsx` (459)** — sits under the quota bars in the Claude tab. The bars
answer "how much allowance is left"; this answers "where did it go", from the token counts
`/claude-usage` reads out of Claude Code's own transcripts. **The cost column is an
estimate and the UI says so in three places**, because the failure mode is somebody
reading it as a bill: Claude Code on a subscription is not billed per token, these are API
list prices applied to real counts. Right for comparing projects, models and sessions
against each other; wrong for predicting a charge. Prices come from `claudePricing.ts`
(`frontend-lib.md`) — the server deliberately never prices anything.

**`HygienePanel.tsx` (159)** — the Monitor's Cleanup tab. Surfaces the two server-side
leaks Yossi hit (duplicate ymux port-watchers, orphaned claude sessions) from the
daemon's `/hygiene` endpoint, and reaps the safe ones via `/hygiene/kill`. Phase 86: a
port-watcher row also carries `orphan` (ppid=1, its SSH channel gone), rendered like a
duplicate and reaped by the same button — "Kill duplicates & orphans".

**`PortsWindow.tsx` (267)** — detect-only plus click-to-forward. The remote watcher
reports a LISTEN port → a row appears with **[Forward]** → the backend opens the tunnel
(with a TCP sanity probe first, so a dead bind never reaches the browser) → the row
flips to **[Open] [Stop]**. Stop tears the tunnel down; the row reverts to detected-only,
or disappears when `port.closed` fires.

**`MobilePairing.tsx` (428)** — the Monitor's Mobile tab. Drives the nginx-proxy install
and the daemon's pairing endpoints via the `mobile_pairing_*` commands. Host and port are
used **only** to render the URL card.

## Agent surface

**`NotificationCenter.tsx` (170)** — unifies the two notification streams (OSC 9/99/777
from terminals, RPC/agent notifications from Claude hooks) into one filterable,
read-aware timeline. The item list and read set live in `App.tsx`; this component is
presentational and owns only the active filter. Each item carries its originating pane
when known, so a click lands on the exact pane.

**`FeedPanel.tsx` (191)** — the allow/deny cards. The feed mixes cards from every
workspace and session, so each card resolves its owning workspace (by `pane_id` →
layout, falling back to `workspace_id`) and can be filtered or grouped by it. Kind /
subkind / state codes are translated, falling back to the raw code for any value without
a key.

**`TicketsPanel.tsx` (366)** + **`TicketModal.tsx` (354)** — workspace-scoped ticket
list, and the dialog that finalizes a captured element into a ticket on disk. **The
capture came from an untrusted page**, so the element HTML is rendered as **text, never
as markup**, and the preview is collapsed by default.

**`QueuePanel.tsx` (BRIEF)** — the cross-workspace agent Queue: every agent pane
grouped by workspace with a count and a "needs you" badge, rows sorted by
`queueModel.ts`'s buckets (blocked/stuck → waiting-for-you → done/closed →
running), each row = status emoji (🔄 ⏸️ ⚠️ 💤 ✅) + pane title + the
"what's happening" line (`got from you: <last prompt>` while running; `ask · rec`
after a stop) + age. **Owns no verdicts** — App hands it the same
`allPaneAgentRows()` the sidebar derives from; this file only paints (all brief
text renders as plain text with `dir="auto"`, never markup — it is agent
output). Row click = `handleSetActive` + `focusPane`; a drawer closes itself on
jump. Rides the shared PanelSurface lifecycle like Tickets.

**`ClaudeUsageIndicator.tsx` (181)** + **`claudeUsageFmt.ts` (120)** — the always-visible
subscription-usage chip. With room it shows session · week · top model; narrow, it
collapses to the single most-critical metric with the rest in the tooltip, one per line,
reset times converted to the viewer's **local** timezone.

## Per-workspace management

**`AddonsWindow.tsx` (81)** wraps **`AddonsTab.tsx` (124)** — add-ons live on the remote,
so they are managed per workspace, opened from the workspace's right-click menu and from
the Insights monitor's install prompt. **`YmuxToolsTab.tsx` (126)** is the same shape for
skills. Both are self-contained specifically so they do not bloat `SettingsModal`.

**`SessionsOverviewWindow.tsx` (462)** — Phase 87, the workspace right-click
**Active sessions…** dialog. A plain `.modal` stretched to the viewport (not a
`PanelSurface`: it has no drawer/float life, it is a full-screen table you open, act in,
and close), header pattern from `PortsWindow`, table class from the Monitor
(`.ins-an-table`). **Two round trips on purpose:** `pane_list_tmux_sessions` with
`projectPath: null` renders the table at once; `sessions_overview_summarize` then runs in
chunks of 10 names and fills the status pill + summary column as it lands (10-30 s per
chunk — it is `claude -p` on the machine). A request counter drops a late answer after a
refresh, so a stale summary never lands on a fresh row; exited (zellij) rows are never
sent. Rows group by `cwd ?? owner_cwd`, the unplaceable ones last under one "unknown
folder" heading; the name column shows `label ?? auto_name ?? claude_title ?? name` with
the raw name beneath. Row actions are props the window does not implement: **Open**
(87.B: hands App the whole row — name, display name, `cwd ?? owner_cwd` — and App opens the
session on a screen of its own, a child workspace row in the tree; nothing here splits),
**Rename** (inline input, `^[A-Za-z0-9_-]{1,64}$` checked here AND in the backend,
disabled on zellij with the reason in the tooltip) and **Kill** (two clicks, the button
re-arms after 3 s). Summaries are screen-derived content: rendered, never passed to
`log.*` (Rule #1).

**`DiffPane.tsx` (341)** — on mount it tells the backend the persisted source (or
`Working`), which restarts the per-pane watcher task; the watcher emits
`diff-pane-updated` and this filters by `pane_id` and re-renders.

**`HelpPane.tsx` (96)** — renders bundled markdown (currently ssh-key-setup) keyed by
topic and UI language, with a Copy button on every fenced block.

**`components/PopoutTerminal.tsx` (136)** — the pop-out terminal window. Ctrl+wheel font
zoom applies to popouts only (the grid stays Settings-driven); all open popouts share one
zoom level, synced via the `popout:zoom` event and persisted in localStorage.

## Invariants

- **Rule #5** — no `any`.
- Content from a page, a remote file, or a transcript is **untrusted**: render as text,
  or sanitize. `TicketModal` and `MarkdownViewer` are the two reference implementations.
- A panel that can float must go through `PanelSurface`, not its own chrome.
- Insights payloads must stay shape-compatible between the remote daemon and
  `insights_local.rs` — this panel parses one shape.

## Read the source when

You need a component's exact props, the Insights JSON field names, or the ticket schema.
The backends are in `backend-panes.md` and `backend-remote.md`; the daemon is in
`server-go.md`.
