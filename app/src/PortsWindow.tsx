import { createMemo, createEffect, createSignal, For, Show, onCleanup } from "solid-js";
import { backend } from "./backend";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ForwardRow, Workspace } from "./types";
import { t } from "./i18n";
import { IconClose, IconCheck, IconGlobe } from "./icons";
import { createLogger } from "./logger";

const log = createLogger("PORTS");

// Phase 46: detect-only + click-to-forward. The watcher reports
// LISTEN ports → row appears with [Forward]. User clicks → backend
// opens the tunnel (with a TCP sanity probe so dead binds don't
// reach the browser) → row flips to [Open] [Stop]. Stop tears down
// the tunnel; the row reverts to detected-only if the port is still
// listening on the remote, or disappears when port.closed fires.

type DetectedPort = {
  workspace_id: string;
  remote_port: number;
  addr: string;
  family: string;
};

interface Props {
  open: boolean;
  /** The currently active workspace — drives scope, name, toggle, color. */
  activeWorkspace: Workspace | null;
  /** Ports the remote watcher has reported (across all workspaces; filtered here). */
  detectedPorts: DetectedPort[];
  /** Currently-open forwards (across all workspaces; filtered here). */
  forwards: ForwardRow[];
  onClose: () => void;
  /** Stop a forwarded tunnel. Detection stays until the remote stops listening. */
  onStop: (workspaceId: string, remotePort: number) => void;
  /** Open a tunnel for a detected port; returns the assigned local port. */
  onStart: (workspaceId: string, remotePort: number) => Promise<number>;
  /** Flip the watcher on/off. No-op for Local workspaces. */
  onToggleAutoForward: (workspaceId: string, enabled: boolean) => void;
}

type RowState =
  | { kind: "detected"; addr: string; family: string }
  | { kind: "forwarded"; local_port: number; addr: string };

type Row = { remote_port: number } & RowState;

