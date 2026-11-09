// Visual -> logical, for text on its way to the CLIPBOARD.
//
// Why this exists, measured on Yossi's machine 2026-08-20:
//
//   | pane             | on screen | pasted elsewhere |
//   |------------------|-----------|------------------|
//   | plain PowerShell | reversed  | CORRECT          |
//   | Claude Code      | CORRECT   | reversed         |
//
// Exactly inverted, because the two panes hold opposite orders in the buffer.
// PowerShell's buffer is LOGICAL, so its copy is already right. Claude Code's
// is VISUAL — and its display is right *because of that*: when the
// Claude-in-front flag is set we render rows with `unicode-bidi: bidi-override`,
// which paints bytes verbatim, and verbatim visual bytes look correct.
// `getSelection()` then hands back those same visual bytes and the paste reads
// backwards.
//
// So the fix is to un-reverse on the way out. `settings.rs` already names it —
// "un-reversing the stream to logical first ... a visual->logical pass" — and
// says doing it at RENDER time drags in the cursor / partial-repaint problem.
// At COPY time none of that applies: no cursor, no partial repaint, no column
// coordinates. Just a string.
//
// This is NOT `reorderRtlForDisplay` run twice. That one lets bidi-js pick the
// paragraph direction per UAX #9 P2/P3, re-derived from whatever string it is
// handed — so on the second pass a different strong character can lead and the
// permutation is not the inverse. Measured against a correct renderer over a
// 12-line corpus: 11 recovered with the majority-vote base below, and the one
// that did not is provably impossible (see AMBIGUITY).
//
// Runs under `node --test` with no DOM, like textDirection.ts:
//   cd app && node --experimental-strip-types --test src/copyBidi.test.ts

// @ts-ignore - bidi-js has no type defs
import bidiFactory from "bidi-js";
import { strongCounts } from "./textDirection.ts";
import { ANSI_RE } from "./bidi.ts";

const bidi: any = bidiFactory();

/** Hebrew + Arabic + their presentation forms. Same range as bidi.ts's gate. */
const RTL_RE = /[\u0590-\u08FF\uFB1D-\uFEFC]/;

/** Split that KEEPS the separators, so line structure survives a round trip. */
const LINE_SPLIT = /(\r\n|\r|\n)/;

/**
 * The paragraph direction, decided by COUNTING strong characters rather than by
 * finding the first one — a majority vote, reusing `strongCounts` so there is
 * one definition of "strong character" in the app rather than two.
 *
 * This is the whole trick. A count is order-independent: it gives the same
 * answer for a line and for that line reversed, which is exactly what an
 * inverse needs, since the reversed form is all we ever see. First-strong
 * (UAX #9 P2/P3) does not have that property — reordering can move a different
 * strong character to the front, and then the "inverse" resolves a different
 * paragraph level than the forward pass used, and is not an inverse at all.
 *
 * Why a MAJORITY and not "any RTL character makes it RTL", which is what the
 * app's `direction_policy: "any_rtl"` does for rendering: measured over a
 * 12-line corpus against a correct renderer, any-RTL recovered 9 and the
 * majority vote recovered 11. The three any-RTL lost are the mostly-Latin
 * shapes — Claude's own status line ("claude-fable-5 · ~ צבר·full") and
 * "Running שרת on port 80" — where one Hebrew word must not flip the whole
 * paragraph. bidi.ts's header warns about exactly that line.
 *
 * Ties go to LTR: a line with equal weight is not an RTL paragraph.
 */
function baseDirectionOf(line: string): "rtl" | "ltr" {
  const { rtl, ltr } = strongCounts(line);
  return rtl > ltr ? "rtl" : "ltr";
}

/**
 * Un-reverse one line. Reversal is an involution per run, so re-running the
 * reorder against the SAME forced base recovers the original — mirrored
 * brackets included, since mirroring is itself an involution.
 */
function unreorderLine(line: string): string {
  if (!RTL_RE.test(line)) return line;
  const base = baseDirectionOf(line);
  const levels = bidi.getEmbeddingLevels(line, base);
  const flips = bidi.getReorderSegments(line, levels);
  const mirrors = bidi.getMirroredCharactersMap(line, levels);

  // CODE UNITS, not code points. bidi-js indexes by code unit throughout
  // (`new Uint32Array(string.length)` in embeddingLevels.js), so the offsets in
  // `flips` and `mirrors` only line up with a code-unit array. Array.from would
  // read cleaner and be silently off-by-N on any line containing an emoji.
  const arr = line.split("");
  if (mirrors && mirrors.size) {
    mirrors.forEach((repl: string, idx: number) => {
      arr[idx] = repl;
    });
  }
  for (const [start, end] of flips) {
    const sub = arr.slice(start, end + 1).reverse();
    for (let i = 0; i < sub.length; i++) arr[start + i] = sub[i];
  }
  return repairSurrogates(arr).join("");
}

