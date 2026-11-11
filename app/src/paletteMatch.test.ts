// Unit tests for the command-palette matcher. Run:
//   cd app && node --experimental-strip-types --test src/paletteMatch.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { matchesQuery } from "./paletteMatch.ts";

// The session-history search (Orca idea #9) uses this matcher in the resume
// picker, and the daemon has a Go copy (term/search.go matchTokens). Both
// test suites read these cases, so the two cannot drift apart.
interface SharedCase {
  name: string;
  query: string;
  fields: string[];
  want: boolean;
}
const shared = JSON.parse(
  readFileSync(new URL("../src-tauri/server/internal/term/testdata/search_match.json", import.meta.url), "utf8"),
) as SharedCase[];

test("the cases shared with the daemon's matchTokens", () => {
  assert.ok(shared.length > 0);
  for (const c of shared) {
    assert.equal(matchesQuery(c.query, c.fields), c.want, c.name);
  }
});

const maximize = ["Pane: Toggle maximize", "Pane: Toggle maximize", "pane.maximize"];
const hebrewClose = ["חלונית: סגור", "Pane: Close", "pane.close"];

test("an empty or whitespace query matches everything", () => {
  assert.ok(matchesQuery("", maximize));
  assert.ok(matchesQuery("   \t", maximize));
});

test("word order does not matter", () => {
  assert.ok(matchesQuery("maximize pane", maximize));
});

test("a colon in the label does not block a query read aloud", () => {
  // The old substring match failed here: "pane: toggle" contains no "pane toggle".
  assert.ok(matchesQuery("pane toggle maximize", maximize));
});

test("match is case-insensitive", () => {
  assert.ok(matchesQuery("PANE MaXiMiZe", maximize));
});

test("every token must match", () => {
  assert.ok(!matchesQuery("pane close", maximize));
});

test("an English token finds a command in a Hebrew UI through the English label", () => {
  assert.ok(matchesQuery("close", hebrewClose));
});

test("the localized label still matches", () => {
  assert.ok(matchesQuery("סגור", hebrewClose));
});

test("tokens can match different fields", () => {
  assert.ok(matchesQuery("סגור pane", hebrewClose));
});

test("the command id matches", () => {
  assert.ok(matchesQuery("workspace.new", ["Create workspace", "Create workspace", "workspace.new"]));
});

test("an empty field never satisfies a token", () => {
  assert.ok(!matchesQuery("close", ["", ""]));
});
