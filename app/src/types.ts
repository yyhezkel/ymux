// Phase 35 (#1.5): the data-model types below are generated from the
// Rust structs by ts-rs and re-exported here so existing imports
// (`from "./types"`) keep working. Regenerate after a Rust struct
// change with `cd app/src-tauri && cargo test`. Do not hand-edit
// `src/bindings/*.ts`.
//
// Note: ts-rs renders `Option<T>` as `T | null` (a required, nullable
// key) rather than the optional `T?` the hand-written mirror used.
// Helpers that accept these structurally (e.g. effectiveIdentity)
// widen their params to `T | null | undefined` accordingly.
export type { Connection } from "./bindings/Connection";
export type { SplitDirection } from "./bindings/SplitDirection";
export type { PaneKind } from "./bindings/PaneKind";
export type { BrowserState } from "./bindings/BrowserState";
export type { EnvVar } from "./bindings/EnvVar";
export type { LayoutNode } from "./bindings/LayoutNode";
export type { Workspace } from "./bindings/Workspace";
// cmux-A A2: sidebar collapsible groups.
export type { WorkspaceGroup } from "./bindings/WorkspaceGroup";
// One row of `git worktree list --porcelain`, for a workspace flagged
// `is_project_root`. There is no ProjectFolder type any more — a pinned
// repo IS a workspace, nested under the one it was pinned from.
export type { WorktreeEntry } from "./bindings/WorktreeEntry";
export type { FeedItem } from "./bindings/FeedItem";
export type { FeedItemState } from "./bindings/FeedItemState";
export type { ClaudeUsage } from "./bindings/ClaudeUsage";
export type { ModelUsage } from "./bindings/ModelUsage";

import type { Connection } from "./bindings/Connection";
import { isWindows } from "./platform";
import type { EnvVar } from "./bindings/EnvVar";
import type { LayoutNode } from "./bindings/LayoutNode";
import type { PaneKind } from "./bindings/PaneKind";
import type { Workspace } from "./bindings/Workspace";

// Phase 80 (unified setup wizard): the payload every create flow hands to
// App.handleCreate → workspace_create. Previously duplicated inline in
// CreateWorkspaceModal props and App.tsx; the wizard's quick flows would
// have made it a 4th copy.
export interface CreateWorkspaceInput {
  name: string;
  connection: Connection;
  color?: string;
  cwd?: string;
  setup_command?: string;
  teardown_command?: string;
  env?: EnvVar[];
}

// Phase 23.F: shape returned by pane_list_tmux_sessions. Used by
// the Connect (tmux) picker.
// Phase 81: optional fields joined from the server-side
// ~/.ymux/session-meta.json (absent on old-CLI servers). Display
// precedence: label > auto_name > claude_title > name.
/** What `pane_kill_session` achieved. Mirrors `KillSessionOutcome` in lib.rs.
 *
 *  Before 2026-08-20 the command returned nothing and the frontend inferred
 *  success from "the invoke did not throw" — so a Kill on a machine with no
 *  multiplexer installed destroyed nothing and reported that it worked. */
export type KillResult =
  | "killed"
  | "already_gone"
  | "no_session"
  | "multiplexer_missing"
  | "failed"
  /** SSH/tmux: the verb was sent, but `|| true` in the remote command means
   *  its exit status is not yet readable. Honest placeholder, not a claim. */
  | "attempted";

export interface KillSessionOutcome {
  result: KillResult;
  backend: "zellij" | "tmux" | "none";
  session?: string;
  /** The multiplexer's own stderr, truncated. Never PTY content. */
  detail?: string;
}

