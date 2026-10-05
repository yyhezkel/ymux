// The seam between the UI and whatever hosts it (WEB-DESIGN §5, Phase C).
//
// Every call the frontend makes to its host goes through `Backend`: the
// desktop answers with Tauri IPC (`TauriBackend`), a browser will answer with
// the daemon's HTTP/WS API (`WebBackend`, Phase C5). Nothing outside
// `src/backend/` imports `@tauri-apps/api/core` or `@tauri-apps/api/event` —
// `backendSeam.test.ts` fails the build when something does.
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
}
