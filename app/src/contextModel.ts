// Phase 105: the pure model behind the Context Rail. Solid-free and
// i18n-free on purpose (same reasoning as queueModel.ts) so
// contextModel.test.ts runs under plain `node --test`.
//
// The card is modeled on tzafrir/human-in-the-loop's task card: short
// fields under fixed labels (🎯 goal / Done when / Now / Next / Waiting on
// you), then the last few ✔ deltas. Everything here turns a
// SessionContext into those one-liners.
//
// The wire types mirror `context_store.rs` (SessionContext / LogEntry) by
// hand — change both together.
import type { BriefStatus } from "./bindings/BriefStatus";

export type LogKind = "turn" | "closed";

export interface LogEntry {
  ts_ms: number;
  kind: LogKind;
  status: BriefStatus;
  task: string | null;
  delta: string | null;
  next: string | null;
  ask: string | null;
  rec: string | null;
  degraded: boolean;
}

export interface SessionContext {
  schema: number;
  session_id: string;
  ws_id: string | null;
  pane_id: string | null;
  cwd: string | null;
  first_prompt: string | null;
  first_prompt_ms: number | null;
  /** Sticky: last non-empty `goal:` from a brief. */
  goal: string | null;
  /** Sticky: last non-empty `done:` from a brief. */
  done_when: string | null;
  log: LogEntry[];
  version: number;
}

/** ✔ lines shown before "N more". */
export const LOG_PREVIEW = 3;
/** The goal line, when it falls back to the first prompt. */
export const GOAL_MAX_CHARS = 80;
/** Every other one-liner on the card. */
export const LINE_MAX_CHARS = 70;

export const RAIL_MIN_W = 260;
export const RAIL_MAX_W = 640;
export const RAIL_DEFAULT_W = 340;
export const RAIL_COLLAPSED_W = 36;

export function clampRailWidth(w: number): number {
  if (!Number.isFinite(w)) return RAIL_DEFAULT_W;
  return Math.min(RAIL_MAX_W, Math.max(RAIL_MIN_W, Math.round(w)));
}

/** A one-liner: newlines/tabs flattened, runs of spaces collapsed, clipped
 *  to `max` characters (code points, so Hebrew is never cut mid-letter)
 *  with an ellipsis. `full` is the flattened, unclipped text — the UI puts
 *  it in the tooltip. */
export function oneLine(s: string, max = LINE_MAX_CHARS): { text: string; full: string; clipped: boolean } {
  const full = s.replace(/\s+/g, " ").trim();
  const chars = [...full];
  if (chars.length <= max) return { text: full, full, clipped: false };
  return { text: `${chars.slice(0, max).join("")}…`, full, clipped: true };
}

/** 🎯 line: the brief's sticky goal, else the first non-empty line of the
 *  session's first prompt clipped to 80. null = nothing to show. */
export function cardGoal(s: SessionContext): { text: string; full: string; fromPrompt: boolean } | null {
  const goal = s.goal?.trim();
  if (goal) {
    const g = oneLine(goal, GOAL_MAX_CHARS);
    return { text: g.text, full: g.full, fromPrompt: false };
  }
  const first = s.first_prompt?.split("\n").map((l) => l.trim()).find((l) => l !== "");
  if (!first) return null;
  const g = oneLine(first, GOAL_MAX_CHARS);
  return { text: g.text, full: g.full, fromPrompt: true };
}

/** The latest Turn entry — the source of Now / Next / Waiting on you. */
export function lastTurn(s: SessionContext): LogEntry | null {
  for (let i = s.log.length - 1; i >= 0; i--) {
    if (s.log[i].kind === "turn") return s.log[i];
  }
  return null;
}

/** "Waiting on you: ask · rec" — only while the latest turn asked and the
 *  session has not closed since. */
export function waitingText(s: SessionContext): string | null {
  if (isClosed(s)) return null;
  const t = lastTurn(s);
  if (!t?.ask) return null;
  return t.rec ? `${t.ask} · ${t.rec}` : t.ask;
}

/** The ✔ lines, newest first: every turn that reported a delta (degraded
 *  ones included — the UI dims them) and every closed line. `limit` null =
 *  all. Never mutates. */
export function doneEntries(s: SessionContext, limit: number | null): LogEntry[] {
  const out: LogEntry[] = [];
  for (let i = s.log.length - 1; i >= 0; i--) {
    const e = s.log[i];
    if (e.kind === "closed" || (e.delta && e.delta.trim() !== "")) {
      out.push(e);
      if (limit != null && out.length >= limit) break;
    }
  }
  return out;
}

/** How many ✔ lines exist in total (for "▸ N more"). */
export function doneCount(s: SessionContext): number {
  return doneEntries(s, null).length;
}

export const LOG_STATUS_ICON: Record<BriefStatus, string> = {
  working: "🔄",
  "waiting-for-you": "⏸️",
  stuck: "⚠️",
  done: "💤",
};

export function logIcon(e: LogEntry): string {
  return e.kind === "closed" ? "✅" : LOG_STATUS_ICON[e.status];
}

export function isClosed(s: SessionContext): boolean {
  const last = s.log[s.log.length - 1];
  return last?.kind === "closed";
}

export function lastActivityMs(s: SessionContext): number {
  const last = s.log[s.log.length - 1]?.ts_ms ?? 0;
  return Math.max(last, s.first_prompt_ms ?? 0);
}

/** The sessions that ran in one pane, newest activity first: index 0 is
 *  the pane's current session (the one the rail shows), the rest are its
 *  earlier sessions (a restarted `claude`, a `/clear`). Never mutates. */
export function sessionsForPane(sessions: SessionContext[], paneId: string | null): SessionContext[] {
  if (!paneId) return [];
  return sessions
    .filter((s) => s.pane_id === paneId)
    .sort((a, b) => lastActivityMs(b) - lastActivityMs(a));
}