export function PortsWindow(p: Props) {
  // Every detected port reaches this panel through the local RPC endpoint.
  // When that endpoint fails to bind, nothing ever arrives and the panel
  // used to sit on "Waiting for remote ports…" forever — indistinguishable
  // from a remote that simply isn't listening. SSH panes keep working
  // meanwhile (they use russh channels directly), which is what made this
  // read as "the ports feature is broken" instead of "the RPC socket is
  // down". Ask doctor once per open and say so.
  const [rpcDown, setRpcDown] = createSignal<string | null>(null);
  createEffect(() => {
    if (!p.open) return;
    void (async () => {
      try {
        const snap = await backend.call<{ rpc_server?: { bind_error?: string | null } }>("doctor");
        setRpcDown(snap.rpc_server?.bind_error ?? null);
      } catch (e) {
        // Diagnostics are best-effort — never let them break the panel.
        log.warn("doctor probe failed", e);
      }
    })();
  });

  // Esc to close.
  createEffect(() => {
    if (!p.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        p.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const ws = () => p.activeWorkspace;
  const isLocal = () => !!ws() && ws()!.connection == null;
  const enabled = () => (ws()?.auto_port_forward ?? false) && !isLocal();

  // Merged rows for the active workspace, sorted by remote_port.
  const rows = createMemo<Row[]>(() => {
    const id = ws()?.id;
    if (!id) return [];
    const byPort = new Map<number, Row>();
    for (const d of p.detectedPorts) {
      if (d.workspace_id !== id) continue;
      byPort.set(d.remote_port, {
        remote_port: d.remote_port,
        kind: "detected",
        addr: d.addr,
        family: d.family,
      });
    }
    // Forwards overlay — even if the remote no longer reports the
    // port (rare race), we still show it while the tunnel is open.
    for (const f of p.forwards) {
      if (f.workspace_id !== id) continue;
      byPort.set(f.remote_port, {
        remote_port: f.remote_port,
        kind: "forwarded",
        local_port: f.local_port,
        addr: f.remote_addr,
      });
    }
    return [...byPort.values()].sort((a, b) => a.remote_port - b.remote_port);
  });

  const toggle = () => {
    const w = ws();
    if (!w || isLocal()) return;
    p.onToggleAutoForward(w.id, !enabled());
  };

  // 127.0.0.1 explicitly to dodge dual-stack `localhost` → ::1 — the
  // russh forward binds IPv4-only, so a browser that resolves to IPv6
  // first would otherwise hit a dead port even with a healthy tunnel.
  const browserUrl = (localPort: number) => `http://127.0.0.1:${localPort}`;

  const openInBrowser = (localPort: number) => {
    void openUrl(browserUrl(localPort)).catch((e) => log.warn("openUrl failed", e));
  };

  const startAndOpen = async (remotePort: number) => {
    const w = ws();
    if (!w) return;
    try {
      const local = await p.onStart(w.id, remotePort);
      openInBrowser(local);
    } catch (e) {
      log.error("forward_port_start failed", e);
      const msg = typeof e === "string" ? e : String(e);
      window.alert(t("ports.window.error.unreachable", { local: remotePort, msg }));
    }
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal ports-window" onClick={(e) => e.stopPropagation()}>
          <div class="settings-head">
            <h3>
              <Show when={ws()} fallback={t("ports.panel.title")}>
                {t("ports.window.title", { workspace: ws()!.name })}
              </Show>
            </h3>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>
              <IconClose />
            </button>
          </div>

          <Show
            when={ws()}
            fallback={
              <div class="ports-window-body">
                <p class="ports-panel-empty">{t("ports.window.empty.noWorkspace")}</p>
              </div>
            }
          >
            <button
              type="button"
              class="ports-toggle-row"
              classList={{ on: enabled(), off: !enabled() }}
              disabled={isLocal()}
              onClick={toggle}
              style={enabled() && ws()!.color ? `--ws-color: ${ws()!.color}` : undefined}
              title={
                enabled()
                  ? t("ports.window.toggle.hint.on")
                  : t("ports.window.toggle.hint.off")
              }
            >
              <span class="ports-toggle-state">
                {enabled() ? (
                  <><IconCheck size={14} /> {t("ports.window.toggle.active")}</>
                ) : (
                  t("ports.window.toggle.inactive")
                )}
              </span>
              <span class="ports-toggle-label">{t("ports.window.toggle.label")}</span>
            </button>

            <div class="ports-window-body">
              <Show
                when={rows().length > 0}
                fallback={
                  <p class="ports-panel-empty">
                    {rpcDown()
                      ? t("ports.window.empty.rpcDown", { error: rpcDown()! })
                      : enabled()
                        ? t("ports.window.empty.detectionOn")
                        : t("ports.window.empty.toggleOff")}
                  </p>
                }
              >
                <For each={rows()}>
                  {(r) => (
                    <Show
                      when={r.kind === "forwarded"}
                      fallback={
                        <div
                          class="ports-row ports-row-detected"
                          title={t("ports.window.row.forwardAction")}
                          onClick={() => void startAndOpen(r.remote_port)}
                        >
                          <span class="ports-row-icon"><IconGlobe size={14} /></span>
                          <span class="ports-row-label">
                            :{r.remote_port}
                            <span class="ports-row-sub">{(r as { addr: string }).addr}</span>
                          </span>
                          <span class="ports-row-actions">
                            <button
                              class="primary"
                              onClick={(e) => {
                                e.stopPropagation();
                                void startAndOpen(r.remote_port);
                              }}
                            >
                              {t("ports.window.row.forwardAction")}
                            </button>
                          </span>
                        </div>
                      }
                    >
                      <div
                        class="ports-row ports-row-forwarded"
                        title={browserUrl((r as { local_port: number }).local_port)}
                      >
                        <span class="ports-row-icon"><IconGlobe size={14} /></span>
                        <span
                          class="ports-row-label"
                          onClick={() => openInBrowser((r as { local_port: number }).local_port)}
                        >
                          :{r.remote_port} → localhost:
                          {(r as { local_port: number }).local_port}
                          <span class="ports-row-sub">
                            {t("ports.row.fromRemote", { remote: r.remote_port })}
                          </span>
                        </span>
                        <span class="ports-row-actions">
                          <button
                            onClick={() => openInBrowser((r as { local_port: number }).local_port)}
                          >
                            {t("ports.window.row.openAction")}
                          </button>
                          <button onClick={() => p.onStop(ws()!.id, r.remote_port)}>
                            {t("ports.window.row.stopAction")}
                          </button>
                        </span>
                      </div>
                    </Show>
                  )}
                </For>
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </Show>
  );
}
