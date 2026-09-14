// Phase 92: headers vs screens — the workspace tree's one rule, shared by
// the sidebar and App.
//
// A ROOT (the machine) or a pinned project folder is a HEADER: a row that
// holds rows and never holds panes (`layout: null`). Every other workspace
// is a SCREEN — the only kind with a layout, the only kind that can be
// active. Derived, not stored: `parent_id` and `is_project_root` already
// say everything. Mirrors `is_header` in `app/src-tauri/src/lib.rs`.
//
// Pure and dependency-free on purpose, so it runs under the bare-node
// `npm test` glob like `cwdShort.ts` and `queueModel.ts`.

/** The fields the rule reads — a full `Workspace` satisfies it. */
export interface TreeNode {
  id: string;
  parent_id: string | null;
  is_project_root: boolean;
  sort_order: number | null;
}

export function isHeader(w: Pick<TreeNode, "parent_id" | "is_project_root">): boolean {
  return !w.parent_id || w.is_project_root;
}

/** Ancestors of `id`, nearest first. Hop-capped so a cycle cannot spin. */
export function ancestorsOf<T extends TreeNode>(all: readonly T[], id: string): T[] {
  const out: T[] = [];
  let cur = all.find((w) => w.id === id);
  let hops = 0;
  while (cur?.parent_id && hops < all.length) {
    const parent = all.find((w) => w.id === cur?.parent_id);
    if (!parent) break;
    out.push(parent);
    cur = parent;
    hops++;
  }
  return out;
}

/** The root above `id` — the last ancestor — or `id` itself. */
export function rootIdOf(all: readonly TreeNode[], id: string): string {
  const chain = ancestorsOf(all, id);
  return chain.length > 0 ? chain[chain.length - 1].id : id;
}

/**
 * Children of `parentId` in sidebar order: `sort_order` ascending, null
 * last, insertion order as the tie-break — the same sort `childrenOf` in
 * Sidebar.tsx and `children_in_order` in lib.rs use.
 */
export function childrenInOrder<T extends TreeNode>(all: readonly T[], parentId: string): T[] {
  const kids: { w: T; ins: number }[] = [];
  all.forEach((w, ins) => {
    if (w.parent_id === parentId) kids.push({ w, ins });
  });
  kids.sort((a, b) => {
    const ao = a.w.sort_order ?? Number.MAX_SAFE_INTEGER;
    const bo = b.w.sort_order ?? Number.MAX_SAFE_INTEGER;
    return ao !== bo ? ao - bo : a.ins - b.ins;
  });
  return kids.map((k) => k.w);
}

/**
 * The first SCREEN under a header. A root's pinned-folder children are
 * headers themselves and are skipped. `null` when there is none yet.
 */
export function firstScreenOf(all: readonly TreeNode[], headerId: string): string | null {
  return childrenInOrder(all, headerId).find((w) => !isHeader(w))?.id ?? null;
}

/**
 * The id to activate for any id: a screen is itself; a header hands over
 * to its first screen; a header with no screens (or an unknown id) is
 * `null` — nothing to show.
 */
export function screenOrSelf(all: readonly TreeNode[], id: string): string | null {
  const w = all.find((x) => x.id === id);
  if (!w) return null;
  return isHeader(w) ? firstScreenOf(all, id) : id;
}
