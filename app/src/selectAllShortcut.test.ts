// Unit tests for the select_all (Ctrl+Shift+A) binding. Run:
//   cd app && node --experimental-strip-types --test src/selectAllShortcut.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeSelectAllBinding, inTerminal } from "./selectAllShortcut.ts";
import { buildShortcutTable, DEFAULT_SHORTCUTS, matches } from "./shortcuts.ts";

const inTermTarget = {
  closest: (s: string) => (s === ".terminal-container" ? {} : null),
} as unknown as EventTarget;
const outsideTarget = { closest: () => null } as unknown as EventTarget;

// Records which panes had selectAll() called.
function fakeTerms(ids: string[]) {
  const calls: string[] = [];
  const termFor = (pid: string) =>
    ids.includes(pid) ? { selectAll: () => void calls.push(pid) } : undefined;
  return { calls, termFor };
}

test("select_all: fires in a terminal with an active pane and selects only that pane", () => {
  // WHY: pins the happy path; selecting a sibling pane would be a data-visible bug.
  const { calls, termFor } = fakeTerms(["p1", "p2"]);
  const b = makeSelectAllBinding({ activePaneId: () => "p1", termFor });
  let prevented = 0;
  assert.equal(b.when({ target: inTermTarget }), true);
  b.run({ preventDefault: () => void prevented++ });
  assert.deepEqual(calls, ["p1"]);
  assert.equal(prevented, 1);
});

test("select_all: does not fire outside a terminal (settings modal)", () => {
  // WHY: Ctrl+Shift+A in a modal input must keep its native behavior.
  const { termFor } = fakeTerms(["p1"]);
  const b = makeSelectAllBinding({ activePaneId: () => "p1", termFor });
  assert.equal(b.when({ target: outsideTarget }), false);
});

test("select_all: does not fire with no active pane", () => {
  // WHY: without a pane there is nothing to select; skip so the key is not swallowed.
  const { termFor } = fakeTerms(["p1"]);
  const b = makeSelectAllBinding({ activePaneId: () => null, termFor });
  assert.equal(b.when({ target: inTermTarget }), false);
});

test("select_all: run is a safe no-op when the active pane has no terminal", () => {
  // WHY: pane may be mid-mount; run must not throw but still own preventDefault.
  const { calls, termFor } = fakeTerms([]);
  const b = makeSelectAllBinding({ activePaneId: () => "gone", termFor });
  let prevented = 0;
  b.run({ preventDefault: () => void prevented++ });
  assert.deepEqual(calls, []);
  assert.equal(prevented, 1);
});

test("inTerminal: null target and targets without closest are false", () => {
  // WHY: window-level key events can have a non-element target; must not throw.
  assert.equal(inTerminal({ target: null }), false);
  assert.equal(inTerminal({ target: {} as EventTarget }), false);
});

test("select_all: default chord is Ctrl+Shift+A", () => {
  // WHY: pins the default accelerator wiring through the real table + matcher.
  const accel = buildShortcutTable(DEFAULT_SHORTCUTS).select_all;
  const ev = { ctrlKey: true, altKey: false, shiftKey: true, metaKey: false, key: "A", code: "KeyA" };
  assert.equal(matches(ev, accel), true);
  assert.equal(matches({ ...ev, shiftKey: false }, accel), false);
});
