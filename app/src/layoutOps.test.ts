// Unit tests for the browser host's layout operations. Run:
//   cd app && node --experimental-strip-types --test src/layoutOps.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
//
// Each case mirrors a Rust original in lib.rs, so a browser workspace and a
// desktop workspace change shape the same way under the same gesture.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  closeLeaf,
  leafIds,
  makeLeaf,
  patchLeaf,
  resetRatios,
  setRatio,
  splitLeaf,
  swapLeaves,
} from "./layoutOps.ts";
import type { LayoutNode } from "./bindings/LayoutNode";

const two = (): LayoutNode => ({
  kind: "split", split_id: "s1", direction: "horizontal", ratio: 0.3,
  first: makeLeaf("a"), second: makeLeaf("b"),
});

test("split puts the original first and the new leaf second, ratio 0.5", () => {
  const out = splitLeaf(makeLeaf("a"), "a", "vertical", makeLeaf("n"));
  assert.ok(out && out.kind === "split");
  assert.equal(out.direction, "vertical");
  assert.equal(out.ratio, 0.5);
  assert.deepEqual(leafIds(out), ["a", "n"]);
  assert.match(out.split_id, /^sp_[0-9a-f]+_[0-9a-f]+$/);
});

test("split of a nested leaf keeps the rest of the tree", () => {
  const out = splitLeaf(two(), "b", "horizontal", makeLeaf("n"));
  assert.ok(out);
  assert.deepEqual(leafIds(out), ["a", "b", "n"]);
  assert.equal(out.kind === "split" && out.ratio, 0.3);
});

test("split of a missing pane is null", () => {
  assert.equal(splitLeaf(two(), "zz", "horizontal", makeLeaf("n")), null);
});

test("close collapses the parent into the sibling; the last leaf stays", () => {
  const r = closeLeaf(two(), "a");
  assert.ok(r.removed);
  assert.deepEqual(r.node, makeLeaf("b"));
  const last = closeLeaf(makeLeaf("a"), "a");
  assert.equal(last.removed, false);
});

test("close inside a nested split", () => {
  const tree = splitLeaf(two(), "b", "vertical", makeLeaf("c"));
  assert.ok(tree);
  const r = closeLeaf(tree, "c");
  assert.ok(r.removed);
  assert.deepEqual(leafIds(r.node), ["a", "b"]);
});

test("ratio is clamped like set_split_ratio_in", () => {
  const n = setRatio(two(), "s1", 2);
  assert.equal(n.kind === "split" && n.ratio, 0.95);
  const m = setRatio(two(), "s1", -1);
  assert.equal(m.kind === "split" && m.ratio, 0.05);
});

test("reset puts every split back to 0.5", () => {
  const tree = splitLeaf(two(), "b", "vertical", makeLeaf("c"));
  assert.ok(tree);
  const out = resetRatios(setRatio(tree, "s1", 0.2));
  const ratios: number[] = [];
  const walk = (n: LayoutNode) => { if (n.kind === "split") { ratios.push(n.ratio); walk(n.first); walk(n.second); } };
  walk(out);
  assert.deepEqual(ratios, [0.5, 0.5]);
});

test("swap two leaves in a split (swap_two_leaves_in_a_split)", () => {
  assert.deepEqual(leafIds(swapLeaves(two(), "a", "b")), ["b", "a"]);
});

test("swap across nested splits keeps the whole leaf", () => {
  const tree = splitLeaf(two(), "b", "vertical", { ...makeLeaf("c"), title: "server" });
  assert.ok(tree);
  const out = swapLeaves(tree, "a", "c");
  assert.deepEqual(leafIds(out), ["c", "b", "a"]);
  assert.equal(out.kind === "split" && out.first.kind === "pane" && out.first.title, "server");
});

test("swap with a missing pane throws; same pane is a no-op", () => {
  assert.throws(() => swapLeaves(two(), "a", "zz"));
  assert.deepEqual(swapLeaves(two(), "a", "a"), two());
});

test("patchLeaf sets a title on one leaf only", () => {
  const out = patchLeaf(two(), "b", { title: "logs" });
  assert.equal(out.kind === "split" && out.second.kind === "pane" && out.second.title, "logs");
  assert.equal(out.kind === "split" && out.first.kind === "pane" && out.first.title, null);
});
