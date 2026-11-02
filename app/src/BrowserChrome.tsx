import {
  createEffect,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { backend } from "./backend";
import type { Workspace } from "./types";
import { t } from "./i18n";
import type { Geometry } from "./floatingWindow";
import {
  IconClose,
  IconRefresh,
  IconUnplug,
  IconGlobe,
  IconWarning,
  IconBug,
  IconTerminal,
} from "./icons";
import { applyDevMode, loadDevMode, saveDevMode } from "./browserDevMode";
import { createLogger } from "./logger";

const log = createLogger("BROWSER");

// Phase 85.C: the workspace Browser's chrome, shared by BOTH hosts —
// the in-app floating window (`BrowserWindow.tsx`) and the popped-out OS
// window (`components/PopoutBrowser.tsx`).
//
// The page itself is always a NATIVE CHILD WEBVIEW that the Rust side
// mounts with `Window::add_child` (`workspace_browser.rs`); everything
// here is the HTML around it — tabs, the port bar, Go, Dev Mode, the Web
// Inspector button, and the empty state. The native content paints ABOVE
// HTML and only over the SLOT, so the chrome stays clickable.
//
// Exactly two things differ between the hosts, and they are injected:
//
//   slotRect()   where the native webview goes, in coordinates relative
//                to its host window. The floating panel derives it from
//                its drag geometry; the popout from the OS window size.
//   windowLabel  which OS window to attach the child to. A webview
//                CANNOT be re-parented, so popping out destroys and
//                respawns rather than moving one.
//
// Split as a factory + a body component rather than one component on
// purpose: the state and effects must outlive the JSX. In the floating
// panel the component stays mounted while the window is closed (only the
// inner `<Show>` collapses), and the falling-edge close effect is what
// hides the native webview — put it inside the `<Show>` and it would
// unmount before it could ever fire. `makeWindowControls` in
// `floatingWindow.tsx` is the same shape.

export interface DetectedPort {
  remote_port: number;
  addr: string;
  family: string;
}
export interface ForwardInfo {
  remote_port: number;
  local_port: number;
}

export interface BrowserChromeProps {
  /** Whether the host surface is visible. The popout host is always open. */
  open: boolean;
  /** The workspace — its id keys the Webview + all persistence. */
  workspace: Workspace | null;
  /** A modal covering the host would be painted under the native webview,
   *  so the host reports it and we hide. Always false in the popout. */
  anyModalOpen: () => boolean;
  detectedPorts: DetectedPort[];
  forwards: ForwardInfo[];
  onEnsurePorts: (workspaceId: string) => void;
  onStartForward: (remotePort: number) => Promise<number>;
  /** Where the native child Webview goes, relative to the host window. */
  slotRect: () => Geometry;
  /** The OS window to attach the child Webview to (`"main"` or
   *  `browser-popout-<ws>`). */
  windowLabel: string;
}

/** Tab bar height. Must match `.browser-window-tabs` in App.css. */
export const CHROME_TABS_H = 26;
/** Port bar height. Must match `.browser-window-portbar` in App.css. */
export const CHROME_PORTBAR_H = 32;

const PORT_KEY = (id: string) => `ymux.workspace-browser-port.${id}`;
const PATH_KEY = (id: string) => `ymux.workspace-browser-path.${id}`;
// Beta.3: per-workspace tabs. Each tab captures its own (port, path) —
// no free-form URL — matching the port-picker model of the port bar.
// Legacy PORT_KEY / PATH_KEY are migrated into a single tab on first
// load, then this key takes over. Keyed on workspace id, NOT on the host
// window, so tabs survive a pop-out round trip (which reloads the page).
const TABS_KEY = (id: string) => `ymux.workspace-browser-tabs.${id}`;
const ACTIVE_TAB_KEY = (id: string) =>
  `ymux.workspace-browser-active-tab.${id}`;

/** One tab inside the workspace browser. `port`/`path` mirror what the
 *  port bar shows — nothing else is captured; the visible label is
 *  derived at render time from `port · path`. */
interface BrowserTab {
  id: string;
  port: number | null;
  path: string;
}

function newTabId(): string {
  const rand = Math.floor(Math.random() * 1e9).toString(36);
  return `tab-${Date.now().toString(36)}-${rand}`;
}

function loadTabs(
  workspaceId: string,
): { tabs: BrowserTab[]; activeId: string } {
  try {
    const raw = localStorage.getItem(TABS_KEY(workspaceId));
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        const tabs: BrowserTab[] = parsed
          .filter(
            (t): t is BrowserTab =>
              typeof t === "object" &&
              t !== null &&
              typeof (t as { id?: unknown }).id === "string" &&
              typeof (t as { path?: unknown }).path === "string" &&
              (typeof (t as { port?: unknown }).port === "number" ||
                (t as { port?: unknown }).port === null),
          )
          .map((t) => ({ id: t.id, port: t.port, path: t.path }));
        if (tabs.length > 0) {
          const activeRaw = localStorage.getItem(ACTIVE_TAB_KEY(workspaceId));
          const activeId =
            activeRaw && tabs.some((t) => t.id === activeRaw)
              ? activeRaw
              : tabs[0]!.id;
          return { tabs, activeId };
        }
      }
    }
  } catch {
    // Corrupt entry — fall through to legacy migration.
  }
  // Legacy migration: build a single tab from the pre-tabs PORT_KEY /
  // PATH_KEY so users don't lose their last-used port/path on upgrade.
  const tab: BrowserTab = {
    id: newTabId(),
    port: loadPort(workspaceId),
    path: loadPath(workspaceId),
  };
  return { tabs: [tab], activeId: tab.id };
}

