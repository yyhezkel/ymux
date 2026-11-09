// Tests for the visual->logical clipboard transform.
// Run: cd app && node --experimental-strip-types --test src/copyBidi.test.ts
//
// The forward direction (logical -> visual) is reorderRtlForDisplay in bidi.ts,
// covered by bidi.test.ts. These tests are about the INVERSE: given what a
// visual-order pane put in the buffer, do we hand the clipboard something that
// reads correctly when pasted?

import test from "node:test";
import assert from "node:assert/strict";
import { stripIsolates, visualToLogical, visualToLogicalStream } from "./copyBidi.ts";
import { reorderRtlForDisplay } from "./bidi.ts";

/** The shapes bidi.test.ts already cares about, plus the ones that bite. */
const ROUND_TRIP = [
  "מה קורה אחי?",
  "שרת רץ על port 4200 — האם זה עובד?",
  "(שלום) עולם",
  "claude-fable-5 · ~ צבר·full",
  "אוקי",
  "יש 42 קבצים בתיקייה",
  "> מה קורה אחי?",
];

test("un-reverses what the display reorder produced", () => {
  for (const logical of ROUND_TRIP) {
    const visual = reorderRtlForDisplay(logical);
    assert.equal(
      visualToLogical(visual),
      logical,
      `round trip failed for ${JSON.stringify(logical)}`,
    );
  }
});

test("text with no RTL is returned untouched", () => {
  // The overwhelmingly common case — every copy in an English session goes
  // through here, so identity has to be exact, not merely equivalent.
  const shellPrompt = String.raw`PS C:\Users\me> git status`;
  for (const s of ["", "hello world", shellPrompt, "1234"]) {
    assert.equal(visualToLogical(s), s);
  }
});

test("digits inside a Hebrew run keep their own order", () => {
  // "42" must not come back as "24". This is the thing a naive
  // reverse-the-whole-string implementation gets wrong.
  const logical = "יש 42 קבצים בתיקייה";
  assert.equal(visualToLogical(reorderRtlForDisplay(logical)), logical);
  assert.ok(visualToLogical(reorderRtlForDisplay(logical)).includes("42"));
});

test("mirrored brackets come back the right way round", () => {
  const logical = "(שלום) עולם";
  const visual = reorderRtlForDisplay(logical);
  // The forward pass really did mirror them — otherwise this test proves nothing.
  assert.notEqual(visual, logical);
  assert.equal(visualToLogical(visual), logical);
});

test("niqqud stays attached to its base letter", () => {
  // UAX #9 W1 gives a combining mark its base letter's level, so a reversal
  // moves the mark AHEAD of its base. The round trip has to put it back.
  const logical = "שָׁלוֹם";
  const visual = reorderRtlForDisplay(logical);
  assert.notEqual(visual, logical);
  assert.equal(visualToLogical(visual), logical);
});

test("an astral character is not torn in half", () => {
  // Array.from, not split(""). A surrogate pair inside a reversed run would
  // otherwise come back as two lone surrogates.
  const logical = "א🌵ב";
  const out = visualToLogical(reorderRtlForDisplay(logical));
  assert.equal(out, logical);
  assert.ok(!/[\uD800-\uDFFF]/.test(out.replace(/[\u{10000}-\u{10FFFF}]/gu, "")));
});

test("line structure survives, including CRLF", () => {
  const logical = "מה קורה אחי?\r\nשלום עולם\nאוקי";
  assert.equal(visualToLogical(reorderRtlForDisplay(logical)), logical);
});

test("each line resolves its own direction", () => {
  // A Latin-first line and a Hebrew-first line in one selection must not be
  // forced to share a paragraph direction.
  const logical = "claude-fable-5 · ~ צבר·full\nמה קורה אחי?";
  assert.equal(visualToLogical(reorderRtlForDisplay(logical)), logical);
});

