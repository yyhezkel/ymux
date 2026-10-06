// Unit tests for the browser's claude-args tokenizer. Run:
//   cd app && node --experimental-strip-types --test src/argv.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { splitArgs } from "./backend/web/argv.ts";

test("whitespace separates, runs collapse", () => {
  assert.deepEqual(splitArgs("  --model   opus  "), ["--model", "opus"]);
  assert.deepEqual(splitArgs(""), []);
});

test("quotes group and are dropped; empty quotes are an argument", () => {
  assert.deepEqual(splitArgs(`--append-system-prompt "be brief" ''`), ["--append-system-prompt", "be brief", ""]);
  assert.deepEqual(splitArgs(`a'b c'd`), ["ab cd"]);
});

test("nothing is evaluated", () => {
  // These reach claude literally: the daemon runs an argv, not a shell.
  assert.deepEqual(splitArgs("$(id) `x` ~ *; rm -rf /"), ["$(id)", "`x`", "~", "*;", "rm", "-rf", "/"]);
});