export interface TmuxSessionInfo {
  name: string;
  created: number;
  attached: boolean;
  windows: number;
  last_attached: number;
  /** zellij only: the session's shell has exited but zellij still holds a
   *  serialized copy, so attaching RESURRECTS it — across a reboot too.
   *  Always false on tmux, which does not keep dead sessions. */
  exited: boolean;
  /** Manual label the user gave the pane (shared across machines). */
  label?: string;
  /** Claude session title extracted from the transcript — rewritten as
   *  the conversation drifts, so it is NOT the session's identity. */
  claude_title?: string;
  /** Stable "<two words> · <date time>" derived once from the session's
   *  first prompt. This is the identity shown in the picker. */
  auto_name?: string;
  /** Claude session UUID running inside this tmux session. */
  claude_session_id?: string;
  /** machine-id of the ymux install that created the session. */
  origin?: string;
  /** 2026-08-23: the session's working directory, from tmux's
   *  `#{session_path}`. Absent on zellij (its list-sessions reports no cwd
   *  at all) and on a tmux answering the pre-2026-08-23 five-field format.
   *  Absent means UNKNOWN — never "somewhere else". */
  cwd?: string;
  /** ymux created or attached this session for the workspace that asked
   *  (`session-owners.json`). The only workspace signal available on
   *  Windows, where zellij reports no directory. */
  owned: boolean;
  /** `cwd` is the workspace's folder, or lives under it. False whenever
   *  `cwd` is unknown. */
  in_cwd: boolean;
  /** 2026-08-24: we can positively place this session somewhere that is NOT
   *  this workspace. Absent means "no evidence" — which an unknown `cwd` with
   *  no ownership row always is.
   *
   *  Invariant from the backend (`annotate_scope_with`): never set while
   *  `owned || in_cwd`. "This folder" is exactly the complement of this field,
   *  so the badge cannot show up there and needs no scope conditional. */
  foreign?: ForeignScope;
  /** Phase 90: the cwd recorded in `session-owners.json` when ymux claimed
   *  this session, whichever workspace did. A grouping key for the
   *  active-sessions overview (zellij rows have no live `cwd`); a claim-time
   *  snapshot, so it can be stale. Feeds no scope verdict. */
  owner_cwd?: string;
}

/** Phase 90: one row of `sessions_overview_summarize`. Mirrors
 *  `SessionSummary` in sessions_overview.rs. `status` is the model's read of
 *  the screen; `unknown` when it said nothing usable. */
export interface SessionSummary {
  name: string;
  status: "idle" | "working" | "waiting_input" | "error" | "unknown";
  summary: string;
}

/** 2026-08-24: where a tmux/zellij session belongs, when it is not here.
 *
 *  Facts, not a sentence — the picker composes the wording from i18n. `kind`
 *  is what picks between "belongs to workspace X" and "runs in folder Y". */
export interface ForeignScope {
  /** `workspace` — another workspace claimed it in `session-owners.json`
   *  (the only signal on Windows, where zellij reports no cwd).
   *  `folder` — unclaimed, but its live cwd is outside this folder. */
  kind: "workspace" | "folder";
  /** The owning workspace's name, or the folder's last path segment. Absent
   *  when the claiming workspace was deleted and recorded no cwd — still a
   *  real warning, just an unnameable one. */
  label?: string;
  /** Full path for the tooltip: the live `#{session_path}` when known, else
   *  the cwd recorded at claim time (which may be stale). */
  path?: string;
}

/** 2026-08-23: what `pane_target_session_state` answers — "which multiplexer
 *  session will this pane land on, and is something already running there?"
 *  `reachable: false` means the host could not be asked, so `exists` carries
 *  no information. */
export interface TargetSessionState {
  name: string;
  exists: boolean;
  attached: boolean;
  reachable: boolean;
}

// Phase 24.D: ChatRole / MessageStatus / ChatMessage / ClaudeChatState
// (Phase 22) removed alongside the ClaudeChat pane. The
// ClaudeLog* types just below are kept for the dead-code-but-
// registered backend (a future unified-view rebuild can consume the
// existing claude_log_sync / list / read commands without re-typing).

