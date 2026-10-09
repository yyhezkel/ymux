// The seam between the UI and whatever hosts it (WEB-DESIGN §5, Phase C).
//
// Every call the frontend makes to its host goes through `Backend`: the
// desktop answers with Tauri IPC (`TauriBackend`), a browser will answer with
// the daemon's HTTP/WS API (`WebBackend`, Phase C5). Nothing outside
// `src/backend/` imports anything from `@tauri-apps/*` — `backendSeam.test.ts`
// fails the build when something does.
//
// This file must stay free of imports from the rest of the app: `logger.ts`
// itself calls through the backend, so a logger import here is a cycle.

/** Same shape as Tauri's `UnlistenFn`, so call sites keep their types. */
export type UnlistenFn = () => void;

/** The part of a Tauri `Event<T>` the frontend reads. */
export interface BackendEvent<T> {
  payload: T;
}

export type EventCallback<T> = (event: BackendEvent<T>) => void;

/** An OS file drag over the window — Tauri's `DragDropEvent`, positions as reported. */
export type DragDropPayload =
  | { type: "enter" | "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

export interface PickPathsOptions {
  directory?: boolean;
  multiple?: boolean;
  defaultPath?: string;
}

/**
 * The window / OS affordances (Phase 107): everything the UI used to take from
 * `@tauri-apps/api/window|webview|app` and the dialog / opener plugins.
 */
export interface HostShell {
  /** This window's label (`main`, `popout-<sid>`, …); "" when unknown. */
  windowLabel(): string;
  setTitle(title: string): Promise<void>;
  setZoom(factor: number): Promise<void>;
  closeWindow(): Promise<void>;
  appVersion(): Promise<string>;
  openUrl(url: string): Promise<void>;
  revealInDir(path: string): Promise<void>;
  /** Native open dialog: a path, several paths, or null when cancelled. */
  pickPaths(opts: PickPathsOptions): Promise<string | string[] | null>;
  /** Native save dialog: the chosen path, or null when cancelled. */
  savePath(opts: { defaultPath?: string }): Promise<string | null>;
  onDragDrop(cb: EventCallback<DragDropPayload>): Promise<UnlistenFn>;
}

/**
 * What this host can do (Phase 107). The desktop can do everything; a browser
 * talking to the daemon cannot reach the local machine, SSH out, or host a
 * webview, so the UI hides those entry points instead of failing on them
 * (WEB-DESIGN §4.1). Gate with `backend.can(cap)`.
 */
export const ALL_CAPABILITIES = [
  "localPanes", // local + WSL workspaces, the local setup wizard
  "ssh", // SSH workspaces, connect-existing, provisioning, keys, reconnect
  "browserPane", // the in-app Browser (pane + workspace webview)
  "popout", // terminal / browser pop-out windows
  "fileManagerLocal", // the local column of the File Manager, native dialogs
  "diffPane",
  "worktrees",
  "tickets",
  "skills",
  "addons",
  "mobilePairingAdmin",
  "updater",
  "fonts",
  "stt", // the local STT endpoint (Web Speech works anywhere)
  "portForward",
] as const;

export type Capability = (typeof ALL_CAPABILITIES)[number];

export interface Backend {
  readonly kind: "tauri" | "web";
  /** A host command. Rejects with the host's error string, like `invoke`. */
  call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  /**
   * Subscribe to a host event. Async on purpose: App.tsx awaits each
   * registration so ordering against session restore stays what it was.
   */
  on<T>(event: string, cb: EventCallback<T>): Promise<UnlistenFn>;
  /** Broadcast an event to every window of this app (desktop popouts). */
  emit(event: string, payload?: unknown): Promise<void>;
  readonly host: HostShell;
  readonly caps: ReadonlySet<Capability>;
  can(cap: Capability): boolean;
}
