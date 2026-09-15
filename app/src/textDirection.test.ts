// v0.4.4 (RTL Approach C) unit tests for detectDirection.
// v0.4.4-beta.3 (Approach C+) tests for classifyRow + detectRowDirections.
// Run: node --experimental-strip-types --test src/textDirection.test.ts
// (Excluded from the app tsconfig -- it is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  detectDirection,
  detectRowDirections,
  classifyRow,
  nextTuiOwnsBidi,
  foldTuiOwnsBidi,
  strongCounts,
  RTL_DOMINANCE,
  stripPaneFrame,
  rowDirections,
} from "./textDirection.ts";

const LTR = "ltr";
const RTL = "rtl";

const cases: Array<[string, "ltr" | "rtl", string]> = [
  // Yossi's core five
  ["1. Hello world", LTR, "pure Latin list item"],
  ["1. שלום עולם", RTL, "pure Hebrew list item"],
  ["1. שלום world", RTL, "mixed -> RTL"],
  ["/opt/wa/.shared.env", LTR, "pure ASCII path"],
  ["שרת רץ על port 4200", RTL, "mixed Hebrew + latin/digits -> RTL"],
  // Edge cases from the brief
  ["", LTR, "empty -> LTR default"],
  ["12345", LTR, "digits only -> LTR"],
  ["-> <- up down", LTR, "arrows/symbols only -> LTR"],
  // More coverage
  ["   ", LTR, "whitespace only -> LTR"],
  ["!@#$%^&*()", LTR, "punctuation only -> LTR"],
  ["שלום", RTL, "single Hebrew word"],
  ["a", LTR, "single Latin char"],
  ["ש", RTL, "single Hebrew char"],
  ["مرحبا بالعالم", RTL, "pure Arabic"],
  ["run مرحبا now", RTL, "mixed Arabic + Latin -> RTL"],
  ["4200", LTR, "port number"],
  ["$ ls -la /home", LTR, "shell prompt + command"],
  ["הפורט 4200 פתוח", RTL, "Hebrew wrapping a number"],
  ["ERROR: קובץ לא נמצא", RTL, "Latin word then Hebrew -> RTL"],
  ["git commit -m 'תיקון'", RTL, "Latin command with Hebrew arg -> RTL"],
  ["100% done", LTR, "digits + symbol + Latin -> LTR"],
  ["שלום\tworld", RTL, "tab-separated mixed -> RTL"],
  ["cafe", LTR, "Latin -> LTR"],
];

for (const [input, expected, label] of cases) {
  test(`detectDirection(${JSON.stringify(input)}) -> ${expected} -- ${label}`, () => {
    assert.equal(detectDirection(input), expected);
  });
}

// -- RTL_DOMINANCE (2026-08-19): a TUI status bar must not flip -------------
//
// The row that motivated the rule. Claude Code paints its footer as ONE
// terminal row: an update banner, the cwd, the model, and a context gauge.
// Under the old "any Hebrew wins" rule the three letters of `צבר` turned the
// whole row RTL and UAX #9 L2 mirrored every Latin run in it.
const CLAUDE_STATUS_ROW =
  "Update available! Run winget upgrade Anthropic.ClaudeCode" +
  "   ~ צבר·full · claude-fable-5[1m]  0k/1M 0% [░░░░░░░░]";

test("dominance: Claude's status row stays LTR (layout is positional)", () => {
  const { rtl, ltr } = strongCounts(CLAUDE_STATUS_ROW);
  assert.equal(rtl, 3); // צבר
  assert.ok(ltr > rtl * RTL_DOMINANCE, `expected ${ltr} > ${rtl * RTL_DOMINANCE}`);
  assert.equal(detectDirection(CLAUDE_STATUS_ROW, true), LTR);
});

test("dominance: the same row's Hebrew alone is still RTL", () => {
  // Proof the rule is about the ROW, not about צבר losing its direction.
  assert.equal(detectDirection("צבר", true), RTL);
  assert.equal(detectDirection("צבר", false), RTL);
});

