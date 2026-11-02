import { Show } from "solid-js";
import { t } from "./i18n";
import {
  elapsedLabel,
  trafficLightKey,
  type TrafficLight,
} from "./paneAgentState";

interface Props {
  /** null renders nothing — see trafficLight() for when that happens. */
  light: TrafficLight | null;
  waitingOnPermission: boolean;
  /** Epoch-ms of the last state change, for the tooltip's elapsed time. */
  stateSince: number | null;
  nowMs: number;
}

// Phase 84.B: the per-pane Claude traffic light.
//
// Shape carries the meaning, not just hue: a filled disc for running, a
// hollow ring for done, a triangle for needs-input, a square for failed. Colour alone fails
// WCAG 1.4.1 and fails anyone with a red-green deficiency, and these four
// have to be told apart at 8px in peripheral vision — which is the entire
// use case, glancing at a strip of tabs.
export function AgentLight(p: Props) {
  const label = (): string => {
    if (!p.light) return "";
    const state = t(trafficLightKey(p.light, p.waitingOnPermission));
    const el = elapsedLabel(p.stateSince, p.nowMs);
    return el ? t("pane.agent.tooltip.elapsed", { state, time: el }) : state;
  };
  return (
    <Show when={p.light}>
      {(light) => (
        <span
          class="pane-agent-light"
          data-light={light()}
          role="img"
          aria-label={label()}
          title={label()}
        />
      )}
    </Show>
  );
}