/** Phase 24.B: kept for future unified-view rebuild — no current consumer. */
export interface ClaudeSyncResult {
  synced: number;
  skipped: number;
  errors: string[];
  total_bytes: number;
}

/** Phase 24.B: kept for future unified-view rebuild — no current consumer. */
export interface ClaudeLogSummary {
  session_id: string;
  message_count: number;
  first_user?: string;
  last_assistant?: string;
  project_path?: string;
  file_size: number;
  /** Unix seconds. */
  local_mtime: number;
}

/** Phase 24.B: kept for future unified-view rebuild — no current consumer. */
export interface ClaudeLogEntry {
  line_no: number;
  /** "user" | "assistant" | "tool_use" | "tool_result" | "system" | "summary" */
  entry_type: string;
  text: string;
  tool_name?: string;
  timestamp?: string;
  session_id?: string;
}

/** Phase 24.B: kept for future unified-view rebuild — no current consumer.
 *  The Rust-side `claudelog` field on LayoutNode::Pane was removed in 24.D
 *  along with `chat`; if the pane comes back, restore both. */
export interface ClaudeLogState {
  session_id?: string;
  filter?: string;
}

// Phase 35: pane_kind is now non-optional in the generated binding
// (ts-rs emits the serde default-elided field as required). The
// `?? "terminal"` is kept as a defensive fallback for any legacy
// object that still lacks it at runtime.
export function paneKindOf(p: LayoutNode & { kind: "pane" }): PaneKind {
  return p.pane_kind ?? "terminal";
}

export type WorkspacesFile = {
  version: 1;
  active_workspace_id: string | null;
  workspaces: Workspace[];
  // cmux-A A2: sidebar groups. Older workspaces.json without this
  // key deserializes as an empty array (backend serde default).
  groups?: import("./bindings/WorkspaceGroup").WorkspaceGroup[];
};

export type PtyDataEvent = { session_id: string; data: string };
export type PtyExitEvent = { session_id: string; reason: string | null };

// Phase 36 (#2.2): a live auto port-forward, as tracked on the
// frontend. opened_at is stamped client-side when the
// port-forward-opened event arrives (the backend doesn't persist it).
export type ForwardRow = {
  workspace_id: string;
  remote_port: number;
  local_port: number;
  remote_addr: string;
  opened_at: number;
};

export type FeedResolvedEvent = { request_id: string; decision: string };

export type NoteStatus = "open" | "done";

export type Note = {
  id: string;
  created_at: string;
  updated_at: string;
  text: string;
  tag?: string;
  status: NoteStatus;
  workspace_id?: string | null;
  pane_id?: string | null;
};

export type NotesFile = {
  version: 1;
  notes: Note[];
};

export function collectPanes(node: LayoutNode): string[] {
  if (node.kind === "pane") return [node.pane_id];
  return [...collectPanes(node.first), ...collectPanes(node.second)];
}

export function findPane(
  node: LayoutNode,
  paneId: string
): (LayoutNode & { kind: "pane" }) | null {
  if (node.kind === "pane")
    return node.pane_id === paneId ? node : null;
  return findPane(node.first, paneId) ?? findPane(node.second, paneId);
}

// Unshipped-fivefer (#4): remove the given pane_ids from a layout tree and
// collapse their parent splits onto the surviving sibling, so the grid reflows
// to fill the vacated space. Used when a pane is "popped out" into its own OS
// window — it leaves the grid entirely and returns on popout close. Returns
// null only when every pane is hidden. Pure — never mutates the input.
export function pruneLayout(
  node: LayoutNode,
  hidden: Set<string>
): LayoutNode | null {
  if (node.kind === "pane") return hidden.has(node.pane_id) ? null : node;
  const first = pruneLayout(node.first, hidden);
  const second = pruneLayout(node.second, hidden);
  if (first && second) return { ...node, first, second };
  return first ?? second;
}

