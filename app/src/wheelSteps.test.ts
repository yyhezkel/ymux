// Phase 98 unit tests for wheelSteps.
// Run: node --experimental-strip-types --test src/wheelSteps.test.ts
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { wheelSteps } from "./wheelSteps.ts";

test("wheelSteps: one mouse notch is exactly one step, either way", () => {
  assert.deepEqual(wheelSteps(0, -100, 0), { steps: -1, carry: 0 });
  assert.deepEqual(wheelSteps(0, 100, 0), { steps: 1, carry: 0 });
});

test("wheelSteps: a scaled notch still counts as one step", () => {
  // Display scaling shrinks or grows the pixel delta of a single notch.
  assert.equal(wheelSteps(0, -83.3, 0).steps, -1);
  assert.equal(wheelSteps(0, 125, 0).steps, 1);
  assert.equal(wheelSteps(0, -83.3, 0).carry, 0);
});

test("wheelSteps: a fast double notch in one event is two steps", () => {
  assert.equal(wheelSteps(0, -200, 0).steps, -2);
});

test("wheelSteps: one event is capped at 10 steps", () => {
  assert.equal(wheelSteps(0, -5000, 0).steps, -10);
});

test("wheelSteps: touchpad deltas accumulate, then pay out one step", () => {
  let carry = 0;
  let total = 0;
  for (let i = 0; i < 4; i++) {
    const r = wheelSteps(carry, -10, 0);
    total += r.steps;
    carry = r.carry;
  }
  assert.equal(total, 0, "40px is under half a notch");
  const r = wheelSteps(carry, -10, 0);
  assert.equal(r.steps, -1, "the fifth 10px event crosses half a notch");
});

test("wheelSteps: a touchpad stream does not run away", () => {
  // 60 events of 5px = 300px of travel = three notches' worth, at most.
  let carry = 0;
  let total = 0;
  for (let i = 0; i < 60; i++) {
    const r = wheelSteps(carry, 5, 0);
    total += r.steps;
    carry = r.carry;
  }
  assert.ok(total >= 3 && total <= 6, `got ${total} steps for 300px`);
});

test("wheelSteps: reversing direction drops the other side's carry", () => {
  const up = wheelSteps(0, -40, 0);
  assert.equal(up.steps, 0);
  assert.equal(up.carry, -40);
  const down = wheelSteps(up.carry, 10, 0);
  assert.equal(down.steps, 0);
  assert.equal(down.carry, 10);
});

test("wheelSteps: line mode — 3 lines is one notch", () => {
  assert.equal(wheelSteps(0, -3, 1).steps, -1);
  assert.equal(wheelSteps(0, 1, 1).steps, 0);
});

test("wheelSteps: zero delta is a no-op that keeps the carry", () => {
  assert.deepEqual(wheelSteps(-30, 0, 0), { steps: 0, carry: -30 });
});
