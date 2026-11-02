// Phase 87 unit tests for shortcuts.ts.
// Run: node --experimental-strip-types --test src/shortcuts.test.ts   (node >= 22.6)
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
//
// shortcuts.ts deliberately has ZERO imports so this file can run under bare
// node: it owns DEFAULT_SHORTCUTS rather than importing it from settings.ts,
// which pulls in the Tauri bridge and the terminal.
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_SHORTCUTS,
  DEPRECATED_SHORTCUT_IDS,
  SHORTCUT_ACTION_IDS,
  SHORTCUT_GROUPS,
  buildShortcutTable,
  canonicalAccel,
  conflictingAccels,
  formatEvent,
  keyEq,
  matches,
  parseShortcut,
  physicalKey,
  type KeyLike,
} from "./shortcuts.ts";

/** A KeyboardEvent-shaped plain object. `code` defaults to "" so a test that
 *  only cares about the logical key can omit it. */
const ev = (o: Partial<KeyLike>): KeyLike => ({
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  key: "",
  code: "",
  ...o,
});

// ─── parseShortcut ─────────────────────────────────────────────────────────

test("parseShortcut: modifiers and plain keys", () => {
  assert.deepEqual(parseShortcut("Ctrl+Shift+C"), {
    ctrl: true, alt: false, shift: true, meta: false, key: "c",
  });
  assert.deepEqual(parseShortcut("Ctrl+,"), {
    ctrl: true, alt: false, shift: false, meta: false, key: ",",
  });
  assert.deepEqual(parseShortcut("Ctrl+Alt+="), {
    ctrl: true, alt: true, shift: false, meta: false, key: "=",
  });
});

test("parseShortcut: named keys the new bindings depend on", () => {
  assert.equal(parseShortcut("Ctrl+Tab")?.key, "Tab");
  assert.equal(parseShortcut("Ctrl+Enter")?.key, "Enter");
  assert.equal(parseShortcut("Ctrl+Alt+ArrowLeft")?.key, "ArrowLeft");
  assert.equal(parseShortcut("Ctrl+Alt+ArrowDown")?.key, "ArrowDown");
  assert.equal(parseShortcut("F11")?.key, "F11");
});

test("parseShortcut: case, whitespace and aliases", () => {
  assert.deepEqual(parseShortcut("ctrl+shift+c"), parseShortcut("Ctrl+Shift+C"));
  assert.deepEqual(parseShortcut("Ctrl + Shift + C"), parseShortcut("Ctrl+Shift+C"));
  assert.equal(parseShortcut("Cmd+K")?.meta, true);
  assert.equal(parseShortcut("Win+K")?.meta, true);
  assert.equal(parseShortcut("Opt+K")?.alt, true);
  assert.equal(parseShortcut("Ctrl+Esc")?.key, "Escape");
  assert.equal(parseShortcut("Ctrl+Space")?.key, " ");
});

test("parseShortcut: rejects empty and modifier-only", () => {
  // A modifier-only chord can never match, so it must not produce a binding
  // the dispatcher would test on every keypress.
  for (const bad of [null, undefined, "", "Ctrl", "Ctrl+Shift", "Alt+Meta"]) {
    assert.equal(parseShortcut(bad), null, JSON.stringify(bad));
  }
});

// ─── physicalKey / keyEq: the non-US-layout path ───────────────────────────

test("physicalKey maps event.code to a layout-independent token", () => {
  assert.equal(physicalKey(ev({ code: "KeyM" })), "m");
  assert.equal(physicalKey(ev({ code: "Digit5" })), "5");
  assert.equal(physicalKey(ev({ code: "Numpad7" })), "7");
  assert.equal(physicalKey(ev({ code: "Equal" })), "=");
  assert.equal(physicalKey(ev({ code: "Comma" })), ",");
  // Named keys are already layout-independent in event.key.
  for (const code of ["Tab", "Enter", "ArrowLeft", ""]) {
    assert.equal(physicalKey(ev({ code })), null, code);
  }
});

test("keyEq matches the physical key on a Hebrew layout", () => {
  // The physical M key reports event.key "צ" under a Hebrew layout.
  assert.ok(keyEq(ev({ key: "צ", code: "KeyM" }), "m"));
  assert.ok(keyEq(ev({ key: "פ", code: "KeyP" }), "p"));
  // ...and the ordinary US-layout path still works, Shift-uppercased or not.
  assert.ok(keyEq(ev({ key: "M", code: "KeyM" }), "m"));
  assert.ok(!keyEq(ev({ key: "n", code: "KeyN" }), "m"));
});

// ─── matches ───────────────────────────────────────────────────────────────