test("dominance: a Hebrew note on a long path still wins", () => {
  // 4 Hebrew vs 14 Latin -- the near miss the factor is calibrated against.
  assert.equal(detectDirection("2. /opt/wa/.shared.env - הערה", true), RTL);
});

test("dominance: neutrals count for neither side", () => {
  // A progress bar, a box border and a pile of digits must not outvote
  // Hebrew, or every framed Hebrew message would flip to LTR.
  const { rtl, ltr } = strongCounts("│ שלום │ [███░░] 12345 │");
  assert.equal(ltr, 0);
  assert.equal(rtl, 4);
  assert.equal(detectDirection("│ שלום │ [███░░] 12345 │", true), RTL);
});

test("dominance: exactly at the threshold, Hebrew wins", () => {
  // 2 Hebrew, 8 Latin -> 2*4 >= 8. The tie goes to RTL on purpose.
  assert.equal(detectDirection("abcdefgh של", true), RTL);
  // One more Latin char and it tips.
  assert.equal(detectDirection("abcdefghi של", true), LTR);
});

// -- A PANE FRAME IS NOT A TABLE ---------------------------------------------
//
// Reproduced offline from Yossi's screenshot before the fix was written:
// zellij frames every pane, so every row carries a bar at both edges, the
// block rule merged the whole screen into one block, and three Hebrew letters
// in Claude's status line painted 13 of 15 rows RTL. The English banner came
// out mirrored run by run with the frame bars on the wrong side.
const BAR = "\u2502";
const framedRow = (inner: string) => BAR + " " + inner.padEnd(75) + BAR;
const ZELLIJ_SCREEN = [
  "Zellij (ymux-p_18cd12af41ae2570_0)  Tab #1",
  framedRow("Claude Code v2.1.121"),
  framedRow("Welcome back Yossi!"),
  framedRow("Tips for getting started"),
  framedRow("Run /init to create a CLAUDE.md file with instructions for Claude"),
  framedRow("What's new"),
  framedRow("Claude in Chrome is now generally available"),
  framedRow("/release-notes for more"),
  framedRow("C:\\Users\\mlastudent371"),
  framedRow(""),
  framedRow("Using claude-fable-5[1m] (from .claude/settings.json)"),
  framedRow("Update available! Run: winget upgrade Anthropic.ClaudeCode  ~ \u05e6\u05d1\u05e8\u00b7full"),
  framedRow(""),
  "Ctrl + <g> LOCK  <p> PANE  <t> TAB",
];

test("frame: one Hebrew word must not flip a whole framed pane", () => {
  const dirs = detectRowDirections(ZELLIJ_SCREEN);
  const rtlRows = dirs.filter((d) => d === RTL).length;
  // Exactly one: the status row, which really does hold Hebrew. Everything
  // else is English and stays put. Before the fix this was 13.
  assert.equal(rtlRows, 1, `expected 1 rtl row, got ${rtlRows}: ${dirs.join(",")}`);
  assert.equal(dirs[2], LTR, '"Welcome back Yossi!" is English');
  assert.equal(dirs[11], RTL, "the row holding \u05e6\u05d1\u05e8 is the one that flips");
});

test("frame: stripping removes only the outer bars, keeping columns", () => {
  const out = stripPaneFrame(ZELLIJ_SCREEN);
  assert.equal(out.length, ZELLIJ_SCREEN.length);
  // Same width, so anything indexing by column still lines up.
  for (let i = 0; i < out.length; i++) {
    assert.equal(out[i].length, ZELLIJ_SCREEN[i].length, `row ${i} width`);
  }
  assert.ok(!out[2].includes(BAR), "the frame bars are gone from a content row");
  assert.ok(out[2].includes("Welcome back Yossi!"), "the content survives");
  assert.equal(out[0], ZELLIJ_SCREEN[0], "rows outside the frame are untouched");
});