function saveTabs(
  workspaceId: string,
  tabs: BrowserTab[],
  activeId: string | null,
): void {
  try {
    localStorage.setItem(TABS_KEY(workspaceId), JSON.stringify(tabs));
    if (activeId) localStorage.setItem(ACTIVE_TAB_KEY(workspaceId), activeId);
  } catch {
    // quota / private mode — ignore
  }
}

/** Human-readable label for a tab: `port · path` or "New tab" if empty. */
function tabLabel(t: BrowserTab): string {
  if (t.port == null) return "New tab";
  return t.path ? `${t.port} · ${t.path}` : `${t.port}`;
}

function loadPort(workspaceId: string): number | null {
  try {
    const raw = localStorage.getItem(PORT_KEY(workspaceId));
    if (!raw) return null;
    const n = Number(raw);
    return Number.isFinite(n) && n > 0 ? n : null;
  } catch {
    return null;
  }
}
function savePort(workspaceId: string, port: number): void {
  try {
    localStorage.setItem(PORT_KEY(workspaceId), String(port));
  } catch {
    // ignore
  }
}
function loadPath(workspaceId: string): string {
  try {
    return localStorage.getItem(PATH_KEY(workspaceId)) || "";
  } catch {
    return "";
  }
}
function savePath(workspaceId: string, path: string): void {
  try {
    localStorage.setItem(PATH_KEY(workspaceId), path);
  } catch {
    // ignore
  }
}

/** Leading-slash-normalize the path field. "" stays "" (root). */
function normalizePath(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "";
  return trimmed.startsWith("/") ? trimmed : `/${trimmed}`;
}

/** A friendly label for a detected port — the port number, plus the
 *  bind address when it's not the loopback default. */
function portLabel(p: DetectedPort): string {
  const showAddr = p.addr && p.addr !== "127.0.0.1" && p.addr !== "localhost";
  return showAddr ? `${p.remote_port} · ${p.addr}` : `${p.remote_port}`;
}

/** What `createBrowserChrome` hands back to its host + body. */
export interface BrowserChromeApi {
  props: BrowserChromeProps;
  currentUrl: () => string | null;
  navError: () => string | null;
  chromeError: () => string | null;
  tabs: () => BrowserTab[];
  activeTabId: () => string | null;
  selectedPort: () => number | null;
  setSelectedPort: (p: number | null) => void;
  pathInput: () => string;
  setPathInput: (s: string) => void;
  devMode: () => boolean;
  toggleDevMode: () => void;
  openDevtools: () => void;
  go: () => Promise<void>;
  refresh: () => void;
  switchTab: (id: string) => void;
  newTab: () => void;
  closeTab: (id: string) => void;
  /** Hide whatever native Webview is currently on screen. The host calls
   *  this when it is going away for good (the popout window closing). */
  hideShown: () => void;
}

