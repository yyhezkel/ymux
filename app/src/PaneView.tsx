import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
import { open as openNativeDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { BoundSession, Connection, KillSessionOutcome, LayoutNode, TargetSessionState, TmuxSessionInfo, RtlProfileKind } from "./types";
import { describeConnection, effectiveIdentity, isLocalConn, isRemoteConn, isRemoteEffective, paneCaps, profileFor } from "./types";
import type { TerminalInstance } from "./terminalInstance";
import { t } from "./i18n";
import { isMac } from "./platform";
import { createLogger } from "./logger";

const log = createLogger("PANE");
import { TechText } from "./TechText";
import { AgentLight } from "./AgentLight";
import type { TrafficLight } from "./paneAgentState";
import {
  paneDragStore,
  startPaneDrag,
} from "./paneDrag";
import {
  IconPencil,
  IconPower,
  IconChevronDown,
  IconArrowLeftRight,
  IconMaximize,
  IconMinimize,
  IconColumns,
  IconRows,
  IconExternalLink,
  IconClose,
  IconWarning,
  IconTerminal,
  IconRefresh,
  IconClock,
  IconFolder,
  IconInfo,
  IconTrash,
} from "./icons";

interface ClaudeSessionInfo {
  session_id: string;
  project_path: string;
  jsonl_path: string;
  mtime_unix: number;
  last_user?: string | null;
  last_assistant?: string | null;
  is_subagent: boolean;
}

export type ConnectOpts = {
  password?: string;
  keyPassphrase?: string;
  acceptUnknownHost?: boolean;
  persistent?: boolean;
  mode?: "default" | "tmux" | "plain" | "cmd" | "claude";
  cwdOverride?: string;
  cmd?: string;
  claudeArgs?: string;
  // Phase 23.F: override the auto-derived tmux session name (picker).
  tmuxSession?: string;
};

export type { TmuxSessionInfo } from "./types";

export type PassphrasePending = { paneId: string; keyPath: string; bad?: boolean };
export type HostTrustPending = {
  paneId: string;
  target: string;
  keyType: string;
  fingerprint: string;
  mismatchOld?: string;
};


interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  // Phase 23.D: the workspace's canonical connection. `isSsh()` falls
  // back to this when the pane itself has no `connection` (FileManager
  // / Browser / ClaudeChat panes, or a fresh Terminal pane). Threaded
  // from App.tsx via LayoutView.
  workspaceConnection?: Connection;
  /** The workspace's own directory, when it has one — a pinned project
   *  folder or a worktree opened under it. Its presence is what turns
   *  the connection wizard into the scoped mini version. */
  workspaceCwd?: string;
  // Phase 23.I: the workspace name. The pane header falls back to it
  // when the user hasn't set a pane-specific title, replacing the
  // noisy "ssh user@host:port" auto-label.
  workspaceName?: string;
  // Phase 31: the workspace's identity, used to compute the effective
  // pane identity (pane override falls back to these). Drives the
  // pane header border + emoji prefix and the rename dialog's
  // "reset to inherit" hint.
  workspaceColor?: string;
  workspaceEmoji?: string;
  isActive: boolean;
  // Phase 65.T: focus/zoom mode. `isMaximized` = this pane currently
  // fills the workspace area (others run hidden in the background);
  // `backgroundPaneCount` = how many other panes are running while
  // focused (drives the "N in background" header badge).
  isMaximized?: boolean;
  backgroundPaneCount?: number;
  // Phase 84.A: this workspace renders its panes as a tab strip. The pane
  // only needs it to drop chrome that tabs already provide (maximize).
  tabsMode?: boolean;
  // Phase 84.B: the Claude traffic light, already resolved by LayoutView
  // (which has every input in scope). null = paint nothing.
  agentLight?: TrafficLight | null;
  agentWaitingOnPermission?: boolean;
  agentStateSince?: number | null;
  agentNowMs?: number;
  // Phase 26: pane is waiting on a blocking agent permission request
  // (a pending blocking feed item bound to this pane_id). Drives the
  // cmux-style pulsing notification ring around the pane.
  isWaiting?: boolean;
  // cmux-A A1: an OSC 9/99/777 terminal notification arrived for this
  // pane and it hasn't been focused since. Drives the amber activity
  // pulse (distinct from the waiting/blocking ring). Setting-gated in
  // the parent so we can render it as a plain boolean here.
  isNotified?: boolean;
  isConnected: boolean;
  pendingPasswordFor: string | null;
  pendingPassphrase: PassphrasePending | null;
  pendingHostTrust: HostTrustPending | null;
  status: { msg: string; err: boolean } | undefined;
  statusText?: string;
  // issue #4: this pane's agent turn timing + a reactive clock for the Ticker.
  agentRun?: { startedAt: number | null; avgMs: number | null };
  agentClockMs?: () => number;
  // Phase 11.A: when this pane is bound to a tmux session, the name. Used
  // to render the "T" badge and to enable "Kill session" in the menu.
  tmuxSession?: string | null;
  // Phase 91: the session this pane must attach to on [Connect] — set by
  // App for the first pane of a workspace with `tmux_session`. smartConnect
  // short-circuits on it: no probe, no picker, attach — or RESUME when the
  // host no longer lists the session and a Claude id is known. Null for an
  // ordinary pane.
  boundSession?: BoundSession | null;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  ensureTerm: (paneId: string, profile: RtlProfileKind) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: "horizontal" | "vertical") => void;
  onClose: (paneId: string) => void;
  // Unshipped-fivefer (#4): pop this pane's terminal into its own window.
  onPopOut: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
  // Phase 11.A: hard-kill the remote tmux session. No-op for plain panes.
  onKillSession: (paneId: string) => void;
}

