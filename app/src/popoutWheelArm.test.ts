// Unit tests for resolvePopoutTmuxArm.
// Run: node --experimental-strip-types --test src/popoutWheelArm.test.ts
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { resolvePopoutTmuxArm } from "./popoutWheelArm.ts";

test("resolvePopoutTmuxArm: listed pane arms the proxy", async () => {
  // Breaking this means tmux-persisted popouts lose copy-mode wheel.
  const armed = await resolvePopoutTmuxArm("p1", async () => ({ p1: "sess" }), () => {});
  assert.equal(armed, true);
});

test("resolvePopoutTmuxArm: unlisted pane stays unarmed", async () => {
  // Plain shell must keep the xterm wheel.
  const armed = await resolvePopoutTmuxArm("p2", async () => ({ p1: "sess" }), () => {});
  assert.equal(armed, false);
});

test("resolvePopoutTmuxArm: null or empty id is false without listing", async () => {
  // No seeded pane id → no backend call.
  let calls = 0;
  const list = async () => {
    calls++;
    return { p1: "sess" };
  };
  assert.equal(await resolvePopoutTmuxArm(null, list, () => {}), false);
  assert.equal(await resolvePopoutTmuxArm("", list, () => {}), false);
  assert.equal(calls, 0);
});

test("resolvePopoutTmuxArm: list failure warns once and returns false", async () => {
  // A backend error must never throw into onMount nor arm the proxy.
  const warns: unknown[][] = [];
  const err = new Error("boom");
  const armed = await resolvePopoutTmuxArm(
    "p1",
    async () => {
      throw err;
    },
    (m, e) => warns.push([m, e]),
  );
  assert.equal(armed, false);
  assert.deepEqual(warns, [["popout wheel-proxy arm failed", err]]);
});