export function createBrowserChrome(p: BrowserChromeProps): BrowserChromeApi {
  const [selectedPort, setSelectedPort] = createSignal<number | null>(null);
  const [pathInput, setPathInput] = createSignal("");
  // The localhost tunnel URL currently loaded in the Webview. null ⇒
  // nothing navigated yet → Webview hidden, empty-state/hint shown.
  const [currentUrl, setCurrentUrl] = createSignal<string | null>(null);
  const [navError, setNavError] = createSignal<string | null>(null);
  // Phase 85.A: a transient error strip inside the port bar. The
  // empty-state `navError` above only renders while NO url is loaded
  // (`<Show when={!currentUrl()}>`), so it can never report a failure of
  // an action taken ON a loaded page — which is how the DevTools button
  // stayed broken for a whole release: its only failure channel was a
  // `log.warn` into a file. Anything in the chrome that can fail while a
  // page is up reports here. Lives in the port-bar row on purpose: the
  // native child webview paints OVER the slot, so an overlay down there
  // would be invisible.
  const [chromeError, setChromeError] = createSignal<string | null>(null);
  let chromeErrorTimer: number | null = null;
  const flashChromeError = (msg: string): void => {
    if (chromeErrorTimer) clearTimeout(chromeErrorTimer);
    setChromeError(msg);
    chromeErrorTimer = window.setTimeout(() => setChromeError(null), 6000);
  };
  onCleanup(() => {
    if (chromeErrorTimer) clearTimeout(chromeErrorTimer);
  });
  const [tabs, setTabs] = createSignal<BrowserTab[]>([]);
  const [activeTabId, setActiveTabId] = createSignal<string | null>(null);
  // Dev Mode: right-click an element in the page to open a ticket. All
  // of the behaviour lives in browserDevMode.ts; this is just the flag.
  const [devMode, setDevMode] = createSignal(false);
  let suppressActiveSync = false;
  // One-shot guard so the persisted "last port" auto-opens only once per
  // open session (not every time detectedPorts updates).
  let autoTried = false;
  // Phase 62.C (F.1): the URL last handed to the native webview. Lets the
  // show effect tell a geometry change (same url → reposition only) from
  // a real URL change (different port → must navigate; the backend
  // fast-path only repositions, it doesn't navigate).
  let lastShownUrl: string | null = null;

  // Phase 85.B: the workspace whose native Webview is actually ON SCREEN
  // right now — which is NOT necessarily `p.workspace?.id`. Every hide
  // path used to pass the ACTIVE workspace id, so: open the Browser in
  // ws A (Webview A shown), switch to ws B, and the switch handler below
  // hid *B* — a no-op, B has no Webview — leaving A's Webview painted
  // over the slot region. X did the same thing, so there was no way back:
  // the ghost survived every workspace switch until the app exited.
  // One writer (the show effect's `.then`), one reader (`hideShown`).
  let shownWsId: string | null = null;
  const hideShown = (): void => {
    const id = shownWsId;
    if (!id) return;
    shownWsId = null;
    void backend.call("workspace_browser_hide", { workspaceId: id }).catch(() => {});
  };

  // Track the workspace by ID (object identity churns on every file()
  // refresh). On a real workspace change: reload persisted port/path,
  // drop the loaded URL, and pull a fresh port snapshot.
  let lastWsId: string | null = null;
  createEffect(() => {
    const id = p.workspace?.id;
    if (!id || id === lastWsId) return;
    // Hide the OUTGOING workspace's Webview before we forget which one
    // it was. `setCurrentUrl(null)` below makes the show effect hide the
    // INCOMING workspace, which is almost always a no-op.
    hideShown();
    lastWsId = id;
    setDevMode(loadDevMode(id));
    // Beta.3: load tabs (migrating legacy PORT_KEY / PATH_KEY on first
    // run) and seed the port/path editors from the active tab.
    const { tabs: loaded, activeId } = loadTabs(id);
    const active = loaded.find((t) => t.id === activeId) ?? loaded[0]!;
    suppressActiveSync = true;
    setTabs(loaded);
    setActiveTabId(active.id);
    setSelectedPort(active.port);
    setPathInput(active.path);
    suppressActiveSync = false;
    // Persist so the legacy migration lands even if the user just
    // opens & closes the window without editing anything.
    saveTabs(id, loaded, active.id);
    setCurrentUrl(null);
    setNavError(null);
    autoTried = false;
    lastShownUrl = null;
    if (p.open) p.onEnsurePorts(id);
  });

  // Beta.3: mirror the live editor state (selectedPort / pathInput)
  // into the active tab and persist. `suppressActiveSync` lets a tab
  // switch / workspace load set the editors without a write-back.
  createEffect(() => {
    const port = selectedPort();
    const path = pathInput();
    const activeId = activeTabId();
    const wsId = p.workspace?.id;
    if (!activeId || !wsId) return;
    if (suppressActiveSync) return;
    setTabs((prev) => {
      let changed = false;
      const next = prev.map((t) => {
        if (t.id !== activeId) return t;
        if (t.port === port && t.path === path) return t;
        changed = true;
        return { ...t, port, path };
      });
      if (!changed) return prev;
      saveTabs(wsId, next, activeId);
      return next;
    });
  });

  // Rising edge of `open`: ensure the watcher + refresh the snapshot so
  // the dropdown is populated even when auto_port_forward is off.
  let wasOpenForPorts = false;
  createEffect(() => {
    const open = p.open;
    const id = p.workspace?.id;
    if (open && !wasOpenForPorts && id) {
      autoTried = false;
      p.onEnsurePorts(id);
    }
    wasOpenForPorts = open;
  });

  // Auto-open the persisted port once it actually shows up in the
  // detected list (reopen-to-last behavior).
  createEffect(() => {
    if (!p.open || p.anyModalOpen()) return;
    if (currentUrl() || autoTried) return;
    const port = selectedPort();
    if (port == null) return;
    if (!p.detectedPorts.some((d) => d.remote_port === port)) return;
    autoTried = true;
    void go();
  });

  // Spawn / show / reposition / navigate the native Webview — or hide it
  // when there's no URL yet (so the empty-state HTML shows through).
  createEffect(() => {
    if (!p.open) return;
    const id = p.workspace?.id;
    if (!id) return;
    if (p.anyModalOpen()) return;
    const url = currentUrl();
    if (!url) {
      hideShown();
      return;
    }
    const s = p.slotRect();
    const label = p.windowLabel;
    // Phase 62.C (F.1): workspace_browser_show spawns (loading `url`) on
    // the first call, or repositions + shows an existing webview. Its
    // fast path does NOT navigate, so when the URL changed (a different
    // port) we navigate explicitly. A geometry change keeps the same url
    // → reposition only.
    const urlChanged = lastShownUrl !== null && lastShownUrl !== url;
    lastShownUrl = url;
    void backend.call("workspace_browser_show", {
      workspaceId: id,
      url,
      x: s.x,
      y: s.y,
      w: s.w,
      h: s.h,
      windowLabel: label,
    })
      .then(() => {
        // Only now is a Webview genuinely on screen for `id` — this is
        // the single writer of `shownWsId` (Phase 85.B).
        shownWsId = id;
        if (urlChanged) {
          return backend.call("workspace_browser_navigate", { workspaceId: id, url });
        }
      })
      .catch((err) => log.error("workspace_browser_show failed", err));
  });

  // Hide the Webview when the host CLOSES (not only on unmount — in the
  // floating panel this factory stays alive; only the JSX collapses).
  // Phase 85.B: hides whatever is actually shown, and no longer requires
  // an active workspace — a close with `p.workspace` momentarily
  // undefined used to skip the hide entirely and leak the Webview.
  let wasOpen = false;
  createEffect(() => {
    const open = p.open;
    if (wasOpen && !open) hideShown();
    wasOpen = open;
  });

  // ── Beta.3: tab operations ────────────────────────────────────────
  const switchTab = (id: string): void => {
    const tab = tabs().find((x) => x.id === id);
    if (!tab) return;
    const wsId = p.workspace?.id;
    suppressActiveSync = true;
    setActiveTabId(id);
    setSelectedPort(tab.port);
    setPathInput(tab.path);
    suppressActiveSync = false;
    setCurrentUrl(null);
    setNavError(null);
    autoTried = false;
    lastShownUrl = null;
    if (wsId) saveTabs(wsId, tabs(), id);
    // Auto-navigate if the tab's port is already detected on the server.
    if (
      tab.port != null &&
      p.detectedPorts.some((d) => d.remote_port === tab.port)
    ) {
      void go();
    }
  };

  const newTab = (): void => {
    const wsId = p.workspace?.id;
    if (!wsId) return;
    // Inherit the current port so exploring multiple paths on one
    // service is one keystroke; blank if nothing's been picked yet.
    const fresh: BrowserTab = {
      id: newTabId(),
      port: selectedPort(),
      path: "",
    };
    const next = [...tabs(), fresh];
    setTabs(next);
    saveTabs(wsId, next, fresh.id);
    switchTab(fresh.id);
  };

  const closeTab = (id: string): void => {
    const list = tabs();
    if (list.length <= 1) return; // never close the last tab
    const idx = list.findIndex((t) => t.id === id);
    if (idx === -1) return;
    const wsId = p.workspace?.id;
    const next = list.filter((t) => t.id !== id);
    setTabs(next);
    if (activeTabId() === id) {
      // Prefer the previous tab; fall through to index 0 if we closed [0].
      const neighbor = next[Math.max(0, idx - 1)] ?? next[0]!;
      if (wsId) saveTabs(wsId, next, neighbor.id);
      switchTab(neighbor.id);
    } else if (wsId) {
      saveTabs(wsId, next, activeTabId());
    }
  };

  // Ctrl+T / Ctrl+W / Ctrl+Tab while the browser is open. Uses
  // window.addEventListener so it fires whether focus is on the port
  // bar, the webview host div, or nowhere in particular.
  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (!p.open) return;
      if (p.anyModalOpen()) return;
      const meta = e.ctrlKey || e.metaKey;
      if (!meta) return;
      if (e.key === "t" && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        newTab();
      } else if (e.key === "w" && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        const id = activeTabId();
        if (id) closeTab(id);
      } else if (e.key === "Tab") {
        e.preventDefault();
        const list = tabs();
        if (list.length < 2) return;
        const idx = list.findIndex((t) => t.id === activeTabId());
        if (idx === -1) return;
        const delta = e.shiftKey ? -1 : 1;
        const nxt = list[(idx + delta + list.length) % list.length]!;
        switchTab(nxt.id);
      }
    };
    window.addEventListener("keydown", handler);
    onCleanup(() => window.removeEventListener("keydown", handler));
  });

  // Dev Mode arms the inspect script inside the page. A page load wipes
  // it (the script lives in the document, not the webview), so re-arm
  // whenever the loaded URL changes while Dev Mode is on. The script is
  // idempotent — it tears down any previous instance first.
  //
  // The delay is a deliberate simplification: `workspace_browser_eval`
  // is fire-and-forget with no load event to hook, so we wait for the
  // new document instead of racing it. If a page is still loading after
  // this, toggling Dev Mode off/on re-arms it.
  createEffect(() => {
    const ws = p.workspace;
    const url = currentUrl();
    if (!ws || !devMode() || !url) return;
    const id = ws.id;
    const timer = setTimeout(() => void applyDevMode(id, true), 500);
    onCleanup(() => clearTimeout(timer));
  });

  const toggleDevMode = () => {
    const ws = p.workspace;
    if (!ws) return;
    const next = !devMode();
    setDevMode(next);
    saveDevMode(ws.id, next);
    void applyDevMode(ws.id, next);
  };

  // Open the inspector on the page itself. Only this webview is
  // inspectable — see `.devtools(...)` in workspace_browser.rs. On macOS
  // this saves a trip through Safari → Develop; on Windows it is the only
  // way in, since F12 is not wired up for a child webview.
  const openDevtools = () => {
    const wsId = p.workspace?.id;
    if (!wsId) return;
    backend.call<void>("workspace_browser_open_devtools", { workspaceId: wsId }).catch(
      (e: unknown) => {
        log.warn(`open devtools failed: ${String(e)}`);
        flashChromeError(t("browser.devtools.failed", { msg: String(e) }));
      },
    );
  };

  // Resolve the chosen remote port to a local tunnel port (reusing an
  // existing forward when present, else opening one) and point the
  // Webview at it.
  const go = async () => {
    const ws = p.workspace;
    if (!ws) return;
    const port = selectedPort();
    if (port == null) return;
    setNavError(null);
    let local = p.forwards.find((f) => f.remote_port === port)?.local_port;
    if (local == null) {
      try {
        local = await p.onStartForward(port);
      } catch (e) {
        setNavError(t("browser.ports.forwardFailed", { msg: String(e) }));
        return;
      }
    }
    const path = normalizePath(pathInput());
    savePort(ws.id, port);
    savePath(ws.id, pathInput());
    // Phase 62.A (item F): use 127.0.0.1, NOT localhost. The local
    // tunnel listener binds 127.0.0.1 (IPv4) only; on dual-stack
    // Windows `localhost` resolves to ::1 (IPv6) first, which both
    // fails to connect AND — when it falls back — makes the page ORIGIN
    // differ from the 127.0.0.1 origin the Ports window uses, tripping
    // the service's CORS / cookie checks. PortsWindow already learned
    // this; the in-app browser now matches it.
    setCurrentUrl(`http://127.0.0.1:${local}${path}`);
  };

  const refresh = () => {
    const id = p.workspace?.id;
    if (id) p.onEnsurePorts(id);
  };

  return {
    props: p,
    currentUrl,
    navError,
    chromeError,
    tabs,
    activeTabId,
    selectedPort,
    setSelectedPort,
    pathInput,
    setPathInput,
    devMode,
    toggleDevMode,
    openDevtools,
    go,
    refresh,
    switchTab,
    newTab,
    closeTab,
    hideShown,
  };
}