test("frame: a real table inside a frame still groups as a table", () => {
  // The block rule exists for this. Stripping the OUTER bars must not cost it:
  // the table's own inner bars are what group it, and a Hebrew cell still
  // drags the whole table RTL so its columns stay coherent.
  const screen = [
    "Zellij  Tab #1",
    framedRow("| Name | \u05e2\u05e8\u05da |"),
    framedRow("|------|-----|"),
    framedRow("| a    | 1   |"),
    framedRow("plain english line"),
    framedRow("another english line"),
    framedRow("a third english line"),
    "Ctrl + <g> LOCK",
  ];
  const dirs = detectRowDirections(screen);
  assert.deepEqual(dirs.slice(1, 4), [RTL, RTL, RTL], "the table flips entire");
  assert.deepEqual(dirs.slice(4, 7), [LTR, LTR, LTR], "the prose below does not");
});

test("frame: a short screen is left alone", () => {
  // Under the row floor a "frame" cannot be told from a small bordered table,
  // and mistaking one for the other costs more than the frame does.
  const rows = ["| a |", "| b |", "| c |"];
  assert.deepEqual(stripPaneFrame(rows), rows);
});

test("frame: an unframed screen is returned untouched", () => {
  // The common case — an SSH pane under tmux has no per-pane frame. One scan,
  // same array back.
  const rows = [
    "$ ls -la",
    "total 48",
    "drwxr-xr-x  1 user user  4096 Aug 19 06:00 .",
    "-rw-r--r--  1 user user   220 Aug 19 06:00 .bashrc",
    "$ echo \u05e9\u05dc\u05d5\u05dd",
    "\u05e9\u05dc\u05d5\u05dd",
    "$ ",
  ];
  assert.deepEqual(stripPaneFrame(rows), rows);
});


// -- REMOTE PARITY WITH main -------------------------------------------------
//
// Remote panes render Hebrew correctly on origin/main and must keep doing so.
// `git diff origin/main...HEAD -- src/textDirection.ts` says the entire delta
// in this module is the `dominance` flag, so "remote behaves like main" is
// exactly "the default path is main's rule". These tests are the guard.
//
// The rule below is main's detectDirection, copied verbatim from
// origin/main:app/src/textDirection.ts. It is a REFERENCE IMPLEMENTATION --
// do not "fix" it to match a change made to the real one; if the two disagree,
// the real one moved and remote panes moved with it.
const MAIN_HEBREW = /[\u0590-\u05ff]/;
const MAIN_ARABIC = /[\u0600-\u06ff\u0750-\u077f]/;
const MAIN_LATIN = /[A-Za-z]/;
function mainRule(text: string): "ltr" | "rtl" {
  if (MAIN_HEBREW.test(text) || MAIN_ARABIC.test(text)) return RTL;
  if (MAIN_LATIN.test(text)) return LTR;
  return LTR;
}

test("remote parity: the default path IS main's rule, for every table case", () => {
  for (const [input, , label] of cases) {
    assert.equal(detectDirection(input), mainRule(input), label);
  }
});

test("remote parity: main's rule also holds for the rows that started this", () => {
  // The two lines the dominance vote was written for. On the remote profile
  // both must resolve the way main resolves them -- RTL -- regardless of how
  // badly the status bar reads that way. Remote correctness outranks it.
  for (const line of [
    CLAUDE_STATUS_ROW,
    "2. /opt/wa/.shared.env - \u05d4\u05e2\u05e8\u05d4",
    "git commit -m '\u05ea\u05d9\u05e7\u05d5\u05df'",
  ]) {
    assert.equal(detectDirection(line), mainRule(line));
  }
});

