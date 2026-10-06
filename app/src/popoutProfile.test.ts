// Unit tests for popoutProfile (popout RTL profile hand-off key + parse).
// Run: node --experimental-strip-types --test src/popoutProfile.test.ts   (node >= 22.6)
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";

const { popoutProfileKey, parsePopoutProfile } = await import("./popoutProfile.ts");

// Pins the key shape: App writes and PopoutTerminal reads it; drift = silent local fallback.
test("popoutProfileKey embeds the session id", () => {
  assert.equal(popoutProfileKey("abc"), "ymux.popout.profile.abc");
});

// Distinct sessions must not share a key, or two popouts would swap profiles.
test("popoutProfileKey differs per session", () => {
  assert.notEqual(popoutProfileKey("a"), popoutProfileKey("b"));
});

// Remote pane popped out must stay remote (the bug this module fixes).
test("parsePopoutProfile accepts exact remote", () => {
  assert.equal(parsePopoutProfile("remote"), "remote");
});

test("parsePopoutProfile accepts local", () => {
  assert.equal(parsePopoutProfile("local"), "local");
});

// Failure path: key absent (reload, older App) → pre-fix default, never a throw.
test("parsePopoutProfile(null) is local", () => {
  assert.equal(parsePopoutProfile(null), "local");
});

// Corrupt or case-variant values must not be promoted to remote.
test("parsePopoutProfile rejects unknown values", () => {
  for (const v of ["", "Remote", " remote", "remote ", "wsl", "undefined"]) {
    assert.equal(parsePopoutProfile(v), "local", v);
  }
});

// Round-trip: every valid kind survives write → read.
test("parsePopoutProfile round-trips both kinds", () => {
  for (const k of ["local", "remote"] as const) {
    assert.equal(parsePopoutProfile(k), k);
  }
});
