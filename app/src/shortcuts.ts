// Phase 16: shortcut parsing + matching helper.
//
// User-typed shortcut strings live in `settings.shortcuts.<name>` as
// human-readable accelerators like "Ctrl+Shift+C" or "Ctrl+,". Same
// vocabulary in the JSON file (hand-editable) and the UI's "click to
// record" picker. The helper parses each accelerator once on settings
// load and exposes `matches(event, accelerator)` so dispatchers stay
// readable.
//
// Vocabulary:
//   - Modifiers: `Ctrl`, `Alt`, `Shift`, `Meta` (Windows / Cmd key)
//   - Keys: single characters (case-insensitive), digits, punctuation,
//     special names (`Enter`, `Escape`, `Tab`, `Space`, `F1`..`F12`,
//     `ArrowUp`/`Down`/`Left`/`Right`, `Backspace`, `Delete`, `Home`,
//     `End`, `PageUp`, `PageDown`, `Insert`)
//   - Joined by `+` with optional whitespace.

export interface ShortcutsSettings {
  copy: string;
  paste: string;
  select_all: string;
  find: string;
  new_workspace: string;
  toggle_notes: string;
  toggle_settings: string;
  summarize_claude: string;
  // Phase 87: everything below was hardcoded in App.tsx's keydown handler
  // until now. Same accelerator vocabulary, same click-to-record picker.
  command_palette: string;
  toggle_sidebar: string;
  toggle_sidebar_soft: string;
  toggle_maximize: string;
  focus_zoom: string;
  reset_terminal: string;
  distribute_evenly: string;
  split_horizontal: string;
  split_vertical: string;
  close_pane: string;
  split_or_move_left: string;
  split_or_move_right: string;
  split_or_move_up: string;
  split_or_move_down: string;
  quadrant_top_left: string;
  quadrant_top_right: string;
  quadrant_bottom_left: string;
  quadrant_bottom_right: string;
  tab_next: string;
  tab_prev: string;
  // BRIEF: the cross-workspace agent Queue panel.
  toggle_queue: string;
  // BRIEF: show the Briefing card for the active workspace.
  show_briefing: string;
  copy_on_select_with_ctrl_c: boolean;
}

export const DEFAULT_SHORTCUTS: ShortcutsSettings = {
  copy: "Ctrl+Shift+C",
  paste: "Ctrl+Shift+V",
  select_all: "Ctrl+Shift+A",
  find: "Ctrl+F",
  new_workspace: "Ctrl+N",
  toggle_notes: "Ctrl+Shift+N",
  toggle_settings: "Ctrl+,",
  summarize_claude: "Ctrl+Alt+B",
  command_palette: "Ctrl+Shift+P",
  toggle_sidebar: "Ctrl+Shift+B",
  toggle_sidebar_soft: "Ctrl+B",
  toggle_maximize: "Ctrl+Enter",
  focus_zoom: "Ctrl+Shift+Z",
  reset_terminal: "Ctrl+Alt+R",
  distribute_evenly: "Ctrl+Alt+=",
  split_horizontal: "Ctrl+Shift+D",
  split_vertical: "Ctrl+Shift+E",
  close_pane: "Ctrl+Shift+W",
  split_or_move_left: "Ctrl+Alt+ArrowLeft",
  split_or_move_right: "Ctrl+Alt+ArrowRight",
  split_or_move_up: "Ctrl+Alt+ArrowUp",
  split_or_move_down: "Ctrl+Alt+ArrowDown",
  quadrant_top_left: "Ctrl+Alt+I",
  quadrant_top_right: "Ctrl+Alt+O",
  quadrant_bottom_left: "Ctrl+Alt+K",
  quadrant_bottom_right: "Ctrl+Alt+L",
  tab_next: "Ctrl+Tab",
  tab_prev: "Ctrl+Shift+Tab",
  toggle_queue: "Ctrl+Shift+Q",
  show_briefing: "Ctrl+Alt+Q",
  copy_on_select_with_ctrl_c: true,
};

/** Every configurable accelerator, i.e. every ShortcutsSettings field
 *  except the one boolean. */
export type ShortcutActionId = Exclude<keyof ShortcutsSettings, "copy_on_select_with_ctrl_c">;

/** Every accelerator field, in display order — i.e. DEFAULT_SHORTCUTS minus
 *  the one boolean. Used by the parser, the conflict check and the UI so a
 *  new binding only has to be added to the interface above. */
export const SHORTCUT_ACTION_IDS = (Object.keys(DEFAULT_SHORTCUTS) as (keyof ShortcutsSettings)[])
  .filter((k) => typeof DEFAULT_SHORTCUTS[k] === "string") as ShortcutActionId[];

/** How the Settings tab groups the 28 rows. Labels come from
 *  `settings.shortcuts.group.<key>`; each row's own label is
 *  `settings.shortcuts.<id>`, derived mechanically so a new binding needs
 *  no pair list. A unit test asserts this covers SHORTCUT_ACTION_IDS
 *  exactly — that is the guard against a field existing in the schema but
 *  never appearing in the UI, which is how `find` and `select_all` came to
 *  be editable rows that dispatch nothing. */