export function PaneView(p: Props) {
  let slotRef!: HTMLDivElement;
  let paneRef!: HTMLDivElement;
  let ti: TerminalInstance | null = null;
  // Phase 49-A: drag-drop into terminal. dropping = visual highlight
  // (border) when a drag enters this pane's bounds; dropMsg = transient
  // status string shown over the pane while an upload is in flight.
  const [dropping, setDropping] = createSignal(false);
  const [dropMsg, setDropMsg] = createSignal<string | null>(null);
  const [pwInput, setPwInput] = createSignal("");
  const [passInput, setPassInput] = createSignal("");
  // Phase 7.A: edit mode for title/annotation.
  const [editingMeta, setEditingMeta] = createSignal(false);
  const [titleDraft, setTitleDraft] = createSignal("");
  const [annotDraft, setAnnotDraft] = createSignal("");
  // Phase 31: identity picker state, mirrors workspace-level picker.
  // `paneColor` / `paneEmoji` hold the pane's own override (None means
  // "inherit from workspace"). `customHex` is the editable field for
  // typing a custom color; reverts on blur if invalid.
  const [paneColor, setPaneColor] = createSignal<string | null>(null);
  const [paneEmoji, setPaneEmoji] = createSignal<string | null>(null);
  const [customHex, setCustomHex] = createSignal("");
  const COLOR_PRESETS = [
    "#1e40af", "#6d28d9", "#16a34a", "#ea580c",
    "#dc2626", "#ca8a04", "#0891b2", "#475569",
  ];
  const EMOJI_PRESETS = ["🟦", "🟣", "🟢", "🟠", "🔴", "🟡", "🔵", "⚪", "⬛"];
  const HEX_RE = /^#[0-9a-fA-F]{6}$/;
  const effective = () =>
    effectiveIdentity(
      { color: paneColor() ?? undefined, emoji: paneEmoji() ?? undefined },
      { color: p.workspaceColor, emoji: p.workspaceEmoji },
    );
  const saveIdentity = async (color: string | null, emoji: string | null) => {
    try {
      await invoke("pane_set_identity", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        color,
        emoji,
      });
      setPaneColor(color);
      setPaneEmoji(emoji);
      setCustomHex(color ?? "");
    } catch (e) {
      log.error("pane_set_identity failed", e);
    }
  };
  const pickColor = (hex: string) => {
    void saveIdentity(hex, paneEmoji());
  };
  const pickEmoji = (g: string) => {
    void saveIdentity(paneColor(), g);
  };
  const onCustomHexBlur = () => {
    const v = customHex().trim();
    if (v === "") {
      setCustomHex(paneColor() ?? "");
      return;
    }
    if (HEX_RE.test(v)) {
      void saveIdentity(v, paneEmoji());
    } else {
      setCustomHex(paneColor() ?? "");
    }
  };
  const onCustomEmojiInput = (v: string) => {
    const trimmed = v.slice(0, 8);
    setPaneEmoji(trimmed === "" ? null : trimmed);
  };
  const onCustomEmojiBlur = () => {
    void saveIdentity(paneColor(), paneEmoji());
  };
  const resetIdentity = () => {
    void saveIdentity(null, null);
  };
  const [showAnnot, setShowAnnot] = createSignal(false);
  // Phase 11.A: dropdown next to the disconnect button.
  const [showDiscMenu, setShowDiscMenu] = createSignal(false);
  // Phase 23.D: workspace dictates connection type. Check pane's own
  // connection first (set on wired Terminal panes), then fall back to
  // the workspace's canonical connection so SSH-only menu items
  // (tmux) show up from FM / Browser / Chat panes too.
  // What the pane's effective target CAN DO (own connection > workspace).
  // A workspace with its own directory (a pinned project folder, or a
  // worktree opened under one) anchors every pane it hosts. The wizard
  // drops its Directory field in that case: wandering out of the folder
  // is precisely what the sub-workspace exists to prevent.
  const folderAnchor = () => {
    const c = p.workspaceCwd?.trim();
    return c ? c : null;
  };
  const caps = () => paneCaps(p.pane, p.workspaceConnection);
  // Kept for the genuinely SSH-only bits — the SFTP directory picker, which
  // has no WSL backend yet. Do not reach for this to answer "does it have
  // tmux" or "can I exec there"; that is what `caps()` is for.
  const isSsh = () => isRemoteEffective(p.pane, p.workspaceConnection);
  // macOS port: a LOCAL workspace on mac has a local tmux server too —
  // `caps().sessionPersistence` says so (capsOf branches on the host OS), so
  // the Connect probe → picker and the tmux/regular toggle apply there.
  // These two only pick platform-specific copy / the native folder dialog;
  // SSH-only commands (ensure_connected, SFTP, ports) keep isSsh().
  const isLocalPane = () => isLocalConn(p.pane.connection ?? p.workspaceConnection);
  const isMacLocal = () => isMac() && isLocalPane();
  const isTmux = () => !!p.tmuxSession;
  // Phase 12.B Smart Connect — the "open in directory" text-input fallback
  // (local panes) still uses this small prompt.
  const [smartModal, setSmartModal] = createSignal<null | "cwd" | "cmd" | "claude_args">(null);
  const [smartInput, setSmartInput] = createSignal("");
  // v0.4.4 (Task 2): unified "new connection" picker. One modal to choose a
  // directory AND a launch command together, then connect — instead of the
  // à-la-carte menu that only ever set one at a time. Connect-time only,
  // nothing persisted. The backend build_smart_connect_script already
  // combines cwd_override + claude/cmd ("cd … && claude"), so this is pure
  // UI. Attaching to an existing tmux session stays a direct action (no
  // picker) via the separate tmux path below.
  type NcCmd =
    | "plain"
    | "claude"
    | "claude-continue"
    | "claude-resume"
    | "claude-skip"
    | "from-list"
    | "custom";
  // v0.4.4-beta.2: connection type is the FIRST step — "regular" = SSH → bare
  // shell; "tmux" = SSH → tmux new-session. Maps to the `persistent` flag
  // (regular=false, tmux=true); the command choice is orthogonal.
  type NcType = "regular" | "tmux";
  const [newConnModal, setNewConnModal] = createSignal(false);
  // v0.4.4-beta.2: the modal is a single shell with swappable views; the
  // header/footer/dimensions stay constant, only the body changes.
  type NcView = "form" | "browse";
  const [ncView, setNcView] = createSignal<NcView>("form");
  const [ncType, setNcType] = createSignal<NcType>("tmux");
  const [ncDir, setNcDir] = createSignal("");
  // Default command is empty ("plain" = inject nothing) — the type toggle
  // decides tmux vs regular; the real commands follow in the dropdown.
  const [ncCmd, setNcCmd] = createSignal<NcCmd>("plain");
  const [ncCustom, setNcCustom] = createSignal("");
  // v0.4.4-beta.2: the Claude-session list is shown ONLY for the dedicated
  // "choose from list" command — NOT for --resume/--continue (those are plain
  // runs). Filter is User / Agent / All (Agent = Task sidechain).
  type NcFilter = "user" | "agent" | "all";
  const [ncSessions, setNcSessions] = createSignal<ClaudeSessionInfo[]>([]);
  const [ncSessionsLoading, setNcSessionsLoading] = createSignal(false);
  const [ncSessionsErr, setNcSessionsErr] = createSignal<string | null>(null);
  const [ncSearch, setNcSearch] = createSignal("");
  const [ncFilter, setNcFilter] = createSignal<NcFilter>("user");
  const [ncPickedSession, setNcPickedSession] = createSignal<ClaudeSessionInfo | null>(null);
  const ncShowsList = (): boolean => ncCmd() === "from-list";
  // v0.4.4-beta.2: SMART [Connect] flow. Clicking Connect first arms the SSH
  // handle headlessly, probes for live tmux sessions, and — if any exist — pops
  // a small picker so the user can RE-ATTACH to one or open a plain terminal.
  // If none exist (or the workspace can't connect headlessly, e.g. password
  // auth) it falls straight through to a regular shell. Reconnect-to-open-tmux
  // lives here, not in the wizard, so the common case is one click.
  const [connectProbing, setConnectProbing] = createSignal(false);
  const [tmuxPick, setTmuxPick] = createSignal<TmuxSessionInfo[] | null>(null);
  // 2026-08-23: which slice of the host's sessions the picker shows.
  // "workspace" = this folder's sessions (owned by this workspace, or living
  // under its directory); "server" = everything the host is running. The
  // backend annotates every row with `owned` / `in_cwd` and never filters, so
  // flipping this is a pure client-side view change — no second round trip,
  // and a session outside the scope is one click away rather than gone.
  type TmuxScope = "workspace" | "server";
  const [tmuxScope, setTmuxScope] = createSignal<TmuxScope>("workspace");
  const inWorkspaceScope = (s: TmuxSessionInfo): boolean => s.owned || s.in_cwd;
  const tmuxPickVisible = (): TmuxSessionInfo[] => {
    const all = tmuxPick() ?? [];
    return tmuxScope() === "server" ? all : all.filter(inWorkspaceScope);
  };
  // Open on "this folder" only when that view can actually say something —
  // otherwise the picker greets the user with an empty list and a toggle they
  // have to discover in order to see their own sessions.
  const pickScopeDefault = (list: TmuxSessionInfo[]): TmuxScope =>
    list.some(inWorkspaceScope) ? "workspace" : "server";
  // 2026-08-23: the session this pane will land on, and whether it is already
  // running. Drives the wizard's attach-only banner. `null` = not asked yet.
  const [targetState, setTargetState] = createSignal<TargetSessionState | null>(null);
  // A live session is one we must NOT type into. `reachable` matters: when the
  // host could not be asked, `exists` is meaningless and we do not claim it.
  const targetIsLive = (): boolean => {
    const st = targetState();
    return !!st && st.reachable && st.exists;
  };
  const fmtSessionAge = (mt: number): string => {
    if (!mt) return "—";
    const sec = Math.max(1, Math.floor(Date.now() / 1000 - mt));
    if (sec < 60) return `${sec}s`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m`;
    if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
    return `${Math.floor(sec / 86400)}d`;
  };
  // issue #4: the Ticker label for this pane, e.g. "⏱ 3:10 · avg 40s".
  // Live elapsed ticks off agentClockMs() (reactive via pulseTick); avg is
  // fixed per turn. Empty string when the pane has no agent-run state.
  const clockMMSS = (sec: number): string =>
    `${Math.floor(sec / 60)}:${String(sec % 60).padStart(2, "0")}`;
  const agentRunLabel = (): string => {
    const run = p.agentRun;
    if (!run) return "";
    const parts: string[] = [];
    if (run.startedAt != null) {
      const clock = p.agentClockMs?.() ?? Date.now();
      parts.push(`⏱ ${clockMMSS(Math.max(0, Math.floor((clock - run.startedAt) / 1000)))}`);
    }
    if (run.avgMs != null) {
      const a = Math.round(run.avgMs / 1000);
      parts.push(`avg ${a < 60 ? `${a}s` : clockMMSS(a)}`);
    }
    return parts.join(" · ");
  };
  const loadNcSessions = async () => {
    setNcSessionsLoading(true);
    setNcSessionsErr(null);
    try {
      // Scoped to the folder when there is one. Not cosmetic: since
      // Claude Code 2.1.223 `--resume <id>` searches "the current
      // project directory and its git worktrees, THEN every other
      // project on this machine", so picking a foreign session does not
      // fail — it succeeds, resuming another repo's conversation inside
      // this folder.
      const list = await invoke<ClaudeSessionInfo[]>("pane_list_claude_sessions", {
        workspaceId: p.workspaceId,
        limit: 40,
        projectPath: folderAnchor(),
      });
      setNcSessions(list);
    } catch (e) {
      setNcSessionsErr(String(e));
    } finally {
      setNcSessionsLoading(false);
    }
  };
  const ncFilteredSessions = (): ClaudeSessionInfo[] => {
    const q = ncSearch().trim().toLowerCase();
    const f = ncFilter();
    return ncSessions().filter((s) => {
      if (f === "user" && s.is_subagent) return false;
      if (f === "agent" && !s.is_subagent) return false;
      if (!q) return true;
      return (
        s.session_id.toLowerCase().includes(q) ||
        (s.project_path ?? "").toLowerCase().includes(q) ||
        (s.last_user ?? "").toLowerCase().includes(q) ||
        (s.last_assistant ?? "").toLowerCase().includes(q)
      );
    });
  };
  // When true, the folder picker returns its choice into the new-connection
  // modal (ncDir) instead of connecting immediately.
  const [dirPickForNewConn, setDirPickForNewConn] = createSignal(false);
  // v0.4.4-beta.2: the tmux session picker + smart-connect caret menu were
  // removed — everything now lives in the two-button flow (Connect / Wizard).
  const submitSmartModal = () => {
    const m = smartModal();
    const v = smartInput();
    setSmartModal(null);
    setSmartInput("");
    if (m === "cwd") p.onConnect(p.pane.pane_id, { cwdOverride: v });
    if (m === "cmd") p.onConnect(p.pane.pane_id, { mode: "cmd", cmd: v });
    if (m === "claude_args") p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: v });
  };

  // Phase 65 (bug AA): "Open in directory" folder picker. Replaces the
  // bare text input — browse the remote tree (SFTP dir-list) with
  // drill-down + a recent-dirs shortcut list (per workspace,
  // localStorage). Local (non-SSH) panes keep the text-input fallback,
  // since file_list_remote needs an SSH session.
  const [dirPicker, setDirPicker] = createSignal<{
    path: string;
    dirs: string[];
    loading: boolean;
    error: string | null;
  } | null>(null);
  const recentDirsKey = () => `ymux.recent-dirs.${p.workspaceId}`;
  const loadRecentDirs = (): string[] => {
    try {
      const raw = localStorage.getItem(recentDirsKey());
      const parsed: unknown = raw ? JSON.parse(raw) : [];
      return Array.isArray(parsed)
        ? parsed.filter((x): x is string => typeof x === "string")
        : [];
    } catch {
      return [];
    }
  };
  const [recentDirs, setRecentDirs] = createSignal<string[]>([]);
  const pushRecentDir = (dir: string) => {
    const next = [dir, ...loadRecentDirs().filter((d) => d !== dir)].slice(0, 8);
    try {
      localStorage.setItem(recentDirsKey(), JSON.stringify(next));
    } catch {
      // quota / private mode — recents are best-effort
    }
    setRecentDirs(next);
  };
  const navigateDirPicker = async (path: string) => {
    setDirPicker({ path, dirs: [], loading: true, error: null });
    try {
      const list = await invoke<{ name: string; is_dir: boolean }[]>(
        "file_list_remote",
        { workspaceId: p.workspaceId, path, showHidden: false },
      );
      const dirs = list
        .filter((e) => e.is_dir)
        .map((e) => e.name)
        .sort((a, b) => a.localeCompare(b));
      setDirPicker({ path, dirs, loading: false, error: null });
    } catch (e) {
      setDirPicker({ path, dirs: [], loading: false, error: String(e) });
    }
  };
  const openDirPicker = async () => {
    setRecentDirs(loadRecentDirs());
    if (isLocalPane()) {
      // Local pane: the host's native folder dialog (Finder sheet on
      // macOS, Explorer on Windows) — the SFTP tree needs an SSH session.
      const dir = await pickLocalFolder();
      if (dir) chooseDir(dir);
      return;
    }
    if (!isSsh()) {
      // WSL pane: no SFTP and a host dialog would hand back a Windows
      // path the distro can't cd to — keep the text input.
      setSmartInput("");
      setSmartModal("cwd");
      return;
    }
    let start = "/";
    try {
      start = (await invoke<string>("file_home_remote", {
        workspaceId: p.workspaceId,
      })) || "/";
    } catch {
      start = "/";
    }
    void navigateDirPicker(start);
  };
  const dirPickerParent = (path: string): string => {
    const trimmed = path.replace(/\/+$/, "");
    const idx = trimmed.lastIndexOf("/");
    if (idx <= 0) return "/";
    return trimmed.slice(0, idx);
  };
  const dirPickerJoin = (path: string, name: string): string =>
    path === "/" ? `/${name}` : `${path.replace(/\/+$/, "")}/${name}`;
  const chooseDir = (dir: string) => {
    pushRecentDir(dir);
    setDirPicker(null);
    // v0.4.4-beta.2: the browser is now an inline VIEW of the new-connection
    // modal — feed the choice into ncDir and switch back to the form view
    // (the modal itself never closed).
    if (dirPickForNewConn()) {
      setDirPickForNewConn(false);
      setNcDir(dir);
      setNcView("form");
      return;
    }
    p.onConnect(p.pane.pane_id, { cwdOverride: dir });
  };
  // v0.4.4-beta.2: cancel the inline browser → back to the form (keep ncDir).
  const cancelBrowse = () => {
    setDirPicker(null);
    setDirPickForNewConn(false);
    setNcView("form");
  };
  // `closeDirPicker` lived here to dismiss the standalone picker and
  // reopen the new-connection modal behind it. It went with the picker
  // (now DirPicker.tsx) — the inline browse view is dismissed by
  // `cancelBrowse`, which returns to the form without ever having
  // closed the modal.
  // v0.4.4-beta.2: open the unified new-connection modal with defaults.
  const openNewConnModal = () => {
    setNcView("form");
    // 2026-08-23: reflect the pane instead of hard-coding "tmux". A pane
    // already mounted on a multiplexer session opens on TMUX because that IS
    // its state; one without persistence available should not be told it has
    // it. The old unconditional setNcType("tmux") is the first link in the
    // chain that ended with a command being typed into a live session.
    setNcType(isTmux() || caps().sessionPersistence ? "tmux" : "regular");
    setNcDir("");
    setNcCmd("plain");
    setNcCustom("");
    setNcSearch("");
    setNcFilter("user");
    setNcPickedSession(null);
    setNcSessions([]);
    setNcSessionsErr(null);
    setTargetState(null);
    setNewConnModal(true);
    // Ask BEFORE the user picks a command, so the banner is a warning rather
    // than an explanation of something that already went wrong. Arming the
    // SSH handle first is what makes the answer trustworthy — without it an
    // SSH workspace reports `reachable: false` and we would have to say
    // nothing at all.
    void (async () => {
      if (isSsh()) {
        try { await invoke("workspace_ensure_connected", { workspaceId: p.workspaceId }); } catch { /* fall through */ }
      }
      try {
        setTargetState(await invoke<TargetSessionState>("pane_target_session_state", {
          workspaceId: p.workspaceId,
          paneId: p.pane.pane_id,
          // Phase 91.G: fall back to the row's bound session name when the
          // pane is not locally attached (panePersistence is empty). Without
          // it the probe asks about the derived `ymux-<paneid>` name, reports
          // "not live" for a session that is actually running on the host, and
          // the wizard would enable a command the backend then drops.
          tmuxSessionName: p.tmuxSession ?? p.boundSession?.name ?? null,
        }));
      } catch (e) {
        log.warn("pane_target_session_state failed", e);
        setTargetState(null);
      }
    })();
  };
  // 2026-08-23: a command cannot be delivered when we are joining a session
  // that is already running — see the attach-only guard in pane_connect. The
  // command controls are disabled rather than hidden so the reason is visible.
  const attachOnly = (): boolean => ncType() === "tmux" && targetIsLive();
  // Validation: directory is OPTIONAL (empty = the user's $HOME root, the
  // backend default — fill it only to run elsewhere). custom needs text;
  // "choose from list" needs a session pick. --resume/--continue are plain runs.
  // A resumable session's cwd — POSIX (`/…`) or Windows (`C:\…`) absolute.
  // Anything else is the encoded `~/.claude/projects/<dir>` name, which is
  // display-only (never `cd` to it).
  const isAbsolutePath = (s: string | undefined | null): s is string =>
    !!s && (s.startsWith("/") || /^[A-Za-z]:[\\/]/.test(s));
  const newConnValid = (): boolean => {
    // 2026-08-23: under attach-only the command is not sent at all, so its
    // requirements are moot — demanding a session pick for a `--resume` that
    // will be discarded would just block Connect for no reason.
    if (attachOnly()) return true;
    if (ncCmd() === "custom" && !ncCustom().trim()) return false;
    if (ncCmd() === "from-list" && !ncPickedSession()) return false;
    return true;
  };
  // v0.4.4-beta.2: browse is now an INLINE view within the same modal (not a
  // separate popup). Load the tree into dirPicker() and switch the body.
  // Native folder chooser for LOCAL panes. Returns null on cancel/error;
  // the dialog itself is the UI, so no inline browse view is needed.
  const pickLocalFolder = async (): Promise<string | null> => {
    try {
      const picked = await openNativeDialog({
        directory: true,
        multiple: false,
        defaultPath: ncDir().trim() || undefined,
      });
      return typeof picked === "string" && picked ? picked : null;
    } catch (e) {
      log.warn("native folder dialog failed", e);
      return null;
    }
  };
  const browseNewConnDir = () => {
    if (isLocalPane()) {
      // Local: native dialog straight into ncDir — the modal stays on
      // the form view (no SFTP tree to render).
      void pickLocalFolder().then((dir) => {
        if (dir) {
          pushRecentDir(dir);
          setNcDir(dir);
        }
      });
      return;
    }
    setDirPickForNewConn(true);
    setNcView("browse");
    void openDirPicker();
  };
  // v0.4.4-beta.2: auto-load the Claude session list when "choose from list"
  // is picked while the modal is open (once; refresh via the list's ⟳).
  createEffect(() => {
    if (newConnModal() && ncShowsList() && ncSessions().length === 0 && !ncSessionsLoading()) {
      void loadNcSessions();
    }
  });
  // v0.4.4-beta.2: SMART [Connect]. Arm the target headlessly → probe tmux →
  // branch: live sessions → picker; otherwise a plain regular shell. A target
  // with no tmux (a Windows shell) connects straight away.
  //
  // Gated on sessionPersistence, not "is it SSH". A WSL pane keeps tmux sessions
  // like a remote one, and the old `!isSsh()` branch connected it with
  // `persistent: false` — i.e. no tmux at all. That silently defeated session
  // restore at the source: there was never a session left behind to come back
  // to, no matter what the restore loop did on the next boot.
  const smartConnect = async () => {
    if (!caps().sessionPersistence) { p.onConnect(p.pane.pane_id, { persistent: false }); return; }
    // Phase 91: a pane that already knows its session attaches to it —
    // this is what makes a session row / a strip pane honest after a
    // restart. Before this, the probe below opened the picker on any host
    // with a session, and App's tmux_session fallback never got a turn.
    const bound = p.boundSession;
    if (bound) {
      // A gone session with a Claude id comes back running `claude --resume`
      // in the cwd it lived in; `new-session -A` recreates the session under
      // the same name. Without an id it is just a fresh session of that name.
      // If the session reappeared between the poll and this click,
      // pane_connect's attach-only guard types nothing into it.
      p.onConnect(
        p.pane.pane_id,
        bound.gone && bound.claudeSessionId
          ? {
              persistent: true,
              tmuxSession: bound.name,
              mode: "claude",
              claudeArgs: `--resume ${bound.claudeSessionId}`,
              cwdOverride: bound.cwd ?? undefined,
            }
          : { persistent: true, tmuxSession: bound.name },
      );
      return;
    }
    setConnectProbing(true);
    try {
      // Idempotent, PTY-free, tmux-free; no-ops on password-auth (can't prompt
      // headlessly) — those simply yield an empty list and connect regular.
      if (isSsh()) {
        try { await invoke("workspace_ensure_connected", { workspaceId: p.workspaceId }); } catch { /* fall through */ }
      }
      let list: TmuxSessionInfo[] = [];
      try {
        // 2026-08-23: hand the backend the folder anchor so it can stamp
        // `in_cwd`. It still returns EVERY session — the scope toggle below
        // decides what is shown — which is why the emptiness test stays on
        // the full list: opening straight into a regular shell because this
        // folder has no sessions would hide the ones that do exist.
        list = await invoke<TmuxSessionInfo[]>("pane_list_tmux_sessions", {
          workspaceId: p.workspaceId,
          projectPath: folderAnchor(),
        });
      } catch { list = []; }
      if (list.length > 0) {
        setTmuxScope(pickScopeDefault(list));
        setTmuxPick(list);
      } else {
        // Omit `persistent` rather than forcing false: the backend applies
        // the target's own default (SSH false, WSL true — pane_connect).
        // Forcing false here meant a fresh WSL pane came up WITHOUT tmux, so
        // nothing was ever left behind and the next boot had nothing to
        // restore. SSH behaviour is unchanged — its default is false anyway.
        p.onConnect(p.pane.pane_id, {});
      }
    } finally {
      setConnectProbing(false);
    }
  };
  // Attach to a chosen live tmux session (persistent + name; inject nothing —
  // its shell is already running), or open a plain regular shell.
  const pickTmuxSession = (name: string | null) => {
    setTmuxPick(null);
    if (name) p.onConnect(p.pane.pane_id, { persistent: true, tmuxSession: name });
    // "None of these" → the target's default persistence, same reasoning as
    // smartConnect above.
    else p.onConnect(p.pane.pane_id, {});
  };
  // 2026-08-23: the explicit way OUT of attach-only. Rather than typing into
  // the running session, make a NEW one beside it — `<name>-2`, `-3`, … — so
  // `tmux new-session -A` creates instead of joining and the user's command
  // runs in a shell of its own. The free suffix is chosen against the live
  // list because guessing one that is already taken would just land us back
  // in attach-only, silently.
  const connectAsNewSession = async () => {
    const base = targetState()?.name ?? p.pane.pane_id;
    let taken: string[] = [];
    try {
      const list = await invoke<TmuxSessionInfo[]>("pane_list_tmux_sessions", {
        workspaceId: p.workspaceId,
        projectPath: null,
      });
      taken = list.map((s) => s.name);
    } catch { /* an empty list only risks a retry, never a wrong write */ }
    let name = base;
    for (let n = 2; taken.includes(name); n++) name = `${base}-${n}`;
    setNewConnModal(false);
    p.onConnect(p.pane.pane_id, buildNewConnOpts({ tmuxSession: name }));
  };
  // Translate the modal's choices into a single ConnectOpts and connect.
  const submitNewConn = () => {
    if (!newConnValid()) return;
    setNewConnModal(false);
    p.onConnect(p.pane.pane_id, buildNewConnOpts({}));
  };
  const buildNewConnOpts = (extra: Partial<ConnectOpts>): ConnectOpts => {
    const opts: ConnectOpts = { ...extra };
    // Connection type → persistent flag (tmux=true, regular=false). We do NOT
    // use mode="tmux"/"plain" here: those force the persistence, which would
    // fight the toggle (e.g. a bare-shell command inside a tmux session). The
    // backend's effective_persistent honors the flag when mode isn't tmux/plain.
    opts.persistent = ncType() === "tmux";
    const c = ncCmd();
    const picked = ncPickedSession();
    // A picked resume session normally overrides the directory with its
    // own project path (so resume lands where the session was created).
    // Inside a folder-anchored workspace that is exactly wrong — the
    // pane must stay in the folder, and the list is already scoped to
    // it, so the override is skipped rather than fought.
    const anchor = folderAnchor();
    let dir = anchor ?? ncDir().trim();
    // `isAbsolutePath`, not `startsWith("/")`: a Windows project path
    // (`C:\...`) is absolute too and failed the old test by accident.
    if (!anchor && picked && isAbsolutePath(picked.project_path)) {
      dir = picked.project_path;
    }
    // Empty stays empty: no cwdOverride → the backend lands in the user's $HOME
    // root (default). Only send an override when the user actually typed a path
    // (or a picked session supplied its project dir).
    if (dir) opts.cwdOverride = dir;
    // 2026-08-23: joining a session that is already running means NOTHING is
    // typed into it — not the command, and not the `cd` either. `cd /srv/app`
    // landing in a live `claude` is the same bug with a shorter payload. The
    // backend enforces this too (the attach-only guard in pane_connect); doing
    // it here as well keeps the request honest about what it is asking for.
    if (attachOnly() && !extra.tmuxSession) {
      delete opts.cwdOverride;
      return opts;
    }
    if (c === "plain") {
      // Bare shell — inject nothing; mode stays undefined so the persistent
      // flag alone decides tmux vs regular.
    } else if (c === "custom") {
      opts.mode = "cmd";
      opts.cmd = ncCustom().trim();
    } else {
      opts.mode = "claude";
      if (picked) opts.claudeArgs = `--resume ${picked.session_id}`;
      else if (c === "claude-continue") opts.claudeArgs = "--continue";
      else if (c === "claude-resume") opts.claudeArgs = "--resume";
      else if (c === "claude-skip") opts.claudeArgs = "--dangerously-skip-permissions";
    }
    return opts;
  };
  const openMeta = () => {
    setTitleDraft(p.pane.title ?? "");
    setAnnotDraft(p.pane.annotation ?? "");
    // Phase 31: hydrate identity from the pane prop (the source of
    // truth between dialog opens). Falls through to None when the
    // pane has no override and is inheriting from the workspace.
    setPaneColor(p.pane.color ?? null);
    setPaneEmoji(p.pane.emoji ?? null);
    setCustomHex(p.pane.color ?? "");
    setEditingMeta(true);
  };
  const saveMeta = () => {
    const newTitle = titleDraft();
    const newAnnot = annotDraft();
    if ((p.pane.title ?? "") !== newTitle)
      p.onSetTitle(p.pane.pane_id, newTitle);
    if ((p.pane.annotation ?? "") !== newAnnot)
      p.onSetAnnotation(p.pane.pane_id, newAnnot);
    setEditingMeta(false);
  };

  // Phase 35 (#1.3): command-palette "pane.rename" dispatches this
  // window event with the target pane id; the matching pane opens its
  // title/annotation editor. Lightweight cross-component trigger that
  // avoids prop-drilling a rename request down from App.
  const onRenameRequest = (e: Event) => {
    const detail = (e as CustomEvent).detail;
    if (detail === p.pane.pane_id) openMeta();
  };

  // Phase 49-A: POSIX single-quote escape for paths typed into the
  // shell. `'foo bar'` is literal; an embedded ' is closed, escaped,
  // and re-opened: foo'bar → 'foo'\''bar'. Safe for any byte sequence.
  const posixQuote = (s: string): string =>
    `'${s.replace(/'/g, `'\\''`)}'`;

  // Effective connection for this pane — pane override beats workspace
  // default. Used to route drops to SFTP (SSH) vs. local-path passthrough.
  const effectiveConn = (): Connection | null =>
    p.pane.connection ?? p.workspaceConnection ?? null;
  const isSshPane = () => isRemoteConn(effectiveConn());

  // Phase 49-A: turn one dropped file path into a string suitable for
  // pty_write. SSH workspaces uploaded via SFTP; the returned remote
  // path is what gets typed. Local panes type the host path verbatim.
  const handleOneDrop = async (hostPath: string): Promise<string | null> => {
    const basename =
      hostPath.split(/[\\/]/).filter(Boolean).pop() || "dropped";
    if (!isSshPane()) {
      return hostPath;
    }
    try {
      setDropMsg(t("pane.drop.uploading", { name: basename }));
      const remote = await invoke<string>("pane_upload_dropped", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        localPath: hostPath,
        fileName: basename,
      });
      setDropMsg(t("pane.drop.uploaded", { name: basename }));
      return remote;
    } catch (e) {
      log.error("pane_upload_dropped failed", e);
      setDropMsg(t("pane.drop.failed", { name: basename, err: String(e) }));
      return null;
    }
  };

  // Phase 49-A: hit-test helper; returns true if (x, y) — in CSS px —
  // sits inside the pane's bounding box. Tauri drag positions arrive
  // in physical px; caller divides by DPR before invoking.
  const pointInPane = (x: number, y: number): boolean => {
    if (!paneRef) return false;
    const r = paneRef.getBoundingClientRect();
    return x >= r.left && x < r.right && y >= r.top && y < r.bottom;
  };

  const writeToPty = (s: string) => {
    if (!ti?.sessionId) return;
    void invoke("pty_write", { sessionId: ti.sessionId, data: s }).catch(
      (e) => log.error("pty_write failed", e),
    );
  };

  // beta.3 (pane-dragdrop): terminal attach is a createEffect so a
  // pane_id swap (workspace_swap_panes moves the two Pane leaves in
  // the layout tree — same tree slots, different pane_ids in them)
  // detaches the previous terminal container and attaches the new
  // one keyed to the new pane_id. Under the pre-dragdrop code this
  // was in onMount() and would stick to the first pane_id, leaving
  // the wrong xterm mounted after a swap. The xterm instance itself
  // survives in the g_terminals registry across detach/reattach.
  createEffect(() => {
    const paneId = p.pane.pane_id;
    if (!slotRef) return;
    const nextTi = p.ensureTerm(paneId, profileFor(effectiveConn()));
    if (ti && ti !== nextTi) {
      // Detach previous terminal's container from THIS slot before
      // hooking up the new one. If it was moved elsewhere already
      // (the other PaneView's effect ran first), parentElement will
      // be that slot — leave it alone.
      if (ti.container.parentElement === slotRef) {
        slotRef.removeChild(ti.container);
      }
    }
    ti = nextTi;
    if (ti.container.parentElement !== slotRef) {
      // If the container is currently hosted in the OTHER slot (mid-
      // swap), detach it there so appendChild here moves it cleanly.
      ti.container.parentElement?.removeChild(ti.container);
      slotRef.appendChild(ti.container);
    }
    ti.container.style.display = "block";
    requestAnimationFrame(() => ti?.fitAndResize());
  });

  onMount(() => {
    window.addEventListener("ymux:pane-rename", onRenameRequest);

    // Phase 49-A: subscribe to the window-wide drag-drop event. Each
    // PaneView registers its own listener and hit-tests against its own
    // bounding rect, so multi-pane layouts route the drop to whichever
    // pane the cursor was over. File-manager panes register their own
    // listener at a different on-screen location, so there's no double
    // claim. The webview consumes file drops at the OS level, so this
    // handler is the only path for OS-file drops; the HTML5 ondrop on
    // the pane div picks up text/URL drags from the browser.
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        unlisten = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload as
            | { type: "enter" | "over"; position: { x: number; y: number } }
            | { type: "drop"; paths: string[]; position: { x: number; y: number } }
            | { type: "leave" };
          if (payload.type === "leave") {
            setDropping(false);
            return;
          }
          // Windows (WebView2) reports the drop position in physical
          // pixels, so it has to be scaled down to compare against
          // CSS-pixel rects. macOS (wry/wkwebview) passes NSDraggingInfo's
          // draggingLocation through unscaled — logical points already —
          // so dividing there halved every coordinate on a Retina screen
          // and the pane hit-test silently missed.
          const scale = isMac() ? 1 : window.devicePixelRatio || 1;
          const x = payload.position.x / scale;
          const y = payload.position.y / scale;
          const inside = pointInPane(x, y);
          if (payload.type === "enter" || payload.type === "over") {
            setDropping(inside);
            return;
          }
          setDropping(false);
          if (payload.type !== "drop" || !inside) return;
          const paths = payload.paths || [];
          if (paths.length === 0) return;
          void (async () => {
            for (const hostPath of paths) {
              const typed = await handleOneDrop(hostPath);
              if (typed) writeToPty(posixQuote(typed) + " ");
            }
            // Clear the toast after a short grace so the user sees it.
            setTimeout(() => setDropMsg(null), 1800);
          })();
        });
      } catch (e) {
        log.warn("onDragDropEvent failed", e);
      }
    })();

    // Cleanup for the async-assigned unlisten.
    onCleanup(() => {
      try { unlisten?.(); } catch {}
    });
  });

  onCleanup(() => {
    window.removeEventListener("ymux:pane-rename", onRenameRequest);
    if (ti && ti.container.parentElement === slotRef) {
      ti.container.parentElement.removeChild(ti.container);
    }
  });

  // Phase 49-A: HTML5 drop for non-file drags (URLs / text dragged
  // from browser tabs). Tauri's onDragDropEvent only fires for OS-level
  // file drops, so URLs need this fallback. URI-list takes priority,
  // then plain text. Same rule: type the string + SPACE.
  const onHtml5Drop = (e: DragEvent) => {
    if (!e.dataTransfer) return;
    // If files are present, Tauri's handler already routed them; bail.
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) return;
    const uri = e.dataTransfer.getData("text/uri-list").trim();
    const txt = uri || e.dataTransfer.getData("text/plain").trim();
    if (!txt) return;
    e.preventDefault();
    setDropping(false);
    writeToPty(posixQuote(txt) + " ");
  };
  const onHtml5DragOver = (e: DragEvent) => {
    // Allow drop. Don't preventDefault for file drops or Tauri's
    // OS-level handler won't see them.
    if (e.dataTransfer?.types?.includes("text/uri-list") ||
        e.dataTransfer?.types?.includes("text/plain")) {
      e.preventDefault();
    }
  };

  const passphraseHere = () =>
    p.pendingPassphrase && p.pendingPassphrase.paneId === p.pane.pane_id
      ? p.pendingPassphrase
      : null;

  const hostTrustHere = () =>
    p.pendingHostTrust && p.pendingHostTrust.paneId === p.pane.pane_id
      ? p.pendingHostTrust
      : null;

  // Phase 31: live effective identity — recomputed when pane props
  // change OR when the user picks something in the open editor.
  const liveEffective = () => {
    const e = effective();
    return {
      color: p.pane.color ?? e.color,
      emoji: p.pane.emoji ?? e.emoji,
    };
  };
  // beta.3 (pane-dragdrop): reactive drop-zone classes for this pane.
  // A pane can be either the drag SOURCE (.pane-dragging → dim) or
  // the drag TARGET (.pane-drop-target → outline + zone-specific
  // .pane-drop-{center|left|right|top|bottom} for a half-tint hint).
  // MVP: only center performs the swap on release — half-zone visuals
  // hint the future split-creation but currently fall back to swap.
  const paneDragClasses = (): string => {
    const cls: string[] = [];
    if (paneDragStore.dragPaneId() === p.pane.pane_id) cls.push("pane-dragging");
    if (paneDragStore.dropTargetId() === p.pane.pane_id) {
      cls.push("pane-drop-target");
      const z = paneDragStore.dropZone();
      if (z) cls.push(`pane-drop-${z}`);
    }
    return cls.join(" ");
  };

  // ── header overflow (priority+ pattern) ──────────────────────────────
  // The header is a no-wrap flex row and `.pane` is `overflow:hidden`, so
  // with many panes open the right-hand buttons used to be silently
  // clipped — no hint they existed. Every button now lives in this array;
  // `visibleCount()` decides how many render inline and the rest fall into
  // a chevron menu. `close` deliberately stays OUT of the array (rendered
  // after the chevron) so the one action you can't afford to lose never
  // moves.
  type HeaderAction = {
    id: string;
    /** inline tooltip */
    title: string;
    /** label in the overflow menu */
    label: string;
    icon: () => JSX.Element;
    active?: boolean;
    run?: () => void;
    /** custom inline form (used by the disconnect split-button) */
    render?: () => JSX.Element;
    /** what this becomes inside the menu when it overflows */
    menuItems?: { label: string; danger?: boolean; run: () => void }[];
  };

  const [visibleCount, setVisibleCount] = createSignal(99);
  const [showOverflow, setShowOverflow] = createSignal(false);
  let headerRef: HTMLDivElement | undefined;

  const actions = createMemo<HeaderAction[]>(() => {
    const list: HeaderAction[] = [];
    if (p.pane.annotation) {
      list.push({
        id: "annot",
        title: t("pane.tooltip.show_annotation"),
        label: t("pane.tooltip.show_annotation"),
        icon: () => <IconInfo size={14} />,
        // NOTE: deliberately does not read showAnnot() — `actions()` is a
        // memo and any signal it reads rebuilds the whole button row.
        run: () => setShowAnnot(!showAnnot()),
      });
    }
    list.push({
      id: "meta",
      title: t("pane.tooltip.edit_meta"),
      label: t("pane.tooltip.edit_meta"),
      icon: () => <IconPencil size={14} />,
      run: openMeta,
    });
    if (p.isConnected) {
      // Atomic item: the inline form keeps the existing power+caret
      // split-button (and its own menu) untouched; in the overflow menu
      // it expands into its two entries instead.
      list.push({
        id: "disc",
        title: isTmux() ? t("pane.tooltip.detach") : t("pane.tooltip.disconnect"),
        label: isTmux() ? t("common.detach") : t("common.disconnect"),
        icon: () => <IconPower size={14} />,
        render: () => renderDiscButton(),
        menuItems: [
          {
            label: isTmux() ? t("common.detach") : t("common.disconnect"),
            run: () => p.onDisconnect(p.pane.pane_id),
          },
          ...(isTmux()
            ? [{
                label: t("common.kill_session"),
                danger: true,
                run: () => p.onKillSession(p.pane.pane_id),
              }]
            : []),
        ],
      });
    }
    list.push({
      id: "bidi",
      title:
        t(p.pane.smart_bidi === true ? "pane.smartBidi.on" : "pane.smartBidi.off")
        + " — " + t("pane.smartBidi.hint"),
      label: t(p.pane.smart_bidi === true ? "pane.smartBidi.on" : "pane.smartBidi.off"),
      icon: () => <IconArrowLeftRight size={14} />,
      active: p.pane.smart_bidi === true,
      run: () => {
        const next = !(p.pane.smart_bidi === true);
        void invoke("pane_set_smart_bidi", {
          workspaceId: p.workspaceId,
          paneId: p.pane.pane_id,
          enabled: next,
        }).catch((err) => log.error("pane_set_smart_bidi failed", err));
      },
    });
    // Phase 84.A: in tabs mode this pane already fills the workspace, so
    // the button is a no-op. Dropping it also gives the overflow fitter
    // one less button to place.
    if (!p.tabsMode) {
      list.push({
        id: "maximize",
        title: p.isMaximized ? t("pane.tooltip.restore") : t("pane.tooltip.focus"),
        label: p.isMaximized ? t("pane.tooltip.restore") : t("pane.tooltip.focus"),
        icon: () => (p.isMaximized ? <IconMinimize size={14} /> : <IconMaximize size={14} />),
        active: p.isMaximized,
        run: () => {
          window.dispatchEvent(
            new CustomEvent("ymux:pane-maximize", {
              detail: { paneId: p.pane.pane_id },
            }),
          );
        },
      });
    }
    list.push({
      id: "split-h",
      title: t("pane.tooltip.split_right"),
      label: t("pane.tooltip.split_right"),
      icon: () => <IconColumns size={14} />,
      run: () => p.onSplit(p.pane.pane_id, "horizontal"),
    });
    list.push({
      id: "split-v",
      title: t("pane.tooltip.split_down"),
      label: t("pane.tooltip.split_down"),
      icon: () => <IconRows size={14} />,
      run: () => p.onSplit(p.pane.pane_id, "vertical"),
    });
    if (p.isConnected) {
      list.push({
        id: "popout",
        title: t("pane.tooltip.popout"),
        label: t("pane.tooltip.popout"),
        icon: () => <IconExternalLink size={14} />,
        run: () => void p.onPopOut(p.pane.pane_id),
      });
    }
    return list;
  });

  const hiddenActions = createMemo(() => actions().slice(visibleCount()));

  // Sum of the laid-out children + gaps. Deliberately NOT `scrollWidth`:
  // in an RTL header the run overflows towards the inline start, and
  // scrollWidth's behaviour there is the historically messy corner of the
  // API. Summing offsetWidth is direction-agnostic and exact — once the
  // flex items are at min-content, the sum exceeds clientWidth by however
  // much is being clipped. The absolutely-positioned dropdowns live inside
  // `position:relative` wrappers, so they contribute nothing here.
  const contentWidth = (el: HTMLElement): number => {
    const kids = el.children;
    if (kids.length === 0) return 0;
    const gap = parseFloat(getComputedStyle(el).columnGap) || 0;
    let w = gap * (kids.length - 1);
    for (let i = 0; i < kids.length; i++) w += (kids[i] as HTMLElement).offsetWidth;
    return w;
  };

  /**
   * The width the children actually get: `clientWidth` is the PADDING
   * box, so comparing a sum of child widths against it hands the run an
   * extra `padding-inline` of slack (20px here) and leaves that much
   * still overflowing. The header used to clip, which hid the mistake.
   */
  const availableWidth = (el: HTMLElement): number => {
    const cs = getComputedStyle(el);
    const pad = (parseFloat(cs.paddingInlineStart) || 0) + (parseFloat(cs.paddingInlineEnd) || 0);
    return el.clientWidth - pad;
  };

  // Shrink until the header stops overflowing. Solid applies signal writes
  // to the DOM synchronously, so each setVisibleCount is reflected in the
  // next measurement. The loop is self-correcting: as soon as n < total the
  // chevron renders and adds its own width to the measure. Bounded by
  // actions().length (<= 8), and only ever runs from rAF.
  const fit = () => {
    const el = headerRef;
    if (!el) return;
    let n = actions().length;
    setVisibleCount(n);
    const avail = availableWidth(el);
    while (n > 0 && contentWidth(el) > avail + 1) {
      n -= 1;
      setVisibleCount(n);
    }
  };

  let fitFrame = 0;
  const scheduleFit = () => {
    if (fitFrame) return;
    fitFrame = requestAnimationFrame(() => {
      fitFrame = 0;
      fit();
    });
  };

  onMount(() => {
    if (!headerRef) return;
    const ro = new ResizeObserver(scheduleFit);
    ro.observe(headerRef);
    onCleanup(() => {
      ro.disconnect();
      if (fitFrame) cancelAnimationFrame(fitFrame);
    });
  });

  // Re-fit when anything that occupies header width appears or
  // disappears without a resize event: the action set itself
  // (connect/disconnect, annotation, pop-out) and the non-action badges.
  // `agentRunLabel()` is deliberately NOT tracked — it ticks every second
  // and its width is fixed by `font-variant-numeric: tabular-nums`.
  createEffect(() => {
    actions().length;
    !!p.statusText;
    isTmux();
    p.isMaximized && (p.backgroundPaneCount ?? 0) > 0;
    scheduleFit();
  });

  // Close the overflow menu on an outside click or Escape.
  createEffect(() => {
    if (!showOverflow()) return;
    const onDocDown = (e: MouseEvent) => {
      const t2 = e.target as HTMLElement | null;
      if (t2?.closest(".pane-overflow-wrap")) return;
      setShowOverflow(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowOverflow(false);
    };
    document.addEventListener("mousedown", onDocDown, true);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("mousedown", onDocDown, true);
      document.removeEventListener("keydown", onKey);
    });
  });

  const renderDiscButton = () => (
    <div class="pane-disc-wrap">
      <button
        class="pane-btn"
        title={isTmux() ? t("pane.tooltip.detach") : t("pane.tooltip.disconnect")}
        onClick={() => p.onDisconnect(p.pane.pane_id)}
      >
        <IconPower size={14} />
      </button>
      <button
        class="pane-btn pane-disc-caret"
        title={t("pane.tooltip.kill_session")}
        onClick={(e) => {
          e.stopPropagation();
          setShowDiscMenu(!showDiscMenu());
        }}
      >
        <IconChevronDown size={13} />
      </button>
      <Show when={showDiscMenu()}>
        <div
          class="pane-disc-menu"
          onClick={(e) => {
            e.stopPropagation();
            setShowDiscMenu(false);
          }}
        >
          <button onClick={() => p.onDisconnect(p.pane.pane_id)}>
            {isTmux() ? t("common.detach") : t("common.disconnect")}
          </button>
          <Show when={isTmux()}>
            <button class="danger" onClick={() => p.onKillSession(p.pane.pane_id)}>
              {t("common.kill_session")}
            </button>
          </Show>
        </div>
      </Show>
    </div>
  );

  return (
    <div
      ref={(el) => (paneRef = el)}
      data-pane-id={p.pane.pane_id}
      class={`pane ${p.isActive ? "active" : ""} ${p.isWaiting ? "waiting" : ""} ${p.isNotified ? "pane-pulse" : ""} ${dropping() ? "drop-target" : ""} ${paneDragClasses()}`}
      data-has-color={liveEffective().color ? "true" : "false"}
      style={liveEffective().color ? `--pane-color: ${liveEffective().color}` : undefined}
      onMouseDown={() => {
        // A short click on the header still focuses the pane. A completed
        // drag sets `didDrag` in paneDrag — but focusing during a drag
        // is harmless and, in fact, matches the sidebar's UX (the source
        // stays selected). The workspace_swap_panes command keeps
        // pane_ids stable, so this focus survives the swap unchanged.
        p.onFocus(p.pane.pane_id);
      }}
      onDrop={onHtml5Drop}
      onDragOver={onHtml5DragOver}
      onDblClick={(e) => {
        // Phase 55-A: maximize toggle on content double-click. Skip
        // when the click landed inside the xterm canvas — xterm's own
        // double-click handler uses that for word-selection. Skip the
        // header too (which has its own rename / connect actions
        // bound to clicks).
        const target = e.target as HTMLElement;
        if (target.closest(".xterm")) return;
        if (target.closest(".pane-header")) return;
        if (target.closest(".pane-drop-toast")) return;
        window.dispatchEvent(
          new CustomEvent("ymux:pane-maximize", {
            detail: { paneId: p.pane.pane_id },
          })
        );
      }}
    >
      {/* Redesign / Industry direction: blueprint registration marks at the
          four pane corners. Rendered always (cheap, empty), shown only under
          [data-theme-preset^="industry"] via themes-redesign.css. */}
      <div class="pane-marks" aria-hidden="true">
        <i class="pane-mark tl" />
        <i class="pane-mark tr" />
        <i class="pane-mark bl" />
        <i class="pane-mark br" />
      </div>
      <Show when={dropMsg()}>
        <div class="pane-drop-toast">{dropMsg()}</div>
      </Show>
      <div
        class="pane-header"
        ref={(el) => (headerRef = el)}
        onPointerDown={(e) => {
          // beta.3 (pane-dragdrop) Fix 1: the whole header is the drag
          // surface (was just the title span — too small to hit).
          // startPaneDrag is left-button-only and bails on interactive
          // children (buttons / .pane-btn), so their clicks keep working.
          const label =
            p.pane.title
              ?? p.pane.auto_title
              ?? p.workspaceName
              ?? (p.pane.connection
                ? describeConnection(p.pane.connection)
                : p.workspaceConnection
                  ? describeConnection(p.workspaceConnection)
                  : p.pane.pane_id);
          startPaneDrag(p.pane.pane_id, label, e);
        }}
      >
        {/* Phase 23.I: header fallback chain — user-set pane.title
            beats workspace name beats the raw SSH URL. The old
            describeConnection() output (e.g. "ssh runner@1.2.3.4:22")
            was noisy and only useful for debugging.
            Phase 31: prepend the effective emoji glyph when set.
            beta.3 (pane-dragdrop): this span is also the pane's drag
            handle — pointerdown starts a pointer-drag reorder. A short
            press stays a click (pane focus + no swap); a >5px move
            promotes to a drag and drops on the pane under the cursor.
            Escape / pointercancel abort with no swap. */}
        <span
          class="pane-conn"
          title={
            p.pane.connection
              ? describeConnection(p.pane.connection)
              : p.workspaceConnection
                ? describeConnection(p.workspaceConnection)
                : undefined
          }
        >
          <Show when={liveEffective().emoji}>
            <span class="pane-emoji">{liveEffective().emoji}</span>{" "}
          </Show>
          {/* Phase 81: auto_title = Claude-derived session title (stop
              hook). Sits between the manual title and the workspace
              name — manual always wins. */}
          <TechText text={
            p.pane.title
              ?? p.pane.auto_title
              ?? p.workspaceName
              ?? (p.pane.connection
                ? describeConnection(p.pane.connection)
                : p.workspaceConnection
                  ? describeConnection(p.workspaceConnection)
                  : "—")
          } />
        </span>
        <Show when={p.statusText}>
          <span class="pane-status-text">{p.statusText}</span>
        </Show>
        {/* Phase 84.B: the traffic light sits immediately before the
            ticker, giving the header a fixed order — title, statusText,
            light, ticker, badges, buttons. A stable slot matters: the
            ticker's width changes every second as digits roll over, and
            a light that slid with it would be unreadable at a glance. */}
        <AgentLight
          light={p.agentLight ?? null}
          waitingOnPermission={p.agentWaitingOnPermission ?? false}
          stateSince={p.agentStateSince ?? null}
          nowMs={p.agentNowMs ?? 0}
        />
        <Show when={agentRunLabel()}>
          <span class="pane-agent-run">{agentRunLabel()}</span>
        </Show>
        {/* Badges are not actions — they never move into the overflow
            menu. They sit between the title and the button run so the
            fitted button row starts at a stable offset. */}
        <Show when={isTmux()}>
          <span
            class="pane-tmux-badge"
            title={t("pane.tooltip.tmux_badge")}
          >
            T
          </span>
        </Show>
        {/* Phase 65.T: focus/zoom badge. Shows how many panes keep
            running in the background while this one is focused. */}
        <Show when={p.isMaximized && (p.backgroundPaneCount ?? 0) > 0}>
          <span
            class="pane-bg-badge"
            title={t("pane.tooltip.background_panes", {
              count: String(p.backgroundPaneCount ?? 0),
            })}
          >
            <IconMaximize size={13} /> {p.backgroundPaneCount}
          </span>
        </Show>
        {/* Fitted button run — see `actions()` / `fit()` above. */}
        <For each={actions().slice(0, visibleCount())}>
          {(a) => (
            <Show
              when={!a.render}
              fallback={a.render?.()}
            >
              <button
                class={`pane-btn ${a.active ? "active" : ""}`}
                title={a.title}
                onClick={(e) => {
                  e.stopPropagation();
                  a.run?.();
                }}
              >
                {a.icon()}
              </button>
            </Show>
          )}
        </For>
        <Show when={hiddenActions().length > 0}>
          <div class="pane-overflow-wrap">
            <button
              class="pane-btn pane-overflow-btn"
              title={t("pane.tooltip.more_actions")}
              aria-haspopup="menu"
              aria-expanded={showOverflow()}
              onClick={(e) => {
                e.stopPropagation();
                setShowOverflow(!showOverflow());
              }}
            >
              <IconChevronDown size={14} />
            </button>
            <Show when={showOverflow()}>
              <div
                class="pane-disc-menu pane-overflow-menu"
                role="menu"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowOverflow(false);
                }}
              >
                <For each={hiddenActions()}>
                  {(a) => (
                    <Show
                      when={!a.menuItems}
                      fallback={
                        <For each={a.menuItems!}>
                          {(mi) => (
                            <button class={mi.danger ? "danger" : ""} onClick={() => mi.run()}>
                              {a.icon()} {mi.label}
                            </button>
                          )}
                        </For>
                      }
                    >
                      <button class={a.active ? "active" : ""} onClick={() => a.run?.()}>
                        {a.icon()} {a.label}
                      </button>
                    </Show>
                  )}
                </For>
              </div>
            </Show>
          </div>
        </Show>
        <button class="pane-btn pane-close" title={t("pane.tooltip.close")} onClick={() => p.onClose(p.pane.pane_id)}><IconClose size={14} /></button>
      </div>
      <Show when={editingMeta()}>
        <div class="pane-meta-editor" onMouseDown={(e) => e.stopPropagation()}>
          <input
            class="pane-meta-title"
            placeholder="title (e.g. trying to find the X bug)"
            maxlength="200"
            value={titleDraft()}
            onInput={(e) => setTitleDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                saveMeta();
              } else if (e.key === "Escape") {
                setEditingMeta(false);
              }
            }}
          />
          <textarea
            class="pane-meta-annot"
            placeholder="annotation (longer free text — context, intent, links)"
            rows="3"
            value={annotDraft()}
            onInput={(e) => setAnnotDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                saveMeta();
              } else if (e.key === "Escape") {
                setEditingMeta(false);
              }
            }}
          />
          {/* Phase 31: identity picker. Same UX as the workspace
              picker (Phase 30), reusing the `ws-identity-*` CSS classes
              and i18n keys. Each click instant-saves via
              pane_set_identity, so the user can preview the border
              color change live behind the open dialog. Reset clears the
              pane's own values → falls back to workspace inheritance. */}
          <div class="ws-identity-block">
            <div class="ws-identity-label">{t("ws.identity.color")}</div>
            <div class="ws-identity-row">
              <For each={COLOR_PRESETS}>
                {(c) => (
                  <button
                    type="button"
                    class={`ws-identity-swatch ${paneColor() === c ? "selected" : ""}`}
                    style={{ background: c }}
                    title={c}
                    onClick={(e) => {
                      e.stopPropagation();
                      pickColor(c);
                    }}
                  />
                )}
              </For>
              <input
                type="text"
                class="ws-identity-hex"
                value={customHex()}
                placeholder={t("ws.identity.customColor")}
                spellcheck={false}
                onInput={(e) => setCustomHex(e.currentTarget.value)}
                onBlur={onCustomHexBlur}
              />
            </div>
            <div class="ws-identity-label" style="margin-top: 8px">{t("ws.identity.emoji")}</div>
            <div class="ws-identity-row">
              <For each={EMOJI_PRESETS}>
                {(g) => (
                  <button
                    type="button"
                    class={`ws-identity-emoji-btn ${paneEmoji() === g ? "selected" : ""}`}
                    title={g}
                    onClick={(e) => {
                      e.stopPropagation();
                      pickEmoji(g);
                    }}
                  >
                    {g}
                  </button>
                )}
              </For>
              <input
                type="text"
                class="ws-identity-emoji-custom"
                value={paneEmoji() ?? ""}
                placeholder={t("ws.identity.customEmoji")}
                maxlength={8}
                onInput={(e) => onCustomEmojiInput(e.currentTarget.value)}
                onBlur={onCustomEmojiBlur}
              />
              <button
                type="button"
                class="ws-identity-reset"
                onClick={resetIdentity}
              >
                {t("ws.identity.reset")}
              </button>
            </div>
          </div>
          <div class="pane-meta-actions">
            <button class="primary" onClick={saveMeta}>
              Save
            </button>
            <button onClick={() => setEditingMeta(false)}>Cancel</button>
            <span class="pane-meta-hint">
              Enter to save title; Ctrl+Enter to save from annotation; Esc to cancel
            </span>
          </div>
        </div>
      </Show>
      <Show when={showAnnot() && p.pane.annotation}>
        <div class="pane-annotation-bar">{p.pane.annotation}</div>
      </Show>
      <div class="pane-body">
        <Show when={!p.isConnected}>
          <div class="pane-connect">
            {/* Host-trust dialog (unknown host or mismatch) — highest priority */}
            <Show when={hostTrustHere()}>
              <div class={`host-trust ${hostTrustHere()!.mismatchOld ? "danger" : ""}`}>
                <Show
                  when={hostTrustHere()!.mismatchOld}
                  fallback={
                    <h3>First connect to {hostTrustHere()!.target}</h3>
                  }
                >
                  <h3><IconWarning size={14} /> HOST KEY CHANGED for {hostTrustHere()!.target}</h3>
                </Show>
                <Show when={hostTrustHere()!.mismatchOld}>
                  <p class="warn">
                    The server's host key is different from the one we trusted before.
                    This may indicate a man-in-the-middle attack — or the server was rekeyed.
                  </p>
                  <p>
                    <span class="label">Old fingerprint:</span>{" "}
                    <code>{hostTrustHere()!.mismatchOld}</code>
                  </p>
                </Show>
                <p>
                  <span class="label">{hostTrustHere()!.keyType} fingerprint:</span>{" "}
                  <code>{hostTrustHere()!.fingerprint}</code>
                </p>
                <div class="trust-buttons">
                  <button
                    class="primary"
                    onClick={() =>
                      p.onConnect(p.pane.pane_id, { acceptUnknownHost: true })
                    }
                  >
                    {hostTrustHere()!.mismatchOld ? "Replace and continue" : "Trust and continue"}
                  </button>
                  <button onClick={() => p.onConnect(p.pane.pane_id, {})}>Cancel</button>
                </div>
              </div>
            </Show>

            {/* Passphrase prompt for encrypted local key */}
            <Show when={!hostTrustHere() && passphraseHere()}>
              <div class="pw-row">
                <span class="pass-hint">
                  Passphrase for {passphraseHere()!.keyPath}
                  {passphraseHere()!.bad ? " (wrong, try again)" : ""}:
                </span>
                <input
                  type="password"
                  placeholder="key passphrase"
                  autofocus
                  value={passInput()}
                  onInput={(e) => setPassInput(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = passInput();
                      setPassInput("");
                      p.onConnect(p.pane.pane_id, { keyPassphrase: v });
                    }
                  }}
                />
                <button
                  class="primary"
                  onClick={() => {
                    const v = passInput();
                    setPassInput("");
                    p.onConnect(p.pane.pane_id, { keyPassphrase: v });
                  }}
                >
                  Connect
                </button>
              </div>
            </Show>

            {/* Password prompt (server auth) */}
            <Show when={!hostTrustHere() && !passphraseHere() && p.pendingPasswordFor === p.pane.pane_id}>
              <div class="pw-row">
                <input
                  type="password"
                  placeholder="password"
                  autofocus
                  value={pwInput()}
                  onInput={(e) => setPwInput(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = pwInput();
                      setPwInput("");
                      p.onConnect(p.pane.pane_id, { password: v });
                    }
                  }}
                />
                <button
                  class="primary"
                  onClick={() => {
                    const v = pwInput();
                    setPwInput("");
                    p.onConnect(p.pane.pane_id, { password: v });
                  }}
                >
                  Connect
                </button>
              </div>
            </Show>

            {/* Default Connect button when no special prompt */}
            <Show
              when={
                !hostTrustHere() &&
                !passphraseHere() &&
                p.pendingPasswordFor !== p.pane.pane_id
              }
            >
              {/* v0.4.4-beta.2: two buttons only — [Connect] probes for live
                  tmux sessions first (arms SSH headlessly, then lists): if any
                  exist it pops a picker to re-attach or open a plain shell;
                  otherwise it connects a regular shell straight away.
                  [Connection wizard] opens the unified wizard (type / directory
                  / command / resume list). */}
              <div class="connect-buttons">
                <button
                  class="primary big"
                  onClick={() => void smartConnect()}
                  disabled={connectProbing()}
                  title={p.boundSession ? t("connect.bound.tooltip", { name: p.boundSession.name }) : undefined}
                >
                  {connectProbing()
                    ? t("connect.probing")
                    : p.boundSession?.gone && p.boundSession.claudeSessionId
                      ? t("connect.resume")
                      : t("common.connect")}
                </button>
                <button class="big nc-wizard-btn" onClick={openNewConnModal} disabled={connectProbing()}>
                  {t("connect.openWizard")}
                </button>
              </div>
            </Show>

            <Show when={p.status}>
              <p class={p.status!.err ? "status-line err" : "status-line"}>
                {p.status!.msg}
              </p>
            </Show>
          </div>
        </Show>
        <div ref={slotRef!} class="pane-terminal-slot" />
      </div>

      {/* Phase 12.B: smart-connect prompt for cwd / cmd / claude args */}
      <Show when={smartModal()}>
        <div class="modal-backdrop" onClick={() => setSmartModal(null)}>
          <div class="modal smart-prompt" onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
            <h3>
              {smartModal() === "cwd" && t("connect.modal.openDir")}
              {smartModal() === "cmd" && t("connect.modal.runCmd")}
              {smartModal() === "claude_args" && t("connect.modal.claudeArgs")}
            </h3>
            <input
              class="pane-meta-title"
              autofocus
              placeholder={
                smartModal() === "cwd"
                  ? "/home/yossi/projects/foo"
                  : smartModal() === "cmd"
                    ? "npm run dev"
                    : "--resume"
              }
              value={smartInput()}
              onInput={(e) => setSmartInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submitSmartModal();
                else if (e.key === "Escape") setSmartModal(null);
              }}
            />
            <div class="modal-buttons">
              <button onClick={() => setSmartModal(null)}>{t("common.cancel")}</button>
              <button class="primary" onClick={submitSmartModal}>{t("common.connect")}</button>
            </div>
          </div>
        </div>
      </Show>

      {/* v0.4.4-beta.2: SMART [Connect] tmux picker — appears only when the
          headless probe found live tmux sessions. Re-attach to one, or open a
          plain regular shell. Portal so it stacks above panels/feed. */}
      <Show when={tmuxPick()}>
        <Portal>
          <div class="modal-backdrop nc-backdrop" onClick={() => setTmuxPick(null)}>
            <div class="nc-modal nc-modal-sm" role="dialog" aria-modal="true"
              onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
              <div class="nc-head">
                <h3>{t("connect.tmuxPick.title")}</h3>
                <button class="feed-x" title={t("common.close")} onClick={() => setTmuxPick(null)}><IconClose size={14} /></button>
              </div>
              <div class="nc-body">
                <p class="nc-hint">{t(isMacLocal() ? "connect.tmuxPick.hintLocal" : "connect.tmuxPick.hint")}</p>
                {/* 2026-08-23: workspace vs server scope. A folder-anchored
                    workspace opens on its own sessions; the count line below
                    keeps the hidden ones visible as a number, so a filtered
                    list can never be mistaken for an empty server. */}
                <div class="nc-segmented nc-filter" role="tablist">
                  <button
                    role="tab"
                    aria-selected={tmuxScope() === "workspace"}
                    class={`nc-seg ${tmuxScope() === "workspace" ? "active" : ""}`}
                    onClick={() => setTmuxScope("workspace")}
                  >
                    <IconFolder size={13} /> {t("connect.tmuxPick.scope.workspace")}
                  </button>
                  <button
                    role="tab"
                    aria-selected={tmuxScope() === "server"}
                    class={`nc-seg ${tmuxScope() === "server" ? "active" : ""}`}
                    onClick={() => setTmuxScope("server")}
                  >
                    <IconTerminal size={13} /> {t("connect.tmuxPick.scope.server")}
                  </button>
                </div>
                <p class="nc-muted">
                  {t("connect.tmuxPick.scope.count", {
                    shown: String(tmuxPickVisible().length),
                    total: String((tmuxPick() ?? []).length),
                  })}
                </p>
                <Show when={tmuxPickVisible().length === 0}>
                  <p class="nc-muted">{t("connect.tmuxPick.scope.empty")}</p>
                </Show>
                <div class="nc-resume-list">
                  <For each={tmuxPickVisible()}>
                    {(s) => {
                      // Phase 81: friendly name from the server-side
                      // session-meta map — manual label beats the stable
                      // auto-name ("<two words> · <date time>", derived
                      // from the session's first prompt) beats the Claude
                      // session title beats the raw tmux name. When a
                      // friendly name shows, the raw name drops to a
                      // muted secondary line so cross-machine sessions
                      // stay identifiable AND attachable.
                      const friendly = s.label ?? s.auto_name ?? s.claude_title;
                      // Claude's rolling read of the conversation stays
                      // reachable on hover once auto_name has the row.
                      const tip = s.claude_title && s.claude_title !== friendly
                        ? `${s.name} — ${s.claude_title}`
                        : s.name;
                      // 2026-08-24: the mess-guard. "Whole server" lists every
                      // session on the host, and nothing used to separate one
                      // that is free to take from one already mounted on
                      // another project. The verdict is the backend's
                      // (annotate_scope_with) and it is never set for a session
                      // in this workspace's scope, so no scope check here.
                      const foreign = s.foreign;
                      const foreignName = foreign?.label
                        ?? t("connect.tmuxPick.foreign.unknown");
                      const foreignTip = foreign
                        ? t(
                            foreign.kind === "workspace"
                              ? "connect.tmuxPick.foreign.tipWorkspace"
                              : "connect.tmuxPick.foreign.tipFolder",
                            { name: foreignName },
                          ) + (foreign.path ? `\n${foreign.path}` : "")
                        : "";
                      return (
                        <div class="nc-resume-row" onClick={() => pickTmuxSession(s.name)} title={tip}>
                          <div class="nc-resume-head">
                            <span class="nc-resume-proj"><IconTerminal size={14} /> {friendly ?? s.name}</span>
                            {/* Left of the transient state flags: this is a
                                property of the session, not of right now. */}
                            <Show when={foreign}>
                              <span class="nc-resume-badge nc-resume-foreign" title={foreignTip}>
                                <IconFolder size={11} /> {foreignName}
                              </span>
                            </Show>
                            <span class="nc-resume-badge">{s.windows}w</span>
                            <Show when={s.attached}>
                              <span class="nc-resume-badge">{t("connect.newConn.tmuxAttached")}</span>
                            </Show>
                            {/* 2026-08-19: zellij lists sessions whose shell
                                has exited — attaching rebuilds them, which is
                                the one thing tmux cannot do. Unlabelled, they
                                looked identical to live ones. */}
                            <Show when={s.exited}>
                              <span class="nc-resume-badge">{t("connect.newConn.sessionExited")}</span>
                              {/* 2026-08-20: the list was append-only. Closing
                                  a pane deliberately leaves its session
                                  running, and a reboot leaves everything
                                  EXITED, so corpses accumulated with no way to
                                  bury one — only to resurrect it.

                                  stopPropagation is MANDATORY: the row's own
                                  onClick attaches, so without it a delete
                                  would attach AND destroy. The confirm is the
                                  second guard. */}
                              <button
                                class="nc-resume-del"
                                title={t("connect.tmuxPick.delete")}
                                onClick={async (e) => {
                                  e.stopPropagation();
                                  if (!confirm(t("connect.tmuxPick.deleteConfirm", { name: s.name }))) return;
                                  try {
                                    await invoke<KillSessionOutcome>("zellij_delete_session", { name: s.name });
                                  } catch (err) {
                                    log.warn("zellij_delete_session failed", err);
                                  }
                                  // Re-ask the machine rather than splicing the
                                  // local array: the picker should show what is
                                  // actually there, including when the delete
                                  // did not take.
                                  try {
                                    setTmuxPick(await invoke<TmuxSessionInfo[]>(
                                      "pane_list_tmux_sessions",
                                      { workspaceId: p.workspaceId, projectPath: folderAnchor() },
                                    ));
                                  } catch { /* leave the list as it was */ }
                                }}
                              >
                                <IconTrash size={13} />
                              </button>
                            </Show>
                            <span class="nc-resume-age">{fmtSessionAge(s.last_attached || s.created)}</span>
                          </div>
                          <Show when={friendly}>
                            <div class="nc-resume-raw">{s.name}</div>
                          </Show>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </div>
              <div class="nc-footer">
                <button onClick={() => pickTmuxSession(null)}><IconTerminal size={14} /> {t("connect.tmuxPick.regular")}</button>
                <button onClick={() => setTmuxPick(null)}>{t("common.cancel")}</button>
              </div>
            </div>
          </div>
        </Portal>
      </Show>

      {/* v0.4.4-beta.2 (Task 2 polish): unified new-connection wizard —
          connection type + directory + command in one modal. Rendered through
          a Portal onto <body> so its own high z-index stacks above the
          sidebar / panels / feed regardless of the pane's local stacking. */}
      <Show when={newConnModal()}>
        <Portal>
          <div
            class="nc-backdrop"
            onClick={() => setNewConnModal(false)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setNewConnModal(false);
            }}
          >
            <div
              class="nc-modal"
              role="dialog"
              aria-modal="true"
              aria-label={t("connect.newConn.title")}
              onClick={(e) => e.stopPropagation()}
              onMouseDown={(e) => e.stopPropagation()}
              onKeyDown={(e) => {
                if (e.key === "Escape") { e.stopPropagation(); setNewConnModal(false); }
                // Enter submits, except while typing in the custom-command field.
                if (
                  e.key === "Enter" &&
                  (e.target as HTMLElement)?.tagName !== "SELECT"
                ) {
                  e.preventDefault();
                  submitNewConn();
                }
              }}
            >
              <div class="nc-head">
                <h3>{t("connect.newConn.title")}</h3>
                <button class="feed-x" title={t("common.close")} onClick={() => setNewConnModal(false)}><IconClose size={14} /></button>
              </div>

              <div class="nc-body">
                {/* ── FORM view ─────────────────────────────────────── */}
                <Show when={ncView() === "form"}>
                  {/* 1. Connection type (Regular | TMUX) */}
                  <div class="nc-section">
                    <label class="nc-label">{t("connect.newConn.type")}</label>
                    <div class="nc-segmented" role="tablist">
                      <button
                        role="tab"
                        aria-selected={ncType() === "regular"}
                        class={`nc-seg ${ncType() === "regular" ? "active" : ""}`}
                        onClick={() => setNcType("regular")}
                      >
                        <IconTerminal size={14} /> {t("connect.newConn.typeRegular")}
                      </button>
                      <button
                        role="tab"
                        aria-selected={ncType() === "tmux"}
                        class={`nc-seg ${ncType() === "tmux" ? "active" : ""}`}
                        onClick={() => setNcType("tmux")}
                      >
                        <IconTerminal size={14} /> {t("connect.newConn.typeTmux")}
                      </button>
                    </div>
                    <p class="nc-hint">
                      {ncType() === "tmux"
                        ? t(isMacLocal() ? "connect.newConn.typeTmux.hintLocal" : "connect.newConn.typeTmux.hint")
                        : t(isMacLocal() ? "connect.newConn.typeRegular.hintLocal" : "connect.newConn.typeRegular.hint")}
                    </p>
                    {/* 2026-08-23: the attach-only warning, shown BEFORE the
                        command list so the user reads it while choosing rather
                        than after their `--resume` was dropped. Its escape
                        hatch makes a NEW session instead of typing into the
                        live one. */}
                    <Show when={attachOnly()}>
                      <div class="nc-attach-only">
                        <p class="nc-muted">
                          <IconWarning size={13} />{" "}
                          {t("connect.newConn.attachOnly.banner", { name: targetState()!.name })}
                        </p>
                        <button class="nc-browse" onClick={() => void connectAsNewSession()}>
                          {t("connect.newConn.attachOnly.newSession")}
                        </button>
                      </div>
                    </Show>
                  </div>

                  {/* 2. Directory. Anchored workspaces show it read-only:
                         the folder IS the workspace, so there is nothing
                         to choose. Everyone else keeps the free field. */}
                  <Show
                    when={!folderAnchor()}
                    fallback={
                      <div class="nc-section">
                        <label class="nc-label">{t("connect.newConn.directory")}</label>
                        <div class="nc-locked-dir" title={folderAnchor()!}>
                          <IconFolder size={13} /> <span>{folderAnchor()}</span>
                        </div>
                      </div>
                    }
                  >
                    <div class="nc-section">
                      <label class="nc-label">
                        {t("connect.newConn.directory")}{" "}
                        <span class="nc-optional">{t("connect.newConn.dirDefault")}</span>
                      </label>
                      <div class="nc-dir-row">
                        <input
                          class="nc-input"
                          autofocus
                          placeholder="/home/user/project"
                          value={ncDir()}
                          onInput={(e) => setNcDir(e.currentTarget.value)}
                        />
                        <Show when={isSsh() || isLocalPane()}>
                          <button class="nc-browse" onClick={browseNewConnDir}>
                            {t("connect.newConn.browse")}
                          </button>
                        </Show>
                      </div>
                    </div>
                  </Show>

                  {/* 3. Command (dropdown; custom field only when chosen).
                         Disabled, not hidden, under attach-only: a control
                         that vanishes reads as a bug, one that is greyed out
                         next to the banner reads as a reason. */}
                  <div class="nc-section" classList={{ "nc-disabled": attachOnly() }}>
                    <label class="nc-label">{t("connect.newConn.command")}</label>
                    <select
                      class="nc-select"
                      disabled={attachOnly()}
                      value={ncCmd()}
                      onChange={(e) => { setNcCmd(e.currentTarget.value as NcCmd); setNcPickedSession(null); }}
                    >
                      <option value="plain"></option>
                      <option value="claude">claude</option>
                      <option value="claude-continue">claude --continue</option>
                      <option value="claude-resume">claude --resume</option>
                      <option value="claude-skip">claude --dangerously-skip-permissions</option>
                      <option value="from-list">{t("connect.newConn.fromList")}</option>
                      <option value="custom">{t("connect.newConn.custom")}</option>
                    </select>
                    <Show when={ncCmd() === "custom"}>
                      <input
                        class="nc-input nc-custom"
                        placeholder="npm run dev"
                        disabled={attachOnly()}
                        value={ncCustom()}
                        onInput={(e) => setNcCustom(e.currentTarget.value)}
                      />
                    </Show>
                    <Show when={attachOnly()}>
                      <p class="nc-hint">{t("connect.newConn.attachOnly.hint")}</p>
                    </Show>
                  </div>

                  {/* 4. Session list — only for the "choose from list" command */}
                  <Show when={ncShowsList() && !attachOnly()}>
                    <div class="nc-section">
                      <label class="nc-label">
                        {t("connect.newConn.resumeTitle")} <span class="nc-req">*</span>
                      </label>
                      <div class="nc-resume-tools">
                        <input
                          class="nc-input nc-search"
                          placeholder={t("connect.newConn.search")}
                          value={ncSearch()}
                          onInput={(e) => setNcSearch(e.currentTarget.value)}
                        />
                        <div class="nc-segmented nc-filter">
                          <For each={[
                            { v: "user", label: t("connect.newConn.filterUser") },
                            { v: "agent", label: t("connect.newConn.filterAgent") },
                            { v: "all", label: t("connect.newConn.filterAll") },
                          ] as { v: NcFilter; label: string }[]}>
                            {(f) => (
                              <button
                                class={`nc-seg ${ncFilter() === f.v ? "active" : ""}`}
                                onClick={() => setNcFilter(f.v)}
                              >
                                {f.label}
                              </button>
                            )}
                          </For>
                        </div>
                        <button class="nc-browse" title={t("connect.newConn.refresh")} onClick={() => void loadNcSessions()}><IconRefresh size={14} /></button>
                      </div>
                      <div class="nc-resume-list">
                        <Show when={ncSessionsLoading()}>
                          <p class="nc-muted">{t("claude_picker.loading")}</p>
                        </Show>
                        <Show when={ncSessionsErr()}>
                          <p class="nc-muted err"><IconWarning size={13} /> {ncSessionsErr()}</p>
                        </Show>
                        <Show when={!ncSessionsLoading() && !ncSessionsErr() && ncFilteredSessions().length === 0}>
                          <p class="nc-muted">{t("claude_picker.empty")}</p>
                        </Show>
                        <For each={ncFilteredSessions()}>
                          {(s) => (
                            <div
                              class={`nc-resume-row ${ncPickedSession()?.session_id === s.session_id ? "picked" : ""}`}
                              onClick={() => setNcPickedSession(s)}
                              title={s.jsonl_path}
                            >
                              <div class="nc-resume-head">
                                <code class="nc-resume-id">{s.session_id.slice(0, 8)}</code>
                                <Show when={s.is_subagent}>
                                  <span class="nc-resume-badge">{t("connect.newConn.filterAgent")}</span>
                                </Show>
                                <span class="nc-resume-proj">{s.project_path}</span>
                                <span class="nc-resume-age">{fmtSessionAge(s.mtime_unix)}</span>
                              </div>
                              <Show when={s.last_user}>
                                <div class="nc-resume-prev">{s.last_user}</div>
                              </Show>
                            </div>
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>
                </Show>

                {/* ── BROWSE view (inline folder tree) ──────────────── */}
                <Show when={ncView() === "browse" && dirPicker()}>
                  <div class="nc-section">
                    <div class="nc-browse-path" title={dirPicker()!.path}>{dirPicker()!.path}</div>
                    <Show when={recentDirs().length > 0}>
                      <div class="nc-recent">
                        <For each={recentDirs()}>
                          {(d) => (
                            <button class="nc-recent-row" title={d} onClick={() => chooseDir(d)}><IconClock size={14} /> {d}</button>
                          )}
                        </For>
                      </div>
                    </Show>
                    <Show when={dirPicker()!.error}>
                      <p class="nc-muted err"><IconWarning size={13} /> {dirPicker()!.error}</p>
                    </Show>
                    <ul class="nc-dir-list">
                      <Show when={dirPicker()!.path !== "/"}>
                        <li class="nc-dir-item up" onClick={() => void navigateDirPicker(dirPickerParent(dirPicker()!.path))}><IconFolder size={14} /> ..</li>
                      </Show>
                      <For each={dirPicker()!.dirs}>
                        {(name) => (
                          <li class="nc-dir-item" onClick={() => void navigateDirPicker(dirPickerJoin(dirPicker()!.path, name))}><IconFolder size={14} /> {name}</li>
                        )}
                      </For>
                      <Show when={!dirPicker()!.loading && dirPicker()!.dirs.length === 0 && !dirPicker()!.error}>
                        <li class="nc-dir-empty">{t("connect.dirPicker.empty")}</li>
                      </Show>
                    </ul>
                  </div>
                </Show>
              </div>

              <div class="nc-footer">
                <Show
                  when={ncView() === "browse"}
                  fallback={
                    <>
                      <button class="nc-btn" onClick={() => setNewConnModal(false)}>{t("common.cancel")}</button>
                      <button class="nc-btn primary" disabled={!newConnValid()} onClick={submitNewConn}>
                        {t("common.connect")}
                      </button>
                    </>
                  }
                >
                  <button class="nc-btn" onClick={cancelBrowse}>{t("connect.newConn.back")}</button>
                  <button class="nc-btn primary" disabled={!dirPicker()} onClick={() => dirPicker() && chooseDir(dirPicker()!.path)}>
                    {t("connect.dirPicker.useThis")}
                  </button>
                </Show>
              </div>
            </div>
          </div>
        </Portal>
      </Show>

      {/* Phase 65 (bug AA): remote folder picker for "Open in directory". */}
      {/* v0.4.4-beta.2: only the standalone "open dir" flow uses this popup;
          the new-connection wizard renders the tree inline (ncView="browse"). */}
      {/* The standalone directory picker that used to live here was
          UNREACHABLE — its guard was `dirPicker() && !dirPickForNewConn()`
          and the only caller of openDirPicker sets dirPickForNewConn
          first. It is now app/src/DirPicker.tsx, with a live caller: the
          workspace context menu's "pin project folder". The inline
          browse view above (ncView() === "browse") is a second copy of
          the same list with wizard chrome — see FOLLOWUPS. */}

      {/* v0.4.4-beta.2: standalone Claude session picker + tmux session picker
          removed — session resume lives in the wizard ("choose from list"),
          and connections go through the two-button Connect / Wizard flow. */}
    </div>
  );
}
