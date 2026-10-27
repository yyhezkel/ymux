// Unit tests for the isolate-aware V6 provider.
// Run: node --experimental-strip-types --test src/unicodeIsolates.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BMP_COMBINING,
  HIGH_COMBINING,
  ISOLATE_AWARE_UNICODE_VERSION,
  isBidiIsolate,
  isolateAwareV6,
} from "./unicodeIsolates.ts";

const props = (width: number, join: boolean) => (width << 1) | (join ? 1 : 0);

test("isBidiIsolate: exactly U+2066..2069", () => {
  // Range edges pin the override to the four isolates; widening it would change other glyphs.
  assert.equal(isBidiIsolate(0x2065), false);
  for (const cp of [0x2066, 0x2067, 0x2068, 0x2069]) assert.equal(isBidiIsolate(cp), true);
  assert.equal(isBidiIsolate(0x206a), false);
});

test("version key is the isolate-aware one", () => {
  // activeVersion lookup depends on this exact key.
  assert.equal(ISOLATE_AWARE_UNICODE_VERSION, "6-isolates");
  assert.equal(isolateAwareV6.version, "6-isolates");
});

test("wcwidth: isolates are 0, V6 spot checks unchanged", () => {
  // Isolate width 0 is the fix; neighbours must keep V6 widths.
  for (const cp of [0x2066, 0x2067, 0x2068, 0x2069]) assert.equal(isolateAwareV6.wcwidth(cp), 0);
  assert.equal(isolateAwareV6.wcwidth(0x41), 1);
  assert.equal(isolateAwareV6.wcwidth(0x05d0), 1);
  assert.equal(isolateAwareV6.wcwidth(0x4e00), 2);
  assert.equal(isolateAwareV6.wcwidth(0x1f), 0);
  assert.equal(isolateAwareV6.wcwidth(0x2065), 1);
  assert.equal(isolateAwareV6.wcwidth(0x2060), 0);
  assert.equal(isolateAwareV6.wcwidth(0x20000), 2);
  assert.equal(isolateAwareV6.wcwidth(0xe0001), 0);
  assert.equal(isolateAwareV6.wcwidth(0x1f600), 1);
});

test("charProperties: isolate joins preceding cell, keeps its width", () => {
  // Join even when preceding=0 (state reset after SGR) so the isolate takes no cell.
  assert.equal(isolateAwareV6.charProperties(0x2068, 0), props(0, true));
  assert.equal(isolateAwareV6.charProperties(0x2068, props(1, false)), props(1, true));
  assert.equal(isolateAwareV6.charProperties(0x2069, props(2, false)), props(2, true));
});

test("charProperties: ordinary chars follow V6", () => {
  // Non-isolates must be byte-identical to V6 behaviour.
  assert.equal(isolateAwareV6.charProperties(0x41, 0), props(1, false));
  assert.equal(isolateAwareV6.charProperties(0x4e00, props(1, false)), props(2, false));
  assert.equal(isolateAwareV6.charProperties(0x0300, props(1, false)), props(1, true));
  assert.equal(isolateAwareV6.charProperties(0x0300, props(2, false)), props(2, true));
});

test("charProperties: combining with preceding 0 does not join", () => {
  // Failure path: V6 only joins onto a real cell; the isolate override must not leak to combining marks.
  assert.equal(isolateAwareV6.charProperties(0x0300, 0), props(0, false));
  assert.equal(isolateAwareV6.charProperties(0x0300, props(0, true)), props(0, false));
});

test("drift: tables and non-isolate widths match xterm UnicodeV6.ts", () => {
  // Vendored copy must track the installed xterm; a bump that changes UnicodeV6.ts fails here.
  let src: string;
  try {
    src = readFileSync(new URL("../node_modules/@xterm/xterm/src/common/input/UnicodeV6.ts", import.meta.url), "utf8");
  } catch {
    return; // xterm source absent (no node_modules)
  }
  const parse = (name: string): number[][] => {
    const m = src.match(new RegExp(`const ${name} = (\\[[\\s\\S]*?\\n\\]);`));
    assert.ok(m, `${name} found in UnicodeV6.ts`);
    return JSON.parse(m[1].replace(/0x[0-9A-Fa-f]+/g, (h) => String(parseInt(h, 16))));
  };
  assert.deepEqual(BMP_COMBINING, parse("BMP_COMBINING"));
  assert.deepEqual(HIGH_COMBINING, parse("HIGH_COMBINING"));
});
