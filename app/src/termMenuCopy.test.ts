// Unit tests for menuCopyText.
// Run: node --experimental-strip-types --test src/termMenuCopy.test.ts
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { menuCopyText } from "./termMenuCopy.ts";

test("menuCopyText: a live xterm selection wins over OSC 52", () => {
  // Shift+drag selection must keep working when the app also owns the mouse.
  assert.equal(menuCopyText("picked", true, "osc"), "picked");
});

test("menuCopyText: no selection + app owns mouse falls back to last OSC 52", () => {
  // zellij drag-select never reaches xterm's selection; its OSC 52 is the only copy.
  assert.equal(menuCopyText("", true, "osc"), "osc");
});

test("menuCopyText: no selection + plain shell stays empty", () => {
  // A stale OSC 52 must not enable Copy in a pane that does not own the mouse.
  assert.equal(menuCopyText("", false, "osc"), "");
});

test("menuCopyText: nothing anywhere is empty", () => {
  assert.equal(menuCopyText("", true, ""), "");
});
