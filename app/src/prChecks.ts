// PR status for the Diff pane: the result of `diff_pane_pr_status`, the
// check rollup → counts, and the "Fix broken checks" prompt. Pure, with
// type-only imports apart from `diffNotes.ts`, so `prChecks.test.ts` runs
// under `node --test`.
//
// Every string here comes from gh, so from the repository and its CI. Treat
// all of it as untrusted: it is sanitized before it reaches a prompt.

import { sanitizeInline } from "./diffNotes.ts";

/** One `statusCheckRollup` entry. Mirrors `PrCheck` in `pr_status.rs`. */
export interface PrCheck {
  __typename?: string | null;
  name?: string | null;
  context?: string | null;
  workflowName?: string | null;
  status?: string | null;
  conclusion?: string | null;
  state?: string | null;
  detailsUrl?: string | null;
  targetUrl?: string | null;
}

/** Mirrors `PrInfo` in `pr_status.rs`. */
export interface PrInfo {
  number: number;
  title: string;
  state: string;
  isDraft: boolean;
  url: string;
  headRefName: string;
  statusCheckRollup: PrCheck[];
}

/** Mirrors `PrStatus` in `pr_status.rs` (serde tag `kind`). */
export type PrStatus =
  | { kind: "ok"; pr: PrInfo }
  | { kind: "gh_missing" }
  | { kind: "not_authenticated" }
  | { kind: "no_pr" }
  | { kind: "not_github" }
  | { kind: "timeout" }
  | { kind: "no_session" }
  | { kind: "error"; message: string };

export type CheckBucket = "pass" | "fail" | "pending";

export interface CheckCounts {
  pass: number;
  fail: number;
  pending: number;
}

/** A failing check, ready for the prompt and the UI. */
export interface FailingCheck {
  name: string;
  url: string | null;
  runId: string | null;
}

const PASSING_CONCLUSIONS = new Set(["SUCCESS", "NEUTRAL", "SKIPPED"]);
const PENDING_STATES = new Set(["PENDING", "EXPECTED"]);

/** CheckRun: not COMPLETED is pending; SUCCESS, NEUTRAL and SKIPPED pass;
 *  every other conclusion (FAILURE, CANCELLED, TIMED_OUT, ACTION_REQUIRED,
 *  STARTUP_FAILURE, STALE) fails. StatusContext: SUCCESS passes, PENDING
 *  and EXPECTED are pending, ERROR and FAILURE fail. */
export function checkBucket(c: PrCheck): CheckBucket {
  const upper = (s: string | null | undefined) => (s ?? "").toUpperCase();
  const isContext = c.__typename === "StatusContext" || (c.__typename == null && c.state != null && c.status == null);
  if (isContext) {
    const st = upper(c.state);
    if (st === "SUCCESS") return "pass";
    if (PENDING_STATES.has(st) || st === "") return "pending";
    return "fail";
  }
  if (upper(c.status) !== "COMPLETED") return "pending";
  return PASSING_CONCLUSIONS.has(upper(c.conclusion)) ? "pass" : "fail";
}

export function rollupCounts(checks: PrCheck[]): CheckCounts {
  const out: CheckCounts = { pass: 0, fail: 0, pending: 0 };
  for (const c of checks) out[checkBucket(c)] += 1;
  return out;
}

/** Only an https URL may be opened or put in a prompt. */
export function safeUrl(u: string | null | undefined): string | null {
  if (!u) return null;
  const s = sanitizeInline(u);
  return /^https:\/\/[^\s]+$/i.test(s) ? s : null;
}

/** The Actions run id in a GitHub check URL, or null for an external CI. */
export function runIdFromUrl(u: string | null | undefined): string | null {
  const m = /\/actions\/runs\/(\d{1,20})(?:[/?#]|$)/.exec(u ?? "");
  return m ? m[1] : null;
}

export function checkName(c: PrCheck): string {
  const base = sanitizeInline(c.name ?? c.context ?? "") || "(unnamed check)";
  const wf = sanitizeInline(c.workflowName ?? "");
  return wf && wf !== base ? `${wf} / ${base}` : base;
}

export function failingChecks(checks: PrCheck[]): FailingCheck[] {
  return checks
    .filter((c) => checkBucket(c) === "fail")
    .map((c) => {
      const url = safeUrl(c.detailsUrl ?? c.targetUrl);
      return { name: checkName(c), url, runId: runIdFromUrl(url) };
    });
}

/** The exact text the user sends. The frame stays in English: it is an
 *  instruction to an agent, not UI. Returns "" when nothing fails. Never
 *  ends with a line break. Each run id appears once, because one run can
 *  hold several failing jobs. */
export function buildFixChecksPrompt(
  pr: Pick<PrInfo, "number" | "url" | "headRefName">,
  failing: FailingCheck[],
): string {
  if (failing.length === 0) return "";
  const url = safeUrl(pr.url);
  const branch = sanitizeInline(pr.headRefName);
  const head = `PR #${pr.number}${url ? ` (${url})` : ""}${branch ? ` on branch ${branch}` : ""} has failing checks.`;
  const lines = failing.map((f, i) => `${i + 1}. ${f.name}${f.url ? ` - ${f.url}` : ""}`);
  const runIds = [...new Set(failing.flatMap((f) => (f.runId ? [f.runId] : [])))];
  const how = runIds.length > 0
    ? [
        "Read the failed logs with:",
        ...runIds.map((id) => `  gh run view ${id} --log-failed`),
      ]
    : [];
  const external = failing.some((f) => !f.runId)
    ? ["For a check without a run id, open its URL to read the log."]
    : [];
  return [
    head,
    "",
    ...lines,
    "",
    ...how,
    ...external,
    "Find the cause of each failure, fix it, and say what you changed. Do not push until I confirm.",
  ].join("\n");
}

/** Seconds until the next poll. Success and "no PR" poll every minute. A
 *  state the user must fix outside the app polls every 10 minutes. A
 *  transient failure doubles the wait, up to 15 minutes. */
export function nextPollSecs(status: PrStatus | null, failures: number): number {
  const BASE = 60;
  if (!status) return BASE;
  switch (status.kind) {
    case "ok":
    case "no_pr":
      return BASE;
    case "gh_missing":
    case "not_authenticated":
    case "not_github":
      return 600;
    case "timeout":
    case "error":
    case "no_session":
      return Math.min(900, BASE * 2 ** Math.max(1, Math.min(failures, 4)));
  }
}
