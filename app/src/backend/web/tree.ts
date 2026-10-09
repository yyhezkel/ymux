// The desktop's workspace tree, for the browser backend (Phase 115, F1).
//
// Pure helpers ported from lib.rs so web.ts gives the sidebar the same tree
// the desktop does: a root header, pinned project folders under it, and
// screens (the rows that hold panes) under those. The rules — names, the
// pin validations, sort order, where activation lands — are lib.rs's, cited
// per function; wsTree.ts already holds the read-side ones (isHeader,
// childrenInOrder, firstScreenOf) and is reused, not copied.

import type { Workspace, WorkspaceGroup } from "../../types";
import { ancestorsOf, isHeader } from "../../wsTree.ts";

/** lib.rs DEFAULT_SCREEN_NAME. */
export const DEFAULT_SCREEN_NAME = "shell";

/** lib.rs unique_sibling_name: base, base-2, base-3 … among parent's children. */
export function uniqueSiblingName(all: Workspace[], parentId: string, base: string): string {
  const taken = new Set(all.filter((w) => w.parent_id === parentId).map((w) => w.name));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) {
    const name = `${base}-${n}`;
    if (!taken.has(name)) return name;
  }
}

/** The pin's label: the given name, else the path's last component, else the path. */
export function folderLabel(path: string, name: string | null | undefined): string {
  const n = (name ?? "").trim();
  if (n) return n;
  const last = path.split(/[\\/]/).filter(Boolean).pop();
  return last || path;
}

/**
 * workspace_pin_project_folder's validations (lib.rs:6253). Returns the
 * trimmed path, or throws the desktop's message.
 */
export function checkPin(all: Workspace[], parentId: string, path: string): string {
  const p = path.trim();
  if (!p) throw new Error("project path is required");
  const parent = all.find((w) => w.id === parentId);
  if (!parent) throw new Error("workspace not found");
  if (parent.is_project_root || ancestorsOf(all, parentId).some((a) => a.is_project_root)) {
    throw new Error("this workspace is already inside a project folder");
  }
  if (all.some((w) => w.parent_id === parentId && w.cwd === p)) {
    throw new Error("this folder is already pinned here");
  }
  return p;
}

/** The order key every reorder uses: (sort_order ?? MAX, array index). */
function byOrder<T extends { sort_order: number | null }>(rows: T[], all: T[]): T[] {
  const idx = new Map(all.map((r, i) => [r, i]));
  return [...rows].sort(
    (a, b) =>
      (a.sort_order ?? Number.MAX_SAFE_INTEGER) - (b.sort_order ?? Number.MAX_SAFE_INTEGER) ||
      (idx.get(a) ?? 0) - (idx.get(b) ?? 0),
  );
}

/**
 * workspace_reorder (lib.rs:7562): the new sort_order (and group) of every
 * row whose value changes. A child's scope is its siblings and it keeps its
 * group; a root's scope is the roots of `groupId`, it joins that group, and
 * the scope it left is renumbered densely.
 */
export function reorder(
  all: Workspace[],
  groups: WorkspaceGroup[],
  id: string,
  groupId: string | null,
  newIndex: number,
): Map<string, Partial<Workspace>> {
  const w = all.find((x) => x.id === id);
  if (!w) throw new Error(`no workspace ${id}`);
  const out = new Map<string, Partial<Workspace>>();
  const set = (row: Workspace, patch: Partial<Workspace>) => {
    const changed = Object.entries(patch).some(([k, v]) => row[k as keyof Workspace] !== v);
    if (changed) out.set(row.id, { ...(out.get(row.id) ?? {}), ...patch });
  };
  const place = (scope: Workspace[], at: number) => {
    const rest = byOrder(scope.filter((x) => x.id !== id), all);
    rest.splice(Math.max(0, Math.min(at, rest.length)), 0, w);
    rest.forEach((x, i) => set(x, { sort_order: i }));
  };
  if (w.parent_id) {
    place(all.filter((x) => x.parent_id === w.parent_id), newIndex);
    return out;
  }
  if (groupId !== null && !groups.some((g) => g.id === groupId)) throw new Error(`no group ${groupId}`);
  const from = w.group_id;
  place(all.filter((x) => !x.parent_id && (x.id === id || x.group_id === groupId)), newIndex);
  set(w, { group_id: groupId });
  if (from !== groupId) {
    byOrder(all.filter((x) => !x.parent_id && x.group_id === from && x.id !== id), all).forEach((x, i) =>
      set(x, { sort_order: i }),
    );
  }
  return out;
}

/** workspace_group_reorder (lib.rs:7668): groups with sort_order 0..N-1. */
export function reorderGroups(groups: WorkspaceGroup[], id: string, newIndex: number): WorkspaceGroup[] {
  const g = groups.find((x) => x.id === id);
  if (!g) return groups;
  const rest = byOrder(groups.filter((x) => x.id !== id), groups);
  rest.splice(Math.max(0, Math.min(newIndex, rest.length)), 0, g);
  return rest.map((x, i) => ({ ...x, sort_order: i }));
}

/** Every id in id's subtree, itself first (lib.rs collect_subtree_ids). */
export function subtreeIds(all: Workspace[], id: string): string[] {
  const out = [id];
  const seen = new Set(out);
  for (let i = 0; i < out.length; i++) {
    for (const w of all) {
      if (w.parent_id === out[i] && !seen.has(w.id)) {
        seen.add(w.id);
        out.push(w.id);
      }
    }
  }
  return out;
}

/**
 * Where activation lands after a delete (lib.rs active_after_delete): a
 * screen under the deleted row's parent first, then any screen under the
 * same root, then any screen at all. A header is never active.
 */
export function activeAfterDelete(rest: Workspace[], deletedParent: string | null): string | null {
  const screens = rest.filter((w) => !isHeader(w));
  if (deletedParent) {
    const sib = screens.find((w) => w.parent_id === deletedParent);
    if (sib) return sib.id;
    const chain = [deletedParent, ...ancestorsOf(rest, deletedParent).map((a) => a.id)];
    const root = chain[chain.length - 1];
    const under = screens.find((w) => w.id === root || ancestorsOf(rest, w.id).some((a) => a.id === root));
    if (under) return under.id;
  }
  return screens[0]?.id ?? null;
}

/**
 * lib.rs pick_session_parent: the deepest project folder under root whose cwd
 * contains `cwd` on a separator boundary, else the root.
 */
export function pickSessionParent(all: Workspace[], rootId: string, cwd: string | null): string {
  if (!cwd) return rootId;
  const norm = (p: string) => p.replace(/[\\/]+$/, "");
  const c = norm(cwd);
  let best: { id: string; depth: number } | null = null;
  for (const w of all) {
    if (!w.is_project_root || !w.cwd) continue;
    const anc = ancestorsOf(all, w.id);
    if (w.id !== rootId && !anc.some((a) => a.id === rootId)) continue;
    const base = norm(w.cwd);
    if (c !== base && !c.startsWith(base + "/") && !c.startsWith(base + "\\")) continue;
    if (!best || anc.length > best.depth) best = { id: w.id, depth: anc.length };
  }
  return best?.id ?? rootId;
}

/** workspace_set_identity's checks: #rrggbb and ≤16 UTF-8 bytes. */
export function checkIdentity(color: string | null, emoji: string | null): void {
  if (color !== null && !/^#[0-9a-fA-F]{6}$/.test(color)) throw new Error("color must be #rrggbb");
  if (emoji !== null && new TextEncoder().encode(emoji).length > 16) throw new Error("emoji is too long");
}
