import { For, Show, createEffect, createSignal, createMemo, onCleanup, onMount } from "solid-js";
import { collectPanes, findPane, isRemoteConn, type Workspace, type WorkspaceGroup, type WorktreeEntry, type ForwardRow } from "./types";
import { t } from "./i18n";
import { TechText } from "./TechText";
import {
  IconNotes,
  IconSettings,
  IconGlobe,
  IconGitBranch,
  IconPlus,
  IconChevronDown,
  IconChevronRight,
  IconFolder,
  IconTerminal,
  IconRefresh,
  IconWarning,
} from "./icons";
import type { SidebarMode } from "./settings";
import { createLogger } from "./logger";

const log = createLogger("SIDEBAR");

// cmux-A A2: eight-color palette for workspace group swatches. Kept
// intentionally small so a group's dot in the sidebar is easy to
// recognize at a glance. Values are theme-neutral hexes that read on
// both dark and light backgrounds.
export const GROUP_COLORS = [
  "#e0af68", // amber
  "#7aa2f7", // blue
  "#9ece6a", // green
  "#bb9af7", // purple
  "#f7768e", // pink
  "#f7768e", // red (shares hex with pink — kept for palette label parity)
  "#7dcfff", // cyan
  "#a0a8b2", // gray
];
// Re-export a de-duplicated list for the picker (the two red/pink slots
// use the same accent, so the picker shows seven visible tiles).
const GROUP_PICKER_COLORS = [
  "#e0af68",
  "#7aa2f7",
  "#9ece6a",
  "#bb9af7",
  "#f7768e",
  "#e06c75",
  "#7dcfff",
  "#a0a8b2",
];

function workspaceBadge(w: Workspace): { label: string; cls: string; title: string } {
  if (!w.layout) {
    if (isRemoteConn(w.connection)) return { label: "S", cls: "ssh", title: "SSH" };
    return { label: "L", cls: "local", title: "Local" };
  }
  const panes = collectPanes(w.layout);
  if (panes.length > 1) return { label: `${panes.length}`, cls: "split", title: `${panes.length} panes` };
  const first = findPane(w.layout, panes[0]);
  if (first?.pane_kind === "browser") return { label: "B", cls: "browser", title: "Browser" };
  if (first?.pane_kind === "filemanager") return { label: "F", cls: "filemanager", title: "File manager" };
  if (isRemoteConn(first?.connection)) return { label: "S", cls: "ssh", title: "SSH" };
  return { label: "L", cls: "local", title: "Local" };
}

interface Props {
  workspaces: Workspace[];
  activeId: string | null;
  connectedIds: Set<string>;
  // Phase 26: workspaces that contain at least one pane with a
  // pending blocking permission request. Renders a pulsing dot on
  // the workspace row so the user can spot waiting work across
  // workspaces.
  waitingWorkspaceIds: Set<string>;
  // beta.3 Fix 4: workspaces that received a passive hook in the last 4s.
  // Renders a soft amber breathing pulse on the row — attention-grabbing
  // but not blocking (`waitingWorkspaceIds` is the blocking red dot).
  hookPulseWorkspaceIds?: Set<string>;
  // BRIEF: workspaces holding a pane whose brief says "needs you" (stuck /
  // waiting-for-you) without a live blocking card. Shares the row's ONE
  // attention dot at a middle intensity: blocking > brief > activity.
  briefAttentionWorkspaceIds?: Set<string>;
  onActivate: (id: string) => void;
  /** Phase 80 — opens the unified SetupWizard (server new/existing +
   *  local existing/smart-install all live behind this one button; the
   *  old separate "provision server" button/prop is gone). */
  onCreate: () => void;
  /** Phase 38 — open the settings modal from the sidebar gear. */
  onOpenSettings: () => void;
  /** Phase 39 — open the notes window from the sidebar. */
  onOpenNotes: () => void;
  onAction: (
    id: string,
    action:
      | "rename"
      | "edit"
      | "delete"
      | "disconnect"
      | "sessions"
      | "addons"
      | "add_project_folder"
      | "check_git",
  ) => void;
  // Project folders are workspaces now (`is_project_root`), nested
  // under whatever workspace they were pinned from. The Sidebar owns
  // the worktree scan cache — lazy and never polled, because a scan is
  // a round-trip over that workspace's connection; the App owns
  // persistence and workspace creation.
  onSetCollapsed: (workspaceId: string, isCollapsed: boolean) => void;
  onNewWorktree: (w: Workspace) => void;
  /** List a project-folder workspace's worktrees. Rejects with no live session. */
  onListWorktrees: (workspaceId: string) => Promise<WorktreeEntry[]>;
  /** Open a worktree that has no workspace yet as a child of the root. */
  onOpenWorktree: (rootWorkspaceId: string, wt: WorktreeEntry) => void;
  /** git says this directory is not a repo — stop treating it as one. */
  onNotARepo: (workspaceId: string) => void;
  // Phase 36.A / 39: all forwards across workspaces, for the per-
  // workspace inline 🌐 badge. Clicking the badge opens the Ports
  // window scoped to that workspace.
  allForwards: ForwardRow[];
  onOpenPorts: (workspaceId: string) => void;
  // Phase 39.A: global Ports button (opens the window on the "All
  // workspaces" tab, no workspace context).
  onOpenPortsGlobal: () => void;
  // Phase 60: onOpenBrowser / onOpenFiles props removed — the
  // buttons moved to the workspace header (App.tsx, next to + diff).
  // Phase 62.B (item I) / 65.P: two-mode sidebar — full / icons.
  mode: SidebarMode;
  onSetMode: (mode: SidebarMode) => void;
  // Live pixel width of the rail. `full` mode is resizable down to 160px,
  // where there is no room for the whole status cluster; the CSS drops the
  // least urgent markers off `[data-narrow]` rather than letting them
  // squeeze the name. Container queries would do this without the prop, but
  // WebView2's floor on older Win10 installs isn't guaranteed to have them.
  widthPx: number;
  // workspace_ids holding a pane with a pending OSC 9/99/777 activity
  // notification. Was an aggregate count in the masthead — a bare number in a
  // <span> styled like a button that did nothing on click, and no way to tell
  // WHICH workspace wanted you. It shares the row's one attention dot with
  // `waitingWorkspaceIds`, at a lower intensity: blocking outranks activity.
  notifiedWorkspaceIds: Set<string>;
  // cmux-A A2: workspace groups (collapsible sidebar sections). The
  // parent App owns the list — Sidebar renders it + delegates the
  // create/rename/color/delete/collapse actions back up.
  groups: WorkspaceGroup[];
  // Returns the created group (or null on failure) so the caller can
  // chain a workspace assignment — creation lives in the workspace
  // context menu ("Move to group ▸" → "New group…") and always
  // creates + assigns in one step.
  onGroupCreate: (name: string, color: string) => Promise<WorkspaceGroup | null>;
  onGroupRename: (id: string, name: string) => void;
  onGroupSetColor: (id: string, color: string) => void;
  onGroupToggleCollapse: (id: string, isCollapsed: boolean) => void;
  onGroupDelete: (id: string) => void;
  onWorkspaceSetGroup: (workspaceId: string, groupId: string | null) => void;
  // beta.3 (ws-dragdrop): direct drag-reorder. `newIndex` is a 0-based
  // slot within the destination scope (workspaces) or the group list.
  // The App handler wraps these to call `workspace_reorder` /
  // `workspace_group_reorder` and reload the workspaces file.
  onWorkspaceReorder: (
    workspaceId: string,
    groupId: string | null,
    newIndex: number,
  ) => void;
  onGroupReorder: (groupId: string, newIndex: number) => void;
}