// Phase 31: a pane's effective identity is its own override falling
// back to its workspace's. Used by the pane header, the rename dialog's
// "inheriting" hint, and the OS window title.
export function effectiveIdentity(
  pane: { color?: string | null; emoji?: string | null } | null | undefined,
  ws: { color?: string | null; emoji?: string | null } | null | undefined,
): { color?: string; emoji?: string } {
  return {
    color: pane?.color ?? ws?.color ?? undefined,
    emoji: pane?.emoji ?? ws?.emoji ?? undefined,
  };
}

// A switch rather than an if-chain with a bare `return ssh …` tail: that
// tail treated ANY unrecognised variant as SSH and rendered
// `ssh undefined@undefined:undefined`. Same latent gap `capsOf` closes.
export function describeConnection(c: Connection): string {
  switch (c.type) {
    case "local":
      return c.shell ? `local · ${c.shell}` : "local";
    // Phase 80: WSL-tmux workspaces.
    case "wsl":
      return c.distro ? `wsl · ${c.distro}` : "wsl";
    case "ssh":
      return `ssh ${c.user}@${c.host}:${c.port}`;
    default:
      return assertNever(c, "unhandled Connection variant in describeConnection");
  }
}


// beta.3-localhost — Unified SSH gate.
//
// The connection discriminator lives on the ts-rs-generated `Connection`
// binding (`{ type: "local" | "ssh" }`). Before this refactor, ~10 sites
// across App.tsx, Sidebar.tsx, FileManagerWindow.tsx and PaneView.tsx
// repeated `c?.type === "ssh"` inline, with subtle semantic drift
// (workspace-only vs. layout-walk vs. pane-with-fallback). These
// predicates collapse the pattern to three sharp questions:
//
//   isRemoteConn(c)        → is *this* connection SSH?
//   isRemoteWorkspace(w)   → workspace's declared connection is SSH?
//   hasSftp(w)             → SFTP-capable? (workspace *or* any pane)
//   hasServer(w)           → has a control-plane server? (today == remote)
//   isRemoteEffective(...) → pane's own conn OR workspace's fallback
//
// ─── WSL parity: ask what a connection CAN DO, not what it IS ──────────
//
// Phase 80 added `Connection::Wsl` and none of the predicates above
// learned about it. `isRemoteConn` is false for WSL (it isn't ssh) and so
// is `isLocalConn` (it isn't local), so WSL fell through BOTH halves of
// every two-way split — got routed down the SSH branch, and died on
// "no active SSH session for this workspace".
//
// The fix is not a third boolean; it is asking a different question.
// Three axes were conflated into one:
//
//   A. can I run POSIX commands there?   ssh ✔  wsl ✔  local ✘
//   B. is it across a network?           ssh ✔  wsl ✘  local ✘
//        (auth prompts, host-key trust, disconnects, latency)
//   C. must a session be live first?     ssh ✔  wsl ✘  local ✘
//        (wsl.exe answers cold; SSH needs a handle)
//
// WSL is A-yes / B-no / C-no, which no single boolean can express.
// `capsOf` names the axes so a gate says what it means, and its switch
// ends in `assertNever` — so connection kind #4 (devcontainer? podman?)
// is a COMPILE ERROR here rather than another silent runtime gap.
// That guard is the actual deliverable; the feature work is downstream.
//
// `hasServer` splits from `isRemote` deliberately: LOCAL-HOST plan #2
// wires a native local server, at which point `hasServer(local)` flips
// to true without churning every callsite. Keep the seam.
export function isRemoteConn(
  c: Connection | null | undefined,
): c is Extract<Connection, { type: "ssh" }> {
  return !!c && c.type === "ssh";
}

export function isLocalConn(
  c: Connection | null | undefined,
): boolean {
  // null / undefined connections are treated as local (matches historical
  // FE behavior — a workspace with no connection field is a local shell).
  return !c || c.type === "local";
}

export function isRemoteWorkspace(w: { connection: Connection | null }): boolean {
  return isRemoteConn(w.connection);
}

