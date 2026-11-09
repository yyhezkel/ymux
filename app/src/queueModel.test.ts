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
  buildJumpRows,
  compareQueueRows,
  inQueue,
  rowSinceMs,
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

const noCwd = () => null;
const plainShell = (paneId: string, wsId = "ws1"): QueueRow =>
  row({ paneId, wsId, state: "unknown", stateSince: null, light: null });

test("compareQueueRows: bucket first, then oldest, then no timestamp", () => {
  const blocked = row({ paneId: "b", state: "needs-input", light: "red", stateSince: NOW - 1_000 });
  const oldDone = row({ paneId: "o", stateSince: NOW - 90_000 });
  const newDone = row({ paneId: "n", stateSince: NOW - 10_000 });
  const noTime = row({ paneId: "t", stateSince: null });
  const sorted = [noTime, newDone, oldDone, blocked].sort(compareQueueRows);
  assert.deepEqual(sorted.map((r) => r.paneId), ["b", "o", "n", "t"]);
});

test("buildJumpRows: a blocked pane outranks a working pane of an earlier workspace", () => {
  const rows = [
    row({ wsId: "a", paneId: "working", state: "running", light: "green", startedAt: NOW - 500_000 }),
    withBrief({ status: "waiting-for-you" }, { wsId: "a", paneId: "waiting" }),
    row({ wsId: "b", paneId: "blocked", state: "needs-input", light: "red" }),
    row({ wsId: "b", paneId: "done" }),
  ];
  assert.deepEqual(
    buildJumpRows(rows, noCwd).map((r) => [r.paneId, r.status]),
    [
      ["blocked", "needs-input"],
      ["waiting", "waiting"],
      ["done", "done"],
      ["working", "working"],
    ],
  );
});

test("buildJumpRows: oldest first inside a bucket", () => {
  const rows = [
    row({ paneId: "new", state: "needs-input", light: "red", stateSince: NOW - 5_000 }),
    row({ paneId: "old", state: "needs-input", light: "red", stateSince: NOW - 50_000 }),
  ];
  assert.deepEqual(buildJumpRows(rows, noCwd).map((r) => r.paneId), ["old", "new"]);
});

test("buildJumpRows: a plain shell sorts after agents and has no invented status", () => {
  const rows = [
    plainShell("shell-1"),
    row({ paneId: "working", state: "running", light: "green" }),
    plainShell("shell-2"),
  ];
  const out = buildJumpRows(rows, noCwd);
  assert.deepEqual(out.map((r) => r.paneId), ["working", "shell-1", "shell-2"]);
  assert.equal(out[1].status, null);
});

test("buildJumpRows: a pane with no agent signal and no connection is left out", () => {
  const dead = row({ paneId: "dead", state: "unknown", light: null, connected: false });
  assert.deepEqual(buildJumpRows([dead], noCwd), []);
});

test("buildJumpRows: an ended session stays, as in the Queue", () => {
  const ended = row({
    paneId: "ended",
    light: null,
    connected: false,
    brief: { brief: null, last_prompt: null, prompt_ms: null, session_ended: true, seq: 1 },
  });
  assert.deepEqual(buildJumpRows([ended], noCwd).map((r) => r.status), ["ended"]);
});

test("buildJumpRows: carries the searchable fields and the workspace's cwd", () => {
  const rows = [row({ wsId: "w", wsName: "runner › crm", paneId: "p", title: "api" })];
  const [r] = buildJumpRows(rows, (id) => (id === "w" ? "/srv/crm" : null));
  assert.deepEqual(r, {
    wsId: "w",
    wsName: "runner › crm",
    paneId: "p",
    title: "api",
    cwd: "/srv/crm",
    status: "done",
  });
});

test("buildJumpRows does not reorder its input", () => {
  const rows = [
    row({ paneId: "done" }),
    row({ paneId: "blocked", state: "needs-input", light: "red" }),
  ];
  buildJumpRows(rows, noCwd);
  assert.deepEqual(rows.map((r) => r.paneId), ["done", "blocked"]);
});

test("rowSinceMs prefers the live turn start while working", () => {
  const r = withBrief({ updated_ms: NOW - 90_000 }, { state: "running", startedAt: NOW - 3_000 });
  assert.equal(rowSinceMs(r), NOW - 3_000);
});
