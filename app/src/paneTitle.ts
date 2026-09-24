// BRIEF (commit 2): the pane display-label precedence, lifted out of
// PaneTabs so the tab strip, the Queue panel and the Briefing card cannot
// drift apart on what a pane is called. Pure.
import { describeConnection, type Connection, type LayoutNode } from "./types";

export type PaneNode = Extract<LayoutNode, { kind: "pane" }>;

export function paneLabel(
  pane: PaneNode,
  ctx: { workspaceName?: string; workspaceConnection?: Connection | null },
): string {
  return (
    pane.title
    ?? pane.auto_title
    ?? ctx.workspaceName
    ?? (pane.connection
      ? describeConnection(pane.connection)
      : ctx.workspaceConnection
        ? describeConnection(ctx.workspaceConnection)
        : "—")
  );
}

// Phase 91: the display-name precedence for a multiplexer session, shared by
// the sessions strip, the active-sessions overview and "Open". `label` is
// the user's own name, `auto_name` the stable identity derived from the
// first prompt, `claude_title` the drifting conversation title.
export function sessionDisplay(s: {
  name: string;
  label?: string;
  auto_name?: string;
  claude_title?: string;
}): string {
  return s.label ?? s.auto_name ?? s.claude_title ?? s.name;
}