export const SHORTCUT_GROUPS: { key: string; ids: ShortcutActionId[] }[] = [
  {
    key: "general",
    ids: [
      "new_workspace",
      "toggle_settings",
      "toggle_notes",
      "command_palette",
      "toggle_sidebar",
      "toggle_sidebar_soft",
      "summarize_claude",
      "toggle_queue",
      "show_briefing",
    ],
  },
  { key: "clipboard", ids: ["copy", "paste", "select_all", "find"] },
  {
    key: "panes",
    ids: [
      "split_horizontal",
      "split_vertical",
      "close_pane",
      "toggle_maximize",
      "focus_zoom",
      "reset_terminal",
    ],
  },
  {
    key: "layout",
    ids: [
      "split_or_move_left",
      "split_or_move_right",
      "split_or_move_up",
      "split_or_move_down",
      "quadrant_top_left",
      "quadrant_top_right",
      "quadrant_bottom_left",
      "quadrant_bottom_right",
      "distribute_evenly",
    ],
  },
  { key: "tabs", ids: ["tab_next", "tab_prev"] },
];

export interface ParsedShortcut {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  /** The non-modifier key, in the normalized form `event.key` would
   *  produce (lower-case for letters, literal for punctuation, name
   *  for special keys). Empty string means "modifier-only" (invalid). */
  key: string;
}

/** Normalize a single token to the form we compare against `event.key`. */
function normalizeKey(token: string): string {
  const t = token.trim();
  if (t.length === 0) return "";
  // Single letters / digits — lowercase the letter form so we can
  // compare with `event.key.toLowerCase()` (event.key is uppercase
  // when Shift is held; we already track Shift separately).
  if (t.length === 1) return t.toLowerCase();
  // Named keys — preserve the canonical browser KeyboardEvent.key
  // capitalisation so the comparison hits.
  const lc = t.toLowerCase();
  const named: Record<string, string> = {
    enter: "Enter",
    escape: "Escape",
    esc: "Escape",
    tab: "Tab",
    space: " ",
    spacebar: " ",
    backspace: "Backspace",
    delete: "Delete",
    del: "Delete",
    home: "Home",
    end: "End",
    pageup: "PageUp",
    pagedown: "PageDown",
    insert: "Insert",
    up: "ArrowUp",
    down: "ArrowDown",
    left: "ArrowLeft",
    right: "ArrowRight",
    arrowup: "ArrowUp",
    arrowdown: "ArrowDown",
    arrowleft: "ArrowLeft",
    arrowright: "ArrowRight",
  };
  if (lc in named) return named[lc];
  // F1..F12
  const fmatch = lc.match(/^f(\d{1,2})$/);
  if (fmatch) return `F${fmatch[1]}`;
  // Anything else: punctuation like "," "/" ";". Keep as-is.
  return t;
}

export function parseShortcut(s: string | undefined | null): ParsedShortcut | null {
  if (!s) return null;
  const parts = s.split("+").map((p) => p.trim()).filter(Boolean);
  if (parts.length === 0) return null;
  let ctrl = false,
    alt = false,
    shift = false,
    meta = false;
  let key = "";
  for (const raw of parts) {
    const low = raw.toLowerCase();
    if (low === "ctrl" || low === "control") ctrl = true;
    else if (low === "alt" || low === "option" || low === "opt") alt = true;
    else if (low === "shift") shift = true;
    else if (low === "meta" || low === "cmd" || low === "command" || low === "win") meta = true;
    else key = normalizeKey(raw);
  }
  if (!key) return null;
  return { ctrl, alt, shift, meta, key };
}

// Phase 62.B (item G): the layout-INDEPENDENT key for an event, derived
// from `event.code` (the physical key). Returns a token comparable to
// normalizeKey() output — lowercase letter, digit, or punctuation char —
// or null for keys we don't map physically (named keys like Enter /
// ArrowUp, which are already layout-independent in `event.key`, so
// callers fall back to that). This is what makes letter / digit / punct
// shortcuts (copy, the STT push-to-talk hotkey, …) fire on non-US
// layouts, where `event.key` is the localized character — e.g. Hebrew
// "צ" for the physical M key, which previously never matched "m".
/** The slice of KeyboardEvent this module actually reads. A real
 *  KeyboardEvent satisfies it structurally, so every call site is
 *  unchanged — but a unit test can hand these functions a plain object
 *  instead of standing up a DOM. */
export interface KeyLike {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  key: string;
  code: string;
}

const CODE_PUNCT: Record<string, string> = {
  Equal: "=",
  Minus: "-",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
};
export function physicalKey(e: KeyLike): string | null {
  const code = e.code;
  if (!code) return null;
  if (code.length === 4 && code.startsWith("Key")) return code[3].toLowerCase(); // KeyM → "m"
  if (code.length === 6 && code.startsWith("Digit")) return code[5]; // Digit5 → "5"
  if (code.startsWith("Numpad")) {
    const rest = code.slice(6);
    if (rest.length === 1 && rest >= "0" && rest <= "9") return rest; // Numpad5 → "5"
  }
  return CODE_PUNCT[code] ?? null;
}

