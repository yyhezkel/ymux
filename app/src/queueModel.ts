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

export interface QueueGroup {
  wsId: string;
  wsName: string;
  rows: QueueRow[];
  /** rows in buckets 0/1 — the group's "needs you" count. */
  attention: number;
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
  const rowKey = (r: QueueRow): [number, number] => [
    QUEUE_BUCKET[queueStatus(r)],
    rowSinceMs(r) ?? Number.MAX_SAFE_INTEGER,
  ];
  const groups = [...byWs.values()];
  for (const g of groups) {
    g.rows.sort((a, b) => {
      const [ba, sa] = rowKey(a);
      const [bb, sb] = rowKey(b);
      return ba !== bb ? ba - bb : sa - sb;
    });
  }
  groups.sort((a, b) => {
    const [ba, sa] = rowKey(a.rows[0]);
    const [bb, sb] = rowKey(b.rows[0]);
    return ba !== bb ? ba - bb : sa - sb;
  });
  return groups;
}
