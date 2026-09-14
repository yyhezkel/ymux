import { For, Show } from "solid-js";
import { IconClose } from "./icons";
import { t } from "./i18n";
import { paneDragStore, startPaneDrag } from "./paneDrag";
import { TechText } from "./TechText";
import { AgentLight } from "./AgentLight";
import type { TrafficLight } from "./paneAgentState";
import { paneLabel, type PaneNode } from "./paneTitle";
import {
  effectiveIdentity,
  type Connection,
} from "./types";

interface Props {
  panes: PaneNode[];
  activePaneId: string | null;
  connectedPaneIds: Set<string>;
  // A pending blocking feed item — same signal that drives `.pane.waiting`.
  waitingPaneIds: Set<string>;
  // OSC 9/99/777 activity the user hasn't looked at yet.
  notifiedPaneIds: Set<string>;
  panePulseEnabled: boolean;
  workspaceName?: string;
  workspaceColor?: string;
  workspaceEmoji?: string;
  workspaceConnection?: Connection;
  // Phase 84.B: the Claude traffic light per pane, resolved by App from
  // the same trafficLight() the pane header uses — so a tab and the pane
  // it selects can never disagree about what colour the agent is.
  agentLights: Record<string, TrafficLight | null>;
  agentStateSince: Record<string, number | null>;
  agentNowMs: number;
  onSelect: (paneId: string) => void;
  onClose: (paneId: string) => void;
  onNew: () => void;
}

// Phase 84.A: the tab strip for a workspace in tabs mode.
//
// Deliberately dumb — it owns no state. The active tab IS `activePaneId`,
// the tab list IS the workspace's pane list, and switching tabs is just
// focusing a pane. Everything it renders already existed as a per-pane
// signal in App.tsx; this is a second view onto it, not a second source.
//
// Markup and classes mirror the browser window's tab strip
// (BrowserWindow.tsx / `.bw-*` in App.css), including its RTL treatment:
// the strip itself stays LTR so tab positions don't jump when the app
// language flips, while each label carries dir="auto".
export function PaneTabs(p: Props) {
  // BRIEF (commit 2): precedence lifted to paneTitle.ts so the Queue and
  // the Briefing card call the exact same label logic.
  const label = (pane: PaneNode): string =>
    paneLabel(pane, {
      workspaceName: p.workspaceName,
      workspaceConnection: p.workspaceConnection,
    });

  // Precedence mirrors the sidebar's two-tier dot (Sidebar.tsx
  // `ws-waiting-dot`): blocking outranks activity outranks connected.
  const dotState = (paneId: string): string => {
    if (p.waitingPaneIds.has(paneId)) return "waiting";
    if (p.panePulseEnabled && p.notifiedPaneIds.has(paneId)) return "activity";
    if (p.connectedPaneIds.has(paneId)) return "connected";
    return "idle";
  };

  return (
    <div class="pane-tabs">
      <div class="pane-tabs-list">
        <For each={p.panes}>
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
                // The whole drag story. paneDrag resolves drop targets with
                // elementFromPoint(...).closest("[data-pane-id]"), so this
                // one attribute gives tab reordering the existing store,
                // ghost, Escape handler and workspace_swap_panes for free.
                // Semantics are swap, matching pane drag-and-drop.
                data-pane-id={id}
                data-dot={dotState(id)}
                onPointerDown={(e) => startPaneDrag(id, label(pane), e)}
                onClick={() => p.onSelect(id)}
                onAuxClick={(e) => {
                  // Middle-click closes, as in the browser strip.
                  if (e.button === 1) {
                    e.preventDefault();
                    p.onClose(id);
                  }
                }}
                title={label(pane)}
              >
                {/* The traffic light comes first — it is the thing the
                    user is scanning the strip for. The connection dot
                    follows and is suppressed while a light is showing,
                    so a tab never carries two competing status marks. */}
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
                  <TechText text={label(pane)} />
                </span>
                <Show when={p.panes.length > 1}>
                  <button
                    class="pane-tab-close pane-btn"
                    onClick={(e) => {
                      e.stopPropagation();
                      p.onClose(id);
                    }}
                    title={t("common.close")}
                    aria-label={t("common.close")}
                  >
                    <IconClose size={10} />
                  </button>
                </Show>
              </div>
            );
          }}
        </For>
        <button
          class="pane-tab-new pane-btn"
          onClick={() => p.onNew()}
          title={t("panetabs.new.tooltip")}
          aria-label={t("panetabs.new")}
        >
          +
        </button>
      </div>
    </div>
  );
}
