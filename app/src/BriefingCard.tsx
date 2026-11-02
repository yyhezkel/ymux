import { For, onCleanup, onMount, Show } from "solid-js";
import { t } from "./i18n";
import type { Workspace } from "./types";
import { inQueue, type QueueRow } from "./queueModel";
import { QueueRowView } from "./QueueRow";
import { IntentEditor } from "./IntentEditor";

// BRIEF: the workspace-entry Briefing card — "what did I want here, what
// happened, what's the state now", in one glance. Shown on return-after-
// absence / idle-return (both opt-in) and manually via shortcut; App owns
// the triggers, this component only renders one workspace's picture.
//
// Native Browser webviews paint above HTML, so App adds the card's signal
// to anyModalOpen() — do not mount this outside that arrangement.

interface Props {
  ws: Workspace;
  /** This workspace's rows, from App's allPaneAgentRows(). */
  rows: QueueRow[];
  nowMs: number;
  onSaveIntent: (text: string) => void;
  onJumpPane: (paneId: string) => void;
  onClose: () => void;
}

export function BriefingCard(p: Props) {
  // Esc closes. Registered on window capture so it wins over the pane
  // focus that sits underneath the backdrop.
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        p.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const agentRows = () => p.rows.filter(inQueue);

  return (
    <div class="modal-backdrop" onClick={p.onClose}>
      <div class="modal briefing-card" onClick={(e) => e.stopPropagation()}>
        <div class="briefing-head">
          <span class="briefing-ws" dir="auto">
            {p.ws.emoji ? `${p.ws.emoji} ` : ""}{p.ws.name}
          </span>
          <button class="side-drawer-btn" title={t("briefing.close")} onClick={p.onClose}>
            ✕
          </button>
        </div>

        {/* 🎯 Intent — IntentEditor.tsx (the Briefing card is its only host). */}
        <IntentEditor intent={p.ws.intent} onSave={p.onSaveIntent} />

        {/* Pane briefs — the same row shape the Queue paints. */}
        <div class="briefing-rows">
          <Show
            when={agentRows().length > 0}
            fallback={<div class="briefing-none">{t("briefing.noAgentRows")}</div>}
          >
            <For each={agentRows()}>
              {(r) => (
                <QueueRowView row={r} nowMs={p.nowMs} onClick={() => p.onJumpPane(r.paneId)} />
              )}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}