test("matches requires exact modifier equality", () => {
  const accel = parseShortcut("Ctrl+Shift+P");
  assert.ok(matches(ev({ ctrlKey: true, shiftKey: true, key: "p", code: "KeyP" }), accel));
  // Phase 87 tightening: the old hardcoded Ctrl+Shift+P branch had no
  // !altKey / !metaKey guard, so Ctrl+Alt+Shift+P opened the palette too.
  assert.ok(!matches(
    ev({ ctrlKey: true, altKey: true, shiftKey: true, key: "p", code: "KeyP" }),
    accel,
  ));
  assert.ok(!matches(ev({ ctrlKey: true, key: "p", code: "KeyP" }), accel));
});

test("matches: Ctrl+Alt+= fires via the physical key", () => {
  // This is why the old `e.code === "Equal"` special case in App.tsx is
  // redundant: on a Hebrew layout Ctrl+Alt is AltGr and event.key is not "=".
  const accel = parseShortcut("Ctrl+Alt+=");
  assert.ok(matches(ev({ ctrlKey: true, altKey: true, key: "±", code: "Equal" }), accel));
});

test("matches: Ctrl+Tab and Ctrl+Shift+Tab do not cross-match", () => {
  const next = parseShortcut("Ctrl+Tab");
  const prev = parseShortcut("Ctrl+Shift+Tab");
  const plain = ev({ ctrlKey: true, key: "Tab", code: "Tab" });
  const shifted = ev({ ctrlKey: true, shiftKey: true, key: "Tab", code: "Tab" });
  assert.ok(matches(plain, next) && !matches(plain, prev));
  assert.ok(matches(shifted, prev) && !matches(shifted, next));
});

test("matches: a null accelerator never fires", () => {
  assert.ok(!matches(ev({ ctrlKey: true, key: "c", code: "KeyC" }), null));
});

// ─── formatEvent (the click-to-record picker) ──────────────────────────────

test("formatEvent records the canonical accelerator on a Hebrew layout", () => {
  assert.equal(
    formatEvent(ev({ ctrlKey: true, shiftKey: true, key: "צ", code: "KeyM" })),
    "Ctrl+Shift+M",
  );
});

test("formatEvent handles the named keys the new bindings use", () => {
  assert.equal(formatEvent(ev({ ctrlKey: true, key: "Tab", code: "Tab" })), "Ctrl+Tab");
  assert.equal(formatEvent(ev({ ctrlKey: true, key: "Enter", code: "Enter" })), "Ctrl+Enter");
  assert.equal(
    formatEvent(ev({ ctrlKey: true, altKey: true, key: "ArrowLeft", code: "ArrowLeft" })),
    "Ctrl+Alt+ArrowLeft",
  );
  assert.equal(formatEvent(ev({ ctrlKey: true, key: " ", code: "Space" })), "Ctrl+Space");
});

test("formatEvent returns null for a bare modifier so the picker keeps listening", () => {
  for (const key of ["Control", "Shift", "Alt", "Meta"]) {
    assert.equal(formatEvent(ev({ key, ctrlKey: true })), null, key);
  }
});

// ─── round trip: what the picker records, the dispatcher must fire ─────────

test("round trip: every default accelerator re-parses to itself", () => {
  for (const id of SHORTCUT_ACTION_IDS) {
    const accel = DEFAULT_SHORTCUTS[id];
    assert.notEqual(parseShortcut(accel), null, `${id} = ${accel} did not parse`);
    assert.equal(canonicalAccel(accel), accel, `${id} is not in canonical form`);
  }
});

test("round trip: formatEvent output parses back and matches the same event", () => {
  const samples: KeyLike[] = [
    ev({ ctrlKey: true, shiftKey: true, key: "c", code: "KeyC" }),
    ev({ ctrlKey: true, altKey: true, key: "=", code: "Equal" }),
    ev({ ctrlKey: true, key: "Tab", code: "Tab" }),
    ev({ ctrlKey: true, altKey: true, key: "ArrowUp", code: "ArrowUp" }),
    ev({ metaKey: true, shiftKey: true, key: "k", code: "KeyK" }),
    ev({ ctrlKey: true, key: ",", code: "Comma" }),
  ];
  for (const e of samples) {
    const formatted = formatEvent(e);
    assert.notEqual(formatted, null);
    assert.ok(matches(e, parseShortcut(formatted)), `${formatted} did not match its own event`);
  }
});

// ─── buildShortcutTable ────────────────────────────────────────────────────

test("buildShortcutTable parses every action and backfills missing fields", () => {
  const full = buildShortcutTable(DEFAULT_SHORTCUTS);
  for (const id of SHORTCUT_ACTION_IDS) {
    assert.notEqual(full[id], null, `${id} missing from the table`);
  }
  // A pre-Phase-85 settings.json has only the original eight.
  const partial = buildShortcutTable({ copy: "Ctrl+Alt+C" } as typeof DEFAULT_SHORTCUTS);
  assert.equal(partial.copy?.alt, true);
  assert.deepEqual(partial.tab_next, parseShortcut(DEFAULT_SHORTCUTS.tab_next));
  // ...and null settings is the first-launch case.
  assert.deepEqual(buildShortcutTable(null), full);
});

