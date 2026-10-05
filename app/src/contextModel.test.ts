// Unit tests for the Context Rail model (Phase 104). Run:
//   cd app && node --experimental-strip-types --test src/contextModel.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  cardGoal,
  clampRailWidth,
  doneCount,
  doneEntries,
  isClosed,
  lastActivityMs,
  lastTurn,
  logIcon,
  oneLine,
  sessionsForPane,
  waitingText,
  GOAL_MAX_CHARS,
  LINE_MAX_CHARS,
  RAIL_DEFAULT_W,
  RAIL_MAX_W,
  RAIL_MIN_W,
  type LogEntry,
  type SessionContext,
} from "./contextModel.ts";

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
  goal: null,
  done_when: null,
  log: [],
  version: 1,
  ...over,
});

test("clampRailWidth bounds and falls back", () => {
  assert.equal(clampRailWidth(10), RAIL_MIN_W);
  assert.equal(clampRailWidth(9999), RAIL_MAX_W);
  assert.equal(clampRailWidth(Number.NaN), RAIL_DEFAULT_W);
  assert.equal(clampRailWidth(300.4), 300);
});

test("oneLine flattens, clips by code point, keeps the full text", () => {
  assert.deepEqual(oneLine("a\n  b\tc"), { text: "a b c", full: "a b c", clipped: false });
  const he = "ש".repeat(LINE_MAX_CHARS + 5);
  const o = oneLine(he);
  assert.equal(o.clipped, true);
  assert.equal([...o.text].length, LINE_MAX_CHARS + 1);
  assert.equal(o.full, he);
});

test("cardGoal: sticky goal wins, else first prompt line clipped to 80", () => {
  assert.deepEqual(cardGoal(session({ goal: "Ship v2", first_prompt: "hi" })), {
    text: "Ship v2",
    full: "Ship v2",
    fromPrompt: false,
  });
  const g = cardGoal(session({ first_prompt: "\n  build the installer\nwith locks" }));
  assert.equal(g?.text, "build the installer");
  assert.equal(g?.fromPrompt, true);
  const long = cardGoal(session({ first_prompt: "x".repeat(200) }));
  assert.equal([...(long?.text ?? "")].length, GOAL_MAX_CHARS + 1);
  assert.equal(cardGoal(session({ goal: "  ", first_prompt: null })), null);
});

test("lastTurn / waitingText follow the latest turn and stop after close", () => {
  const asked = session({
    log: [entry({ task: "a" }), entry({ task: "b", ask: "Ship?", rec: "Yes" })],
  });
  assert.equal(lastTurn(asked)?.task, "b");
  assert.equal(waitingText(asked), "Ship? · Yes");
  assert.equal(waitingText(session({ log: [entry({ ask: "Q" })] })), "Q");
  const closed = session({ log: [...asked.log, entry({ kind: "closed" })] });
  assert.equal(lastTurn(closed)?.task, "b");
  assert.equal(waitingText(closed), null);
  assert.equal(waitingText(session({ log: [entry({ ask: null })] })), null);
});

test("doneEntries: deltas + closed lines, newest first, degraded included", () => {
  const s = session({
    log: [
      entry({ ts_ms: 1, delta: "one" }),
      entry({ ts_ms: 2, delta: null }),
      entry({ ts_ms: 3, delta: "three", degraded: true }),
      entry({ ts_ms: 4, delta: "four" }),
      entry({ ts_ms: 5, kind: "closed" }),
    ],
  });
  assert.deepEqual(doneEntries(s, 3).map((e) => e.ts_ms), [5, 4, 3]);
  assert.deepEqual(doneEntries(s, null).map((e) => e.ts_ms), [5, 4, 3, 1]);
  assert.equal(doneCount(s), 4);
  assert.equal(s.log[0].ts_ms, 1, "input untouched");
});

test("logIcon: closed wins over status", () => {
  assert.equal(logIcon(entry({ status: "stuck" })), "⚠️");
  assert.equal(logIcon(entry({ kind: "closed", status: "stuck" })), "✅");
});

test("isClosed / lastActivityMs", () => {
  const s = session({ first_prompt_ms: 5, log: [entry({ ts_ms: 9, kind: "closed" })] });
  assert.equal(isClosed(s), true);
  assert.equal(lastActivityMs(s), 9);
  assert.equal(lastActivityMs(session({ first_prompt_ms: 5 })), 5);
});

test("sessionsForPane: only that pane, newest first", () => {
  const sessions = [
    session({ session_id: "old", pane_id: "p1", first_prompt_ms: 1 }),
    session({ session_id: "other", pane_id: "p2", first_prompt_ms: 9 }),
    session({ session_id: "new", pane_id: "p1", log: [entry({ ts_ms: 5 })] }),
  ];
  assert.deepEqual(sessionsForPane(sessions, "p1").map((s) => s.session_id), ["new", "old"]);
  assert.deepEqual(sessionsForPane(sessions, null), []);
  assert.deepEqual(sessionsForPane(sessions, "p9"), []);
  assert.equal(sessions[0].session_id, "old", "input untouched");
});
