// Unit tests for claudeRunning.
// Run: node --experimental-strip-types --test src/claudeRunning.test.ts   (node >= 22.6)
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { tuiSignalOnConnect, claudeRunningWrite } from "./claudeRunning.ts";

// WHY: a claude-mode connect always means Claude runs; breaking it drops the signal on launch.
test("claude mode connect signals true regardless of persisted", () => {
  assert.equal(tuiSignalOnConnect("claude", false, null), true);
  assert.equal(tuiSignalOnConnect("claude", true, false), true);
});

// WHY: a fresh shell connect must clear any inherited TUI state.
test("fresh non-claude connect signals false", () => {
  assert.equal(tuiSignalOnConnect("shell", false, true), false);
  assert.equal(tuiSignalOnConnect("shell", false, null), false);
});

// WHY: restore/reattach with persisted true must start in Claude bidi state without a hook.
test("restore reattach with persisted true signals true", () => {
  assert.equal(tuiSignalOnConnect("shell", true, true), true);
});

// WHY: restore with nothing persisted must leave the signal untouched (null = no call).
test("restore with persisted false/null/undefined makes no call", () => {
  assert.equal(tuiSignalOnConnect("shell", true, false), null);
  assert.equal(tuiSignalOnConnect("shell", true, null), null);
  assert.equal(tuiSignalOnConnect("shell", true, undefined), null);
});

// WHY: pins the accepted stale-true trade-off and its correction path (session-end hook).
test("stale persisted true restores true, then stop write corrects it", () => {
  assert.equal(tuiSignalOnConnect("shell", true, true), true);
  assert.equal(claudeRunningWrite(true, false), false);
});

// WHY: writes only on transitions, so repeated stop/start hooks never rewrite workspaces.json.
test("claudeRunningWrite skips no-op transitions", () => {
  assert.equal(claudeRunningWrite(true, true), null);
  assert.equal(claudeRunningWrite(false, false), null);
  assert.equal(claudeRunningWrite(null, false), null);
  assert.equal(claudeRunningWrite(undefined, false), null);
  assert.equal(claudeRunningWrite(null, true), true);
  assert.equal(claudeRunningWrite(false, true), true);
});
