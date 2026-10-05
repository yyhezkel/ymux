// Unit tests for the Context Rail model (Phase 103). Run:
//   cd app && node --experimental-strip-types --test src/contextModel.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampRailWidth,
  clipPrompt,
  isClosed,
  lastActivityMs,
  logIcon,
  logLineText,
  logNewestFirst,
  othersSummary,
  sessionTitle,
  RAIL_DEFAULT_W,
  RAIL_MAX_W,
  RAIL_MIN_W,
  type LogEntry,
  type SessionContext,
} from "./contextModel.ts";
import type { QueueRow } from "./queueModel.ts";

const entry = (over: Partial<LogEntry> = {}): LogEntry => ({
  ts_ms: 1,
  kind: "turn",
  status: "working",
  task: null,
  delta: null,
  next: null,
  ask: null,
  rec: null,
  degraded: false,
  ...over,
});

const session = (over: Partial<SessionContext> = {}): SessionContext => ({
  schema: 1,
  session_id: "0b6e3a1c-4f2d",
  ws_id: "ws1",
  pane_id: "p1",
  cwd: null,
  first_prompt: null,
  first_prompt_ms: null,
  log: [],
  version: 1,
  ...over,
});

const row = (over: Partial<QueueRow> = {}): QueueRow => ({
  wsId: "ws1",
  wsName: "CRM",
  paneId: "p1",
  title: "t",
  state: "done",
  stateSince: 1,
  startedAt: null,
  waitingOnPermission: false,
  connected: true,
  brief: null,
  light: "yellow",
  ...over,
});

test("clampRailWidth bounds and falls back", () => {
  assert.equal(clampRailWidth(10), RAIL_MIN_W);
  assert.equal(clampRailWidth(9999), RAIL_MAX_W);
  assert.equal(clampRailWidth(Number.NaN), RAIL_DEFAULT_W);
  assert.equal(clampRailWidth(300.4), 300);
});

test("logNewestFirst reverses without mutating and limits", () => {
  const log = [entry({ ts_ms: 1 }), entry({ ts_ms: 2 }), entry({ ts_ms: 3 })];
  assert.deepEqual(logNewestFirst(log, 2).map((e) => e.ts_ms), [3, 2]);
  assert.deepEqual(logNewestFirst(log, null).map((e) => e.ts_ms), [3, 2, 1]);
  assert.equal(log[0].ts_ms, 1);
});

test("logLineText prefers ask·rec, then delta, appends next", () => {
  assert.equal(logLineText(entry({ ask: "Ship?", rec: "Yes", delta: "d" })), "Ship? · Yes");
  assert.equal(logLineText(entry({ ask: "Ship?" })), "Ship?");
  assert.equal(logLineText(entry({ delta: "lock done", next: "wire it" })), "lock done → wire it");
  assert.equal(logLineText(entry({ kind: "closed", delta: "clear", next: "x" })), "clear");
  assert.equal(logLineText(entry()), "");
});

test("logIcon: closed wins over status", () => {
  assert.equal(logIcon(entry({ status: "stuck" })), "⚠️");
  assert.equal(logIcon(entry({ kind: "closed", status: "stuck" })), "✅");
});

test("clipPrompt counts characters, not UTF-16 units", () => {
  assert.deepEqual(clipPrompt("short", 10), { text: "short", clipped: false });
  const he = "ש".repeat(12);
  const c = clipPrompt(he, 10);
  assert.equal(c.clipped, true);
  assert.equal([...c.text].length, 11);
});

test("sessionTitle: latest task, then prompt, then id", () => {
  assert.equal(
    sessionTitle(session({ log: [entry({ task: "a" }), entry({ task: "b" }), entry()] })),
    "b",
  );
  assert.equal(sessionTitle(session({ first_prompt: "build it" })), "build it");
  assert.equal(sessionTitle(session()), "0b6e3a1c");
});

test("isClosed / lastActivityMs", () => {
  const s = session({ first_prompt_ms: 5, log: [entry({ ts_ms: 9, kind: "closed" })] });
  assert.equal(isClosed(s), true);
  assert.equal(lastActivityMs(s), 9);
  assert.equal(lastActivityMs(session({ first_prompt_ms: 5 })), 5);
});

test("othersSummary counts needs-you rows outside the current workspace", () => {
  const rows = [
    row({ wsId: "ws1", state: "needs-input" }), // current ws: excluded
    row({ wsId: "ws2", paneId: "a", state: "needs-input" }),
    row({ wsId: "ws2", paneId: "b", state: "running", light: "green" }), // working: excluded
    row({
      wsId: "ws3",
      paneId: "c",
      brief: {
        brief: {
          task: null, status: "waiting-for-you", ask: "q", rec: "r",
          next: null, delta: null, degraded: false, updated_ms: 1,
        },
        last_prompt: null, prompt_ms: null, session_ended: false, seq: 1,
      },
    }),
    row({ wsId: "ws4", paneId: "d", light: null, brief: null }), // not in queue
  ];
  const s = othersSummary(rows, "ws1");
  assert.equal(s.sessions, 2);
  assert.equal(s.workspaces, 2);
  assert.equal(s.firstWsId, "ws2", "blocked outranks waiting");
  assert.deepEqual(othersSummary([], "ws1"), { sessions: 0, workspaces: 0, firstWsId: null });
});
