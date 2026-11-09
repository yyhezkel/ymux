// Unit tests for the Diff pane's line notes. Run:
//   cd app && node --experimental-strip-types --test src/diffNotes.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseDiff } from "./diffModel.ts";
import {
  NOTE_MAX_CHARS,
  bracketedPastePayload,
  buildNotesPrompt,
  diffTargetKey,
  indexLines,
  noteKey,
  noteStatus,
  sanitizeInline,
  sanitizeNote,
  type DiffNote,
} from "./diffNotes.ts";

const DIFF = [
  "diff --git a/src/a.ts b/src/a.ts",
  "index 111..222 100644",
  "--- a/src/a.ts",
  "+++ b/src/a.ts",
  "@@ -10,3 +10,4 @@ fn x",
  " keep",
  "-gone",
  "+added one",
  "+added two",
  " tail",
  "\\ No newline at end of file",
  "",
].join("\n");

const note = (over: Partial<DiffNote>): DiffNote => ({
  id: "1",
  file: "src/a.ts",
  side: "new",
  line: 11,
  lineText: "added one",
  text: "rename",
  ...over,
});

const ctx = { cwd: "/srv/app", branch: "main", source: { kind: "head" } as const };

test("parseDiff numbers each side from the hunk header", () => {
  const p = parseDiff(DIFF);
  const by = (text: string) => p.refs[p.lines.findIndex((l) => l.text === text)];
  assert.deepEqual(by("keep"), { file: "src/a.ts", side: "new", line: 10 });
  assert.deepEqual(by("gone"), { file: "src/a.ts", side: "old", line: 11 });
  assert.deepEqual(by("added one"), { file: "src/a.ts", side: "new", line: 11 });
  assert.deepEqual(by("added two"), { file: "src/a.ts", side: "new", line: 12 });
  assert.deepEqual(by("tail"), { file: "src/a.ts", side: "new", line: 13 });
  assert.equal(by("\\ No newline at end of file"), null);
  assert.equal(p.refs.length, p.lines.length);
});

test("parseDiff reads SQL-style content as content, not a file header", () => {
  const p = parseDiff([
    "diff --git a/q.sql b/q.sql",
    "--- a/q.sql",
    "+++ b/q.sql",
    "@@ -1,2 +1,2 @@",
    "--- old comment",
    "+++ new comment",
    " select 1",
  ].join("\n"));
  assert.deepEqual(
    p.lines.slice(4).map((l) => [l.kind, l.text]),
    [["del", "-- old comment"], ["add", "++ new comment"], ["context", "select 1"]],
  );
  assert.deepEqual(p.refs[6], { file: "q.sql", side: "new", line: 2 });
});

test("a combined hunk gets no refs", () => {
  const p = parseDiff("diff --git a/m b/m\n@@@ -1,2 -1,2 +1,2 @@@\n+x");
  assert.deepEqual(p.refs, [null, null, null]);
});

test("a note survives a re-parse of the same diff", () => {
  const n = note({});
  assert.equal(noteStatus(n, indexLines(parseDiff(DIFF))), "current");
  assert.equal(noteStatus(n, indexLines(parseDiff(`${DIFF}`))), "current");
});

test("a note is outdated when the text changed or the line is gone", () => {
  const changed = DIFF.replace("+added one", "+added ONE");
  assert.equal(noteStatus(note({}), indexLines(parseDiff(changed))), "outdated");
  assert.equal(noteStatus(note({}), indexLines(parseDiff(""))), "outdated");
  assert.equal(noteStatus(note({ side: "old" }), indexLines(parseDiff(DIFF))), "outdated");
  assert.equal(noteStatus(note({ file: "other.ts" }), indexLines(parseDiff(DIFF))), "outdated");
});

test("keys keep distinct anchors and targets distinct", () => {
  assert.notEqual(noteKey(note({})), noteKey(note({ side: "old" })));
  assert.notEqual(
    diffTargetKey("w", "/a", { kind: "head" }),
    diffTargetKey("w", "/b", { kind: "head" }),
  );
  assert.notEqual(
    diffTargetKey("w", "/a", { kind: "ref", git_ref: "x" }),
    diffTargetKey("w", "/a", { kind: "ref", git_ref: "y" }),
  );
});

test("sanitizing removes terminal controls and bidi overrides", () => {
  assert.equal(sanitizeInline("a\x1b[201~b\x07\x9bc‮d\te\r\nf"), "a[201~bcd  e f");
  assert.equal(sanitizeNote("  one\r\ntwo three\x00  "), "one\ntwo\nthree");
  assert.equal(sanitizeNote("הערה בעברית"), "הערה בעברית");
  assert.equal(Array.from(sanitizeNote("x".repeat(NOTE_MAX_CHARS + 5))).length, NOTE_MAX_CHARS);
  assert.equal(sanitizeNote("\x1b\x07"), "");
});

test("the prompt lists every note with its anchor and marks outdated ones", () => {
  const out = buildNotesPrompt(
    [
      { note: note({}), status: "current" },
      { note: note({ id: "2", side: "old", line: 11, lineText: "gone", text: "why\nsecond" }), status: "outdated" },
    ],
    ctx,
  );
  assert.equal(
    out,
    [
      "Review notes on a git diff (all changes vs HEAD) in /srv/app, branch main. Address each note.",
      "",
      "1. src/a.ts:11 (new side)",
      "   code: added one",
      "   note: rename",
      "2. src/a.ts:11 (old side) [outdated: this line changed after the note]",
      "   code: gone",
      "   note: why",
      "   | second",
    ].join("\n"),
  );
  assert.ok(!out.endsWith("\n"), "no trailing line break: nothing may submit it");
});

test("the prompt carries no control character from any input", () => {
  const out = buildNotesPrompt(
    [{ note: note({ file: "a\x1b[201~\n.ts", lineText: "x\x1b]0;t\x07", text: "n\x1b[201~\rrm -rf /" }), status: "current" }],
    { cwd: "/a\x1b", branch: "b\x9b", source: { kind: "ref", git_ref: "r\x00" } },
  );
  // eslint-disable-next-line no-control-regex
  assert.doesNotMatch(out, /[\u0000-\u0009\u000b-\u001f\u007f-\u009f]/);
});

test("the prompt skips a note with no text and is empty with none left", () => {
  assert.equal(buildNotesPrompt([{ note: note({ text: "\x1b\x07" }), status: "current" }], ctx), "");
  assert.equal(buildNotesPrompt([], ctx), "");
});

test("the payload is one bracketed paste and has no Enter after it", () => {
  assert.equal(bracketedPastePayload("a\nb"), "\x1b[200~a\rb\x1b[201~");
  assert.equal(bracketedPastePayload("a\n"), "\x1b[200~a\x1b[201~");
  assert.equal(bracketedPastePayload(""), "");
});

test("a pasted end-bracket cannot close the paste early", () => {
  const out = bracketedPastePayload("x\x1b[201~ echo pwned");
  assert.equal(out.split("\x1b").length - 1, 2, "only our two ESCs");
  assert.ok(out.endsWith("\x1b[201~"));
});
