import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { t } from "./i18n";
import type { Workspace } from "./types";
import {
  queueStatus,
  rowSinceMs,
  whatsHappening,
  inQueue,
  type QueueRow,
} from "./queueModel";
import { relAge, STATUS_EMOJI } from "./QueuePanel";

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
  const [draft, setDraft] = createSignal(p.ws.intent ?? "");

  const saveIfChanged = () => {
    const text = draft().trim();
    if (text !== (p.ws.intent ?? "")) p.onSaveIntent(text);
  };

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

        {/* 🎯 Intent — what you said you wanted here. Enter/blur save too,
            but the explicit button is the visible affordance (beta
            feedback: a field that saves invisibly reads as one that
            doesn't save at all). Disabled = current draft is saved. */}
        <label class="briefing-intent">
          <span class="briefing-intent-label">🎯 {t("briefing.intent.label")}</span>
          <div class="briefing-intent-row">
            <input
              type="text"
              dir="auto"
              placeholder={t("briefing.intent.placeholder")}
              value={draft()}
              onInput={(e) => setDraft(e.currentTarget.value)}
              onBlur={saveIfChanged}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  saveIfChanged();
                }
                e.stopPropagation();
              }}
            />
            <button
              class="primary briefing-intent-save"
              disabled={draft().trim() === (p.ws.intent ?? "")}
              onClick={saveIfChanged}
            >
              {draft().trim() === (p.ws.intent ?? "")
                ? t("briefing.intent.saved")
                : t("briefing.intent.save")}
            </button>
          </div>
        </label>

        {/* Pane briefs — the same row shape the Queue paints. */}
        <div class="briefing-rows">
          <Show
            when={agentRows().length > 0}
            fallback={<div class="briefing-none">{t("briefing.noAgentRows")}</div>}
          >
            <For each={agentRows()}>
              {(r) => {
                const status = queueStatus(r);
                const h = whatsHappening(r);
                return (
                  <div
                    class="queue-row"
                    data-status={status}
                    onClick={() => p.onJumpPane(r.paneId)}
                    title={t("queue.jump")}
                  >
                    <span class="queue-row-status" title={t(`queue.status.${status}`)}>
                      {STATUS_EMOJI[status]}
                    </span>
                    <div class="queue-row-main">
                      <div class="queue-row-top">
                        <span class="queue-row-title" dir="auto">{r.title}</span>
                        <span class="queue-row-age">{relAge(rowSinceMs(r), p.nowMs)}</span>
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
          </Show>
        </div>
      </div>
    </div>
  );
}
