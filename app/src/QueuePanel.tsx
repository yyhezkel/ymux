import { For, Show } from "solid-js";
import { t } from "./i18n";
import { IconBot } from "./icons";
import { PanelSurface } from "./PanelSurface";
import type { Geometry } from "./floatingWindow";
import type { Surface } from "./panels";
import {
  groupQueueRows,
  queueStatus,
  rowSinceMs,
  whatsHappening,
  type QueueRow,
  type QueueStatus,
} from "./queueModel";

// BRIEF: the Queue — every agent pane across every workspace, sorted by
// who needs the user. Grouped by workspace (the reference table's "CRM —
// 5" shape); all verdicts come from queueModel.ts, this file only paints.
// Rides the shared PanelSurface lifecycle like Tickets/Files/Monitor.
//
// Deliberately dumb about data: App hands it the same rows the sidebar
// indicator derives from (allPaneAgentRows), so the two can't disagree.

interface Props {
  surface: Surface;
  rows: QueueRow[];
  nowMs: number;
  onJump: (workspaceId: string, paneId: string) => void;
  onClose: () => void;
  onDrawer: () => void;
  onFloat: () => void;
  onFullscreen: () => void;
}

export const STATUS_EMOJI: Record<QueueStatus, string> = {
  "needs-input": "⏸️",
  stuck: "⚠️",
  waiting: "⏸️",
  working: "🔄",
  done: "💤",
  ended: "✅",
};

export function relAge(ms: number | null, now: number): string {
  if (ms == null) return "";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return t("notif.time.now");
  const m = Math.round(s / 60);
  if (m < 60) return t("notif.time.min").replace("{n}", String(m));
  const h = Math.round(m / 60);
  if (h < 24) return t("notif.time.hour").replace("{n}", String(h));
  return t("notif.time.day").replace("{n}", String(Math.round(h / 24)));
}

export function QueuePanel(p: Props) {
  const groups = () => groupQueueRows(p.rows);

  return (
    <PanelSurface
      surface={p.surface}
      icon={<IconBot />}
      title={t("queue.title")}
      bodyClass="queue-body"
      drawerStorageKey="ymux.drawer-width.queue"
      drawerDefaultWidth={460}
      drawerMinWidth={340}
      floatStorageKey="ymux.panel-queue-geometry"
      floatDefault={{ x: 200, y: 80, w: 520, h: 640 } satisfies Geometry}
      floatMinW={340}
      floatMinH={360}
      onClose={p.onClose}
      onDrawer={p.onDrawer}
      onFloat={p.onFloat}
      onFullscreen={p.onFullscreen}
      body={() => (
        <Show
          when={groups().length > 0}
          fallback={
            <div class="queue-empty">
              <div class="queue-empty-title">{t("queue.empty.title")}</div>
              <div class="queue-empty-desc">{t("queue.empty.desc")}</div>
            </div>
          }
        >
          <For each={groups()}>
            {(g) => (
              <div class="queue-group">
                <div class="queue-group-head">
                  <span class="queue-group-name" dir="auto">{g.wsName}</span>
                  <span class="queue-group-count">{g.rows.length}</span>
                  <Show when={g.attention > 0}>
                    <span class="queue-group-attn" title={t("queue.attention")}>
                      {g.attention}
                    </span>
                  </Show>
                </div>
                <For each={g.rows}>
                  {(r) => {
                    const status = queueStatus(r);
                    const h = whatsHappening(r);
                    return (
                      <div
                        class="queue-row"
                        data-status={status}
                        onClick={() => p.onJump(r.wsId, r.paneId)}
                        title={t("queue.jump")}
                      >
                        <span class="queue-row-status" title={t(`queue.status.${status}`)}>
                          {STATUS_EMOJI[status]}
                        </span>
                        <div class="queue-row-main">
                          <div class="queue-row-top">
                            <span class="queue-row-title" dir="auto">{r.title}</span>
                            <span class="queue-row-age">
                              {relAge(rowSinceMs(r), p.nowMs)}
                            </span>
                          </div>
                          <Show when={h}>
                            {(hh) => (
                              <div
                                class="queue-row-happening"
                                classList={{ "queue-dim": hh().dim }}
                                dir="auto"
                              >
                                {hh().kind === "prompt"
                                  ? t("queue.gotFromYou", { text: hh().text })
                                  : hh().text}
                              </div>
                            )}
                          </Show>
                        </div>
                      </div>
                    );
                  }}
                </For>
              </div>
            )}
          </For>
        </Show>
      )}
    />
  );
}