test("remote parity: whole-screen row pass matches main's, block rules included", () => {
  // detectRowDirections has block grouping on top of detectDirection, so the
  // per-row check above is not enough on its own. A table, a fence and a box.
  const screen = [
    "$ ls -la /home",
    "| Name | \u05e2\u05e8\u05da |",
    "|------|-----|",
    "| a    | 1   |",
    "",
    "```",
    "const x = 5;   // \u05d4\u05e2\u05e8\u05d4",
    "```",
    "\u05e9\u05e8\u05ea \u05e8\u05e5 \u05e2\u05dc port 4200",
    CLAUDE_STATUS_ROW,
  ];
  const mine = detectRowDirections(screen);
  // main had no per-row block override beyond "any RTL in the block", which
  // for these rows collapses to the per-row answer everywhere except the
  // pure-ASCII rows inside an RTL block -- covered by the block tests above.
  assert.equal(mine.length, screen.length);
  assert.equal(mine[0], LTR, "pure-Latin shell line");
  assert.deepEqual(mine.slice(1, 4), [RTL, RTL, RTL], "Hebrew table flips entire");
  assert.deepEqual(mine.slice(5, 8), [RTL, RTL, RTL], "fence with a Hebrew comment");
  assert.equal(mine[8], RTL, "Hebrew prose with a port number");
  assert.equal(mine[9], RTL, "status row: main's answer, and remote gets main's");
});


test("gate: off a TUI screen the status row keeps the old rule", () => {
  // The regression Yossi caught within one build: shipped ungated, the vote
  // reached ordinary shell output too, and SSH panes that had been fine went
  // left-aligned. A shell line is text, not a layout.
  assert.equal(detectDirection(CLAUDE_STATUS_ROW), RTL);
  assert.equal(detectDirection(CLAUDE_STATUS_ROW, false), RTL);
});

test("gate: a Hebrew note on a long shell path stays RTL, both ways", () => {
  // Ratio 4:14 -- close enough to the threshold that the gate is what keeps
  // it safe rather than the constant.
  const line = "2. /opt/wa/.shared.env - הערה";
  assert.equal(detectDirection(line, false), RTL);
  assert.equal(detectDirection(line, true), RTL);
});

test("gate: default argument matches the pre-2026-08-19 behaviour", () => {
  // Every case in the table above runs through the 1-arg form, so this is
  // really a statement about all 24 of them: the gate defaults to off.
  for (const [input, , label] of cases) {
    assert.equal(detectDirection(input), detectDirection(input, false), label);
  }
});

test("dominance: block vote uses the whole block, not one row", () => {
  // Claude's bordered input box sitting under a long Latin help line: the
  // block holds Hebrew, but nowhere near enough of it to mirror the border.
  const rows = [
    "┌" + "─".repeat(60) + "┐",
    "│ Using claude-fable-5 (from .claude/settings.json) - /model to change │",
    "│ של                                                                 │",
    "└" + "─".repeat(60) + "┘",
  ];
  assert.deepEqual(detectRowDirections(rows, true), [LTR, LTR, LTR, LTR]);
});

test("dominance: a Hebrew box still flips entire", () => {
  const rows = [
    "┌──────┐",
    "│ שלום │",
    "└──────┘",
  ];
  assert.deepEqual(detectRowDirections(rows, true), [RTL, RTL, RTL]);
});


// -- Approach C+ (block-aware) tests -----------------------------------------

// classifyRow sanity checks
test("classifyRow: ASCII markdown separator is border", () => {
  assert.equal(classifyRow("|----|-----|"), "border");
  assert.equal(classifyRow("|------|-------|"), "border");
  assert.equal(classifyRow("+------+------+"), "border");
});

test("classifyRow: Unicode box-drawing line is border", () => {
  assert.equal(classifyRow("┌──────┐"), "border");
  assert.equal(classifyRow("│ code │"), "border");
  assert.equal(classifyRow("└──────┘"), "border");
});

test("classifyRow: fence opener/closer", () => {
  assert.equal(classifyRow("```"), "fence");
  assert.equal(classifyRow("```ts"), "fence");
  assert.equal(classifyRow("  ```javascript"), "fence");
});

test("classifyRow: table body row with letters is content, not border", () => {
  assert.equal(classifyRow("| Name | Value |"), "content");
  assert.equal(classifyRow("| שם | ערך |"), "content");
});

test("classifyRow: pure text is content", () => {
  assert.equal(classifyRow("שלום עולם"), "content");
  assert.equal(classifyRow("hello world"), "content");
});