/** Layout-independent single-key compare for HARDCODED shortcuts.
 *  Matches `key` (a normalized lowercase letter / digit / punct / named
 *  key) against BOTH the logical `event.key` and the physical
 *  `event.code`, so e.g. `keyEq(e, "p")` fires for the physical P key on
 *  a Hebrew layout (where `event.key` is "פ"). The caller checks
 *  modifiers. */
export function keyEq(e: KeyLike, key: string): boolean {
  const k = key.toLowerCase();
  if (e.key.toLowerCase() === k) return true;
  const phys = physicalKey(e);
  return phys != null && phys.toLowerCase() === k;
}

export function matches(e: KeyLike, accel: ParsedShortcut | null): boolean {
  if (!accel) return false;
  if (e.ctrlKey !== accel.ctrl) return false;
  if (e.altKey !== accel.alt) return false;
  if (e.shiftKey !== accel.shift) return false;
  if (e.metaKey !== accel.meta) return false;
  // Match the logical key (event.key, handles named keys + US layout)
  // OR the physical key (event.code, layout-independent). The physical
  // fallback is what makes a letter hotkey work on a Hebrew layout.
  return keyEq(e, accel.key);
}

/** The parsed form of every configurable accelerator, keyed by action id. */
export type ShortcutTable = Record<ShortcutActionId, ParsedShortcut | null>;

/** Build a parsed-shortcut table from the current settings (with the
 *  defaults backfilled for any missing field). Returned at settings
 *  load and re-built on every settings:changed.
 *
 *  Phase 87: iterates SHORTCUT_ACTION_IDS instead of hand-listing the
 *  fields, so adding a binding to ShortcutsSettings is enough — the
 *  boolean `copy_on_select_with_ctrl_c` is not an accelerator and is
 *  filtered out there. Callers read it from `settings.shortcuts`. */
export function buildShortcutTable(
  s: ShortcutsSettings | null | undefined,
): ShortcutTable {
  const merged: ShortcutsSettings = { ...DEFAULT_SHORTCUTS, ...(s ?? {}) };
  const table = {} as ShortcutTable;
  for (const id of SHORTCUT_ACTION_IDS) {
    table[id] = parseShortcut(merged[id]);
  }
  return table;
}

/** Canonical spelling of an accelerator, for comparing two bindings:
 *  "ctrl+shift+m" and "Shift+Ctrl+M" both canonicalise to
 *  "Ctrl+Shift+M". Returns null for anything unparseable (which
 *  therefore never counts as a conflict). */
export function canonicalAccel(s: string | undefined | null): string | null {
  const p = parseShortcut(s);
  if (!p) return null;
  const parts: string[] = [];
  if (p.ctrl) parts.push("Ctrl");
  if (p.alt) parts.push("Alt");
  if (p.shift) parts.push("Shift");
  if (p.meta) parts.push("Meta");
  let label = p.key;
  if (label.length === 1 && label.match(/[a-z]/i)) label = label.toUpperCase();
  if (label === " ") label = "Space";
  parts.push(label);
  return parts.join("+");
}

/** Accelerators claimed by more than one action — the set is of canonical
 *  strings, so a row is in conflict when canonicalAccel(its value) is a
 *  member. `extra` carries bindings that live outside settings.shortcuts
 *  (today: the STT push-to-talk hotkey, which is stored under
 *  settings.stt) so those collide too — that is not hypothetical, the
 *  Focus/Zoom binding had to be moved off Ctrl+Shift+M by hand because it
 *  silently shadowed push-to-talk. */
export function conflictingAccels(
  s: ShortcutsSettings | null | undefined,
  extra?: Record<string, string | undefined | null>,
): Set<string> {
  const merged: ShortcutsSettings = { ...DEFAULT_SHORTCUTS, ...(s ?? {}) };
  const seen = new Set<string>();
  const dupes = new Set<string>();
  const values = [
    ...SHORTCUT_ACTION_IDS.map((id) => merged[id]),
    ...Object.values(extra ?? {}),
  ];
  for (const v of values) {
    const c = canonicalAccel(v);
    if (!c) continue;
    if (seen.has(c)) dupes.add(c);
    else seen.add(c);
  }
  return dupes;
}

/** Format a KeyboardEvent as an accelerator string, used by the
 *  Settings UI's "click to record" picker. Returns null if the
 *  event has no non-modifier key (so the picker can keep listening). */
export function formatEvent(e: KeyLike): string | null {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Meta");
  // Exclude bare modifier keys.
  if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return null;
  // Phase 62.B (item G): prefer the PHYSICAL key so recording the hotkey
  // on a non-US layout still stores the canonical accelerator (physical
  // M → "M", not the Hebrew "צ"). Named keys (Enter, ArrowUp…) have no
  // physical mapping → fall back to event.key.
  let label = physicalKey(e) ?? e.key;
  // Letters: uppercase for display ("Ctrl+Shift+C", not "Ctrl+Shift+c").
  if (label.length === 1 && label.match(/[a-z]/i)) label = label.toUpperCase();
  // Space → "Space" for readability.
  if (label === " ") label = "Space";
  parts.push(label);
  return parts.join("+");
}
