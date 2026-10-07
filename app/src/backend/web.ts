// The browser backend (Phase 109, WEB-DESIGN C5). Answers the desktop's own
// command names from the daemon that served this page, so the UI code is the
// desktop's, unchanged.
//
// - Commands: one handler per name in `handlers` below. A name with no handler
//   is a feature the browser host does not have; it rejects (logged once) — a
//   button that can reach one is a capability-gating bug (Phase 107).
// - Events: `EventBus` (events.ts), fed by the daemon's events socket and by
//   this file (PTY, translated events).
// - Workspaces: the daemon's `/api/v2/web/workspaces` documents, mapped to the
//   desktop `Workspace` shape. Layout operations run here (layoutOps.ts) and
//   are written back with the document's version; a 409 re-applies the
//   operation once on the newer document.
// - Panes: a leaf's pane id is also its tmux session's hook pane id (the
//   daemon takes `pane_id` on create since 2.10.0), so the leaf a hook lights
//   is the leaf the UI drew, across reconnects and reloads.
//
// Logging: this module cannot import `logger.ts` (the logger calls through
// `backend`, which would be an import cycle), so it uses `console.*` — in a
// browser that IS the log sink; there is no debug.log to ship to. Rule #1 /
// Rule #8: no PTY bytes and no token ever reach it.

import type { LayoutNode } from "../bindings/LayoutNode";
import type { Settings } from "../settings";
import type { WorkspacesFile } from "../types";
import type { Workspace } from "../bindings/Workspace";
import type { WorkspaceGroup } from "../bindings/WorkspaceGroup";
import type { WorktreeEntry } from "../bindings/WorktreeEntry";
import type { DiffSource } from "../bindings/DiffSource";
import type { Connection } from "../bindings/Connection";
import type { SplitDirection } from "../bindings/SplitDirection";
import type { PaneKind } from "../bindings/PaneKind";
import {
  closeLeaf,
  findLeaf,
  leafIds,
  makeLeaf,
  newPaneId,
  patchLeaf,
  resetRatios,
  setRatio,
  splitLeaf,
  swapLeaves,
} from "../layoutOps";
import { getPaneSession, rememberPaneSession } from "../sessionRestore";
import { ApiError, api, forgetToken, getToken, setUnauthorizedHandler } from "./web/api";
import { startPwa } from "./web/pwa";
import { installWebMono } from "./web/fonts";
import { isHeader, rootIdOf, screenOrSelf } from "../wsTree";
import {
  DEFAULT_SCREEN_NAME,
  activeAfterDelete,
  checkIdentity,
  checkPin,
  folderLabel,
  pickSessionParent,
  reorder,
  reorderGroups,
  subtreeIds,
  uniqueSiblingName,
} from "./web/tree";
import { WEB_DEFAULT_SETTINGS, withDefaults } from "./web/defaults";
import { EventBus, EventsSocket, type Hello } from "./web/events";
import { splitArgs } from "./web/argv";
import { FilesBridge, LARGE_FILE_BYTES } from "./web/files";
import { fileFor, onDragDrop as onLocalDragDrop, pickPaths as pickLocalFiles } from "./web/localfiles";
import { PtySessions } from "./web/pty";
import type {
  Backend,
  Capability,
  DragDropPayload,
  EventCallback,
  HostShell,
  PickPathsOptions,
  UnlistenFn,
} from "./types";

/** The daemon's browser workspace document (term/webws.go). */
interface WebWS {
  id: string;
  name: string;
  version: number;
  layout?: LayoutNode | null;
  tabs_mode?: boolean;
  intent?: string;
  is_project_root?: boolean;
  /** Phase 115 (F1): the desktop's tree fields (META_KEYS), stored opaque. */
  meta?: Record<string, unknown> | null;
  created_at: string;
  updated_at: string;
}

/** The Workspace fields that travel in `meta` (term/webws.go). */
const META_KEYS = [
  "parent_id",
  "cwd",
  "is_folder",
  "is_collapsed",
  "sort_order",
  "group_id",
  "color",
  "emoji",
  "tmux_session",
] as const;

/** The fields the daemon stores as named columns. */
const TOP_KEYS = ["name", "layout", "intent", "is_project_root", "tabs_mode"] as const;

const strOrNull = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

/** What the daemon's session list returns (term/meta.go Annotated). */
interface DaemonSession {
  name: string;
  windows: number;
  created: number;
  attached: number;
  path: string;
  display: string;
  label?: string;
  auto_name?: string;
  claude_title?: string;
  claude_session_id?: string;
  origin?: string;
}

type Args = Record<string, unknown>;

/** The desktop's diff-pane-updated payload (diff_pane.rs). */
interface DiffEvent {
  pane_id: string;
  diff_text: string;
  files: { xy: string; path: string; orig_path: string | null }[];
  error: string | null;
  cwd: string;
  branch: string | null;
  truncated: boolean;
}
type Handler = (a: Args) => Promise<unknown>;

const ACTIVE_KEY = "ymux.web.activeWorkspace";
/** Popout windows (F1): `/?popout=<sid>`; the opener stores sid → tmux name here. */
const POPOUT_KEY = (sid: string) => `ymux.web.popout.${sid}`;
const popoutSid = ((): string | null => {
  try {
    return new URLSearchParams(location.search).get("popout");
  } catch {
    return null;
  }
})();
const str = (v: unknown): string => (typeof v === "string" ? v : "");
const num = (v: unknown, d: number): number => (typeof v === "number" && Number.isFinite(v) ? v : d);

/** Every browser pane runs on the daemon's own box — remote, tmux-backed. */
const serverConnection = (): Connection => ({
  type: "ssh",
  host: location.hostname,
  user: "",
  port: location.port ? Number(location.port) : location.protocol === "https:" ? 443 : 80,
  key_path: null,
});

/** tmux-safe: ValidName rejects ':' '.' control chars and a leading '-'. */
const slug = (s: string): string =>
  s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 24) || "ws";

/** The tmux name a browser pane's session is created with. */
const derivedName = (w: { name: string }, paneId: string): string =>
  `${slug(w.name)}-${paneId.slice(-6).replace(/[^a-z0-9]/gi, "")}`;

