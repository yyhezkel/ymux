// Unit tests for the browser's default settings merge. Run:
//   cd app && node --experimental-strip-types --test src/webDefaults.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { WEB_DEFAULT_SETTINGS, withDefaults } from "./backend/web/defaults.ts";

test("no document → the defaults", () => {
  assert.deepEqual(withDefaults(null), WEB_DEFAULT_SETTINGS);
  assert.deepEqual(withDefaults([1]), WEB_DEFAULT_SETTINGS);
});

test("a stored group merges over its default one level deep", () => {
  const s = withDefaults({ font: { terminal_size_pt: 16 } });
  assert.equal(s.font.terminal_size_pt, 16);
  assert.equal(s.font.ui_family, "system-ui"); // filled from the default
});

test("a scalar where the default is a group is ignored", () => {
  // The 2026-10-05 live-test bug: {"theme":"dark"} replaced the theme object
  // and applyTheme crashed reading text_primary off a string.
  const s = withDefaults({ theme: "dark", i18n: 7 });
  assert.equal(s.theme.text_primary, WEB_DEFAULT_SETTINGS.theme.text_primary);
  assert.equal(s.i18n.language, "en");
});

test("unknown and scalar fields pass through", () => {
  const s = withDefaults({ restore_sessions_on_start: false, future: { a: 1 } }) as unknown as Record<string, unknown>;
  assert.equal(s.restore_sessions_on_start, false);
  assert.deepEqual(s.future, { a: 1 });
});

test("the defaults are not shared between calls", () => {
  const a = withDefaults(null);
  a.font.terminal_size_pt = 99;
  assert.equal(withDefaults(null).font.terminal_size_pt, 13);
});