/** Tab bar + port bar + slot. The host supplies whatever wraps it —
 *  a draggable header and resize handles in the floating panel, nothing
 *  at all in the popout (the OS window has its own title bar). */
export function BrowserChromeBody(props: { api: BrowserChromeApi }) {
  const a = () => props.api;
  return (
    <>
      {/* Beta.3: tab bar. Each tab is a (port, path) pair — no URL —
          matching the port-picker model of the port bar below. */}
      <div class="browser-window-tabs">
        <div class="bw-tabs-list">
          <For each={a().tabs()}>
            {(tab) => (
              <div
                class={`bw-tab${
                  a().activeTabId() === tab.id ? " bw-tab-active" : ""
                }`}
                onClick={() => a().switchTab(tab.id)}
                onAuxClick={(e) => {
                  // Middle-click closes the tab (button === 1).
                  if (e.button === 1) {
                    e.preventDefault();
                    a().closeTab(tab.id);
                  }
                }}
                title={tabLabel(tab)}
              >
                <span class="bw-tab-label">{tabLabel(tab)}</span>
                <Show when={a().tabs().length > 1}>
                  <button
                    class="bw-tab-close"
                    onClick={(e) => {
                      e.stopPropagation();
                      a().closeTab(tab.id);
                    }}
                    title={t("common.close")}
                    aria-label={t("common.close")}
                  >
                    <IconClose size={10} />
                  </button>
                </Show>
              </div>
            )}
          </For>
          <button
            class="bw-tab-new"
            onClick={() => a().newTab()}
            title={t("browser.tabs.new")}
            aria-label={t("browser.tabs.new")}
          >
            +
          </button>
        </div>
      </div>
      {/* Phase 62.C: port bar replaces the free URL bar. Refresh ·
          remote-port dropdown · path · Go. */}
      <div class="browser-window-portbar">
        <button
          class="bw-port-btn"
          title={t("browser.ports.refresh")}
          onClick={() => a().refresh()}
        >
          <IconRefresh size={14} />
        </button>
        <span class="bw-port-server">{t("browser.ports.serverPrefix")}</span>
        <Show
          when={a().props.detectedPorts.length > 0}
          fallback={
            <span class="bw-port-none">{t("browser.ports.none.inline")}</span>
          }
        >
          <select
            class="bw-port-select"
            value={a().selectedPort() ?? ""}
            onChange={(e) => {
              const v = e.currentTarget.value;
              a().setSelectedPort(v === "" ? null : Number(v));
            }}
          >
            <Show when={a().selectedPort() == null}>
              <option value="">{t("browser.ports.choose")}</option>
            </Show>
            <For each={a().props.detectedPorts}>
              {(d) => <option value={d.remote_port}>{portLabel(d)}</option>}
            </For>
          </select>
        </Show>
        <span class="bw-port-sep">/</span>
        <input
          class="bw-port-path"
          type="text"
          value={a().pathInput()}
          placeholder={t("browser.ports.path.placeholder")}
          onInput={(e) => a().setPathInput(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void a().go();
            }
            // Keep keystrokes out of the global shortcut handler.
            e.stopPropagation();
          }}
        />
        <button
          class="bw-port-go"
          onClick={() => void a().go()}
          disabled={a().selectedPort() == null}
          title={t("browser.ports.go")}
        >
          {t("browser.ports.go")}
        </button>
        {/* Dev Mode. Off by default; while on, right-clicking any
            element in the page opens a ticket for it. */}
        <button
          class={`bw-port-btn bw-devmode-btn ${a().devMode() ? "active" : ""}`}
          onClick={() => a().toggleDevMode()}
          title={t(a().devMode() ? "browser.dev.off" : "browser.dev.on")}
          aria-pressed={a().devMode()}
        >
          <IconBug size={14} />
        </button>
        {/* Web Inspector on the loaded page — console, network, the
            site's own errors. Enabled only for this webview. */}
        <button
          class="bw-port-btn bw-devtools-btn"
          onClick={() => a().openDevtools()}
          title={t("browser.devtools.open")}
          aria-label={t("browser.devtools.open")}
        >
          <IconTerminal size={14} />
        </button>
        {/* Phase 85.A: failures of chrome actions taken while a page is
            loaded. The empty-state error below can't cover those — it
            only renders when no URL is loaded. */}
        <Show when={a().chromeError()}>
          <span class="bw-chrome-err" title={a().chromeError()!}>
            <IconWarning size={12} /> {a().chromeError()}
          </span>
        </Show>
      </div>
      <div class="browser-window-slot">
        {/* Empty-state / hint — visible only while the Webview is
            hidden (no URL loaded). */}
        <Show when={!a().currentUrl()}>
          <div class="browser-window-empty">
            <Show
              when={a().props.detectedPorts.length === 0}
              fallback={
                <div class="bw-empty-state">
                  <div class="bw-empty-icon"><IconUnplug /></div>
                  <p class="bw-empty-body">{t("browser.empty.pickHint")}</p>
                  <Show when={a().navError()}>
                    <p class="bw-empty-err"><IconWarning size={14} /> {a().navError()}</p>
                  </Show>
                </div>
              }
            >
              <div class="bw-empty-state">
                <div class="bw-empty-icon"><IconGlobe /></div>
                <h3 class="bw-empty-title">{t("browser.empty.title")}</h3>
                <p class="bw-empty-body">{t("browser.empty.body")}</p>
                <button class="bw-empty-refresh" onClick={() => a().refresh()}>
                  <IconRefresh size={14} /> {t("browser.ports.refresh.label")}
                </button>
                <Show when={a().navError()}>
                  <p class="bw-empty-err"><IconWarning size={14} /> {a().navError()}</p>
                </Show>
              </div>
            </Show>
          </div>
        </Show>
      </div>
    </>
  );
}
