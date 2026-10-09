import { createSignal, onCleanup, onMount } from "solid-js";
import { backend } from "../backend";
import type { Workspace, WorkspacesFile } from "../types";
import type { Geometry } from "../floatingWindow";
import {
  BrowserChromeBody,
  createBrowserChrome,
  CHROME_PORTBAR_H,
  CHROME_TABS_H,
  type DetectedPort,
  type ForwardInfo,
} from "../BrowserChrome";
import { createLogger } from "../logger";

const log = createLogger("BROWSER");

// Phase 85.C: the workspace Browser popped out into its own OS window.
//
// Sibling of `PopoutTerminal.tsx` and built the same way: `index.tsx`
// bails to this BEFORE mounting `<App>`, so none of the workspace /
// settings bootstrap runs here. The workspace id comes from the window
// LABEL (`browser-popout-<ws>`), never from the URL — in a built app
// Tauri's asset protocol serves a blank page for any suffixed path.
//
// This window is a SEPARATE JS context, so it cannot read App's signals.
// It self-serves the two things the chrome needs — the detected-port list
// and the ability to open a forward — by invoking the same commands App
// does and subscribing to the same app-wide broadcasts, filtering by
// workspace id. That is exactly how PopoutTerminal filters `pty:data`.
//
// Chrome here is tabs + port bar only: the OS window supplies the title
// bar, the X, and the resize grips, so there is no ymux header, no bottom
// strip, and no side inset for resize handles.

/** Tab bar + port bar. No header and no resize gutter in an OS window. */
const POPOUT_CHROME_TOP = CHROME_TABS_H + CHROME_PORTBAR_H;

export function PopoutBrowser(props: { workspaceId: string }) {
  const [workspace, setWorkspace] = createSignal<Workspace | null>(null);
  const [detectedPorts, setDetectedPorts] = createSignal<DetectedPort[]>([]);
  const [forwards, setForwards] = createSignal<ForwardInfo[]>([]);
  // Window-relative slot rect. Tracked as a signal so the chrome's show
  // effect re-runs (and repositions the native child) on every resize —
  // there is no drag geometry to key off here.
  const [viewport, setViewport] = createSignal<{ w: number; h: number }>({
    w: window.innerWidth,
    h: window.innerHeight,
  });

  const slotRect = (): Geometry => {
    const v = viewport();
    return {
      x: 0,
      y: POPOUT_CHROME_TOP,
      w: Math.max(1, v.w),
      h: Math.max(1, v.h - POPOUT_CHROME_TOP),
    };
  };

  const ensurePorts = (wsId: string): void => {
    void backend.call("workspace_ensure_port_watcher", { workspaceId: wsId }).catch(
      (e: unknown) => log.warn("workspace_ensure_port_watcher failed", e),
    );
    void backend.call<DetectedPort[]>("list_detected_ports", { workspaceId: wsId })
      .then((snapshot) => setDetectedPorts(snapshot))
      .catch((e: unknown) => log.warn("list_detected_ports failed", e));
  };

  const chrome = createBrowserChrome({
    open: true,
    // No modal can cover this window — the ymux modals all live in
    // `main`, and hiding on their account would blank a window on
    // another monitor for no reason.
    anyModalOpen: () => false,
    get workspace() {
      return workspace();
    },
    get detectedPorts() {
      return detectedPorts();
    },
    get forwards() {
      return forwards();
    },
    onEnsurePorts: ensurePorts,
    onStartForward: (remotePort) =>
      backend.call<number>("forward_port_start", {
        workspaceId: props.workspaceId,
        remotePort,
      }),
    slotRect,
    windowLabel: backend.host.windowLabel(),
  });

  onMount(() => {
    const onResize = () =>
      setViewport({ w: window.innerWidth, h: window.innerHeight });
    window.addEventListener("resize", onResize);
    onCleanup(() => window.removeEventListener("resize", onResize));

    // The workspace row, for the window title and so the chrome has a
    // non-null `workspace` to key its persistence on.
    void backend.call<WorkspacesFile>("workspaces_load")
      .then((file) => {
        const ws = file.workspaces.find((w) => w.id === props.workspaceId);
        if (!ws) {
          log.error(`popout browser: workspace ${props.workspaceId} not found`);
          return;
        }
        setWorkspace(ws);
        void backend.host.setTitle(`${ws.name} — Browser`).catch(() => {});
      })
      .catch((e: unknown) => log.error("workspaces_load failed", e));

    ensurePorts(props.workspaceId);

    // App-wide broadcasts, filtered by workspace id — same pattern as
    // PopoutTerminal's `pty:data`. Both windows receive every event.
    const unlistens: (() => void)[] = [];
    void (async () => {
      unlistens.push(
        await backend.on<DetectedPort & { workspace_id: string }>(
          "port-detected",
          (e) => {
            if (e.payload.workspace_id !== props.workspaceId) return;
            setDetectedPorts((prev) => [
              ...prev.filter((d) => d.remote_port !== e.payload.remote_port),
              {
                remote_port: e.payload.remote_port,
                addr: e.payload.addr,
                family: e.payload.family,
              },
            ]);
          },
        ),
      );
      unlistens.push(
        await backend.on<{ workspace_id: string; remote_port: number }>(
          "port-undetected",
          (e) => {
            if (e.payload.workspace_id !== props.workspaceId) return;
            setDetectedPorts((prev) =>
              prev.filter((d) => d.remote_port !== e.payload.remote_port),
            );
          },
        ),
      );
      unlistens.push(
        await backend.on<{ workspace_id: string }>("port-detection-cleared", (e) => {
          if (e.payload.workspace_id !== props.workspaceId) return;
          setDetectedPorts([]);
        }),
      );
      unlistens.push(
        await backend.on<{
          workspace_id: string;
          remote_port: number;
          local_port: number;
        }>("port-forwarded", (e) => {
          if (e.payload.workspace_id !== props.workspaceId) return;
          setForwards((prev) => [
            ...prev.filter((f) => f.remote_port !== e.payload.remote_port),
            {
              remote_port: e.payload.remote_port,
              local_port: e.payload.local_port,
            },
          ]);
        }),
      );
      unlistens.push(
        await backend.on<{ workspace_id: string; remote_port: number }>(
          "port-forward-stopped",
          (e) => {
            if (e.payload.workspace_id !== props.workspaceId) return;
            setForwards((prev) =>
              prev.filter((f) => f.remote_port !== e.payload.remote_port),
            );
          },
        ),
      );
    })();
    onCleanup(() => {
      for (const un of unlistens) un();
    });
  });

  return (
    <div class="popout-browser">
      <BrowserChromeBody api={chrome} />
    </div>
  );
}
