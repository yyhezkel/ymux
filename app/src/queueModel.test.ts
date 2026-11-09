// Unit tests for the Queue model (BRIEF). Run:
//   cd app && node --experimental-strip-types --test src/queueModel.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
//
// The thesis these guard: live hook state outranks a brief for placement,
// and the queue never invents urgency — a running pane is bucket 3 no
// matter what its previous brief said.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  queueStatus,
  QUEUE_BUCKET,
  whatsHappening,
  groupQueueRows,
  inQueue,
  rowSinceMs,
  overviewVerdict,
  type QueueRow,
} from "./queueModel.ts";
import type { PaneBrief } from "./bindings/PaneBrief.ts";

const NOW = 1_700_000_000_000;

const brief = (over: Partial<PaneBrief> = {}): PaneBrief => ({
  task: null,
  status: "done",
  ask: null,
  rec: null,
  next: null,
  delta: null,
  goal: null,
  done: null,
  degraded: false,
  updated_ms: NOW - 60_000,
  ...over,
});

const row = (over: Partial<QueueRow> = {}): QueueRow => ({
  wsId: "ws1",
  wsName: "CRM",
  paneId: "p1",
  title: "supplier-flows",
  state: "done",
  stateSince: NOW - 60_000,
  startedAt: null,
  waitingOnPermission: false,
  connected: true,
  brief: null,
  light: "yellow",
  ...over,
});

const withBrief = (b: Partial<PaneBrief>, over: Partial<QueueRow> = {}): QueueRow =>
  row({
    brief: {
      brief: brief(b),
      last_prompt: null,
      prompt_ms: null,
      session_ended: false,
      seq: 1,
    },
    ...over,
  });

test("permission card outranks everything, including a calm brief", () => {
  const r = withBrief({ status: "done" }, { waitingOnPermission: true });
  assert.equal(queueStatus(r), "needs-input");
  assert.equal(QUEUE_BUCKET[queueStatus(r)], 0);
});

test("a failed pane is stuck, not done", () => {
  // An API-error turn must land in the stuck bucket, not look finished.
  const r = withBrief({ status: "done" }, { state: "failed" });
  assert.equal(queueStatus(r), "stuck");
});

test("a running pane sorts as running no matter what the old brief said", () => {
  const r = withBrief({ status: "stuck" }, { state: "running", startedAt: NOW - 5_000 });
  assert.equal(queueStatus(r), "working");
  assert.equal(QUEUE_BUCKET[queueStatus(r)], 3);
});

test("brief statuses refine the ended bucket", () => {
  assert.equal(queueStatus(withBrief({ status: "stuck" })), "stuck");
  assert.equal(queueStatus(withBrief({ status: "waiting-for-you" })), "waiting");
  assert.equal(queueStatus(withBrief({ status: "done" })), "done");
});

test("session_ended wins over the brief status", () => {
  const r = withBrief({ status: "waiting-for-you" });
  r.brief!.session_ended = true;
  r.state = "unknown";
  r.light = null;
  assert.equal(queueStatus(r), "ended");
  assert.equal(QUEUE_BUCKET.ended, 2);
  // …and an ended session still earns a queue slot via its brief.
  assert.ok(inQueue(r));
});

test("running rows show the user's last prompt; brief only as a dim fallback", () => {
  const r = withBrief({ delta: "old delta" }, { state: "running", startedAt: NOW });
  r.brief!.last_prompt = "run the migration on DEV";
  const h = whatsHappening(r);
  assert.equal(h?.kind, "prompt");
  assert.equal(h?.text, "run the migration on DEV");
  assert.equal(h?.dim, false);

  r.brief!.last_prompt = null;
  const h2 = whatsHappening(r);
  assert.equal(h2?.kind, "delta");
  assert.equal(h2?.dim, true);
});

test("ask · rec beats delta beats next", () => {
  const r = withBrief({ ask: "Redis or JWT?", rec: "Redis", delta: "d", next: "n" });
  assert.equal(whatsHappening(r)?.text, "Redis or JWT? · Redis");
  const r2 = withBrief({ delta: "d", next: "n" });
  assert.equal(whatsHappening(r2)?.kind, "delta");
  const r3 = withBrief({ next: "n" });
  assert.equal(whatsHappening(r3)?.kind, "next");
});

test("rows with no agent signal stay out of the queue", () => {
  assert.equal(inQueue(row({ light: null, brief: null })), false);
  assert.equal(inQueue(row({ light: "green" })), true);
});

test("groups sort by most urgent row, rows oldest-first inside a bucket", () => {
  const rows: QueueRow[] = [
    row({ wsId: "a", wsName: "Tax", paneId: "t1", state: "running", startedAt: NOW }),
    withBrief({ status: "waiting-for-you", updated_ms: NOW - 10_000 }, {
      wsId: "b",
      wsName: "CRM",
      paneId: "c1",
      stateSince: NOW - 10_000,
    }),
    withBrief({ status: "waiting-for-you", updated_ms: NOW - 90_000 }, {
      wsId: "b",
      wsName: "CRM",
      paneId: "c2",
      stateSince: NOW - 90_000,
    }),
  ];
  const groups = groupQueueRows(rows);
  assert.equal(groups[0].wsName, "CRM"); // has waiting rows; Tax only runs
  assert.equal(groups[0].attention, 2);
  assert.equal(groups[0].rows[0].paneId, "c2"); // oldest waiting first
  assert.equal(groups[1].wsName, "Tax");
  assert.equal(groups[1].attention, 0);
});

