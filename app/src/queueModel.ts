// BRIEF: the pure model behind the Queue panel — bucketing, grouping and
// the "what's happening" synthesis. Solid-free on purpose, same reasoning
// as paneAgentState.ts: one verdict function shared by every renderer, and
// unit-testable under node (queueModel.test.ts).
//
// The live hook state ALWAYS outranks a brief for placement (hooks say
// *now*, a brief says *what the last turn concluded*): a pane that is
// running sorts as running no matter what its previous brief claimed, and
// a pane blocked on a permission card is "needs you" no matter how calm
// its brief sounded.
import type { PaneAgentState, TrafficLight } from "./paneAgentState";
import type { PaneBriefEntry } from "./bindings/PaneBriefEntry";

/** One agent pane, anywhere in the app — built by App's allPaneAgentRows(). */
export interface QueueRow {
  wsId: string;
  wsName: string;
  paneId: string;
  title: string;
  state: PaneAgentState;
  stateSince: number | null;
  startedAt: number | null;
  waitingOnPermission: boolean;
  connected: boolean;
  brief: PaneBriefEntry | null;
  light: TrafficLight | null;
}

/** The queue's status vocabulary — Yossi's reference table's language.
 *  needs-input/stuck/waiting are the "needs you" family; working is live;
 *  done finished its chunk; ended = the session itself closed (✅). */
export type QueueStatus =
  | "needs-input"
  | "stuck"
  | "waiting"
  | "working"
  | "done"
  | "ended";

export function queueStatus(r: QueueRow): QueueStatus {
  // Live signals first — a permission card or a needs-input notification
  // is "blocked on you right now" regardless of any brief.
  if (r.waitingOnPermission || r.state === "needs-input") return "needs-input";
  // StopFailure: the turn died on an API error; it needs a look, not an answer.
  if (r.state === "failed") return "stuck";
  if (r.brief?.session_ended) return "ended";
  if (r.state === "running") return "working";
  const bs = r.brief?.brief?.status;
  if (bs === "stuck") return "stuck";
  if (bs === "waiting-for-you") return "waiting";
  if (bs === "working") return "working";
  return "done";
}

/** Sort buckets: who-needs-you first. 0 = blocked/stuck, 1 = waiting for a
 *  decision, 2 = done/closed (review material), 3 = running (pulls no
 *  attention — that's the point). */
export const QUEUE_BUCKET: Record<QueueStatus, number> = {
  "needs-input": 0,
  stuck: 0,
  waiting: 1,
  done: 2,
  ended: 2,
  working: 3,
};

/** The "what's happening" cell, as data — the component adds i18n phrasing.
 *  `dim` marks content carried over from a previous turn. */
export interface Happening {
  kind: "prompt" | "ask" | "delta" | "next";
  text: string;
  dim: boolean;
}

export function whatsHappening(r: QueueRow): Happening | null {
  const b = r.brief?.brief ?? null;
  if (queueStatus(r) === "working") {
    const p = r.brief?.last_prompt;
    if (p) return { kind: "prompt", text: p, dim: false };
    // No prompt captured — fall through to the previous brief, dimmed.
    if (b) {
      const h = fromBrief(b);
      if (h) return { ...h, dim: true };
    }
    return null;
  }
  return b ? fromBrief(b) : null;
}

function fromBrief(b: NonNullable<NonNullable<QueueRow["brief"]>["brief"]>): Happening | null {
  if (b.ask) return { kind: "ask", text: b.rec ? `${b.ask} · ${b.rec}` : b.ask, dim: false };
  if (b.delta) return { kind: "delta", text: b.delta, dim: false };
  if (b.next) return { kind: "next", text: b.next, dim: false };
  return null;
}

/** When this row's current situation began — for the age column and the
 *  oldest-first tie-break inside a bucket. */
export function rowSinceMs(r: QueueRow): number | null {
  if (queueStatus(r) === "working" && r.startedAt != null) return r.startedAt;
  return r.stateSince ?? r.brief?.brief?.updated_ms ?? r.brief?.prompt_ms ?? null;
}

/** A row earns a queue slot when there is any agent signal for it: a live
 *  traffic light, or a brief/prompt from a session (even an ended one). */
export function inQueue(r: QueueRow): boolean {
  return r.light !== null || r.brief !== null;
}

/** One active-sessions row's verdict, and only ever one. `queue` = a pane on
 *  this session has a current Queue state, shown for free. `screen` = the row
 *  needs the on-demand LLM read of the screen, which stays local to the
 *  overview and never feeds the Queue: an unexpiring guess must not light the
 *  sidebar or the badge. `summaryTickable`: the status is free, but no real
 *  summary text exists (a degraded brief, or nothing to show). */