export class WebBackend implements Backend {
  readonly kind = "web" as const;
  // "popout": a pane opens in its own browser window (popout_pane below).
  // Phase 118 (F4): the Diff pane and worktrees, over the daemon's git.
  readonly caps: ReadonlySet<Capability> = new Set<Capability>(["popout", "diffPane", "worktrees"]);
  readonly bus = new EventBus();
  private pty = new PtySessions(this.bus);
  private files = new FilesBridge();
  private events: EventsSocket;
  private hello: Hello | null = null;
  private ws: WebWS[] = [];
  private groups: WorkspaceGroup[] = [];
  private groupsVersion = 0;
  private settingsVersion = 0;
  private settingsCache: Settings = structuredClone(WEB_DEFAULT_SETTINGS);
  /** leaf pane id → tmux session name (hello `panes` + our creates). */
  private paneSession = new Map<string, string>();
  /** leaf pane id → live attach sid, and back. */
  private paneSid = new Map<string, string>();
  private warned = new Set<string>();
  /** Diff panes being watched: pane id → the poll's state (Phase 118). */
  private diffWatch = new Map<string, { timer: number; hash: string | null; err: string | null; stopped: boolean }>();
  version = "";

  readonly host: HostShell = {
    // index.tsx routes `popout-<sid>` to PopoutTerminal, as on the desktop.
    windowLabel: () => (popoutSid ? `popout-${popoutSid}` : "main"),
    setTitle: async (title) => {
      document.title = title;
    },
    setZoom: async (factor) => {
      document.documentElement.style.setProperty("zoom", String(factor));
    },
    closeWindow: async () => window.close(),
    appVersion: async () => this.version,
    openUrl: async (url) => {
      window.open(url, "_blank", "noopener,noreferrer");
    },
    revealInDir: () => Promise.reject("not available in the browser"),
    // Phase 116 (F2): the user's own files as path-shaped tokens (web/localfiles.ts).
    pickPaths: (opts: PickPathsOptions) => pickLocalFiles(opts),
    savePath: () => Promise.resolve(null),
    onDragDrop: (cb: EventCallback<DragDropPayload>) => onLocalDragDrop(cb),
  };

  constructor() {
    this.events = new EventsSocket(
      () => (this.settingsCache.i18n?.language === "he" ? "he" : "en"),
      (type, data) => this.onDaemonEvent(type, data),
      (h, isFirst) => this.onHello(h, isFirst),
    );
    setUnauthorizedHandler(() => {
      forgetToken();
      location.reload();
    });
  }

  can(cap: Capability): boolean {
    return this.caps.has(cap);
  }

  /**
   * Boot: version, settings, workspaces, then the events socket's first hello.
   * Rejects with an ApiError (401/403) when the token cannot reach the
   * terminal routes, so index.tsx can show the login screen instead.
   */
  async init(): Promise<void> {
    if (!getToken()) throw new ApiError(401, "not signed in");
    const font = installWebMono(); // in parallel; awaited before the first pane
    const v = await fetch("/api/version").then((r) => r.json() as Promise<{ version?: string }>);
    this.version = v.version ?? "";
    await this.loadSettings();
    await this.reloadWorkspaces();
    await this.events.start();
    await this.seedRestoreHints();
    await font;
    if (popoutSid) {
      // A popout window: its own tmux client on the opener's session, under
      // the opener's sid (PopoutTerminal's only handle). tmux redraws on the
      // first resize, so nothing before the attach is lost.
      let name = "";
      try {
        name = localStorage.getItem(POPOUT_KEY(popoutSid)) ?? "";
      } catch {
        /* no storage */
      }
      if (!name) throw new Error("this popout's session is unknown — open it again from the pane");
      await this.pty.open(name, 80, 24, popoutSid);
      return;
    }
    // Phase 114: the PWA side — never blocks the boot.
    void startPwa({
      lang: () => (this.settingsCache.i18n?.language === "he" ? "he" : "en"),
      onGateResolved: (cb) =>
        this.bus.on("feed:item-resolved", (e) => {
          const id = (e.payload as { request_id?: unknown } | null)?.request_id;
          if (typeof id === "string") cb(id);
        }),
    });
  }

  /**
   * Session restore (App.restoreSessions) re-attaches a pane only when this
   * browser's localStorage remembers its tmux session. A second browser, or
   * one whose storage was cleared, remembers nothing — so seed the hints from
   * what the daemon knows: the hello's pane → session map, else a live
   * session with the leaf's derived name.
   */
  private async seedRestoreHints(): Promise<void> {
    let live: Set<string>;
    try {
      live = new Set((await this.sessions()).map((x) => x.name));
    } catch {
      return; // restore just falls back to [Connect]
    }
    for (const w of this.ws) {
      for (const pid of leafIds(w.layout ?? null)) {
        const known = this.paneSession.get(pid);
        const name = known && live.has(known) ? known : live.has(derivedName(w, pid)) ? derivedName(w, pid) : "";
        if (!name) continue;
        this.paneSession.set(pid, name);
        if (!getPaneSession(pid)) rememberPaneSession(pid, name);
      }
    }
  }

  // ── Backend ────────────────────────────────────────────────────────────

