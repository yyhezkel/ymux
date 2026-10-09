// The one `Backend` the app talks to. Chosen synchronously at module load so
// any module may call it from its first line: inside the desktop shell
// (`__TAURI_INTERNALS__` is injected before any script runs) it is the Tauri
// backend; in a plain browser tab served by the daemon it is the WebBackend
// (Phase 109), which index.tsx must `initBackend()` before the first render.

import { TauriBackend } from "./tauri";
import type { Backend } from "./types";
import { WebBackend } from "./web";
import { ApiError } from "./web/api";

export { ALL_CAPABILITIES } from "./types";
export type {
  Backend,
  Capability,
  BackendEvent,
  DragDropPayload,
  EventCallback,
  HostShell,
  PickPathsOptions,
  UnlistenFn,
} from "./types";

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const web: WebBackend | null = inTauri ? null : new WebBackend();

/** A tab served by the daemon (not the desktop shell). */
export const isBrowserHost = !inTauri;

export const backend: Backend = web ?? new TauriBackend();

/**
 * What index.tsx renders. `ready` → <App>; `login` → the pairing screen;
 * `no-shell` → signed in, but the device lacks `shell:attach`.
 */
export type BootState = "ready" | "login" | "no-shell";

export async function initBackend(): Promise<BootState> {
  if (!web) return "ready";
  try {
    await web.init();
    return "ready";
  } catch (e) {
    if (e instanceof ApiError && e.status === 403) return "no-shell";
    if (e instanceof ApiError && e.status === 401) return "login";
    // The daemon is up but something else failed (events socket refused…):
    // the login screen is still the one place the user can act from.
    console.error("[web] boot failed", e);
    return "login";
  }
}