export type OverviewVerdict =
  | { source: "queue"; status: QueueStatus; happening: Happening | null; summaryTickable: boolean }
  | { source: "screen" };

/** Event-based freshness, no age cap: a brief is current until a newer prompt
 *  starts a turn. During a live turn the hook state is the verdict, so the
 *  brief's age is irrelevant. */
function countsInOverview(r: QueueRow): boolean {
  if (!r.connected || !inQueue(r)) return false;
  if (r.state === "running" || r.state === "needs-input" || r.state === "failed" || r.waitingOnPermission) {
    return true;
  }
  const prompt = r.brief?.prompt_ms ?? null;
  if (prompt === null) return true;
  const updated = r.brief?.brief?.updated_ms;
  return updated !== undefined && prompt <= updated;
}

/** `paneIds` is the session's stamped panes; absent on the web backend, which
 *  sends the row to the LLM. Several panes can mount one session: the most
 *  urgent wins, then the most recent situation. */
export function overviewVerdict(
  paneIds: readonly string[] | undefined,
  rows: readonly QueueRow[],
): OverviewVerdict {
  if (!paneIds || paneIds.length === 0) return { source: "screen" };
  const ids = new Set(paneIds);
  let top: QueueRow | null = null;
  for (const r of rows) {
    if (!ids.has(r.paneId) || !countsInOverview(r)) continue;
    if (top === null || moreUrgent(r, top)) top = r;
  }
  if (top === null) return { source: "screen" };
  const happening = whatsHappening(top);
  return {
    source: "queue",
    status: queueStatus(top),
    happening,
    summaryTickable: happening === null || top.brief?.brief?.degraded === true,
  };
}

function moreUrgent(a: QueueRow, b: QueueRow): boolean {
  const ba = QUEUE_BUCKET[queueStatus(a)];
  const bb = QUEUE_BUCKET[queueStatus(b)];
  return ba !== bb ? ba < bb : (rowSinceMs(a) ?? 0) > (rowSinceMs(b) ?? 0);
}

export interface QueueGroup {
  wsId: string;
  wsName: string;
  rows: QueueRow[];
  /** rows in buckets 0/1 — the group's "needs you" count. */
  attention: number;
}

/** The Queue's one ordering rule: bucket first (who needs you), then oldest.
 *  A row with no timestamp sorts last in its bucket. */
export function compareQueueRows(a: QueueRow, b: QueueRow): number {
  const bucket = QUEUE_BUCKET[queueStatus(a)] - QUEUE_BUCKET[queueStatus(b)];
  if (bucket !== 0) return bucket;
  return (rowSinceMs(a) ?? Number.MAX_SAFE_INTEGER) - (rowSinceMs(b) ?? Number.MAX_SAFE_INTEGER);
}

/** One row of the jump palette. `status` is null for a plain shell: it has
 *  no agent signal, so any status word would be invented. */
export interface JumpRow {
  wsId: string;
  wsName: string;
  paneId: string;
  title: string;
  cwd: string | null;
  status: QueueStatus | null;
}

/** The jump palette's rows: agent panes in Queue order, across all
 *  workspaces (no grouping, so a blocked pane always outranks a working
 *  pane of another workspace), then connected plain shells in layout order.
 *  A pane with neither signal is not a session and is left out. Pure. */
export function buildJumpRows(
  rows: readonly QueueRow[],
  cwdOf: (wsId: string) => string | null,
): JumpRow[] {
  const agents = rows.filter(inQueue).sort(compareQueueRows);
  const shells = rows.filter((r) => !inQueue(r) && r.connected);
  return [...agents, ...shells].map((r) => ({
    wsId: r.wsId,
    wsName: r.wsName,
    paneId: r.paneId,
    title: r.title,
    cwd: cwdOf(r.wsId),
    status: inQueue(r) ? queueStatus(r) : null,
  }));
}

/** Group by workspace (the reference table's shape: "CRM — 5"), rows
 *  sorted bucket-then-oldest inside each group, groups sorted by their
 *  most urgent row then by that row's age. Pure. */
export function groupQueueRows(rows: QueueRow[]): QueueGroup[] {
  const byWs = new Map<string, QueueGroup>();
  for (const r of rows) {
    if (!inQueue(r)) continue;
    let g = byWs.get(r.wsId);
    if (!g) {
      g = { wsId: r.wsId, wsName: r.wsName, rows: [], attention: 0 };
      byWs.set(r.wsId, g);
    }
    g.rows.push(r);
    if (QUEUE_BUCKET[queueStatus(r)] <= 1) g.attention += 1;
  }
  const groups = [...byWs.values()];
  for (const g of groups) g.rows.sort(compareQueueRows);
  groups.sort((a, b) => compareQueueRows(a.rows[0], b.rows[0]));
  return groups;
}
