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
import { WEB_DEFAULT_SETTINGS, withDefaults } from "./web/defaults";
import { EventBus, EventsSocket, type Hello } from "./web/events";
import { splitArgs } from "./web/argv";
import { FilesBridge, LARGE_FILE_BYTES } from "./web/files";
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
  created_at: string;
  updated_at: string;
}

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
type Handler = (a: Args) => Promise<unknown>;

const ACTIVE_KEY = "ymux.web.activeWorkspace";
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
  readonly caps: ReadonlySet<Capability> = new Set<Capability>();
  readonly bus = new EventBus();
  private pty = new PtySessions(this.bus);
  private files = new FilesBridge();
  private events: EventsSocket;
  private hello: Hello | null = null;
  private ws: WebWS[] = [];
  private settingsVersion = 0;
  private settingsCache: Settings = structuredClone(WEB_DEFAULT_SETTINGS);
  /** leaf pane id → tmux session name (hello `panes` + our creates). */
  private paneSession = new Map<string, string>();
  /** leaf pane id → live attach sid, and back. */
  private paneSid = new Map<string, string>();
  private warned = new Set<string>();
  version = "";

  readonly host: HostShell = {
    windowLabel: () => "main",
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
    pickPaths: (_opts: PickPathsOptions) => Promise.resolve(null),
    savePath: () => Promise.resolve(null),
    onDragDrop: (_cb: EventCallback<DragDropPayload>) => Promise.resolve(() => {}),
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
    const v = await fetch("/api/version").then((r) => r.json() as Promise<{ version?: string }>);
    this.version = v.version ?? "";
    await this.loadSettings();
    await this.reloadWorkspaces();
    await this.events.start();
    await this.seedRestoreHints();
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
    this.ws = await api<WebWS[]>("GET", "/api/v2/web/workspaces");
  }

  private activeId(): string | null {
    let id: string | null = null;
    try {
      id = localStorage.getItem(ACTIVE_KEY);
    } catch {
      /* no storage */
    }
    if (id && this.ws.some((w) => w.id === id)) return id;
    return this.ws[0]?.id ?? null;
  }

  private toWorkspace(w: WebWS, i: number): Workspace {
    const updated = Date.parse(w.updated_at) || 0;
    return {
      id: w.id,
      name: w.name,
      color: null,
      emoji: null,
      cwd: null,
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
      group_id: null,
      sort_order: i,
      parent_id: null,
      is_project_root: w.is_project_root ?? false,
      is_collapsed: false,
      tabs_mode: w.tabs_mode ?? false,
      tmux_session: null,
      intent: w.intent ?? null,
      known_sessions: [],
      is_folder: false,
    };
  }

  private file(): WorkspacesFile {
    return {
      version: 1,
      active_workspace_id: this.activeId(),
      workspaces: this.ws.map((w, i) => this.toWorkspace(w, i)),
      groups: [],
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
          cmd,
        });
      } catch (e) {
        // The name is taken by an unrelated session: let the daemon mint one.
        if (!(e instanceof ApiError && e.status === 409)) throw e;
        created = await api<{ name: string }>("POST", "/api/v2/term/sessions", {
          cwd: str(a.cwdOverride),
          workspace_id: wsId,
          pane_id: paneId,
          cmd,
        });
      }
      name = created.name;
    }
    this.paneSession.set(paneId, name);
    const sid = await this.pty.open(name, num(a.cols, 80), num(a.rows, 24));
    this.paneSid.set(paneId, sid);
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
      try {
        localStorage.setItem(ACTIVE_KEY, str(a.workspaceId));
      } catch {
        /* per-tab only */
      }
      return this.file();
    },
    workspace_create: async (a) => {
      const input = (a.input ?? {}) as { name?: string };
      const created = await api<WebWS>("POST", "/api/v2/web/workspaces", { name: input.name || "workspace" });
      this.ws.push(created);
      const f = await this.mutate(created.id, () => ({ layout: makeLeaf(newPaneId()) }));
      try {
        localStorage.setItem(ACTIVE_KEY, created.id);
      } catch {
        /* per-tab only */
      }
      return { ...f, active_workspace_id: created.id };
    },
    workspace_rename: (a) => this.mutate(str(a.workspaceId), () => ({ name: str(a.name) })),
    workspace_update: (a) =>
      this.mutate(str(a.workspaceId), () => (typeof a.name === "string" && a.name ? { name: a.name } : {})),
    workspace_delete: async (a) => {
      const id = str(a.workspaceId);
      for (const pid of leafIds(this.find(id).layout ?? null)) this.disconnect(pid);
      await api("DELETE", `/api/v2/web/workspaces/${encodeURIComponent(id)}`);
      this.ws = this.ws.filter((w) => w.id !== id);
      return this.file();
    },
    workspace_reorder: async () => this.file(),
    workspace_set_collapsed: async () => this.file(),
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

    // panes / PTY
    pane_connect: (a) => this.connect(a),
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
    pane_list_tmux_sessions: async () =>
      (await this.sessions()).map((s) => ({
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
        owned: true,
        in_cwd: false,
      })),
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
      await this.files.remove(str(a.path));
      return null;
    },
    file_large_threshold: async () => LARGE_FILE_BYTES,
    web_download: async (a) => {
      await this.files.download(str(a.remotePath), str(a.name));
      return null;
    },
  };

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