test("buildShortcutTable yields null for an unparseable value, it does not throw", () => {
  const t = buildShortcutTable({ ...DEFAULT_SHORTCUTS, copy: "Ctrl" });
  assert.equal(t.copy, null);
});

// ─── conflict detection ────────────────────────────────────────────────────

test("canonicalAccel normalises spelling and ordering", () => {
  assert.equal(canonicalAccel("ctrl+shift+m"), "Ctrl+Shift+M");
  assert.equal(canonicalAccel("Shift+Ctrl+m"), "Ctrl+Shift+M");
  assert.equal(canonicalAccel("Ctrl+Space"), "Ctrl+Space");
  assert.equal(canonicalAccel("Ctrl"), null);
});

test("the shipped defaults contain no conflicts", () => {
  // The regression guard. Phase 65 bug V was exactly this: Focus/Zoom and STT
  // push-to-talk both defaulted to Ctrl+Shift+M, and the zoom binding silently
  // shadowed the voice one until someone noticed by hand.
  assert.deepEqual([...conflictingAccels(DEFAULT_SHORTCUTS)], []);
  assert.deepEqual(
    [...conflictingAccels(DEFAULT_SHORTCUTS, { push_to_talk: "Ctrl+Shift+M" })],
    [],
  );
});

test("conflictingAccels flags a duplicate, including across schemas", () => {
  const dup = conflictingAccels({ ...DEFAULT_SHORTCUTS, focus_zoom: DEFAULT_SHORTCUTS.copy });
  assert.deepEqual([...dup], ["Ctrl+Shift+C"]);
  // push_to_talk lives under settings.stt, not settings.shortcuts, so it is
  // passed in separately — but a clash with it still has to be reported.
  const cross = conflictingAccels(DEFAULT_SHORTCUTS, { push_to_talk: "Ctrl+Shift+Z" });
  assert.deepEqual([...cross], ["Ctrl+Shift+Z"]);
});

test("conflictingAccels ignores unparseable values rather than grouping them", () => {
  const t = { ...DEFAULT_SHORTCUTS, copy: "Ctrl", paste: "Shift" };
  assert.deepEqual([...conflictingAccels(t)], []);
});

// ─── the UI covers the schema ──────────────────────────────────────────────

test("SHORTCUT_GROUPS covers every action exactly once", () => {
  // Without this, a field can exist in the schema and never get a row — which
  // is how `find` and `select_all` became editable settings that dispatch
  // nothing at all (see FOLLOWUPS).
  const listed = SHORTCUT_GROUPS.flatMap((g) => g.ids);
  assert.equal(listed.length, new Set(listed).size, "an action is listed in two groups");
  assert.deepEqual([...listed].sort(), [...SHORTCUT_ACTION_IDS].sort());
});

test("SHORTCUT_ACTION_IDS excludes the one non-accelerator field", () => {
  assert.ok(!(SHORTCUT_ACTION_IDS as string[]).includes("copy_on_select_with_ctrl_c"));
  assert.equal(
    SHORTCUT_ACTION_IDS.length,
    Object.keys(DEFAULT_SHORTCUTS).length - 1 - DEPRECATED_SHORTCUT_IDS.length,
  );
});

test("deprecated ids get no row but keep their default", () => {
  // Pins the schema-compat half: `find` must stay in the defaults so old
  // settings.json round-trips, while never reaching the UI or the table.
  for (const id of DEPRECATED_SHORTCUT_IDS) {
    assert.ok(!(SHORTCUT_ACTION_IDS as string[]).includes(id), id);
    assert.ok(!SHORTCUT_GROUPS.some((g) => (g.ids as string[]).includes(id)), id);
    assert.equal(typeof DEFAULT_SHORTCUTS[id], "string", id);
  }
});

test("select_all rebind: the new chord matches, the old default does not", () => {
  const t = buildShortcutTable({ ...DEFAULT_SHORTCUTS, select_all: "Ctrl+Alt+A" });
  assert.ok(matches(ev({ ctrlKey: true, altKey: true, key: "a", code: "KeyA" }), t.select_all));
  assert.ok(!matches(ev({ ctrlKey: true, shiftKey: true, key: "A", code: "KeyA" }), t.select_all));
});

test("a live binding on the deprecated find chord is not a conflict", () => {
  // Breaking this means the hidden `find` default (Ctrl+F) blocks users from
  // binding Ctrl+F to a real action.
  assert.deepEqual([...conflictingAccels({ ...DEFAULT_SHORTCUTS, copy: "Ctrl+F" })], []);
});
