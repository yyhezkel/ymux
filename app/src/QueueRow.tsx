import { Show } from "solid-js";
import { t } from "./i18n";
import {
  queueStatus,
  rowSinceMs,
  whatsHappening,
  type QueueRow,
  type QueueStatus,
} from "./queueModel";

// BRIEF / Phase 104: one agent row — status emoji, title, age, and the
// "what's happening" line. Shared by the Queue panel, the Briefing card
// and the Context Rail so the three can never paint the same pane
// differently. All verdicts come from queueModel.ts; this only paints.
// Agent text is rendered as plain text (Solid escapes it).

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

interface Props {
  row: QueueRow;
  nowMs: number;
  onClick: () => void;
}

export function QueueRowView(p: Props) {
  const status = () => queueStatus(p.row);
  const happening = () => whatsHappening(p.row);
  return (
    <div
      class="queue-row"
      data-status={status()}
      role="button"
      tabIndex={0}
      onClick={() => p.onClick()}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          p.onClick();
        }
      }}
      title={t("queue.jump")}
    >
      <span class="queue-row-status" title={t(`queue.status.${status()}`)}>
        {STATUS_EMOJI[status()]}
      </span>
      <div class="queue-row-main">
        <div class="queue-row-top">
          <span class="queue-row-title" dir="auto">{p.row.title}</span>
          <span class="queue-row-age">{relAge(rowSinceMs(p.row), p.nowMs)}</span>
        </div>
        <Show when={happening()}>
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
}
