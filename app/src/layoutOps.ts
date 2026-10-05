// Layout-tree operations for the browser host (Phase 109, WEB-DESIGN §5).
//
// On the desktop these live in Rust (`lib.rs`: split_pane_in, close_pane_in,
// set_split_ratio_in, swap_two_panes_in_layout, reset_all_split_ratios) and
// the frontend only ever receives the result. A browser has no Rust, so the
// WebBackend runs the same operations here and writes the tree back to the
// daemon. Pure and Solid-free on purpose — `layoutOps.test.ts` pins the
// semantics against the Rust originals.

import type { LayoutNode } from "./bindings/LayoutNode";
import type { PaneKind } from "./bindings/PaneKind";
import type { SplitDirection } from "./bindings/SplitDirection";

export type PaneLeaf = LayoutNode & { kind: "pane" };

let counter = 0;
const stamp = (): string =>
  `${(Date.now() * 1000 + Math.floor(Math.random() * 1000)).toString(16)}_${(counter++).toString(16)}`;

/** Same shape as Rust's `new_pane_id` (`p_<hex>_<hex>`). */
export const newPaneId = (): string => `p_${stamp()}`;
/** Same shape as Rust's `new_split_id`. */
export const newSplitId = (): string => `sp_${stamp()}`;

/** A fresh leaf with every optional field explicitly null, as serde writes it. */
export function makeLeaf(paneId: string, kind: PaneKind = "terminal"): PaneLeaf {
  return {
    kind: "pane",
    pane_id: paneId,
    pane_kind: kind,
    connection: null,
    browser: null,
    title: null,
    auto_title: null,
    annotation: null,
    color: null,
    emoji: null,
    help_topic: kind === "help" ? "ssh-key-setup" : null,
    diff_source: kind === "diff" ? { kind: "head" } : null,
    smart_bidi: null,
    diff_cwd: null,
  };
}

export function findLeaf(node: LayoutNode | null, paneId: string): PaneLeaf | null {
  if (!node) return null;
  if (node.kind === "pane") return node.pane_id === paneId ? node : null;
  return findLeaf(node.first, paneId) ?? findLeaf(node.second, paneId);
}

/**
 * Rust `split_pane_in`: the target leaf becomes a split whose first child is
 * the original and whose second is `newLeaf`, ratio 0.5. Returns null when
 * the target is not in the tree.
 */
export function splitLeaf(
  node: LayoutNode,
  target: string,
  direction: SplitDirection,
  newLeaf: PaneLeaf,
): LayoutNode | null {
  if (node.kind === "pane") {
    if (node.pane_id !== target) return null;
    return { kind: "split", split_id: newSplitId(), direction, first: node, second: newLeaf, ratio: 0.5 };
  }
  const first = splitLeaf(node.first, target, direction, newLeaf);
  if (first) return { ...node, first };
  const second = splitLeaf(node.second, target, direction, newLeaf);
  if (second) return { ...node, second };
  return null;
}

/**
 * Rust `close_pane_in`: removing a leaf collapses its parent split into the
 * sibling. The last leaf of a tree cannot be removed (returned unchanged,
 * `removed: false`), exactly like the desktop.
 */
export function closeLeaf(node: LayoutNode, target: string): { node: LayoutNode; removed: boolean } {
  if (node.kind === "pane") return { node, removed: false };
  if (node.first.kind === "pane" && node.first.pane_id === target) return { node: node.second, removed: true };
  if (node.second.kind === "pane" && node.second.pane_id === target) return { node: node.first, removed: true };
  const a = closeLeaf(node.first, target);
  if (a.removed) return { node: { ...node, first: a.node }, removed: true };
  const b = closeLeaf(node.second, target);
  if (b.removed) return { node: { ...node, second: b.node }, removed: true };
  return { node, removed: false };
}

/** Rust `set_split_ratio_in`: clamp to [0.05, 0.95]. */
export function setRatio(node: LayoutNode, splitId: string, ratio: number): LayoutNode {
  if (node.kind === "pane") return node;
  if (node.split_id === splitId) return { ...node, ratio: Math.min(0.95, Math.max(0.05, ratio)) };
  return { ...node, first: setRatio(node.first, splitId, ratio), second: setRatio(node.second, splitId, ratio) };
}

/** Rust `reset_all_split_ratios`: every split back to 0.5. */
export function resetRatios(node: LayoutNode): LayoutNode {
  if (node.kind === "pane") return node;
  return { ...node, ratio: 0.5, first: resetRatios(node.first), second: resetRatios(node.second) };
}

/** Rust `swap_two_panes_in_layout`: two leaves trade places, whole. */
export function swapLeaves(node: LayoutNode, a: string, b: string): LayoutNode {
  if (a === b) return node;
  const la = findLeaf(node, a);
  const lb = findLeaf(node, b);
  if (!la) throw new Error(`no pane ${a} in workspace layout`);
  if (!lb) throw new Error(`no pane ${b} in workspace layout`);
  const walk = (n: LayoutNode): LayoutNode => {
    if (n.kind === "pane") return n.pane_id === a ? lb : n.pane_id === b ? la : n;
    return { ...n, first: walk(n.first), second: walk(n.second) };
  };
  return walk(node);
}

/** Apply `patch` to one leaf (title, annotation, …). Unchanged tree when absent. */
export function patchLeaf(node: LayoutNode, paneId: string, patch: Partial<PaneLeaf>): LayoutNode {
  if (node.kind === "pane") return node.pane_id === paneId ? ({ ...node, ...patch, kind: "pane" } as PaneLeaf) : node;
  return { ...node, first: patchLeaf(node.first, paneId, patch), second: patchLeaf(node.second, paneId, patch) };
}

export function leafIds(node: LayoutNode | null): string[] {
  if (!node) return [];
  if (node.kind === "pane") return [node.pane_id];
  return [...leafIds(node.first), ...leafIds(node.second)];
}
