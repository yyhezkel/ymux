// The browser's theme presets (backend/web/presets.gen.ts) are generated
// from app/src-tauri/src/settings.rs list_presets(); this pins that they
// still match it — ids, labels, and every quoted colour. Run:
//   cd app && node --experimental-strip-types --test src/webPresets.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { PRESETS } from "./backend/web/presets.gen.ts";

const rust = readFileSync(new URL("../src-tauri/src/settings.rs", import.meta.url), "utf8");
const table = rust.slice(rust.indexOf("pub(crate) fn list_presets()"));

test("same preset ids and labels, in the same order", () => {
  const ids = [...table.matchAll(/id: "([^"]+)"\.into\(\),\s*label: "([^"]+)"/g)].map((m) => [m[1], m[2]]);
  assert.deepEqual(
    PRESETS.map((p) => [p.id, p.label]),
    ids.slice(0, PRESETS.length),
  );
  assert.equal(PRESETS.length, 13);
});

test("every theme colour is one the Rust source names", () => {
  for (const p of PRESETS) {
    const { ansi, ...rest } = p.theme;
    for (const v of [...Object.values(rest), ...Object.values(ansi)]) {
      assert.ok(rust.includes(`"${v}".into()`), `${p.id}: ${v} not in settings.rs`);
    }
  }
});
