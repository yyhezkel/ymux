// Unit tests for the Diff pane model (Phase 91.F). Run:
//   cd app && node --experimental-strip-types --test src/diffModel.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseDiff, fileFromGitHeader, statusLetter, pathKey } from "./diffModel.ts";

test("fileFromGitHeader handles identical paths, spaces and renames", () => {
  assert.equal(fileFromGitHeader("diff --git a/src/x.rs b/src/x.rs"), "src/x.rs");
  assert.equal(fileFromGitHeader("diff --git a/my file.txt b/my file.txt"), "my file.txt");
  assert.equal(fileFromGitHeader("diff --git a/old.rs b/new.rs"), "new.rs");
});

test("parseDiff slices hunks and records anchors", () => {
  const text = [
    "diff --git a/a.rs b/a.rs",
    "index 111..222 100644",
    "--- a/a.rs",
    "+++ b/a.rs",
    "@@ -1,2 +1,2 @@",
    " ctx",
    "-old",
    "+new",
    "diff --git a/b.rs b/b.rs",
    "@@ -0,0 +1 @@",
    "+hello",
  ].join("\n");
  const p = parseDiff(text);
  assert.equal(p.hunks.length, 2);
  assert.equal(p.hunks[0].fileLabel, "a.rs");
  assert.equal(p.hunks[1].fileLabel, "b.rs");
  // anchors point at the `diff --git` line for each file.
  assert.equal(p.lines[p.anchors["a.rs"]].kind, "file");
  assert.ok(p.lines[p.anchors["a.rs"]].text.startsWith("diff --git a/a.rs"));
  assert.ok(p.anchors["b.rs"] > p.anchors["a.rs"]);
  // add/del lines strip their leading marker.
  assert.deepEqual(
    p.lines.filter((l) => l.kind === "add").map((l) => l.text),
    ["new", "hello"],
  );
});

test("parseDiff keeps a Binary files line as a file row", () => {
  const p = parseDiff("diff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ");
  assert.ok(p.lines.some((l) => l.kind === "file" && l.text.startsWith("Binary files")));
});

test("statusLetter maps porcelain codes", () => {
  assert.equal(statusLetter("??"), "?");
  assert.equal(statusLetter(" M"), "M");
  assert.equal(statusLetter("A "), "A");
  assert.equal(statusLetter("R "), "R");
  assert.equal(statusLetter("MM"), "M");
});

test("pathKey normalizes separators, trailing slash and drive case", () => {
  assert.equal(pathKey("C:\\Users\\y\\repo\\"), "c:/Users/y/repo");
  assert.equal(pathKey("/home/y/repo/"), "/home/y/repo");
  assert.equal(pathKey("a/b"), "a/b");
});
