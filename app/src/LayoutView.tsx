import { Match, Show, Switch, onCleanup, onMount } from "solid-js";
import type { BoundSession, RtlProfileKind } from "./types";
import { Divider } from "./Divider";
import { paneDragStore, installPaneDragEscape } from "./paneDrag";
// Phase 53 (rebased): BrowserPane no longer imported — the Browser
// surface moved to the workspace-level floating BrowserWindow
// (sidebar 🌐). The file stays in the repo as historical reference
// in case any of its in-pane Webview wiring proves useful for a
// future iteration.
import { DiffPane } from "./DiffPane";
// Phase 53 (rebased): FileManagerPane no longer imported here — the
// Files surface moved to the workspace-level floating FileManagerWindow
// (sidebar 🗂). FileManagerPane itself stays in the repo; it's now
// consumed by FileManagerWindow.tsx, not by the pane layout.
import { HelpPane } from "./HelpPane";
import { t } from "./i18n";
// Phase 24.D: ClaudeChatPane (Phase 22) + ClaudeLogPane (Phase 24.B)
// removed. Files deleted, Match arms below stripped.
import {
  PaneView,
  type ConnectOpts,
  type HostTrustPending,
  type PassphrasePending,
} from "./PaneView";
import {
  paneKindOf,
  type Connection,
  type LayoutNode,
  type SplitDirection,
} from "./types";
import type { TerminalInstance } from "./terminalInstance";
import { trafficLight, type PaneAgentState } from "./paneAgentState";
import { IconGlobe, IconFolderOpen, IconClose } from "./icons";

interface Props {
  workspaceId: string;
  node: LayoutNode;
  activePaneId: string | null;
  connectedPaneIds: Set<string>;
  // Phase 26: pane_ids with a pending blocking feed item — these
  // panes get the cmux-style notification ring.
  waitingPaneIds: Set<string>;
  // cmux-A A1: pane_ids that received an OSC 9/99/777 terminal
  // notification and haven't been focused since. Drives the amber
  // "activity" pulse (distinct from waiting/blocking pulse).
  notifiedPaneIds: Set<string>;
  // cmux-A A1: master switch for the pulse — mirrors the
  // notifications.pane_pulse_on_activity setting. Passed as a plain
  // boolean so PaneView doesn't need to import settings shape.
  panePulseEnabled: boolean;
  pendingPasswordFor: string | null;
  pendingPassphrase: PassphrasePending | null;
  pendingHostTrust: HostTrustPending | null;
  paneStatus: Record<string, { msg: string; err: boolean }>;
  paneStatusText: Record<string, string>;
  // issue #4: per-pane agent turn timing + a reactive clock for the Ticker.
  // Phase 84.B widened it with the effective agent state behind the
  // traffic light.
  agentRuns: Record<
    string,
    {
      startedAt: number | null;
      avgMs: number | null;
      state: PaneAgentState;
      stateSince: number | null;
      seq: number;
    }
  >;
  agentClockMs: () => number;
  ensureTerm: (paneId: string, profile: RtlProfileKind) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: SplitDirection) => void;
  onClose: (paneId: string) => void;
  // Unshipped-fivefer (#4): pop this pane's terminal into its own window.
  onPopOut: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
  // Phase 11.A: tmux session map keyed by pane_id; presence = persistent.
  panePersistence: Record<string, string>;
  // Phase 91: pane_id → the session its [Connect] must attach to (the
  // workspace's first pane bound to `tmux_session`). Only panes that are
  // NOT live carry one; `gone` + a Claude id make it a Resume.
  boundSessions: Record<string, BoundSession>;
  onKillSession: (paneId: string) => void;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  onRatioDrag: (splitId: string, ratio: number) => void;
  onRatioCommit: (splitId: string, ratio: number) => void;
  // Phase 8.A: browser-pane callbacks.
  onBrowserNavigate: (paneId: string, url: string) => void;
  onBrowserGoBack: (paneId: string) => void;
  onBrowserGoHome: (paneId: string) => void;
  // Phase 8.B: per-pane forward toggle.
  onBrowserSetForward: (paneId: string, forward: boolean) => void;
  // Phase 16: workspace-level SSH detection. The file manager pane
  // can't tell from its own LayoutNode whether the workspace is SSH
  // (file-manager panes carry no connection), so the parent (App)
  // tells it explicitly. True iff the workspace has at least one
  // pane with an SSH connection.
  workspaceIsSsh: boolean;
  // Phase 23.D: the workspace's canonical connection, threaded
  // through to PaneView so isSsh() can fall back to it.
  workspaceConnection?: Connection;
  workspaceCwd?: string;
  // Phase 23.I: workspace name. PaneView's header falls back to it
  // when the pane has no user-set title.
  workspaceName?: string;
  // Phase 31: workspace identity. Threaded through to PaneView so each
  // pane can compute its effective identity (own override falls back
  // to the workspace's value).
  workspaceColor?: string;
  workspaceEmoji?: string;
  // Phase 65.T: focus/zoom mode. `maximizedPaneId` = the pane currently
  // filling the workspace area (null = normal split layout);
  // `workspacePaneCount` = total panes in the workspace, so the
  // maximized pane's header can show how many run in the background.
  maximizedPaneId: string | null;
  workspacePaneCount: number;
  // Phase 84.A: this workspace renders its panes as a tab strip. Only
  // forwarded to PaneView, which uses it to drop the (now meaningless)
  // maximize button. SplitView spreads `{...s.all}` so it propagates
  // through nested splits for free.
  tabsMode: boolean;
  // Phase 24.D: onWorkspacesFileUpdate removed — its only consumers
  // were the (now-gone) ChatPane / ClaudeLogPane Match arms.
}