export function isLocalWorkspace(w: { connection: Connection | null }): boolean {
  return isLocalConn(w.connection);
}

// SFTP is available when the workspace declares SSH OR any pane in the
// layout is on an SSH connection (used by FileManagerPane to decide
// whether to render the remote column). Kept as a single predicate so
// call sites don't re-inline the layout walk.
export function hasSftp(
  w: { connection: Connection | null; layout: LayoutNode | null },
): boolean {
  if (isRemoteConn(w.connection)) return true;
  if (!w.layout) return false;
  const walk = (n: LayoutNode): boolean => {
    if (n.kind === "pane") return isRemoteConn(n.connection);
    return walk(n.first) || walk(n.second);
  };
  return walk(w.layout);
}

// beta.3-lh-insights: native local Insights daemon shipped. Every workspace
// now has a control-plane server — remote via SSH → `ymux-server`, local
// via in-process sysinfo + bollard. So `hasServer` collapses to `true`, but
// we keep the predicate (rather than deleting every call site) in case a
// future workspace kind ("read-only", "sandbox") lands without a server.
export function hasServer(_w: { connection: Connection | null }): boolean {
  return true;
}

// Pane's effective connection: own > workspace fallback. Used by PaneView
// where the pane may not carry a connection of its own (FM/Browser/Chat)
// but still sits inside an SSH workspace and needs SSH-only menu items.
export function isRemoteEffective(
  pane: { connection?: Connection | null },
  workspaceConn: Connection | null | undefined,
): boolean {
  return isRemoteConn(pane.connection ?? workspaceConn);
}

/** How a port listening inside the target is reached from Windows. */
export type PortForwardKind =
  /** Needs an SSH direct-tcpip forward before anything can connect. */
  | "ssh"
  /** Already reachable on the same localhost port — nothing to forward.
   *  WSL2 shares the loopback in mirrored mode, and proxies it in NAT
   *  mode via `localhostForwarding` (the .wslconfig default). */
  | "sharedLoopback"
  /** Ports are not a concept here. */
  | "none";

export interface ConnCaps {
  /** POSIX commands can be run there (`exec`-shaped work). */
  posixExec: boolean;
  /** A POSIX filesystem can be listed / read / written. */
  fileTransfer: boolean;
  /** Multiplexer sessions outlive the app, so panes can re-attach on boot.
   *  tmux over SSH, zellij on a native Windows pane (2026-08-19). Renamed
   *  from `tmuxPersistence` when it stopped being tmux-only. */
  sessionPersistence: boolean;
  /** A control-plane server (ymux-server / insights) can serve it. */
  controlServer: boolean;
  portForward: PortForwardKind;
  /** Must a pane be connected before the capabilities are usable?
   *  SSH needs a live handle; wsl.exe answers from cold. */
  sessionBound: boolean;
  /** Password / passphrase / host-key prompts apply. */
  networkAuth: boolean;
}

const LOCAL_CAPS: ConnCaps = {
  posixExec: false,
  fileTransfer: false,
  // 2026-08-19: native Windows panes gained persistence via zellij, which
  // is what makes WSL unnecessary. Not a user setting — a local pane is
  // persistent the same way an SSH pane is.
  sessionPersistence: true,
  controlServer: true, // native insights_local
  portForward: "none",
  sessionBound: false,
  networkAuth: false,
};

// macOS port: a local shell on a unix host IS a POSIX host with its own
// tmux server — pane_connect wraps persistent panes in `tmux new-session`
// and pane_list/probe_tmux_sessions answer from the local binary. Only
// fileTransfer stays false for the same SFTP-backend reason as WSL.
const LOCAL_UNIX_CAPS: ConnCaps = {
  ...LOCAL_CAPS,
  posixExec: true,
  sessionPersistence: true,
};

