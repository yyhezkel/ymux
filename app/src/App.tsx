import { createEffect, createSignal, ErrorBoundary, onCleanup, onMount, Show } from "solid-js";
import type { RtlProfileKind } from "./types";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Sidebar } from "./Sidebar";
import { CreateWorkspaceModal } from "./CreateWorkspaceModal";
import { NotificationCenter, NotifHeaderActions, type NotifItem } from "./NotificationCenter";
import { WelcomeScreen } from "./WelcomeScreen";
import { LayoutView } from "./LayoutView";
import { PaneTabs } from "./PaneTabs";
import { trafficLight, type PaneAgentState, type TrafficLight } from "./paneAgentState";
import type { PaneAgentSnapshot } from "./bindings/PaneAgentSnapshot";
import type { PaneBriefEntry } from "./bindings/PaneBriefEntry";
import { QueuePanel } from "./QueuePanel";
import { inQueue, queueStatus, QUEUE_BUCKET, type QueueRow } from "./queueModel";
import { paneLabel, type PaneNode } from "./paneTitle";
import { setPaneSwapHandler } from "./paneDrag";
import {
  allPaneSessions,
  forgetPaneSession,
  getPaneSession,
  prunePaneSessions,
  rememberPaneSession,
} from "./sessionRestore";
import { pruneFmPaths } from "./fmPaths";
import { FeedPanel } from "./FeedPanel";
import { NotesModal } from "./NotesModal";
import { SetupWizard } from "./SetupWizard";
import { InsightsWindow } from "./InsightsWindow";
import { ClaudeUsageIndicator } from "./ClaudeUsageIndicator";
import {
  IconBell,
  IconFolder,
  IconGlobe,
  IconActivity,
  IconBug,
  IconGitCompare,
  IconColumns,
  IconMore,
  IconRows,
} from "./icons";
import { createNarrow } from "./useNarrow";
import { AddonsWindow } from "./AddonsWindow";
import { SettingsModal } from "./SettingsModal";
import { SshKeyOfferModal } from "./SshKeyOfferModal";
import { CommandPalette, type Command } from "./CommandPalette";
import { PortsWindow } from "./PortsWindow";
import { BrowserWindow } from "./BrowserWindow";
import { TicketModal } from "./TicketModal";
import { ProjectFolderModal, type ProjectFolderModalMode } from "./ProjectFolderModal";
import { ConfirmDeleteWorkspace } from "./ConfirmDeleteWorkspace";
import { DirPicker } from "./DirPicker";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { TicketsPanel } from "./TicketsPanel";
import { parseCapture, pendingCapture, setPendingCapture } from "./browserDevMode";
import { FileManagerPane } from "./FileManagerPane";
import { PanelSurface } from "./PanelSurface";
import type { Geometry } from "./floatingWindow";
import { closeOtherDrawers, type PanelId, type Surface, type PanelSurfaces } from "./panels";
import {
  TerminalInstance,
  copyTerminalSelection,
  pasteIntoActiveTerminal,
  readClipboardText,
  setCtrlCCopyOnSelect,
  setPaneTuiSignal,
} from "./terminalInstance";
import { saveRemoteFileAs } from "./download";
import { MarkdownViewer } from "./MarkdownViewer";
import { initTransferListener } from "./transferStore";
import {
  applyTheme,
  watchSystemTheme,
  loadSettings,
  saveSettings,
  DEFAULT_SHORTCUTS,
  DEFAULT_HOOKS_UPDATES,
  type Settings,
  type SidebarMode,
  type UpdateInfo,
  type HooksOutdatedInfo,
} from "./settings";
import { applyI18nSettings, t } from "./i18n";
import { isMac } from "./platform";
import {
  buildShortcutTable,
  keyEq,
  matches,
  parseShortcut,
  type ShortcutActionId,
  type ShortcutTable,
} from "./shortcuts";
import { makeSttRecorder, type SttRecorder } from "./stt";
import {
  collectPanes,
  describeConnection,
  effectiveIdentity,
  findPane,
  hasSftp,
  isLocalConn,
  isRemoteConn,
  isRemoteWorkspace,
  paneCaps,
  wsCaps,
  paneKindOf,
  pruneLayout,
  type Connection,
  type CreateWorkspaceInput,
  type EnvVar,
  type FeedItem,
  type ForwardRow,
  type WorktreeEntry,
  type FeedResolvedEvent,
  type KillSessionOutcome,
  type LayoutNode,
  type Note,
  type NotesFile,
  type PtyDataEvent,
  type PtyExitEvent,
  type SplitDirection,
  type TmuxSessionInfo,
  type Workspace,
  type WorkspaceGroup,
  type WorkspacesFile,
} from "./types";
import { createLogger, setLoggerLevel } from "./logger";
import "@xterm/xterm/css/xterm.css";
import "./App.css";
import "./sidebar.css"; // the left rail, split out of App.css (must load after it, before tokens.css)
import "./tokens.css"; // Design Pass 01 (#2): --wmx-* tokens + dark/light mode (must load after App.css)
import "./themes-redesign.css"; // Claude Design handoff: 4 direction themes (must load after tokens.css)

const log = createLogger("APP");

type PaneStatus = { msg: string; err: boolean };

// Phase 62.B (item I): sidebar is a 3-state control — full / icons /
// hidden. The MODE persists in settings.json (atomic, Rule #7 — see
// settings.rs `sidebar_mode`). The full-mode WIDTH (continuous drag
// geometry) stays in localStorage, the right home for per-machine
// pixel geometry.
const SIDEBAR_MIN_W = 160;
const SIDEBAR_MAX_W = 480;
const SIDEBAR_DEFAULT_W = 224;
const SIDEBAR_ICONS_W = 48;
const SIDEBAR_W_KEY = "ymux.sidebar-width";
function loadSidebarWidth(): number {
  try {
    const n = Number(localStorage.getItem(SIDEBAR_W_KEY));
    if (Number.isFinite(n) && n >= SIDEBAR_MIN_W && n <= SIDEBAR_MAX_W) return n;
  } catch {
    // ignore
  }
  return SIDEBAR_DEFAULT_W;
}