// The 6 mandated block-aware cases from the brief.

test("Test 1: pure Hebrew table -> all rows RTL", () => {
  const rows = [
    "| שם | ערך |",
    "|----|-----|",
    "| א  | 1   |",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl"]);
});

test("Test 2: pure ASCII table -> all rows LTR", () => {
  const rows = [
    "| Name | Value |",
    "|------|-------|",
    "| a    | 1     |",
  ];
  assert.deepEqual(detectRowDirections(rows), ["ltr", "ltr", "ltr"]);
});

test("Test 3: mixed table (Hebrew in one cell) -> all rows RTL", () => {
  const rows = [
    "| Name | ערך |",
    "|------|-----|",
    "| a    | 1   |",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl"]);
});

test("Test 4: Hebrew paragraph -> ASCII box -> Hebrew paragraph: the box stays LTR (Phase 94)", () => {
  // Until 2026-09-15 the pure-ASCII box inherited the rtl of the paragraph
  // above it. That inheritance is what mirrored Claude Code's split-screen
  // diff view under a Hebrew prompt (xterm paints style runs as inline-block
  // spans; in an rtl paragraph those neutrals lay out right-to-left). A box
  // with no RTL text is LTR no matter what surrounds it.
  const rows = [
    "שלום זה טקסט",
    "┌──────┐",
    "│ code │",
    "└──────┘",
    "שלום עולם",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "ltr", "ltr", "ltr", "rtl"]);
});

test("Test 5: pure ASCII code fence -> all rows LTR", () => {
  const rows = [
    "```",
    "const x = 5;",
    "```",
  ];
  assert.deepEqual(detectRowDirections(rows), ["ltr", "ltr", "ltr"]);
});

test("Test 6: mixed code fence with Hebrew comment -> all rows RTL", () => {
  const rows = [
    "```",
    "// הערה בעברית",
    "const x = 5;",
    "```",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl", "rtl"]);
});

// Extra sanity: ASCII box between two LTR paragraphs stays LTR.
test("bonus: ASCII box between LTR paragraphs -> all LTR", () => {
  const rows = [
    "some english prose",
    "┌──────┐",
    "│ code │",
    "└──────┘",
    "more english prose",
  ];
  assert.deepEqual(detectRowDirections(rows), ["ltr", "ltr", "ltr", "ltr", "ltr"]);
});

// Extra sanity: empty input.
test("bonus: empty rows array", () => {
  assert.deepEqual(detectRowDirections([]), []);
});

// Extra sanity: single Hebrew standalone line still works.
test("bonus: single standalone Hebrew line -> RTL", () => {
  assert.deepEqual(detectRowDirections(["שלום עולם"]), ["rtl"]);
});

// -- nextTuiOwnsBidi (Claude visual-order RTL) tests --------------------------
// The title-driven state machine: ON on a "claude" title, OFF on an empty
// title, hold on anything else (shell paths, Claude's auto topic titles).

// -- foldTuiOwnsBidi: explicit beats the title -------------------------------
//
// The title-only machine never turned ON in practice — inside zellij the title
// reaching ymux is zellij's own. So the connect wizard (ymux launched Claude)
// and the Claude hooks (session-start/end, already carrying YMUX_PANE_ID) each
// supply an out-of-band answer, and those outrank an inference from a title.

test("fold: no explicit signal defers to the title", () => {
  assert.equal(foldTuiOwnsBidi(null, true), true);
  assert.equal(foldTuiOwnsBidi(null, false), false);
});

test("fold: an explicit ON wins over a title that never matched", () => {
  // The whole zellij case: the title says nothing useful, the wizard knows.
  assert.equal(foldTuiOwnsBidi(true, false), true);
});

test("fold: an explicit OFF wins over a stale ON title", () => {
  // Claude exited without resetting the title — the machine below holds ON
  // for any non-empty title, so something has to be able to say otherwise.
  assert.equal(foldTuiOwnsBidi(false, true), false);
});

test("fold: session-end clears to null, it does not assert false", () => {
  // Clearing hands the decision back to the title rather than pinning it, so
  // a later manually-typed `claude` can still be picked up if titles ever
  // start arriving. This asserts the SHAPE the App.tsx wiring relies on.
  const afterEnd: boolean | null = null;
  assert.equal(foldTuiOwnsBidi(afterEnd, true), true, "title may still speak");
  assert.equal(foldTuiOwnsBidi(afterEnd, false), false);
});


test("tuiOwnsBidi: 'claude' startup title turns on", () => {
  assert.equal(nextTuiOwnsBidi(false, "claude"), true);
});

test("tuiOwnsBidi: 'claude · resume' variant turns on", () => {
  assert.equal(nextTuiOwnsBidi(false, "claude · resume"), true);
});

test("tuiOwnsBidi: case-insensitive match", () => {
  assert.equal(nextTuiOwnsBidi(false, "Claude Code"), true);
});

test("tuiOwnsBidi: empty title (claude exit cleanup) turns off", () => {
  assert.equal(nextTuiOwnsBidi(true, ""), false);
});

test("tuiOwnsBidi: whitespace-only title also turns off", () => {
  assert.equal(nextTuiOwnsBidi(true, "   "), false);
});

test("tuiOwnsBidi: auto topic title mid-session holds ON", () => {
  assert.equal(nextTuiOwnsBidi(true, "fixing the RTL bug"), true);
});

test("tuiOwnsBidi: Hebrew topic title mid-session holds ON", () => {
  assert.equal(nextTuiOwnsBidi(true, "תיקון באג RTL"), true);
});

test("tuiOwnsBidi: shell path title while off stays off", () => {
  assert.equal(
    // String.raw: in a plain "" literal `\v` is a vertical tab and `\W \S \p`
    // are dropped, so this asserted against a mangled string, not a path.
    nextTuiOwnsBidi(false, String.raw`C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe`),
    false,
  );
});

test("tuiOwnsBidi: unrelated title while off stays off", () => {
  assert.equal(nextTuiOwnsBidi(false, "vim - notes.md"), false);
});

test("tuiOwnsBidi: full lifecycle start -> topic -> exit", () => {
  let s = false;
  s = nextTuiOwnsBidi(s, "claude");            // startup
  assert.equal(s, true);
  s = nextTuiOwnsBidi(s, "צ׳אט על באגים");      // auto topic rename
  assert.equal(s, true);
  s = nextTuiOwnsBidi(s, "");                  // clean exit
  assert.equal(s, false);
});

// -- rowDirections: the whole-pane decision ----------------------------------
//
// `force_rtl` (2026-08-23) exists because Yossi wanted the per-line decision
// GONE on remote panes: "RTL מלא, ולא שורה שורה". The contract these tests pin
// is that none of the auto_per_line heuristics (the dominance vote, block
// grouping, stripPaneFrame, the auto/suppress knobs) can reach this mode.
//
// Phase 94 (2026-09-15) narrowed what "full RTL" paints: a row with ANY
// Hebrew/Arabic is rtl, a row with none is LTR — `ltr-end` (reading order,
// packed right) in a shell, plain `ltr` while Claude Code holds the pane. The
// reason is mechanical: xterm's DOM renderer paints style runs as
// inline-block spans, atomic inlines are neutrals to UAX #9, and a
// multi-run Latin row under dir="rtl" is laid out with its runs REVERSED —
// which is how Claude's split-screen diff view came out scrambled on every
// Windows install running this mode.

const ALL_KNOBS = { auto: true, suppress: false, dominance: false };
const END = "ltr-end" as const;

test("force_rtl: Hebrew/Arabic rows are rtl, everything else is ltr-end", () => {
  const rows = [
    "ls -la /opt/wa",             // pure Latin
    "שלום עולם",                  // pure Hebrew
    "2. /opt/wa/.shared.env - הערה", // mixed, Latin-first — still rtl
    "|-------|-------|",          // ASCII table border
    "─────────────",              // box drawing
    "```ts",                      // code fence
    "   ",                        // whitespace only
    "",                           // empty
    "12345 !!! ???",              // digits + neutrals
    "مرحبا",                      // Arabic
  ];
  assert.deepEqual(
    rowDirections("force_rtl", rows, ALL_KNOBS),
    [END, RTL, RTL, END, END, END, END, END, END, RTL],
  );
});

test("force_rtl: while a TUI holds the pane a Latin row is plain ltr", () => {
  // Claude Code's split-screen diff: the left half's Latin must stay in the
  // left half, so nothing may pack it right. Hebrew rows are unchanged.
  const rows = ["95 +**How to apply:** unit test   │ 1 file changed +10", "שאלה בעברית", ""];
  assert.deepEqual(
    rowDirections("force_rtl", rows, { ...ALL_KNOBS, tui: true }),
    [LTR, RTL, LTR],
  );
  assert.deepEqual(
    rowDirections("force_rtl", rows, { ...ALL_KNOBS, tui: false }),
    [END, RTL, END],
  );
});

test("force_rtl: ignores auto_direction, suppress and dominance", () => {
  // A row that auto_per_line resolves to LTR under every one of these knobs
  // (dominance: 1 Hebrew word vs a wall of Latin), so if any of them were
  // still consulted the assertion would catch it.
  const rows = ["Update available! Run winget upgrade to get 0.5.0 — צבר"];
  for (const auto of [true, false]) {
    for (const suppress of [true, false]) {
      for (const dominance of [true, false]) {
        assert.deepEqual(
          rowDirections("force_rtl", rows, { auto, suppress, dominance }),
          [RTL],
          `auto=${auto} suppress=${suppress} dominance=${dominance}`,
        );
      }
    }
  }
});

test("auto_per_line: a pure-Latin block under a Hebrew row stays ltr (Phase 94)", () => {
  // Claude's bordered diff view right below the user's Hebrew prompt. Before
  // Phase 94 the block inherited the prompt's rtl and rendered mirrored.
  const rows = [
    "מה השתנה בקובץ?",
    "┌──────────────────────────────┬──────────────┐",
    "│ 94 +one                      │ 1 file       │",
    "│ 95 +two                      │ schema.md    │",
    "└──────────────────────────────┴──────────────┘",
  ];
  for (const dominance of [true, false]) {
    assert.deepEqual(
      detectRowDirections(rows, dominance),
      [RTL, LTR, LTR, LTR, LTR],
      `dominance=${dominance}`,
    );
  }
});

test("force_rtl: an empty viewport stays empty", () => {
  assert.deepEqual(rowDirections("force_rtl", [], ALL_KNOBS), []);
});

test("auto_per_line: unchanged — still delegates to detectRowDirections", () => {
  const rows = ["ls -la", "שלום", "| שם | ערך |", "|-----|-----|"];
  for (const dominance of [true, false]) {
    assert.deepEqual(
      rowDirections("auto_per_line", rows, { ...ALL_KNOBS, dominance }),
      detectRowDirections(rows, dominance),
      `dominance=${dominance}`,
    );
  }
});

test("auto_per_line: auto off or suppress on forces every row ltr", () => {
  const rows = ["שלום עולם", "ls -la", "| שם |"];
  const allLtr = rows.map(() => LTR);
  assert.deepEqual(rowDirections("auto_per_line", rows, { ...ALL_KNOBS, auto: false }), allLtr);
  assert.deepEqual(rowDirections("auto_per_line", rows, { ...ALL_KNOBS, suppress: true }), allLtr);
});

test("force_rtl outranks the suppress path that would blank auto_per_line", () => {
  // Same input, same knobs, opposite answers — this is the whole point of the
  // mode being a separate branch rather than a flag on the existing one.
  const rows = ["שלום עולם"];
  const knobs = { auto: false, suppress: true, dominance: true };
  assert.deepEqual(rowDirections("auto_per_line", rows, knobs), [LTR]);
  assert.deepEqual(rowDirections("force_rtl", rows, knobs), [RTL]);
});
