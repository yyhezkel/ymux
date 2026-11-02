// Unit tests for backend/web/hello.ts (browser hello filter). Run:
//   cd app && node --experimental-strip-types --test src/webHello.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { touchedEntries } from "./backend/web/hello.ts";

test("drops an entry no hook has touched", () => {
  // seq 0 = zero-value hello entry; keeping it would show "agent done" on a plain shell
  assert.deepEqual(touchedEntries({ a: { seq: 0, status: "done" } }), {});
});

test("keeps an entry a hook has written", () => {
  // finished turn / session_ended must survive: seq > 0 is the desktop shape
  const m = { a: { seq: 1, status: "done" }, b: { seq: 7, event: "session_ended" } };
  assert.deepEqual(touchedEntries(m), m);
});

test("null and undefined give an empty map", () => {
  // hello may be absent before the first frame
  assert.deepEqual(touchedEntries(null), {});
  assert.deepEqual(touchedEntries(undefined), {});
});

test("drops non-object, missing and non-numeric seq", () => {
  // malformed daemon data must not leak into the queue
  assert.deepEqual(touchedEntries({ a: 5, b: { seq: "1" }, c: {} }), {});
});

test("mixed map keeps only touched entries", () => {
  // filter is per-key, not all-or-nothing
  assert.deepEqual(touchedEntries({ a: { seq: 0 }, b: { seq: 2 } }), { b: { seq: 2 } });
});