export function Sidebar(p: Props) {
  const [menuFor, setMenuFor] = createSignal<string | null>(null);
  // Phase 65 (bug 4.4): the context menu must escape the sidebar's
  // scroll container. `.sidebar-list` has `overflow-y:auto`, which CSS
  // coerces `overflow-x` to non-visible too, so an absolutely-positioned
  // menu gets clipped at the (narrow, in icons mode) sidebar edge. We
  // render it `position:fixed` at the cursor instead, anchored here.
  const [menuPos, setMenuPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 });
  const [groupMenuFor, setGroupMenuFor] = createSignal<string | null>(null);
  const [groupMenuPos, setGroupMenuPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 });
  const [moveMenuFor, setMoveMenuFor] = createSignal<string | null>(null);
  const [newGroupName, setNewGroupName] = createSignal<string | null>(null);
  const [renamingGroup, setRenamingGroup] = createSignal<{ id: string; name: string } | null>(null);
  const [colorPickerFor, setColorPickerFor] = createSignal<string | null>(null);

  // beta.3 (ws-dragdrop): within each bucket workspaces sort by
  // `sort_order` ascending (nulls → end, ties broken by insertion
  // order). A pre-beta.3 workspaces.json has no sort_order at all —
  // that path reduces to the previous insertion-order rendering.
  const groupedWorkspaces = createMemo(() => {
    const validGroupIds = new Set(p.groups.map((g) => g.id));
    // Only ROOTS bucket into groups; a child is rendered by its parent's
    // subtree. The `validIds` guard mirrors `validGroupIds`: a workspace
    // whose parent no longer exists is treated as a root rather than
    // vanishing from the sidebar entirely. The backend repairs those at
    // load, so this is the belt to that suspenders — the v2 version of
    // this line had no such guard and could hide rows outright.
    const validIds = new Set(p.workspaces.map((w) => w.id));
    const ungrouped: { w: Workspace; ins: number }[] = [];
    const byGroup = new Map<string, { w: Workspace; ins: number }[]>();
    p.workspaces.forEach((w, ins) => {
      if (w.parent_id && validIds.has(w.parent_id)) return;
      const gid = w.group_id;
      if (gid && validGroupIds.has(gid)) {
        const list = byGroup.get(gid) ?? [];
        list.push({ w, ins });
        byGroup.set(gid, list);
      } else {
        ungrouped.push({ w, ins });
      }
    });
    const cmp = (a: { w: Workspace; ins: number }, b: { w: Workspace; ins: number }) => {
      const ao = a.w.sort_order ?? Number.MAX_SAFE_INTEGER;
      const bo = b.w.sort_order ?? Number.MAX_SAFE_INTEGER;
      if (ao !== bo) return ao - bo;
      return a.ins - b.ins;
    };
    ungrouped.sort(cmp);
    for (const list of byGroup.values()) list.sort(cmp);
    return {
      ungrouped: ungrouped.map((x) => x.w),
      byGroup: new Map(
        Array.from(byGroup.entries()).map(([k, v]) => [k, v.map((x) => x.w)]),
      ),
    };
  });

  // beta.3 (ws-dragdrop): render groups in sort_order (nulls → end).
  const sortedGroups = createMemo(() => {
    const arr = p.groups.map((g, ins) => ({ g, ins }));
    arr.sort((a, b) => {
      const ao = a.g.sort_order ?? Number.MAX_SAFE_INTEGER;
      const bo = b.g.sort_order ?? Number.MAX_SAFE_INTEGER;
      if (ao !== bo) return ao - bo;
      return a.ins - b.ins;
    });
    return arr.map((x) => x.g);
  });

  // beta.3 (ws-dragdrop): pointer-based drag-reorder.
  //
  // HTML5 drag-and-drop can't be used here: Tauri's WebView2 OS drop handler
  // stays enabled so Phase 49-A file drops onto terminal panes keep working,
  // and on Windows that handler swallows in-page HTML5 drags (the drop event
  // never fires — a cursor showed but nothing moved). So reorder is driven by
  // pointer events instead. A small move threshold separates a click
  // (switch / collapse) from a drag; the drop target is found by hit-testing
  // the DOM under the cursor. `newIndex` in the reorder callbacks is a 0-based
  // index within the destination scope; the App handler wraps these to call
  // `workspace_reorder` / `workspace_group_reorder` and reload the file.
  type Drop =
    | { kind: "ws-line"; targetId: string; where: "above" | "below" }
    | { kind: "group-line"; targetId: string; where: "above" | "below" }
    | { kind: "into-group"; targetId: string | null }
    | null;
  const [dragKind, setDragKind] = createSignal<"ws" | "group" | null>(null);
  const [dragId, setDragId] = createSignal<string | null>(null);
  const [drop, setDrop] = createSignal<Drop>(null);
  const [ghostPos, setGhostPos] = createSignal<{ x: number; y: number } | null>(null);

  // Typed accessors for the drop indicator classes (avoids `as any` casts in
  // the JSX class strings while keeping the discriminated union narrowed).
  const dropWsWhere = (id: string): "above" | "below" | null => {
    const d = drop();
    return d && d.kind === "ws-line" && d.targetId === id ? d.where : null;
  };
  const dropGroupWhere = (id: string): "above" | "below" | null => {
    const d = drop();
    return d && d.kind === "group-line" && d.targetId === id ? d.where : null;
  };
  const dropIntoGroup = (id: string | null): boolean => {
    const d = drop();
    return !!d && d.kind === "into-group" && d.targetId === id;
  };
  // Label for the floating ghost that follows the cursor mid-drag.
  const ghostLabel = (): string => {
    const id = dragId();
    if (!id) return "";
    if (dragKind() === "ws") return p.workspaces.find((w) => w.id === id)?.name ?? "";
    if (dragKind() === "group") return p.groups.find((g) => g.id === id)?.name ?? "";
    return "";
  };

  // Non-reactive scratch state for the in-flight gesture. `pending` holds the
  // press until the move threshold is crossed; `didDrag` guards the trailing
  // click so a completed drag never also switches/collapses.
  const DRAG_THRESHOLD = 5;
  let pending: { kind: "ws" | "group"; id: string; startX: number; startY: number } | null = null;
  let didDrag = false;

  // A workspace's reorder scope is its PARENT when it has one, and its
  // group otherwise. Without the parent case a child would be renumbered
  // against workspaces it is not a sibling of.
  const scopeListOf = (wsId: string): Workspace[] => {
    const w = p.workspaces.find((x) => x.id === wsId);
    if (w?.parent_id) return childrenOf().get(w.parent_id) ?? [];
    const scope = w?.group_id ?? null;
    return scope === null
      ? groupedWorkspaces().ungrouped
      : groupedWorkspaces().byGroup.get(scope) ?? [];
  };
  const scopeIndexOf = (wsId: string): number =>
    scopeListOf(wsId).findIndex((w) => w.id === wsId);
  /** Size of a GROUP scope — only used for "drop onto a group header",
   *  which children can never reach (they have no group). */
  const groupScopeSize = (scope: string | null): number =>
    (scope === null
      ? groupedWorkspaces().ungrouped
      : groupedWorkspaces().byGroup.get(scope) ?? []).length;
  const groupIndexOf = (gid: string): number => sortedGroups().findIndex((g) => g.id === gid);
  const whereByMidpoint = (clientY: number, el: HTMLElement): "above" | "below" => {
    const r = el.getBoundingClientRect();
    return clientY < r.top + r.height / 2 ? "above" : "below";
  };

  // Resolve the cursor position to a drop target using the DOM under it.
  const updateDropTarget = (x: number, y: number) => {
    const kind = dragKind();
    const id = dragId();
    if (!kind || !id) {
      setDrop(null);
      return;
    }
    const under = document.elementFromPoint(x, y) as HTMLElement | null;
    if (!under) {
      setDrop(null);
      return;
    }
    if (kind === "ws") {
      const dragged = p.workspaces.find((w) => w.id === id);
      const wsEl = under.closest<HTMLElement>("[data-ws-id]");
      if (wsEl) {
        const targetId = wsEl.dataset.wsId ?? "";
        if (targetId === id || targetId === "") {
          setDrop(null);
          return;
        }
        // Reorder within one level only. Re-parenting is not a drag
        // gesture — a workspace belongs to the folder it was opened
        // from, and dragging one out would silently orphan a worktree.
        const target = p.workspaces.find((w) => w.id === targetId);
        if ((dragged?.parent_id ?? null) !== (target?.parent_id ?? null)) {
          setDrop(null);
          return;
        }
        setDrop({ kind: "ws-line", targetId, where: whereByMidpoint(y, wsEl) });
        return;
      }
      const gEl = under.closest<HTMLElement>("[data-group-id]");
      if (gEl) {
        // Children have no group: only roots can join one.
        if (dragged?.parent_id) {
          setDrop(null);
          return;
        }
        const raw = gEl.dataset.groupId ?? "";
        setDrop({ kind: "into-group", targetId: raw === "" ? null : raw });
        return;
      }
      setDrop(null);
    } else {
      // Dragging a group: only *other* real group headers are valid targets.
      const gEl = under.closest<HTMLElement>("[data-group-id]");
      if (gEl) {
        const raw = gEl.dataset.groupId ?? "";
        if (raw === "" || raw === id) {
          setDrop(null);
          return;
        }
        setDrop({ kind: "group-line", targetId: raw, where: whereByMidpoint(y, gEl) });
        return;
      }
      setDrop(null);
    }
  };

  const applyDrop = (kind: "ws" | "group", id: string, d: Drop) => {
    if (!d) return;
    if (kind === "ws") {
      if (d.kind === "ws-line") {
        // Drops are same-level only (updateDropTarget enforces it), so
        // source and target share one scope list and the group id below
        // is the target's own — for a child the backend ignores it and
        // renumbers among siblings instead.
        const destScope = p.workspaces.find((w) => w.id === d.targetId)?.group_id ?? null;
        const targetIdx = scopeIndexOf(d.targetId);
        const srcIdx = scopeIndexOf(id);
        let idx = d.where === "above" ? targetIdx : targetIdx + 1;
        if (srcIdx !== -1 && srcIdx < targetIdx) idx -= 1;
        p.onWorkspaceReorder(id, destScope, Math.max(0, idx));
      } else if (d.kind === "into-group") {
        p.onWorkspaceReorder(id, d.targetId, groupScopeSize(d.targetId));
      }
    } else if (d.kind === "group-line") {
      const targetIdx = groupIndexOf(d.targetId);
      const draggingIdx = groupIndexOf(id);
      let idx = d.where === "above" ? targetIdx : targetIdx + 1;
      if (draggingIdx !== -1 && draggingIdx < targetIdx) idx -= 1;
      p.onGroupReorder(id, Math.max(0, idx));
    }
  };

  const onWinPointerMove = (e: PointerEvent) => {
    if (!pending) return;
    if (!didDrag) {
      if (Math.hypot(e.clientX - pending.startX, e.clientY - pending.startY) < DRAG_THRESHOLD) {
        return;
      }
      didDrag = true;
      setDragKind(pending.kind);
      setDragId(pending.id);
      setDrop(null);
      document.body.classList.add("ymux-dragging");
    }
    setGhostPos({ x: e.clientX, y: e.clientY });
    updateDropTarget(e.clientX, e.clientY);
  };
  const onWinPointerUp = () => {
    const wasDrag = didDrag;
    const kind = dragKind();
    const id = dragId();
    const d = drop();
    endPointerGesture();
    if (wasDrag && kind && id) applyDrop(kind, id, d);
    // `didDrag` stays true until the next pointerdown resets it, so the click
    // the browser fires immediately after a drag is swallowed by the guards.
  };
  // A cancelled pointer (focus loss, touch interruption) aborts with no reorder.
  const onWinPointerCancel = () => abortDrag();
  // Tear down window listeners + transient drag UI. Leaves `didDrag` intact
  // (the click guard); `pending` is cleared so no stale press lingers.
  function endPointerGesture() {
    window.removeEventListener("pointermove", onWinPointerMove);
    window.removeEventListener("pointerup", onWinPointerUp);
    window.removeEventListener("pointercancel", onWinPointerCancel);
    document.body.classList.remove("ymux-dragging");
    setGhostPos(null);
    setDragKind(null);
    setDragId(null);
    setDrop(null);
    pending = null;
  }
  // Escape / unmount: abort with no reorder and clear the click guard too.
  const abortDrag = () => {
    endPointerGesture();
    didDrag = false;
  };
  const startPointerDrag = (kind: "ws" | "group", id: string, e: PointerEvent) => {
    if (e.button !== 0) return; // left button only; right-click → context menu
    const el = e.target as HTMLElement;
    // Never start a drag from an interactive child — those own their clicks.
    if (el.closest("button, input, .ws-menu, .ws-port-badge, .group-menu, .group-swatch-picker")) {
      return;
    }
    didDrag = false;
    pending = { kind, id, startX: e.clientX, startY: e.clientY };
    window.addEventListener("pointermove", onWinPointerMove);
    window.addEventListener("pointerup", onWinPointerUp);
    window.addEventListener("pointercancel", onWinPointerCancel);
  };

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && (pending || dragKind() !== null)) abortDrag();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
      endPointerGesture();
    });
  });

  const openMenuAt = (e: MouseEvent, setter: (v: { x: number; y: number }) => void) => {
    setter({
      x: Math.min(e.clientX, window.innerWidth - 200),
      y: Math.min(e.clientY, window.innerHeight - 260),
    });
  };

  // Create a group from the "Move to group ▸" submenu and put the
  // workspace straight into it. Color rotates through the picker
  // palette so consecutive groups don't all come out amber.
  const commitNewGroupFor = async (workspaceId: string) => {
    const name = (newGroupName() ?? "").trim();
    setNewGroupName(null);
    if (name.length === 0) return;
    const color = GROUP_PICKER_COLORS[p.groups.length % GROUP_PICKER_COLORS.length];
    const g = await p.onGroupCreate(name, color);
    if (g) p.onWorkspaceSetGroup(workspaceId, g.id);
    setMenuFor(null);
    setMoveMenuFor(null);
  };

  // ── project-folder workspaces ────────────────────────────────────
  //
  // A pinned repo IS a workspace (`is_project_root`), nested under the
  // one it was pinned from, and its git worktrees render beneath it. A
  // worktree that already has a workspace renders as that workspace's
  // ordinary row; one that doesn't renders dim, and clicking it creates
  // the workspace.
  //
  // Scans are keyed by workspace id and never polled: `git worktree
  // list` is a round-trip over that workspace's connection, so it runs
  // when a subtree is open and has no result yet, and on an explicit ⟳.
  type ScanState =
    | { status: "loading" }
    | { status: "ok"; entries: WorktreeEntry[] }
    // "There is no session yet" is not a failure, it is a not-yet. The
    // scan fires while the sidebar paints, which on a cold start is
    // BEFORE anything has connected, so treating it as an error left a
    // red block on every launch that only a manual ⟳ could clear.
    | { status: "offline" }
    | { status: "error"; message: string };
  const [scans, setScans] = createSignal<Record<string, ScanState>>({});

  // Logged at every outcome: a failing round-trip to another machine is
  // otherwise invisible, and "it just doesn't show" is undiagnosable
  // without it. Metadata only — never the worktree paths.
  const scanFolder = async (ws: Workspace) => {
    setScans((prev) => ({ ...prev, [ws.id]: { status: "loading" } }));
    log.info(`worktree scan start ws=${ws.id}`);
    try {
      const entries = await p.onListWorktrees(ws.id);
      setScans((prev) => ({ ...prev, [ws.id]: { status: "ok", entries } }));
      log.info(`worktree scan ok ws=${ws.id} count=${entries.length}`);
    } catch (e) {
      const msg = String(e);
      // Two very different failures wear the same red row otherwise.
      // "No live SSH session" is transient and worth retrying; "not a git
      // repository" is a permanent answer about this directory, so the
      // workspace stops claiming to be a repo instead of asking again on
      // every expand and every restart.
      // No session yet: park it and let the connectivity effect retry.
      if (/no live SSH session/i.test(msg)) {
        setScans((prev) => ({ ...prev, [ws.id]: { status: "offline" } }));
        log.info(`worktree scan deferred ws=${ws.id} — host not connected yet`);
        return;
      }
      if (/not a git repository/i.test(msg)) {
        setScans((prev) => {
          const next = { ...prev };
          delete next[ws.id];
          return next;
        });
        log.info(`ws=${ws.id} is not a git repo — dropping the project-root flag`);
        p.onNotARepo(ws.id);
        return;
      }
      setScans((prev) => ({
        ...prev,
        [ws.id]: { status: "error", message: msg },
      }));
      log.error(`worktree scan failed ws=${ws.id}`, e);
    }
  };

  /** `~/src/ymux-feature-x` → `ymux-feature-x`, for the dim path hint. */
  const pathTail = (path: string) => {
    const norm = path.replace(/\\/g, "/").replace(/\/+$/, "");
    const i = norm.lastIndexOf("/");
    return i === -1 ? norm : norm.slice(i + 1);
  };

  /**
   * Normalize a path for worktree↔workspace binding: git and the shell
   * disagree about separators and trailing slashes, and a mismatch here
   * would silently render a duplicate row instead of adopting the
   * existing workspace.
   */
  const pathKey = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "");

  // A parked scan resumes the moment any workspace reports a live
  // session — the folder's host is almost always the one that just came
  // up, and a redundant `git worktree list` is cheaper than a stale red
  // row the user has to notice and clear by hand.
  createEffect(() => {
    const live = p.connectedIds;
    if (live.size === 0) return;
    const parked = Object.entries(scans())
      .filter(([, st]) => st.status === "offline")
      .map(([id]) => id);
    for (const id of parked) {
      const ws = p.workspaces.find((w) => w.id === id);
      if (ws) void scanFolder(ws);
    }
  });

  /**
   * parent id → its children, sorted the same way the flat list is
   * (sort_order ascending, nulls last, insertion order as tie-break).
   *
   * MUST stay above the component's `return` — everything below it is a
   * hoisted function declaration, but a `const` there is a temporal dead
   * zone and the whole sidebar becomes an ErrorBoundary card (cbef36e).
   */
  const childrenOf = createMemo(() => {
    const m = new Map<string, { w: Workspace; ins: number }[]>();
    p.workspaces.forEach((w, ins) => {
      if (!w.parent_id) return;
      const list = m.get(w.parent_id) ?? [];
      list.push({ w, ins });
      m.set(w.parent_id, list);
    });
    const out = new Map<string, Workspace[]>();
    for (const [k, v] of m) {
      v.sort((a, b) => {
        const ao = a.w.sort_order ?? Number.MAX_SAFE_INTEGER;
        const bo = b.w.sort_order ?? Number.MAX_SAFE_INTEGER;
        return ao !== bo ? ao - bo : a.ins - b.ins;
      });
      out.set(k, v.map((x) => x.w));
    }
    return out;
  });

  return (
    <div
      class={`sidebar ${p.mode}`}
      data-narrow={p.widthPx < 190 ? "true" : undefined}
      // The masthead scales with the rail, so CSS needs the live width as a
      // length it can do arithmetic on.
      style={{ "--sidebar-w": `${p.widthPx}px` }}
    >
      {/* The masthead IS the collapse control — no separate button, so the
          wordmark gets the whole row. Still a real <button>: single click does
          nothing (collapsing the rail by brushing its header would be a nasty
          surprise), double click toggles, Enter/Space toggles for the keyboard
          since there is no double-Enter idiom. Ctrl+B does it too. */}
      <div class="sidebar-header">
        <button
          class="sidebar-brand-row"
          title={p.mode === "full" ? t("sidebar.collapse.dblclick") : t("sidebar.expand.dblclick")}
          aria-label={p.mode === "full" ? t("sidebar.collapse.tooltip") : t("sidebar.expand.tooltip")}
          onDblClick={() => p.onSetMode(p.mode === "full" ? "icons" : "full")}
          onKeyDown={(e) => {
            if (e.key !== "Enter" && e.key !== " ") return;
            e.preventDefault();
            p.onSetMode(p.mode === "full" ? "icons" : "full");
          }}
        >
        <svg
          class="sidebar-logo"
          viewBox="0 0 1024 1024"
          xmlns="http://www.w3.org/2000/svg"
          aria-hidden="true"
        >
          <defs>
            <linearGradient id="sb-bg" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stop-color="#1d2330" />
              <stop offset="100%" stop-color="#0e1116" />
            </linearGradient>
            <linearGradient id="sb-acc" x1="0" y1="0" x2="1" y2="1">
              <stop offset="0%" stop-color="#7aa2f7" />
              <stop offset="100%" stop-color="#4ec9b0" />
            </linearGradient>
          </defs>
          <rect width="1024" height="1024" rx="200" fill="url(#sb-bg)" />
          <rect
            x="20"
            y="20"
            width="984"
            height="984"
            rx="184"
            fill="none"
            stroke="#21262d"
            stroke-width="4"
          />
          <polyline
            points="300,330 560,512 300,694"
            fill="none"
            stroke="url(#sb-acc)"
            stroke-width="86"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
          <rect x="600" y="640" width="190" height="56" rx="28" fill="url(#sb-acc)" />
          <circle cx="848" cy="176" r="20" fill="#5cd87f" />
        </svg>
        <span class="sidebar-brand">{t("sidebar.title")}</span>
        </button>
      </div>
      <div class="sidebar-list">
        {/* Design Pass 01 (#1): friendly CTA card while the list is empty,
            so a fresh sidebar isn't just blank above the action buttons. */}
        <Show when={p.workspaces.length === 0}>
          <div class="sidebar-empty-card">
            <div class="sidebar-empty-icon" aria-hidden="true">＋</div>
            <div class="sidebar-empty-title">{t("ws.welcome.sidebar.title")}</div>
            <div class="sidebar-empty-desc">{t("ws.welcome.sidebar.desc")}</div>
            <button class="primary" onClick={p.onCreate}>
              {t("ws.welcome.sidebar.cta")}
            </button>
          </div>
        </Show>
        <Show when={p.groups.length > 0}>
          <div
            data-group-id=""
            class={`group-header ${dropIntoGroup(null) ? "drop-into" : ""}`}
            style="cursor: default"
          >
            <span class="group-header-name">{t("sidebar.ungrouped")}</span>
            <span class="group-header-count">({groupedWorkspaces().ungrouped.length})</span>
          </div>
        </Show>
        <For each={groupedWorkspaces().ungrouped}>
          {(w) => renderWorkspaceItem(w)}
        </For>
        <For each={sortedGroups()}>
          {(g) => {
            const members = () => groupedWorkspaces().byGroup.get(g.id) ?? [];
            const collapsed = () => g.is_collapsed;
            return (
              <>
                <div
                  data-group-id={g.id}
                  class={`group-header ${collapsed() ? "group-collapsed" : ""} ${
                    dragKind() === "group" && dragId() === g.id ? "dragging" : ""
                  } ${dropIntoGroup(g.id) ? "drop-into" : ""} ${
                    dropGroupWhere(g.id) ? `drop-${dropGroupWhere(g.id)}` : ""
                  }`}
                  onPointerDown={(e) => startPointerDrag("group", g.id, e)}
                  onClick={() => {
                    if (didDrag) return;
                    p.onGroupToggleCollapse(g.id, !collapsed());
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    if (groupMenuFor() === g.id) {
                      setGroupMenuFor(null);
                      return;
                    }
                    openMenuAt(e, setGroupMenuPos);
                    setGroupMenuFor(g.id);
                    setColorPickerFor(null);
                  }}
                >
                  <span
                    class="group-swatch"
                    style={{ "--group-color": g.color || "#6b7682" } as any}
                  />
                  <Show
                    when={renamingGroup()?.id !== g.id}
                    fallback={
                      <input
                        class="group-inline-input"
                        value={renamingGroup()?.name ?? g.name}
                        autofocus
                        onClick={(e) => e.stopPropagation()}
                        onInput={(e) =>
                          setRenamingGroup({ id: g.id, name: e.currentTarget.value })
                        }
                        onKeyDown={(e) => {
                          if (e.key === "Enter") {
                            const nm = (renamingGroup()?.name ?? "").trim();
                            if (nm.length > 0) p.onGroupRename(g.id, nm);
                            setRenamingGroup(null);
                          } else if (e.key === "Escape") {
                            setRenamingGroup(null);
                          }
                        }}
                        onBlur={() => setRenamingGroup(null)}
                      />
                    }
                  >
                    <span class="group-header-name">
                      <TechText text={g.name} />
                    </span>
                  </Show>
                  <span class="group-header-count">({members().length})</span>
                  <span class="group-header-chevron"><IconChevronDown size={12} /></span>
                </div>
                <Show when={groupMenuFor() === g.id}>
                  <div
                    class="group-menu"
                    style={{
                      top: `${groupMenuPos().y}px`,
                      left: `${groupMenuPos().x}px`,
                    }}
                    onClick={(e) => e.stopPropagation()}
                  >
                    <button
                      onClick={() => {
                        setGroupMenuFor(null);
                        setRenamingGroup({ id: g.id, name: g.name });
                      }}
                    >
                      {t("sidebar.rename_group")}
                    </button>
                    <button
                      onClick={() => {
                        setGroupMenuFor(null);
                        setColorPickerFor(g.id);
                      }}
                    >
                      {t("sidebar.change_color")}
                    </button>
                    <button
                      class="danger"
                      onClick={() => {
                        setGroupMenuFor(null);
                        p.onGroupDelete(g.id);
                      }}
                    >
                      {t("sidebar.delete_group")}
                    </button>
                  </div>
                </Show>
                <Show when={colorPickerFor() === g.id}>
                  <div class="group-swatch-picker" onClick={(e) => e.stopPropagation()}>
                    <For each={GROUP_PICKER_COLORS}>
                      {(c) => (
                        <button
                          class={g.color === c ? "selected" : ""}
                          style={{ background: c }}
                          onClick={() => {
                            p.onGroupSetColor(g.id, c);
                            setColorPickerFor(null);
                          }}
                          aria-label={c}
                        />
                      )}
                    </For>
                  </div>
                </Show>
                <Show when={!collapsed()}>
                  <For each={members()}>{(w) => renderWorkspaceItem(w)}</For>
                </Show>
              </>
            );
          }}
        </For>
      </div>
      <div class="sidebar-actions-row">
        <button class="ws-action-half" onClick={p.onOpenNotes} title={t("sidebar.notes.tooltip")}>
          <span class="ws-action-emoji"><IconNotes /></span>
          <span class="ws-action-label">{t("sidebar.notes.tooltip")}</span>
        </button>
        <button class="ws-action-half" onClick={p.onOpenSettings} title={t("sidebar.settings.tooltip")}>
          <span class="ws-action-emoji"><IconSettings /></span>
          <span class="ws-action-label">{t("sidebar.settings.tooltip")}</span>
        </button>
        <button class="ws-action-half" onClick={p.onOpenPortsGlobal} title={t("sidebar.ports.tooltip")}>
          <span class="ws-action-emoji"><IconGlobe /></span>
          <span class="ws-action-label">{t("sidebar.ports.label")}</span>
        </button>
      </div>
      <button class="ws-add" onClick={p.onCreate} title={t("sidebar.new_workspace")}>
        <span class="ws-action-emoji"><IconPlus /></span>
        <span class="ws-action-label">{t("sidebar.new_workspace")}</span>
      </button>
      <Show when={dragKind() !== null && ghostPos() !== null}>
        <div
          class="ws-ghost"
          style={{
            left: `${ghostPos()!.x + 12}px`,
            top: `${ghostPos()!.y + 10}px`,
          }}
        >
          {ghostLabel()}
        </div>
      </Show>
    </div>
  );

  function renderWorkspaceItem(w: Workspace) {
    return renderWorkspaceSubtree(w, 0);
  }

  /**
   * One workspace and everything under it, emitted as a FLAT fragment.
   *
   * Deliberately not wrapped in a container element: `updateDropTarget`
   * resolves a hover with `closest("[data-ws-id]")`, so a wrapper
   * carrying the parent's id would make a hover over a worktree stub
   * paint a drop line on the parent row.
   *
   * `seen` is not a paranoia flag — the backend repairs cycles at load,
   * but if one ever slipped through, an unguarded recursion here hangs
   * the UI with no error card and nothing in debug.log, which is
   * strictly worse than the crash it replaced.
   */
  function renderWorkspaceSubtree(w: Workspace, depth: number, ancestors: readonly string[] = []) {
    // The cycle guard used to be a MUTABLE Set threaded down the tree.
    // That is wrong across time, not just across depth: <For> re-runs its
    // callback whenever the item references change — which is every
    // `updateFile()`, since the workspaces are freshly parsed JSON — and
    // the ids were already in the set, so every child rendered as null
    // and the whole subtree silently vanished. An immutable ancestor
    // chain has no state to go stale.
    if (ancestors.includes(w.id) || depth > 8) return null;
    const chain = [...ancestors, w.id];
    const kids = () => childrenOf().get(w.id) ?? [];
    const scan = () => scans()[w.id];
    // One rule covers every way a subtree ends up open: this click, a
    // freshly pinned folder, and a restart with it already open.
    createEffect(() => {
      if (w.is_project_root && !w.is_collapsed && !scans()[w.id]) void scanFolder(w);
    });
    return (
      <>
        {renderWorkspaceRow(w, depth)}
        <Show when={!w.is_collapsed}>
          <For each={kids()}>{(k) => renderWorkspaceSubtree(k, depth + 1, chain)}</For>
          <Show when={w.is_project_root}>
            <Show when={scan()?.status === "loading"}>
              <div class="pf-hint" style={`--ws-depth: ${depth + 1}`}>{t("pf.scanning")}</div>
            </Show>
            <Show when={scan()?.status === "offline"}>
              <div class="pf-hint" style={`--ws-depth: ${depth + 1}`}>
                {t("pf.waitingForConnection")}
              </div>
            </Show>
            <Show when={scan()?.status === "error"}>
              {/* git's own message — a bad path and a dead connection
                  read very differently and the user needs to tell them
                  apart. */}
              <div
                class="pf-hint pf-error"
                style={`--ws-depth: ${depth + 1}`}
                title={(scan() as { message: string }).message}
              >
                <IconWarning size={12} /> {(scan() as { message: string }).message}
              </div>
            </Show>
            <Show when={scan()?.status === "ok"}>
              {(() => {
                const bound = () => {
                  const m = new Map<string, Workspace>();
                  for (const k of kids()) if (k.cwd) m.set(pathKey(k.cwd), k);
                  return m;
                };
                // `git worktree list` includes the repo root itself —
                // that entry IS this workspace, so rendering a stub for
                // it would offer to open a child sharing its parent's
                // directory.
                const stubs = () =>
                  (scan() as { entries: WorktreeEntry[] }).entries.filter(
                    (wt) =>
                      pathKey(wt.path) !== pathKey(w.cwd ?? "") &&
                      !bound().has(pathKey(wt.path)),
                  );
                return (
                  <>
                    <For each={stubs()}>
                      {(wt) => (
                        <div
                          class={`ws-item pf-unopened ${wt.is_prunable ? "prunable" : ""}`}
                          style={`--ws-depth: ${depth + 1}`}
                          title={`${wt.path}${wt.is_locked ? " — " + t("pf.locked") : ""}${
                            wt.is_prunable ? " — " + t("pf.prunable") : ""
                          }\n${t("pf.openWorktree")}`}
                          onClick={() => p.onOpenWorktree(w.id, wt)}
                        >
                          <span class="wt-icon"><IconGitBranch size={12} /></span>
                          <span class="wt-branch">
                            <TechText text={wt.branch ?? (wt.is_detached ? wt.head.slice(0, 7) : "—")} />
                          </span>
                          <span class="wt-path"><TechText text={pathTail(wt.path)} /></span>
                          <Show when={wt.is_locked}>
                            <span class="wt-flag" title={t("pf.locked")}>🔒</span>
                          </Show>
                          <Show when={wt.is_prunable}>
                            <span class="wt-flag" title={t("pf.prunable")}>⚠</span>
                          </Show>
                        </div>
                      )}
                    </For>
                    <Show when={kids().length === 0 && stubs().length === 0}>
                      <div class="pf-hint" style={`--ws-depth: ${depth + 1}`}>
                        {t("pf.noWorktrees")}
                      </div>
                    </Show>
                  </>
                );
              })()}
            </Show>
          </Show>
        </Show>
      </>
    );
  }

  function renderWorkspaceRow(w: Workspace, depth = 0) {
    return (
      <div
        data-ws-id={w.id}
        class={`ws-item ${p.activeId === w.id ? "active" : ""} ${
          p.waitingWorkspaceIds.has(w.id) ? "has-waiting" : ""
        } ${
          p.hookPulseWorkspaceIds?.has(w.id) ? "hook-pulse" : ""
        } ${dragKind() === "ws" && dragId() === w.id ? "dragging" : ""} ${
          dropWsWhere(w.id) ? `drop-${dropWsWhere(w.id)}` : ""
        }`}
        data-has-color={w.color ? "true" : "false"}
        // One style STRING, not an object: mixing the two forms on the
        // same attribute silently drops one of them.
        style={`--ws-depth: ${depth}${w.color ? `; --ws-color: ${w.color}` : ""}`}
        // Always, not just in icons mode: `full` mode can be dragged down to
        // 160px, where .ws-name ellipsizes and the tooltip is the only way
        // left to read the name.
        title={w.name}
        // beta.3 (ws-dragdrop): pointer-drag reorder. A press that never crosses
        // the move threshold is a click → switch; a completed drag sets
        // `didDrag`, which swallows the trailing click here.
        onPointerDown={(e) => startPointerDrag("ws", w.id, e)}
        onClick={() => {
          if (didDrag) return;
          p.onActivate(w.id);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          if (menuFor() === w.id) {
            setMenuFor(null);
            setMoveMenuFor(null);
            return;
          }
          openMenuAt(e, setMenuPos);
          setMenuFor(w.id);
          setMoveMenuFor(null);
        }}
      >
        <Show when={w.is_project_root || (childrenOf().get(w.id) ?? []).length > 0}>
          {/* A <button> so `startPointerDrag`'s interactive-child
              exclusion already skips it; stopPropagation keeps the click
              from also activating the workspace. */}
          <button
            class="ws-chevron"
            title={w.is_collapsed ? t("pf.expand") : t("pf.collapse")}
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              p.onSetCollapsed(w.id, !w.is_collapsed);
            }}
          >
            {w.is_collapsed ? <IconChevronRight size={12} /> : <IconChevronDown size={12} />}
          </button>
        </Show>
        <Show
          when={!w.is_project_root}
          fallback={
            <span class="pf-icon pf-icon-repo" title={t("pf.isRepo")}>
              <IconFolder size={13} />
              <span class="pf-git-badge"><IconGitBranch size={9} /></span>
            </span>
          }
        >
          {/* Phase 87.B: a row opened FOR a multiplexer session wears a
              terminal glyph instead of the colour dot; the raw session
              name is the tooltip. Everything else about the row — click,
              collapse, drag, delete — is the plain child-workspace path. */}
          <Show
            when={!w.tmux_session}
            fallback={
              <span class="ws-session-icon" title={w.tmux_session ?? undefined}>
                <IconTerminal size={13} />
              </span>
            }
          >
            <span
              class="ws-dot"
              style={{ background: w.color || "#6b7682" }}
            />
          </Show>
        </Show>
        <span class="ws-name">
          <Show when={w.emoji}>{w.emoji} </Show>
          <TechText text={w.name} />
        </span>
        <Show when={w.git_worktree}>
          <span class="ws-worktree-chip" title={w.git_worktree!}><IconGitBranch size={13} /></span>
        </Show>
        <Show when={w.is_project_root}>
          <button
            class="pf-btn"
            title={t("pf.newWorktree")}
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => { e.stopPropagation(); p.onNewWorktree(w); }}
          >
            <IconPlus size={12} />
          </button>
          <button
            class="pf-btn"
            title={t("pf.refresh")}
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => { e.stopPropagation(); void scanFolder(w); }}
          >
            <IconRefresh size={12} />
          </button>
        </Show>
        {/* Every status marker lives in ONE trailing cluster, in a fixed
            order, so they line up in a column down the list instead of
            landing wherever flex leaves room on each row. The kind/pane
            badge goes LAST — it is the only one always present, so putting
            it flush against the inline end is what makes it a column; the
            optional markers vary in count and stack inboard of it. */}
        <span class="ws-meta">
          {(() => {
            const fwds = p.allForwards.filter((f) => f.workspace_id === w.id);
            return (
              <Show when={fwds.length > 0}>
                <span
                  class="ws-port-badge"
                  title={t(
                    fwds.length === 1
                      ? "ports.workspaceBadge.tooltipOne"
                      : "ports.workspaceBadge.tooltipMany",
                    { count: fwds.length },
                  )}
                  onClick={(e) => {
                    e.stopPropagation();
                    p.onOpenPorts(w.id);
                  }}
                >
                  <IconGlobe size={12} /> {fwds.length}
                </span>
              </Show>
            );
          })()}
          {/* A real element, not the `.has-waiting::after` it replaces: the
              pseudo-element was absolutely positioned at inset-inline-end
              and painted ON TOP of the badges. The class stays on the row —
              themes-redesign.css and the pulse both key off it. */}
          <Show
            when={
              p.waitingWorkspaceIds.has(w.id)
              || p.briefAttentionWorkspaceIds?.has(w.id)
              || p.notifiedWorkspaceIds.has(w.id)
            }
          >
            <span
              class={`ws-waiting-dot ${
                p.waitingWorkspaceIds.has(w.id)
                  ? ""
                  : p.briefAttentionWorkspaceIds?.has(w.id)
                    ? "brief-attn"
                    : "activity"
              }`}
              title={t(
                p.waitingWorkspaceIds.has(w.id)
                  ? "sidebar.workspaceWaitingTitle"
                  : p.briefAttentionWorkspaceIds?.has(w.id)
                    ? "sidebar.workspaceBriefTitle"
                    : "sidebar.workspaceActivityTitle",
              )}
            />
          </Show>
          <Show when={p.connectedIds.has(w.id)}>
            <span class="ws-live" title={t("sidebar.workspaceConnectedTitle")} />
          </Show>
          <WorkspaceBadge w={w} />
        </span>
        <Show when={menuFor() === w.id}>
          <div
            class="ws-menu ws-menu-fixed"
            style={{
              position: "fixed",
              top: `${menuPos().y}px`,
              left: `${menuPos().x}px`,
              right: "auto",
              "z-index": "1000",
            }}
            onClick={(e) => {
              e.stopPropagation();
              if (moveMenuFor() !== w.id) {
                setMenuFor(null);
                setMoveMenuFor(null);
              }
            }}
          >
            <button onClick={() => p.onAction(w.id, "rename")}>
              {t("ws.context.rename")}
            </button>
            <button onClick={() => p.onAction(w.id, "edit")}>
              {t("ws.context.edit")}
            </button>
            {/* Phase 87: every multiplexer session on this workspace's
                machine, with an agent summary per row. Above Add-ons on
                purpose — it is the thing you open several times a day. */}
            <button onClick={() => p.onAction(w.id, "sessions")}>
              {t("ws.context.sessions")}
            </button>
            <button onClick={() => p.onAction(w.id, "addons")}>
              {t("ws.context.addons")}
            </button>
            {/* Pinning happens from a workspace because that is what
                gives the folder browser a host to walk: `file_list_remote`
                resolves SFTP from this workspace's live SSH session. The
                folder itself stores a copy of the connection, so it
                outlives the workspace it was picked from. */}
            <button onClick={() => p.onAction(w.id, "add_project_folder")}>
              {t("pf.pinFolder")}…
            </button>
            {/* The way back. A directory that git rejected is demoted
                once and never re-asked — correct while the answer is
                stable, wrong the moment someone runs `git init` there.
                Re-pinning is not the escape hatch either: the duplicate
                check refuses the same path under the same parent. So the
                re-check is an explicit action, on demand, which also
                keeps it inside the "scans are lazy, never polled" rule. */}
            <Show when={w.cwd && !w.is_project_root}>
              <button onClick={() => p.onAction(w.id, "check_git")}>
                {t("pf.checkGit")}
              </button>
            </Show>
            <button
              onClick={() => {
                setMoveMenuFor(moveMenuFor() === w.id ? null : w.id);
                setNewGroupName(null);
              }}
            >
              {t("sidebar.move_to_group")}▸
            </button>
            <Show when={moveMenuFor() === w.id}>
              <div
                class="group-menu"
                style={{
                  position: "static",
                  "box-shadow": "none",
                  border: "0",
                  padding: "0",
                }}
              >
                <Show when={w.group_id}>
                  <button
                    onClick={() => {
                      p.onWorkspaceSetGroup(w.id, null);
                      setMenuFor(null);
                      setMoveMenuFor(null);
                    }}
                  >
                    {t("sidebar.move_out_of_group")}
                  </button>
                </Show>
                <For each={p.groups.filter((g) => g.id !== w.group_id)}>
                  {(g) => (
                    <button
                      onClick={() => {
                        p.onWorkspaceSetGroup(w.id, g.id);
                        setMenuFor(null);
                        setMoveMenuFor(null);
                      }}
                    >
                      <span
                        class="group-swatch"
                        style={{
                          "--group-color": g.color || "#6b7682",
                          "margin-inline-end": "6px",
                        } as any}
                      />
                      <TechText text={g.name} />
                    </button>
                  )}
                </For>
                <Show
                  when={newGroupName() === null}
                  fallback={
                    <input
                      class="group-inline-input"
                      placeholder={t("sidebar.group_name_prompt")}
                      value={newGroupName() ?? ""}
                      // `autofocus` doesn't fire on elements inserted into an
                      // already-loaded document — focus explicitly on mount.
                      ref={(el) => queueMicrotask(() => el.focus())}
                      onClick={(e) => e.stopPropagation()}
                      onInput={(e) => setNewGroupName(e.currentTarget.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void commitNewGroupFor(w.id);
                        else if (e.key === "Escape") setNewGroupName(null);
                      }}
                      onBlur={() => setNewGroupName(null)}
                    />
                  }
                >
                  <button onClick={() => setNewGroupName("")}>
                    {t("sidebar.new_group")}…
                  </button>
                </Show>
              </div>
            </Show>
            <Show when={p.connectedIds.has(w.id)}>
              <button onClick={() => p.onAction(w.id, "disconnect")}>
                {t("ws.context.disconnect")}
              </button>
            </Show>
            <button
              class="danger"
              onClick={() => p.onAction(w.id, "delete")}
            >
              {t("ws.context.delete")}
            </button>
          </div>
        </Show>
      </div>
    );
  }
}

// Regression-fix v2: extracted from an inline IIFE that was re-evaluated on every
// parent render. The IIFE form caused churn that intermittently mis-routed clicks
// on the workspace items themselves and (separately) drove a `workspace_set_active`
// autosave loop. As a stable child component, Solid reuses the same instance.
function WorkspaceBadge(props: { w: Workspace }) {
  const b = () => workspaceBadge(props.w);
  return (
    <span class={`ws-badge ${b().cls}`} title={b().title}>
      {b().label}
    </span>
  );
}