const SSH_CAPS: ConnCaps = {
  posixExec: true,
  fileTransfer: true,
  sessionPersistence: true,
  controlServer: true,
  portForward: "ssh",
  sessionBound: true,
  networkAuth: true,
};

const WSL_CAPS: ConnCaps = {
  posixExec: true,
  // FALSE ON PURPOSE, and not a statement about WSL. The distro's
  // filesystem is perfectly reachable — but every file_* command is still
  // implemented against an SFTP session, so flipping this would light up a
  // dir picker that calls `file_home_remote` and gets back "no active SSH
  // session". Flip it in the same commit that lands the WSL file backend;
  // a capability table that lies is worse than the boolean it replaced.
  fileTransfer: false,
  sessionPersistence: true,
  controlServer: true,
  portForward: "sharedLoopback",
  sessionBound: false,
  networkAuth: false,
};

/**
 * The one place a connection kind is mapped to what it can do.
 *
 * The `default` arm is not dead code — it is the point. `assertNever`
 * makes tsc reject a new `Connection` variant that forgets a case here,
 * which is exactly what did NOT happen when `wsl` was added.
 */
export function capsOf(c: Connection | null | undefined): ConnCaps {
  const local = isWindows() ? LOCAL_CAPS : LOCAL_UNIX_CAPS;
  if (!c) return local; // a workspace with no connection is a local shell
  switch (c.type) {
    case "local":
      return local;
    case "ssh":
      return SSH_CAPS;
    case "wsl":
      return WSL_CAPS;
    default:
      return assertNever(c, "unhandled Connection variant in capsOf");
  }
}

/** Which RTL settings profile a pane uses. See `profileFor`. */
export type RtlProfileKind = "local" | "remote";

/**
 * 2026-08-19: pick the RTL profile for a connection.
 *
 * Measured live, both directions, same build and session: a native Windows
 * pane (ConPTY) delivers Hebrew ALREADY in visual order — the Windows console
 * keeps RTL text visually ordered in its screen buffer and ConPTY re-emits
 * that buffer — so `bidi_reorder` renders it correctly and a browser bidi pass
 * reverses it. An SSH pane delivers logical order, where `auto_per_line` is
 * correct and `off` comes out reversed. Opposite modes, not different tuning,
 * which is why one global setting could never serve both.
 *
 * The axis is `posixExec`, deliberately NOT a local/remote boolean — see the
 * commentary above `capsOf`. **WSL is "remote"**: it is Linux with tmux and a
 * Linux Claude, and is local only geographically.
 */
export function profileFor(c: Connection | null | undefined): RtlProfileKind {
  return capsOf(c).posixExec ? "remote" : "local";
}

/**
 * Workspace-level capabilities: the workspace's own connection, widened
 * by any pane that carries one of its own. Mirrors the layout walk
 * `hasSftp` already did — a workspace with no declared connection but an
 * SSH pane inside it is still SFTP-capable.
 */
export function wsCaps(w: {
  connection: Connection | null;
  layout?: LayoutNode | null;
}): ConnCaps {
  const own = capsOf(w.connection);
  if (own.posixExec || !w.layout) return own;
  let found: ConnCaps | null = null;
  const walk = (n: LayoutNode): void => {
    if (found) return;
    if (n.kind === "pane") {
      const c = capsOf(n.connection);
      if (c.posixExec) found = c;
      return;
    }
    walk(n.first);
    walk(n.second);
  };
  walk(w.layout);
  return found ?? own;
}

/** Pane's effective capabilities: own connection > workspace fallback. */
export function paneCaps(
  pane: { connection?: Connection | null },
  workspaceConn: Connection | null | undefined,
): ConnCaps {
  return capsOf(pane.connection ?? workspaceConn);
}

// Exhaustiveness guard — if a future Connection variant is added the
// TypeScript compiler flags every switch that forgot to handle it.
export function assertNever(x: never, msg = "unreachable"): never {
  throw new Error(`${msg}: ${JSON.stringify(x)}`);
}
