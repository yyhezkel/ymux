// The one `Backend` the app talks to. Chosen synchronously at module load so
// any module may call it from its first line; Phase C5 adds the browser arm
// (`"__TAURI_INTERNALS__" in window` → Tauri, else Web).

import { TauriBackend } from "./tauri";
import type { Backend } from "./types";

export type { Backend, BackendEvent, EventCallback, UnlistenFn } from "./types";

export const backend: Backend = new TauriBackend();
