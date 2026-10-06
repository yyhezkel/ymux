// Unit tests for the window-title pane name. Run:
//   cd app && node --experimental-strip-types --test src/windowPaneName.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { windowPaneName } from "./windowPaneName.ts";

const describe = (c: { host: string }) => `ssh ${c.host}`;

test("title wins over auto_title and connection", () => {
  // Breaking this means a user-set name stops showing in the window title.
  assert.equal(windowPaneName({ title: "T", auto_title: "A", connection: { host: "h" } }, describe), "T");
});

test("auto_title used when no title", () => {
  assert.equal(windowPaneName({ auto_title: "A", connection: { host: "h" } }, describe), "A");
});

test("empty title falls through", () => {
  // `||` not `??`: an empty-string title must not blank the window title.
  assert.equal(windowPaneName({ title: "", auto_title: "", connection: { host: "h" } }, describe), "ssh h");
});

test("connection described when no titles", () => {
  assert.equal(windowPaneName({ connection: { host: "box" } }, describe), "ssh box");
});

test("null when nothing to show", () => {
  // Caller keeps its workspace-name fallback only if this stays null.
  assert.equal(windowPaneName({}, describe), null);
  assert.equal(windowPaneName({ title: null, auto_title: null, connection: null }, describe), null);
});
