// Unit tests for the browser's port of the desktop workspace tree (Phase 115,
// F1 — backend/web/tree.ts mirrors lib.rs). Run:
//   cd app && node --experimental-strip-types --test src/webTree.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import type { Workspace, WorkspaceGroup } from "./types.ts";
import {
  activeAfterDelete,
  checkIdentity,
  checkPin,
  folderLabel,
  pickSessionParent,
  reorder,
  reorderGroups,
  subtreeIds,
  uniqueSiblingName,
} from "./backend/web/tree.ts";

const ws = (id: string, parent: string | null, extra: Partial<Workspace> = {}): Workspace =>
  ({
    id,
    name: id,
    parent_id: parent,
    is_project_root: false,
    is_folder: false,
    sort_order: null,
    group_id: null,
    cwd: null,
    layout: null,
    ...extra,
  }) as Workspace;

// srv → shell, app (git folder /srv/app) → app-shell ; docs (folder, no git)
// other (root, group g1) → other-shell
const tree = (): Workspace[] => [
  ws("srv", null),
  ws("shell", "srv", { layout: { kind: "terminal" } as Workspace["layout"] }),
  ws("app", "srv", { is_folder: true, is_project_root: true, cwd: "/srv/app" }),
  ws("app-shell", "app", { cwd: "/srv/app", layout: { kind: "terminal" } as Workspace["layout"] }),
  ws("docs", "srv", { is_folder: true, cwd: "/srv/docs" }),
  ws("other", null, { group_id: "g1" }),
  ws("other-shell", "other", { layout: { kind: "terminal" } as Workspace["layout"] }),
];
const groups: WorkspaceGroup[] = [{ id: "g1", name: "work", color: "#ff0000", is_collapsed: false, sort_order: null }];

test("unique sibling names count up from -2", () => {
  const all = [...tree(), ws("s2", "srv", { name: "shell" }), ws("s3", "srv", { name: "shell-2" })];
  assert.equal(uniqueSiblingName(all, "srv", "shell"), "shell-3");
  assert.equal(uniqueSiblingName(all, "app", "shell"), "shell");
});

test("a pin's label is the name, else the last path component", () => {
  assert.equal(folderLabel("/srv/app/", null), "app");
  assert.equal(folderLabel("C:\\code\\x", ""), "x");
  assert.equal(folderLabel("/", "  "), "/");
  assert.equal(folderLabel("/srv/app", " api "), "api");
});

test("pin validations carry the desktop's messages", () => {
  const all = tree();
  assert.throws(() => checkPin(all, "srv", "  "), /project path is required/);
  assert.throws(() => checkPin(all, "nope", "/x"), /workspace not found/);
  assert.throws(() => checkPin(all, "app", "/srv/app/sub"), /already inside a project folder/);
  assert.throws(() => checkPin(all, "app-shell", "/x"), /already inside a project folder/);
  assert.throws(() => checkPin(all, "srv", "/srv/docs"), /already pinned here/);
  assert.equal(checkPin(all, "srv", " /srv/new "), "/srv/new");
  assert.equal(checkPin(all, "docs", "/srv/docs/a"), "/srv/docs/a"); // no git above: allowed
});

test("a child reorders among its siblings only", () => {
  const ch = reorder(tree(), groups, "docs", "g1", 0);
  assert.deepEqual(Object.fromEntries(ch), {
    docs: { sort_order: 0 },
    shell: { sort_order: 1 },
    app: { sort_order: 2 },
  });
});

test("a root moved into a group joins it; the scope it left renumbers", () => {
  const all = [...tree(), ws("third", null)];
  const ch = reorder(all, groups, "srv", "g1", 1);
  assert.deepEqual(ch.get("srv"), { sort_order: 1, group_id: "g1" });
  assert.deepEqual(ch.get("other"), { sort_order: 0 });
  assert.deepEqual(ch.get("third"), { sort_order: 0 });
  assert.throws(() => reorder(all, groups, "srv", "nope", 0), /no group nope/);
});

test("groups reorder to 0..N-1", () => {
  const gs = [...groups, { ...groups[0], id: "g2" }, { ...groups[0], id: "g3" }];
  assert.deepEqual(
    reorderGroups(gs, "g3", 0).map((g) => [g.id, g.sort_order]),
    [["g3", 0], ["g1", 1], ["g2", 2]],
  );
});

test("a delete takes the subtree; activation lands on a sibling screen, then the root's", () => {
  const all = tree();
  assert.deepEqual(subtreeIds(all, "srv").sort(), ["app", "app-shell", "docs", "shell", "srv"]);
  const rest = all.filter((w) => w.id !== "app" && w.id !== "app-shell");
  assert.equal(activeAfterDelete(rest, "srv"), "shell");
  const noSib = all.filter((w) => w.id !== "app-shell");
  assert.equal(activeAfterDelete(noSib, "app"), "shell");
  assert.equal(activeAfterDelete(all.filter((w) => !subtreeIds(all, "srv").includes(w.id)), null), "other-shell");
});

test("a session lands under the deepest git folder containing its cwd", () => {
  const all = tree();
  assert.equal(pickSessionParent(all, "srv", "/srv/app/src"), "app");
  assert.equal(pickSessionParent(all, "srv", "/srv/app"), "app");
  assert.equal(pickSessionParent(all, "srv", "/srv/application"), "srv");
  assert.equal(pickSessionParent(all, "srv", "/srv/docs/x"), "srv"); // not a project root
  assert.equal(pickSessionParent(all, "other", "/srv/app"), "other"); // another root's folder
  assert.equal(pickSessionParent(all, "srv", null), "srv");
});

test("identity: #rrggbb and at most 16 bytes", () => {
  checkIdentity("#a1B2c3", "🚀");
  checkIdentity(null, null);
  assert.throws(() => checkIdentity("red", null), /#rrggbb/);
  assert.throws(() => checkIdentity(null, "🚀🚀🚀🚀🚀"), /too long/);
});
