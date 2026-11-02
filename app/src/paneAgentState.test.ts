// Unit tests for the pane traffic light (Phase 84.B). Run:
//   cd app && node --experimental-strip-types --test src/paneAgentState.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
//
// The thesis these guard: a WRONG light is worse than no light. Every
// case below is one where painting a colour would be a lie.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  trafficLight,
  trafficLightKey,
  agentAnnounceKey,
  elapsedLabel,
  STALE_AFTER_MS,
  type AgentLightInput,
} from "./paneAgentState.ts";

const NOW = 1_700_000_000_000;

const input = (over: Partial<AgentLightInput> = {}): AgentLightInput => ({
  state: "unknown",
  stateSince: NOW,
  waitingOnPermission: false,
  connected: true,
  nowMs: NOW,
  ...over,
});

test("a pane with no agent gets no light", () => {
  // A plain shell pane must not sprout a status dot just because the
  // workspace happens to be in tabs mode.
  assert.equal(trafficLight(input({ state: "unknown" })), null);
  assert.equal(trafficLight(input({ state: "idle" })), null);
});

test("the three colours map to the three states", () => {
  assert.equal(trafficLight(input({ state: "running" })), "green");
  assert.equal(trafficLight(input({ state: "done" })), "yellow");
  assert.equal(trafficLight(input({ state: "needs-input" })), "red");
});

test("a failed turn paints the failed light, but only while trustworthy", () => {
  // StopFailure must not look like yellow "done"; a disconnected or stale
  // failed pane is no evidence, same gates as every other state.
  assert.equal(trafficLight(input({ state: "failed" })), "failed");
  assert.equal(trafficLight(input({ state: "failed", connected: false })), null);
  assert.equal(
    trafficLight(input({ state: "failed", stateSince: NOW - STALE_AFTER_MS - 1 })),
    null,
  );
});

test("a disconnected pane shows nothing, whatever it last said", () => {
  // Otherwise a pane keeps displaying the state it had when its session
  // died, which reads as a live agent that is simply never finishing.
  for (const state of ["running", "done", "needs-input"] as const) {
    assert.equal(trafficLight(input({ state, connected: false })), null);
  }
});

test("state older than the staleness cutoff stops being evidence", () => {
  // A SIGKILLed Claude emits no SessionEnd, so its last state sits in
  // memory forever. Without this the pane is green until the app restarts.
  const stale = input({
    state: "running",
    stateSince: NOW - STALE_AFTER_MS - 1,
  });
  assert.equal(trafficLight(stale), null);

  const fresh = input({
    state: "running",
    stateSince: NOW - STALE_AFTER_MS + 1000,
  });
  assert.equal(trafficLight(fresh), "green");
});

test("a pending permission card outranks everything and never goes stale", () => {
  // The card is on screen right now waiting for a click; its own presence
  // is the evidence, so the age of the last hook is irrelevant.
  const old = input({
    state: "running",
    stateSince: NOW - STALE_AFTER_MS * 10,
    waitingOnPermission: true,
  });
  assert.equal(trafficLight(old), "red");
  // ...but a card on a pane with no session still shows nothing.
  assert.equal(
    trafficLight(input({ waitingOnPermission: true, connected: false })),
    null,
  );
});

test("a state with no timestamp is trusted rather than dropped", () => {
  // stateSince is null only before the first real transition. Treating
  // that as stale would blank a light that just arrived.
  assert.equal(
    trafficLight(input({ state: "running", stateSince: null })),
    "green",
  );
});

test("the tooltip key distinguishes our card from the agent's own ask", () => {
  // "Waiting for your approval" (a ymux card you can click) is a different
  // instruction to the user than "Claude needs your input" (go type).
  assert.equal(
    trafficLightKey("red", true),
    "pane.agent.state.waiting_approval",
  );
  assert.equal(trafficLightKey("red", false), "pane.agent.state.needs_input");
  assert.equal(trafficLightKey("green", false), "pane.agent.state.running");
  assert.equal(trafficLightKey("yellow", false), "pane.agent.state.done");
  assert.equal(trafficLightKey("failed", false), "pane.agent.state.failed");
});

test("elapsed renders M:SS and never counts backwards", () => {
  assert.equal(elapsedLabel(NOW - 65_000, NOW), "1:05");
  assert.equal(elapsedLabel(NOW - 5_000, NOW), "0:05");
  assert.equal(elapsedLabel(null, NOW), "");
  // Clock skew between the backend's stamp and the frontend's tick must
  // not produce "-1:-1".
  assert.equal(elapsedLabel(NOW + 5_000, NOW), "0:00");
});

// Phase 84.C: focused-pane announcement rule. Breaking any of these means a
// screen reader either misses the focused pane's transition or chatters about
// panes/focus changes the user did not cause.
const snap = (paneId: string | null, key: string | null) => ({ paneId, key });

test("O1: same focused pane, key changed -> announce the new key", () => {
  assert.equal(
    agentAnnounceKey(snap("p1", "pane.agent.state.running"), snap("p1", "pane.agent.state.done")),
    "pane.agent.state.done",
  );
});

test("O2: same focused pane -> red announced (needs input / waiting approval)", () => {
  // Red states are the ones the user must act on; missing either means a
  // blind user never learns Claude is blocked on them.
  assert.equal(
    agentAnnounceKey(snap("p1", "pane.agent.state.done"), snap("p1", "pane.agent.state.needs_input")),
    "pane.agent.state.needs_input",
  );
  assert.equal(
    agentAnnounceKey(snap("p1", "pane.agent.state.running"), snap("p1", "pane.agent.state.waiting_approval")),
    "pane.agent.state.waiting_approval",
  );
});

test("O3: same pane, same key -> silent (no repeat on unrelated ticks)", () => {
  assert.equal(
    agentAnnounceKey(snap("p1", "pane.agent.state.done"), snap("p1", "pane.agent.state.done")),
    null,
  );
});

test("O4: focus moved to another pane -> silent; next change on that pane announced", () => {
  // Focus move re-baselines: speaking the new pane's state would be chatter
  // the user did not cause, but its following change must still be heard.
  assert.equal(
    agentAnnounceKey(snap("p1", "pane.agent.state.running"), snap("p2", "pane.agent.state.done")),
    null,
  );
  assert.equal(
    agentAnnounceKey(snap("p2", "pane.agent.state.done"), snap("p2", "pane.agent.state.needs_input")),
    "pane.agent.state.needs_input",
  );
});

test("mount from no focus -> silent", () => {
  assert.equal(
    agentAnnounceKey(snap(null, null), snap("p1", "pane.agent.state.running")),
    null,
  );
});

test("a null next key or null next pane never announces", () => {
  // Light vanished (agent gone) or nothing focused: nothing to say.
  assert.equal(agentAnnounceKey(snap("p1", "pane.agent.state.running"), snap("p1", null)), null);
  assert.equal(agentAnnounceKey(snap("p1", "pane.agent.state.running"), snap(null, null)), null);
});