test("rowSinceMs prefers the live turn start while working", () => {
  const r = withBrief({ updated_ms: NOW - 90_000 }, { state: "running", startedAt: NOW - 3_000 });
  assert.equal(rowSinceMs(r), NOW - 3_000);
});

// ---- overviewVerdict -------------------------------------------------------
const withPrompt = (promptMs: number, r: QueueRow): QueueRow => ({
  ...r,
  brief: { ...r.brief!, last_prompt: "fix it", prompt_ms: promptMs },
});

test("overview: missing or empty pane_ids fall back to the screen read", () => {
  assert.deepEqual(overviewVerdict(undefined, [withBrief({})]), { source: "screen" });
  assert.deepEqual(overviewVerdict([], [withBrief({})]), { source: "screen" });
});

test("overview: a pane id with no matching row is a screen read", () => {
  assert.deepEqual(overviewVerdict(["other"], [withBrief({})]), { source: "screen" });
});

test("overview: a current brief gives the queue status and the delta, not tickable", () => {
  const v = overviewVerdict(["p1"], [withBrief({ status: "stuck", delta: "broke" })]);
  assert.equal(v.source, "queue");
  if (v.source !== "queue") return;
  assert.equal(v.status, "stuck");
  assert.equal(v.happening?.text, "broke");
  assert.equal(v.summaryTickable, false);
});

test("overview: a disconnected pane does not count", () => {
  const r = withBrief({ delta: "x" }, { connected: false });
  assert.deepEqual(overviewVerdict(["p1"], [r]), { source: "screen" });
});

test("overview: a pane not in the queue does not count", () => {
  assert.deepEqual(overviewVerdict(["p1"], [row({ light: null, brief: null })]), { source: "screen" });
});

test("overview: a prompt newer than the brief voids it, unless a live hook says otherwise", () => {
  const base = withBrief({ delta: "x", updated_ms: NOW - 60_000 });
  const stale = withPrompt(NOW - 1_000, base);
  assert.deepEqual(overviewVerdict(["p1"], [stale]), { source: "screen" });
  for (const over of [
    { state: "running" as const },
    { state: "needs-input" as const },
    { state: "failed" as const },
    { waitingOnPermission: true },
  ]) {
    assert.equal(overviewVerdict(["p1"], [{ ...stale, ...over }]).source, "queue");
  }
});

test("overview: a prompt not newer than the brief keeps it", () => {
  const base = withBrief({ delta: "x", updated_ms: NOW - 1_000 });
  assert.equal(overviewVerdict(["p1"], [withPrompt(NOW - 1_000, base)]).source, "queue");
});

test("overview: a brief has no age cap", () => {
  const r = withBrief({ delta: "x", updated_ms: 1_000 });
  assert.equal(overviewVerdict(["p1"], [r]).source, "queue");
});

test("overview: a degraded brief keeps the status and leaves the summary tickable", () => {
  const v = overviewVerdict(["p1"], [withBrief({ degraded: true, delta: "first line" })]);
  assert.equal(v.source, "queue");
  if (v.source !== "queue") return;
  assert.equal(v.status, "done");
  assert.equal(v.summaryTickable, true);
});

test("overview: a status with no text to show leaves the summary tickable", () => {
  const v = overviewVerdict(["p1"], [row({ state: "done", brief: null })]);
  assert.equal(v.source, "queue");
  if (v.source !== "queue") return;
  assert.equal(v.happening, null);
  assert.equal(v.summaryTickable, true);
});

test("overview: the most urgent pane on a session wins", () => {
  const calm = withBrief({ status: "done", delta: "calm" }, { paneId: "a" });
  const stuck = withBrief({ status: "stuck", delta: "help" }, { paneId: "b" });
  const running = withBrief({ delta: "old" }, { paneId: "c", state: "running" });
  for (const order of [[calm, stuck, running], [running, stuck, calm]]) {
    const v = overviewVerdict(["a", "b", "c"], order);
    assert.equal(v.source === "queue" && v.status, "stuck");
  }
});

test("overview: a tie goes to the most recent situation", () => {
  const older = withBrief({ delta: "old" }, { paneId: "a", stateSince: NOW - 90_000 });
  const newer = withBrief({ delta: "new" }, { paneId: "b", stateSince: NOW - 1_000 });
  for (const order of [[older, newer], [newer, older]]) {
    const v = overviewVerdict(["a", "b"], order);
    assert.equal(v.source === "queue" && v.happening?.text, "new");
  }
});

test("overview: only the panes on the session are read", () => {
  const mine = withBrief({ status: "done", delta: "mine" }, { paneId: "a" });
  const other = withBrief({ status: "stuck", delta: "other" }, { paneId: "b" });
  const v = overviewVerdict(["a"], [mine, other]);
  assert.equal(v.source === "queue" && v.happening?.text, "mine");
});