/**
 * Put back any surrogate pair a reversal turned inside out.
 *
 * Reversing a run of code units flips a pair from [high, low] to [low, high],
 * which is two lone surrogates — U+FFFD soup once it reaches the clipboard.
 * Reversing in code-point space instead is not an option (see above), so the
 * repair happens after the fact: a low surrogate immediately followed by a high
 * one can only have come from a flipped pair, since that order is invalid in
 * well-formed UTF-16.
 */
function repairSurrogates(arr: string[]): string[] {
  for (let i = 0; i < arr.length - 1; i++) {
    const a = arr[i].charCodeAt(0);
    const b = arr[i + 1].charCodeAt(0);
    if (a >= 0xdc00 && a <= 0xdfff && b >= 0xd800 && b <= 0xdbff) {
      arr[i] = String.fromCharCode(b);
      arr[i + 1] = String.fromCharCode(a);
      i++;
    }
  }
  return arr;
}

/**
 * Convert visual-order text to logical order, line by line.
 *
 * No-ops on text with no RTL characters, so it is safe to call on every copy.
 *
 * AMBIGUITY — the one case this cannot get right, and it is not fixable.
 * Two different logical strings can have the SAME visual form. Measured:
 *
 *     "שלום עולם 123 abc"  ->  "abc 123 םלוע םולש"
 *     "שלום עולם abc 123"  ->  "abc 123 םלוע םולש"
 *
 * In logical order the digits and the Latin word are two separate level-2 runs
 * split by a level-1 space; in visual order that space sits between two level-2
 * runs and joins them into one. The distinction is destroyed by the encoding,
 * so no inverse can recover it — we return the first reading. This only bites
 * when a number and a Latin word are adjacent at the edge of an RTL line.
 */
export function visualToLogical(text: string): string {
  if (!text || !RTL_RE.test(text)) return text;
  return unreorderTextSegment(text);
}

/** Line-by-line, keeping the separators so structure survives. */
function unreorderTextSegment(seg: string): string {
  if (!RTL_RE.test(seg)) return seg;
  return seg
    .split(LINE_SPLIT)
    .map((part) => (LINE_SPLIT.test(part) ? part : unreorderLine(part)))
    .join("");
}

/**
 * The same conversion, for a chunk of a live PTY STREAM rather than a clipboard
 * string — i.e. text with ANSI escapes in it.
 *
 * `visualToLogical` above is escape-BLIND on purpose: a clipboard selection
 * never contains escapes, so there is nothing to protect. Hand it a PTY chunk
 * and it happily permutes the bytes of a CSI sequence into garbage. This walks
 * the escapes the way `reorderRtlForDisplay` does — pushing each one through
 * untouched and converting only the text between them.
 *
 * KNOWN LIMIT, inherited and unchanged: a text run is the span between two
 * escapes, not always a whole visual line, so a heavily-coloured line is split
 * into fragments that each resolve their own base direction. bidi.ts documents
 * the same limit for the forward pass. A caller that feeds this arbitrary chunk
 * boundaries (a rAF flush, say) adds a second source of the same fragmentation.
 */
export function visualToLogicalStream(chunk: string): string {
  if (!chunk || !RTL_RE.test(chunk)) return chunk;
  const out: string[] = [];
  let last = 0;
  for (const m of chunk.matchAll(ANSI_RE)) {
    const idx = m.index!;
    if (idx > last) out.push(unreorderTextSegment(chunk.slice(last, idx)));
    out.push(m[0]);
    last = idx + m[0].length;
  }
  if (last < chunk.length) out.push(unreorderTextSegment(chunk.slice(last)));
  return out.join("");
}

/**
 * Remove bidi isolates (U+2066-U+2069) from text on its way to the clipboard.
 * The smart-bidi filter inserts them into the stream and xterm stores them in
 * the cell text, so a selection of `main.rs` would otherwise paste with an
 * invisible trailing PDI.
 */
export function stripIsolates(text: string): string {
  return text.replace(/[\u2066-\u2069]/g, "");
}
