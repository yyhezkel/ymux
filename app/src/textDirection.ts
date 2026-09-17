// v0.4.4 (RTL Approach C): per-line text direction for the terminal.
//
// xterm.js's DOM renderer, given `dir="auto"` on a row, uses the Unicode
// "first strong directional character wins" rule. That mis-renders a mixed
// Hebrew+Latin line that HAPPENS to start with Latin -- e.g. a numbered list
// item "2. /opt/wa/.shared.env - הערה" got laid out LTR because the first
// strong char is Latin, even though the line is mostly Hebrew.
//
// Yossi's rule instead (per-line, Approach C):
//   - line contains ANY Hebrew/Arabic char  -> RTL   (mixed OR pure RTL)
//   - line is pure Latin                     -> LTR
//   - digits / symbols / whitespace only     -> LTR   (safe default)
//
// 2026-08-19 amendment: "ANY Hebrew" is now "Hebrew unless massively
// outnumbered" -- see RTL_DOMINANCE. A TUI status bar is a positional row,
// and flipping its paragraph direction mirrors layout the TUI already placed.
//
// v0.4.4-beta.3 (Approach C+): pure per-line detection breaks TABLES and BOXES.
// Claude Code drawing a Hebrew table produces:
//     | שם | ערך |     <- Hebrew content, per-line rule -> RTL
//     |----|-----|      <- pure ASCII border, per-line rule -> LTR
//     | א  | 1   |     <- Hebrew content, per-line rule -> RTL
// The middle row's LTR direction visually mirrors the border columns
// relative to the RTL data rows, so the table looks column-flipped.
//
// Fix: group consecutive rows into "blocks" (tables, code fences, boxed text)
// and give the WHOLE block a single direction. Any Hebrew/Arabic in a block
// -> RTL; pure-ASCII blocks inherit from the previous content row's direction
// (default LTR). See `detectRowDirections`.
//
// Embedded Latin runs inside an RTL line (paths, "port 4200") still get their
// natural LTR ordering from the browser's BiDi algorithm once the row's
// paragraph direction is RTL -- which is exactly what we want.
//
// This module is intentionally dependency-free and pure so it can be unit
// tested under `node --test` without a bundler or the DOM.

const HEBREW = /[֐-׿]/; // Hebrew block
const ARABIC = /[؀-ۿݐ-ݿ]/; // Arabic + Arabic Supplement
// Global twins of the above, for COUNTING rather than testing.
const HEBREW_G = /[֐-׿]/g;
const ARABIC_G = /[؀-ۿݐ-ݿ]/g;
const LATIN_G = /[A-Za-z]/g;

/**
 * 2026-08-19: how much Latin it takes to OUTVOTE the Hebrew on one row.
 *
 * The rule used to be "any Hebrew char anywhere -> the whole row is RTL",
 * which is right for prose and catastrophic for a TUI status bar. Claude
 * Code's footer is a single terminal row carrying ~60 Latin characters of
 * layout plus the cwd -- and when that cwd is `צבר`, three Hebrew letters
 * flipped the row's paragraph direction and UAX #9 rule L2 mirrored every
 * Latin run inside it. From Yossi's screenshot, "Update available! Run winget
 * upgrade ..." came out as "winget upgrade ... :Update available! Run" -- with
 * the Hebrew itself rendering perfectly. Such a row is POSITIONAL: the TUI
 * already chose the column of every glyph, and bidi has no way to reconstruct
 * a layout it just mirrored.
 *
 * So Hebrew still wins by default, and loses only when the row is
 * overwhelmingly Latin -- the signature of TUI chrome, not of a sentence.
 *
 * SCOPE, and this part was learned the hard way. Shipped unconditionally, the
 * rule reached ordinary shell output too: `ls` with one Hebrew filename, a
 * long path with a short Hebrew note. Yossi's report was immediate and
 * covered both panes -- "עכשוו זה מיושר לשמאל - וגם במרוחקים זה מיושר לשמאל" --
 * and the SSH side had been working. So the vote is gated: it applies only
 * while a full-screen TUI owns the pane (`nextTuiOwnsBidi` below, which is
 * how we already know Claude Code is in front). A shell line is text, and
 * text keeps the pre-2026-08-19 rule: any Hebrew takes the row.
 *
 * The discriminator was never the ratio -- it is whether something PLACED the
 * glyphs. The ratio is just how a placed row is recognised once we know we
 * are looking at one.
 *
 * This moves the PARAGRAPH direction only. An LTR row still has its RTL runs
 * reversed by the browser, so `צבר` reads correctly either way; what changes
 * is the alignment and which end the neutrals land on.
 *
 * 4 is calibrated, not derived. It has to keep `2. /opt/wa/.shared.env -
 * הערה` (4 Hebrew, 14 Latin) RTL while letting the status bar (3 Hebrew,
 * ~64 Latin) go LTR; anything from ~3.6 to ~20 separates those two, and 4
 * leaves the margin on the Hebrew side, which is the side users notice.
 */
