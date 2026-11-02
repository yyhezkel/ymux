import { createSignal, For, Show } from "solid-js";
import { t } from "./i18n";
import { IconBot } from "./icons";
import { createLogger } from "./logger";
import { PanelSurface } from "./PanelSurface";
import type { Geometry } from "./floatingWindow";
import type { Surface } from "./panels";
import { groupQueueRows, type QueueRow } from "./queueModel";
import { QueueRowView } from "./QueueRow";

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

const log = createLogger("QUEUE");

// The adoption snippet — what the user pastes into a project's CLAUDE.md
// (or the agent's equivalent) so its agents actually write briefs.
// Deliberately English and verbatim: it is instructions FOR an agent, not
// UI copy, and it must match brief.rs's parser exactly.
export const BRIEF_SNIPPET = `## ymux briefs

End EVERY final answer with this plain-text block, as the last lines of
the message:

[ymux-brief]
task: the task in a few words
status: working | waiting-for-you | stuck | done
ask: one closed question — only when you need my decision
rec: your recommendation for that question, in one sentence
next: the immediate next step
delta: what changed since your previous brief
goal: the session's overall goal, one imperative line
done: when this session counts as done, one line

Rules: every value ≤ 80 chars, imperative, one line. Write \`goal\` and
\`done\` in your FIRST brief and repeat them only when they change.
\`ask\` never appears without \`rec\`; no history — only this turn's
facts; omit a field rather than leaving it empty.`;

export function QueuePanel(p: Props) {
  const groups = () => groupQueueRows(p.rows);
  const [copied, setCopied] = createSignal(false);

  const copySnippet = async () => {
    try {
      await navigator.clipboard.writeText(BRIEF_SNIPPET);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch (e) {
      log.warn("copy snippet failed", e);
    }
  };

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
              <div class="queue-empty-hint">{t("queue.empty.snippetHint")}</div>
              <pre class="queue-snippet" dir="ltr">{BRIEF_SNIPPET}</pre>
              <button class="primary" onClick={() => void copySnippet()}>
                {copied() ? t("queue.copied") : t("queue.copy")}
              </button>
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
                  {(r) => (
                    <QueueRowView
                      row={r}
                      nowMs={p.nowMs}
                      onClick={() => p.onJump(r.wsId, r.paneId)}
                    />
                  )}
                </For>
              </div>
            )}
          </For>
        </Show>
      )}
    />
  );
}
