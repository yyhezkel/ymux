// Unit tests for the Diff pane's PR status model. Run:
//   cd app && node --experimental-strip-types --test src/prChecks.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildFixChecksPrompt,
  checkBucket,
  checkName,
  failingChecks,
  nextPollSecs,
  rollupCounts,
  runIdFromUrl,
  safeUrl,
  type PrCheck,
} from "./prChecks.ts";

const run = (name: string, status: string, conclusion: string | null, url?: string): PrCheck => ({
  __typename: "CheckRun",
  name,
  workflowName: "ci-windows",
  status,
  conclusion,
  detailsUrl: url ?? `https://github.com/o/r/actions/runs/100/job/${name.length}`,
});

const ctx = (context: string, state: string, url?: string): PrCheck => ({
  __typename: "StatusContext",
  context,
  state,
  targetUrl: url ?? "https://ci.example/build/7",
});

test("check runs: completed conclusions and pending statuses", () => {
  assert.equal(checkBucket(run("a", "COMPLETED", "SUCCESS")), "pass");
  assert.equal(checkBucket(run("a", "COMPLETED", "NEUTRAL")), "pass");
  assert.equal(checkBucket(run("a", "COMPLETED", "SKIPPED")), "pass");
  for (const c of ["FAILURE", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE", "STALE"]) {
    assert.equal(checkBucket(run("a", "COMPLETED", c)), "fail", c);
  }
  for (const s of ["QUEUED", "IN_PROGRESS", "WAITING", "PENDING", "REQUESTED"]) {
    assert.equal(checkBucket(run("a", s, null)), "pending", s);
  }
});

test("status contexts: state decides", () => {
  assert.equal(checkBucket(ctx("x", "SUCCESS")), "pass");
  assert.equal(checkBucket(ctx("x", "PENDING")), "pending");
  assert.equal(checkBucket(ctx("x", "EXPECTED")), "pending");
  assert.equal(checkBucket(ctx("x", "FAILURE")), "fail");
  assert.equal(checkBucket(ctx("x", "ERROR")), "fail");
});

test("rollup counts add up", () => {
  const counts = rollupCounts([
    run("a", "COMPLETED", "SUCCESS"),
    run("b", "COMPLETED", "FAILURE"),
    run("c", "IN_PROGRESS", null),
    ctx("d", "ERROR"),
    ctx("e", "SUCCESS"),
  ]);
  assert.deepEqual(counts, { pass: 2, fail: 2, pending: 1 });
  assert.deepEqual(rollupCounts([]), { pass: 0, fail: 0, pending: 0 });
});

test("run id comes from an Actions URL only", () => {
  assert.equal(runIdFromUrl("https://github.com/o/r/actions/runs/123456/job/9"), "123456");
  assert.equal(runIdFromUrl("https://github.com/o/r/actions/runs/77"), "77");
  assert.equal(runIdFromUrl("https://ci.example/build/7"), null);
  assert.equal(runIdFromUrl(null), null);
});

test("only https URLs survive", () => {
  assert.equal(safeUrl("https://github.com/o/r/pull/1"), "https://github.com/o/r/pull/1");
  assert.equal(safeUrl("javascript:alert(1)"), null);
  assert.equal(safeUrl("http://example.com"), null);
  assert.equal(safeUrl("https://a.example/x y"), null);
  assert.equal(safeUrl(""), null);
});

test("check names drop control characters and add the workflow", () => {
  assert.equal(checkName(run("rust", "COMPLETED", "FAILURE")), "ci-windows / rust");
  assert.equal(checkName({ name: "a\u001b[31mb\nc" }), "a[31mb c");
  assert.equal(checkName({}), "(unnamed check)");
});

test("fix prompt lists failing checks and each run once", () => {
  const checks = [
    run("rust", "COMPLETED", "FAILURE", "https://github.com/o/r/actions/runs/555/job/1"),
    run("go", "COMPLETED", "FAILURE", "https://github.com/o/r/actions/runs/555/job/2"),
    run("frontend", "COMPLETED", "SUCCESS"),
    ctx("ext/ci", "ERROR"),
  ];
  const failing = failingChecks(checks);
  assert.equal(failing.length, 3);
  const prompt = buildFixChecksPrompt(
    { number: 42, url: "https://github.com/o/r/pull/42", headRefName: "feat/x" },
    failing,
  );
  assert.ok(prompt.startsWith("PR #42 (https://github.com/o/r/pull/42) on branch feat/x has failing checks."));
  assert.ok(prompt.includes("1. ci-windows / rust - https://github.com/o/r/actions/runs/555/job/1"));
  assert.ok(prompt.includes("3. ext/ci - https://ci.example/build/7"));
  assert.equal(prompt.split("gh run view 555 --log-failed").length - 1, 1);
  assert.ok(prompt.includes("open its URL"));
  assert.ok(!prompt.includes("frontend"));
  assert.ok(!prompt.endsWith("\n"));
  // eslint-disable-next-line no-control-regex
  assert.ok(!/[\u0000-\u0009\u000b-\u001f\u007f-\u009f]/.test(prompt));
});

test("fix prompt is empty when nothing fails, and drops a bad PR URL", () => {
  assert.equal(buildFixChecksPrompt({ number: 1, url: "", headRefName: "" }, []), "");
  const p = buildFixChecksPrompt(
    { number: 1, url: "javascript:x", headRefName: "b‮ad" },
    [{ name: "a", url: null, runId: null }],
  );
  assert.ok(p.startsWith("PR #1 on branch bad has failing checks."));
  assert.ok(!p.includes("gh run view"));
});

test("poll interval backs off on failure only", () => {
  assert.equal(nextPollSecs(null, 0), 60);
  assert.equal(nextPollSecs({ kind: "no_pr" }, 0), 60);
  assert.equal(nextPollSecs({ kind: "gh_missing" }, 0), 600);
  assert.equal(nextPollSecs({ kind: "timeout" }, 1), 120);
  assert.equal(nextPollSecs({ kind: "error", message: "x" }, 3), 480);
  assert.equal(nextPollSecs({ kind: "timeout" }, 10), 900);
});
