// Unit tests for the card path line (Phase 91.E). Run:
//   cd app && node --experimental-strip-types --test src/cwdShort.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { shortenCwd } from "./cwdShort.ts";

test("home prefixes collapse to ~", () => {
  assert.equal(shortenCwd("/home/yossi/src/x", "yossi"), "~/src/x");
  assert.equal(shortenCwd("/home/yossi", "yossi"), "~");
  assert.equal(shortenCwd("/home/yossi/src/x", null), "~/src/x");
  assert.equal(shortenCwd("/root/deploy", "root"), "~/deploy");
  assert.equal(shortenCwd("/Users/yossi/dev/app", null), "~/dev/app");
  assert.equal(shortenCwd("C:\\Users\\yossi\\dev", null), "~\\dev");
});

test("another user's home is not ours", () => {
  assert.equal(shortenCwd("/home/alice/src", "yossi"), "/home/alice/src");
  assert.equal(shortenCwd("/root/x", "yossi"), "/root/x");
  assert.equal(shortenCwd("/homework/x", "yossi"), "/homework/x");
});

test("long paths keep the last two segments", () => {
  assert.equal(
    shortenCwd("/srv/projects/customer/very-long-repository-name/packages/web", null),
    "…/packages/web",
  );
  // 37 chars after the ~ collapse, so it trips the 34 cutoff (the earlier
  // single-letter version was 31 and did not — CI caught the test, not the code).
  assert.equal(shortenCwd("/home/yossi/aa/bb/cc/dd/ee/ff/gg/hh/ii/jj/kk/ll", "yossi"), "…/kk/ll");
  // Short enough is left alone even when it has many segments.
  assert.equal(shortenCwd("/a/b/c/d/e", null), "/a/b/c/d/e");
});