test("KNOWN AMBIGUITY: adjacent digits and Latin at an RTL edge", () => {
  // Not a bug we can fix — the two logical readings below have the SAME visual
  // form, so the information is destroyed before we ever see it. Pinned so the
  // day someone "fixes" it, they find out here that there is nothing to fix.
  const a = "שלום עולם 123 abc";
  const b = "שלום עולם abc 123";
  const va = reorderRtlForDisplay(a);
  const vb = reorderRtlForDisplay(b);
  assert.equal(va, vb, "precondition: the two readings collide in visual form");

  const out = visualToLogical(va);
  assert.ok(
    out === a || out === b,
    `expected one of the two readings, got ${JSON.stringify(out)}`,
  );
});

// ── The stream variant ───────────────────────────────────────────────────
//
// Same conversion, but for PTY chunks, which carry ANSI escapes. The plain
// `visualToLogical` is escape-blind by design (a clipboard selection has no
// escapes); handing it a stream chunk would permute the bytes of a CSI
// sequence. These pin that the stream variant does not.

const CSI = "\x1b[31m";
const RESET = "\x1b[0m";
const OSC = "\x1b]0;title\x07";
const CRLF = "\r\n";

test("stream: escapes pass through byte-for-byte", () => {
  const logical = "מה קורה אחי?";
  const visual = reorderRtlForDisplay(logical);
  const out = visualToLogicalStream(CSI + visual + RESET);
  assert.equal(out, CSI + logical + RESET);
});

test("stream: an OSC sequence is not reordered into garbage", () => {
  // The failure this exists to prevent: `OSC 0 ; title BEL` treated as text and
  // permuted, which would corrupt the title and could emit a stray BEL.
  const out = visualToLogicalStream(OSC + reorderRtlForDisplay("שלום"));
  assert.ok(out.startsWith(OSC), `OSC mangled: ${JSON.stringify(out)}`);
  assert.equal(out, OSC + "שלום");
});

test("stream: a chunk with no RTL is returned identical", () => {
  const s = CSI + String.raw`PS C:\Users\me> git status` + RESET + CRLF;
  assert.equal(visualToLogicalStream(s), s);
});

test("stream: escapes between text runs still split the runs", () => {
  // Documented limit, pinned so it is a decision and not a surprise: each run
  // between escapes resolves its own base direction.
  const a = reorderRtlForDisplay("שלום");
  const b = reorderRtlForDisplay("עולם");
  const out = visualToLogicalStream(a + CSI + b);
  assert.equal(out, "שלום" + CSI + "עולם");
});

test("stream: a mid-line fragment is handled, not thrown", () => {
  // `merged` is a TIME boundary (one rAF), never a content boundary, so
  // fragments are guaranteed to arrive. This does not assert correctness of the
  // split — only that it is total and lossless in length.
  const visual = reorderRtlForDisplay("שרת רץ על port 4200 — האם זה עובד?");
  for (let cut = 1; cut < visual.length; cut += 7) {
    const head = visual.slice(0, cut);
    const tail = visual.slice(cut);
    const out = visualToLogicalStream(head) + visualToLogicalStream(tail);
    assert.equal([...out].length, [...visual].length, `length changed at cut ${cut}`);
  }
});

test("stream and clipboard agree when there are no escapes", () => {
  const visual = reorderRtlForDisplay("יש 42 קבצים בתיקייה");
  assert.equal(visualToLogicalStream(visual), visualToLogical(visual));
});

test("stripIsolates removes U+2066-U+2069 and nothing else", () => {
  assert.equal(stripIsolates("\u2068main.rs\u2069"), "main.rs");
  assert.equal(stripIsolates("\u2066a\u2067b\u2068c\u2069"), "abc");
  assert.equal(stripIsolates("שלום main.rs"), "שלום main.rs");
  // Neighbours of the range stay: LRM/RLM, ZWJ, and U+2065 / U+206A.
  const keep = "\u200e\u200f\u200d\u2065\u206a";
  assert.equal(stripIsolates(keep), keep);
  assert.equal(stripIsolates("\u2068\u2069"), "");
});