export function LayoutView(p: Props) {
  // beta.3 (pane-dragdrop): install the global Escape handler for the
  // pane drag store once per workspace mount, and render the floating
  // ghost. LayoutView is the outermost workspace-scope component so
  // its lifetime matches the drag lifecycle we care about (a
  // workspace switch unmounts LayoutView → drag is cancelled).
  onMount(() => {
    const off = installPaneDragEscape();
    onCleanup(off);
  });
  return (
    <>
      <Show
        when={p.node.kind === "split"}
        fallback={<LeafPane all={p} pane={p.node as Extract<LayoutNode, { kind: "pane" }>} />}
      >
        <SplitView
          {...(p.node as Extract<LayoutNode, { kind: "split" }>)}
          all={p}
        />
      </Show>
      <Show when={paneDragStore.dragPaneId() !== null && paneDragStore.ghostPos() !== null}>
        <div
          class="pane-ghost"
          style={{
            left: `${paneDragStore.ghostPos()!.x + 12}px`,
            top: `${paneDragStore.ghostPos()!.y + 10}px`,
          }}
        >
          {paneDragStore.dragLabel()}
        </div>
      </Show>
    </>
  );
}

// Phase 8.A/regression-fix: render a leaf pane. Extracted from the previous
// inline IIFE (`fallback={(() => { ... })()}`) — the IIFE was re-evaluated on
// every parent render, which under some conditions caused the leaf component
// to thrash mount/unmount and lose click events on Connect / sidebar items.
// As a stable component, Solid reuses the same instance across re-renders.
function LeafPane(props: { all: Props; pane: Extract<LayoutNode, { kind: "pane" }> }) {
  const isActive = () => props.all.activePaneId === props.pane.pane_id;
  const kind = () => paneKindOf(props.pane);
  // Phase 84.B: every input to the traffic light is already in scope
  // here, so the verdict is computed once and passed down rather than
  // re-derived per consumer. Accessors, not a spread of a plain object —
  // Solid compiles `prop={expr}` into a getter, but a spread of an
  // eagerly-evaluated object freezes at first render.
  //
  // Reading agentClockMs() ties this to the existing 250ms pulseTick, so
  // the staleness cutoff re-evaluates for free — no second timer.
  const waitingOnPermission = () =>
    props.all.waitingPaneIds.has(props.pane.pane_id);
  const agentLight = () => {
    const run = props.all.agentRuns[props.pane.pane_id];
    return trafficLight({
      state: run?.state ?? "unknown",
      stateSince: run?.stateSince ?? null,
      waitingOnPermission: waitingOnPermission(),
      connected: props.all.connectedPaneIds.has(props.pane.pane_id),
      nowMs: props.all.agentClockMs(),
    });
  };
  // Phase 53 (rebased): the workspaceIsSsh local that fed the
  // FileManager Match arm is gone alongside the arm itself. The
  // prop stays on the Props interface so existing call sites in
  // App.tsx keep compiling; FileManagerWindow derives its own
  // SSH-ness from the workspace.connection field.
  return (
    <Switch
      fallback={
        <PaneView
          workspaceId={props.all.workspaceId}
          pane={props.pane}
          workspaceConnection={props.all.workspaceConnection}
          workspaceCwd={props.all.workspaceCwd}
          workspaceName={props.all.workspaceName}
          workspaceColor={props.all.workspaceColor}
          workspaceEmoji={props.all.workspaceEmoji}
          isActive={isActive()}
          isMaximized={props.all.maximizedPaneId === props.pane.pane_id}
          backgroundPaneCount={Math.max(0, props.all.workspacePaneCount - 1)}
          tabsMode={props.all.tabsMode}
          isWaiting={props.all.waitingPaneIds.has(props.pane.pane_id)}
          isNotified={
            props.all.panePulseEnabled &&
            props.all.notifiedPaneIds.has(props.pane.pane_id)
          }
          isConnected={props.all.connectedPaneIds.has(props.pane.pane_id)}
          pendingPasswordFor={props.all.pendingPasswordFor}
          pendingPassphrase={props.all.pendingPassphrase}
          pendingHostTrust={props.all.pendingHostTrust}
          status={props.all.paneStatus[props.pane.pane_id]}
          statusText={props.all.paneStatusText[props.pane.pane_id]}
          agentRun={props.all.agentRuns[props.pane.pane_id]}
          agentClockMs={props.all.agentClockMs}
          agentLight={agentLight()}
          agentWaitingOnPermission={waitingOnPermission()}
          agentStateSince={props.all.agentRuns[props.pane.pane_id]?.stateSince ?? null}
          agentNowMs={props.all.agentClockMs()}
          ensureTerm={props.all.ensureTerm}
          onFocus={props.all.onFocus}
          onConnect={props.all.onConnect}
          onSplit={props.all.onSplit}
          onClose={props.all.onClose}
          onPopOut={props.all.onPopOut}
          onDisconnect={props.all.onDisconnect}
          tmuxSession={props.all.panePersistence[props.pane.pane_id] ?? null}
          boundSession={props.all.boundSessions[props.pane.pane_id] ?? null}
          onKillSession={props.all.onKillSession}
          onSetTitle={props.all.onSetTitle}
          onSetAnnotation={props.all.onSetAnnotation}
        />
      }
    >
      <Match when={kind() === "browser"}>
        {/* Phase 53 (rebased): Browser is no longer a pane kind — it's
            a workspace-level floating window opened from the sidebar's
            Browser button. The 53.C load-time migration rewrites any
            existing Browser pane to Terminal on first boot, so this
            arm only ever renders if a user hand-edits workspaces.json
            after migration. Defensive placeholder + escape hatch. */}
        <div
          class={`pane ${isActive() ? "active" : ""}`}
          onClick={() => props.all.onFocus(props.pane.pane_id)}
        >
          <div class="pane-header">
            <span class="pane-conn"><IconGlobe size={14} /> {t("browser.legacyPane.title")}</span>
            <button
              class="pane-btn pane-close"
              title={t("common.close")}
              onClick={(e) => {
                e.stopPropagation();
                props.all.onClose(props.pane.pane_id);
              }}
            >
              <IconClose size={14} />
            </button>
          </div>
          <div class="pane-body legacy-pane-placeholder">
            <p>{t("browser.legacyPane.body")}</p>
          </div>
        </div>
      </Match>
      <Match when={kind() === "filemanager"}>
        {/* Phase 53 (rebased): File Manager moved out of pane layout
            into a workspace-level floating window (sidebar 🗂 Files).
            53.C migration rewrites legacy FileManager panes to
            Terminal on first boot; this arm only fires if a user
            hand-edits workspaces.json after migration. Defensive
            placeholder + escape hatch. */}
        <div
          class={`pane ${isActive() ? "active" : ""}`}
          onClick={() => props.all.onFocus(props.pane.pane_id)}
        >
          <div class="pane-header">
            <span class="pane-conn"><IconFolderOpen size={14} /> {t("files.legacyPane.title")}</span>
            <button
              class="pane-btn pane-close"
              title={t("common.close")}
              onClick={(e) => {
                e.stopPropagation();
                props.all.onClose(props.pane.pane_id);
              }}
            >
              <IconClose size={14} />
            </button>
          </div>
          <div class="pane-body legacy-pane-placeholder">
            <p>{t("files.legacyPane.body")}</p>
          </div>
        </div>
      </Match>
      <Match when={kind() === "diff"}>
        {/* Phase 50 (#2.4): live git diff pane. Self-contained — owns
            its own header (source dropdown + Refresh) and body. */}
        <DiffPane
          workspaceId={props.all.workspaceId}
          pane={props.pane}
          isActive={isActive()}
          onFocus={props.all.onFocus}
          onClose={props.all.onClose}
        />
      </Match>
      <Match when={kind() === "help"}>
        {/* Phase 33: in-app help pane. Self-contained — no SSH/PTY,
            no remote state. The header title comes from i18n keyed by
            the topic (e.g. "help.title.sshKeySetup"). */}
        <div
          class={`pane ${isActive() ? "active" : ""}`}
          onClick={() => props.all.onFocus(props.pane.pane_id)}
        >
          <div class="pane-header">
            <span class="pane-conn">
              {t(`help.title.${
                (props.pane.help_topic ?? "ssh-key-setup")
                  .split("-")
                  .map((s, i) => (i === 0 ? s : s.charAt(0).toUpperCase() + s.slice(1)))
                  .join("")
              }`)}
            </span>
            <button
              class="pane-btn pane-close"
              title={t("common.close")}
              onClick={(e) => {
                e.stopPropagation();
                props.all.onClose(props.pane.pane_id);
              }}
            >
              <IconClose size={14} />
            </button>
          </div>
          <div class="pane-body">
            <HelpPane topic={props.pane.help_topic ?? "ssh-key-setup"} />
          </div>
        </div>
      </Match>
      {/* Phase 24.D: ClaudeChat + ClaudeLog Match arms removed
          with their pane kinds. Legacy panes with those pane_kind
          values are aliased to Terminal at deserialize time, so
          the Switch fallback (PaneView) handles them. */}
    </Switch>
  );
}

// Keep Show imported so older usages elsewhere keep working.
void Show;

function SplitView(
  s: Extract<LayoutNode, { kind: "split" }> & { all: Props }
) {
  let containerRef!: HTMLDivElement;
  return (
    <div ref={containerRef!} class={`split split-${s.direction}`}>
      <div class="split-side" style={{ flex: `${s.ratio}` }}>
        <LayoutView {...s.all} node={s.first} />
      </div>
      <Divider
        direction={s.direction}
        parentEl={() => containerRef}
        onDrag={(r) => s.all.onRatioDrag(s.split_id, r)}
        onCommit={(r) => s.all.onRatioCommit(s.split_id, r)}
      />
      <div class="split-side" style={{ flex: `${1 - s.ratio}` }}>
        <LayoutView {...s.all} node={s.second} />
      </div>
    </div>
  );
}
