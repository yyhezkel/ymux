import { createSignal, For, Show } from "solid-js";
import { IconClose, IconFolder, IconRefresh } from "./icons";
import { t } from "./i18n";
import { paneDragStore, startPaneDrag } from "./paneDrag";
import { TechText } from "./TechText";
import { AgentLight } from "./AgentLight";
import type { TrafficLight } from "./paneAgentState";
import { paneLabel, type PaneNode } from "./paneTitle";
import { effectiveIdentity, type Connection, type ForeignScope } from "./types";

// Phase 91: one entry of the sessions strip, resolved by App.tsx from the
// workspace's `known_sessions` (order) joined with the host's live list.
export type SessionEntry = {
  name: string;
  display: string;
  /** The pane holding (or about to hold) this session, if any. */
  paneId: string | null;
  /** Known to the workspace but absent from the host's live list. */
  gone: boolean;
  foreign: ForeignScope | null;
  attached: boolean;
  windows: number;
  claudeSessionId: string | null;
};

interface Props {
  entries: SessionEntry[];
  // Layout leaves bound to no session (a split-off shell, a diff pane) —
  // rendered as plain tabs after the sessions so nothing is unreachable.
  extraPanes: PaneNode[];
  activePaneId: string | null;
  connectedPaneIds: Set<string>;
  waitingPaneIds: Set<string>;
  notifiedPaneIds: Set<string>;
  panePulseEnabled: boolean;
  workspaceName?: string;
  workspaceColor?: string;
  workspaceEmoji?: string;
  workspaceConnection?: Connection;
  agentLights: Record<string, TrafficLight | null>;
  agentStateSince: Record<string, number | null>;
  agentNowMs: number;
  /** False when the host could not be asked (cold password-auth SSH, a
   *  failed list) — then nothing is painted as gone. */
  reachable: boolean;
  loading: boolean;
  error: string | null;
  onSelectSession: (name: string) => void;
  onKillSession: (name: string) => void;
  onNewSession: () => void;
  onRefresh: () => void;
  onSelectPane: (paneId: string) => void;
  onClosePane: (paneId: string) => void;
}