export const RTL_DOMINANCE = 4;

/**
 * Count strong directional characters. Neutrals -- digits, punctuation,
 * box-drawing, whitespace -- deliberately count for neither side, so a
 * progress bar or a table border cannot outvote a Hebrew word.
 */
export function strongCounts(text: string): { rtl: number; ltr: number } {
  const heb = text.match(HEBREW_G)?.length ?? 0;
  const ara = text.match(ARABIC_G)?.length ?? 0;
  return { rtl: heb + ara, ltr: text.match(LATIN_G)?.length ?? 0 };
}

export function detectDirection(text: string, dominance = false): "ltr" | "rtl" {
  const { rtl, ltr } = strongCounts(text);
  if (rtl === 0) return "ltr"; // pure Latin / digits / symbols / empty
  // Off a TUI-painted screen, ANY Hebrew still takes the row -- see the
  // `dominance` note on detectRowDirections for why the two differ.
  if (!dominance) return "rtl";
  return rtl * RTL_DOMINANCE >= ltr ? "rtl" : "ltr";
}

// -- Approach C+ (block-aware direction) -------------------------------------

export type BlockRole = "content" | "border" | "fence";

// Unicode box-drawing glyphs used by TUIs and Claude Code for tables/frames.
// Two-or-more of these in a row => it's a border row.
const BOX_CHARS = /[─-╿]/g;
// ASCII "markdown/ASCII table" border characters. Only treated as a border
// row when the row has NO letters -- so "|-------|" is border but
// "|-- Value |" (which contains letters) stays content.
const ASCII_BORDER_CHARS = /[\-|+=]/g;
// Any letter (Latin / Hebrew / Arabic). If present, a row is not a pure
// ASCII-border separator.
const HAS_LETTER = /[A-Za-z֐-׿؀-ۿݐ-ݿ]/;
// A code-fence opener/closer, possibly with a language tag: ``` or ```ts
const FENCE_START = /^\s*```/;
// A content row that "belongs" to an adjacent table block -- it has a column
// separator (Unicode U+2502 or ASCII |).
const HAS_TABLE_BAR = /[│|]/;

/**
 * A pane frame is not a table. Strip it before anything classifies rows.
 *
 * 2026-08-19, reproduced offline before this was written: zellij draws a frame
 * around every pane, so EVERY row carries a vertical bar at both edges —
 * including Claude Code's status line, the row that holds the cwd. classifyRow
 * then calls every row a border, detectRowDirections merges the whole pane
 * into ONE block, and three Hebrew letters in the cwd paint the entire screen
 * RTL. 13 of 15 rows flipped in the repro, which is exactly what Yossi's
 * screenshot showed: English banner text mirrored run by run, the frame bars
 * landing on the wrong side.
 *
 * It is NOT a local-vs-remote problem and it is not new. An SSH pane under
 * tmux has no per-pane frame, which is why remote never showed it, and the
 * behaviour is the same on main — zellij going in front of local panes is only
 * what made it visible.
 *
 * The block rule exists so a Hebrew TABLE keeps its columns. A frame wraps the
 * whole viewport, which is how the two are told apart here: when most rows are
 * bar-delimited at BOTH edges, those outer bars are a frame. Removing just
 * them leaves any real table inside the frame still grouped by its inner bars.
 *
 * Returns the rows unchanged when no frame is detected, so the common case
 * costs one scan and nothing else.
 */
const FRAME_BAR = /[\u2502|]/;
export function stripPaneFrame(rows: string[]): string[] {
  const n = rows.length;
  // Below this a "frame" cannot be told from a short table with an outer
  // border, and getting it wrong there costs more than the frame does.
  if (n < 6) return rows;
  const framed: boolean[] = rows.map((r) => {
    const t = r.trim();
    return t.length >= 2 && FRAME_BAR.test(t[0]) && FRAME_BAR.test(t[t.length - 1]);
  });
  let count = 0;
  for (const f of framed) if (f) count++;
  // A frame surrounds the pane, so the only rows outside it are chrome the
  // terminal draws around the pane (zellij's tab bar and status bar).
  if (count < Math.max(5, Math.ceil(n * 0.7))) return rows;
  return rows.map((r, i) => {
    if (!framed[i]) return r;
    const lead = r.length - r.trimStart().length;
    const trail = r.length - r.trimEnd().length;
    const body = r.slice(lead, r.length - trail);
    // Keep the original padding: column positions still matter to classifyRow
    // (a border row is recognised by how many box glyphs it holds) and to
    // anything downstream that indexes by column.
    return (
      r.slice(0, lead) + " " + body.slice(1, -1) + " " + r.slice(r.length - trail)
    );
  });
}

/**
 * Classify a single row by shape (not by direction). See detectRowDirections
 * for how these roles are grouped into blocks.
 */
export function classifyRow(text: string): BlockRole {
  if (FENCE_START.test(text)) return "fence";
  const boxCount = (text.match(BOX_CHARS) || []).length;
  if (boxCount >= 2) return "border";
  // ASCII "markdown/table" separator row: no letters, plenty of |/-/+/=.
  if (!HAS_LETTER.test(text)) {
    const asciiBorderCount = (text.match(ASCII_BORDER_CHARS) || []).length;
    if (asciiBorderCount >= 3) return "border";
  }
  return "content";
}

/**
 * Approach C+: compute a direction PER ROW using block-aware grouping.
 *
 * Blocks:
 *   - Fence block: from a ```-opener row to the next ```-closer row (inclusive).
 *   - Table/box block: a run of "border" rows, extended on either side to
 *     include adjacent "content" rows that carry a column separator.
 *   - Standalone rows: everything else, resolved per-row via detectDirection.
 *
 * `dominance` is passed straight through to detectDirection: true only while
 * a full-screen TUI owns the pane, where rows are positional. See
 * RTL_DOMINANCE.
 *
 * Direction for a block:
 *   - Block holds Hebrew/Arabic  -> detectDirection over the block's joined
 *     text, so the RTL_DOMINANCE rule sees the whole block at once
 *   - No RTL at all              -> inherit from the nearest resolved row
 *                                   BEFORE the block
 *   - Otherwise LTR (safe default)
 *
 * The block heuristic keeps tables coherent: a table with a single Hebrew cell
 * renders RTL end-to-end (columns preserved from the user's perspective); a
 * pure-ASCII code fence stays LTR even if it happens to sit between Hebrew
 * paragraphs; a pure-ASCII BOX between Hebrew paragraphs flips RTL to match.
 */
export function detectRowDirections(
  rows: string[],
  dominance = false,
): ("ltr" | "rtl")[] {
  const n = rows.length;
  if (n === 0) return [];
  const result: (("ltr" | "rtl") | null)[] = new Array(n).fill(null);

  // A pane frame would otherwise make every row a "border" and merge the whole
  // screen into one block — see stripPaneFrame. Everything below reads `rows`,
  // which from here on is the framed content without its outer bars.
  rows = stripPaneFrame(rows);

  const roles: BlockRole[] = rows.map(classifyRow);
  const hasRtl: boolean[] = rows.map((t) => HEBREW.test(t) || ARABIC.test(t));

  // 1. Identify block ranges.
  const blocks: Array<{ start: number; end: number }> = [];
  const inBlock = new Array<boolean>(n).fill(false);
  let i = 0;
  while (i < n) {
    if (inBlock[i]) { i++; continue; }
    if (roles[i] === "fence") {
      // Fence: opener .. next fence row (inclusive), or clamp to EOF.
      let j = i + 1;
      while (j < n && roles[j] !== "fence") j++;
      const end = j < n ? j : n - 1; // unterminated fence: run to end
      blocks.push({ start: i, end });
      for (let k = i; k <= end; k++) inBlock[k] = true;
      i = end + 1;
      continue;
    }
    if (roles[i] === "border") {
      // Table/box: extend backward through table-like content, and forward
      // through consecutive border rows or table-like content rows.
      let start = i;
      while (
        start > 0 &&
        !inBlock[start - 1] &&
        roles[start - 1] === "content" &&
        HAS_TABLE_BAR.test(rows[start - 1])
      ) {
        start--;
      }
      let end = i;
      while (end + 1 < n) {
        const nextRole = roles[end + 1];
        if (nextRole === "border") { end++; continue; }
        if (nextRole === "content" && HAS_TABLE_BAR.test(rows[end + 1])) {
          end++;
          continue;
        }
        break;
      }
      blocks.push({ start, end });
      for (let k = start; k <= end; k++) inBlock[k] = true;
      i = end + 1;
      continue;
    }
    i++;
  }

  // 2. Blocks that contain ANY Hebrew/Arabic -> whole block RTL. Blocks with
  //    zero RTL content are "unresolved" until step 4 (they need to see the
  //    surrounding standalone rows first).
  const unresolved: Array<{ start: number; end: number }> = [];
  for (const b of blocks) {
    let anyRtl = false;
    for (let k = b.start; k <= b.end; k++) {
      if (hasRtl[k]) { anyRtl = true; break; }
    }
    if (!anyRtl) {
      unresolved.push(b);
      continue;
    }
    // 2026-08-19: the dominance rule (see RTL_DOMINANCE) is applied to the
    // block AS A WHOLE, not per row -- so one Hebrew word inside a wall of
    // Latin (Claude's bordered input box under its status bar) no longer
    // mirrors the box's columns, while a Hebrew table still flips entire.
    const dir = detectDirection(rows.slice(b.start, b.end + 1).join(" "), dominance);
    for (let k = b.start; k <= b.end; k++) result[k] = dir;
  }

  // 3. Standalone (non-block) rows: classic per-row detection.
  for (let k = 0; k < n; k++) {
    if (!inBlock[k] && result[k] === null) {
      result[k] = detectDirection(rows[k], dominance);
    }
  }

  // 4. Blocks with zero Hebrew/Arabic are LTR, full stop.
  //
  //    Phase 94 (2026-09-15): this used to inherit the direction of the
  //    nearest resolved row ABOVE the block, so Claude Code's split-screen
  //    diff view — a bordered, pure-Latin block sitting under the user's
  //    Hebrew prompt — came out dir="rtl" without a single RTL character in
  //    it. xterm's DOM renderer paints every style run as an inline-block
  //    span, an atomic inline is a neutral to UAX #9, and a row of neutrals
  //    in an RTL paragraph is laid out right-to-left: the diff's columns,
  //    gutters and line numbers landed in mirrored order. A block that holds
  //    no RTL text has nothing to gain from an RTL paragraph and everything
  //    to lose, so it no longer asks its neighbours.
  for (const b of unresolved) {
    for (let k = b.start; k <= b.end; k++) result[k] = "ltr";
  }

  return result as ("ltr" | "rtl")[];
}

// -- The whole-pane direction decision (all DOM-renderer modes) ---------------

/**
 * Modes that render on xterm's DOM renderer and therefore carry a real `dir`
 * attribute per row. WebGL paints to a canvas and has no per-cell DOM for the
 * browser's bidi engine to hook into, so `bidi_reorder` / `off` are excluded.
 *
 * Kept here rather than in terminalInstance.ts so the row-direction decision
 * and the renderer gate that enables it read from one place. Six separate
 * `=== "auto_per_line"` gates is what let a pane land in a combination that
 * was none of the modes — see the note on `applyRenderer`.
 */
export type RowDirMode = "auto_per_line" | "force_rtl";

/**
 * What a row-dir pass wants painted on one row. `ltr` / `rtl` become the
 * row's `dir` attribute. `ltr-end` (Phase 94) is `dir="ltr"` PLUS
 * `text-align: right`: the run order is left-to-right, the whole run sits
 * against the right edge — "לטינית נשארת בימין, בלי היפוך".
 */
export type RowDir = "ltr" | "rtl" | "ltr-end";

/**
 * Decide the `dir` of every visible row.
 *
 * Extracted from `TerminalInstance.applyRowDirections` on 2026-08-23 so the
 * decision is testable: the method itself is bound to the DOM and never ran
 * under `node --test`, and in these modules the tests are the specification
 * (see `docs/vault/INDEX.md`).
 *
 * `force_rtl` is deliberately the FIRST branch and reads none of the
 * auto_per_line knobs (`auto`, `suppress`, `dominance`). The mode exists
 * because Yossi wanted the per-line decision gone on remote panes — "RTL
 * מלא, ולא שורה שורה" — so the dominance vote, the block-aware table
 * grouping, `stripPaneFrame` and the `auto_direction` escape hatch are all
 * bypassed rather than tuned. textDirection.test.ts pins that.
 *
 * Phase 94 (2026-09-15) narrowed WHAT "full RTL" paints, after Claude Code's
 * split-screen diff view came out scrambled on every Windows install running
 * this mode. A row with Hebrew/Arabic is `rtl`, exactly as before. A row
 * with NO RTL text gets nothing from an RTL paragraph — xterm's DOM renderer
 * paints each style run as an inline-block, a neutral to UAX #9, so under
 * `dir="rtl"` a multi-run Latin row is laid out with its runs in REVERSE
 * order (the diff's columns, gutters and line numbers mirrored). So:
 *
 *   - `o.tui` (Claude Code holds the pane — the OSC-title / hook signal that
 *     `foldTuiOwnsBidi` already folds): a Latin row is plain `ltr`, painted
 *     exactly where the TUI put it, so a two-column layout keeps both halves.
 *   - otherwise (a shell): `ltr-end` — Latin in reading order, packed against
 *     the right edge, which is what the unconditional-RTL shell looked like
 *     for a single-run row and never for a coloured one.
 *
 * `o.tui` is a DETECTED state and is deliberately not the profile's
 * `tui_owns_bidi` switch — that switch means "the TUI emits visual order",
 * a different question, and it is off for remote panes on purpose.
 */
export function rowDirections(
  mode: RowDirMode,
  texts: string[],
  o: { auto: boolean; suppress: boolean; dominance: boolean; tui?: boolean },
): RowDir[] {
  if (mode === "force_rtl") {
    return texts.map((t) =>
      HEBREW.test(t) || ARABIC.test(t) ? "rtl" : o.tui ? "ltr" : "ltr-end",
    );
  }
  // `suppress` = a self-bidi TUI already emitted visual order; `auto` off is
  // the classic-terminal escape hatch. Both mean "no bidi from us".
  if (!o.auto || o.suppress) return texts.map(() => "ltr");
  return detectRowDirections(texts, o.dominance);
}

// -- TUI-owned BiDi (Claude Code visual-order output) -------------------------
//
// Claude Code (>= 2.1.74, verified live on 2.1.210) writes RTL text to the
// PTY pre-reordered into VISUAL order — it assumes a terminal with no bidi
// support (Windows Terminal). Rendering such rows with dir="rtl" applies the
// browser's bidi algorithm ON TOP of Claude's reorder: Hebrew letters
// double-reverse back to looking correct, but neutrals (?, ., brackets)
// resolve against the RTL paragraph and land at the wrong end — a trailing
// "?" typed into Claude's input box shows up at the line START. There is no
// claude-side opt-out (WT_SESSION / TERM_PROGRAM=WezTerm/mintty all still
// reorder; upstream closed the Apple-Terminal double-reorder as not-planned).
//
// Fix: while a self-bidi TUI holds a pane's foreground, render every row LTR
// (exactly what Windows Terminal shows). Detection is terminal-title based:
//   - Claude sets the title to "claude" on startup (process.title="claude")
//     and variants like "claude · resume"; mid-session auto topic titles /
//     "/rename" can be ANY text, so unrelated titles must NOT clear the state.
//   - On clean exit Claude resets the title to "" (process.title="") — that
//     is the off signal. Shells set path-like titles, never empty ones.
// The machine is deliberately conservative: ON only on a title containing
// "claude", OFF only on an empty title, hold otherwise.
/**
 * Fold the two sources of "a self-reordering TUI is in front of this pane".
 *
 * 2026-08-19. The title-only machine below turned out never to turn ON in
 * practice: inside zellij the title reaching ymux is zellij's own, and Claude's
 * never matched. Yossi's debug.log has `tui=0` on every direction pass, on
 * every pane, both profiles.
 *
 * So a second source was added that does not travel in-band at all:
 *   - the connect wizard, when ymux itself launches Claude (mode="claude"), and
 *   - the Claude hooks, which already carry YMUX_PANE_ID and fire
 *     session-start / session-end (cli/src/main.rs, rpc_server.rs).
 * Both survive any multiplexer, and the hooks work over SSH too.
 *
 * Precedence is explicit-wins: an out-of-band signal KNOWS, while the title is
 * an inference. `null` means "nobody told us", and only then does the title
 * decide. session-end clears back to null rather than asserting false, so a
 * later manually-typed `claude` can still be picked up by the title if the
 * title ever starts arriving.
 */
export function foldTuiOwnsBidi(
  explicit: boolean | null,
  fromTitle: boolean,
): boolean {
  return explicit ?? fromTitle;
}

export function nextTuiOwnsBidi(prev: boolean, title: string): boolean {
  if (/claude/i.test(title)) return true;
  if (title.trim() === "") return false;
  return prev;
}
