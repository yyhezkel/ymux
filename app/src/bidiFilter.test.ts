// The browser's smart-bidi filter (backend/web/bidiFilter.ts) — the Rust
// tests of app/src-tauri/src/bidi_filter.rs, carried over. Run:
//   cd app && node --experimental-strip-types --test src/bidiFilter.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { BidiFilter } from "./backend/web/bidiFilter.ts";

const FSI = "⁨";
const PDI = "⁩";
const on = () => new BidiFilter(true);

test("a disabled filter is a passthrough", () => {
  const s = "שלום DEV מהעולם";
  assert.equal(new BidiFilter(false).process(s), s);
});

test("ASCII alone and Hebrew alone are unchanged", () => {
  assert.equal(on().process("the quick brown fox"), "the quick brown fox");
  assert.equal(on().process("שלום עולם"), "שלום עולם");
});

test("Latin after Hebrew is isolated", () => {
  assert.ok(on().process("שלום DEV עולם").includes(`${FSI}DEV${PDI}`));
});

test("escapes pass through; the Latin inside them is wrapped, never the ESC", () => {
  const out = on().process("שלום \x1b[31mDEV\x1b[0m עולם");
  assert.ok(out.includes("\x1b[31m") && out.includes("\x1b[0m"));
  assert.ok(out.includes(`${FSI}DEV${PDI}`));
  assert.ok(!out.includes(`${FSI}\x1b`));
});

test("a box-drawing-dominated run is left alone", () => {
  const out = on().process("┏━━━━━━━━━━━━ DEV ━━━━━━━━━━━━┓ שלום");
  assert.ok(!out.includes(FSI) && !out.includes(PDI));
});

test("a CSI split across chunks is reassembled", () => {
  const f = on();
  const combined = f.process("שלום \x1b[3") + f.process("1mDEV\x1b[0m");
  assert.ok(combined.includes("\x1b[31m"));
  assert.ok(combined.includes(`${FSI}DEV${PDI}`));
});

test("OSC (BEL and ST), DCS and cursor moves pass through untouched", () => {
  for (const s of [
    "\x1b]9;Build finished\x07",
    "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
    "\x1bPq#0;2;100;100;100#1~~@@vv\x1b\\",
  ]) {
    assert.equal(on().process(s), s);
  }
  assert.ok(on().process("שלום \x1b[10;5HDEV\x1b[0m").includes("\x1b[10;5H"));
});

test("a newline resets the RTL context", () => {
  const out = on().process(`שלום\n${"x".repeat(50)} DEV`);
  assert.ok(!out.includes(`${FSI}DEV${PDI}`));
});

test("toggling resets transient state", () => {
  const f = on();
  f.process("שלום \x1b[3"); // half a CSI
  f.setEnabled(true);
  assert.equal(f.process("DEV"), "DEV"); // no stale escape, no RTL context
});
