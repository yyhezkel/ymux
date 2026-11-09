// Unit tests for the shared byte formatter.
// Run: node --experimental-strip-types --test src/insightsFmt.test.ts  (node >= 22.6)
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";

import { fmtBps, fmtBytes } from "./insightsFmt.ts";

const CASES: Array<[number, string]> = [
  [0, "0 B"],
  [512, "512 B"],
  [1023, "1023 B"],
  [1024, "1.0 KB"],
  [1536, "1.5 KB"],
  [1048576, "1.0 MB"],
  [1500000000, "1.4 GB"],
];

for (const [input, expected] of CASES) {
  test(`fmtBytes(${input}) is ${expected}`, () => {
    assert.equal(fmtBytes(input), expected);
  });
}

test("a zero rate keeps its unit", () => {
  assert.equal(fmtBps(0), "0 B/s");
});
