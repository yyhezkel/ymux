// The desktop backend: a straight pass-through to Tauri IPC. Behaviour is
// exactly what the call sites did before Phase 106 — `call` is `invoke`,
// `on` is `listen`, `emit` is `emit`.

import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import type { Backend, EventCallback, UnlistenFn } from "./types";

export class TauriBackend implements Backend {
  readonly kind = "tauri" as const;

  call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return invoke<T>(cmd, args);
  }

  on<T>(event: string, cb: EventCallback<T>): Promise<UnlistenFn> {
    return listen<T>(event, cb);
  }

  emit(event: string, payload?: unknown): Promise<void> {
    return emit(event, payload);
  }
}
