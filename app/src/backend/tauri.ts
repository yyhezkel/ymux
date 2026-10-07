// The desktop backend: a straight pass-through to Tauri IPC. Behaviour is
// exactly what the call sites did before Phase 106 — `call` is `invoke`,
// `on` is `listen`, `emit` is `emit`, and `host` is the window / dialog /
// opener calls the components used to make themselves (Phase 107).

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { ALL_CAPABILITIES } from "./types";
import type {
  Backend,
  Capability,
  DragDropPayload,
  EventCallback,
  HostShell,
  PickPathsOptions,
  UnlistenFn,
} from "./types";

const tauriHost: HostShell = {
  windowLabel() {
    try {
      return getCurrentWindow().label;
    } catch {
      return ""; // window metadata not ready — callers treat it as `main`
    }
  },
  setTitle: (title) => getCurrentWindow().setTitle(title),
  setZoom: (factor) => getCurrentWebview().setZoom(factor),
  closeWindow: () => getCurrentWindow().close(),
  appVersion: () => getVersion(),
  openUrl: (url) => openUrl(url),
  revealInDir: (path) => revealItemInDir(path),
  pickPaths: (opts: PickPathsOptions) => open(opts),
  savePath: (opts) => save(opts),
  onDragDrop: (cb: EventCallback<DragDropPayload>) =>
    getCurrentWebview().onDragDropEvent((e) =>
      cb({ payload: e.payload as DragDropPayload }),
    ),
};

export class TauriBackend implements Backend {
  readonly kind = "tauri" as const;
  readonly host = tauriHost;
  readonly caps: ReadonlySet<Capability> = new Set(ALL_CAPABILITIES);

  can(cap: Capability): boolean {
    return this.caps.has(cap);
  }

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
