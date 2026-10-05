// Phase 103: the pure model behind the Context Rail. Solid-free and
// i18n-free on purpose (same reasoning as queueModel.ts) so
// contextModel.test.ts runs under plain `node --test`.
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
  log: LogEntry[];
  version: number;
}

/** Log lines shown before "show all". */
export const LOG_PREVIEW = 5;
/** First-prompt characters shown before expanding. */
export const PROMPT_PREVIEW_CHARS = 180;

export const RAIL_MIN_W = 260;
export const RAIL_MAX_W = 640;
export const RAIL_DEFAULT_W = 340;
export const RAIL_COLLAPSED_W = 36;

export function clampRailWidth(w: number): number {
  if (!Number.isFinite(w)) return RAIL_DEFAULT_W;
  return Math.min(RAIL_MAX_W, Math.max(RAIL_MIN_W, Math.round(w)));
}

/** Newest first; `limit` null = all. Never mutates the input. */
export function logNewestFirst(log: LogEntry[], limit: number | null): LogEntry[] {
  const rev = [...log].reverse();
  return limit == null ? rev : rev.slice(0, limit);
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

/** The text of one log line: `ask · rec` when the agent asked something,
 *  else `delta`, with `→ next` appended when present. Empty string when
 *  the entry carries nothing (a closed line with no reason). */
export function logLineText(e: LogEntry): string {
  const parts: string[] = [];
  if (e.ask) parts.push(e.rec ? `${e.ask} · ${e.rec}` : e.ask);
  else if (e.delta) parts.push(e.delta);
  if (e.next && e.kind === "turn") parts.push(`→ ${e.next}`);
  return parts.join(" ");
}

/** Clip for the collapsed first-prompt view. */
export function clipPrompt(s: string, max = PROMPT_PREVIEW_CHARS): { text: string; clipped: boolean } {
  const chars = [...s];
  if (chars.length <= max) return { text: s, clipped: false };
  return { text: `${chars.slice(0, max).join("")}…`, clipped: true };
}

/** A card title for a session with no live pane row: its latest task,
 *  else the start of its first prompt, else a short session id. */
export function sessionTitle(s: SessionContext): string {
  for (let i = s.log.length - 1; i >= 0; i--) {
    const task = s.log[i].task;
    if (task) return task;
  }
  if (s.first_prompt) return clipPrompt(s.first_prompt, 60).text;
  return s.session_id.slice(0, 8);
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