  call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const h = this.handlers[cmd];
    if (!h) {
      if (!this.warned.has(cmd)) {
        this.warned.add(cmd);
        console.warn(`[web] command not available in the browser: ${cmd}`);
      }
      return Promise.reject(`not available in the browser: ${cmd}`);
    }
    // Tauri rejects with the command's error string; keep that shape.
    return (h(args ?? {}) as Promise<T>).catch((e: unknown) =>
      Promise.reject(e instanceof Error ? e.message : String(e)),
    );
  }

  on<T>(event: string, cb: EventCallback<T>): Promise<UnlistenFn> {
    return Promise.resolve(this.bus.on(event, cb));
  }

  emit(event: string, payload?: unknown): Promise<void> {
    this.bus.emit(event, payload ?? null);
    return Promise.resolve();
  }

  // ── events ─────────────────────────────────────────────────────────────

  private onHello(h: Hello, isFirst: boolean): void {
    this.hello = h;
    for (const [pid, p] of Object.entries(h.panes ?? {})) this.paneSession.set(pid, p.session);
    if (!isFirst) {
      // The socket came back: whatever happened meanwhile is in this hello.
      void this.reloadWorkspaces().then(() => this.bus.emit("workspaces:changed", null));
      this.bus.emit("backend:resync", null);
    }
  }

  private onDaemonEvent(type: string, data: unknown): void {
    if (type === "settings:changed") {
      const d = data as { version?: number; settings?: unknown };
      this.settingsVersion = num(d.version, this.settingsVersion);
      this.settingsCache = withDefaults(d.settings);
      this.bus.emit("settings:changed", this.settingsCache);
      return;
    }
    if (type === "workspaces:changed") {
      // The desktop's payload is (); App reloads through workspaces_load.
      void this.reloadWorkspaces().then(() => this.bus.emit("workspaces:changed", null));
      return;
    }
    this.bus.emit(type, data);
  }

  // ── settings ───────────────────────────────────────────────────────────

  private async loadSettings(): Promise<Settings> {
    const d = await api<{ version: number; settings: unknown }>("GET", "/api/v2/settings");
    this.settingsVersion = d.version;
    this.settingsCache = withDefaults(d.settings);
    return this.settingsCache;
  }

  private async saveSettings(s: Settings): Promise<Settings> {
    const put = (version: number) =>
      api<{ version: number; settings: unknown }>("PUT", "/api/v2/settings", { version, settings: s });
    let d: { version: number; settings: unknown };
    try {
      d = await put(this.settingsVersion);
    } catch (e) {
      // Another browser saved first. Settings saves are whole documents from
      // the Settings dialog, so the latest save wins — same as the desktop.
      if (!(e instanceof ApiError && e.status === 409)) throw e;
      const cur = e.body as { version?: number } | null;
      d = await put(num(cur?.version, this.settingsVersion));
    }
    this.settingsVersion = d.version;
    this.settingsCache = withDefaults(d.settings);
    return this.settingsCache;
  }

  // ── workspaces ─────────────────────────────────────────────────────────

  private async reloadWorkspaces(): Promise<void> {
    const [ws, g] = await Promise.all([
      api<WebWS[]>("GET", "/api/v2/web/workspaces"),
      api<{ version: number; groups: WorkspaceGroup[] }>("GET", "/api/v2/web/groups").catch(() => null),
    ]);
    this.ws = ws;
    if (g) {
      this.groups = Array.isArray(g.groups) ? g.groups : [];
      this.groupsVersion = g.version;
    }
  }

  private activeId(): string | null {
    let id: string | null = null;
    try {
      id = localStorage.getItem(ACTIVE_KEY);
    } catch {
      /* no storage */
    }
    const all = this.all();
    // A header is never active (lib.rs workspace_set_active): it hands over
    // to its first screen.
    const screen = id && all.some((w) => w.id === id) ? screenOrSelf(all, id) : null;
    return screen ?? activeAfterDelete(all, null);
  }

  private setActive(id: string | null): void {
    try {
      if (id) localStorage.setItem(ACTIVE_KEY, id);
    } catch {
      /* per-tab only */
    }
  }

  private all(): Workspace[] {
    return this.ws.map((w, i) => this.toWorkspace(w, i));
  }

  private toWorkspace(w: WebWS, _i: number): Workspace {
    const updated = Date.parse(w.updated_at) || 0;
    const m = w.meta ?? {};
    return {
      id: w.id,
      name: w.name,
      color: strOrNull(m.color),
      emoji: strOrNull(m.emoji),
      cwd: strOrNull(m.cwd),
      connection: serverConnection(),
      layout: w.layout ?? null,
      setup_command: null,
      teardown_command: null,
      env: [],
      auto_port_forward: false,
      // ts-rs types u64 as bigint, but serde_json (and so every value the
      // desktop frontend has ever seen here) is a plain number.
      last_active_at: updated as unknown as bigint,
      git_worktree: null,
      claude_separate_account: false,
      group_id: strOrNull(m.group_id),
      sort_order: typeof m.sort_order === "number" ? m.sort_order : null,
      parent_id: strOrNull(m.parent_id),
      is_project_root: w.is_project_root ?? false,
      is_collapsed: m.is_collapsed === true,
      tabs_mode: w.tabs_mode ?? false,
      tmux_session: strOrNull(m.tmux_session),
      intent: w.intent ?? null,
      known_sessions: [],
      is_folder: m.is_folder === true,
    };
  }

  /** Split a Workspace patch into the daemon's columns and its meta object. */
  private body(cur: WebWS | null, patch: Partial<Workspace>): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const k of TOP_KEYS) if (k in patch) out[k] = patch[k];
    if (META_KEYS.some((k) => k in patch)) {
      const meta: Record<string, unknown> = { ...(cur?.meta ?? {}) };
      for (const k of META_KEYS) if (k in patch) meta[k] = patch[k];
      out.meta = meta;
    }
    return out;
  }

  /** Change fields of one row (columns and meta alike). */
  private patch(id: string, p: Partial<Workspace>): Promise<WorkspacesFile> {
    return this.mutate(id, (w) => this.body(w, p));
  }

  /** Create a row; `meta` always present, so the daemon never migrates it. */
  private async createRow(p: Partial<Workspace> & { name: string }): Promise<Workspace> {
    const b = this.body(null, p);
    const created = await api<WebWS>("POST", "/api/v2/web/workspaces", { ...b, meta: b.meta ?? {} });
    this.ws.push(created);
    return this.toWorkspace(created, this.ws.length - 1);
  }

  /** lib.rs screen_under: a screen inheriting its header's directory. */
  private screenUnder(header: Workspace, name: string): Promise<Workspace> {
    return this.createRow({
      name,
      parent_id: header.id,
      cwd: header.cwd,
      layout: makeLeaf(newPaneId()),
    });
  }

  private getRow(id: string): Workspace {
    const w = this.all().find((x) => x.id === id);
    if (!w) throw new Error("workspace not found");
    return w;
  }

  private async saveGroups(next: WorkspaceGroup[]): Promise<void> {
    const put = (version: number) =>
      api<{ version: number; groups: WorkspaceGroup[] }>("PUT", "/api/v2/web/groups", { version, groups: next });
    let d: { version: number; groups: WorkspaceGroup[] };
    try {
      d = await put(this.groupsVersion);
    } catch (e) {
      // Groups change from one dialog at a time; the latest write wins.
      if (!(e instanceof ApiError && e.status === 409)) throw e;
      d = await put(num((e.body as { version?: number } | null)?.version, this.groupsVersion));
    }
    this.groups = d.groups;
    this.groupsVersion = d.version;
  }

  private async gitWorktrees(path: string): Promise<{ ok: boolean; worktrees: WorktreeEntry[]; error?: string }> {
    return api("POST", "/api/v2/web/git/worktrees", { path });
  }

  private file(): WorkspacesFile {
    return {
      version: 1,
      active_workspace_id: this.activeId(),
      workspaces: this.all(),
      groups: this.groups,
    };
  }

  private find(id: string): WebWS {
    const w = this.ws.find((x) => x.id === id);
    if (!w) throw new Error(`no workspace ${id}`);
    return w;
  }

  /**
   * Write a change to one workspace. `op` maps the current document to the
   * fields to PUT; on a version conflict it runs once more on the newer one.
   */
  private async mutate(id: string, op: (w: WebWS) => Record<string, unknown>): Promise<WorkspacesFile> {
    const put = (w: WebWS) => api<WebWS>("PUT", `/api/v2/web/workspaces/${encodeURIComponent(id)}`, { version: w.version, ...op(w) });
    let next: WebWS;
    try {
      next = await put(this.find(id));
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409 && e.body)) throw e;
      next = await put(e.body as WebWS);
    }
    this.ws = this.ws.map((x) => (x.id === id ? next : x));
    return this.file();
  }

  private layoutOp(id: string, fn: (l: LayoutNode) => LayoutNode): Promise<WorkspacesFile> {
    return this.mutate(id, (w) => {
      if (!w.layout) throw new Error("workspace has no layout");
      return { layout: fn(w.layout) };
    });
  }

  // ── panes ──────────────────────────────────────────────────────────────

  private async sessions(): Promise<DaemonSession[]> {
    return api<DaemonSession[]>("GET", "/api/v2/term/sessions");
  }

  private async connect(a: Args): Promise<string> {
    const wsId = str(a.workspaceId);
    const paneId = str(a.paneId);
    const w = this.find(wsId);
    if (!findLeaf(w.layout ?? null, paneId)) throw new Error(`no pane ${paneId}`);
    const old = this.paneSid.get(paneId);
    if (old) this.pty.close(old);
    const live = new Set((await this.sessions()).map((s) => s.name));
    // Which session is this leaf's: the caller's choice (picker / restore
    // hint), else what we already know, else a live session with the name
    // this leaf's session was created with (a browser that never saw it).
    let name = str(a.tmuxSessionName) || this.paneSession.get(paneId) || "";
    if (!name && live.has(derivedName(w, paneId))) name = derivedName(w, paneId);
    if (!name || !live.has(name)) {
      // A fresh pane, or one whose session ended: (re)create it carrying the
      // leaf's own pane id.
      const want = name || derivedName(w, paneId);
      // "claude" mode runs claude as the session's program (daemon 2.11.0);
      // a custom `cmd` string is not split into an argv here, so it opens a
      // shell like the default mode.
      const cmd = str(a.mode) === "claude" ? ["claude", ...splitArgs(str(a.claudeArgs))] : undefined;
      let created: { name: string };
      try {
        created = await api<{ name: string }>("POST", "/api/v2/term/sessions", {
          name: want,
          cwd: str(a.cwdOverride),
          workspace_id: wsId,
          pane_id: paneId,
          // A leaf shows what it was last connected to: a new session takes
          // the pane over from one still running (it keeps running, released).
          replace_pane: true,
          cmd,
        });
      } catch (e) {
        // The name is taken by an unrelated session: let the daemon mint one.
        if (!(e instanceof ApiError && e.status === 409)) throw e;
        created = await api<{ name: string }>("POST", "/api/v2/term/sessions", {
          cwd: str(a.cwdOverride),
          workspace_id: wsId,
          pane_id: paneId,
          // A leaf shows what it was last connected to: a new session takes
          // the pane over from one still running (it keeps running, released).
          replace_pane: true,
          cmd,
        });
      }
      name = created.name;
    }
    this.paneSession.set(paneId, name);
    const sid = await this.pty.open(name, num(a.cols, 80), num(a.rows, 24));
    this.paneSid.set(paneId, sid);
    // Seed smart bidi from the leaf (lib.rs seeds the filter at pane_connect).
    const leaf = findLeaf(w.layout ?? null, paneId);
    if (leaf && "smart_bidi" in leaf && leaf.smart_bidi) this.pty.setBidi(sid, true);
    return sid;
  }

  private disconnect(paneId: string): void {
    const sid = this.paneSid.get(paneId);
    if (sid) this.pty.close(sid);
    this.paneSid.delete(paneId);
  }

  private async kill(paneId: string): Promise<unknown> {
    const name = this.paneSession.get(paneId);
    if (!name) return { result: "no_session", backend: "tmux" };
    try {
      await api("DELETE", `/api/v2/term/sessions/${encodeURIComponent(name)}`);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return { result: "already_gone", backend: "tmux", session: name };
      throw e;
    }
    this.paneSession.delete(paneId);
    return { result: "killed", backend: "tmux", session: name };
  }

  // ── the command table ──────────────────────────────────────────────────

  private handlers: Record<string, Handler> = {
    // host
    host_platform: async () => "linux",
    ui_log_batch: async () => null, // the console already has it
    diag_log: async () => null,
    set_tray_badge: async () => null,
    record_recent_path: async () => null,
    list_recent_paths: async () => [],
    workspace_ensure_connected: async () => null, // the daemon is the host
    workspace_ensure_port_watcher: async () => null,
    list_detected_ports: async () => [],
    log_dir_path: async () => "", // no local log file: the browser console is the log

    // settings
    settings_load: () => this.loadSettings(),
    settings_save: (a) => this.saveSettings(a.settings as Settings),
    settings_reset: () => this.saveSettings(structuredClone(WEB_DEFAULT_SETTINGS)),
    settings_get_presets: async () => [],
    list_system_fonts: async () => ({ ui: [], mono: [] }),
    font_catalog: async () => [],

    // workspaces
    workspaces_load: async () => {
      await this.reloadWorkspaces();
      return this.file();
    },
    workspace_set_active: async (a) => {
      const id = str(a.workspaceId);
      if (id) {
        const screen = screenOrSelf(this.all(), id);
        if (!screen) throw new Error(`"${this.getRow(id).name}" has no screens yet`);
        this.setActive(screen);
      }
      return this.file();
    },
    // ── the tree (Phase 115, F1 — lib.rs semantics, backend/web/tree.ts) ──
    workspace_create: async (a) => {
      // A root header + its first screen (lib.rs create_root_with_screen).
      const input = (a.input ?? {}) as { name?: string; cwd?: string | null; color?: string | null };
      const root = await this.createRow({
        name: (input.name ?? "").trim() || "workspace",
        cwd: strOrNull(input.cwd?.trim()),
        color: strOrNull(input.color),
      });
      const screen = await this.screenUnder(root, DEFAULT_SCREEN_NAME);
      this.setActive(screen.id);
      return this.file();
    },
    workspace_new_screen: async (a) => {
      const header = this.getRow(str(a.parentWorkspaceId));
      if (!isHeader(header)) throw new Error("a screen cannot hold screens — use its header");
      const base = str(a.name).trim() || DEFAULT_SCREEN_NAME;
      const screen = await this.screenUnder(header, uniqueSiblingName(this.all(), header.id, base));
      this.setActive(screen.id);
      return this.file();
    },
    project_folder_probe: async (a) => {
      if (!str(a.path).trim()) throw new Error("project path is required");
      const r = await this.gitWorktrees(str(a.path).trim());
      return r.ok && r.worktrees.length > 0;
    },
    git_probe_worktrees: async (a) => {
      if (!str(a.path).trim()) throw new Error("project path is required");
      const r = await this.gitWorktrees(str(a.path).trim());
      if (!r.ok) throw new Error(r.error || "not a git repository");
      return r.worktrees;
    },
    workspace_pin_project_folder: async (a) => {
      const parentId = str(a.parentWorkspaceId);
      const path = checkPin(this.all(), parentId, str(a.path));
      const folder = await this.createRow({
        name: folderLabel(path, a.name as string | null),
        cwd: path,
        parent_id: parentId,
        is_project_root: a.isProjectRoot === true,
        is_folder: true,
      });
      const screen = await this.screenUnder(folder, DEFAULT_SCREEN_NAME);
      this.setActive(screen.id);
      return this.file();
    },
    workspace_set_project_root: async (a) => {
      const w = this.getRow(str(a.workspaceId));
      const want = a.isProjectRoot === true;
      return w.is_project_root === want ? this.file() : this.patch(w.id, { is_project_root: want });
    },
    workspace_open_session: async (a) => {
      const name = str(a.sessionName).trim();
      if (!name) throw new Error("session name is required");
      const all = this.all();
      const rootId = rootIdOf(all, this.getRow(str(a.workspaceId)).id);
      // One box, so "same host" is every row.
      const existing = all.find((w) => w.tmux_session === name);
      if (existing) {
        this.setActive(existing.id);
        return this.file();
      }
      const cwd = strOrNull(str(a.cwd).trim());
      const row = await this.createRow({
        name: str(a.displayName).trim() || name,
        cwd,
        parent_id: pickSessionParent(all, rootId, cwd),
        tmux_session: name,
        layout: makeLeaf(newPaneId()),
      });
      this.setActive(row.id);
      return this.file();
    },
    workspace_set_collapsed: (a) => this.patch(str(a.workspaceId), { is_collapsed: a.collapsed === true }),
    workspace_set_intent: async (a) => {
      const id = str(a.workspaceId);
      await this.patch(id, { intent: strOrNull(str(a.intent).trim()) });
      return this.getRow(id);
    },
    workspace_set_identity: async (a) => {
      const id = str(a.workspaceId);
      const color = a.color === null || a.color === undefined ? null : str(a.color);
      const emoji = a.emoji === null || a.emoji === undefined ? null : str(a.emoji);
      checkIdentity(color, emoji);
      await this.patch(id, { color, emoji });
      return this.getRow(id);
    },
    workspace_reorder: async (a) => {
      const changes = reorder(
        this.all(),
        this.groups,
        str(a.workspaceId),
        a.groupId === null || a.groupId === undefined ? null : str(a.groupId),
        num(a.newIndex, 0),
      );
      for (const [id, p] of changes) await this.patch(id, p);
      return this.file();
    },
    workspace_group_create: async (a) => {
      const name = str(a.name).trim();
      if (!name) throw new Error("group name is required");
      const g: WorkspaceGroup = {
        id: `g_${Date.now().toString(16)}${Math.random().toString(16).slice(2, 6)}`,
        name,
        color: str(a.color),
        is_collapsed: false,
        sort_order: null,
      };
      await this.saveGroups([...this.groups, g]);
      return g;
    },
    workspace_group_update: async (a) => {
      const id = str(a.id);
      if (!this.groups.some((g) => g.id === id)) throw new Error(`no group ${id}`);
      await this.saveGroups(
        this.groups.map((g) =>
          g.id !== id
            ? g
            : {
                ...g,
                name: typeof a.name === "string" && a.name.trim() ? a.name.trim() : g.name,
                color: typeof a.color === "string" ? a.color : g.color,
                is_collapsed: typeof a.isCollapsed === "boolean" ? a.isCollapsed : g.is_collapsed,
              },
        ),
      );
      return null;
    },
    workspace_group_delete: async (a) => {
      const id = str(a.id);
      for (const w of this.all().filter((x) => x.group_id === id)) await this.patch(w.id, { group_id: null });
      await this.saveGroups(this.groups.filter((g) => g.id !== id));
      return null;
    },
    workspace_set_group: async (a) => {
      const gid = a.groupId === null || a.groupId === undefined ? null : str(a.groupId);
      if (gid !== null && !this.groups.some((g) => g.id === gid)) throw new Error(`no group ${gid}`);
      const id = str(a.workspaceId);
      if (!this.ws.some((w) => w.id === id)) throw new Error(`no workspace ${id}`);
      await this.patch(id, { group_id: gid });
      return null;
    },
    workspace_group_reorder: async (a) => {
      await this.saveGroups(reorderGroups(this.groups, str(a.groupId), num(a.newIndex, 0)));
      return this.file();
    },
    workspace_rename: (a) => this.mutate(str(a.workspaceId), () => ({ name: str(a.name) })),
    workspace_update: (a) =>
      this.mutate(str(a.workspaceId), () => (typeof a.name === "string" && a.name ? { name: a.name } : {})),
    workspace_delete: async (a) => {
      // A row owns its subtree (lib.rs workspace_delete). Panes detach; the
      // tmux sessions are the caller's to kill, as on the desktop.
      const id = str(a.workspaceId);
      const wasActive = this.activeId();
      const parent = this.getRow(id).parent_id;
      const ids = subtreeIds(this.all(), id);
      for (const x of [...ids].reverse()) {
        for (const pid of leafIds(this.find(x).layout ?? null)) this.disconnect(pid);
        await api("DELETE", `/api/v2/web/workspaces/${encodeURIComponent(x)}`);
        this.ws = this.ws.filter((w) => w.id !== x);
      }
      if (wasActive && ids.includes(wasActive)) this.setActive(activeAfterDelete(this.all(), parent));
      return this.file();
    },
    workspace_remember_sessions: async () => this.file(),
    workspace_mirror_sessions: async () => this.file(),
    workspace_set_tabs_mode: (a) => this.mutate(str(a.workspaceId), () => ({ tabs_mode: a.tabsMode === true })),
    workspace_split: (a) => {
      const kind = (str(a.paneKind) || "terminal") as PaneKind;
      return this.layoutOp(str(a.workspaceId), (l) => {
        const out = splitLeaf(l, str(a.paneId), (str(a.direction) || "horizontal") as SplitDirection, makeLeaf(newPaneId(), kind));
        if (!out) throw new Error(`no pane ${str(a.paneId)}`);
        return out;
      });
    },
    workspace_close_pane: (a) => {
      // A DETACH, like the desktop: the tmux session outlives the leaf.
      this.disconnect(str(a.paneId));
      this.diffStop(str(a.paneId));
      return this.layoutOp(str(a.workspaceId), (l) => closeLeaf(l, str(a.paneId)).node);
    },
    workspace_set_split_ratio: (a) =>
      this.layoutOp(str(a.workspaceId), (l) => setRatio(l, str(a.splitId), num(a.ratio, 0.5))),
    workspace_distribute_evenly: (a) => this.layoutOp(str(a.workspaceId), resetRatios),
    workspace_swap_panes: (a) =>
      this.layoutOp(str(a.workspaceId), (l) => swapLeaves(l, str(a.paneAId), str(a.paneBId))),
    workspace_reset_layout: (a) => this.mutate(str(a.workspaceId), () => ({ layout: makeLeaf(newPaneId()) })),
    pane_set_title: (a) =>
      this.layoutOp(str(a.workspaceId), (l) => patchLeaf(l, str(a.paneId), { title: a.title === null ? null : str(a.title) })),
    pane_set_annotation: (a) =>
      this.layoutOp(str(a.workspaceId), (l) =>
        patchLeaf(l, str(a.paneId), { annotation: a.annotation === null ? null : str(a.annotation) }),
      ),

    // ── Phase 117 (F3): pane fields the desktop keeps on the layout leaf ──
    pane_set_identity: async (a) => {
      const color = a.color === null || a.color === undefined ? null : str(a.color);
      const emoji = a.emoji === null || a.emoji === undefined ? null : str(a.emoji);
      checkIdentity(color, emoji);
      await this.layoutOp(str(a.workspaceId), (l) => patchLeaf(l, str(a.paneId), { color, emoji }));
      return { pane_id: str(a.paneId), color, emoji };
    },
    pane_set_smart_bidi: async (a) => {
      const on = a.enabled === true;
      const f = await this.layoutOp(str(a.workspaceId), (l) => patchLeaf(l, str(a.paneId), { smart_bidi: on }));
      const sid = this.paneSid.get(str(a.paneId));
      if (sid) this.pty.setBidi(sid, on);
      return f;
    },
    pane_set_claude_running: (a) =>
      this.layoutOp(str(a.workspaceId), (l) => patchLeaf(l, str(a.paneId), { claude_running: a.running === true })),
    // The resume picker: the daemon reads ~/.claude/projects on its own box.
    pane_list_claude_sessions: (a) => {
      const q = new URLSearchParams({ limit: String(num(a.limit, 30)) });
      if (str(a.projectPath)) q.set("project_path", str(a.projectPath));
      return api("GET", `/api/v2/claude/sessions?${q}`);
    },
    // The Context Rail: the daemon's per-session records (term/context.go);
    // `context:changed` arrives on the events socket like any other event.
    session_context_list: (a) => api("GET", `/api/v2/context/sessions?ws_id=${encodeURIComponent(str(a.wsId))}`),
    // The desktop keeps this for the SSH ticket-cwd lookup; nothing here reads it.
    pane_set_active: async () => null,
    sessions_kill_by_name: async (a) => {
      const name = str(a.name);
      const holder = [...this.paneSession].find(([, n]) => n === name)?.[0];
      if (holder) return this.kill(holder);
      try {
        await api("DELETE", `/api/v2/term/sessions/${encodeURIComponent(name)}`);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return { result: "already_gone", backend: "tmux", session: name };
        throw e;
      }
      return { result: "killed", backend: "tmux", session: name };
    },
    // One box: a pane's own connection is the daemon's.
    pane_probe_tmux_sessions: (a) => this.handlers.pane_list_tmux_sessions({ workspaceId: a.workspaceId, projectPath: null }),

    // ── the Diff pane + worktrees (Phase 118, F4) ──
    diff_pane_start: async (a) => {
      this.diffStart(str(a.paneId));
      return null;
    },
    diff_pane_stop: async (a) => {
      this.diffStop(str(a.paneId));
      return null;
    },
    diff_pane_set_source: async (a) => {
      const src = (a.source ?? {}) as DiffSource;
      if (src.kind === "ref") {
        const ref = (src.git_ref ?? "").trim();
        if (!ref || ref.startsWith("-") || /[\u0000-\u001f\u007f]/.test(ref)) throw new Error("invalid git ref");
      }
      await this.diffPatch(str(a.paneId), { diff_source: src });
      this.diffStart(str(a.paneId)); // a restart: the next snapshot always emits
      return null;
    },
    diff_pane_set_cwd: async (a) => {
      const cwd = typeof a.cwd === "string" && a.cwd.trim() ? a.cwd.trim() : null;
      await this.diffPatch(str(a.paneId), { diff_cwd: cwd });
      this.diffStart(str(a.paneId));
      return null;
    },
    diff_pane_refresh: async (a) => {
      const ev = await this.diffFetch(str(a.paneId));
      if (!ev) throw new Error(`no Diff pane with id ${str(a.paneId)}`);
      this.bus.emit("diff-pane-updated", ev); // once, whatever the hash
      return null;
    },
    diff_pane_worktrees: async (a) => {
      const ctx = this.diffContext(str(a.paneId));
      if (!ctx) throw new Error(`no Diff pane with id ${str(a.paneId)}`);
      if (!ctx.cwd.trim()) throw new Error("this workspace has no project directory");
      const r = await this.gitWorktrees(ctx.cwd);
      if (!r.ok) throw new Error(r.error || "git failed");
      return r.worktrees;
    },
    workspace_create_project_worktree: async (a) => {
      const w = this.getRow(str(a.workspaceId));
      if (!w.cwd) throw new Error("this workspace has no project directory");
      return api("POST", "/api/v2/git/worktree-add", {
        cwd: w.cwd,
        branch: str(a.branchName),
        base: str(a.baseBranch),
        target: strOrNull(str(a.targetPath).trim()) ?? "",
      });
    },
    // lib.rs workspace_open_worktree: a child row of the project folder, at
    // the worktree's path — or the existing one, made active.
    workspace_open_worktree: async (a) => {
      const path = str(a.worktreePath).trim();
      if (!path) throw new Error("worktree path is required");
      const all = this.all();
      const root = all.find((w) => w.id === str(a.rootWorkspaceId));
      if (!root) throw new Error("project folder workspace not found");
      const key = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
      const existing = all.find((w) => w.parent_id === root.id && w.cwd && key(w.cwd) === key(path));
      if (existing) {
        this.setActive(screenOrSelf(all, existing.id) ?? existing.id);
        return this.file();
      }
      const row = await this.createRow({
        name: str(a.name) || "worktree",
        cwd: path,
        parent_id: root.id,
        layout: makeLeaf(newPaneId()),
      });
      this.setActive(row.id);
      return this.file();
    },

    // panes / PTY
    pane_connect: (a) => this.connect(a),
    // "Open in a separate window": a browser window on the same tmux session
    // (a second tmux client). The pane hides until the window closes, then
    // `popout:closed` brings it back — App.tsx's desktop flow, unchanged.
    popout_pane: async (a) => {
      const sid = str(a.sessionId);
      const pane = [...this.paneSid].find(([, s]) => s === sid)?.[0];
      const name = pane ? this.paneSession.get(pane) : undefined;
      if (!name) throw new Error("this pane has no session to pop out");
      try {
        localStorage.setItem(POPOUT_KEY(sid), name);
      } catch {
        throw new Error("the browser's storage is unavailable");
      }
      const w = Math.min(screen.availWidth, Math.max(480, num(a.cols, 100) * 9 + 40));
      const h = Math.min(screen.availHeight, Math.max(320, num(a.rows, 30) * 19 + 60));
      const win = window.open(`/?popout=${encodeURIComponent(sid)}`, `ymux-popout-${sid}`, `popup=yes,width=${w},height=${h}`);
      if (!win) throw new Error("the browser blocked the popup window — allow popups for this site");
      // This window lets go of the session while the popout holds it: two
      // tmux clients of different sizes would frame this one with tmux's dot
      // fill. close() is a detach (no pty:exit); the sid stays mapped.
      this.pty.close(sid);
      const timer = window.setInterval(() => {
        if (!win.closed) return;
        window.clearInterval(timer);
        try {
          localStorage.removeItem(POPOUT_KEY(sid));
        } catch {
          /* gone with the storage */
        }
        // Re-attach under the same sid, then let App re-arm the pane.
        void this.pty
          .open(name, num(a.cols, 80), num(a.rows, 24), sid)
          .catch(() => this.bus.emit("pty:exit", { session_id: sid, reason: "session ended" }))
          .finally(() => this.bus.emit("popout:closed", sid));
      }, 1000);
      return null;
    },
    pane_disconnect: async (a) => {
      this.disconnect(str(a.paneId));
      return null;
    },
    pty_write: async (a) => {
      this.pty.write(str(a.sessionId), str(a.data));
      return null;
    },
    pty_resize: async (a) => {
      this.pty.resize(str(a.sessionId), num(a.cols, 0), num(a.rows, 0));
      return null;
    },
    pane_kill_session: (a) => this.kill(str(a.paneId)),
    pane_persistence_list: async () => {
      const out: Record<string, string> = {};
      for (const pid of this.paneSid.keys()) {
        const name = this.paneSession.get(pid);
        if (name) out[pid] = name;
      }
      return out;
    },
    pane_list_tmux_sessions: async (a) => {
      // lib.rs annotate_scope_with, minus the owners file: a session is this
      // workspace's when one of its panes holds it (or the row IS that
      // session), and in its folder when its path is under projectPath. The
      // picker's "This folder" view is `owned || in_cwd`.
      const wsId = str(a.workspaceId);
      const root = str(a.projectPath).replace(/\/+$/, "");
      const row = this.ws.find((w) => w.id === wsId);
      const mine = new Set<string>();
      if (row) {
        for (const pid of leafIds(row.layout ?? null)) {
          const n = this.paneSession.get(pid);
          if (n) mine.add(n);
        }
        const ts = row.meta?.tmux_session;
        if (typeof ts === "string") mine.add(ts);
      }
      const within = (p: string) => !!root && !!p && (p === root || p.startsWith(root + "/"));
      return (await this.sessions()).map((s) => ({
        name: s.name,
        created: s.created,
        attached: s.attached > 0,
        windows: s.windows,
        last_attached: 0,
        exited: false,
        label: s.label,
        claude_title: s.claude_title,
        auto_name: s.auto_name,
        claude_session_id: s.claude_session_id,
        origin: s.origin,
        cwd: s.path,
        owned: mine.has(s.name),
        in_cwd: within(s.path),
      }));
    },
    pane_target_session_state: async (a) => {
      const name = str(a.sessionName) || str(a.fallbackName) || this.paneSession.get(str(a.paneId)) || "";
      const s = (await this.sessions()).find((x) => x.name === name);
      return { name, exists: !!s, attached: (s?.attached ?? 0) > 0, reachable: true };
    },
    tmux_rename_session: async (a) => {
      await api("POST", `/api/v2/term/sessions/${encodeURIComponent(str(a.oldName))}/rename`, { new_name: str(a.newName) });
      for (const [pid, n] of this.paneSession) if (n === str(a.oldName)) this.paneSession.set(pid, str(a.newName));
      return null;
    },

    // hydration (from the events socket's hello) + the ymux surfaces
    pane_agent_states: async () => this.hello?.pane_agent_states ?? {},
    pane_briefs: async () => this.hello?.pane_briefs ?? {},
    feed_list: async () => this.hello?.feed ?? [],
    feed_decide: (a) =>
      api("POST", `/api/v2/feed/${encodeURIComponent(str(a.requestId))}/decide`, { decision: str(a.decision) }),
    notifications_list: async () => this.hello?.notifications ?? [],
    notifications_clear: () => api("DELETE", "/api/v2/notifications"),
    notes_load: async () => ({ version: 1, notes: await api<unknown[]>("GET", "/api/v2/notes") }),
    notes_add: (a) => api("POST", "/api/v2/notes", { text: str(a.text), tag: str(a.tag), pane_id: str(a.paneId) }),
    notes_update: (a) => {
      const body: Record<string, unknown> = {};
      if (typeof a.text === "string") body.text = a.text;
      if (typeof a.status === "string") body.status = a.status;
      if (typeof a.tag === "string") body.tag = a.tag;
      return api("PATCH", `/api/v2/notes/${encodeURIComponent(str(a.id))}`, body);
    },
    notes_delete: (a) => api("DELETE", `/api/v2/notes/${encodeURIComponent(str(a.id))}`),

    // Monitor (Phase 110): the desktop curls these same daemon paths over SSH;
    // here they are same-origin. The body is handed back as text, like Rust.
    insights_fetch: (a) => this.insights("GET", str(a.path)),
    insights_docker_action: (a) =>
      this.insights("POST", `/docker/${encodeURIComponent(str(a.containerId))}/action`, { cmd: str(a.action) }),
    insights_hygiene_kill: (a) => this.insights("POST", "/hygiene/kill", { pids: Array.isArray(a.pids) ? a.pids : [] }),

    // File Manager, remote side (Phase 110, web/files.ts).
    file_home_remote: () => this.files.home(),
    file_list_remote: (a) => this.files.list(str(a.path)),
    file_read_remote: (a) => this.files.read(str(a.path)),
    file_write_remote: async (a) => {
      await this.files.write(str(a.path), str(a.text));
      return null;
    },
    file_create_remote: async (a) => {
      await this.files.write(str(a.path), "");
      return null;
    },
    file_delete_remote: async (a) => {
      // Recursive, like the desktop's SFTP delete (the UI confirms first).
      await this.files.removeTree(str(a.path));
      return null;
    },
    // ── Phase 116 (F2): the rest of the File Manager ──
    file_mkdir_remote: async (a) => {
      await this.files.mkdir(str(a.path));
      return null;
    },
    file_rename_remote: async (a) => {
      await this.files.rename(str(a.oldPath), str(a.newPath));
      return null;
    },
    file_copy_remote: async (a) => {
      await this.files.copy(str(a.src), str(a.dest));
      return null;
    },
    file_manager_zip_remote: (a) => this.archive(a, "zip"),
    file_manager_targz_remote: (a) => this.archive(a, "targz"),
    file_manager_unzip_remote: (a) => this.files.unzip(str(a.zipPath)),
    file_manager_unzip_remote_check: (a) => this.files.exists(str(a.zipPath).replace(/\.zip$/i, "")),
    // The desktop opens a temp copy in the OS app; a browser downloads it,
    // and the browser decides whether to show it.
    file_open_remote: async (a) => {
      const p = str(a.remotePath);
      await this.files.download(p, p.split("/").pop() ?? "file");
      return "browser download";
    },
    // From the user's computer: localPath is a webfile token (pickPaths /
    // a drop), never a real path.
    file_upload: async (a) => {
      const f = fileFor(str(a.localPath));
      await this.files.write(str(a.remotePath), f);
      return f.size;
    },
    // A file dropped on a terminal: ~/ymux-drops/<name>, the desktop's spot;
    // the returned path is what gets typed into the pane.
    pane_upload_dropped: async (a) => {
      const f = fileFor(str(a.localPath));
      const name = (str(a.fileName) || f.name).split(/[\\/]/).pop() ?? "";
      if (!name || name === "." || name === "..") throw new Error("invalid file name");
      const home = await this.files.home();
      const dest = `${home === "/" ? "" : home}/ymux-drops/${name}`;
      await this.files.write(dest, f); // the daemon creates ymux-drops/
      return dest;
    },
    fm_transfer_cancel: async () => null, // uploads here are single requests, not tracked transfers
    file_large_threshold: async () => LARGE_FILE_BYTES,
    web_download: async (a) => {
      await this.files.download(str(a.remotePath), str(a.name));
      return null;
    },
  };

  private archive(a: Args, format: "zip" | "targz"): Promise<string> {
    const names = Array.isArray(a.paths) ? a.paths.filter((x): x is string => typeof x === "string") : [];
    return this.files.archive(str(a.cwd), names, str(a.outputName), format);
  }

  // ── the Diff pane (Phase 118, F4) ────────────────────────────────────
  // The desktop runs a poller per pane in Rust and emits diff-pane-updated;
  // here the poll runs in the browser against POST /api/v2/git/diff (one
  // stateless snapshot) and the same event goes onto the local bus, by the
  // desktop's rules: emit when the bundle's hash changes; an error once until
  // it changes, then the next success always emits.

  /** lib.rs lookup_pane_context: the leaf's diff_cwd ?? the row's cwd. */
  private diffContext(paneId: string): { cwd: string; source: DiffSource } | null {
    for (const w of this.all()) {
      const leaf = findLeaf(w.layout ?? null, paneId);
      if (!leaf) continue;
      return { cwd: leaf.diff_cwd ?? w.cwd ?? "", source: leaf.diff_source ?? { kind: "working" } };
    }
    return null;
  }

  private async diffFetch(paneId: string): Promise<DiffEvent | null> {
    const ctx = this.diffContext(paneId);
    if (!ctx) return null;
    try {
      const b = await api<Omit<DiffEvent, "pane_id">>("POST", "/api/v2/git/diff", ctx);
      return { pane_id: paneId, ...b };
    } catch (e) {
      return { pane_id: paneId, diff_text: "", files: [], error: String(e instanceof Error ? e.message : e), cwd: ctx.cwd, branch: null, truncated: false };
    }
  }

  private diffStart(paneId: string): void {
    this.diffStop(paneId);
    const st = { timer: 0, hash: null as string | null, err: null as string | null, stopped: false };
    this.diffWatch.set(paneId, st);
    const tick = async () => {
      if (st.stopped) return;
      const ev = await this.diffFetch(paneId);
      if (st.stopped) return;
      if (!ev) {
        this.diffStop(paneId); // the pane left every layout
        return;
      }
      if (ev.error) {
        if (ev.error !== st.err) this.bus.emit("diff-pane-updated", ev);
        st.err = ev.error;
        st.hash = null;
      } else {
        st.err = null;
        const h = JSON.stringify([ev.diff_text, ev.files, ev.branch]);
        if (h !== st.hash) this.bus.emit("diff-pane-updated", ev);
        st.hash = h;
      }
      // A browser polls over the network: 2 s rather than the desktop's 1 s.
      if (!st.stopped) st.timer = window.setTimeout(() => void tick(), 2000);
    };
    void tick();
  }

  private diffStop(paneId: string): void {
    const st = this.diffWatch.get(paneId);
    if (!st) return;
    st.stopped = true;
    window.clearTimeout(st.timer);
    this.diffWatch.delete(paneId);
  }

  /** Patch a Diff leaf wherever it lives; the desktop's error when nowhere. */
  private async diffPatch(paneId: string, patch: Record<string, unknown>): Promise<void> {
    const w = this.all().find((x) => findLeaf(x.layout ?? null, paneId));
    if (!w) throw new Error(`no Diff pane with id ${paneId}`);
    await this.layoutOp(w.id, (l) => patchLeaf(l, paneId, patch));
  }

  /** A daemon insights path (same allow-list shape as Rust's safe_api_path). */
  private async insights(method: string, path: string, body?: unknown): Promise<string> {
    if (!/^\/[A-Za-z0-9/_\-?=&.,]*$/.test(path)) throw new Error("invalid insights path");
    const r = await fetch(path, {
      method,
      headers: { Authorization: `Bearer ${getToken()}`, ...(body ? { "Content-Type": "application/json" } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    if (r.status === 401 || r.status === 403) throw new Error("this device is not allowed to read server insights");
    if (!r.ok) throw new Error(`insights daemon returned HTTP ${r.status}`);
    return r.text();
  }
}