// Phase 91: the sessions strip — PaneTabs' sibling for sessions mode.
//
// Same markup and classes as PaneTabs so the two strips look identical;
// the difference is what a tab IS. Here a tab is a multiplexer session on
// the host, whether or not a pane holds it yet, and its order is the
// workspace's `known_sessions` (first seen first), not the layout tree —
// which is why session tabs carry no `data-pane-id` and no drag: a swap
// would move panes the strip is not ordered by.
//
// One piece of local state, the deliberate exception to PaneTabs' "owns
// no state": the two-step kill confirm (first × asks, second × kills,
// 3 s to change your mind). A kill is irreversible and the overview
// dialog uses the same two-step, so no new modal.
export function SessionTabs(p: Props) {
  const [pendingKill, setPendingKill] = createSignal<string | null>(null);
  let pendingTimer: number | null = null;

  const askKill = (name: string) => {
    if (pendingKill() === name) {
      if (pendingTimer) window.clearTimeout(pendingTimer);
      pendingTimer = null;
      setPendingKill(null);
      p.onKillSession(name);
      return;
    }
    setPendingKill(name);
    if (pendingTimer) window.clearTimeout(pendingTimer);
    pendingTimer = window.setTimeout(() => setPendingKill(null), 3000);
  };

  // Precedence mirrors PaneTabs: blocking outranks activity outranks
  // connected. A session with no pane yet is idle by definition.
  const dotState = (paneId: string | null): string => {
    if (!paneId) return "idle";
    if (p.waitingPaneIds.has(paneId)) return "waiting";
    if (p.panePulseEnabled && p.notifiedPaneIds.has(paneId)) return "activity";
    if (p.connectedPaneIds.has(paneId)) return "connected";
    return "idle";
  };

  const foreignTip = (f: ForeignScope): string => {
    const name = f.label ?? t("connect.tmuxPick.foreign.unknown");
    return (
      t(
        f.kind === "workspace"
          ? "connect.tmuxPick.foreign.tipWorkspace"
          : "connect.tmuxPick.foreign.tipFolder",
        { name },
      ) + (f.path ? `\n${f.path}` : "")
    );
  };

  const entryTitle = (e: SessionEntry): string => {
    if (e.gone) {
      return e.claudeSessionId
        ? t("sessiontabs.gone.tooltip", { name: e.name })
        : t("sessiontabs.gone.plainTooltip", { name: e.name });
    }
    return e.foreign ? `${e.name}\n${foreignTip(e.foreign)}` : e.name;
  };

  const paneTabLabel = (pane: PaneNode): string =>
    paneLabel(pane, {
      workspaceName: p.workspaceName,
      workspaceConnection: p.workspaceConnection,
    });

  return (
    <div class="pane-tabs">
      <div class="pane-tabs-list">
        <For each={p.entries}>
          {(e) => {
            const light = () => (e.paneId ? p.agentLights[e.paneId] ?? null : null);
            return (
              <div
                class="pane-tab"
                classList={{
                  "pane-tab-active": !!e.paneId && p.activePaneId === e.paneId,
                  "pane-tab-gone": e.gone,
                }}
                data-dot={dotState(e.paneId)}
                onClick={() => p.onSelectSession(e.name)}
                onAuxClick={(ev) => {
                  if (ev.button === 1) {
                    ev.preventDefault();
                    askKill(e.name);
                  }
                }}
                title={entryTitle(e)}
              >
                <AgentLight
                  light={light()}
                  waitingOnPermission={!!e.paneId && p.waitingPaneIds.has(e.paneId)}
                  stateSince={e.paneId ? p.agentStateSince[e.paneId] ?? null : null}
                  nowMs={p.agentNowMs}
                />
                <Show when={!light()}>
                  <span class={`pane-tab-dot pane-tab-dot-${dotState(e.paneId)}`} />
                </Show>
                <Show when={e.foreign}>
                  <span class="pane-tab-badge" aria-hidden="true">
                    <IconFolder size={11} />
                  </span>
                </Show>
                <span class="pane-tab-label" dir="auto">
                  <TechText text={e.display} />
                </span>
                <button
                  class="pane-tab-close pane-btn"
                  classList={{ "pane-tab-confirm": pendingKill() === e.name }}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    askKill(e.name);
                  }}
                  title={t("sessiontabs.kill.tooltip", { name: e.name })}
                  aria-label={t("sessiontabs.kill.tooltip", { name: e.name })}
                >
                  <Show when={pendingKill() === e.name} fallback={<IconClose size={10} />}>
                    {t("sessions.killConfirm")}
                  </Show>
                </button>
              </div>
            );
          }}
        </For>
        {/* Unbound panes: PaneTabs' row verbatim, drag included — these
            ARE ordered by the layout tree. */}
        <For each={p.extraPanes}>
          {(pane) => {
            const id = pane.pane_id;
            const ident = () =>
              effectiveIdentity(pane, {
                color: p.workspaceColor,
                emoji: p.workspaceEmoji,
              });
            return (
              <div
                class="pane-tab"
                classList={{
                  "pane-tab-active": p.activePaneId === id,
                  "pane-tab-drop": paneDragStore.dropTargetId() === id,
                }}
                data-pane-id={id}
                data-dot={dotState(id)}
                onPointerDown={(e) => startPaneDrag(id, paneTabLabel(pane), e)}
                onClick={() => p.onSelectPane(id)}
                onAuxClick={(e) => {
                  if (e.button === 1) {
                    e.preventDefault();
                    p.onClosePane(id);
                  }
                }}
                title={`${paneTabLabel(pane)}\n${t("sessiontabs.pane.tooltip")}`}
              >
                <AgentLight
                  light={p.agentLights[id] ?? null}
                  waitingOnPermission={p.waitingPaneIds.has(id)}
                  stateSince={p.agentStateSince[id] ?? null}
                  nowMs={p.agentNowMs}
                />
                <Show when={!p.agentLights[id]}>
                  <span class={`pane-tab-dot pane-tab-dot-${dotState(id)}`} />
                </Show>
                <Show when={ident().emoji}>
                  <span class="pane-tab-emoji">{ident().emoji}</span>
                </Show>
                <span class="pane-tab-label" dir="auto">
                  <TechText text={paneTabLabel(pane)} />
                </span>
                <button
                  class="pane-tab-close pane-btn"
                  onClick={(e) => {
                    e.stopPropagation();
                    p.onClosePane(id);
                  }}
                  title={t("common.close")}
                  aria-label={t("common.close")}
                >
                  <IconClose size={10} />
                </button>
              </div>
            );
          }}
        </For>
        <button
          class="pane-tab-new pane-btn"
          onClick={() => p.onNewSession()}
          title={t("sessiontabs.new.tooltip")}
          aria-label={t("sessiontabs.new")}
        >
          +
        </button>
        <button
          class="pane-tab-new pane-btn pane-tab-refresh"
          classList={{ "pane-tab-refresh-busy": p.loading }}
          onClick={() => p.onRefresh()}
          disabled={p.loading}
          title={p.error ?? t("sessiontabs.refresh")}
          aria-label={t("sessiontabs.refresh")}
        >
          <IconRefresh size={12} />
        </button>
        <Show when={!p.reachable}>
          <span class="pane-tabs-note">{t("sessiontabs.unreachable")}</span>
        </Show>
        <Show when={p.reachable && p.entries.length === 0 && p.extraPanes.length === 0 && !p.loading}>
          <span class="pane-tabs-note">{t("sessiontabs.empty")}</span>
        </Show>
      </div>
    </div>
  );
}
