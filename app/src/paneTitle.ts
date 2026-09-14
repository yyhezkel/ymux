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
