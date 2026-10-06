// Phase 84.B: the traffic light shown on a pane header and on its tab.
//
// Green = Claude is working. Yellow = it finished, your move. Red = it is
// blocked on you. A red square (failed) = the turn died on an API error,
// nothing is pending. Nothing at all = we don't know, which is the honest
// answer for a plain shell pane, a disconnected pane, or one whose state
// is old enough to be untrustworthy.
//
// Pure and Solid-free on purpose: this is the single place the pane
// header and the tab strip both call, so the two can never disagree
// about what colour a pane is. Tests live in paneAgentState.test.ts.

/// Mirrors PaneAgentState::as_str() in app/src-tauri/src/lib.rs. The
/// desktop owns the transition table; this file only decides how to
/// PAINT the state it is handed.
export type PaneAgentState =
  | "unknown"
  | "idle"
  | "running"
  | "done"
  | "needs-input"
  | "failed";

export type TrafficLight = "green" | "yellow" | "red" | "failed";

export interface AgentLightInput {
  state: PaneAgentState;
  /** Epoch-ms of the last real state change, from the backend. */
  stateSince: number | null;
  /** A pending blocking ymux permission card is bound to this pane. */
  waitingOnPermission: boolean;
  /** The pane has a live session. */
  connected: boolean;
  /** Reactive clock, so staleness re-evaluates without its own timer. */
  nowMs: number;
}

// A pane whose Claude was SIGKILLed emits no SessionEnd, so its last
// state sits in memory forever — green, indefinitely. Six hours is long
// enough never to fire during real work and short enough that a pane left
// overnight isn't still claiming to be running in the morning.
export const STALE_AFTER_MS = 6 * 60 * 60 * 1000;

/**
 * The colour to paint, or null to paint nothing.
 *
 * Two hard gates run before the state is even consulted, because both
 * describe situations where the stored state is not evidence of anything:
 * a pane with no live session, and a state old enough that no hook has
 * confirmed it in hours.
 *
 * Precedence below that: our own blocking permission card outranks the
 * agent's own "I need input", which outranks running, which outranks
 * done. The card wins because it is synchronous with its own lifecycle —
 * it is on screen right now, waiting for a click.
 */
export function trafficLight(i: AgentLightInput): TrafficLight | null {
  if (!i.connected) return null;
  // The permission card is checked before staleness: the card's own
  // presence is the evidence, and it does not go stale while it is up.
  if (i.waitingOnPermission) return "red";
  if (i.state === "unknown" || i.state === "idle") return null;
  if (i.stateSince != null && i.nowMs - i.stateSince > STALE_AFTER_MS) {
    return null;
  }
  switch (i.state) {
    case "needs-input":
      return "red";
    case "running":
      return "green";
    case "done":
      return "yellow";
    case "failed":
      return "failed";
  }
}

/** i18n key describing the light, for the tooltip and the aria-label. */
export function trafficLightKey(
  light: TrafficLight,
  waitingOnPermission: boolean,
): string {
  if (waitingOnPermission) return "pane.agent.state.waiting_approval";
  switch (light) {
    case "red":
      return "pane.agent.state.needs_input";
    case "green":
      return "pane.agent.state.running";
    case "yellow":
      return "pane.agent.state.done";
    case "failed":
      return "pane.agent.state.failed";
  }
}

/** The focused pane's light, reduced to what the live region needs. */
export interface AnnounceSnapshot {
  paneId: string | null;
  key: string | null;
}

/** i18n key to speak, or null for silence. Speaks only when the SAME focused
 *  pane's key changed: focus moves and first sight of a pane stay quiet. */
export function agentAnnounceKey(
  prev: AnnounceSnapshot,
  next: AnnounceSnapshot,
): string | null {
  if (next.paneId === null || next.key === null) return null;
  if (prev.paneId !== next.paneId) return null;
  return prev.key === next.key ? null : next.key;
}

/** M:SS, zero-padded seconds. Shared with the pane header's turn ticker. */
export function clockMMSS(sec: number): string {
  return `${Math.floor(sec / 60)}:${String(sec % 60).padStart(2, "0")}`;
}

/** How long the pane has been in its current state, as M:SS, or "". */
export function elapsedLabel(
  stateSince: number | null,
  nowMs: number,
): string {
  if (stateSince == null) return "";
  return clockMMSS(Math.max(0, Math.floor((nowMs - stateSince) / 1000)));
}