function App() {
  const [file, setFile] = createSignal<WorkspacesFile>({
    version: 1,
    active_workspace_id: null,
    workspaces: [],
  });
  // Phase 80 (unified setup wizard): one entry point for every create
  // flow. `false` = closed; an object = open (a FRESH object per open, so
  // the keyed <Show> fully remounts the wizard each time — Phase 56-A
  // semantics). `target` deep-links the level-1 pick (palette "SSH:
  // Provision a server" pre-selects server; the Welcome CTAs deep-link
  // too). Editing an existing workspace is a separate modal driven by
  // `editingWorkspace` below.
  const [showSetup, setShowSetup] = createSignal<
    false | { target?: "server" | "local" }
  >(false);
  // Unshipped-fivefer (#1): Notification Center. Session-accumulating store
  // fed by both notification streams (OSC + RPC/agent); read-state persists
  // per-machine in localStorage (the items themselves are in-memory only, so
  // disk-persisting read-state would outlive its subjects).
  const [notifications, setNotifications] = createSignal<NotifItem[]>([]);
  // Unified side-panel lifecycle (see panels.ts). One registry replaces the
  // former scattered per-panel booleans (showNotifCenter / showInsights +
  // insightsMode / showFilesWindow). Each panel opens docked as a drawer,
  // then floats out or expands to fullscreen; only one drawer at a time.
  const [panels, setPanels] = createSignal<PanelSurfaces>({});
  const surfaceOf = (id: PanelId): Surface => panels()[id] ?? "closed";
  const setSurface = (id: PanelId, s: Surface) => setPanels((p) => ({ ...p, [id]: s }));
  const openPanel = (id: PanelId) =>
    setPanels((p) => ({ ...closeOtherDrawers(p, id), [id]: "drawer" })); // rule: opens docked
  const closePanel = (id: PanelId) => setSurface(id, "closed");
  const floatPanel = (id: PanelId) => setSurface(id, "float"); // ⤢ → in-app floating window
  const expandPanel = (id: PanelId) => setSurface(id, "fullscreen"); // ⛶ → maximized-pane overlay

  // v0.4.4 (Task 1): auto-connect on secondary panels. Opening Monitor / Files
  // / Browser / Ports in a *disconnected* SSH workspace used to fail with
  // "no active SSH session — connect a terminal pane first". The panels resolve
  // their SSH handle in the backend (scanning sessions for the workspace_id,
  // including the headless __headless__<ws> handle), and they fetch once on
  // mount with no polling — so we headlessly arm the connection FIRST, then
  // open. `workspace_ensure_connected` is idempotent, PTY-free and tmux-free
  // (no orphan risk), and silently no-ops on password-only workspaces (can't
  // connect without a prompt) — those fall back to the existing hint.
  const [connectingWs, setConnectingWs] = createSignal<string | null>(null);
  const armWorkspaceConnection = async (): Promise<void> => {
    const ws = activeWs();
    if (!ws || !isRemoteWorkspace(ws)) return;
    setConnectingWs(ws.id);
    try {
      await invoke("workspace_ensure_connected", { workspaceId: ws.id });
    } catch (e) {
      log.warn("armWorkspaceConnection failed", e);
    } finally {
      setConnectingWs(null);
    }
  };
  // Arm the SSH connection, then open an SSH-dependent panel.
  const openPanelConnected = async (id: PanelId): Promise<void> => {
    await armWorkspaceConnection();
    openPanel(id);
  };
  const NOTIF_READ_KEY = "ymux.notif.read";
  const loadNotifRead = (): Set<number> => {
    try {
      return new Set(JSON.parse(localStorage.getItem(NOTIF_READ_KEY) ?? "[]") as number[]);
    } catch {
      return new Set();
    }
  };
  const [notifRead, setNotifRead] = createSignal<Set<number>>(loadNotifRead());
  const persistNotifRead = (s: Set<number>) => {
    try {
      localStorage.setItem(NOTIF_READ_KEY, JSON.stringify([...s]));
    } catch {
      /* private mode / quota */
    }
  };
  const pushNotif = (n: NotifItem) =>
    setNotifications((prev) =>
      prev.some((x) => x.id === n.id) ? prev : [n, ...prev].slice(0, 300),
    );
  const markNotifRead = (id: number) =>
    setNotifRead((prev) => {
      const n = new Set(prev);
      n.add(id);
      persistNotifRead(n);
      return n;
    });
  const markAllNotifRead = () =>
    setNotifRead(() => {
      const n = new Set(notifications().map((x) => x.id));
      persistNotifRead(n);
      return n;
    });
  const clearNotifs = () => {
    void invoke("notifications_clear").catch(() => {});
    setNotifications([]);
  };
  const unreadNotifs = () => notifications().filter((n) => !notifRead().has(n.id)).length;
  // #2: mirror the unread count to the Windows taskbar badge.
  createEffect(() => {
    const c = unreadNotifs();
    void invoke("set_tray_badge", { count: c }).catch(() => {});
  });
  // #1 fix: map a FeedItem (hooks/permissions/passive) to a NotifItem so the
  // Notification Center shows the same stream the user sees in the feed. The
  // id is a stable hash of request_id so an add+resolve don't duplicate.
  const feedToNotif = (f: FeedItem): NotifItem => {
    let h = 0;
    for (let i = 0; i < f.request_id.length; i++) h = (h * 31 + f.request_id.charCodeAt(i)) | 0;
    const kind =
      f.kind === "notification" ? "notification" : f.kind === "error" ? "error" : "agent";
    return {
      id: Math.abs(h),
      title: f.title || f.summary || "",
      body: f.title ? f.summary : "",
      workspace_id: f.workspace_id ?? null,
      // 66.G: keep the originating pane so a Notification Center click can
      // land on the exact pane, not just the workspace.
      pane_id: f.pane_id ?? null,
      timestamp_ms: f.created_ms,
      kind,
    };
  };
  const [editingWorkspace, setEditingWorkspace] = createSignal<Workspace | null>(null);
  const [activePaneId, setActivePaneId] = createSignal<string | null>(null);
  // Phase 55-A: pane maximize toggle. When set, LayoutView gets just
  // that leaf as its node (the rest of the split tree still lives in
  // ws.layout; restore swaps it back). pty_resize fires for every
  // pane in the workspace after enter/exit so xterm geometry catches
  // up to the new available area.
  const [maximizedPaneId, setMaximizedPaneId] = createSignal<string | null>(null);
  // Which tab was last active in each workspace, so switching away and
  // back doesn't dump you on tab 1. In-memory only: persisting it would
  // mean a workspaces.json write per focus change, and the cost of
  // getting it wrong is one click.
  const lastPaneByWs = new Map<string, string>();
  // Unshipped-fivefer (#4): pane_ids currently living in their own pop-out OS
  // window. They're pruned from the grid render tree (siblings reflow to fill),
  // and returned to their slot on `popout:closed`.
  const [poppedOut, setPoppedOut] = createSignal<Set<string>>(new Set());
  const [pendingPwFor, setPendingPwFor] = createSignal<string | null>(null);
  const [pendingPassphraseFor, setPendingPassphraseFor] = createSignal<{
    paneId: string;
    keyPath: string;
    bad?: boolean;
  } | null>(null);
  const [pendingHostTrust, setPendingHostTrust] = createSignal<{
    paneId: string;
    target: string;
    keyType: string;
    fingerprint: string;
    mismatchOld?: string;
  } | null>(null);
  const [paneStatus, setPaneStatus] = createSignal<Record<string, PaneStatus>>({});
  // Live pane status text (e.g. "bootstrapping ymux…") set by backend events.
  const [paneStatusText, setPaneStatusText] = createSignal<Record<string, string>>({});
  // issue #4 (ymux-tools chrome Ticker): per-pane agent turn timing. The
  // backend emits pane:agent-run only on turn start/end; the label ticks
  // locally off `pulseTick` (see agentRunLabel). startedAt=null → no live turn.
  const [agentRuns, setAgentRuns] = createSignal<
    Record<
      string,
      {
        startedAt: number | null;
        avgMs: number | null;
        // Phase 84.B: the effective agent state behind the traffic light,
        // plus when it last changed and the backend's per-pane sequence.
        state: PaneAgentState;
        stateSince: number | null;
        seq: number;
      }
    >
  >({});
  // BRIEF: per-pane brief entries (agent-written brief + last user prompt),
  // mirrored off `pane:brief` events and hydrated by `pane_briefs` on
  // reload. Same lifecycle as agentRuns above.
  const [briefs, setBriefs] = createSignal<Record<string, PaneBriefEntry>>({});
  // cmux-A A1: pane_ids that received an OSC 9/99/777 notification and
  // haven't been focused since. Drives the amber pulse ring on the pane
  // + the sidebar aggregate badge. Cleared when the pane is focused.
  const [paneNotified, setPaneNotified] = createSignal<Set<string>>(new Set());
  const addPaneNotified = (pid: string) =>
    setPaneNotified((prev) => {
      if (prev.has(pid)) return prev;
      const n = new Set(prev);
      n.add(pid);
      return n;
    });
  const clearPaneNotified = (pid: string) =>
    setPaneNotified((prev) => {
      if (!prev.has(pid)) return prev;
      const n = new Set(prev);
      n.delete(pid);
      return n;
    });
  // Phase 6.5: agent feed (most recent first; capped to 50 server-side).
  const [feedItems, setFeedItems] = createSignal<FeedItem[]>([]);
  // Phase 7.B: notes
  const [notes, setNotes] = createSignal<Note[]>([]);
  const [showNotes, setShowNotes] = createSignal(false);
  // Phase 9.A: settings + Phase 9.B: update banner.
  const [settings, setSettings] = createSignal<Settings | null>(null);
  const [showSettings, setShowSettings] = createSignal(false);
  const [updateBanner, setUpdateBanner] = createSignal<UpdateInfo | null>(null);
  // Phase 27: in-flight state for the one-click installer download.
  const [installingUpdate, setInstallingUpdate] = createSignal(false);
  // Phase 65 (U): set when the one-click install fails, so the banner
  // surfaces the manual "Download from GitHub" escape hatch — users are
  // never stuck on an old version even if auto-install can't proceed.
  const [installError, setInstallError] = createSignal(false);
  const installUpdate = async () => {
    if (installingUpdate()) return;
    setInstallingUpdate(true);
    setInstallError(false);
    try {
      // Backend will exit() the app ~800ms after this returns; the
      // invoke promise resolves before exit so we can show "downloading"
      // → "installing" cleanly. On error the app keeps running.
      await invoke("download_and_install_update");
      // We're still alive briefly; the user sees the button locked in
      // "downloading…" state until the process actually exits.
    } catch (e) {
      flashSummaryToast("err", t("update_banner.install_failed", { msg: String(e) }));
      setInstallingUpdate(false);
      setInstallError(true);
    }
  };
  // Phase 65 (U): snooze the banner for a day.
  const remindUpdateLater = async () => {
    try {
      await invoke("updater_remind_later", { hours: 24 });
    } catch (e) {
      log.warn("updater_remind_later failed", e);
    }
    setUpdateBanner(null);
  };
  // Phase 65 (U): skip this version — banner stays hidden until a newer
  // one is published.
  const skipUpdateVersion = async () => {
    const v = updateBanner()?.latest_version;
    if (v) {
      try {
        await invoke("updater_skip_version", { version: v });
      } catch (e) {
        log.warn("updater_skip_version failed", e);
      }
    }
    setUpdateBanner(null);
  };
  // Phase 14.A: server provisioning wizard. Phase 65.R folded the
  // "Connect to existing server" flow into this wizard's "existing"
  // mode, so there's no separate connect-existing modal anymore.
  // Phase 80: showProvision folded into showSetup (target: "server").
  // Monitor's open/drawer/float state now lives in the unified `panels`
  // registry (see panels.ts) under the "monitor" id.
  const [addonsWin, setAddonsWin] = createSignal<{ id: string; name: string } | null>(null);
  // Project folders: the pin dialog and the new-worktree dialog share
  // one modal, discriminated by `kind`.
  const [projectFolderModal, setProjectFolderModal] =
    createSignal<ProjectFolderModalMode | null>(null);
  // Phase 35 (#1.3): command palette (Ctrl+Shift+P).
  const [showPalette, setShowPalette] = createSignal(false);
  // Phase 36 (#2.2): live auto port-forwards (all workspaces).
  const [portForwards, setPortForwards] = createSignal<ForwardRow[]>([]);
  // Phase 46: ports the remote watcher has reported but the user
  // hasn't chosen to forward yet (one click → forward + browser).
  const [detectedPorts, setDetectedPorts] = createSignal<
    { workspace_id: string; remote_port: number; addr: string; family: string }[]
  >([]);
  // Phase 40: floating Ports window — scoped to the active workspace.
  const [showPortsWindow, setShowPortsWindow] = createSignal(false);
  // Phase 53 (rebased): floating workspace-level Browser window. Each
  // workspace owns its own browser session + remembered geometry; the
  // signal tracks the open/closed visibility of the host shell only
  // (the native Webview is hidden on close, not destroyed — page state
  // survives across open/close cycles).
  const [showBrowserWindow, setShowBrowserWindow] = createSignal(false);
  // Header ⋯ overflow menu: view-mode toggle, + diff, Insights, Tickets.
  // Browser / Files / the notification bell stay as visible buttons.
  const [wsMenuOpen, setWsMenuOpen] = createSignal(false);
  let wsMenuRef: HTMLDivElement | undefined;
  // Click-away: pointerdown, not click — a press that lands in a terminal
  // pane never bubbles a click back up here.
  createEffect(() => {
    if (!wsMenuOpen()) return;
    const onDown = (e: PointerEvent) => {
      if (wsMenuRef && e.target instanceof Node && wsMenuRef.contains(e.target)) return;
      setWsMenuOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    onCleanup(() => document.removeEventListener("pointerdown", onDown));
  });
  // Phase 85.C: workspaces whose Browser currently lives in its own OS
  // window. Their native Webview is a child of THAT window, so nothing
  // here may hide, show, or reposition it — see the modal broadcast
  // below, which skips them.
  const [poppedOutBrowsers, setPoppedOutBrowsers] = createSignal<Set<string>>(
    new Set(),
  );
  // Pop the Browser out. The child Webview cannot be re-parented, so the
  // Rust side destroys the one under `main` and the popped-out window
  // spawns a fresh one under itself — the page reloads. Once it is out,
  // this window's floating panel has nothing left to show, so it closes.
  const popOutBrowser = async () => {
    const ws = activeWs();
    if (!ws) return;
    // Order matters. Closing the panel FIRST makes its falling-edge
    // effect fire `workspace_browser_hide` now, while the only Webview
    // for this workspace is still the one hosted by `main`. Do it after
    // the await instead and the hide races the popped-out window's own
    // `workspace_browser_show` — same workspace id, same map entry — so
    // a slow hide would land on the NEW child and blank a window that
    // has no reason to ever call show again.
    setPoppedOutBrowsers((prev) => new Set(prev).add(ws.id));
    setShowBrowserWindow(false);
    try {
      await invoke("browser_popout_open", {
        workspaceId: ws.id,
        title: `${ws.name} — Browser`,
      });
    } catch (e) {
      // Nothing was popped out — put the panel back exactly as it was.
      setPoppedOutBrowsers((prev) => {
        const n = new Set(prev);
        n.delete(ws.id);
        return n;
      });
      setShowBrowserWindow(true);
      log.error("browser_popout_open failed", e);
      flashSummaryToast("err", t("browser.popout.failed", { msg: String(e) }));
    }
  };

  // Boot restore: the Browser was popped out when the app last exited,
  // so bring the window back instead of the in-app panel. One-shot —
  // `settings()` and `activeWs()` both arrive asynchronously, so this
  // waits for the pair rather than racing them, and the guard keeps a
  // later workspace switch from re-firing it.
  let popoutRestored = false;
  createEffect(() => {
    if (popoutRestored) return;
    const s = settings();
    const ws = activeWs();
    if (!s || !ws) return;
    popoutRestored = true;
    if (s.floating_windows?.browser?.mode !== "popout") return;
    void popOutBrowser();
  });
  // Phase 53 (rebased): workspace-level File Manager. Pure HTML — wraps
  // the existing FileManagerPane. Its open/drawer/float state now lives in
  // the unified `panels` registry (see panels.ts) under the "files" id.
  // Phase 62.B (item I): sidebar mode lives in settings.json; full-mode
  // width lives in localStorage. Phase 65.P: two modes only (full /
  // icons). Any legacy "hidden" value migrates to "icons" on read so
  // older settings.json files don't strand the sidebar off-screen.
  const [sidebarWidth, setSidebarWidth] = createSignal(loadSidebarWidth());
  // Collapse the workspace-header tool buttons to icon-only when the header is
  // too narrow to fit their labels (labels then live in each button's title).
  const wsHeaderNarrow = createNarrow(640);
  const sidebarMode = (): SidebarMode => {
    // Read as a plain string: a legacy settings.json may still hold the
    // dropped "hidden" value, which is outside the SidebarMode union.
    const raw = settings()?.sidebar_mode as string | undefined;
    return raw === "icons" || raw === "hidden" ? "icons" : "full";
  };
  const sidebarPx = () => {
    const m = sidebarMode();
    if (m === "icons") return SIDEBAR_ICONS_W;
    return sidebarWidth();
  };
  const setSidebarMode = (mode: SidebarMode) => {
    const s = settings();
    if (!s) return;
    const next: Settings = { ...s, sidebar_mode: mode };
    setSettings(next);
    void saveSettings(next).catch((e) =>
      log.warn("saveSettings (sidebar_mode) failed", e),
    );
  };
  // Phase 65.P: Ctrl+B toggles full ↔ icons (two modes only); the
  // header button does the same. No "hidden" state anymore.
  const cycleSidebarMode = () => {
    setSidebarMode(sidebarMode() === "full" ? "icons" : "full");
  };
  createEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_W_KEY, String(sidebarWidth()));
    } catch {
      // ignore (quota / private mode)
    }
  });
  const startSidebarResize = (e: MouseEvent) => {
    e.preventDefault();
    // Direction-aware: in RTL the sidebar sits on the right, so its
    // width grows as the pointer moves LEFT — measure from the correct
    // edge.
    const rtl =
      getComputedStyle(document.documentElement).direction === "rtl";
    const onMove = (ev: MouseEvent) => {
      const raw = rtl ? window.innerWidth - ev.clientX : ev.clientX;
      setSidebarWidth(
        Math.max(SIDEBAR_MIN_W, Math.min(SIDEBAR_MAX_W, Math.round(raw))),
      );
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  // Phase 58: push-to-talk voice input. Active recorder instance +
  // listening indicator. The recorder is created lazily on keydown
  // and reused for the lifetime of the press; release fires stop()
  // which resolves the start() promise with the transcribed text.
  let sttRecorder: SttRecorder | null = null;
  const [sttListening, setSttListening] = createSignal(false);
  const [sttError, setSttError] = createSignal<string | null>(null);
  const stopForward = (workspaceId: string, remotePort: number) => {
    void invoke("port_forward_stop", { workspaceId, remotePort });
  };
  // Phase 46: open a forward on demand from PortsWindow. The backend
  // sanity-probes the local port before returning, so on success we
  // know the browser tab will actually reach something. Returns the
  // assigned local port (or throws).
  const startForward = (workspaceId: string, remotePort: number): Promise<number> =>
    invoke<number>("forward_port_start", { workspaceId, remotePort });
  // Phase 35: webview zoom factor for view.zoom.* palette commands.
  const [zoomFactor, setZoomFactor] = createSignal(1);
  const applyZoom = (f: number) => {
    const clamped = Math.max(0.3, Math.min(3, f));
    setZoomFactor(clamped);
    void getCurrentWebview().setZoom(clamped).catch((e) => log.warn("setZoom failed", e));
  };
  // Phase 18: hooks-outdated banners — at most one banner per agent
  // at a time; the user dismisses (skip-this-version persists), defers
  // (banner gone until next connect), or triggers an in-place update.
  const [hooksBanner, setHooksBanner] = createSignal<HooksOutdatedInfo | null>(null);
  // The remote `ymux` CLI could not be converged onto the build this
  // desktop embeds. Kept per workspace and shown as a banner rather than a
  // pane status line: the status line self-clears after five seconds, which
  // is precisely how a version-skewed CLI stayed invisible while it broke
  // hooks and the reverse tunnel.
  type CliAlignmentEvent = {
    workspace_id: string;
    aligned: boolean;
    expected?: string;
    actual?: string;
    reason?: string;
  };
  const [cliSkew, setCliSkew] = createSignal<Record<string, CliAlignmentEvent>>({});
  const [hooksUpdating, setHooksUpdating] = createSignal(false);
  // Phase 53 (rebased): native child Webviews always paint above
  // HTML, so opening a modal would visually hide it behind the
  // workspace-level Browser window. This derived signal collects
  // every "is a modal open" state; the effect below hides every
  // workspace's Browser Webview when any modal opens. Re-show on
  // close is owned by the BrowserWindow component (Phase 53.E) — its
  // own visibility effect re-calls `workspace_browser_show` with the
  // current rect once `anyModalOpen()` flips back to false.
  const anyModalOpen = () =>
    showSetup() !== false || editingWorkspace() !== null || showNotes() ||
    showSettings() || showPalette() || showPortsWindow() || installingUpdate() ||
    // Dev-Mode ticket modal — same reason as the rest: the native
    // Browser Webview paints above HTML and must be hidden for it.
    pendingCapture() !== null || projectFolderModal() !== null ||
    dirPickerFor() !== null;
  createEffect(() => {
    if (!anyModalOpen()) return;
    // Broadcast hide to every workspace's Browser Webview. At most
    // one is actually visible at a time (the active workspace's), but
    // hiding any others that may exist is a cheap no-op on the
    // backend side (the command silently ignores workspaces with no
    // Webview spawned).
    //
    // Phase 85.C: EXCEPT a workspace that has been popped out. Its
    // Webview is a child of a different OS window, on possibly a
    // different monitor — no modal here is covering it, so hiding it
    // would blank that window every time the user opened Settings, and
    // nothing over there would ever show it again.
    for (const w of file().workspaces) {
      if (poppedOutBrowsers().has(w.id)) continue;
      void invoke("workspace_browser_hide", {
        workspaceId: w.id,
      }).catch(() => {});
    }
  });

  // Phase 17: ephemeral toast for "Summary saved as note" + the
  // ad-hoc errors that can come back from `claude_summarize`. Auto-
  // dismisses after 4s.
  const [summaryToast, setSummaryToast] = createSignal<
    | { kind: "ok"; text: string }
    | { kind: "err"; text: string }
    | null
  >(null);
  let summaryToastTimer: number | null = null;
  const flashSummaryToast = (kind: "ok" | "err", text: string) => {
    if (summaryToastTimer) clearTimeout(summaryToastTimer);
    setSummaryToast({ kind, text });
    summaryToastTimer = window.setTimeout(() => setSummaryToast(null), 4500);
  };

  // beta.3 (netfree, Track 1b): reconnect toast + backoff-retry driver.
  //
  // When the backend emits `ssh:disconnected` (transport dropped, not a
  // clean Eof/Close), we own a small state machine here that:
  //   1) shows a persistent toast — "מנסה להתחבר מחדש… (N/5)"
  //   2) sleeps with backoff (1s → 3s → 8s → 15s → 30s, ±20% jitter)
  //   3) invokes the existing `pane_connect` command for each attempt
  //      (auth params come from the pane's stored connection — no
  //      credentials cached client-side, which is the whole reason the
  //      retry loop lives here and not in the backend io-task)
  //   4) cancel button aborts the timer + invokes `ssh_cancel_reconnect`
  //   5) on success, replaces the toast with a green "מחובר מחדש"
  //   6) after all attempts fail, shows the "click pane to retry" error
  //
  // tmux side: server-side tmux session survives; a successful reconnect
  // just runs `tmux attach -t <name>` again via the persistent flag, so
  // the user's scrollback + running processes come back intact.
  //
  // The state is keyed BY PANE. It used to be three module-scope
  // singletons (one toast signal, one timer, one cancelled flag), and
  // `startReconnect` opened with `if (reconnectToast()) cancelReconnect()`
  // — so on a full outage, where every pane drops in the same tick, each
  // incoming `ssh:disconnected` cancelled the previous pane's retry loop
  // AND invoked `ssh_cancel_reconnect` on it. Only the LAST pane to fire
  // ever retried; the rest were abandoned without a single attempt. The
  // old comment called concurrent drops "rare … but not impossible on a
  // full network outage" — which is exactly the unplug-the-laptop case
  // this is meant to survive.
  type ReconnectToast = {
    paneId: string;
    host: string;
    workspaceId: string;
    attempt: number;
    max: number;
  };
  const [reconnectToasts, setReconnectToasts] = createSignal<ReconnectToast[]>([]);
  // Timer + cancel handle per pane — held outside the signal so cancel()
  // can clear them without racing the state update.
  const reconnectRuns = new Map<string, { timer: number | null; cancelled: boolean }>();
  // 1s → 3s → 8s → 15s → 30s. 5 attempts total per spec.
  const RECONNECT_BACKOFFS_MS = [1_000, 3_000, 8_000, 15_000, 30_000];
  const RECONNECT_MAX = RECONNECT_BACKOFFS_MS.length;
  const reconnectJitter = (ms: number) => {
    // ±20% — spreads out concurrent retries so a filter that just came
    // back up doesn't get pounded by every client at the same instant.
    // With N panes retrying in parallel this also staggers them, which is
    // what keeps N simultaneous bootstraps off the same SFTP channel.
    const jitter = ms * 0.2 * (Math.random() * 2 - 1);
    return Math.max(0, Math.round(ms + jitter));
  };
  const clearReconnectTimer = (paneId: string) => {
    const run = reconnectRuns.get(paneId);
    if (run?.timer != null) {
      window.clearTimeout(run.timer);
      run.timer = null;
    }
  };
  // Drop one pane's run: stop its timer, forget it, and remove its toast
  // row. Does NOT touch any other pane's loop.
  const endReconnect = (paneId: string) => {
    clearReconnectTimer(paneId);
    reconnectRuns.delete(paneId);
    setReconnectToasts((prev) => prev.filter((r) => r.paneId !== paneId));
  };
  // Cancel one pane, or every pane in flight when called with no argument
  // (the toast's Cancel button covers the whole batch — the user asked for
  // the reconnecting to stop, not for one arbitrary pane of it to stop).
  const cancelReconnect = (paneId?: string) => {
    const ids = paneId ? [paneId] : reconnectToasts().map((r) => r.paneId);
    for (const id of ids) {
      const run = reconnectRuns.get(id);
      if (run) run.cancelled = true;
      endReconnect(id);
      // Best-effort — a "no such pane" is fine (session may already be gone).
      invoke("ssh_cancel_reconnect", { paneId: id }).catch(() => {});
    }
  };
  type SshDisconnectedEvent = {
    workspace_id: string;
    pane_id: string;
    host: string;
    user: string;
    port: number;
    key_path: string | null;
    tmux_session_name: string | null;
    persistent: boolean;
    reason: string;
  };

  // Phase 18: hooks-outdated banner actions.
  const triggerHooksUpdate = async () => {
    const b = hooksBanner();
    if (!b) return;
    setHooksUpdating(true);
    try {
      // Pipe the setup-hooks command through the active SSH pane via
      // the existing tunnel by reusing the connect-with-cmd path. We
      // can't shell out from Rust without an SSH handle; the user's
      // own pane runs the CLI under their PATH (which AddYmuxToPath
      // sets up). The command writes settings.json, then a fresh
      // restart of Claude picks up the new hooks.
      await invoke("ssh_exec_in_workspace", {
        workspaceId: b.workspace_id,
        cmd: "ymux setup-hooks --agent claude --force --source github",
      }).catch(async () => {
        // Older builds without ssh_exec_in_workspace — fall back to a
        // pane.send: ask the user to run the command themselves.
        log.warn("ssh_exec_in_workspace not available; user must run manually");
      });
      flashSummaryToast("ok", t("hooks_update.toast_done", { version: b.latest }));
      setHooksBanner(null);
    } catch (e) {
      flashSummaryToast("err", String(e));
    } finally {
      setHooksUpdating(false);
    }
  };

  const dismissHooksLater = () => setHooksBanner(null);

  const skipHooksVersion = async () => {
    const b = hooksBanner();
    if (!b) return;
    const s = settings();
    if (!s) {
      setHooksBanner(null);
      return;
    }
    const next: Settings = {
      ...s,
      hooks_updates: {
        ...(s.hooks_updates ?? DEFAULT_HOOKS_UPDATES),
        dismissed: {
          ...(s.hooks_updates?.dismissed ?? {}),
          [b.agent]: Array.from(
            new Set([
              ...((s.hooks_updates?.dismissed ?? {})[b.agent] ?? []),
              b.latest,
            ])
          ),
        },
      },
    };
    try {
      await saveSettings(next);
    } catch (e) {
      log.warn("saveSettings failed (skipHooksVersion)", e);
    }
    setHooksBanner(null);
  };

  const summarizeActivePane = async () => {
    const ws = activeWs();
    if (!ws) {
      flashSummaryToast("err", t("claude.summary.no_workspace"));
      return;
    }
    try {
      const r: any = await invoke("claude_summarize", {
        workspaceId: ws.id,
        paneId: activePaneId() ?? null,
        sessionId: null,
        historyCount: null,
        promptOverride: null,
      });
      flashSummaryToast(
        "ok",
        t("claude.summary.toast", { count: r.messages_count ?? "" }),
      );
      // Refresh notes so the new summary note is visible in the
      // Notes modal next time it opens.
      void refreshNotes();
    } catch (e) {
      flashSummaryToast("err", String(e));
    }
  };
  // Phase 16: parsed shortcut accelerators, rebuilt on every settings
  // load + settings:changed event. Backfilled with DEFAULT_SHORTCUTS
  // when the field is missing (pre-16 settings.json).
  const [shortcutTable, setShortcutTable] = createSignal<ShortcutTable>(
    buildShortcutTable(DEFAULT_SHORTCUTS),
  );
  // Phase 11.A: per-pane tmux persistence map { pane_id → session_name }.
  const [panePersistence, setPanePersistence] = createSignal<Record<string, string>>({});
  const refreshPersistence = async () => {
    try {
      const m = await invoke<Record<string, string>>("pane_persistence_list");
      setPanePersistence(m ?? {});
      // Phase 80: every refresh is a fresh, authoritative "pane → tmux session"
      // answer from the backend, so record it here rather than only in the
      // post-connect callback. That callback fires on one 100ms timer down one
      // code path; this covers every path that reaches a live tmux session —
      // the picker, the reconnect driver, a CLI-triggered connect — and it
      // re-writes the hint on every refresh, so a single missed write can't
      // cost the user their session.
      //
      // ADD ONLY, never remove: this map lists LIVE sessions, and a pane that
      // is merely detached (app closed, pane closed) is absent from it while
      // its tmux session is very much alive. Dropping hints is the job of the
      // explicit forget calls — kill, close, and "the server says it's gone".
      for (const [paneId, tmuxName] of Object.entries(m ?? {})) {
        if (tmuxName) rememberPaneSession(paneId, tmuxName);
      }
    } catch (e) {
      log.warn("pane_persistence_list failed", e);
    }
  };
  const refreshNotes = async () => {
    try {
      const f = await invoke<NotesFile>("notes_load");
      setNotes(f.notes ?? []);
    } catch (e) {
      log.warn("notes_load failed", e);
    }
  };
  const FEED_AUTO_DISMISS_MS = 3000;
  const scheduleFeedDismiss = (request_id: string) => {
    setTimeout(() => {
      setFeedItems((prev) => prev.filter((i) => i.request_id !== request_id));
    }, FEED_AUTO_DISMISS_MS);
  };
  const [tick, setTick] = createSignal(0);
  const bump = () => setTick(tick() + 1);

  const terms = new Map<string, TerminalInstance>();
  const paneToSession = new Map<string, string>();
  const sessionToPane = new Map<string, string>();

  const ensureTerm = (
    paneId: string,
    profile: RtlProfileKind = "local",
  ): TerminalInstance => {
    let ti = terms.get(paneId);
    // 2026-08-19: a pane can be built before its connection is known (the
    // effect that mounts the terminal runs before Connect), and local vs
    // remote want opposite RTL modes. If the profile now resolves differently
    // AND that flips the renderer, the instance cannot adapt — xterm.js has
    // no DOM<->WebGL swap — so rebuild it rather than leave a pane that
    // silently ignores its own settings.
    if (ti && ti.profile !== profile && ti.staleRenderer) {
      ti.dispose();
      terms.delete(paneId);
      ti = undefined;
    }
    if (!ti) {
      ti = new TerminalInstance(paneId, profile);
      terms.set(paneId, ti);
    }
    return ti;
  };

  // Unshipped-fivefer (#4): pop a live pane's terminal into its own OS
  // window. The popout (index.html?popout=<sid>) becomes the input + resize
  // authority; this pane detaches to a read-only mirror — the global
  // pty:data listener keeps rendering it. Re-attaches on `popout:closed`.
  const popOutPane = async (paneId: string) => {
    const sid = paneToSession.get(paneId);
    const ti = terms.get(paneId);
    if (!sid || !ti) return;
    const label = activeWs()?.name ?? "ymux";
    const dir = document.documentElement.dir === "rtl" ? "rtl" : "ltr";
    // Seed the popout's Ctrl+wheel zoom from the configured terminal size the
    // first time only — later wheel zooms own it (localStorage, shared origin).
    if (localStorage.getItem("ymux.popout.font_size_pt") == null) {
      localStorage.setItem(
        "ymux.popout.font_size_pt",
        String(settings()?.font.terminal_size_pt ?? 13),
      );
    }
    try {
      await invoke("popout_pane", {
        sessionId: sid,
        title: `${label} — ymux`,
        cols: ti.term.cols,
        rows: ti.term.rows,
        dir,
      });
      // The pane now lives in its own OS window — vacate its grid slot so the
      // siblings reflow to fill it (it returns on popout:closed). detach() so
      // the hidden grid terminal is no longer the input/resize authority.
      ti.detach();
      const nextHidden = new Set(poppedOut());
      nextHidden.add(paneId);
      setPoppedOut(nextHidden);
      // If we just hid the active pane, move focus to a still-visible one.
      const wsLayout = activeWs()?.layout;
      if (activePaneId() === paneId && wsLayout) {
        const survivor = collectPanes(wsLayout).find((p) => !nextHidden.has(p));
        if (survivor) {
          setActivePaneId(survivor);
          terms.get(survivor)?.focus();
        }
      }
    } catch (e) {
      log.error("popout_pane failed", e);
    }
  };

  // Phase 65.O (round 6): the tmux wheel-proxy was deleted — xterm.js
  // handles the wheel natively in every case. No per-pane flag to sync.

  const setStatus = (paneId: string, msg: string, err: boolean) =>
    setPaneStatus({ ...paneStatus(), [paneId]: { msg, err } });
  const clearStatus = (paneId: string) => {
    const s = { ...paneStatus() };
    delete s[paneId];
    setPaneStatus(s);
  };

  const activeWs = (): Workspace | null =>
    file().workspaces.find((w) => w.id === file().active_workspace_id) ?? null;

  // Phase 84.A: tabs mode. No signal — the flag is persisted on the
  // workspace and `file()` is already reactive, so this reads through.
  const tabsMode = (): boolean => activeWs()?.tabs_mode === true;

  // Phase 84.A: focusing a pane, extracted from LayoutView's onFocus so
  // the tab strip goes through exactly the same path. Switching tabs and
  // clicking a pane must not drift apart.
  const focusPane = (paneId: string) => {
    setActivePaneId(paneId);
    // cmux-A A1: focusing a pane clears its pulse.
    clearPaneNotified(paneId);
    const wsId = activeWs()?.id;
    if (wsId) lastPaneByWs.set(wsId, paneId);
    terms.get(paneId)?.focus();
  };

  // Phase 35 (#1.3): cycle focus through the active workspace's panes.
  const focusAdjacentPane = (delta: number) => {
    const ws = activeWs();
    if (!ws?.layout) return;
    const panes = collectPanes(ws.layout);
    if (panes.length === 0) return;
    const cur = activePaneId();
    const idx = cur ? panes.indexOf(cur) : -1;
    const next = panes[(idx + delta + panes.length) % panes.length];
    // In tabs mode this is the Ctrl+Tab handler, so it must actually move
    // focus into the terminal — setActivePaneId alone only repaints.
    if (next) focusPane(next);
  };

  // Phase 48-E: find the pane that's the nearest neighbor of `paneId`
  // in a given direction. Walks the layout tree: collects the path
  // from root to the pane (leaf-first), then finds the closest
  // ancestor whose split direction matches and where our subtree sits
  // on the side opposite the target direction. Returns the
  // first/leftmost/topmost leaf of the sibling subtree on the target
  // side. Returns null if no neighbor exists in that direction.
  const findDirectionalNeighbor = (
    root: LayoutNode,
    paneId: string,
    dir: "left" | "right" | "up" | "down",
  ): string | null => {
    const path: { node: LayoutNode & { kind: "split" }; side: "first" | "second" }[] = [];
    const walk = (n: LayoutNode): boolean => {
      if (n.kind === "pane") return n.pane_id === paneId;
      if (walk(n.first)) {
        path.push({ node: n, side: "first" });
        return true;
      }
      if (walk(n.second)) {
        path.push({ node: n, side: "second" });
        return true;
      }
      return false;
    };
    if (!walk(root)) return null;
    const needSplitDir = dir === "left" || dir === "right" ? "horizontal" : "vertical";
    // To go RIGHT/DOWN we need to be on the FIRST side of a matching
    // split; the sibling on the SECOND side holds our neighbor. Reverse
    // for LEFT/UP. Then descend into the sibling: for LEFT/UP, take
    // SECOND repeatedly (rightmost/bottommost leaf); for RIGHT/DOWN,
    // take FIRST repeatedly (leftmost/topmost leaf).
    const seekSide = dir === "right" || dir === "down" ? "first" : "second";
    const descendSide = dir === "right" || dir === "down" ? "first" : "second";
    for (const step of path) {
      if (step.node.direction === needSplitDir && step.side === seekSide) {
        let cur: LayoutNode = step.side === "first" ? step.node.second : step.node.first;
        while (cur.kind === "split") {
          cur = (cur as Extract<LayoutNode, { kind: "split" }>)[descendSide];
        }
        return cur.pane_id;
      }
    }
    return null;
  };

  // Phase 48-E: Ctrl+Alt+Arrow — if there's a pane in that direction,
  // focus it; otherwise split the current pane in that direction.
  // Left/Right map to horizontal splits, Up/Down to vertical.
  const splitOrMove = (dir: "left" | "right" | "up" | "down") => {
    const ws = activeWs();
    const cur = activePaneId();
    if (!ws?.layout || !cur) return;
    const neighbor = findDirectionalNeighbor(ws.layout, cur, dir);
    if (neighbor) {
      setActivePaneId(neighbor);
      return;
    }
    const splitDir: SplitDirection =
      dir === "left" || dir === "right" ? "horizontal" : "vertical";
    void splitPane(cur, splitDir);
  };

  // Phase 35 (#1.3): the command-palette catalog. Each command reuses
  // the same handler the existing UI calls. `enabled` hides commands
  // that need context they don't have (no active workspace / pane).
  const paletteCommands = (): Command[] => {
    const ws = activeWs();
    const pid = activePaneId();
    const hasWs = !!ws;
    const hasPane = !!pid;
    return [
      { id: "workspace.new", label: t("cmd.workspace.new"), handler: () => setShowSetup({}) },
      { id: "queue.open", label: t("cmd.queue.open"), handler: () => openPanel("queue") },
      { id: "workspace.rename", label: t("cmd.workspace.rename"), enabled: () => hasWs, handler: () => { if (ws) setEditingWorkspace(ws); } },
      { id: "workspace.disconnect", label: t("cmd.workspace.disconnect"), enabled: () => hasWs, handler: () => { if (ws) void handleDisconnectWorkspace(ws.id); } },
      { id: "workspace.delete", label: t("cmd.workspace.delete"), enabled: () => hasWs, handler: () => { if (ws) void handleDelete(ws.id); } },
      { id: "pane.split.right", label: t("cmd.pane.split.right"), enabled: () => hasPane, handler: () => { if (pid) void splitPane(pid, "horizontal"); } },
      { id: "pane.split.down", label: t("cmd.pane.split.down"), enabled: () => hasPane, handler: () => { if (pid) void splitPane(pid, "vertical"); } },
      { id: "pane.close", label: t("cmd.pane.close"), enabled: () => hasPane, handler: () => { if (pid) void closePane(pid); } },
      { id: "pane.focus.next", label: t("cmd.pane.focus.next"), enabled: () => hasPane, handler: () => focusAdjacentPane(1) },
      { id: "pane.focus.prev", label: t("cmd.pane.focus.prev"), enabled: () => hasPane, handler: () => focusAdjacentPane(-1) },
      // Phase 55-A: maximize toggle (Ctrl+Enter / double-click pane content).
      { id: "pane.maximize", label: t("cmd.pane.maximize"), enabled: () => hasPane, handler: () => toggleMaximize() },
      // Phase 84.A: split ⇄ tabs for the active workspace.
      { id: "pane.viewMode.toggle", label: t("cmd.pane.viewMode.toggle"), enabled: () => !!activeWs(), handler: () => void setTabsMode(!tabsMode()) },
      // Phase 55-B: distribute splits evenly (Ctrl+Alt+=).
      { id: "pane.distributeEvenly", label: t("cmd.pane.distributeEvenly"), enabled: () => hasPane, handler: () => void distributeEvenly() },
      { id: "pane.rename", label: t("cmd.pane.rename"), enabled: () => hasPane, handler: () => { if (pid) window.dispatchEvent(new CustomEvent("ymux:pane-rename", { detail: pid })); } },
      { id: "ssh.connect", label: t("cmd.ssh.connect"), enabled: () => hasPane, handler: () => { if (pid) void connectPane(pid); } },
      { id: "ssh.disconnect", label: t("cmd.ssh.disconnect"), enabled: () => hasPane, handler: () => { if (pid) void disconnectPane(pid); } },
      { id: "pane.reset", label: t("cmd.reset_terminal"), enabled: () => hasPane, handler: () => { if (pid) terms.get(pid)?.resetTerminal(); } },
      { id: "ssh.provision", label: t("cmd.ssh.provision"), handler: () => setShowSetup({ target: "server" }) },
      { id: "insights.monitor", label: t("cmd.insights.monitor"), enabled: () => hasWs, handler: () => void openPanelConnected("monitor") },
      { id: "settings.open", label: t("cmd.settings.open"), handler: () => setShowSettings(true) },
      { id: "settings.language", label: t("cmd.settings.language"), handler: () => setShowSettings(true) },
      { id: "settings.theme", label: t("cmd.settings.theme"), handler: () => setShowSettings(true) },
      { id: "view.zoom.in", label: t("cmd.view.zoom.in"), handler: () => applyZoom(zoomFactor() + 0.1) },
      { id: "view.zoom.out", label: t("cmd.view.zoom.out"), handler: () => applyZoom(zoomFactor() - 0.1) },
      { id: "view.zoom.reset", label: t("cmd.view.zoom.reset"), handler: () => applyZoom(1) },
      { id: "fm.open", label: t("cmd.fm.open"), enabled: () => hasPane && hasWs, handler: () => {
        if (ws && pid) void invoke("workspace_split", { workspaceId: ws.id, paneId: pid, direction: "horizontal", paneKind: "filemanager", browserUrl: null, helpTopic: null });
      } },
    ];
  };

  const connectedPanes = (): Set<string> => {
    void tick();
    return new Set(paneToSession.keys());
  };

  const liveWorkspaceIds = (): Set<string> => {
    void tick();
    const live = new Set<string>();
    for (const w of file().workspaces) {
      if (!w.layout) continue;
      const ps = collectPanes(w.layout);
      if (ps.some((p) => paneToSession.has(p))) live.add(w.id);
    }
    return live;
  };

  // workspace_ids holding at least one pane with a pending OSC 9/99/777
  // activity notification. Same shape as liveWorkspaceIds above. This used to
  // surface ONLY as an aggregate count in the sidebar masthead — a number with
  // no way to act on it, in a <span> styled like a button that did nothing on
  // click. The rail is a list of workspaces, so the workspace says it itself.
  const notifiedWorkspaceIds = (): Set<string> => {
    const notified = paneNotified();
    const s = new Set<string>();
    if (notified.size === 0) return s;
    for (const w of file().workspaces) {
      if (!w.layout) continue;
      if (collectPanes(w.layout).some((id) => notified.has(id))) s.add(w.id);
    }
    return s;
  };

  // Phase 26: pane_ids with a pending blocking feed item — these get
  // the notification ring. Recomputed reactively as feedItems changes.
  // Phase 84.B: the traffic light for every pane in the active workspace.
  // Computed here rather than inside PaneTabs so the strip and the pane
  // header go through the same trafficLight() with the same inputs — two
  // call sites deriving a colour independently is how they drift.
  //
  // BRIEF (commit 2): generalized to EVERY workspace. allPaneAgentRows is
  // the single source; the Queue panel, the sidebar attention set and the
  // active-workspace lights below all derive from it, so they cannot
  // disagree about a pane's state.
  const collectPaneNodes = (node: LayoutNode): PaneNode[] =>
    node.kind === "pane"
      ? [node]
      : [...collectPaneNodes(node.first), ...collectPaneNodes(node.second)];
  const allPaneAgentRows = (): QueueRow[] => {
    const runs = agentRuns();
    const waiting = waitingPaneIds();
    const connected = connectedPanes();
    const now = agentClockMs();
    const briefMap = briefs();
    const out: QueueRow[] = [];
    for (const w of file().workspaces) {
      if (!w.layout) continue;
      for (const pane of collectPaneNodes(w.layout)) {
        const pid = pane.pane_id;
        const run = runs[pid];
        out.push({
          wsId: w.id,
          wsName: w.name,
          paneId: pid,
          title: paneLabel(pane, {
            workspaceName: w.name,
            workspaceConnection: w.connection,
          }),
          state: run?.state ?? "unknown",
          stateSince: run?.stateSince ?? null,
          startedAt: run?.startedAt ?? null,
          waitingOnPermission: waiting.has(pid),
          connected: connected.has(pid),
          brief: briefMap[pid] ?? null,
          light: trafficLight({
            state: run?.state ?? "unknown",
            stateSince: run?.stateSince ?? null,
            waitingOnPermission: waiting.has(pid),
            connected: connected.has(pid),
            nowMs: now,
          }),
        });
      }
    }
    return out;
  };
  const paneAgentLights = (): Record<string, TrafficLight | null> => {
    const wsId = activeWs()?.id;
    const out: Record<string, TrafficLight | null> = {};
    if (!wsId) return out;
    for (const r of allPaneAgentRows()) {
      if (r.wsId === wsId) out[r.paneId] = r.light;
    }
    return out;
  };
  // BRIEF: workspaces holding a pane in the "needs you" buckets (blocked /
  // stuck / waiting-for-you). Superset of waitingWorkspaceIds — feeds the
  // sidebar's attention dot at a lower intensity than blocking red.
  const queueAttentionWorkspaceIds = (): Set<string> => {
    const s = new Set<string>();
    for (const r of allPaneAgentRows()) {
      if (inQueue(r) && QUEUE_BUCKET[queueStatus(r)] <= 1) s.add(r.wsId);
    }
    return s;
  };
  const paneAgentStateSince = (): Record<string, number | null> => {
    const runs = agentRuns();
    const out: Record<string, number | null> = {};
    for (const [pid, r] of Object.entries(runs)) out[pid] = r.stateSince;
    return out;
  };

  const waitingPaneIds = (): Set<string> => {
    const s = new Set<string>();
    for (const it of feedItems()) {
      if (it.state === "pending" && it.blocking && it.pane_id) s.add(it.pane_id);
    }
    return s;
  };
  // Phase 26: workspace_ids that contain at least one waiting pane —
  // drives the sidebar tab highlight.
  const waitingWorkspaceIds = (): Set<string> => {
    const s = new Set<string>();
    for (const it of feedItems()) {
      if (it.state === "pending" && it.blocking && it.workspace_id) {
        s.add(it.workspace_id);
      }
    }
    return s;
  };
  // beta.3 Fix 4: workspace_ids that received a *passive* hook (pre-tool-use
  // audit, stop, notification, or one of the new observability subkinds) in
  // the last 4 seconds. Feeds a soft amber breathing pulse on the sidebar
  // row so Yossi sees "something happened over there" without a modal ask.
  // Cleared by 4s decay + the row's own tick (App re-renders on any feed
  // change; the cutoff is recomputed each read). Blocking items are already
  // caught by `waitingWorkspaceIds` — this only adds the passive stream.
  const HOOK_PULSE_WINDOW_MS = 4_000;
  // Notification routing — the subkinds that "require something from you".
  // These get the OS sound (via the retained toast) + a blink (sidebar row +
  // pane border) + a Notification-Center entry with the workspace name.
  // Everything else is quiet history in the Notification Center only.
  // `pre-tool-use` normally arrives as a BLOCKING permission card (handled by
  // the blocking branch + `waitingWorkspaceIds`), so the pulse path effectively
  // fires for stop/notification; listing it here is harmless + future-proof.
  const MEANINGFUL_SUBKINDS = new Set(["stop", "notification", "pre-tool-use"]);
  // Sidebar pulse source, decoupled from feedItems(): passive `stop` no longer
  // lands in the feed, so we track {workspace_id -> last-meaningful-ts} here.
  // Decoupling also fixes the old FEED_AUTO_DISMISS_MS(3s) < window(4s) bug that
  // cut the pulse short.
  const [meaningfulPulses, setMeaningfulPulses] = createSignal<Map<string, number>>(new Map());
  const pulseWorkspace = (wsId: string) =>
    setMeaningfulPulses((prev) => {
      const now = Date.now();
      const n = new Map(prev);
      n.set(wsId, now);
      // Bound growth: drop entries already past the window.
      for (const [k, ts] of n) if (now - ts > HOOK_PULSE_WINDOW_MS) n.delete(k);
      return n;
    });
  const activeHookWorkspaceIds = (): Set<string> => {
    const s = new Set<string>();
    const now = Date.now();
    for (const [wsId, ts] of meaningfulPulses()) {
      if (now - ts <= HOOK_PULSE_WINDOW_MS) s.add(wsId);
    }
    return s;
  };
  // beta.3 Fix 4: 250ms ticker so the pulse fades on its own after 4s even
  // when no new feed items arrive. Piggybacks a signal `pulseTick` that
  // `activeHookWorkspaceIds` reads through (see below).
  const [pulseTick, setPulseTick] = createSignal(0);
  const pulseTimer = setInterval(() => setPulseTick((n) => n + 1), 250);
  onCleanup(() => clearInterval(pulseTimer));
  // issue #4: a reactive wall-clock the Ticker label reads through, so the
  // "M:SS" elapsed re-renders every pulse without any per-second backend event.
  const agentClockMs = (): number => {
    void pulseTick();
    return Date.now();
  };
  // Re-evaluate on tick — the closure reads pulseTick() so Solid tracks the dep.
  const activeHookWorkspaceIdsReactive = (): Set<string> => {
    void pulseTick();
    return activeHookWorkspaceIds();
  };

  // Phase 30 → Phase 31: live-update the OS window title from the
  // FOCUSED pane's effective identity (pane override falls back to
  // workspace). With pane-level identity, Yossi can see in Alt+Tab
  // which client he's looking at even when multiple panes from
  // different clients share the same workspace. Format:
  //   "🟣 ClientB ● — ymux"        (focused pane has title/identity)
  //   "🟦 ClientA — ymux"          (no focused pane → workspace fallback)
  // The ● appears when any pane in the active workspace is waiting
  // (cmux-style dirty indicator on the window itself).
  createEffect(() => {
    const ws = activeWs();
    if (!ws) {
      // Phase 65 (bug CC): swallow rejection — needs the
      // core:window:allow-set-title capability; a missing/denied perm
      // shouldn't surface as an unhandled promise rejection.
      void getCurrentWindow().setTitle("ymux").catch(() => {});
      return;
    }
    const parts: string[] = [];
    const pid = activePaneId();
    const focused = pid && ws.layout ? findPane(ws.layout, pid) : null;
    const ident = effectiveIdentity(focused ?? undefined, ws);
    if (ident.emoji) parts.push(ident.emoji);
    const focusedName =
      focused?.title ||
      (focused?.connection ? describeConnection(focused.connection) : null);
    parts.push(focusedName ?? ws.name);
    if (waitingWorkspaceIds().has(ws.id)) parts.push("●");
    const title = parts.join(" ") + " — ymux";
    void getCurrentWindow().setTitle(title).catch(() => {});
  });

  // Phase 41: when the user activates an SSH workspace and the setting is
  // on (default), establish a background SSH session so the tmux picker and
  // file manager populate without opening a terminal pane. Fire-and-forget;
  // the backend command is idempotent and skips password-mode workspaces.
  // The id guard fires once per workspace switch (the effect otherwise
  // re-runs on every file() change). We do NOT consume the workspace while
  // settings is still loading, so the initial workspace still auto-connects
  // once settings arrives.
  let lastAutoConnectWs: string | null = null;
  createEffect(() => {
    const ws = activeWs();
    const s = settings();
    if (!ws) {
      lastAutoConnectWs = null;
      return;
    }
    if (!s) return;
    if (ws.id === lastAutoConnectWs) return;
    lastAutoConnectWs = ws.id;
    if (s.auto_connect_on_workspace_select === false) return;
    if (!isRemoteWorkspace(ws)) return;
    void invoke("workspace_ensure_connected", { workspaceId: ws.id }).catch((e) =>
      log.warn("workspace_ensure_connected failed", e),
    );
  });

  // Phase 47: on workspace activation, if it's SSH and detection is on,
  // make sure the remote port-watcher is running for this workspace AND
  // replay the current detected_ports snapshot from the backend. Events
  // alone don't fill the FE signal when the workspace was previously
  // active in another session — the detected_ports state may exist on
  // the backend without the FE having seen its events.
  // Phase 47 → 62.C: ensure the remote port-watcher is running and pull
  // a fresh snapshot of detected ports into the FE signal. Extracted from
  // the workspace-activation effect so the Browser window (item C) can
  // call it on open / Refresh too — the Browser needs the port list even
  // when auto_port_forward is off (it forwards on demand per chosen port).
  const ensurePortsSnapshot = (wsId: string) => {
    void invoke("workspace_ensure_port_watcher", { workspaceId: wsId }).catch((e) =>
      log.warn("workspace_ensure_port_watcher failed", e),
    );
    void invoke<{ remote_port: number; addr: string; family: string }[]>(
      "list_detected_ports",
      { workspaceId: wsId },
    )
      .then((snapshot) => {
        setDetectedPorts((prev) => {
          // Replace this workspace's entries with the backend snapshot.
          const other = prev.filter((p) => p.workspace_id !== wsId);
          const mine = snapshot.map((d) => ({
            workspace_id: wsId,
            remote_port: d.remote_port,
            addr: d.addr,
            family: d.family,
          }));
          return [...other, ...mine];
        });
      })
      .catch((e) => log.warn("list_detected_ports failed", e));
  };

  let lastPortsEnsuredWs: string | null = null;
  createEffect(() => {
    const ws = activeWs();
    if (!ws) {
      lastPortsEnsuredWs = null;
      return;
    }
    // Re-fire when the workspace itself changes OR its toggle flips on
    // (so flipping the toggle "live" also kicks the watcher).
    const key = `${ws.id}:${ws.auto_port_forward ? 1 : 0}`;
    if (key === lastPortsEnsuredWs) return;
    lastPortsEnsuredWs = key;
    if (!isRemoteWorkspace(ws)) return;
    if (!ws.auto_port_forward) return;
    ensurePortsSnapshot(ws.id);
  });

  const reconcilePanes = (file: WorkspacesFile) => {
    const live = new Set<string>();
    for (const ws of file.workspaces) {
      if (ws.layout) for (const p of collectPanes(ws.layout)) live.add(p);
    }
    for (const [pid, ti] of [...terms]) {
      if (!live.has(pid)) {
        const sid = paneToSession.get(pid);
        if (sid) {
          sessionToPane.delete(sid);
          paneToSession.delete(pid);
        }
        ti.dispose();
        terms.delete(pid);
      }
    }
  };

  const updateFile = (f: WorkspacesFile) => {
    setFile(f);
    reconcilePanes(f);
    bump();
  };

  // ─── workspace mutations ────────────────────────────────────────────────

  // Phase 34 (hoisted in Phase 80): split a Help pane off the currently-
  // active workspace's focused pane. No-op when no workspace exists
  // (fresh-launch state). Shared by the edit modal and the setup wizard.
  const openSshHelp = () => {
    const ws = activeWs();
    const pid = activePaneId();
    if (!ws || !pid) return;
    void invoke("workspace_split", {
      workspaceId: ws.id,
      paneId: pid,
      direction: "horizontal",
      paneKind: "help",
      browserUrl: null,
      helpTopic: "ssh-key-setup",
    });
  };

  const handleCreate = async (input: CreateWorkspaceInput) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_create", { input });
      updateFile(f);
    } catch (e) {
      log.error("workspace_create failed", e);
    }
  };

  const handleUpdate = async (
    id: string,
    fields: {
      name?: string;
      color?: string;
      cwd?: string;
      setup_command?: string;
      teardown_command?: string;
      env?: EnvVar[];
      connection?: Connection;
    }
  ) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_update", {
        workspaceId: id,
        name: fields.name,
        color: fields.color,
        cwd: fields.cwd,
        setupCommand: fields.setup_command,
        teardownCommand: fields.teardown_command,
        env: fields.env,
        connection: fields.connection ?? null,
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_update failed", e);
    }
  };

  const handleRename = async (id: string) => {
    const ws = file().workspaces.find((w) => w.id === id);
    if (!ws) return;
    const next = window.prompt("Rename workspace", ws.name);
    if (!next || !next.trim()) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_rename", {
        workspaceId: id,
        name: next.trim(),
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_rename failed", e);
    }
  };

  /** Every workspace id under `id`, including it. Mirrors the backend
   *  BFS, visited set and all — a cycle here would hang the confirm. */
  const subtreeOf = (id: string): Workspace[] => {
    const all = file().workspaces;
    const out: Workspace[] = [];
    const seen = new Set<string>([id]);
    const queue = [id];
    while (queue.length) {
      const cur = queue.pop()!;
      const w = all.find((x) => x.id === cur);
      if (w) out.push(w);
      for (const c of all) {
        if (c.parent_id === cur && !seen.has(c.id)) {
          seen.add(c.id);
          queue.push(c.id);
        }
      }
    }
    return out;
  };

  /**
   * Deleting a workspace takes its pinned project folders and every
   * worktree workspace under them, along with any live remote session
   * they hold. Directories on the host are untouched.
   *
   * That used to be two stacked `window.confirm` calls — an unstyled OS
   * box describing the damage in prose. It read as no warning at all, so
   * the dialog IS the warning now: `ConfirmDeleteWorkspace` lists every
   * workspace by name, marks the live ones, and keeps focus on Cancel.
   */
  const [pendingDelete, setPendingDelete] = createSignal<Workspace[] | null>(null);

  /** Notes across a subtree. Legacy unassigned (null) notes survive. */
  const notesInSubtree = (subtree: Workspace[]): number => {
    const ids = new Set(subtree.map((w) => w.id));
    return notes().filter((n) => n.workspace_id && ids.has(n.workspace_id)).length;
  };

  const handleDelete = (id: string) => {
    const ws = file().workspaces.find((w) => w.id === id);
    if (!ws) return;
    setPendingDelete(subtreeOf(id));
  };

  const commitDelete = async (id: string) => {
    setPendingDelete(null);
    try {
      const f = await invoke<WorkspacesFile>("workspace_delete", {
        workspaceId: id,
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_delete failed", e);
      flashSummaryToast("err", String(e));
    }
  };

  const handleSetActive = async (id: string) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_set_active", {
        workspaceId: id,
      });
      updateFile(f);
      const ws = f.workspaces.find((w) => w.id === id);
      if (ws?.layout) {
        const panes = collectPanes(ws.layout);
        // Phase 84.A: come back to the tab you left. Falls through to the
        // first pane when this workspace hasn't been visited yet, or when
        // the remembered pane has since been closed.
        const remembered = lastPaneByWs.get(id);
        const pick =
          remembered && panes.includes(remembered) ? remembered : panes[0];
        if (pick) setActivePaneId(pick);
      }
    } catch (e) {
      log.error("workspace_set_active failed", e);
    }
  };

  // Phase 40: flip auto_port_forward from the Ports window. The command
  // returns the updated workspace; patch it into the file state.
  const handleToggleAutoForward = async (workspaceId: string, enabled: boolean) => {
    try {
      const updated = await invoke<Workspace>("workspace_set_auto_port_forward", {
        workspaceId,
        enabled,
      });
      const f = file();
      updateFile({
        ...f,
        workspaces: f.workspaces.map((w) => (w.id === updated.id ? updated : w)),
      });
    } catch (e) {
      log.error("workspace_set_auto_port_forward failed", e);
    }
  };

  const handleDisconnectWorkspace = async (id: string) => {
    const ws = file().workspaces.find((w) => w.id === id);
    if (!ws?.layout) return;
    for (const paneId of collectPanes(ws.layout)) {
      await disconnectPane(paneId);
    }
  };

  // ─── project folders ────────────────────────────────────────────────────

  // ── pinning a project folder ────────────────────────────────────────────
  //
  // Entry point is the workspace context menu, because that is what gives
  // the browser a host to walk: `file_list_remote` resolves SFTP from a
  // LIVE ssh session for that workspace. SSH gets the real remote browser
  // (DirPicker); Local gets the native dialog; WSL gets neither — there is
  // no SFTP for it (`WSL_CAPS.fileTransfer` is false) — so it falls back to
  // typing the path, which is what the pin modal is still there for.
  const [dirPickerFor, setDirPickerFor] =
    createSignal<{ workspaceId: string; connection: Connection | null } | null>(null);

  /** Probe, then persist. The probe is what keeps a non-repo directory
   *  from landing as a dead section — git's own message comes back. */
  const pinProjectFolder = async (
    parentWorkspaceId: string,
    path: string,
    connection: Connection | null,
  ) => {
    try {
      await invoke<WorktreeEntry[]>("git_probe_worktrees", { path, connection });
      const f = await invoke<WorkspacesFile>("workspace_pin_project_folder", {
        parentWorkspaceId,
        path,
        name: null,
      });
      updateFile(f);
      if (f.active_workspace_id) await handleSetActive(f.active_workspace_id);
    } catch (e) {
      log.error("pin project folder failed", e);
      flashSummaryToast("err", String(e));
    }
  };

  /**
   * Ask git again whether this workspace's directory is a repo.
   *
   * Demotion is one-way and deliberate — a folder git rejected should
   * not be re-probed on every expand and restart. But `git init` makes
   * the old answer wrong, and there was otherwise no route back:
   * `workspace_pin_project_folder` refuses a duplicate path under the
   * same parent, so re-pinning could not undo it either.
   */
  const recheckGit = async (workspaceId: string) => {
    const ws = file().workspaces.find((w) => w.id === workspaceId);
    if (!ws?.cwd) return;
    try {
      await invoke<WorktreeEntry[]>("git_probe_worktrees", {
        path: ws.cwd,
        connection: ws.connection ?? null,
      });
      const f = await invoke<WorkspacesFile>("workspace_set_project_root", {
        workspaceId,
        isProjectRoot: true,
      });
      updateFile(f);
      flashSummaryToast("ok", t("pf.checkGit.found", { name: ws.name }));
    } catch (e) {
      // git's own message: "not a git repository" and "no live SSH
      // session" are different problems and the user has to tell them
      // apart to know whether retrying is worth anything.
      log.info(`recheck git ws=${workspaceId} — ${String(e)}`);
      flashSummaryToast("err", String(e));
    }
  };

  const startPinProjectFolder = async (workspaceId: string) => {
    const ws = file().workspaces.find((w) => w.id === workspaceId);
    if (!ws) return;
    const conn = ws.connection ?? null;
    if (conn?.type === "ssh") {
      // Arm the connection first so pinning works on a workspace whose
      // panes are all disconnected — otherwise the browser opens straight
      // onto "connect a terminal pane first". Idempotent and PTY-free.
      try {
        await invoke("workspace_ensure_connected", { workspaceId });
      } catch (e) {
        log.warn("ensure_connected before folder pick failed", e);
      }
      setDirPickerFor({ workspaceId, connection: conn });
      return;
    }
    if (conn === null || conn.type === "local") {
      const picked = await openFileDialog({ directory: true, multiple: false });
      if (typeof picked === "string") await pinProjectFolder(workspaceId, picked, conn);
      return;
    }
    // WSL: no SFTP and no meaningful native path, so type it.
    setProjectFolderModal({ kind: "pin", workspaceId, connection: conn });
  };

  /** Last path component, for naming a detached worktree's workspace. */
  const pathTailOf = (path: string): string | undefined => {
    const norm = path.replace(/\\/g, "/").replace(/\/+$/, "");
    const tail = norm.slice(norm.lastIndexOf("/") + 1);
    return tail || undefined;
  };

  /** Reload the workspaces file after a project-folder mutation. */
  const reloadWorkspaces = async () => {
    const f = await invoke<WorkspacesFile>("workspaces_load");
    updateFile(f);
  };

  /**
   * Open a worktree as its own workspace — the click target of a
   * worktree row that has no workspace yet.
   *
   * The backend does the whole job (create + activate, or activate the
   * existing one), so there is nothing to stitch together here. The
   * workspace's first pane connects through the normal restore path;
   * its cwd reaches a remote shell via `effectiveCwdOverride`.
   */
  const openWorktree = async (rootWorkspaceId: string, wt: WorktreeEntry) => {
    const root = file().workspaces.find((w) => w.id === rootWorkspaceId);
    try {
      const f = await invoke<WorkspacesFile>("workspace_open_worktree", {
        rootWorkspaceId,
        worktreePath: wt.path,
        // Branch name is what the user recognises; a detached worktree
        // falls back to the directory name rather than a bare sha.
        name: wt.branch ?? pathTailOf(wt.path) ?? root?.name ?? "worktree",
      });
      updateFile(f);
      if (f.active_workspace_id) await handleSetActive(f.active_workspace_id);
    } catch (e) {
      log.error("workspace_open_worktree failed", e);
      flashSummaryToast("err", String(e));
    }
  };


  // ─── pane operations ────────────────────────────────────────────────────

  const splitPane = async (
    paneId: string,
    direction: SplitDirection,
    kind: "terminal" | "browser" | "filemanager" | "diff" = "terminal",
    browserUrl?: string
  ) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_split", {
        workspaceId: ws.id,
        paneId,
        direction,
        paneKind: kind,
        browserUrl: browserUrl ?? null,
      });
      updateFile(f);
    } catch (e) {
      log.error("split failed", e);
    }
  };

  // Phase 84.A: flip the active workspace between the split grid and the
  // tab strip. The layout tree is untouched — see workspace_set_tabs_mode.
  const setTabsMode = async (enabled: boolean) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_set_tabs_mode", {
        workspaceId: ws.id,
        tabsMode: enabled,
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_set_tabs_mode failed", e);
      return;
    }
    // Maximize is meaningless in tabs mode; clear it so flipping back to
    // split doesn't land in a stale zoom.
    setMaximizedPaneId(null);
    // Same fan-out as toggleMaximize: every pane's available area just
    // changed, so xterm has to catch up.
    queueMicrotask(() => {
      const cur = activeWs();
      if (!cur?.layout) return;
      for (const pid of collectPanes(cur.layout)) {
        terms.get(pid)?.fitAndResize();
      }
    });
  };

  // Phase 84.A: open a tab. workspace_split still builds a Split node —
  // that is deliberate, it's the tree we render again if the user flips
  // back to split mode. Splitting from the ACTIVE pane and focusing the
  // result makes collectPanes' in-order DFS walk match creation order, so
  // new tabs append instead of landing in the middle.
  //
  // workspace_split returns the whole WorkspacesFile rather than the new
  // pane_id, so diff the pane sets instead of changing a signature that
  // half a dozen call sites depend on.
  const newTab = async () => {
    const ws = activeWs();
    const pid = activePaneId();
    if (!ws?.layout || !pid) return;
    const before = new Set(collectPanes(ws.layout));
    await splitPane(pid, "horizontal");
    const after = activeWs()?.layout;
    if (!after) return;
    const added = collectPanes(after).find((p) => !before.has(p));
    if (added) focusPane(added);
  };

  // Phase 84.A: close a tab and land on a sensible neighbour. The
  // successor is picked from the order BEFORE the close, because after it
  // the index is gone.
  const closeTab = async (paneId: string) => {
    const layout = activeWs()?.layout;
    const idx = layout ? collectPanes(layout).indexOf(paneId) : -1;
    await closePane(paneId);
    if (activePaneId() !== paneId) return;
    const after = activeWs()?.layout;
    const next = after ? collectPanes(after) : [];
    // Closing the last pane leaves layout null; the Show around the strip
    // hides it, so there is nothing to focus.
    const pick = next[Math.min(Math.max(idx, 0), next.length - 1)];
    if (pick) focusPane(pick);
  };

  // beta.3 (pane-dragdrop): swap two panes' positions in the active
  // workspace's layout tree. Called by paneDrag.ts on pointerup — the
  // tree is mutated on the Rust side and the returned WorkspacesFile
  // is spread through updateFile, which reactively re-renders
  // LayoutView. Terminal instances survive because they're keyed by
  // pane_id in the g_terminals registry; PaneView's createEffect on
  // p.pane.pane_id detaches from the old slot and attaches to the new
  // one without touching the underlying xterm.
  const swapPanes = async (paneAId: string, paneBId: string) => {
    const ws = activeWs();
    if (!ws) return;
    if (paneAId === paneBId) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_swap_panes", {
        workspaceId: ws.id,
        paneAId,
        paneBId,
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_swap_panes failed", e);
    }
  };

  // Register the swap handler once. paneDrag.ts is a module-scope
  // store shared by every PaneView, so it needs the swap callback
  // installed before the user can initiate a drag.
  onMount(() => {
    setPaneSwapHandler((a, b) => swapPanes(a, b));
    onCleanup(() => setPaneSwapHandler(null));
  });

  const browserNavigate = async (paneId: string, url: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_navigate", {
        workspaceId: ws.id,
        paneId,
        url,
      });
      updateFile(f);
    } catch (e) {
      log.error("browser navigate failed", e);
    }
  };

  const browserGoBack = async (paneId: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_go_back", {
        workspaceId: ws.id,
        paneId,
      });
      updateFile(f);
    } catch (e) {
      log.error("browser go-back failed", e);
    }
  };

  const browserGoHome = async (paneId: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_go_home", {
        workspaceId: ws.id,
        paneId,
      });
      updateFile(f);
    } catch (e) {
      log.error("browser go-home failed", e);
    }
  };

  // Utility: collapse a workspace's layout back to a single terminal pane,
  // useful when you've split a workspace many times and want to start over.
  const handleResetLayout = async (id: string) => {
    if (
      !window.confirm(
        "Reset this workspace to a single terminal pane? All splits and browser panes in this workspace will be removed."
      )
    )
      return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_reset_layout", {
        workspaceId: id,
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_reset_layout failed", e);
    }
  };

  const browserSetForward = async (paneId: string, forward: boolean) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_set_forward", {
        workspaceId: ws.id,
        paneId,
        forward,
      });
      updateFile(f);
    } catch (e) {
      log.error("browser set-forward failed", e);
    }
  };

  const closePane = async (paneId: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_close_pane", {
        workspaceId: ws.id,
        paneId,
      });
      // The pane_id is retired; its restore hint can never match again.
      forgetPaneSession(paneId);
      updateFile(f);
    } catch (e) {
      log.error("close failed", e);
    }
  };

  let ratioCommitTimer: number | null = null;
  const setRatio = (splitId: string, ratio: number, commit: boolean) => {
    const ws = activeWs();
    if (!ws || !ws.layout) return;
    // Optimistic local update for instant feedback
    const updated = updateRatioInLayout(ws.layout, splitId, ratio);
    setFile({
      ...file(),
      workspaces: file().workspaces.map((w) =>
        w.id === ws.id ? { ...w, layout: updated } : w
      ),
    });
    // Trigger fit + pty_resize on all panes in this workspace
    queueMicrotask(() => {
      for (const pid of collectPanes(updated)) terms.get(pid)?.fitAndResize();
    });
    if (commit) {
      if (ratioCommitTimer) clearTimeout(ratioCommitTimer);
      invoke("workspace_set_split_ratio", {
        workspaceId: ws.id,
        splitId,
        ratio,
      }).catch(() => {});
    }
  };

  type ConnectOpts = {
    password?: string;
    keyPassphrase?: string;
    acceptUnknownHost?: boolean;
    persistent?: boolean;
    // Phase 12.B Smart Connect.
    mode?: "default" | "tmux" | "plain" | "cmd" | "claude";
    cwdOverride?: string;
    cmd?: string;
    claudeArgs?: string;
    // Phase 23.F: override tmux session name (picker path).
    tmuxSession?: string;
    // Phase 80: this connect was started by session restore, not by a click.
    // Failures must stay on the pane — never as a modal (see the catch below).
    restoring?: boolean;
  };

  /**
   * `Workspace.cwd` is honored natively only by the Local and WSL spawn
   * paths (`spawn_local_pty` sets `cmd.cwd`, `spawn_wsl_pty` passes
   * `--cd`). The SSH arm of `pane_connect` never forwards it, so an SSH
   * pane has always landed in `$HOME` and the workspace's cwd was
   * silently inert — an SSH channel has no working-directory parameter
   * to set, which is exactly what `cwdOverride` exists to paper over: it
   * runs a `cd '<dir>'` through `build_smart_connect_script` once the
   * shell is up.
   *
   * So for a remote workspace we default the override to the workspace's
   * own cwd. Project folders depend on this — a worktree workspace is
   * nothing but a cwd on a server — but the gap was never
   * worktree-specific, and narrowing the fix to worktrees would leave
   * every hand-set SSH cwd still quietly ignored.
   *
   * WSL is in the same boat for a different reason: `spawn_wsl_pty`
   * guards `--cd` with a WINDOWS-side `Path::new(d).is_dir()`, which a
   * Linux path like /home/y/src always fails, so the flag is silently
   * dropped. Injecting the `cd` covers it. Local is genuinely fine —
   * `spawn_local_pty` sets the process cwd directly — so it stays out.
   */
  const effectiveCwdOverride = (ws: Workspace, opts: ConnectOpts): string | null => {
    if (opts.cwdOverride) return opts.cwdOverride;
    if (!ws.cwd) return null;
    const needsInjection =
      isRemoteWorkspace(ws) || ws.connection?.type === "wsl";
    return needsInjection ? ws.cwd : null;
  };

  const connectPane = async (paneId: string, opts: ConnectOpts = {}) => {
    const ws = activeWs();
    if (!ws) return;
    const ti = ensureTerm(paneId);
    // Phase 62.B (item J): tag the terminal with its workspace so an
    // OSC 8 file:// link click knows which remote to SFTP-download from.
    ti.workspaceId = ws.id;
    // 2026-08-19: when ymux itself launches Claude, nothing has to be
    // detected. Claude Code writes RTL pre-reordered, so the pane must not
    // bidi it a second time — and the title-based detector never learns this
    // inside zellij, which eats the title (measured: `title-seen … match=0`
    // on every title, 57 chars of zellij's own).
    //
    // A RESTORE is left alone rather than cleared. Re-attaching to a
    // persistent session says nothing about what is running inside it — that
    // is the whole point of persistence — so clearing here would throw away a
    // signal a hook may have already delivered.
    if (opts.mode === "claude") ti.setTuiSignal(true);
    else if (!opts.restoring) ti.setTuiSignal(null);
    setStatus(paneId, "connecting…", false);
    try {
      const sessionId = await invoke<string>("pane_connect", {
        workspaceId: ws.id,
        paneId,
        password: opts.password ?? null,
        keyPassphrase: opts.keyPassphrase ?? null,
        acceptUnknownHost: opts.acceptUnknownHost ?? false,
        persistent: opts.persistent ?? false,
        mode: opts.mode ?? null,
        cwdOverride: effectiveCwdOverride(ws, opts),
        cmd: opts.cmd ?? null,
        claudeArgs: opts.claudeArgs ?? null,
        tmuxSessionName: opts.tmuxSession ?? null,
        cols: ti.term.cols || 80,
        rows: ti.term.rows || 24,
      });
      paneToSession.set(paneId, sessionId);
      sessionToPane.set(sessionId, paneId);
      ti.attach(sessionId);
      clearStatus(paneId);
      setPendingPwFor(null);
      setPendingPassphraseFor(null);
      setPendingHostTrust(null);
      bump();
      // Phase 11.A: persistence map refresh (the SshSession was just inserted
      // with its tmux_session field set or unset). Tiny delay so the handler
      // has finished registering.
      setTimeout(() => {
        void refreshPersistence().then(() => {
          // Session restore: the refreshed map is the backend's own answer to
          // "what tmux session is this pane on?", so we store the real name
          // rather than re-deriving sanitize_tmux_session_name here. A pane
          // that connected WITHOUT tmux has no entry — forget any stale hint
          // so the next start doesn't try to re-attach a session it left.
          const name = panePersistence()[paneId];
          if (name) rememberPaneSession(paneId, name);
          else forgetPaneSession(paneId);
        });
      }, 100);
    } catch (e) {
      const msg = String(e);
      // Phase 80: a restore-driven connect nobody asked for must not open a
      // modal. A password box or a "confirm this fingerprint" dialog thrown at
      // the user the instant the app opens is exactly the prompt they'd learn
      // to click through. Show the error on the pane and stop — the [Connect]
      // button is right there when they actually want it.
      if (opts.restoring) {
        setStatus(paneId, msg, true);
        return;
      }
      // KEY_PASSPHRASE_REQUIRED:<key_path>
      const pasReq = msg.match(/KEY_PASSPHRASE_REQUIRED:(.+)$/);
      if (pasReq) {
        setPendingPassphraseFor({ paneId, keyPath: pasReq[1] });
        setStatus(paneId, "key requires passphrase", false);
        return;
      }
      // KEY_PASSPHRASE_BAD:<key_path>:<inner_err>
      const pasBad = msg.match(/KEY_PASSPHRASE_BAD:([^:]+):/);
      if (pasBad) {
        setPendingPassphraseFor({
          paneId,
          keyPath: pasBad[1],
          bad: true,
        });
        setStatus(paneId, "wrong passphrase, try again", true);
        return;
      }
      // UNKNOWN_HOST:<target>:<key_type>:<fingerprint>
      const unk = msg.match(/UNKNOWN_HOST:([^:]+:\d+):([^:]+):(.+)$/);
      if (unk) {
        setPendingHostTrust({
          paneId,
          target: unk[1],
          keyType: unk[2],
          fingerprint: unk[3],
        });
        setStatus(paneId, "unknown host — confirm fingerprint", false);
        return;
      }
      // HOST_KEY_MISMATCH:<target>:<key_type>:<old_fp>:<new_fp>
      const mis = msg.match(/HOST_KEY_MISMATCH:([^:]+:\d+):([^:]+):([^:]+):(.+)$/);
      if (mis) {
        setPendingHostTrust({
          paneId,
          target: mis[1],
          keyType: mis[2],
          fingerprint: mis[4],
          mismatchOld: mis[3],
        });
        setStatus(paneId, "host key CHANGED — possible MITM!", true);
        return;
      }
      // Otherwise treat as a generic auth failure → password prompt for SSH
      setStatus(paneId, msg, true);
      const pane = findPaneInActiveWs(paneId);
      if (
        pane &&
        isRemoteConn(pane.connection) &&
        msg.includes("authentication failed")
      ) {
        setPendingPwFor(paneId);
      }
    }
  };

  // beta.3 (netfree, Track 1b): reconnect driver — defined AFTER
  // connectPane so the closure captures a valid binding at runtime.
  const startReconnect = (ev: SshDisconnectedEvent) => {
    // Every pane gets its own loop. A repeat drop for a pane already in
    // flight restarts that pane's loop only — siblings keep retrying.
    const paneId = ev.pane_id;
    if (reconnectRuns.has(paneId)) {
      const prev = reconnectRuns.get(paneId)!;
      prev.cancelled = true;
      clearReconnectTimer(paneId);
    }
    const run = { timer: null as number | null, cancelled: false };
    reconnectRuns.set(paneId, run);
    const state: ReconnectToast = {
      paneId,
      host: ev.host,
      workspaceId: ev.workspace_id,
      attempt: 0,
      max: RECONNECT_MAX,
    };
    setReconnectToasts((prev) => [...prev.filter((r) => r.paneId !== paneId), state]);
    const attemptOnce = async () => {
      // `run` is captured, so a loop superseded by a newer drop for the
      // same pane stops here instead of racing the new one.
      if (run.cancelled || reconnectRuns.get(paneId) !== run) return;
      const cur = reconnectToasts().find((r) => r.paneId === paneId);
      if (!cur) return;
      const nextAttempt = cur.attempt + 1;
      setReconnectToasts((prev) =>
        prev.map((r) => (r.paneId === paneId ? { ...r, attempt: nextAttempt } : r)),
      );
      try {
        // Reuse the existing pane_connect path — it does the full
        // handshake (host key check, auth via stored key / cached agent)
        // and, for persistent panes, re-runs `tmux new-session -A -s <name>`
        // which attaches to the still-alive server-side session.
        await connectPane(paneId, {
          persistent: ev.persistent,
          tmuxSession: ev.tmux_session_name ?? undefined,
        });
        // Success — drop this pane's row and confirm. Siblings still in
        // flight keep their rows and their loops.
        if (reconnectRuns.get(paneId) !== run) return;
        endReconnect(paneId);
        flashSummaryToast("ok", t("reconnect.success", { host: ev.host }));
      } catch (e) {
        // Attempt failed — schedule the next one, unless we're out of attempts.
        if (run.cancelled || reconnectRuns.get(paneId) !== run) return;
        if (nextAttempt >= RECONNECT_MAX) {
          endReconnect(paneId);
          flashSummaryToast("err", t("reconnect.failed", { host: ev.host }));
          // Best-effort clear of the server flag so a future drop can
          // re-emit cleanly.
          invoke("ssh_cancel_reconnect", { paneId }).catch(() => {});
          return;
        }
        const delay = reconnectJitter(RECONNECT_BACKOFFS_MS[nextAttempt]);
        run.timer = window.setTimeout(attemptOnce, delay);
      }
    };
    // First attempt runs after the first backoff (1s) — gives the network
    // a beat to recover before we spam it.
    run.timer = window.setTimeout(
      attemptOnce,
      reconnectJitter(RECONNECT_BACKOFFS_MS[0]),
    );
  };

  const disconnectPane = async (paneId: string) => {
    try {
      await invoke("pane_disconnect", { paneId });
    } catch (e) {
      log.warn("disconnect failed", e);
    }
    const sid = paneToSession.get(paneId);
    if (sid) {
      sessionToPane.delete(sid);
      paneToSession.delete(paneId);
    }
    terms.get(paneId)?.detach();
    bump();
    void refreshPersistence();
  };

  // Phase 11.A: hard-kill the remote tmux session (if any) and disconnect.
  const killSession = async (paneId: string) => {
    // 2026-08-20: the backend now REPORTS what it achieved. This used to be
    // `let killed = true` with a catch that flipped it — i.e. it only ever
    // learned about an IPC failure, never about a kill that did nothing, and
    // with zellij uninstalled that is exactly what happened.
    let out: KillSessionOutcome | null = null;
    try {
      out = await invoke<KillSessionOutcome>("pane_kill_session", { paneId });
    } catch (e) {
      log.warn("kill_session failed", e);
    }
    // Three states, not two. "We could not prove it" is its own answer.
    const killed =
      out !== null &&
      (out.result === "killed" ||
        out.result === "already_gone" ||
        out.result === "no_session");
    // `attempted` = the verb was sent but its exit status never came back
    // (SSH drained to EOF or timed out). Almost certainly fine, but not known
    // — so keep the hint and stay quiet. If the session really is gone,
    // restore drops the hint on the next start when the name is not listed;
    // if it survived, the hint is what brings it back. Self-correcting either
    // way, which a toast here would not be.
    if (!killed && out?.result !== "attempted") {
      log.warn("kill_session did not destroy the session", out);
      flashSummaryToast(
        "err",
        out?.result === "multiplexer_missing"
          ? t("pane.kill.noMultiplexer")
          : t("pane.kill.failed", { name: out?.session ?? paneId }),
      );
    }
    // The session is gone from the server — drop the restore hint so the next
    // start doesn't probe for a name that can never come back. (disconnectPane
    // deliberately keeps its hint: that path DETACHES, and the session lives.
    // A FAILED kill keeps its hint too — the session may well still be alive,
    // and forgetting it would cost the user their work on the next start.
    // That branch was unreachable until the outcome above existed.)
    if (killed) forgetPaneSession(paneId);
    const sid = paneToSession.get(paneId);
    if (sid) {
      sessionToPane.delete(sid);
      paneToSession.delete(paneId);
    }
    terms.get(paneId)?.detach();
    bump();
    void refreshPersistence();
  };

  // Phase 80: session restore — on app start, re-attach the ACTIVE workspace's
  // terminal panes to the tmux sessions they were on when the app last closed,
  // so the user lands back in their conversations instead of a grid of
  // [Connect] buttons.
  //
  // SSH *and* WSL. The gate is `caps.sessionPersistence`, not "is it SSH": a WSL
  // pane keeps its tmux session across an app restart exactly like a remote
  // one does. The hints were already being recorded for WSL by
  // refreshPersistence (pane_persistence_list reports WSL tmux names too) —
  // the restore loop was simply throwing them away.
  //
  // WSL restores WITHOUT the auth gate below: `wsl.exe` answers from cold, so
  // there is no handle to open and no prompt to throw. That is `sessionBound`.
  //
  // Why this works at all: closing a pane/app DETACHES tmux, never kills it
  // (DECISIONS "DD"), so the remote sessions are still alive. All we're
  // missing on the next start is WHICH session belonged to which pane —
  // that's the localStorage hint written by connectPane (sessionRestore.ts).
  //
  // Deliberate scope limits:
  //  - Active workspace only, once per app run. Restoring every workspace at
  //    boot would open an SSH channel per host before the user asked for one.
  //  - Panes with no remembered tmux session are left alone — a plain SSH
  //    shell leaves nothing on the server to come back to, so reconnecting it
  //    would just show an empty prompt where the user expected their work.
  //  - `workspace_ensure_connected` is the auth gate: it's headless and
  //    agent/key-only, and it never auto-accepts an unknown host key. A
  //    workspace needing a password / passphrase / host-key decision simply
  //    doesn't restore — boot never throws a prompt at the user.
  let sessionRestoreRan = false;

  // Wait until the pane's terminal is actually in the DOM. `attach()` fits and
  // pushes the real cols/rows to the remote, so connecting before PaneView has
  // mounted its container would hand tmux the 80×24 fallback and force a
  // visible redraw a moment later.
  const waitForPaneMount = async (paneId: string): Promise<void> => {
    const deadline = performance.now() + 3000;
    for (;;) {
      const ti = terms.get(paneId);
      if (ti?.container.isConnected) return;
      if (performance.now() >= deadline) return; // connect anyway; fit follows
      await new Promise((r) => setTimeout(r, 50));
    }
  };

  // Every exit path below says why it took it. Restore is invisible when it
  // works and indistinguishable from "feature not built" when it doesn't, and
  // the app only runs on Windows — so without this line, diagnosing a silent
  // no-op means a full build round-trip per guess. Goes to debug.log (dlog,
  // user-visible) AND the console. Pane ids and tmux session names are
  // metadata, the same class the backend already logs on connect — no PTY
  // content (Rule #1).
  const restoreLog = (msg: string): void => {
    console.log("session restore:", msg);
    void invoke("diag_log", { level: "info", msg: `[restore] ${msg}` }).catch(
      () => {},
    );
  };

  const restoreSessions = async (): Promise<void> => {
    if (sessionRestoreRan) return;
    const ws = activeWs();
    const s = settings();
    // Called once, from the end of onMount. No `settings()` means the load
    // above threw (corrupt settings.json) — the app is already degraded, and
    // reaching for the network on its own without knowing the user's
    // auto-connect preference is the wrong move. Skip for this run.
    if (!ws || !s) {
      restoreLog(`skip: not ready (workspace=${!!ws} settings=${!!s})`);
      return;
    }
    sessionRestoreRan = true;
    const wsId = ws.id;

    // Housekeeping first, so it happens even when nothing is restorable: drop
    // hints for panes, and file-manager directories for workspaces, that no
    // longer exist.
    const livePanes = new Set<string>();
    const liveWorkspaces = new Set<string>();
    for (const w of file().workspaces) {
      liveWorkspaces.add(w.id);
      if (w.layout) for (const p of collectPanes(w.layout)) livePanes.add(p);
    }
    prunePaneSessions(livePanes);
    pruneFmPaths(liveWorkspaces);
    // What survived pruning, before any filtering — so "the map is empty" and
    // "the map is fine but no pane matched" are never confused for each other.
    const hints = allPaneSessions();
    restoreLog(
      `remembered sessions: ${Object.keys(hints).length} ` +
        `[${Object.entries(hints).map(([k, v]) => `${k}→${v}`).join(", ")}]`,
    );

    // Opt-in (Settings → General). Off by default: restore makes startup reach
    // for the network on its own, and that should be the user's decision, not
    // something an update quietly starts doing to their servers.
    if (s.restore_sessions_on_start !== true) {
      restoreLog("skip: disabled in settings (restore_sessions_on_start)");
      return;
    }
    // Same opt-out as the headless workspace connect: a user who turned off
    // auto-connect doesn't want the app reaching for the network on its own.
    if (s.auto_connect_on_workspace_select === false) {
      restoreLog("skip: auto_connect_on_workspace_select is off");
      return;
    }
    if (!ws.layout) {
      restoreLog(`skip: workspace ${wsId} has no layout`);
      return;
    }

    const candidates: { paneId: string; tmux: string }[] = [];
    let seenTerminals = 0;
    let skippedNoTmux = 0;
    let skippedLive = 0;
    let skippedNoHint = 0;
    let skippedNotTerminal = 0;
    for (const paneId of collectPanes(ws.layout)) {
      const pane = findPane(ws.layout, paneId);
      if (!pane) continue;
      // paneKindOf, NOT `pane.pane_kind`: the field is only non-optional in
      // the generated binding. A layout written before Phase 35 has no
      // `pane_kind` at all, and reading it directly makes every legacy
      // terminal pane look like "not a terminal" — which silently emptied the
      // candidate list on exactly the installs that have sessions to restore.
      if (paneKindOf(pane) !== "terminal") {
        skippedNotTerminal++;
        continue;
      }
      seenTerminals++;
      if (!paneCaps(pane, ws.connection).sessionPersistence) {
        skippedNoTmux++;
        continue;
      }
      if (paneToSession.has(paneId)) {
        skippedLive++; // already live
        continue;
      }
      const tmux = getPaneSession(paneId);
      if (tmux) candidates.push({ paneId, tmux });
      else skippedNoHint++;
    }
    restoreLog(
      `workspace=${wsId} terminals=${seenTerminals} candidates=${candidates.length} ` +
        `(skipped: not-a-terminal=${skippedNotTerminal} no-tmux=${skippedNoTmux} ` +
        `already-connected=${skippedLive} no-remembered-session=${skippedNoHint})`,
    );
    // no-remembered-session on a FIRST run with this build is expected: the
    // hint is only written after a successful connect, so there is nothing to
    // come back to until the app has been closed once with a session attached.
    if (candidates.length === 0) return;

    // The liveness check runs for a workspace that declares its own
    // tmux-capable connection. Both commands below are workspace-scoped:
    // `workspace_ensure_connected` reads `workspace.connection`, and
    // `pane_list_tmux_sessions` answers at the workspace level. A workspace
    // whose PANES each carry their own connection (what `paneCaps` exists
    // for) has neither at boot — so gating on it meant that whole setup
    // silently never restored. Now it re-attaches directly and the pane's own
    // connection does the authenticating.
    //
    // WSL takes this same path: `pane_list_tmux_sessions` already has a WSL
    // branch that shells out through wsl_exec and needs no handle, and
    // `workspace_ensure_connected` returns Ok(()) for any non-SSH connection —
    // so the call below is a harmless no-op rather than a special case. That
    // is why `sessionBound` gates only the ensure-connected call.
    const wsc = wsCaps(ws);
    let alive: Set<string> | null = null;
    // Names that are EXITED-but-resurrectable, so the per-pane log below can
    // say which of the two kinds of re-attach it is doing. Accumulated from
    // both the workspace list and the per-pane probes; only ever read for a
    // log line, so a name collision across hosts costs nothing.
    const resurrectable = new Set<string>();
    if (wsc.sessionPersistence) {
      try {
        if (wsc.sessionBound) {
          await invoke("workspace_ensure_connected", { workspaceId: ws.id });
        }
      } catch (e) {
        restoreLog(`abort: ensure_connected failed — ${String(e)}`);
        return;
      }
      let sessions: TmuxSessionInfo[] = [];
      try {
        // No projectPath: restore must see EVERY session on the host.
        // A pane whose session sits outside the workspace's folder still has
        // to come back — scoping this call would silently strand it.
        sessions = await invoke<TmuxSessionInfo[]>("pane_list_tmux_sessions", {
          workspaceId: ws.id,
          projectPath: null,
        });
      } catch (e) {
        restoreLog(`abort: list_tmux_sessions failed — ${String(e)}`);
        return;
      }
      // An EMPTY list is ambiguous: on SSH it also means "no live handle" (the
      // command returns Ok([]) rather than erroring — password-auth workspaces
      // land here), and on WSL it is what a wsl_exec failure degrades to.
      // Treat it as "can't tell" and keep every hint, so a workspace that
      // merely needs a prompt — or a distro that was asleep — still restores
      // tomorrow. Erring this way is deliberate: attaching on a guess would
      // have `tmux new-session -A` CREATE an empty session under the
      // remembered name, destroying the very hint we came back for.
      if (sessions.length === 0) {
        restoreLog(
          "abort: reported 0 tmux sessions — ambiguous (also what 'no live SSH " +
            "handle' looks like, e.g. a password-auth workspace the headless " +
            "connect can't open, or a WSL distro that failed to answer). " +
            "Hints kept, nothing restored.",
        );
        return;
      }
      // EXITED sessions are counted as alive ON PURPOSE. Do not "fix" this
      // with `.filter(s => !s.exited)`.
      //
      // A Windows reboot leaves EVERY zellij session EXITED, and `attach -c`
      // resurrects it — that is the one thing zellij does that tmux cannot,
      // and the reason it was adopted at all (docs/DECISIONS.md). This line
      // feeding attach IS the reboot-restore path; filtering here would delete
      // the feature.
      //
      // The guarantee that a session the user KILLED does not come back lives
      // in pane_kill_session, which since 2026-08-20 sends `delete-session -f`
      // — so a killed session is not in this list at all, EXITED or otherwise,
      // and the `!liveNames.has()` branch below drops its hint on its own.
      alive = new Set(sessions.map((x) => x.name));
      for (const x of sessions) if (x.exited) resurrectable.add(x.name);
      const dead = sessions.filter((x) => x.exited).length;
      restoreLog(
        `server has ${sessions.length - dead} live and ${dead} resurrectable session(s)`,
      );
    } else {
      // Per-pane connections: ask over the PANE's own connection instead
      // (pane_probe_tmux_sessions). A null answer means "couldn't ask" — the
      // pane is then left alone. Nothing gets attached on a guess: `tmux
      // new-session -A` would CREATE an empty session under the remembered
      // name, which is the opposite of what's wanted when the session is gone.
      restoreLog(
        "workspace has no connection of its own — its panes carry theirs; " +
          "probing tmux over each pane's connection",
      );
    }

    // One probe per host, not per pane: several panes usually share a
    // connection, and each probe is a full SSH handshake.
    const probes = new Map<string, Set<string> | null>();
    const aliveFor = async (paneId: string): Promise<Set<string> | null> => {
      if (alive) return alive; // workspace-level list already covers every pane
      const pane = ws.layout ? findPane(ws.layout, paneId) : null;
      const conn = pane?.connection ?? ws.connection ?? null;
      const key = JSON.stringify(conn);
      const cached = probes.get(key);
      if (cached !== undefined) return cached;
      let result: Set<string> | null = null;
      try {
        // macOS local pane: ask the LOCAL tmux server directly (workspace-
        // scoped, no SSH arming — `workspace_ensure_connected` is SSH-only).
        // The answer is authoritative: [] really means "no live sessions".
        const list = isMac() && isLocalConn(conn)
          // projectPath: null — restore is unscoped, see above.
          ? await invoke<TmuxSessionInfo[]>("pane_list_tmux_sessions", { workspaceId: wsId, projectPath: null })
          : await invoke<TmuxSessionInfo[] | null>(
              "pane_probe_tmux_sessions",
              { workspaceId: wsId, paneId },
            );
        // null = "couldn't ask" (not SSH, or the headless connect failed:
        // password-only, passphrase-locked, unknown host key, host down).
        // Distinct from [] = "asked, the host has no sessions".
        result = list ? new Set(list.map((x) => x.name)) : null;
        if (list) for (const x of list) if (x.exited) resurrectable.add(x.name);
        restoreLog(
          result
            ? `probe: host has ${result.size} live tmux session(s)`
            : "probe: could not reach the host headlessly — those panes stay on [Connect]",
        );
      } catch (e) {
        restoreLog(`probe failed — ${String(e)}`);
      }
      probes.set(key, result);
      return result;
    };

    // Sequential on purpose: N parallel connects would open N SSH channels
    // and N tmux attaches in the same instant on a freshly-armed connection.
    let restored = 0;
    for (const c of candidates) {
      const liveNames = await aliveFor(c.paneId);
      // Nothing is attached on a guess. `tmux new-session -A` CREATES the
      // session when it's missing, so attaching without a confirmed answer
      // would hand the user a blank shell wearing their old session's name.
      if (liveNames === null) {
        restoreLog(
          `pane ${c.paneId}: liveness unknown — left on [Connect], hint kept for next time`,
        );
        continue;
      }
      if (!liveNames.has(c.tmux)) {
        restoreLog(
          `pane ${c.paneId}: remembered session "${c.tmux}" is gone from the server — left on [Connect], hint dropped`,
        );
        forgetPaneSession(c.paneId); // session died on the server — stop trying
        continue;
      }
      await waitForPaneMount(c.paneId);
      // Both guards are re-checked HERE, not once up front: the loop spans
      // seconds, and the user is free to act during them.
      //  - a different active workspace means connectPane (which resolves the
      //    workspace itself, from activeWs()) would send our pane id with the
      //    wrong workspace — a guaranteed "no pane" error, plus a terminal
      //    mistagged with a host it doesn't belong to. Give up; the user is
      //    somewhere else now.
      //  - a pane that came alive while we waited was connected by the user
      //    clicking [Connect]. pane_connect kills any prior session for the
      //    pane, so restoring on top of it would throw away what they just
      //    started.
      if (activeWs()?.id !== wsId) {
        restoreLog(
          `stop: active workspace changed mid-restore (${restored}/${candidates.length} done)`,
        );
        return;
      }
      if (paneToSession.has(c.paneId)) {
        restoreLog(`pane ${c.paneId}: connected manually while waiting — left alone`);
        continue;
      }
      // Say WHICH of the two this is. The log could not tell a live reattach
      // from a resurrection, which is why "a killed session came back" took a
      // code read to spot instead of showing up in debug.log.
      restoreLog(
        `pane ${c.paneId}: re-attaching to "${c.tmux}"` +
          (resurrectable.has(c.tmux) ? " (resurrecting a saved session)" : ""),
      );
      await connectPane(c.paneId, {
        persistent: true,
        tmuxSession: c.tmux,
        restoring: true,
      });
      restored++;
    }
    restoreLog(`done: ${restored}/${candidates.length} pane(s) re-attached`);
  };

  const findPaneInActiveWs = (paneId: string) => {
    const ws = activeWs();
    if (!ws?.layout) return null;
    const search = (n: LayoutNode): any => {
      if (n.kind === "pane") return n.pane_id === paneId ? n : null;
      return search(n.first) ?? search(n.second);
    };
    return search(ws.layout);
  };

  // Phase 58: push-to-talk start/stop. Lazily constructs the
  // recorder, drives the indicator, and pastes the returned text
  // into the focused terminal pane on success.
  const startPushToTalk = () => {
    const stt = settings()?.stt;
    if (!stt?.enabled) return;
    setSttError(null);
    const rec = makeSttRecorder(stt.backend, stt.language || "auto");
    sttRecorder = rec;
    setSttListening(true);
    rec
      .start()
      .then((text) => {
        // Chars only, never the transcript itself (Rule #1).
        log.info(`stt result: backend=${stt.backend} chars=${text.length}`);
        if (text && text.length > 0) {
          pasteIntoActiveTerminal(text);
        }
      })
      .catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        log.error(`stt failed: backend=${stt.backend} — ${msg}`);
        setSttError(msg);
        // Auto-clear after 5s so the toast doesn't linger forever.
        setTimeout(() => setSttError(null), 5000);
      })
      .finally(() => {
        sttRecorder = null;
        setSttListening(false);
      });
  };
  const stopPushToTalk = () => {
    if (!sttRecorder) return;
    try {
      sttRecorder.stop();
    } catch (e) {
      log.warn("stt stop failed", e);
    }
  };

  // Phase 55-B → 60: distribute split ratios evenly. Phase 60
  // (smoke-test 4.2) made the reset OPTIMISTIC: apply the 0.5 ratios
  // to the local file() signal immediately, then let the backend
  // persist + return the canonical snapshot. The visual reset is now
  // instant and independent of the invoke round-trip, and if the
  // backend errors the next workspaces:changed refresh reconciles.
  const distributeEvenly = async () => {
    const ws = activeWs();
    if (!ws) return;
    // Optimistic local pass — walk the layout, reset every ratio.
    const resetRatios = (n: LayoutNode): LayoutNode =>
      n.kind === "split"
        ? { ...n, ratio: 0.5, first: resetRatios(n.first), second: resetRatios(n.second) }
        : n;
    if (ws.layout) {
      const updated = resetRatios(ws.layout);
      setFile({
        ...file(),
        workspaces: file().workspaces.map((w) =>
          w.id === ws.id ? { ...w, layout: updated } : w,
        ),
      });
      queueMicrotask(() => {
        for (const pid of collectPanes(updated)) {
          terms.get(pid)?.fitAndResize();
        }
      });
    }
    try {
      const f = await invoke<WorkspacesFile>("workspace_distribute_evenly", {
        workspaceId: ws.id,
      });
      updateFile(f);
    } catch (e) {
      log.error("workspace_distribute_evenly failed", e);
    }
  };

  // Phase 55-A: maximize toggle. Setting/clearing the signal swaps
  // LayoutView's `node` between the full split tree and the lone
  // leaf; fit+resize fires for every pane in the workspace after the
  // signal flips so xterm catches up to the new available area.
  const toggleMaximize = (paneId?: string) => {
    // Phase 84.A: in tabs mode every pane is already full-screen, so
    // maximize has nothing to do. One guard here disables Ctrl+Enter,
    // Ctrl+Shift+Z, the Esc restore, the double-click gesture and the
    // ymux:pane-maximize event in a single place.
    if (tabsMode()) return;
    const cur = maximizedPaneId();
    if (cur) {
      setMaximizedPaneId(null);
    } else {
      const target = paneId ?? activePaneId();
      if (!target) return;
      setMaximizedPaneId(target);
    }
    queueMicrotask(() => {
      const ws = activeWs();
      if (!ws?.layout) return;
      for (const pid of collectPanes(ws.layout)) {
        terms.get(pid)?.fitAndResize();
      }
    });
  };

  // ─── keyboard shortcuts ─────────────────────────────────────────────────

  // ─── the keyboard binding table ─────────────────────────────────────────
  //
  // Phase 87: every accelerator below used to be a hardcoded `if` in
  // handleKey with no way to rebind it. They now come out of
  // `shortcutTable()`, which is rebuilt from `settings.shortcuts` on every
  // settings:changed — so rebinding one in Settings takes effect without a
  // relaunch.
  //
  // `when` is an EVENT-aware gate, and it is deliberately NOT the same
  // thing as "this action is unavailable": returning false means "not
  // mine, keep scanning and then let the key through", never "swallow
  // it". That distinction is load-bearing for toggle_sidebar_soft (plain
  // Ctrl+B), which has to reach a focused terminal because Ctrl+b is
  // tmux's prefix.
  //
  // `run` owns its own preventDefault(), exactly as each old branch did —
  // `copy` deliberately does NOT preventDefault when the terminal had no
  // selection, so a native text-selection copy still works elsewhere.
  //
  // Built once: every closure reads its reactive state at call time.
  interface KeyBinding {
    id: ShortcutActionId;
    when?: (e: KeyboardEvent) => boolean;
    run: (e: KeyboardEvent) => void;
  }
  const inTerminal = (e: KeyboardEvent): boolean =>
    !!(e.target as HTMLElement | null)?.closest?.(".terminal-container");
  const hasActivePane = (): boolean => !!activePaneId();
  const quadrant = (v: "up" | "down", h: "left" | "right") => {
    splitOrMove(v);
    // Tiny delay so the first split's layout update lands in file() before
    // the second hop reads it. setTimeout(0) is enough for Solid's
    // reactive batch + the Tauri round-trip.
    setTimeout(() => splitOrMove(h), 0);
  };
  const keyBindings: KeyBinding[] = [
    // ── tabs. First in the table so a rebind elsewhere can't shadow tab
    //    cycling. Gated on tabsMode() so split-mode workspaces — and the
    //    terminal apps running in them — keep every key they have today.
    //    Deliberately NOT binding Ctrl+T: readline uses it (transpose-chars).
    { id: "tab_next", when: tabsMode, run: (e) => { e.preventDefault(); focusAdjacentPane(1); } },
    { id: "tab_prev", when: tabsMode, run: (e) => { e.preventDefault(); focusAdjacentPane(-1); } },

    // ── pane geometry ──
    // Phase 55-A: maximize the active pane. tmux uses Ctrl+b z for the same
    // gesture; raw Ctrl+Enter is a ymux-specific convenience.
    { id: "toggle_maximize", run: (e) => { e.preventDefault(); toggleMaximize(); } },
    // Phase 65.T / V: the explicit Focus/Zoom hotkey, alongside
    // toggle_maximize / double-click / the pane-header ⛶ button — mnemonic
    // matches tmux's Prefix+z zoom. Its default was Ctrl+Shift+M until bug
    // V: that collides with STT push-to-talk, which is exactly the class of
    // clash conflictingAccels() now surfaces in Settings.
    { id: "focus_zoom", run: (e) => { e.preventDefault(); toggleMaximize(); } },
    // v0.4.4-beta.2: reset the active terminal — clears leaked mouse-tracking
    // modes (the escape-text leak from an unclean vim/fzf/less exit) + text
    // attributes.
    { id: "reset_terminal", when: hasActivePane, run: (e) => {
      e.preventDefault();
      const pid = activePaneId();
      if (pid) terms.get(pid)?.resetTerminal();
    } },

    // ── app chrome ──
    // Phase 35 (#1.3): the command palette.
    { id: "command_palette", run: (e) => { e.preventDefault(); setShowPalette((v) => !v); } },
    // Phase 65.W: the GLOBAL sidebar toggle — works everywhere, including
    // when an xterm pane or the FileManager has focus.
    { id: "toggle_sidebar", run: (e) => { e.preventDefault(); cycleSidebarMode(); } },
    // Phase 62.B (item I) / 65.P: the same toggle on a bare Ctrl+B, but ONLY
    // outside a terminal. We can't make it global: inside an xterm pane
    // Ctrl+b is tmux's prefix and must reach the PTY, and stealing it would
    // break every tmux keybinding. `when` false ⇒ skip and keep scanning ⇒
    // the event reaches the terminal.
    { id: "toggle_sidebar_soft", when: (e) => !inTerminal(e), run: (e) => {
      e.preventDefault();
      cycleSidebarMode();
    } },
    { id: "toggle_notes", run: (e) => { e.preventDefault(); setShowNotes((v) => !v); } },
    { id: "toggle_settings", run: (e) => { e.preventDefault(); setShowSettings((v) => !v); } },
    { id: "new_workspace", run: (e) => { e.preventDefault(); setShowSetup({}); } },
    // BRIEF: the cross-workspace agent Queue.
    { id: "toggle_queue", run: (e) => {
      e.preventDefault();
      if (surfaceOf("queue") === "closed") openPanel("queue");
      else closePanel("queue");
    } },

    // ── clipboard ──
    { id: "copy", run: (e) => {
      // Try the focused terminal first; if it has a selection, copy.
      // Otherwise let the browser handle the event (which may be a
      // text-selection copy in a non-terminal pane) — hence no blanket
      // preventDefault, and none before the promise resolves either.
      void copyTerminalSelection().then((handled) => {
        if (handled) e.preventDefault();
      });
    } },
    { id: "paste", run: (e) => {
      e.preventDefault();
      // readClipboardText, not navigator.clipboard.readText: WebView2 denies
      // clipboard READ (while allowing write), so this shortcut silently did
      // nothing. Host-side read via the Rust command instead.
      readClipboardText().then((text) => {
        if (text) pasteIntoActiveTerminal(text);
      }).catch((err) => log.warn("paste failed", err));
    } },
    // Phase 17: Claude session summary.
    { id: "summarize_claude", run: (e) => { e.preventDefault(); void summarizeActivePane(); } },

    // ── layout ──
    // Phase 55-B → 60 (smoke-test 4.2): distribute splits evenly. The
    // original check also matched e.key === "+" and e.code === "Equal"
    // directly, because on a Hebrew layout Ctrl+Alt is AltGr and e.key can
    // come back as something else depending on the compose state. matches()
    // already falls back to the PHYSICAL key via physicalKey(), so that
    // special case is redundant here.
    { id: "distribute_evenly", run: (e) => { e.preventDefault(); void distributeEvenly(); } },
    // Phase 48-E: split-or-move. Focus the neighbour in that direction if one
    // exists, else split the current pane in that direction.
    { id: "split_or_move_left", run: (e) => { e.preventDefault(); splitOrMove("left"); } },
    { id: "split_or_move_right", run: (e) => { e.preventDefault(); splitOrMove("right"); } },
    { id: "split_or_move_up", run: (e) => { e.preventDefault(); splitOrMove("up"); } },
    { id: "split_or_move_down", run: (e) => { e.preventDefault(); splitOrMove("down"); } },
    // Phase 49-D: land the active pane in a quadrant. From a single pane:
    // vertical split + horizontal split puts the current pane in one of the
    // four corners. From an existing layout: two split-or-move hops in the
    // corner's direction pair. The 50-50 split convention means the result is
    // approximate — good enough for the common 1-pane and 2-pane starts;
    // complex layouts may land off-corner.
    { id: "quadrant_top_left", run: (e) => { e.preventDefault(); quadrant("up", "left"); } },
    { id: "quadrant_top_right", run: (e) => { e.preventDefault(); quadrant("up", "right"); } },
    { id: "quadrant_bottom_left", run: (e) => { e.preventDefault(); quadrant("down", "left"); } },
    { id: "quadrant_bottom_right", run: (e) => { e.preventDefault(); quadrant("down", "right"); } },

    // ── split / close. Pane-relative: bound to the ACTIVE pane rather than
    //    a global action, so `when` gates on there being one. Phase 84.A: in
    //    tabs mode both split keys mean "new tab" (there is no visible split
    //    to aim at) and close routes through closeTab so focus lands on a
    //    neighbour instead of nowhere.
    { id: "split_horizontal", when: hasActivePane, run: (e) => {
      e.preventDefault();
      const pid = activePaneId();
      if (!pid) return;
      if (tabsMode()) void newTab();
      else void splitPane(pid, "horizontal");
    } },
    { id: "split_vertical", when: hasActivePane, run: (e) => {
      e.preventDefault();
      const pid = activePaneId();
      if (!pid) return;
      if (tabsMode()) void newTab();
      else void splitPane(pid, "vertical");
    } },
    { id: "close_pane", when: hasActivePane, run: (e) => {
      e.preventDefault();
      const pid = activePaneId();
      if (!pid) return;
      if (tabsMode()) void closeTab(pid);
      else void closePane(pid);
    } },
  ];

  const handleKey = (e: KeyboardEvent) => {
    // ── Phase 84.A: Ctrl+1..9 jumps to tab N. Stays OUT of the table: it is
    // a numeric family of nine bindings where 9 means "last", not "ninth",
    // and a ParsedShortcut holds exactly one key. Settings lists it
    // read-only under "fixed shortcuts". Runs before the table so a rebound
    // accelerator can't shadow it.
    if (tabsMode() && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey) {
      for (let n = 1; n <= 9; n++) {
        if (!keyEq(e, String(n))) continue;
        e.preventDefault();
        const layout = activeWs()?.layout;
        const panes = layout ? collectPanes(layout) : [];
        // 9 means "last", the way browsers do it, so the shortcut is useful
        // in a workspace with more than nine tabs.
        const pick = n === 9 ? panes[panes.length - 1] : panes[n - 1];
        if (pick) focusPane(pick);
        return;
      }
    }
    // ── bare Escape. Also out of the table, and not close: these are
    // contextual DISMISSALS, not accelerators. Each acts only when there is
    // something to dismiss and otherwise falls through so the escape
    // sequence reaches the PTY. Rebinding Escape away would leave a user
    // with a maximized pane and no way out, and gains nobody anything.
    if (e.key === "Escape") {
      // A fullscreen side panel sits above the panes (z 95), so Esc collapses
      // it back to a drawer first — before the pane-maximize restore below.
      const fs = (Object.keys(panels()) as PanelId[]).find(
        (id) => panels()[id] === "fullscreen",
      );
      if (fs) {
        e.preventDefault();
        setSurface(fs, "drawer");
        return;
      }
      // Restore ONLY when something is maximized (otherwise we step on
      // terminal escape sequences).
      if (maximizedPaneId()) {
        e.preventDefault();
        toggleMaximize();
        return;
      }
    }
    // ── the configurable table. First match wins. matches() compares all
    // four modifiers AND the key exactly, so there is no prefix ambiguity
    // between entries: order only decides who wins a duplicate the user
    // created, which the Shortcuts tab flags in red.
    const sc = shortcutTable();
    for (const b of keyBindings) {
      if (!matches(e, sc[b.id])) continue;
      if (b.when && !b.when(e)) continue; // not mine — let the key through
      b.run(e);
      return;
    }
    // Phase 58: push-to-talk (down). Lives in settings.stt, not
    // settings.shortcuts, so it is parsed on every press rather than read
    // out of the table — cheap, and it lets the settings edit take effect
    // without a relaunch. Repeats are suppressed by the sttRecorder guard.
    // conflictingAccels() still sees it, so a clash with a table binding is
    // reported in Settings even though the two live in different schemas.
    const stt = settings()?.stt;
    if (stt?.enabled) {
      const accel = parseShortcut(stt.push_to_talk_hotkey);
      if (accel && matches(e, accel) && !sttRecorder) {
        e.preventDefault();
        startPushToTalk();
        return;
      }
    }
  };

  // ─── lifecycle ──────────────────────────────────────────────────────────

  const refreshFromBackend = async () => {
    try {
      const prevActive = file().active_workspace_id;
      const f = await invoke<WorkspacesFile>("workspaces_load");
      updateFile(f);
      // If active workspace changed externally (e.g. via CLI), pick a pane to focus.
      if (
        f.active_workspace_id &&
        f.active_workspace_id !== prevActive
      ) {
        const ws = f.workspaces.find((w) => w.id === f.active_workspace_id);
        if (ws?.layout) {
          const firstPane = collectPanes(ws.layout)[0];
          if (firstPane) setActivePaneId(firstPane);
        }
      }
    } catch (e) {
      log.error("refreshFromBackend failed", e);
    }
  };

  onMount(async () => {
    // Phase 81.B: one global subscription to the SFTP transfer events.
    // Transfers start from several places (File Manager, terminal
    // drag-drop, OSC 8 links) but all feed the same store, so the
    // listener belongs at the root rather than in any one pane.
    void initTransferListener();

    // Phase 48-D: lightweight UI-stall instrumentation. A 100ms heartbeat
    // measures actual elapsed vs expected and reports gaps >300ms; a
    // PerformanceObserver on `longtask` reports any single task >200ms.
    // Both go to debug.log via the unified logger so future
    // support tickets can correlate UI jank with backend activity.
    // No cleanup: these run for the app's lifetime.
    {
      const HEARTBEAT_MS = 100;
      const STALL_THRESHOLD_MS = 300;
      const LONGTASK_THRESHOLD_MS = 200;
      let lastTick = performance.now();
      window.setInterval(() => {
        const now = performance.now();
        const gap = now - lastTick;
        lastTick = now;
        // macOS: WebKit suspends a fully-occluded page's timers to 1Hz, so a
        // hidden window reports a ~1000ms "stall" every second — throttling,
        // not jank, and it buried the real signal under ~60 warns/minute.
        // Only measure while the page is actually visible.
        if (gap > STALL_THRESHOLD_MS && !document.hidden) {
          log.warn(`UI stall: ${Math.round(gap)}ms (expected ~${HEARTBEAT_MS}ms)`);
        }
      }, HEARTBEAT_MS);
      // Re-baseline on show, else the first visible tick reports the whole
      // hidden stretch as one giant stall. (Lost in the zellij merge and
      // restored 2026-08-20 — the merge kept the heartbeat and dropped this.)
      document.addEventListener("visibilitychange", () => {
        lastTick = performance.now();
      });
      try {
        const obs = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            if (entry.duration > LONGTASK_THRESHOLD_MS) {
              log.warn(`longtask ${entry.name || "(anon)"} ${Math.round(entry.duration)}ms`);
            }
          }
        });
        obs.observe({ entryTypes: ["longtask"] });
      } catch {
        // Some WebView versions don't support the longtask entry type — skip.
      }
    }

    // Phase 9.A: load + apply settings as early as possible so the splash
    // colors don't pop to a different palette on first paint.
    try {
      const s = await loadSettings();
      setSettings(s);
      setLoggerLevel(s.logs?.level ?? "info");
      applyTheme(s);
      // Design Pass 01 (#2): re-tint if the OS scheme flips while on "system".
      watchSystemTheme(() => settings() ?? s);
      applyI18nSettings(s.i18n);
      // #1: seed the Notification Center with any notifications already
      // collected this session (RPC/agent items live in the backend Vec).
      try {
        const seed = await invoke<NotifItem[]>("notifications_list");
        setNotifications(seed.map((n) => ({ ...n, kind: n.kind || "agent" })).reverse());
      } catch (e) {
        log.warn("notifications_list failed", e);
      }
      setShortcutTable(buildShortcutTable(s.shortcuts ?? DEFAULT_SHORTCUTS));
      setCtrlCCopyOnSelect(
        (s.shortcuts ?? DEFAULT_SHORTCUTS).copy_on_select_with_ctrl_c,
      );
    } catch (e) {
      log.warn("settings_load failed", e);
    }
    await refreshFromBackend();
    const ws0 = file().workspaces.find((w) => w.id === file().active_workspace_id);
    if (ws0?.layout) {
      const p0 = collectPanes(ws0.layout)[0];
      if (p0) setActivePaneId(p0);
    }

    // Phase 84.B: seed the traffic lights from the backend's live state.
    // Without this every webview reload (F5, devtools, an HMR round in
    // dev) blanks every light until the next hook fires — and for an agent
    // sitting idle waiting on the user, that could be never. The backend
    // keeps this in memory only, so an app restart legitimately starts
    // empty; it is a reload we are covering, which is far more common.
    try {
      const snaps = await invoke<Record<string, PaneAgentSnapshot>>(
        "pane_agent_states",
      );
      const seeded: ReturnType<typeof agentRuns> = {};
      for (const [pid, s2] of Object.entries(snaps)) {
        seeded[pid] = {
          startedAt: s2.started_at,
          avgMs: s2.avg_ms,
          state: s2.state as PaneAgentState,
          stateSince: s2.state_since,
          seq: s2.seq,
        };
      }
      setAgentRuns(seeded);
    } catch (e) {
      log.warn("pane_agent_states failed", e);
    }

    // BRIEF: same reload story for the brief entries.
    try {
      setBriefs(await invoke<Record<string, PaneBriefEntry>>("pane_briefs"));
    } catch (e) {
      log.warn("pane_briefs failed", e);
    }

    const unlistens: UnlistenFn[] = [];
    unlistens.push(
      await listen<PtyDataEvent>("pty:data", (e) => {
        const pid = sessionToPane.get(e.payload.session_id);
        if (!pid) return;
        terms.get(pid)?.writeData(e.payload.data);
      })
    );
    unlistens.push(
      await listen<PtyExitEvent>("pty:exit", (e) => {
        const pid = sessionToPane.get(e.payload.session_id);
        if (!pid) return;
        sessionToPane.delete(e.payload.session_id);
        paneToSession.delete(pid);
        // If the pane was popped out, its window is closing too — return the
        // (now-dead) pane to the grid so it isn't pruned away forever. Done
        // here because popout:closed can't map the sid once the maps are gone.
        if (poppedOut().has(pid)) {
          setPoppedOut((s) => {
            const n = new Set(s);
            n.delete(pid);
            return n;
          });
        }
        const ti = terms.get(pid);
        ti?.notice(
          `[disconnected${e.payload.reason ? ` (${e.payload.reason})` : ""}]`
        );
        // v0.4.4-beta.2: extra safety on pane process exit — a full-screen
        // app that enabled SGR/X10 mouse tracking and then died with the
        // PTY (SSH drop, kill -9, tmux crash) never got to send its
        // disable sequence. Clearing xterm's mouse state now means the
        // stale display we leave behind can't emit \e[<..M events if the
        // user clicks around while re-reading the "[disconnected]" notice.
        // Fixed control string — never PTY content (Rule #1).
        ti?.resetMouseModes();
        ti?.detach();
        bump();
        void refreshPersistence();
      })
    );
    // beta.3 (netfree, Track 1b): SSH transport dropped. Backend emitted
    // `ssh:disconnected` with the pane's connection identity so we can
    // drive the auto-reconnect toast + backoff loop. pty:exit fires
    // alongside — the `[disconnected]` terminal notice still shows.
    unlistens.push(
      await listen<SshDisconnectedEvent>("ssh:disconnected", (e) => {
        // Guard: only handle transport drops; a clean Eof/Close doesn't
        // emit this event (backend filters), but defense in depth.
        if (e.payload.reason !== "transport-dropped") return;
        startReconnect(e.payload);
      })
    );
    // Connect-time verdict on whether the remote CLI matches our build.
    unlistens.push(
      await listen<CliAlignmentEvent>("workspace:cli-alignment", (e) => {
        const p = e.payload;
        setCliSkew((prev) => {
          const next = { ...prev };
          if (p.aligned) delete next[p.workspace_id];
          else next[p.workspace_id] = p;
          return next;
        });
      }),
    );
    // Unshipped-fivefer (#4): a pop-out window closed — re-attach the origin
    // pane's terminal (input + resize) if its session is still live. If the
    // popout closed *because* of pty:exit, the exit handler above already
    // cleared the maps, so this is a no-op.
    // Dev Mode: an element was right-clicked in a workspace Browser.
    // The payload crossed a JSON boundary from an untrusted page, so it
    // is narrowed before anything opens (see parseCapture).
    unlistens.push(
      await listen<unknown>("browser:ticket-captured", (e) => {
        const raw = e.payload;
        if (typeof raw !== "object" || raw === null) return;
        const o = raw as Record<string, unknown>;
        const wsId = typeof o.workspace_id === "string" ? o.workspace_id : "";
        const capture = parseCapture(o.capture);
        if (!wsId || !capture) return;
        setPendingCapture({ workspaceId: wsId, capture });
      }),
    );

    unlistens.push(
      await listen<string>("popout:closed", (e) => {
        const sid = e.payload;
        const pid = sessionToPane.get(sid);
        // pty:exit-driven close already cleared the maps AND un-pruned the
        // pane (see the pty:exit handler); nothing left to do here.
        if (!pid || paneToSession.get(pid) !== sid) return;
        // Return the pane to its grid slot, then re-attach input + resize.
        setPoppedOut((s) => {
          const n = new Set(s);
          n.delete(pid);
          return n;
        });
        const ti = terms.get(pid);
        if (!ti) return;
        ti.attach(sid);
        ti.notice(t("pane.popout.reattached"));
        requestAnimationFrame(() => ti.fitAndResize(true));
      })
    );

    // Phase 85.C: a popped-out Browser window closed (its X, or the
    // workspace was deleted). Per Yossi's call the Browser does NOT
    // re-attach into the floating panel — closing that window means
    // closing the Browser. All we do is forget the workspace, so the
    // modal broadcast-hide covers it again and a later re-open spawns
    // the child under `main`. The Rust `Destroyed` handler already
    // dropped the Webview and cleared the persisted mode.
    unlistens.push(
      await listen<string>("browser-popout:closed", (e) => {
        const wsId = e.payload;
        setPoppedOutBrowsers((prev) => {
          if (!prev.has(wsId)) return prev;
          const n = new Set(prev);
          n.delete(wsId);
          return n;
        });
      })
    );
    // Initial feed load.
    try {
      const items = await invoke<FeedItem[]>("feed_list");
      // The live feed now carries ONLY actionable blocking permission cards;
      // passive hooks (stop / notification / …) live in the Notification
      // Center. Re-hydrate just the still-pending permission requests so a
      // restart doesn't resurrect stale passive cards.
      setFeedItems(
        items.filter((i) => i.kind === "permission_request" && i.state === "pending").reverse(),
      );
    } catch (e) {
      log.warn("feed_list failed", e);
    }
    // Phase 6.5 feed events.
    unlistens.push(
      await listen<FeedItem>("feed:item-added", (e) => {
        const f = e.payload;
        const isBlocking = f.kind === "permission_request" && f.state === "pending";
        const isMeaningful = !isBlocking && MEANINGFUL_SUBKINDS.has(f.subkind);
        // BLOCKING permission asks are the ONLY items that get a transient
        // live-feed card — they're actionable (allow/deny) and drive the
        // waiting red-dot highlight. Passive hooks never touch the feed.
        if (isBlocking) {
          setFeedItems((prev) => [f, ...prev.filter((i) => i.request_id !== f.request_id)]);
        }
        // 2026-08-19: session-start / session-end are literally "Claude
        // started / stopped in pane X" — the hook already carries
        // YMUX_PANE_ID (cli/src/main.rs) — so they drive the per-pane
        // "don't bidi this twice" state. This works over SSH and through any
        // multiplexer, unlike the terminal title, which zellij consumes.
        // session-end clears back to null rather than asserting false, so the
        // title can still speak if it ever starts arriving.
        // A Claude hook can only be fired from INSIDE Claude, and it already
        // carries YMUX_PANE_ID (cli/src/main.rs), so ANY of them is proof that
        // Claude holds that pane — which is what decides whether the pane may
        // bidi its output. session-end is the one that means the opposite.
        //
        // Not just session-start: the case that matters most is re-attaching
        // to a persistent zellij session where Claude never stopped, so no
        // session-start ever fires. `stop` lands after every reply, so the
        // state corrects itself on the first interaction. Measured need —
        // Yossi's log had `tui=0` on panes that had Claude running in them.
        //
        // Clears to null rather than false so the title can still speak, in
        // case it ever starts arriving.
        if (f.pane_id) {
          if (f.subkind === "session-end") setPaneTuiSignal(f.pane_id, null);
          else setPaneTuiSignal(f.pane_id, true);
        }
        // Every hook is recorded in the Notification Center history; feedToNotif
        // carries the workspace_id so the entry shows which workspace it's from.
        pushNotif(feedToNotif(f));
        // MEANINGFUL passive events ("your turn" / "Claude asked") blink the
        // sidebar workspace row + the pane border, without cluttering the feed.
        if (isMeaningful) {
          if (f.workspace_id) pulseWorkspace(f.workspace_id);
          const pid = f.pane_id;
          if (pid) {
            addPaneNotified(pid);
            // Focused pane: brief one-shot flash, then auto-clear (mirrors OSC).
            if (activePaneId() === pid) setTimeout(() => clearPaneNotified(pid), 2000);
          }
        }
      })
    );
    unlistens.push(
      await listen<FeedResolvedEvent>("feed:item-resolved", (e) => {
        const verdict = e.payload.decision === "allow" ? "allowed" : e.payload.decision === "deny" ? "denied" : e.payload.decision === "timeout" ? "timedout" : "denied";
        setFeedItems((prev) =>
          prev.map((i) =>
            i.request_id === e.payload.request_id
              ? { ...i, state: verdict as FeedItem["state"] }
              : i
          )
        );
        scheduleFeedDismiss(e.payload.request_id);
      })
    );
    // Phase 35 (#1.2): OSC 9/99/777 terminal notifications. The
    // backend's PTY reader detects the escape sequence and emits this
    // event; we surface it as a passive feed item (same rendering as
    // agent-hook passive items). Universal complement to the
    // Claude-specific hooks — works for cargo, pytest, any tool that
    // prints the escape sequence.
    unlistens.push(
      await listen<{ pane_id: string; title: string; body: string; kind: string }>(
        "osc-notification",
        (e) => {
          const { title, body } = e.payload;
          const hasTitle = title.trim().length > 0;
          // OSC 9/99/777 notifications are passive: they no longer get a
          // transient feed card (the live feed carries only blocking permission
          // asks). They still land in the Notification Center and pulse the
          // originating pane border below.
          // #1: record it in the Notification Center timeline.
          pushNotif({
            id: Date.now() * 1000 + Math.floor(Math.random() * 1000),
            title: hasTitle ? title : body,
            body: hasTitle ? body : "",
            workspace_id: null,
            // 66.G: OSC notifications know their pane; the jump handler
            // resolves the workspace from it (workspace_id stays null).
            pane_id: e.payload.pane_id ?? null,
            timestamp_ms: Date.now(),
            kind: "notification",
          });
          // cmux-A A1: mark the pane so its border pulses. A non-focused
          // pane keeps pulsing until the user focuses it (cleared in
          // onFocus). The focused pane still gets a brief one-shot
          // confirmation flash, then auto-clears after 2s — so activity
          // is visible even when you're watching the pane it came from.
          const pid = e.payload.pane_id;
          if (pid) {
            addPaneNotified(pid);
            if (activePaneId() === pid) {
              setTimeout(() => clearPaneNotified(pid), 2000);
            }
          }
        },
      ),
    );
    // #1: RPC/agent notifications (Claude hooks). Backend pushes to
    // state.notifications AND emits this — the center mirrors it live.
    unlistens.push(
      await listen<NotifItem>("notification:new", (e) => pushNotif(e.payload)),
    );
    // #2: tray menu actions routed from the Rust tray handler.
    unlistens.push(
      await listen<string>("tray:action", (e) => {
        if (e.payload === "new_workspace") setShowSetup({});
        else if (e.payload === "settings") setShowSettings(true);
      }),
    );
    // Phase 36 (#2.2): auto port-forward lifecycle. The backend opens a
    // local SSH forward when the remote watcher reports a new listening
    // port, and emits these events. We track them for the Ports panel
    // Phase 46: ports are DETECTED on remote LISTEN, but a forward is
    // only opened on user click. No FeedItem on either event — the
    // PortsWindow is the only surface. Events:
    //   port-detected      → add to detectedPorts
    //   port-undetected    → remove from detectedPorts (also cleans
    //                         forwards if the port was tunneled)
    //   port-forwarded     → add to portForwards
    //   port-forward-stopped → remove from portForwards
    unlistens.push(
      await listen<{ workspace_id: string; remote_port: number; addr: string; family: string }>(
        "port-detected",
        (e) => {
          setDetectedPorts((prev) => [
            ...prev.filter(
              (p) => !(p.workspace_id === e.payload.workspace_id && p.remote_port === e.payload.remote_port),
            ),
            {
              workspace_id: e.payload.workspace_id,
              remote_port: e.payload.remote_port,
              addr: e.payload.addr,
              family: e.payload.family,
            },
          ]);
        },
      ),
    );
    unlistens.push(
      await listen<{ workspace_id: string; remote_port: number }>(
        "port-undetected",
        (e) => {
          setDetectedPorts((prev) =>
            prev.filter(
              (p) => !(p.workspace_id === e.payload.workspace_id && p.remote_port === e.payload.remote_port),
            ),
          );
        },
      ),
    );
    // Phase 47: detection toggled off → wipe the workspace's entries.
    unlistens.push(
      await listen<{ workspace_id: string }>(
        "port-detection-cleared",
        (e) => {
          setDetectedPorts((prev) =>
            prev.filter((p) => p.workspace_id !== e.payload.workspace_id),
          );
        },
      ),
    );
    unlistens.push(
      await listen<{ workspace_id: string; remote_addr: string; remote_port: number; local_port: number }>(
        "port-forwarded",
        (e) => {
          const row: ForwardRow = {
            workspace_id: e.payload.workspace_id,
            remote_port: e.payload.remote_port,
            local_port: e.payload.local_port,
            remote_addr: e.payload.remote_addr,
            opened_at: Date.now(),
          };
          setPortForwards((prev) => [
            ...prev.filter(
              (f) => !(f.workspace_id === row.workspace_id && f.remote_port === row.remote_port),
            ),
            row,
          ]);
        },
      ),
    );
    unlistens.push(
      await listen<{ workspace_id: string; remote_port: number }>(
        "port-forward-stopped",
        (e) => {
          setPortForwards((prev) =>
            prev.filter(
              (f) =>
                !(
                  f.workspace_id === e.payload.workspace_id &&
                  f.remote_port === e.payload.remote_port
                ),
            ),
          );
        },
      ),
    );
    // 2026-08-23: the attach-only notice. `pane_connect` refuses to type a
    // command into a multiplexer session that was already running, and this is
    // how the user finds out — a command they chose must never disappear in
    // silence. Only raised when there WAS a command; a plain attach is the
    // normal case and needs no announcement.
    unlistens.push(
      await listen<{
        pane_id: string;
        session_name: string;
        skipped: string;
        had_command: boolean;
      }>("pane-connect-notice", (e) => {
        if (e.payload.skipped !== "attach-only" || !e.payload.had_command) return;
        flashSummaryToast(
          "err",
          t("connect.attachOnly.toast", { name: e.payload.session_name }),
        );
      })
    );
    // Phase 7.B: notes
    await refreshNotes();
    unlistens.push(
      await listen("notes:changed", () => {
        void refreshNotes();
      })
    );
    // Per-pane status events (e.g. remote-bootstrap progress).
    unlistens.push(
      await listen<{ pane_id: string; text: string }>("pane:status", (e) => {
        const next = { ...paneStatusText() };
        if (e.payload.text) {
          next[e.payload.pane_id] = e.payload.text;
        } else {
          delete next[e.payload.pane_id];
        }
        setPaneStatusText(next);
      })
    );
    // issue #4: per-pane agent turn timing for the chrome Ticker.
    unlistens.push(
      await listen<{
        pane_id: string;
        started_at: number | null;
        avg_ms: number | null;
        running: boolean;
        state?: PaneAgentState;
        state_since?: number | null;
        seq?: number;
      }>("pane:agent-run", (e) => {
        const pid = e.payload.pane_id;
        const prev = agentRuns()[pid];
        const seq = e.payload.seq ?? 0;
        // Phase 84.B: hooks are separate processes racing over a socket,
        // so events can arrive out of order. Drop anything not newer than
        // what we hold. Cheap, and a partial answer to the stale-hook
        // thread in docs/DECISIONS.md — a floor, not the fix.
        if (prev && e.payload.seq != null && seq <= prev.seq) return;
        const next = { ...agentRuns() };
        // The clear signal is `state: "unknown"` (session-end), NOT the
        // old `!running && avg_ms == null`. That test also matches a
        // perfectly normal `stop` on a pane whose turns have all been
        // shorter than the ticker's 2s minimum, which would delete the
        // entry and take the yellow light down with it.
        if (e.payload.state === "unknown") {
          delete next[pid];
        } else {
          next[pid] = {
            startedAt: e.payload.running ? e.payload.started_at : null,
            avgMs: e.payload.avg_ms,
            state: e.payload.state ?? "unknown",
            stateSince: e.payload.state_since ?? null,
            seq,
          };
        }
        setAgentRuns(next);
      })
    );
    // BRIEF: per-pane brief entries. Same seq guard as pane:agent-run —
    // hooks race over a socket, drop anything not newer than what we hold.
    unlistens.push(
      await listen<{ pane_id: string; entry: PaneBriefEntry }>("pane:brief", (e) => {
        const { pane_id, entry } = e.payload;
        const prev = briefs()[pane_id];
        if (prev && entry.seq <= prev.seq) return;
        setBriefs({ ...briefs(), [pane_id]: entry });
      })
    );
    // Live refresh when an external mutation happens (RPC over named pipe).
    unlistens.push(
      await listen("workspaces:changed", () => {
        void refreshFromBackend();
      })
    );
    // Phase 9.A: settings updated externally (CLI / RPC) — re-apply theme.
    unlistens.push(
      await listen<Settings>("settings:changed", (e) => {
        setSettings(e.payload);
        setLoggerLevel(e.payload.logs?.level ?? "info");
        applyTheme(e.payload);
        applyI18nSettings(e.payload.i18n);
        setShortcutTable(
          buildShortcutTable(e.payload.shortcuts ?? DEFAULT_SHORTCUTS),
        );
        setCtrlCCopyOnSelect(
          (e.payload.shortcuts ?? DEFAULT_SHORTCUTS).copy_on_select_with_ctrl_c,
        );
      })
    );
    // Phase 18: agent-hooks outdated event from the backend's
    // post-bootstrap probe. Surface the banner once per connection.
    unlistens.push(
      await listen<HooksOutdatedInfo>("hooks:outdated", (e) => {
        setHooksBanner(e.payload);
      })
    );

    // Phase 9.B: update available — show a banner; user clicks to open notes.
    unlistens.push(
      await listen<UpdateInfo>("update:available", (e) => {
        setUpdateBanner(e.payload);
      })
    );

    window.addEventListener("keydown", handleKey);
    // Phase 65 (bug 3.3): in production builds, block the WebView2
    // DevTools accelerators (F12, Ctrl+Shift+I, Ctrl+Shift+J) so they
    // can't corrupt an xterm.js pane. Release builds already compile
    // without the `devtools` Cargo feature (DevTools is disabled), so
    // this is belt-and-suspenders + documents intent; dev builds keep
    // DevTools fully available. Capture phase so it beats the bubble
    // handlers. Ctrl+Shift+C is intentionally NOT blocked — it's the
    // copy-selection shortcut (handleKey), and with DevTools off it
    // can't open the inspector anyway.
    const blockDevtoolsKeys = (e: KeyboardEvent) => {
      if (!import.meta.env.PROD) return;
      const isF12 = e.key === "F12";
      const isInspect =
        e.ctrlKey &&
        e.shiftKey &&
        !e.altKey &&
        !e.metaKey &&
        (keyEq(e, "i") || keyEq(e, "j"));
      if (isF12 || isInspect) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("keydown", blockDevtoolsKeys, true);
    // Phase 58: keyup half of push-to-talk. We register a generic
    // keyup that stops the active recorder regardless of which
    // modifier was released — typical PTT UX is "any release ends
    // the capture", and trying to match the exact hotkey on keyup
    // misses the very-common case where the user releases Shift
    // before M.
    const handleKeyUp = (_e: KeyboardEvent) => {
      if (sttRecorder) {
        stopPushToTalk();
      }
    };
    window.addEventListener("keyup", handleKeyUp);
    // Phase 55-A: PaneView dispatches a custom event on content
    // double-click (skipping xterm + the header). We listen at the
    // App level so the toggle stays co-located with the maximized
    // signal + the post-toggle fit/resize fanout.
    const handlePaneMaximize = (e: Event) => {
      const detail = (e as CustomEvent).detail as { paneId?: string };
      if (detail?.paneId) toggleMaximize(detail.paneId);
    };
    window.addEventListener("ymux:pane-maximize", handlePaneMaximize);

    // Phase 62.B (item J): a terminal OSC 8 file:// link was clicked.
    // SFTP-download it to the user's Downloads folder, with toasts.
    const handleOscFileLink = (e: Event) => {
      const detail = (e as CustomEvent).detail as {
        workspaceId: string | null;
        path: string;
      } | null;
      if (!detail) return;
      const name = detail.path.split("/").filter(Boolean).pop() || detail.path;
      if (!detail.workspaceId) {
        flashSummaryToast("err", t("osc.download.noRemote"));
        return;
      }
      // Phase 65 (bug K): always ask where to save (native Save dialog)
      // instead of silently dropping into ~/Downloads.
      void saveRemoteFileAs(detail.workspaceId, detail.path, name)
        .then((local) => {
          if (local) flashSummaryToast("ok", t("osc.download.done", { path: local }));
          // null = user cancelled the dialog → no toast.
        })
        .catch((err) =>
          flashSummaryToast("err", t("osc.download.failed", { msg: String(err) })),
        );
    };
    window.addEventListener("ymux:osc-file-link", handleOscFileLink);

    // Phase 64 (J, Track B): a plain-text `[file]` link with a RELATIVE
    // path was clicked. We can't resolve it against the pane's remote cwd
    // (no OSC 7 tracking yet), so copy the path to the clipboard and tell
    // the user it's relative to the pane's directory.
    const handleFileLinkRelative = (e: Event) => {
      const detail = (e as CustomEvent).detail as { path: string } | null;
      if (!detail?.path) return;
      void navigator.clipboard.writeText(detail.path).then(
        () =>
          flashSummaryToast(
            "ok",
            t("filelink.relative.copied", { path: detail.path }),
          ),
        () =>
          flashSummaryToast(
            "err",
            t("filelink.relative.copyfail", { path: detail.path }),
          ),
      );
    };
    window.addEventListener("ymux:file-link-relative", handleFileLinkRelative);

    // Session restore runs LAST in onMount: the `pty:data` listener above has
    // to be live before any pane attaches, or the first screenful tmux paints
    // on re-attach would be dropped. Fire-and-forget — a failure here only
    // means the user clicks [Connect] themselves.
    void restoreSessions();

    onCleanup(() => {
      for (const u of unlistens) u();
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("keydown", blockDevtoolsKeys, true);
      window.removeEventListener("keyup", handleKeyUp);
      window.removeEventListener("ymux:pane-maximize", handlePaneMaximize);
      window.removeEventListener("ymux:osc-file-link", handleOscFileLink);
      window.removeEventListener(
        "ymux:file-link-relative",
        handleFileLinkRelative,
      );
      for (const [pid] of paneToSession) {
        invoke("pane_disconnect", { paneId: pid }).catch(() => {});
      }
      for (const [, ti] of terms) ti.dispose();
      terms.clear();
    });
  });

  return (
    <div
      class="app"
      style={{ "grid-template-columns": `${sidebarPx()}px 1fr` }}
    >
      {/* v0.4.4 (Task 1): headless auto-connect indicator — shown while a
          secondary panel arms the workspace's SSH handle in the background. */}
      <Show when={connectingWs()}>
        <div class="connecting-pill" role="status">
          <span class="connecting-pill-spinner" aria-hidden="true" />
          {t("panel.connecting")}
        </div>
      </Show>
      {/* Phase 78: global Claude subscription-usage indicator (top-right). */}
      <Show when={settings()?.claude_usage?.show_top_indicator ?? true}>
        <ClaudeUsageIndicator
          workspaceId={file().active_workspace_id ?? undefined}
          live={
            !!file().active_workspace_id &&
            liveWorkspaceIds().has(file().active_workspace_id!)
          }
          displayMode={settings()?.claude_usage?.display_mode ?? "percent"}
          refreshMinutes={settings()?.claude_usage?.auto_refresh_minutes ?? 10}
        />
      </Show>
      <ErrorBoundary
        fallback={(err) => {
          // A sidebar render crash used to leave NOTHING in debug.log —
          // the card on screen was the only evidence, so a user report
          // of "the sidebar" was undiagnosable after the fact. This one
          // cost two build-and-test rounds (a TDZ ReferenceError from
          // state declared after the component's return, fixed in
          // cbef36e). Logging in a render function is a side effect, but
          // this branch renders once per error and never in the happy
          // path.
          log.error("sidebar render crashed", err);
          return (
            <div class="sidebar-error">
              <p>{t("error.sidebarRender")}</p>
              <pre>{String(err)}</pre>
              <button class="primary" onClick={() => setShowSetup({})}>
                + New workspace
              </button>
            </div>
          );
        }}
      >
        <Sidebar
          workspaces={file().workspaces}
          activeId={file().active_workspace_id}
          connectedIds={liveWorkspaceIds()}
          waitingWorkspaceIds={waitingWorkspaceIds()}
          hookPulseWorkspaceIds={activeHookWorkspaceIdsReactive()}
          notifiedWorkspaceIds={notifiedWorkspaceIds()}
          briefAttentionWorkspaceIds={queueAttentionWorkspaceIds()}
          groups={file().groups ?? []}
          onGroupCreate={async (name, color) => {
            try {
              const g = await invoke<WorkspaceGroup>("workspace_group_create", { name, color });
              const f = await invoke<WorkspacesFile>("workspaces_load");
              updateFile(f);
              return g;
            } catch (e) {
              log.error("workspace_group_create failed", e);
              return null;
            }
          }}
          onGroupRename={(id, name) => {
            void (async () => {
              try {
                await invoke("workspace_group_update", { id, name, color: null, isCollapsed: null });
                const f = await invoke<WorkspacesFile>("workspaces_load");
                updateFile(f);
              } catch (e) { log.error("workspace_group_update rename failed", e); }
            })();
          }}
          onGroupSetColor={(id, color) => {
            void (async () => {
              try {
                await invoke("workspace_group_update", { id, name: null, color, isCollapsed: null });
                const f = await invoke<WorkspacesFile>("workspaces_load");
                updateFile(f);
              } catch (e) { log.error("workspace_group_update color failed", e); }
            })();
          }}
          onGroupToggleCollapse={(id, isCollapsed) => {
            void (async () => {
              try {
                await invoke("workspace_group_update", { id, name: null, color: null, isCollapsed });
                const f = await invoke<WorkspacesFile>("workspaces_load");
                updateFile(f);
              } catch (e) { log.error("workspace_group_update collapse failed", e); }
            })();
          }}
          onGroupDelete={(id) => {
            void (async () => {
              try {
                await invoke("workspace_group_delete", { id });
                const f = await invoke<WorkspacesFile>("workspaces_load");
                updateFile(f);
              } catch (e) { log.error("workspace_group_delete failed", e); }
            })();
          }}
          onWorkspaceSetGroup={(workspaceId, groupId) => {
            void (async () => {
              try {
                await invoke("workspace_set_group", { workspaceId, groupId });
                const f = await invoke<WorkspacesFile>("workspaces_load");
                updateFile(f);
              } catch (e) { log.error("workspace_set_group failed", e); }
            })();
          }}
          // beta.3 (ws-dragdrop): direct drag reorder. Both commands
          // return the updated WorkspacesFile so we can drop the extra
          // `workspaces_load` round-trip that the group-CRUD handlers
          // above do.
          onWorkspaceReorder={(workspaceId, groupId, newIndex) => {
            void (async () => {
              try {
                const f = await invoke<WorkspacesFile>("workspace_reorder", {
                  workspaceId,
                  groupId,
                  newIndex,
                });
                updateFile(f);
              } catch (e) { log.error("workspace_reorder failed", e); }
            })();
          }}
          onGroupReorder={(groupId, newIndex) => {
            void (async () => {
              try {
                const f = await invoke<WorkspacesFile>("workspace_group_reorder", {
                  groupId,
                  newIndex,
                });
                updateFile(f);
              } catch (e) { log.error("workspace_group_reorder failed", e); }
            })();
          }}
          onActivate={handleSetActive}
          onCreate={() => setShowSetup({})}
          onOpenSettings={() => setShowSettings(true)}
          onOpenNotes={() => setShowNotes(true)}
          onAction={(id, action) => {
            if (action === "rename") handleRename(id);
            else if (action === "edit") {
              const ws = file().workspaces.find((w) => w.id === id);
              if (ws) setEditingWorkspace(ws);
            } else if (action === "delete") handleDelete(id);
            else if (action === "disconnect")
              void handleDisconnectWorkspace(id);
            else if (action === "addons") {
              const ws = file().workspaces.find((w) => w.id === id);
              setAddonsWin({ id, name: ws?.name ?? "" });
            } else if (action === "add_project_folder") {
              void startPinProjectFolder(id);
            } else if (action === "check_git") {
              void recheckGit(id);
            }
            // Phase 65.Q removed the "add_machine" action — joining an
            // existing server is handled by the main wizard (R).
          }}
          // ── project-folder workspaces ─────────────────────────────
          onSetCollapsed={(workspaceId, isCollapsed) => {
            void (async () => {
              try {
                const f = await invoke<WorkspacesFile>("workspace_set_collapsed", {
                  workspaceId,
                  collapsed: isCollapsed,
                });
                updateFile(f);
              } catch (e) { log.error("workspace_set_collapsed failed", e); }
            })();
          }}
          onNewWorktree={(w) => setProjectFolderModal({ kind: "worktree", workspace: w })}
          // Rejection is meaningful here — the Sidebar renders git's own
          // message (bad path, no live SSH session) rather than an empty
          // list, which would read as "this repo has no worktrees".
          onListWorktrees={(workspaceId) =>
            invoke<WorktreeEntry[]>("workspace_list_worktrees", { workspaceId })
          }
          onOpenWorktree={(rootWorkspaceId, wt) => void openWorktree(rootWorkspaceId, wt)}
          onNotARepo={(workspaceId) => {
            void (async () => {
              try {
                const f = await invoke<WorkspacesFile>("workspace_set_project_root", {
                  workspaceId,
                  isProjectRoot: false,
                });
                updateFile(f);
              } catch (e) {
                log.error("workspace_set_project_root failed", e);
              }
            })();
          }}
          allForwards={portForwards()}
          onOpenPorts={(workspaceId) => {
            // Badge click: activate that workspace, then open the
            // (active-workspace-scoped) Ports window.
            void handleSetActive(workspaceId);
            setShowPortsWindow(true);
          }}
          onOpenPortsGlobal={() => void armWorkspaceConnection().then(() => setShowPortsWindow(true))}
          mode={sidebarMode()}
          onSetMode={setSidebarMode}
          widthPx={sidebarPx()}
        />
      </ErrorBoundary>
      {/* Phase 62.B (item I): drag handle on the sidebar/main boundary —
          only in full mode (icons is a fixed width). Phase 65.P removed
          the "hidden" mode and its edge reopen-tab. */}
      <Show when={sidebarMode() === "full"}>
        <div
          class="sidebar-resizer"
          style={{ "inset-inline-start": `${sidebarPx()}px` }}
          onMouseDown={startSidebarResize}
          title={t("sidebar.resize.tooltip")}
        />
      </Show>
      <div class="main">
        {/* Phase 30: per-workspace accent strip. Sets the CSS variable
            inline so the rule in App.css can paint it without needing a
            second class per workspace. Hidden via data-empty when the
            workspace has no color (or no active workspace at all). */}
        <div
          class="ws-accent-strip"
          data-empty={activeWs()?.color ? "false" : "true"}
          style={activeWs()?.color ? `--ws-color: ${activeWs()!.color}` : undefined}
        />
        <Show when={activeWs()}>
          <ErrorBoundary
            fallback={(err) => (
              <div class="ws-header layout-error">
                <span class="ws-title">{activeWs()?.name ?? "(unknown)"}</span>
                <span class="ws-conn-info">{String(err)}</span>
                <button
                  class="ws-header-btn"
                  onClick={() => handleResetLayout(activeWs()!.id)}
                >
                  Reset to single pane
                </button>
              </div>
            )}
          >
          <div
            class="ws-header"
            classList={{ compact: wsHeaderNarrow.narrow() }}
            ref={wsHeaderNarrow.ref}
          >
            <span
              class="ws-dot"
              style={{ background: activeWs()!.color || "#6b7682" }}
            />
            <span class="ws-title">{activeWs()!.name}</span>
            <Show when={activeWs()!.layout?.kind === "pane"}>
              <span class="ws-conn-info">
                {(() => {
                  const layout = activeWs()!.layout as Extract<
                    LayoutNode,
                    { kind: "pane" }
                  >;
                  if (layout.pane_kind === "browser") return "browser";
                  return layout.connection
                    ? describeConnection(layout.connection)
                    : "—";
                })()}
              </span>
            </Show>
            <Show when={activeWs()!.layout?.kind === "split"}>
              <span class="ws-conn-info">
                {collectPanes(activeWs()!.layout!).length} panes
              </span>
            </Show>
            <Show when={activeWs()!.layout && activePaneId()}>
              {/* Phase 60 (smoke-test 2a): Browser + Files buttons
                  live HERE, next to + diff — they're workspace-scoped
                  tools, so they belong in the workspace header, not
                  in the global sidebar. The i18n keys keep their
                  historical "sidebar." prefix; renaming 8 keys × 4
                  locales for a cosmetic prefix isn't worth the churn. */}
              <button
                class="ws-header-btn"
                title={t("sidebar.browser.tooltip")}
                onClick={() => void armWorkspaceConnection().then(() => setShowBrowserWindow(true))}
              >
                <IconGlobe />
                <span class="ws-header-btn-label">{t("sidebar.browser.label")}</span>
              </button>
              <button
                class="ws-header-btn"
                title={t("sidebar.files.tooltip")}
                onClick={() => void openPanelConnected("files")}
              >
                <IconFolder />
                <span class="ws-header-btn-label">{t("sidebar.files.label")}</span>
              </button>
              {/* Feedback reorg: Notifications button lives at the header edge,
                  after Monitor. Moved here from the sidebar so all workspace
                  tools sit together. Badge shows the unread count. */}
              <button
                class="ws-header-btn notif-bell"
                title={t("notif.title")}
                onClick={() => openPanel("notifications")}
              >
                <IconBell />
                <span class="ws-header-btn-label">{t("notif.title")}</span>
                <Show when={unreadNotifs() > 0}>
                  <span class="notif-bell-badge">{unreadNotifs() > 99 ? "99+" : unreadNotifs()}</span>
                </Show>
              </button>
              {/* Header ⋯ menu (tabs-toggle declutter): the less-frequent
                  workspace actions — view mode (Phase 84.A), + diff
                  (Phase 50), Insights (Phase 68), Tickets — collapsed
                  behind one button. Every item calls the exact handler
                  its old standalone button (and the command palette)
                  used; this is a second entry point, not a fork. */}
              <div class="ws-header-more" ref={wsMenuRef}>
                <button
                  class="ws-header-btn"
                  title={t("ws_header.more")}
                  aria-label={t("ws_header.more")}
                  aria-expanded={wsMenuOpen()}
                  onClick={() => setWsMenuOpen(!wsMenuOpen())}
                >
                  <IconMore />
                </button>
                <Show when={wsMenuOpen()}>
                  <div class="ws-header-menu">
                    <button
                      title={t("ws_header.view_mode.tooltip")}
                      onClick={() => {
                        setWsMenuOpen(false);
                        void setTabsMode(!tabsMode());
                      }}
                    >
                      <Show when={tabsMode()} fallback={<IconRows />}>
                        <IconColumns />
                      </Show>
                      {tabsMode()
                        ? t("ws_header.view_mode.split")
                        : t("ws_header.view_mode.tabs")}
                    </button>
                    <button
                      title={t("ws_header.split_diff_title")}
                      onClick={() => {
                        setWsMenuOpen(false);
                        const pid = activePaneId();
                        if (pid) splitPane(pid, "horizontal", "diff");
                      }}
                    >
                      <IconGitCompare />
                      {t("ws_header.add_diff")}
                    </button>
                    <button
                      title={t("sidebar.insights.tooltip")}
                      onClick={() => {
                        setWsMenuOpen(false);
                        void openPanelConnected("monitor");
                      }}
                    >
                      <IconActivity />
                      {t("sidebar.insights.label")}
                    </button>
                    {/* Tickets stays openPanel, not openPanelConnected —
                        local files, no connection needed. */}
                    <button
                      title={t("sidebar.tickets.tooltip")}
                      onClick={() => {
                        setWsMenuOpen(false);
                        openPanel("tickets");
                      }}
                    >
                      <IconBug />
                      {t("sidebar.tickets.label")}
                    </button>
                  </div>
                </Show>
              </div>
              {/* Phase 24.D: removed + chat / + claude log buttons.
                  The two pane kinds + their backends are rolled back
                  pending a future unified-view rebuild. */}
            </Show>
          </div>
          </ErrorBoundary>
        </Show>

        {/* Design Pass 01 (#1): zero workspaces → full welcome screen.
            Workspaces exist but none active → light "pick one" prompt. */}
        <Show when={file().workspaces.length === 0}>
          <WelcomeScreen
            onCreate={() => setShowSetup({ target: "local" })}
            onConnectSsh={() => setShowSetup({ target: "server" })}
            onProvision={() => setShowSetup({ target: "server" })}
          />
        </Show>
        <Show when={file().workspaces.length > 0 && !activeWs()}>
          <div class="empty">
            <p>{t("ws.empty.none")}</p>
            <button class="primary" onClick={() => setShowSetup({})}>
              {t("ws.empty.new")}
            </button>
          </div>
        </Show>

        {/* Phase 84.A: the tab strip, a sibling of .layout-root inside the
            flex-column .main. Built from the PRUNED tree, not ws.layout —
            a pane popped out into its own OS window must not leave a tab
            behind that selects nothing. */}
        <Show when={tabsMode() ? activeWs() : null}>
          {(ws) => {
            // Callback form, not an IIFE: Show's children must stay lazy or
            // this body runs before `when` is even evaluated, and dereferences
            // a workspace that isn't there.
            const base = (): LayoutNode | null => {
              const layout = ws().layout;
              if (!layout) return null;
              const hidden = poppedOut();
              return hidden.size > 0 ? pruneLayout(layout, hidden) ?? layout : layout;
            };
            return (
              <Show when={base()}>
                {(tree) => (
                  <PaneTabs
                    panes={collectPanes(tree())
                      .map((id) => findPane(tree(), id))
                      .filter((n): n is Extract<LayoutNode, { kind: "pane" }> => n !== null)}
                    activePaneId={activePaneId()}
                    connectedPaneIds={connectedPanes()}
                    waitingPaneIds={waitingPaneIds()}
                    notifiedPaneIds={paneNotified()}
                    panePulseEnabled={settings()?.notifications?.pane_pulse_on_activity ?? true}
                    workspaceName={ws().name}
                    workspaceColor={ws().color ?? undefined}
                    workspaceEmoji={ws().emoji ?? undefined}
                    workspaceConnection={ws().connection ?? undefined}
                    agentLights={paneAgentLights()}
                    agentStateSince={paneAgentStateSince()}
                    agentNowMs={agentClockMs()}
                    onSelect={focusPane}
                    onClose={(pid) => void closeTab(pid)}
                    onNew={() => void newTab()}
                  />
                )}
              </Show>
            );
          }}
        </Show>

        <Show when={activeWs()?.layout}>
          {/* Phase 62.B (item H): workspace color frames the whole pane
              area (outer border). Pane colors frame each pane inside. */}
          <div
            class="layout-root"
            data-has-color={activeWs()?.color ? "true" : "false"}
            style={activeWs()?.color ? `--ws-color: ${activeWs()!.color}` : undefined}
          >
            {/* Phase 8 fix v3: ErrorBoundary so a single corrupted workspace
                layout (e.g. from the recent autosave-loop nesting) doesn't
                blank the whole app. Falls back to a clear reset button. */}
            <ErrorBoundary
              fallback={(err, _reset) => (
                <div class="layout-error">
                  <p>{t("error.layoutRender")}</p>
                  <pre class="layout-error-detail">{String(err)}</pre>
                  <button
                    class="primary"
                    onClick={() => handleResetLayout(activeWs()!.id)}
                  >
                    Reset to single pane
                  </button>
                </div>
              )}
            >
              {/* Phase 28: keyed Show on workspace id. Switching
                  workspaces (id changes) tears down the LayoutView
                  subtree so PaneView's onMount re-runs and attaches
                  the correct terminal container — fixes the
                  "switching workspaces shows the previous workspace's
                  terminal" bug. Layout edits within ONE workspace
                  (split / close pane) keep the same id, so Solid's
                  fine-grained reactivity handles them without a
                  full subtree recreation. Terminal instances live in
                  the g_terminals registry keyed by pane_id, so they
                  survive the DOM detach/reattach with no scrollback
                  or session loss. */}
              <Show when={activeWs()?.id} keyed>
                {(_id) => (
                  <LayoutView
                    workspaceId={activeWs()!.id}
                    node={(() => {
                      const ws = activeWs()!;
                      // Unshipped-fivefer (#4): prune panes that are popped out
                      // into their own OS window so the grid reflows to fill
                      // their slot. The pane_ids stay in ws.layout, so closing
                      // the popout un-prunes and restores them in place. Fall
                      // back to the full layout if EVERY pane is popped out
                      // (sole-pane workspace) so we never render an empty grid.
                      const hidden = poppedOut();
                      const base =
                        hidden.size > 0
                          ? pruneLayout(ws.layout!, hidden) ?? ws.layout!
                          : ws.layout!;
                      // Phase 55-A: when a pane is maximized, swap
                      // the tree for that one leaf so it fills the
                      // workspace area. Splits + the other panes are
                      // still in `ws.layout` so restore brings them
                      // straight back without re-creating any
                      // TerminalInstance (those are keyed by pane_id
                      // in the `terms` map, surviving the DOM detach).
                      //
                      // Phase 84.A: tabs mode is the same swap with the
                      // target always set — the active tab. That is the
                      // entire rendering story for tabs; the strip is
                      // just a control surface over `activePaneId`.
                      const target = tabsMode()
                        ? activePaneId() ?? collectPanes(base)[0] ?? null
                        : maximizedPaneId();
                      if (!target) return base;
                      const node = findPane(base, target);
                      if (node) return node;
                      // Falling back to `base` here would render the whole
                      // split grid, which in tabs mode is never right: the
                      // active pane can legitimately be missing (popped out
                      // into its own window, or left over from another
                      // workspace). Land on the first surviving leaf.
                      if (!tabsMode()) return base;
                      const first = collectPanes(base)[0];
                      return (first ? findPane(base, first) : null) ?? base;
                    })()}
                    activePaneId={activePaneId()}
                    connectedPaneIds={connectedPanes()}
                    waitingPaneIds={waitingPaneIds()}
                    notifiedPaneIds={paneNotified()}
                    panePulseEnabled={settings()?.notifications?.pane_pulse_on_activity ?? true}
                    workspaceConnection={activeWs()?.connection ?? undefined}
                    workspaceCwd={activeWs()?.cwd ?? undefined}
                    workspaceName={activeWs()?.name}
                    workspaceColor={activeWs()?.color ?? undefined}
                    workspaceEmoji={activeWs()?.emoji ?? undefined}
                    maximizedPaneId={maximizedPaneId()}
                    tabsMode={tabsMode()}
                    workspacePaneCount={(() => {
                      const l = activeWs()?.layout;
                      return l ? collectPanes(l).length : 0;
                    })()}
                    workspaceIsSsh={
                      // beta.3-localhost: was an inline layout walk (Phase 16).
                      // Collapsed to hasSftp() — same semantics, single site of
                      // truth. LayoutView keeps the prop for signature stability
                      // even though the local that consumed it is gone (see
                      // LayoutView.tsx LeafPane comment).
                      (() => {
                        const ws = activeWs();
                        return ws ? hasSftp(ws) : false;
                      })()
                    }
                    pendingPasswordFor={pendingPwFor()}
                    pendingPassphrase={pendingPassphraseFor()}
                    pendingHostTrust={pendingHostTrust()}
                    paneStatus={paneStatus()}
                    paneStatusText={paneStatusText()}
                    agentRuns={agentRuns()}
                    agentClockMs={agentClockMs}
                    panePersistence={panePersistence()}
                    ensureTerm={ensureTerm}
                    onFocus={focusPane}
                    onConnect={(pid, opts) => connectPane(pid, opts)}
                    onSplit={splitPane}
                    onClose={closePane}
                    onPopOut={popOutPane}
                    onDisconnect={disconnectPane}
                    onKillSession={killSession}
                    onSetTitle={(pid, title) => {
                      const ws = activeWs();
                      if (!ws) return;
                      invoke<WorkspacesFile>("pane_set_title", {
                        workspaceId: ws.id,
                        paneId: pid,
                        title: title.trim() === "" ? null : title,
                      })
                        .then((f) => updateFile(f))
                        .catch((e) => log.error("pane_set_title failed", e));
                    }}
                    onSetAnnotation={(pid, annotation) => {
                      const ws = activeWs();
                      if (!ws) return;
                      invoke<WorkspacesFile>("pane_set_annotation", {
                        workspaceId: ws.id,
                        paneId: pid,
                        annotation:
                          annotation.trim() === "" ? null : annotation,
                      })
                        .then((f) => updateFile(f))
                        .catch((e) =>
                          log.error("pane_set_annotation failed", e)
                        );
                    }}
                    onRatioDrag={(sid, r) => setRatio(sid, r, false)}
                    onRatioCommit={(sid, r) => setRatio(sid, r, true)}
                    onBrowserNavigate={browserNavigate}
                    onBrowserGoBack={browserGoBack}
                    onBrowserGoHome={browserGoHome}
                    onBrowserSetForward={browserSetForward}
                  />
                )}
              </Show>
            </ErrorBoundary>
          </div>
        </Show>
      </div>

      {/* Phase GG: in-app Markdown viewer (floating window). Reads its
          own global store, opened by FileManager .md double-click. */}
      <MarkdownViewer />

      {/* Unified side-panel lifecycle (see panels.ts): Notifications + Files
          each open docked as a drawer, then float out or expand to fullscreen.
          One PanelSurface per panel drives all three surfaces. Monitor is the
          InsightsWindow above; Diff + Browser follow on their own tracks. */}
      <PanelSurface
        surface={surfaceOf("notifications")}
        icon={<IconBell />}
        title={t("notif.title")}
        bodyClass="notif-body"
        drawerStorageKey="ymux.drawer-width.notifications"
        drawerDefaultWidth={440}
        drawerMinWidth={320}
        floatStorageKey="ymux.panel-notifications-geometry"
        floatDefault={{ x: 220, y: 90, w: 440, h: 640 } satisfies Geometry}
        floatMinW={320}
        floatMinH={360}
        onClose={() => closePanel("notifications")}
        onDrawer={() => openPanel("notifications")}
        onFloat={() => floatPanel("notifications")}
        onFullscreen={() => expandPanel("notifications")}
        headerActions={() => (
          <NotifHeaderActions onMarkAllRead={markAllNotifRead} onClear={clearNotifs} />
        )}
        body={() => (
          <NotificationCenter
            items={notifications()}
            readIds={notifRead()}
            onClose={() => closePanel("notifications")}
            // 66.G: jump to the exact pane. When only the pane is known
            // (OSC path), resolve its workspace by scanning the layouts.
            onJump={(wsId, paneId) => {
              const targetWs =
                wsId ??
                (paneId
                  ? file().workspaces.find(
                      (w) => w.layout && collectPanes(w.layout).includes(paneId),
                    )?.id ?? null
                  : null);
              if (!targetWs) return;
              void handleSetActive(targetWs).then(() => {
                if (!paneId) return;
                const ws = file().workspaces.find((w) => w.id === targetWs);
                if (ws?.layout && collectPanes(ws.layout).includes(paneId)) {
                  setActivePaneId(paneId);
                }
              });
            }}
            onMarkRead={markNotifRead}
            workspaceName={(id) => file().workspaces.find((w) => w.id === id)?.name}
          />
        )}
      />
      <PanelSurface
        surface={surfaceOf("files")}
        icon={<IconFolder />}
        title={t("files.window.title", { workspace: activeWs()?.name ?? "" })}
        drawerStorageKey="ymux.drawer-width.files"
        drawerDefaultWidth={900}
        drawerMinWidth={520}
        bodyClass="files-body"
        floatStorageKey={`ymux.panel-files-geometry.${file().active_workspace_id ?? "none"}`}
        floatDefault={{ x: 160, y: 100, w: 1100, h: 700 } satisfies Geometry}
        floatMinW={600}
        floatMinH={380}
        onClose={() => closePanel("files")}
        onDrawer={() => openPanel("files")}
        onFloat={() => floatPanel("files")}
        onFullscreen={() => expandPanel("files")}
        body={() => {
          const ws = activeWs();
          return ws ? (
            <FileManagerPane
              workspaceId={ws.id}
              hasSsh={isRemoteWorkspace(ws)}
              hasActiveSession={liveWorkspaceIds().has(ws.id)}
              rememberPath={settings()?.file_manager_remember_path === true}
            />
          ) : (
            <></>
          );
        }}
      />

      <CreateWorkspaceModal
        open={editingWorkspace() !== null}
        editing={editingWorkspace()}
        onClose={() => setEditingWorkspace(null)}
        onUpdate={handleUpdate}
        onOpenSshHelp={openSshHelp}
      />

      {/* Phase 47.E: removed the floating Notes (📝 N) and Settings (⚙)
          FABs from the workspace area — duplicates of the sidebar bottom
          row [📝 Notes][⚙ Settings][🌐 Ports] added in Phase 39 (re-added
          in Phase 40). The Ctrl+Shift+N keyboard shortcut for Notes
          stays wired separately. */}
      {/* Phase 56-A (extended by Phase 80): keyed Show forces the unified
          SetupWizard to fully unmount on close + freshly remount on
          re-open. Without this, the component instance lives across opens
          and its internal signals (mode tree, flow state, runId, …)
          stick — so opening the wizard after a completion screen would
          reopen to that completion. setShowSetup({}) creates a FRESH
          object per open, so the keyed remount holds. */}
      <Show keyed when={showSetup()}>
        {(opts) => (
          <SetupWizard
            initialTarget={opts.target}
            onClose={() => setShowSetup(false)}
            onCreateWorkspace={handleCreate}
            onOpenSshHelp={openSshHelp}
            onOpenWorkspace={(wsId, mode) => {
              // Phase 14.A.2: the wizard's backend already emitted
              // `workspaces:changed` when it created/updated the
              // workspace, so by the time we land here our local state
              // already shows the new entry. Switch to it + auto-connect
              // the first pane.
              void (async () => {
                try {
                  await handleSetActive(wsId);
                  const ws = file().workspaces.find((w) => w.id === wsId);
                  const firstPane =
                    ws?.layout ? collectPanes(ws.layout)[0] : null;
                  if (firstPane) {
                    setActivePaneId(firstPane);
                    connectPane(firstPane, {
                      persistent: true,
                      ...(mode === "claude" ? { mode: "claude" } : {}),
                    });
                  }
                } catch (e) {
                  log.error("open created workspace failed", e);
                }
              })();
            }}
          />
        )}
      </Show>

      {/* Phase 65.R/66: the Connect-to-existing-server flow lives inside
          the unified SetupWizard (server → existing → password); no
          separate modal mount here. */}

      <Show when={settings()}>
        <SettingsModal
          open={showSettings()}
          settings={settings()!}
          activeWorkspaceId={file().active_workspace_id ?? undefined}
          onClose={() => setShowSettings(false)}
          onChange={(next) => setSettings(next)}
        />
      </Show>

      {/* Phase 68.D: Server Insights monitor. Round B: docks as a side
          drawer by default; ⤢ pops it out into the floating window. */}
      <InsightsWindow
        surface={surfaceOf("monitor")}
        workspaceId={file().active_workspace_id ?? undefined}
        workspaceName={activeWs()?.name}
        local={activeWs()?.connection?.type === "local"}
        onClose={() => closePanel("monitor")}
        onDrawer={() => openPanel("monitor")}
        onFloat={() => floatPanel("monitor")}
        onFullscreen={() => expandPanel("monitor")}
        onInstall={() => {
          const ws = activeWs();
          if (ws) setAddonsWin({ id: ws.id, name: ws.name });
        }}
      />

      {/* Dev-Mode tickets for the active workspace. Same drawer → float
          → fullscreen lifecycle as the other side panels. */}
      <TicketsPanel
        surface={surfaceOf("tickets")}
        workspaceId={file().active_workspace_id ?? undefined}
        workspaceName={activeWs()?.name}
        onClose={() => closePanel("tickets")}
        onDrawer={() => openPanel("tickets")}
        onFloat={() => floatPanel("tickets")}
        onFullscreen={() => expandPanel("tickets")}
      />

      {/* BRIEF: the cross-workspace agent Queue — every agent pane sorted
          by who needs the user. Data comes from allPaneAgentRows so it can
          never disagree with the tab strip or the sidebar. */}
      <QueuePanel
        surface={surfaceOf("queue")}
        rows={allPaneAgentRows()}
        nowMs={agentClockMs()}
        onJump={(wsId, paneId) => {
          if (surfaceOf("queue") === "drawer") closePanel("queue");
          void (async () => {
            await handleSetActive(wsId);
            focusPane(paneId);
          })();
        }}
        onClose={() => closePanel("queue")}
        onDrawer={() => openPanel("queue")}
        onFloat={() => floatPanel("queue")}
        onFullscreen={() => expandPanel("queue")}
      />

      {/* Phase 68 (UX): per-workspace Add-ons window (from right-click). */}
      <AddonsWindow
        open={!!addonsWin()}
        workspaceId={addonsWin()?.id}
        workspaceName={addonsWin()?.name}
        separateClaudeAccount={
          file().workspaces.find((w) => w.id === addonsWin()?.id)
            ?.claude_separate_account ?? false
        }
        onToggleSeparateClaudeAccount={(v) => {
          const id = addonsWin()?.id;
          if (!id) return;
          void invoke("workspace_set_claude_separate_account", {
            workspaceId: id,
            enabled: v,
          }).catch((e) =>
            log.error("workspace_set_claude_separate_account failed", e),
          );
        }}
        onClose={() => setAddonsWin(null)}
      />

      {/* Phase 32.B: SSH key offer. Self-contained — listens for the
          `ssh-key-offer` event on its own, no props needed. */}
      <SshKeyOfferModal />

      {/* Phase 35 (#1.3): command palette (Ctrl+Shift+P). */}
      <CommandPalette
        open={showPalette()}
        commands={paletteCommands()}
        onClose={() => setShowPalette(false)}
      />

      {/* Phase 40 → 46: floating Ports window, scoped to the active
          workspace. Detected ports show as click-to-forward; forwarded
          ports show Open/Stop. No FeedItem on either event. */}
      <PortsWindow
        open={showPortsWindow()}
        activeWorkspace={activeWs()}
        detectedPorts={detectedPorts()}
        forwards={portForwards()}
        onClose={() => setShowPortsWindow(false)}
        onStop={stopForward}
        onStart={startForward}
        onToggleAutoForward={handleToggleAutoForward}
      />

      {/* Phase 53 (rebased): floating workspace-level Browser window.
          The native child Webview lives on the Rust side keyed by
          workspace_id; this shell owns the chrome (header, drag, resize,
          persisted geometry). Hide-on-close preserves page state until
          the workspace is deleted. */}
      <BrowserWindow
        open={showBrowserWindow()}
        workspace={activeWs()}
        anyModalOpen={anyModalOpen}
        onClose={() => setShowBrowserWindow(false)}
        onPopOut={() => void popOutBrowser()}
        detectedPorts={(() => {
          const id = file().active_workspace_id;
          return id
            ? detectedPorts()
                .filter((p) => p.workspace_id === id)
                .map((p) => ({
                  remote_port: p.remote_port,
                  addr: p.addr,
                  family: p.family,
                }))
            : [];
        })()}
        forwards={(() => {
          const id = file().active_workspace_id;
          return id
            ? portForwards()
                .filter((f) => f.workspace_id === id)
                .map((f) => ({
                  remote_port: f.remote_port,
                  local_port: f.local_port,
                }))
            : [];
        })()}
        onEnsurePorts={ensurePortsSnapshot}
        onStartForward={(remotePort) => {
          const id = file().active_workspace_id;
          if (!id) return Promise.reject(new Error("no active workspace"));
          return startForward(id, remotePort);
        }}
      />

      {/* Dev Mode ticket capture. Opened by browser:ticket-captured;
          folded into anyModalOpen() above so the Browser Webview is
          hidden while it's up (it would otherwise paint over this). */}
      {/* Remote folder browser for pinning a project folder. Scoped to
          the workspace whose context menu opened it — that is where the
          SFTP session comes from. */}
      <Show when={dirPickerFor()}>
        {(d) => (
          <DirPicker
            workspaceId={d().workspaceId}
            onClose={() => setDirPickerFor(null)}
            onPick={(dir) => {
              // Read EVERYTHING off the accessor before clearing the
              // signal: `setDirPickerFor(null)` unmounts this <Show>, and
              // `d()` past that point is not guaranteed to still resolve.
              const { workspaceId, connection } = d();
              setDirPickerFor(null);
              void pinProjectFolder(workspaceId, dir, connection);
            }}
          />
        )}
      </Show>

      {/* Destructive, and cascades over a subtree — so it gets a real
          dialog rather than a browser confirm. */}
      <Show when={pendingDelete()}>
        {(sub) => (
          <ConfirmDeleteWorkspace
            subtree={sub()}
            liveIds={liveWorkspaceIds()}
            noteCount={notesInSubtree(sub())}
            onClose={() => setPendingDelete(null)}
            onConfirm={() => void commitDelete(sub()[0].id)}
          />
        )}
      </Show>

      {/* Project folders: pin a repo path, or create a worktree inside
          one. Folded into anyModalOpen() for the same Webview reason. */}
      <Show when={projectFolderModal()}>
        {(m) => (
          <ProjectFolderModal
            mode={m()}
            onClose={() => setProjectFolderModal(null)}
            onDone={() => void reloadWorkspaces()}
          />
        )}
      </Show>

      <Show when={pendingCapture()}>
        {(pc) => (
          <TicketModal
            workspaceId={pc().workspaceId}
            capture={pc().capture}
            onClose={() => setPendingCapture(null)}
            onSaved={() => {
              setPendingCapture(null);
              flashSummaryToast("ok", t("browser.dev.captured"));
            }}
          />
        )}
      </Show>

      {/* Phase 58: voice-input recording indicator + error toast.
          Floating top-right, dismissible only by stopping the
          recording (release the PTT key) or letting the 5s timeout
          clear the error. Mutually exclusive in practice — the
          recorder finally{} clears sttListening before sttError
          gets set on the error path. */}
      <Show when={sttListening()}>
        <div class="stt-indicator" role="status">
          <span class="stt-indicator-dot" />
          <span>{t("stt.listening")}</span>
        </div>
      </Show>
      <Show when={sttError()}>
        <div class="stt-indicator stt-indicator-err" role="alert">
          {t("stt.error", { message: sttError()! })}
        </div>
      </Show>

      <Show when={updateBanner()}>
        <div class="update-banner" role="status">
          <div class="update-banner-body">
            <strong>ymux {updateBanner()!.latest_version}</strong>{" "}
            is available — current {updateBanner()!.current_version}.
            {/* Phase 65 (U): when auto-install fails, tell the user they
                can still get the update manually so they're never stuck. */}
            <Show when={installError()}>
              {" "}
              <span class="update-banner-err">{t("update_banner.install_error_hint")}</span>
            </Show>
          </div>
          <div class="update-banner-actions">
            {/* Phase 27: one-click auto-install. The backend downloads
                the NSIS installer, verifies its sha256 against the
                manifest, runs it, and exits the app. */}
            <button
              class="update-banner-install"
              disabled={installingUpdate()}
              onClick={() => void installUpdate()}
            >
              {installingUpdate()
                ? t("update_banner.installing")
                : t("update_banner.install")}
            </button>
            {/* Phase 65 (U): manual GitHub fallback — always available
                as the release-notes/download link, and the primary
                escape hatch after an install error. */}
            <Show when={updateBanner()!.notes_url}>
              <a
                class="update-banner-link"
                href={updateBanner()!.notes_url ?? "#"}
                target="_blank"
                rel="noopener noreferrer"
              >
                {installError()
                  ? t("update_banner.manual_download")
                  : t("update_banner.notes")}
              </a>
            </Show>
            {/* Phase 65 (U): defer options. */}
            <button
              class="update-banner-secondary"
              disabled={installingUpdate()}
              onClick={() => void remindUpdateLater()}
            >
              {t("update_banner.remind_later")}
            </button>
            <button
              class="update-banner-secondary"
              disabled={installingUpdate()}
              onClick={() => void skipUpdateVersion()}
            >
              {t("update_banner.skip")}
            </button>
            <button class="update-banner-x" onClick={() => setUpdateBanner(null)}>×</button>
          </div>
        </div>
      </Show>

      {/* Remote CLI out of sync for the ACTIVE workspace. No dismiss button:
          it is a live functional gap (hooks, tickets and the reverse-tunnel
          watcher are all switched off while it shows), and it clears itself
          the moment a bootstrap converges. */}
      <Show when={cliSkew()[activeWs()?.id ?? ""]}>
        <div class="hooks-banner" role="status">
          <div class="hooks-banner-body">
            <strong>{t("cli_skew.banner.title")}</strong>
            <span class="hooks-banner-detail">
              {t("cli_skew.banner.text", {
                expected: cliSkew()[activeWs()!.id].expected ?? "—",
                actual: cliSkew()[activeWs()!.id].actual || "—",
                reason: cliSkew()[activeWs()!.id].reason ?? "—",
              })}
            </span>
          </div>
        </div>
      </Show>

      <Show when={hooksBanner()}>
        <div class="hooks-banner" role="status">
          <div class="hooks-banner-body">
            <strong>{t("hooks_update.banner.title")}</strong>
            <span class="hooks-banner-detail">
              {t("hooks_update.banner.text", {
                agent: hooksBanner()!.agent,
                current: hooksBanner()!.current ?? "—",
                latest: hooksBanner()!.latest,
              })}
            </span>
          </div>
          <div class="hooks-banner-actions">
            <button
              class="hooks-banner-btn primary"
              disabled={hooksUpdating()}
              onClick={() => void triggerHooksUpdate()}
            >
              {hooksUpdating() ? t("common.saving") : t("hooks_update.btn.update")}
            </button>
            <button class="hooks-banner-btn" onClick={dismissHooksLater}>
              {t("hooks_update.btn.later")}
            </button>
            <button class="hooks-banner-btn" onClick={() => void skipHooksVersion()}>
              {t("hooks_update.btn.skip")}
            </button>
          </div>
        </div>
      </Show>

      <Show when={summaryToast()}>
        <div
          class={`summary-toast ${summaryToast()!.kind}`}
          onClick={() => setSummaryToast(null)}
          role="status"
        >
          <span class="summary-toast-icon">{summaryToast()!.kind === "ok" ? "✓" : "⚠"}</span>
          <span class="summary-toast-text">{summaryToast()!.text}</span>
        </div>
      </Show>

      {/* beta.3 (netfree, Track 1b): reconnect toast — persistent (does
          NOT auto-dismiss), shows attempt counter + a cancel button.
          Rendered next to summary-toast so both can coexist visually. */}
      <Show when={reconnectToasts().length > 0}>
        <div class="reconnect-toast" role="status">
          <div class="reconnect-toast-body">
            <span class="reconnect-toast-spinner" aria-hidden="true">⟳</span>
            <div class="reconnect-toast-text">
              <div class="reconnect-toast-title">
                {reconnectToasts().length === 1
                  ? t("reconnect.title", { host: reconnectToasts()[0].host })
                  : t("reconnect.title_multi", {
                      n: String(reconnectToasts().length),
                    })}
              </div>
              <div class="reconnect-toast-attempt">
                {/* Panes are jittered apart, so they sit on different
                    attempt numbers — show the furthest along, which is
                    the one that tells the user how close we are to
                    giving up. */}
                {reconnectToasts().length === 1
                  ? t("reconnect.attempt", {
                      n: String(Math.max(1, reconnectToasts()[0].attempt)),
                      max: String(reconnectToasts()[0].max),
                    })
                  : t("reconnect.attempt_multi", {
                      n: String(reconnectToasts().length),
                      a: String(
                        Math.max(1, ...reconnectToasts().map((r) => r.attempt)),
                      ),
                      max: String(RECONNECT_MAX),
                    })}
              </div>
            </div>
          </div>
          <button
            type="button"
            class="reconnect-toast-cancel"
            onClick={() => cancelReconnect()}
          >
            {t("reconnect.cancel")}
          </button>
        </div>
      </Show>

      <NotesModal
        open={showNotes()}
        notes={notes()}
        workspaces={file().workspaces}
        activeWorkspaceId={file().active_workspace_id}
        onClose={() => setShowNotes(false)}
        onAdd={(text, tag, workspaceId) => {
          invoke<Note>("notes_add", {
            text,
            tag: tag ?? null,
            workspaceId: workspaceId ?? null,
            paneId: null,
          })
            .then(() => refreshNotes())
            .catch((e) => log.error("notes_add failed", e));
        }}
        onDone={(id) =>
          invoke("notes_update", { id, status: "done" })
            .then(() => refreshNotes())
            .catch((e) => log.error("notes_update done failed", e))
        }
        onReopen={(id) =>
          invoke("notes_update", { id, status: "open" })
            .then(() => refreshNotes())
            .catch((e) => log.error("notes_update reopen failed", e))
        }
        onDelete={(id) =>
          invoke("notes_delete", { id })
            .then(() => refreshNotes())
            .catch((e) => log.error("notes_delete failed", e))
        }
      />

      <FeedPanel
        items={feedItems()}
        workspaces={file().workspaces}
        activeWorkspaceId={activeWs()?.id ?? null}
        onDecide={(rid, dec) => {
          // Optimistic local update — backend event will reaffirm.
          setFeedItems((prev) =>
            prev.map((i) =>
              i.request_id === rid
                ? { ...i, state: dec === "allow" ? "allowed" : "denied" }
                : i
            )
          );
          invoke("feed_decide", { requestId: rid, decision: dec }).catch(
            (err) => log.error("feed_decide failed", err)
          );
        }}
        onDismiss={(rid) =>
          setFeedItems((prev) => prev.filter((i) => i.request_id !== rid))
        }
      />
    </div>
  );
}

function updateRatioInLayout(
  node: LayoutNode,
  splitId: string,
  ratio: number
): LayoutNode {
  if (node.kind === "pane") return node;
  if (node.split_id === splitId) {
    return { ...node, ratio: Math.max(0.05, Math.min(0.95, ratio)) };
  }
  return {
    ...node,
    first: updateRatioInLayout(node.first, splitId, ratio),
    second: updateRatioInLayout(node.second, splitId, ratio),
  };
}

export default App;
