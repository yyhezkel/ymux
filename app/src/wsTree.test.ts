// Unit tests for the header / screen rule (Phase 92). Run:
//   cd app && node --experimental-strip-types --test src/wsTree.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ancestorsOf,
  childrenInOrder,
  firstScreenOf,
  isHeader,
  rootIdOf,
  screenOrSelf,
  type TreeNode,
} from "./wsTree.ts";

const node = (id: string, parent: string | null, extra: Partial<TreeNode> = {}): TreeNode => ({
  id,
  parent_id: parent,
  is_project_root: false,
  sort_order: null,
  ...extra,
});

// srv (root) → shell, app (folder) → wt (sort 0), app-shell (sort -1)
//            → row-dev
// bare (root, no children)
const tree: TreeNode[] = [
  node("srv", null),
  node("shell", "srv"),
  node("app", "srv", { is_project_root: true }),
  node("wt", "app", { sort_order: 0 }),
  node("app-shell", "app", { sort_order: -1 }),
  node("row-dev", "srv"),
  node("bare", null),
];

test("a root and a pinned folder are headers; everything else is a screen", () => {
  assert.equal(isHeader(node("srv", null)), true);
  assert.equal(isHeader(node("app", "srv", { is_project_root: true })), true);
  assert.equal(isHeader(node("wt", "app")), false);
  assert.equal(isHeader(node("row", "srv")), false);
});

test("ancestors are nearest first and the root is the last of them", () => {
  assert.deepEqual(ancestorsOf(tree, "wt").map((w) => w.id), ["app", "srv"]);
  assert.deepEqual(ancestorsOf(tree, "srv"), []);
  assert.equal(rootIdOf(tree, "wt"), "srv");
  assert.equal(rootIdOf(tree, "srv"), "srv");
  assert.equal(rootIdOf(tree, "unknown"), "unknown");
});

test("a parent cycle does not spin", () => {
  const loop = [node("a", "b"), node("b", "a")];
  assert.equal(rootIdOf(loop, "a").length > 0, true);
});

test("children sort by sort_order, null last, insertion order as the tie-break", () => {
  assert.deepEqual(childrenInOrder(tree, "app").map((w) => w.id), ["app-shell", "wt"]);
  assert.deepEqual(childrenInOrder(tree, "srv").map((w) => w.id), ["shell", "app", "row-dev"]);
});

test("the first screen skips a root's folder children", () => {
  assert.equal(firstScreenOf(tree, "srv"), "shell");
  assert.equal(firstScreenOf(tree, "app"), "app-shell");
  assert.equal(firstScreenOf(tree, "bare"), null);
  const folderOnly = [node("srv", null), node("app", "srv", { is_project_root: true })];
  assert.equal(firstScreenOf(folderOnly, "srv"), null);
});

test("screenOrSelf: a screen is itself, a header hands over, an empty header is null", () => {
  assert.equal(screenOrSelf(tree, "wt"), "wt");
  assert.equal(screenOrSelf(tree, "srv"), "shell");
  assert.equal(screenOrSelf(tree, "app"), "app-shell");
  assert.equal(screenOrSelf(tree, "bare"), null);
  assert.equal(screenOrSelf(tree, "nope"), null);
});
