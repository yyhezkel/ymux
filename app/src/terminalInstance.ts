import { Terminal, type IDisposable, type ILink, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { ClipboardAddon } from "@xterm/addon-clipboard";
import type {
  IClipboardProvider,
  ClipboardSelectionType,
} from "@xterm/addon-clipboard";

import { visualToLogical, visualToLogicalStream } from "./copyBidi";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { reorderRtlForDisplay } from "./bidi";
import { createLogger } from "./logger";

// Rule #9: user-visible logging. The rest of this file still uses raw
// console.* — NOT pre-existing debt as once assumed: Phase 79 converted this
// file (24 logger sites, 0 console.*) and merge bcaa330 threw the conversion
// away. See FOLLOWUPS. The clipboard paths were converted back because an
// invisible failure there is what hid the paste bug.
const termLog = createLogger("TERM");
import {
  detectDirection,
  foldTuiOwnsBidi,
  rowDirections,
  strongCounts,
  nextTuiOwnsBidi,
} from "./textDirection";
import type { RowDirMode } from "./textDirection";
import { transformMouseX, findRow } from "./mouseRtl";
import type { RtlProfileKind } from "./types";
import { t } from "./i18n";

// Phase 62.B (item J): parse a `file://` URI (as emitted in Claude Code's
// OSC 8 hyperlinks, e.g. `file:///home/runner/.env.prod`) into the bare
// remote path. Returns null for non-file URIs. Handles the empty-host
// form (`file:///path`) and a `file://host/path` form, plus percent-
// decoding.
function fileUriToPath(uri: string): string | null {
  if (!uri.startsWith("file://")) return null;
  let rest = uri.slice("file://".length);
  if (!rest.startsWith("/")) {
    // `file://host/path` — drop the host component.
    const slash = rest.indexOf("/");
    rest = slash >= 0 ? rest.slice(slash) : "/";
  }
  try {
    rest = decodeURIComponent(rest);
  } catch {
    // Leave as-is if it isn't valid percent-encoding.
  }
  return rest;
}

// Phase 64 (J, Track B): Claude Code prints produced files as PLAIN TEXT —
// `[file] <path> (<size>)` — with no OSC 8 wrapping (confirmed live
// 2026-07-15: the OSC 8 transport works for Claude's *other* links, but
// `[file]` never arrives as one). This regex drives a custom xterm link
// provider that makes the `[file] <path> (<size>)` run clickable. The path
// is the first whitespace-free token after the tag; the trailing size paren
// is optional so the match survives format drift.
const FILE_LINK_RE = /\[file\]\s+(\S+)(?:\s+\(([^()]{1,32})\))?/g;

// Phase 9.A live font apply + Phase 15.A per-line auto-direction.
//
// A global registry of all live terminals lets a settings change walk
// every open pane and re-apply the new font / RTL mode. The current
// values are also tracked so a freshly constructed terminal picks up
// the user's choice rather than the hard-coded defaults below.
//
// `rtl_mode` selects how mixed Hebrew / Arabic text is displayed:
//   - "auto_per_line" (default in 15.A): no logical-order reorder; the
//     terminal uses the DOM renderer and each row div gets `dir="auto"`
//     so the browser decides direction per line — first strong
//     directional character wins, exactly like Termius. Mirrors what
//     most users expect for an SSH prompt that prints Hebrew.
//   - "force_rtl" (2026-08-23): same DOM renderer, but EVERY row gets
//     dir="rtl" with no detection at all — no dominance vote, no block
//     grouping, no frame stripping. A shell that is unconditionally
//     right-to-left, which is what Yossi asked for on remote panes ("RTL
//     מלא, ולא שורה שורה"). Latin runs inside a row still read correctly:
//     the browser's bidi resolves them as LTR runs inside an RTL paragraph.
//     Positional rows (tmux status, zellij frame, vim, htop) DO come out
//     mirrored — see `rowDirections` in textDirection.ts for why that is
//     the deal being struck rather than a bug.
//   - "bidi_reorder" (legacy v1 behavior): WebGL renderer + bidi-js
//     reorder bytes into visual order before writing. Faster, but
//     surprises users who expect editable lines to remain in logical
//     order, and breaks on selection / copy.
//   - "off": WebGL renderer, no reorder. Hebrew prints
//     logical-order-as-written, which most monospace fonts show
//     left-to-right.
//
// 1pt ≈ 1.333px at 96dpi; xterm.js wants pixels.
const PT_TO_PX = 1.3333;
export type RtlMode = "auto_per_line" | "force_rtl" | "bidi_reorder" | "off";

/**
 * The modes that run on xterm's DOM renderer and get a real `dir` per row.
 *
 * 2026-08-23: added with `force_rtl`. There were SIX separate
 * `=== "auto_per_line"` gates — the renderer choice in the constructor,
 * `applyRenderer`, `staleRenderer`, the mouse capture, the dir observer and
 * the row pass — and `applyRenderer`'s own comment records what happens when
 * they disagree: the pane lands in a combination that is none of the modes and
 * does no bidi at all. One predicate so a fifth mode is one edit, not six.
 *
 * NOT the same question as `normaliseIncomingToLogical` (which stays pinned to
 * `auto_per_line`): that one asks whether the incoming BUFFER is visual order,
 * and a remote pane's never is.
 */
export function usesRowDir(m: RtlMode): m is RowDirMode {
  return m === "auto_per_line" || m === "force_rtl";
}

// Minimal typed view into xterm's private CharSizeService so we can call its
// own measure() directly and read the measured cell — without an `any`.
interface XtermInternals {
  _core?: {
    _charSizeService?: {
      width?: number;
      height?: number;
      hasValidSize?: boolean;
      measure?: () => void;
    };
  };
}

let g_fontFamily: string | null = null;
let g_fontSizePx: number | null = null;
/** Redesign pass 4: the terminal follows the active theme. Cached so new
 *  panes construct with it; null until the first applyTheme() pushes one
 *  (the constructor then falls back to the Tokyo Night default below). */
let g_termTheme: ITheme | null = null;
/** Phase 16: when true, Ctrl+C with a non-empty selection copies the
 *  selection to clipboard instead of sending SIGINT. Mirrors Windows
 *  Terminal / VS Code's behavior. Settings → Shortcuts toggles it. */
let g_ctrlCCopyOnSelect = true;

/** The four RTL knobs, for ONE class of pane. Mirrors `RtlProfile` in
 *  settings.rs. */
export interface RtlProfileSettings {
  rtlMode: RtlMode;
  /** Per-line `dir` from the text. Only meaningful in `auto_per_line`;
   *  `force_rtl` ignores it by design, and the WebGL modes have no rows. */
  autoDirection: boolean;
  /** Mirror Left/Right arrows while the cursor sits on an RTL line. */
  mirrorArrowsRtl: boolean;
  /** Legacy: force LTR while a self-bidi TUI holds the pane. Default off. */
  tuiOwnsBidi: boolean;
  /**
   * Which rule decides a row's paragraph direction.
   *
   *  - "any_rtl"        any Hebrew/Arabic on the row takes it RTL. What every
   *                     shipped version before 2026-08-19 did, and what remote
   *                     panes are known to render correctly.
   *  - "tui_dominance"  the RTL_DOMINANCE vote (see textDirection.ts): Hebrew
   *                     wins unless massively outnumbered by Latin, which
   *                     stops a TUI status bar from mirroring its own layout.
   *
   * 2026-08-19: this is deliberately keyed on the PANE CLASS and never on what
   * is running inside the pane. The vote first shipped gated on `tuiOwnsBidi`,
   * and since the OSC title propagates over SSH — that is how Claude Code is
   * detected at all — it fired on remote panes too and broke them. Yossi's
   * instruction after that: "תוודא שיש הפרדה מוחלטת בין המרוחק למקומי - ואז
   * השינויים שתרצה לעשות על המקומי לא יפגעו במרוחק". A per-profile field is that
   * separation; `remote_direction_policy_is_the_pre_2026_08_19_rule` in
   * settings.rs and the parity tests in textDirection.test.ts enforce it.
   */
  directionPolicy: DirectionPolicy;
}

/** See `RtlProfileSettings.directionPolicy`. */
export type DirectionPolicy = "any_rtl" | "tui_dominance";

/**
 * 2026-08-19: these used to be four scalar globals, which made every pane in
 * the app share one RTL strategy — so a setting that fixed local panes broke
 * remote ones and vice versa. They are per-profile now; each TerminalInstance
 * reads through `this.profile` (see `profileFor` in types.ts).
 *
 * The defaults were briefly different — raw ConPTY hands over visual-order
 * Hebrew, so a native Windows pane wanted `bidi_reorder`. Then zellij went in
 * front of local panes and normalised the stream to logical order, the way a
 * Linux pty does, so both sides converged on `auto_per_line`. The split still
 * lets a user tune them apart; it just no longer needs to.
 *
 * Real values arrive from settings on boot; these only cover the window before
 * the first `applyTheme`.
 */
const g_rtl: Record<RtlProfileKind, RtlProfileSettings> = {
  local: {
    // Back to auto_per_line, together with the detection that makes
    // `tuiOwnsBidi` real. A local SHELL is logical order and renders perfectly
    // this way — Yossi's side-by-side screenshot shows cmd.exe correct and
    // right-aligned. Only Claude needs the bidi switched off, and that is now
    // driven per pane rather than by pinning the whole profile to "off".
    rtlMode: "auto_per_line",
    autoDirection: true,
    mirrorArrowsRtl: true,
    tuiOwnsBidi: true,
    directionPolicy: "any_rtl",
  },
  remote: {
    rtlMode: "auto_per_line",
    autoDirection: true,
    mirrorArrowsRtl: true,
    tuiOwnsBidi: false,
    // Do not change this without reading the note on `directionPolicy`. Remote
    // panes render Hebrew correctly on this rule and the parity tests pin it.
    directionPolicy: "any_rtl",
  },
};
/** Phase 65.O (round 6): one-time guard so the "no wheel proxy" note is
 *  logged once, not once per pane. */
let g_loggedNoWheelProxy = false;
const g_terminals: Set<TerminalInstance> = new Set();

// v0.4.4-beta.2: mouse-tracking leak recovery. When a full-screen app
// (vim `mouse=a`, fzf, less, htop) enables SGR/X10 mouse tracking and then
// exits UNCLEANLY (Ctrl+C, SSH drop, kill), it never sends the disable
// sequence, so xterm.js keeps mouse reporting on — every later click in the
// bare shell sends `\e[<0;x;yM` (SGR 1006) which the shell prints as literal
// text. Writing these DECRST sequences to xterm (NOT the PTY) clears xterm's
// mouse state so it stops emitting mouse events. Covers X10 (1000),
// button-event (1002), any-event (1003), SGR (1006), urxvt (1015), and X10
// hilite (9). Rule #1: this is a fixed control string, never PTY content.
const MOUSE_DISABLE_SEQ =
  "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?9l";
/** v0.4.4-beta.2: when true (default) each pane clears stale mouse-tracking
 *  state on connect/attach. Settings → Terminal. */
let g_autoResetOnConnect = true;
export function setAutoResetOnConnect(on: boolean): void {
  g_autoResetOnConnect = on;
}

/**
 * Push a new font family + size into every live xterm and remember the
 * values so future TerminalInstance constructions inherit them. Family
 * is passed through `quoteFamily()`-style fallbacks already by the
 * caller. Called from App.tsx on settings load and on every
 * `settings:changed`.
 */
/** Tokyo Night — the historical hardcoded palette, kept as the fallback
 *  for terminals constructed before the first applyTheme() call. */
const DEFAULT_TERM_THEME: ITheme = {
  background: "#0e1116",
  foreground: "#e6edf3",
  cursor: "#7aa2f7",
  cursorAccent: "#0e1116",
  selectionBackground: "rgba(122, 162, 247, 0.35)",
  black: "#15161e",
  red: "#f7768e",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#a9b1d6",
  brightBlack: "#414868",
  brightRed: "#ff7a93",
  brightGreen: "#b9f27c",
  brightYellow: "#ff9e64",
  brightBlue: "#7da6ff",
  brightMagenta: "#bb9af7",
  brightCyan: "#0db9d7",
  brightWhite: "#c0caf5",
};

/**
 * Redesign pass 4: push the active theme's terminal palette into every
 * live xterm and cache it for future constructions — same contract as
 * setTerminalFont. Called from applyTheme() on load, preset switch, and
 * any base-colour edit.
 */
export function setTerminalTheme(theme: ITheme): void {
  g_termTheme = theme;
  for (const ti of g_terminals) {
    try {
      ti.term.options.theme = theme;
      // Keep the contrast floor on terminals constructed before this
      // module version (HMR) — setting it repeatedly is idempotent.
      ti.term.options.minimumContrastRatio = 4.5;
      ti.term.refresh(0, ti.term.rows - 1);
    } catch (e) {
      console.warn("setTerminalTheme: per-instance update failed", e);
    }
  }
}

export function setTerminalFont(family: string, sizePt: number): void {
  const px = Math.max(8, Math.round(sizePt * PT_TO_PX));
  g_fontFamily = family;
  g_fontSizePx = px;
  for (const ti of g_terminals) {
    try {
      ti.logFontSwap("before");
      ti.term.options.fontFamily = family;
      ti.term.options.fontSize = px;
      ti.fitAndResize();
      ti.term.refresh(0, ti.term.rows - 1);
      ti.logFontSwap("afterSet");
      requestAnimationFrame(() => ti.logFontSwap("settled"));
    } catch (e) {
      console.warn("setTerminalFont: per-instance update failed", e);
    }
  }
}

/**
 * Change ONLY the terminal font size (keeps the current family). Used by the
 * pop-out window's Ctrl+wheel zoom — a separate webview context, so this only
 * touches that context's terminals, never the main grid. `sizePt` is in
 * points; converted to px with the same clamp as setTerminalFont.
 */
export function setTerminalFontSize(sizePt: number): void {
  const px = Math.max(8, Math.round(sizePt * PT_TO_PX));
  g_fontSizePx = px;
  for (const ti of g_terminals) {
    try {
      ti.term.options.fontSize = px;
      ti.fitAndResize();
      ti.term.refresh(0, ti.term.rows - 1);
    } catch (e) {
      console.warn("setTerminalFontSize: per-instance update failed", e);
    }
  }
}

/**
 * Phase 15.A: switch the RTL handling strategy. Existing terminals
 * keep their previously-constructed renderer (DOM vs WebGL changes
 * require a fresh xterm), so this only affects newly-opened panes —
 * we surface a hint in the Settings UI so the user knows to re-open
 * affected panes. The reorder pipeline (writeData) flips immediately
 * for all current panes.
 */
export function setRtlProfiles(next: Record<RtlProfileKind, RtlProfileSettings>): void {
  g_rtl.local = { ...next.local };
  g_rtl.remote = { ...next.remote };
  // 2026-08-19: every open pane moves to the new mode NOW, renderer included.
  // This used to only refresh the direction pass, on the belief that xterm.js
  // could not swap DOM↔WebGL live — see `applyRenderer`, which is where that
  // belief is disproved. Until then a mode change on an open pane produced a
  // half-applied state that no setting screen described.
  for (const ti of g_terminals) {
    ti.applyRenderer();
    ti.ensureDirObserver();
    ti.refreshRowDirections();
  }
}

/** Route an out-of-band "Claude is in front" signal to one pane. Used by
 *  App.tsx for the connect wizard and for session-start/end hooks; a pane that
 *  is not open yet simply has nothing to tell. */
export function setPaneTuiSignal(paneId: string, on: boolean | null): void {
  for (const ti of g_terminals) {
    if (ti.paneId === paneId) ti.setTuiSignal(on);
  }
}

export function getRtlProfile(kind: RtlProfileKind): RtlProfileSettings {
  return g_rtl[kind];
}

/** True when at least one live pane was built under a different renderer than
 *  its profile now asks for, i.e. the user must re-open it for the change to
 *  fully apply. Drives the "re-open panes" hint in Settings — previously the
 *  contract was only a code comment, so a mode switch looked like it silently
 *  did nothing. */
export function hasStaleRenderer(): boolean {
  for (const ti of g_terminals) if (ti.staleRenderer) return true;
  return false;
}

// v0.4.4 (RTL Approach C): the per-line auto-direction escape hatch. When ON
// (default), each terminal row in `auto_per_line` mode gets an explicit
// `dir` computed by detectDirection (mixed→RTL, pure-Latin→LTR). When OFF,
// rows fall back to plain `dir="ltr"` (classic terminal, no BiDi flipping).
// Only meaningful in `auto_per_line` mode — every other RtlMode ignores it,
// `force_rtl` deliberately so (it is the mode with no escape hatches).
// (now `autoDirection` on each RtlProfileSettings — see g_rtl above)

/** Phase 16: flip the Ctrl+C-copies-on-selection behavior at runtime. */
export function setCtrlCCopyOnSelect(enabled: boolean): void {
  g_ctrlCCopyOnSelect = enabled;
}

// (mirrorArrowsRtl and tuiOwnsBidi now live on each RtlProfileSettings and
//  arrive through setRtlProfiles — see g_rtl above)


/** Phase HH: swap a Left/Right cursor-key escape sequence to the other
 *  direction. Handles both normal (`\e[C`/`\e[D`) and application-cursor
 *  mode (`\eOC`/`\eOD`), so it's correct regardless of the TUI's mode.
 *  Returns the input unchanged if it isn't a horizontal arrow. */
function swapArrowSeq(data: string): string {
  switch (data) {
    case "\x1b[C":
      return "\x1b[D";
    case "\x1b[D":
      return "\x1b[C";
    case "\x1bOC":
      return "\x1bOD";
    case "\x1bOD":
      return "\x1bOC";
    default:
      return data;
  }
}

/**
 * Read the system clipboard as text.
 *
 * `navigator.clipboard.readText()` is not usable here: WebView2 permits
 * clipboard WRITE but gates READ behind a permission the host never grants,
 * so it rejects. That is why terminal Copy worked and Paste did nothing —
 * and why it was invisible: the only handler was a `console.warn`.
 *
 * Try the web API first anyway (it is synchronous-ish and works if a future
 * runtime does allow it), then fall back to the host-side command.
 */
export async function readClipboardText(): Promise<string> {
  try {
    const t = await navigator.clipboard.readText();
    if (t) return t;
  } catch {
    // Expected in WebView2 — fall through rather than log noise per paste.
  }
  return await invoke<string>("clipboard_read_text");
}

/** Paste arbitrary text into the active terminal. xterm.js will wrap
 *  the bytes with bracketed-paste escape codes if the connected shell
 *  has enabled the mode (which most modern shells do). Falls back to
 *  the first focused terminal if a specific instance isn't passed. */
export function pasteIntoActiveTerminal(text: string): void {
  if (!text) return;
  let target: TerminalInstance | null = null;
  for (const ti of g_terminals) {
    if (ti.container.contains(document.activeElement)) {
      target = ti;
      break;
    }
  }
  if (!target) target = g_terminals.values().next().value ?? null;
  try {
    target?.term.paste(text);
    // Phase 65 (bug X): keep focus in the terminal after a paste.
    // Reading the clipboard (and the menu/keystroke that triggered it)
    // can pull focus off the xterm textarea, so the caret "jumps" out of
    // the pane. Re-assert focus on the pasted-into terminal.
    target?.term.focus();
  } catch (e) {
    console.warn("paste failed", e);
  }
}

/** Copy the current xterm.js selection (if any) to the system
 *  clipboard. Returns true on success — the caller uses the boolean
 *  to decide whether to fall through to a different binding. */
export async function copyTerminalSelection(): Promise<boolean> {
  for (const ti of g_terminals) {
    if (!ti.container.contains(document.activeElement)) continue;
    const sel = ti.term.getSelection();
    if (!sel) return false;
    return ti.copyToClipboard(sel, "shortcut");
  }
  return false;
}

// Phase 62.A (item E): custom terminal right-click menu. Phase 60
// blocked the native WebView2 context menu (which the user had been
// using to Copy/Paste in the PLAIN terminal) and replaced right-click
// with paste-only — that read as "copy AND paste stopped working".
// This restores a discoverable Copy / Paste / Select-all menu. The
// native menu stays suppressed. One menu at a time, document-wide.
let g_termMenu: HTMLDivElement | null = null;
function dismissTerminalMenu(): void {
  if (!g_termMenu) return;
  g_termMenu.remove();
  g_termMenu = null;
  document.removeEventListener("mousedown", onTermMenuOutside, true);
  document.removeEventListener("keydown", onTermMenuKey, true);
  window.removeEventListener("blur", dismissTerminalMenu);
  window.removeEventListener("resize", dismissTerminalMenu);
}
function onTermMenuOutside(e: MouseEvent): void {
  if (g_termMenu && !g_termMenu.contains(e.target as Node)) dismissTerminalMenu();
}
function onTermMenuKey(e: KeyboardEvent): void {
  if (e.key === "Escape") dismissTerminalMenu();
}
function showTerminalContextMenu(ti: TerminalInstance, x: number, y: number): void {
  dismissTerminalMenu();
  const sel = ti.term.getSelection();
  const menu = document.createElement("div");
  menu.className = "term-ctx-menu";
  const addItem = (label: string, enabled: boolean, action: () => void) => {
    const b = document.createElement("button");
    b.className = "term-ctx-item";
    b.textContent = label;
    b.disabled = !enabled;
    b.addEventListener("click", () => {
      action();
      dismissTerminalMenu();
    });
    menu.appendChild(b);
  };
  addItem(t("term.ctx.copy"), !!sel, () => {
    if (sel) void ti.copyToClipboard(sel, "context-menu");
  });
  addItem(t("term.ctx.paste"), true, () => {
    readClipboardText()
      .then((text) => {
        if (text) ti.term.paste(text);
        // Phase 65 (bug X): the context menu stole focus; return it to
        // the terminal so the caret stays at the paste site.
        ti.term.focus();
      })
      // Rule #9: this used to be console.warn, so the one failure that
      // mattered — clipboard read being denied — left no trace anywhere a
      // user or a maintainer would look.
      .catch((err) => termLog.warn("terminal paste failed", err));
  });
  addItem(t("term.ctx.selectAll"), true, () => ti.term.selectAll());

  // Append first so we can measure, then clamp inside the viewport.
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  const px = Math.max(4, Math.min(x, window.innerWidth - r.width - 8));
  const py = Math.max(4, Math.min(y, window.innerHeight - r.height - 8));
  menu.style.left = `${px}px`;
  menu.style.top = `${py}px`;
  g_termMenu = menu;
  // Capture-phase dismiss so a click anywhere else closes it before
  // that click does anything surprising.
  document.addEventListener("mousedown", onTermMenuOutside, true);
  document.addEventListener("keydown", onTermMenuKey, true);
  window.addEventListener("blur", dismissTerminalMenu);
  window.addEventListener("resize", dismissTerminalMenu);
}

export class TerminalInstance {
  term: Terminal;
  fit: FitAddon;
  container: HTMLDivElement;
  sessionId: string | null = null;
  paneId: string;
  // Phase 62.B (item J): the workspace this pane belongs to, set on
  // connect. Needed so an OSC 8 `file://` link click can SFTP-download
  // from the right remote. null until connected (download needs a live
  // SSH session anyway).
  workspaceId: string | null = null;
  // Phase 62.C (J.1): one-shot diagnostic flag — have we yet seen an
  // OSC 8 hyperlink sequence arrive in this pane's stream? Logged once
  // (metadata only, Rule #1) so we can tell "Claude isn't emitting OSC 8"
  // apart from "our linkHandler isn't firing".
  private oscHyperlinkLogged = false;
  /** The RTL mode active when this terminal was constructed. Changing
   * settings later only affects the data-write pipeline (and the
   * per-row dir attribute observer); the renderer choice is sticky. */
  /** The mode this pane is CURRENTLY rendering under — which renderer is
   *  loaded and which write pipeline runs. Was `rtlModeAtConstruct` and frozen
   *  for the pane's life until 2026-08-19; `applyRenderer` now keeps it equal
   *  to the live setting. */
  rtlModeRendered: RtlMode;
  private dataDisposable: IDisposable | null = null;
  // Phase 64 (J, Track B): the plain-text `[file]` link provider handle +
  // a one-shot "it matched" diagnostic flag (metadata only, Rule #1) so a
  // live test can distinguish "regex never matched the real format" from
  // "click path is broken".
  private fileLinkProvider: IDisposable | null = null;
  private fileLinkMatchLogged = false;
  private ro: ResizeObserver | null = null;
  private dirObserver: MutationObserver | null = null;
  /** 2026-08-23: the RTL mouse capture is install-once. See
   *  `installRtlMouseCapture` for why it is never removed again. */
  private rtlMouseInstalled = false;
  // v0.4.4 (RTL Approach C): rAF handle + per-row text cache for the
  // per-line direction pass. The MutationObserver coalesces a burst of cell
  // mutations into a single applyDir() per animation frame; the cache lets
  // that pass skip any row whose text is unchanged since we last set its dir.
  private dirRafId: number | null = null;
  private dirCache: WeakMap<Element, string> = new WeakMap();
  // Phase 23.E: keep a handle to the WebGL addon so we can flush its
  // glyph atlas on resize. Without that, the GPU canvas keeps painting
  // the previous viewport's grid metrics — visible as "stuck" lines
  // that don't reflow when the user drags the pane divider.
  private webglAddon: WebglAddon | null = null;
  // Phase 23.E: rAF-throttle the ResizeObserver fire-rate. During a
  // drag, RO fires per-pixel and each call sends a SIGWINCH down the
  // SSH channel — tmux struggles to keep up and the renderer thrashes.
  // One fit per animation frame is enough; the trailing call after
  // the drag stops is what produces the final correct layout.
  private resizeRafId: number | null = null;
  // Phase 25.B: trailing-edge debounce. The rAF throttle alone is
  // leading-edge — during a FAST drag the very last container size
  // can land between rAF ticks and never get a fit, leaving the
  // terminal "stuck" at an intermediate width until the user
  // re-focuses the pane. This timer fires one authoritative fit
  // ~140ms after the resize storm ends, guaranteeing the final
  // dimensions are always applied.
  private resizeSettleTimer: number | null = null;
  // Phase 35 (#1.1): rAF-coalesced PTY writer. During fast streaming
  // (Claude generating, a noisy build), the backend fires many small
  // pty:data events per frame. Calling term.write() on each one forces
  // xterm to reflow/repaint per chunk and starves the event loop —
  // the window shows "(Not Responding)". Instead we accumulate chunks
  // and flush a single merged write per animation frame.
  private pendingChunks: string[] = [];
  private flushRafId: number | null = null;
  // v0.4.4-beta.4 (RTL mouse fix): capture-phase mouse listener disposer.
  // The listener runs before xterm.js's own bubble-phase handlers and, for
  // events over an RTL row, dispatches a synthetic MouseEvent with clientX
  // mirrored around the row midpoint. See mouseRtl.ts for the rationale.
  private rtlMouseTeardown: (() => void) | null = null;
  // 2026-07-15 (Claude visual-order RTL): true while a TUI that does its
  // own bidi reordering (Claude Code) holds this pane's foreground. While
  // set, the per-row dir pass renders everything LTR and arrow-mirroring /
  // the bidi_reorder write pipeline stand down — the TUI's output is
  // already in visual order and any second bidi pass corrupts it. Driven
  // by terminal-title changes; see nextTuiOwnsBidi in textDirection.ts.
  private tuiOwnsBidi = false;
  /** Last direction vector reported by `logDirections`, so the per-frame pass
   *  only speaks when its answer actually changed. */
  private lastDirSignature: string | null = null;

  /** The EFFECT of tuiOwnsBidi, as opposed to the detected state. Detection
   *  and its log line always run — they are what made the 2026-08-18
   *  diagnosis possible in minutes — but the LTR forcing is gated, because
   *  current Claude emits logical-order Hebrew and forcing LTR corrupts it.
   *  See settings.rs `tui_owns_bidi`. */
  /** Out-of-band "Claude is in front", from the connect wizard or a Claude
   *  hook. `null` = nobody told us, fall back to the title. See
   *  `foldTuiOwnsBidi` for why this outranks the title. */
  private tuiExplicit: boolean | null = null;

  /** Called by App.tsx: `true` on connect-with-Claude and on a session-start
   *  hook for this pane, `null` when Claude ends or the pane connects to
   *  something else. */
  setTuiSignal(on: boolean | null): void {
    if (this.tuiExplicit === on) return;
    this.tuiExplicit = on;
    termLog.info(
      `tui-signal pane=${this.paneId} ` +
        `explicit=${on === null ? "-" : on ? 1 : 0} title=${this.tuiOwnsBidi ? 1 : 0}`,
    );
    this.applyRowDirections(true);
  }

  /**
   * Is a self-reordering TUI (Claude Code on Windows) holding this pane, AND is
   * the profile switch that handles it turned on?
   *
   * 2026-08-20: what "handles it" MEANS changed. It used to mean "render the
   * bytes verbatim with `unicode-bidi: bidi-override`", which fixes the letters
   * and forbids right alignment in the same stroke — `dir` has to be "ltr" for
   * an override to make sense, and right alignment can only come from
   * `dir="rtl"`. It now means "normalise the stream to logical on the way IN",
   * after which the pane is indistinguishable from a remote one and the browser
   * does the bidi. Same switch, same signal, different strategy — see
   * `normaliseIncomingToLogical`.
   */
  private get bidiOwnedByTui(): boolean {
    return foldTuiOwnsBidi(this.tuiExplicit, this.tuiOwnsBidi) && this.rtl.tuiOwnsBidi;
  }

  /**
   * Convert this pane's incoming bytes from visual to logical order?
   *
   * The whole point of the 2026-08-20 change. Measured truth table, from
   * Yossi's screen:
   *
   *   buffer   | render          | letters | placement
   *   ---------|-----------------|---------|----------
   *   visual   | dir="rtl"       | wrong   | RIGHT
   *   visual   | bidi-override   | RIGHT   | left      <- what shipped before
   *   LOGICAL  | dir="rtl"       | RIGHT   | RIGHT     <- this, and what remote
   *                                                       has always done
   *
   * `reorderRtlForDisplay` cannot substitute for this: it reorders runs IN
   * PLACE and does no padding, so it can never move a line to the right edge.
   * Measured — `reorder(unreorder(v)) === v` on every sample. Placement comes
   * from the browser under `dir="rtl"`, and the browser only gets that right if
   * we hand it logical text.
   *
   * Only in `auto_per_line`, because that is the mode with the DOM renderer;
   * `bidi_reorder` and `off` run WebGL, which has no `dir` and therefore no
   * placement to gain.
   *
   * 2026-08-23: `force_rtl` also has the DOM renderer and is still excluded,
   * and the distinction is worth keeping straight. `usesRowDir` asks which
   * modes PAINT rows with a `dir`; this asks whether the incoming BUFFER is
   * visual order and needs converting. Visual order is a local/ConPTY
   * condition, and `force_rtl` was built for remote panes, whose stream is
   * logical already. Normalising a logical stream reverses it.
   */
  private get normaliseIncomingToLogical(): boolean {
    return this.bidiOwnedByTui && this.rtlModeRendered === "auto_per_line";
  }

  /**
   * Does THIS pane's buffer hold visual-order text? If so, anything copied out
   * of it has to be un-reversed before it reaches the clipboard.
   *
   * Measured on Yossi's machine 2026-08-20, and the two rows are exactly
   * inverted, which is what makes the rule decidable:
   *
   *   plain PowerShell — displays reversed, pastes CORRECTLY  (buffer logical)
   *   Claude Code      — displays CORRECTLY, pastes reversed  (buffer visual)
   *
   * Two independent ways a buffer ends up visual:
   *
   *  1. The SOURCE wrote visual order — Claude Code on Windows does. Note this
   *     reads `foldTuiOwnsBidi` directly and does NOT `&&` in
   *     `this.rtl.tuiOwnsBidi` the way `bidiOwnedByTui` does. That setting is a
   *     RENDERING preference; whether Claude is in front is a fact about the
   *     source. Turning off a render checkbox must not silently put reversed
   *     text back on the clipboard.
   *  2. WE reordered it — under rtl_mode="bidi_reorder", flushPending writes
   *     reordered bytes into the buffer, so the selection inherits them. That
   *     branch is gated `&& !bidiOwnedByTui`, so the two never stack.
   */
  private get bufferIsVisualOrder(): boolean {
    // 2026-08-20, second revision: if we normalised on the way in, the buffer
    // is LOGICAL and the clipboard must not flip it again. That case has to be
    // subtracted first — it is precisely the case where the source is visual,
    // so testing `sourceIsVisual` alone would now be wrong in exactly the
    // situation the normalisation exists for.
    if (this.normaliseIncomingToLogical) return false;
    const sourceIsVisual = foldTuiOwnsBidi(this.tuiExplicit, this.tuiOwnsBidi);
    const weReordered =
      this.rtlModeRendered === "bidi_reorder" && !this.bidiOwnedByTui;
    return sourceIsVisual || weReordered;
  }

  /**
   * The one place terminal text becomes clipboard text.
   *
   * All four copy paths land here — the OSC 52 provider (which is how zellij's
   * copy-on-select and Claude's fullscreen renderer copy), Ctrl+C with a
   * selection, the Ctrl+Shift+C shortcut, and the right-click menu. They used
   * to call `navigator.clipboard.writeText` separately, which is how a fix
   * could have landed for local panes and missed SSH ones.
   */
  async copyToClipboard(text: string, via: string): Promise<boolean> {
    if (!text) return false;
    const flip = this.bufferIsVisualOrder;
    const out = flip ? visualToLogical(text) : text;
    try {
      await navigator.clipboard.writeText(out);
      // Rule #1: lengths and a route label, never the content itself.
      console.debug(
        `[copy] ${via} ${out.length} chars${flip ? " (visual->logical)" : ""}`,
      );
      return true;
    } catch (e) {
      console.warn(`clipboard write failed (${via})`, e);
      return false;
    }
  }

  /* NOTE FOR THE NEXT SESSION — the two facts this cost three rounds to pin:
   *
   *  1. Claude Code on Windows writes RTL in VISUAL order. Measured with
   *     `zellij action dump-screen` on a live pane: the cwd came back as
   *     U+05E8 U+05D1 U+05E6, which is צבר reversed. Anything that reorders
   *     on top of that is a SECOND pass and produces logical order on screen,
   *     i.e. "the letters are backwards".
   *
   *  2. `dir` does not suppress reordering. It picks the paragraph's base
   *     direction; rule L2 still reverses RTL runs inside it. Only
   *     `unicode-bidi: bidi-override` renders bytes verbatim.
   *
   * Together they say: a pre-reordered stream needs NO bidi, and the only two
   * ways to get none are the WebGL renderer (rtl_mode="off") or bidi-override
   * on the row. Reach for either of those, not for `dir`.
   */

  /** Re-run the direction pass after a live settings change. */
  refreshRowDirections(): void {
    this.applyRowDirections(true);
  }

  /** Which RTL profile this pane follows. Fixed for the pane's lifetime;
   *  `App.tsx` rebuilds the instance if a connect changes the answer. */
  readonly profile: RtlProfileKind;

  /** The four RTL knobs for THIS pane's class, read live so a settings
   *  change applies without waiting for a new pane (except the renderer,
   *  which xterm.js cannot swap — see `staleRenderer`). */
  private get rtl(): RtlProfileSettings {
    return g_rtl[this.profile];
  }

  /** True when this pane was built for a different renderer than its profile
   *  now asks for. The DOM↔WebGL choice is frozen at construction, so the
   *  only cure is re-opening the pane; the Settings UI says so. */
  get staleRenderer(): boolean {
    // Kept as an assertion rather than deleted: `applyRenderer` runs on every
    // settings change, so this must now always be false. If it is ever true,
    // a pane slipped past that walk and the no-bidi hole is back.
    const wantsDom = usesRowDir(this.rtl.rtlMode);
    const builtDom = usesRowDir(this.rtlModeRendered);
    return wantsDom !== builtDom;
  }

  constructor(paneId: string, profile: RtlProfileKind = "local") {
    this.profile = profile;
    this.paneId = paneId;
    this.rtlModeRendered = g_rtl[profile].rtlMode;
    this.container = document.createElement("div");
    this.container.className = "terminal-container";

    this.term = new Terminal({
      fontFamily:
        g_fontFamily ??
        '"Cascadia Mono", "JetBrains Mono", Consolas, "Courier New", monospace',
      fontSize: g_fontSizePx ?? 14,
      lineHeight: 1.15,
      cursorBlink: true,
      cursorStyle: "bar",
      cursorWidth: 2,
      allowProposedApi: true,
      allowTransparency: true,
      theme: g_termTheme ?? DEFAULT_TERM_THEME,
      // Redesign pass 6: apps pick 256/truecolor foregrounds assuming a
      // dark ground (Claude Code's dim-gray bullets vanish on the light
      // themes). The 16 ANSI slots are theme-mapped, but indexed/true
      // colors can't be — so let xterm auto-adjust any foreground that
      // misses WCAG AA contrast against the background (VS Code's
      // default terminal behaviour, same 4.5 ratio).
      minimumContrastRatio: 4.5,
      scrollback: 10000,
      windowsPty: { backend: "conpty" },
      windowOptions: { setWinSizeChars: true },
      // Phase 62.B (item J): handle OSC 8 hyperlinks. Claude Code emits
      // file:// links for files it produces; clicking one SFTP-downloads
      // it to the user's Downloads folder. http(s) links open in the
      // system browser. allowNonHttpProtocols lets xterm render the
      // file:// runs as clickable links at all.
      linkHandler: {
        allowNonHttpProtocols: true,
        activate: (_event: MouseEvent, uri: string) => {
          const filePath = fileUriToPath(uri);
          if (filePath !== null) {
            // Dispatch to App, which owns the toast + the download invoke.
            window.dispatchEvent(
              new CustomEvent("ymux:osc-file-link", {
                detail: { workspaceId: this.workspaceId, path: filePath },
              }),
            );
            return;
          }
          if (/^https?:\/\//i.test(uri)) {
            void openUrl(uri).catch((e) => console.warn("openUrl failed", e));
          }
        },
        hover: (_event: MouseEvent, uri: string) => {
          this.container.title = uri;
        },
        leave: () => {
          this.container.removeAttribute("title");
        },
      },
      // Phase 55-C: convertEol stays FALSE. Despite occasional
      // complaints that "newlines look wrong after a resize," flipping
      // this to true would double every CRLF that ConPTY (and every
      // modern PTY) already emits, because convertEol rewrites LF →
      // CRLF on the INPUT stream regardless of what's there. The
      // reflow problem is structural to scrollback rasterisation —
      // see docs/CONTRIBUTING.md "Scrollback reflow is fundamentally
      // limited" for the full background.
      convertEol: false,
      // Phase 23.E: explicit reflow=true. xterm.js's default is true,
      // but if a previous setting drifted, scrollback wouldn't rewrap
      // when the pane is resized — text appears "stuck" at the
      // pre-resize column width. Belt-and-suspenders.
      // (Property removed in xterm v5+; if the type errors leave it
      // out — we still get reflow because that's now the unconditional
      // behaviour.)
    });
    this.fit = new FitAddon();
    this.term.loadAddon(this.fit);
    // Phase LL: OSC 52 clipboard support. Claude Code's new fullscreen
    // renderer copies the selection by emitting an OSC 52 escape sequence
    // (the alt-screen + SGR-mouse mode steals drag-selection, so OSC 52 is
    // how it reaches the system clipboard). xterm.js ignores OSC 52 without
    // this addon — which is exactly why "copy didn't work" in fullscreen.
    // Phase LL: OSC 52 clipboard provider — WRITE-ONLY. A remote program (e.g.
    // Claude's fullscreen renderer, or zellij's copy-on-select) can copy its
    // selection into the OS clipboard via OSC 52; we deliberately return "" for
    // OSC 52 READ queries so a remote can NEVER exfiltrate the local clipboard.
    // The addon hands us the already-base64-decoded text.
    //
    // 2026-08-20: built PER INSTANCE rather than shared. It used to be a module
    // singleton, which meant it had no idea which pane the text came from — and
    // whether a pane's buffer holds visual or logical order is a per-pane fact.
    const oscClipboard: IClipboardProvider = {
      readText: (_sel: ClipboardSelectionType): string => "",
      writeText: async (
        _sel: ClipboardSelectionType,
        text: string,
      ): Promise<void> => {
        await this.copyToClipboard(text, "osc52");
      },
    };
    this.term.loadAddon(new ClipboardAddon(undefined, oscClipboard));
    // Phase 64 (J, Track B): make Claude's plain-text `[file] <path> (<size>)`
    // lines clickable. Absolute paths reuse the existing OSC 8 file-link
    // download path (Save-As dialog via App); relative paths can't be
    // resolved — ymux has no per-pane remote cwd (OSC 7 tracking is the
    // future prerequisite) — so the click copies the path and a toast says
    // it's relative to the pane's directory.
    this.fileLinkProvider = this.term.registerLinkProvider({
      provideLinks: (y, callback) => {
        const line = this.term.buffer.active.getLine(y - 1);
        if (!line) return callback(undefined);
        const text = line.translateToString(true);
        let links: ILink[] | undefined;
        FILE_LINK_RE.lastIndex = 0;
        for (let m = FILE_LINK_RE.exec(text); m; m = FILE_LINK_RE.exec(text)) {
          const path = m[1];
          if (!this.fileLinkMatchLogged) {
            this.fileLinkMatchLogged = true;
            void invoke("diag_log", {
              level: "info",
              msg: `[file] link provider matched in pane ${this.paneId}`,
            }).catch(() => {});
          }
          (links ??= []).push({
            // xterm buffer ranges are 1-based with an inclusive end cell.
            range: {
              start: { x: m.index + 1, y },
              end: { x: m.index + m[0].length, y },
            },
            text: m[0],
            activate: () => {
              if (path.startsWith("/")) {
                window.dispatchEvent(
                  new CustomEvent("ymux:osc-file-link", {
                    detail: { workspaceId: this.workspaceId, path },
                  }),
                );
              } else {
                window.dispatchEvent(
                  new CustomEvent("ymux:file-link-relative", {
                    detail: { path },
                  }),
                );
              }
            },
            hover: () => {
              this.container.title = path;
            },
            leave: () => {
              this.container.removeAttribute("title");
            },
          });
        }
        callback(links);
      },
    });
    this.term.open(this.container);

    // Phase 16: custom key handler. When the user presses plain
    // Ctrl+C with a non-empty selection AND the setting is enabled,
    // copy to clipboard + suppress the keystroke (so the shell never
    // sees a SIGINT). All other keystrokes fall through unchanged.
    // Returning `false` from `attachCustomKeyEventHandler` tells
    // xterm.js NOT to forward the event to the PTY.
    this.term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      if (
        g_ctrlCCopyOnSelect &&
        e.ctrlKey &&
        !e.shiftKey &&
        !e.altKey &&
        !e.metaKey &&
        (e.key === "c" || e.key === "C")
      ) {
        const sel = this.term.getSelection();
        if (sel) {
          void this.copyToClipboard(sel, "ctrl-c");
          return false; // swallow — don't send SIGINT
        }
      }
      return true;
    });

    // Phase 60 → 62.A (item E): right-click opens a custom Copy / Paste
    // / Select-all menu. Phase 60 suppressed the native WebView2 menu
    // and made right-click paste-only; in the plain terminal that lost
    // the user's Copy affordance entirely ("copy and paste stopped
    // working"). We keep the native menu suppressed but give back a
    // real menu. The full mouse contract is now:
    //   left-drag      → native xterm selection
    //   Ctrl+C w/ sel  → copy (copy-on-select setting, above)
    //   Ctrl+Shift+C   → copy (global shortcut table)
    //   Ctrl+Shift+V   → paste (global shortcut table)
    //   right-click    → Copy / Paste / Select-all menu
    this.container.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      showTerminalContextMenu(this, e.clientX, e.clientY);
    });

    // Phase 65.O (round 6 — final): NO custom wheel handler. Earlier
    // rounds intercepted the wheel and injected Alt+arrows to drive tmux
    // copy-mode, but that fought xterm.js's native behaviour and broke
    // the common case — Yossi's `TMUX=`(empty) / `#{mouse}`=0 diag showed
    // the proxy was firing in a PLAIN bash shell (not even tmux), sending
    // Alt+Up that bash read as history navigation. xterm.js's built-in
    // wheel handling is left alone:
    //   - plain shell (no tmux)      → scrolls xterm.js's own scrollback
    //   - an app tracking the mouse  → emits SGR mouse events; the app scrolls
    //   - inside tmux (mouse OFF since Phase 91.B) → the wheel reaches
    //     nothing: tmux holds the alt screen, so xterm.js has no scrollback
    //     of its own there, and tmux never sees the wheel. Scrollback inside
    //     tmux is the keyboard — PageUp (the conf's root bind) or Ctrl+b [.
    // `scrollback` is set in the Terminal options above. (One-time note in
    // the console for future debugging.)
    if (!g_loggedNoWheelProxy) {
      g_loggedNoWheelProxy = true;
      console.log(
        "[ymux] terminal: native wheel scrollback enabled, no wheel proxy",
      );
    }

    // Phase 15.A: only load the WebGL addon for the non-auto modes.
    // The row-dir modes need the DOM renderer so we can attach
    // dir="auto" per row. WebGL paints to a canvas and has no per-cell
    // DOM, so the browser BiDi engine has nothing to hook into.
    if (!usesRowDir(this.rtlModeRendered)) {
      this.loadWebgl();
    }
    this.finishConstruction();
  }

  /**
   * Load the WebGL addon. Extracted from the constructor on 2026-08-19 so a
   * live mode change can call it too — see `applyRenderer`.
   */
  private loadWebgl(): void {
    if (this.webglAddon) return;
    {
      try {
        const addon = new WebglAddon();
        // Phase 25: WebGL contexts can be lost — GPU driver resets,
        // memory pressure, or an aggressive resize. When that happens
        // the canvas goes permanently blank unless we react. Disposing
        // the addon makes xterm.js fall back to the DOM renderer,
        // which is slower but always renders. Without this handler a
        // lost context = a dead-blank terminal with no recovery
        // (the "terminal goes blank after resizing post-conversation"
        // bug).
        addon.onContextLoss(() => {
          console.warn("WebGL context lost — falling back to DOM renderer");
          try {
            addon.dispose();
          } catch {}
          this.webglAddon = null;
          // Force a full repaint on the DOM renderer that xterm.js
          // now falls back to.
          try {
            this.term.refresh(0, this.term.rows - 1);
          } catch {}
        });
        this.term.loadAddon(addon);
        this.webglAddon = addon;
      } catch (e) {
        console.warn("WebGL addon unavailable", e);
      }
    }
  }

  /**
   * Move this pane onto the renderer its current mode asks for, live.
   *
   * 2026-08-19. The comment this replaces said xterm.js "cannot swap DOM↔WebGL
   * on a live Terminal", and the proof that it can was 200 lines below it: the
   * WebGL context-loss handler has always disposed the addon and let xterm.js
   * fall back to the DOM renderer. Disposing is the swap; `loadAddon` is the
   * swap back. Nothing in either depends on being inside the constructor.
   *
   * That mistaken belief had a real cost. The renderer and the per-row `dir`
   * pass keyed off the frozen construction mode while the write pipeline read
   * the LIVE mode, so changing the setting on an open pane landed it in a
   * combination that is none of the three modes — and one of those
   * combinations does no bidi AT ALL (no reorder because the live mode is not
   * bidi_reorder, no `dir` because the built mode is not auto_per_line). Yossi
   * reported local Hebrew broken "in all 3 options"; two of those three were
   * that state, so only one mode was ever really under test.
   */
  applyRenderer(): void {
    const want = this.rtl.rtlMode;
    if (want === this.rtlModeRendered) return;
    const wantDom = usesRowDir(want);
    if (wantDom && this.webglAddon) {
      try {
        this.webglAddon.dispose();
      } catch (e) {
        termLog.warn(`applyRenderer: webgl dispose failed pane=${this.paneId}`, e);
      }
      this.webglAddon = null;
    } else if (!wantDom && !this.webglAddon) {
      this.loadWebgl();
    }
    this.rtlModeRendered = want;
    // The dir observer, the mouse capture and the row pass all gate on the
    // rendered mode, so they have to be re-evaluated after it moves — attaching
    // the observer when we have just arrived on the DOM renderer, dropping it
    // otherwise, and installing the coordinate mirroring that a pane built on
    // WebGL never got.
    this.ensureDirObserver();
    this.installRtlMouseCapture();
    this.applyRowDirections(true);
    try {
      this.term.refresh(0, this.term.rows - 1);
    } catch {}
    termLog.info(
      `rtl-renderer pane=${this.paneId} profile=${this.profile} ` +
        `now=${want} renderer=${this.webglAddon ? "webgl" : "dom"}`,
    );
  }

  /** Constructor tail, split out when `loadWebgl` was extracted above. */
  private finishConstruction(): void {

    // Phase 23.E + 25.B: resize handling has two layers.
    //  - rAF throttle: smooth live updates during the drag without
    //    flooding SIGWINCH down the SSH channel.
    //  - trailing settle timer: fires ONE authoritative fit ~140ms
    //    after the last resize event, so the final container size is
    //    always applied even if a fast drag's last delta landed
    //    between rAF ticks. Fixes "terminal stuck at an intermediate
    //    width until I re-focus the pane".
    this.ro = new ResizeObserver(() => {
      if (this.resizeRafId == null) {
        this.resizeRafId = requestAnimationFrame(() => {
          this.resizeRafId = null;
          this.fitAndResize();
        });
      }
      if (this.resizeSettleTimer != null) {
        clearTimeout(this.resizeSettleTimer);
      }
      this.resizeSettleTimer = window.setTimeout(() => {
        this.resizeSettleTimer = null;
        // Phase 25.C: force=true — always re-send the settled
        // dimensions to tmux even if xterm.js thinks nothing changed.
        this.fitAndResize(true);
      }, 120);
    });
    this.ro.observe(this.container);
    g_terminals.add(this);

    // 2026-08-19: the log line FOLLOWUPS.md P2 asked for and that two rounds
    // of screenshot-reading went without. Every RTL question so far has come
    // down to "which pipeline did THIS pane actually get", and the answer is
    // knowable only here: the renderer and `rtlModeRendered` are frozen at
    // construction while the mode and policy are read live, so a pane can be
    // running a combination no setting screen displays.
    //
    // Metadata only (Rule #1): a mode name, a profile name, three booleans.
    // No byte of terminal content appears in it.
    termLog.info(
      `rtl-pane pane=${this.paneId} profile=${this.profile} ` +
        `mode=${this.rtl.rtlMode} builtAs=${this.rtlModeRendered} ` +
        `renderer=${this.webglAddon ? "webgl" : "dom"} ` +
        `policy=${this.rtl.directionPolicy} auto=${this.rtl.autoDirection ? 1 : 0} ` +
        `stale=${this.staleRenderer ? 1 : 0}`,
    );

    this.ensureDirObserver();
    this.installRtlMouseCapture();

    // Claude visual-order RTL: watch OSC 0/2 title reports to learn when a
    // self-bidi TUI takes/releases the pane. Listener lifetime is tied to
    // the Terminal — term.dispose() in dispose() drops it.
    this.term.onTitleChange((title) => this.onTitleChanged(title));

    // Font-init fix: a fresh pane rendered "compressed" until the user
    // swapped the terminal font and back (notably with Courier). Root
    // cause: xterm caches the character-cell size from its FIRST
    // measurement at `term.open()` above — but at construction the
    // container is still detached from the DOM and the web font may not
    // be loaded yet, so the cached cell is wrong and every later `fit()`
    // (which only divides the container size by that stale cell) inherits
    // the bad metrics. Schedule a one-shot re-measure once the container
    // is actually in the DOM and fonts are ready.
    this.scheduleInitialFontMeasure();
  }

  /**
   * v0.4.4-beta.4 (RTL mouse fix): capture-phase listeners on the terminal
   * element that mirror `clientX` for events landing over an RTL row. See
   * mouseRtl.ts for the math and the docs on why we intercept in capture
   * phase (xterm.js binds its own mousedown to `this.element` in bubble
   * phase, and its drag lifecycle attaches mousemove/mouseup to `document`
   * -- also bubble). Capture on `this.container` fires before either.
   *
   * The listener is a no-op when the pointer is over an LTR row, over no
   * row (whitespace / outside `.xterm-rows`), or when the DOM renderer
   * isn't being used (the `dir` attributes only get set in the row-dir
   * modes -- the WebGL renderer has no per-row DOM).
   *
   * Re-entry: `dispatchEvent` re-enters the capture phase, so we gate on
   * `event.isTrusted` to skip synthetic events we ourselves fired.
   */
  private installRtlMouseCapture(): void {
    // 2026-08-23: also called from `applyRenderer`, not just at construction.
    // It used to run once in `finishConstruction`, so a pane built on WebGL
    // (`off` / `bidi_reorder`) and then switched LIVE to a row-dir mode got
    // its `dir="rtl"` rows without the coordinate mirroring — clicks and
    // drag-selection landed on the mirrored column until the pane was
    // reopened. Installing is idempotent and the listener never needs
    // removing: it re-reads each row's live `dir` per event and no-ops on
    // anything that is not currently an RTL row, so under WebGL (where no
    // row carries a `dir`) it costs one `findRow` miss and returns.
    if (this.rtlMouseInstalled) return;
    if (!usesRowDir(this.rtlModeRendered)) return;
    this.rtlMouseInstalled = true;
    const el = this.container;

    const forward = (e: MouseEvent): void => {
      // Skip synthetic events we dispatched (re-entry guard). Only real
      // OS-generated events are trusted.
      if (!e.isTrusted) return;
      const rowsHost = el.querySelector(".xterm-rows") as HTMLElement | null;
      if (!rowsHost) return;
      const row = findRow(rowsHost, e.clientY);
      if (!row || row.dir !== "rtl") return;
      const newX = transformMouseX(e.clientX, row);
      if (newX === e.clientX) return;

      // Suppress the original event before it reaches xterm's own handlers
      // (bubble phase on this.element for mousedown, on document for
      // mousemove/mouseup during a drag). stopPropagation is enough --
      // stopImmediatePropagation would also block any OTHER capture
      // listener on the way down, which we don't need to do.
      e.stopPropagation();
      // preventDefault so the browser doesn't also start a native text
      // selection over the mirrored coord (which would clash with xterm's).
      e.preventDefault();

      const target = (e.target as EventTarget | null) ?? el;
      const clone = new MouseEvent(e.type, {
        bubbles: true,
        cancelable: true,
        composed: true,
        button: e.button,
        buttons: e.buttons,
        clientX: newX,
        clientY: e.clientY,
        screenX: e.screenX + (newX - e.clientX),
        screenY: e.screenY,
        ctrlKey: e.ctrlKey,
        shiftKey: e.shiftKey,
        altKey: e.altKey,
        metaKey: e.metaKey,
        detail: e.detail,
        view: window,
      });
      target.dispatchEvent(clone);
    };

    // Deliberately NOT included:
    //  - `contextmenu`: the existing handler in the constructor positions
    //    the custom Copy/Paste menu at the raw clientX. Mirroring would put
    //    the menu on the "wrong" (visual-opposite) side of the click.
    //  - `wheel`: no per-column meaning; xterm's wheel handling is scroll-
    //    based, not cell-based.
    const events: Array<keyof HTMLElementEventMap> = [
      "mousedown",
      "mousemove",
      "mouseup",
      "click",
      "dblclick",
    ];
    for (const ev of events) {
      el.addEventListener(ev, forward as EventListener, true);
    }
    // Drag past the terminal edge: xterm's SelectionService binds mousemove
    // and mouseup on `document` for the drag lifecycle. When the pointer
    // stays inside the terminal those events also propagate through
    // `this.container`, so the listeners above catch them. But if the
    // pointer LEAVES the terminal (drag out, release outside), the target
    // is no longer under `this.container` and only the document-level
    // listener would need mirroring. We accept the minor drop-off there --
    // most drags start and end inside the pane, and mirroring outside-row
    // coords would require guessing which row the pointer "would have"
    // hit. Documented as a known limitation.

    this.rtlMouseTeardown = () => {
      for (const ev of events) {
        el.removeEventListener(ev, forward as EventListener, true);
      }
    };
  }

  /**
   * v0.4.4 (RTL Approach C): give every VISIBLE row div under `.xterm-rows`
   * an explicit `dir` computed from its text by detectDirection (mixed→RTL,
   * pure-Latin→LTR), instead of the old `dir="auto"` (which used the browser's
   * "first strong char wins" rule and mis-rendered mixed lines starting with
   * Latin). xterm.js's DOM renderer recycles its row divs as the buffer
   * scrolls and rewrites their text in place, so a MutationObserver
   * (childList + characterData) re-triggers the pass; it is coalesced to one
   * run per animation frame and a per-row text cache skips unchanged rows.
   * Only visible rows carry DOM nodes, so scrollback size is irrelevant.
   */
  ensureDirObserver(): void {
    // Only relevant in auto-per-line mode AND when we built with the
    // DOM renderer. In any other mode, drop any existing observer.
    if (!usesRowDir(this.rtlModeRendered)) {
      if (this.dirObserver) {
        this.dirObserver.disconnect();
        this.dirObserver = null;
      }
      return;
    }
    if (this.dirObserver) return;

    const rowsHost = this.container.querySelector(".xterm-rows") as HTMLElement | null;
    if (!rowsHost) {
      // Renderer not mounted yet — retry on the next animation frame.
      requestAnimationFrame(() => this.ensureDirObserver());
      return;
    }
    // The rows host stays LTR so the grid geometry (column origin) is stable;
    // only the per-row paragraph direction flips.
    rowsHost.setAttribute("dir", "ltr");
    this.applyRowDirections(true);
    const obs = new MutationObserver(() => this.scheduleRowDirections());
    obs.observe(rowsHost, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    this.dirObserver = obs;
  }

  /** Claude visual-order RTL: fold a title report into the per-pane
   *  "TUI owns bidi" state. On a transition, force a full direction
   *  re-pass so rows already on screen flip immediately (Claude's banner
   *  prints before the title-driven state lands, and shell scrollback is
   *  still visible when Claude exits). */
  private onTitleChanged(title: string): void {
    // 2026-08-19: report EVERY title, not only the ones that change the state.
    // Nothing else could answer "does zellij forward a title at all, and does
    // it ever contain 'claude'" — the absence of transitions in the log was
    // equally consistent with "no titles" and "titles that never matched".
    //
    // Rule #1: a title can be Claude's auto topic name, i.e. derived from
    // conversation content. Whether it matched and how long it was are
    // metadata; the text itself is never logged and a length cannot
    // reconstruct it.
    termLog.info(
      `title-seen pane=${this.paneId} match=${/claude/i.test(title) ? 1 : 0} ` +
        `len=${title.length}`,
    );
    const next = nextTuiOwnsBidi(this.tuiOwnsBidi, title);
    if (next === this.tuiOwnsBidi) return;
    this.tuiOwnsBidi = next;
    // Metadata only (Rule #1): titles can derive from conversation content
    // (Claude's auto topic titles) — never log the title itself.
    termLog.info(`tui-owns-bidi ${next ? "on" : "off"} pane=${this.paneId}`);
    this.applyRowDirections(true);
  }

  /** Coalesce a burst of row mutations into one direction pass per frame. */
  private scheduleRowDirections(): void {
    if (this.dirRafId != null) return;
    this.dirRafId = requestAnimationFrame(() => {
      this.dirRafId = null;
      this.applyRowDirections(false);
    });
  }

  /**
   * Set `dir` on each visible row from its text.
   *
   * `force` recomputes every row (used on first attach and when the setting
   * toggles); otherwise the per-row cache skips rows whose text is unchanged
   * AND whose *neighbors* haven't shifted the block classification. When any
   * row's text changes we run the full block-aware pass ({@link
   * detectRowDirections}) — so a Hebrew cell landing in a table drags the
   * whole block RTL, and adding a border row that groups earlier content rows
   * into a new block re-flows their direction too.
   *
   * `detectDirection` (single-row) is retained above only as a fallback for
   * `auto_direction=false` (see below).
   */
  applyRowDirections(force: boolean): void {
    const mode = this.rtlModeRendered;
    if (!usesRowDir(mode)) return;
    const rowsHost = this.container.querySelector(".xterm-rows") as HTMLElement | null;
    if (!rowsHost) return;
    const auto = this.rtl.autoDirection;
    const children = Array.from(rowsHost.children) as HTMLElement[];
    const texts = children.map((el) => el.textContent ?? "");

    // Fast path: if not forced AND every row text is unchanged since last
    // pass, we can skip the whole recompute. Block classification depends on
    // ALL rows, so a per-row cache with per-row skip (as before) would race
    // with block-boundary shifts. Cheap array-equality check instead.
    if (!force) {
      let allSame = children.length > 0;
      for (let i = 0; i < children.length; i++) {
        if (this.dirCache.get(children[i]) !== texts[i]) { allSame = false; break; }
      }
      if (allSame) return;
    }

    // tuiOwnsBidi: the foreground TUI (Claude) already emitted visual-order
    // RTL — render rows plain LTR so we don't bidi it a second time.
    //
    // 2026-08-19: which direction rule applies comes from the PANE'S PROFILE,
    // never from what is running inside the pane. This briefly read
    // `this.tuiOwnsBidi` instead — and because the OSC title propagates over
    // SSH, that fired on remote panes and broke a path that was working. See
    // the note on `directionPolicy` above.
    // 2026-08-20: `normaliseIncomingToLogical` panes take the NORMAL path. The
    // buffer they hold is logical, because flushPending converted it on the way
    // in — so they need exactly what a remote pane needs: a real per-row `dir`
    // and the browser's own bidi. Forcing "ltr" here is what pinned Hebrew to
    // the left edge, since right placement can only come from dir="rtl".
    const suppress = this.bidiOwnedByTui && !this.normaliseIncomingToLogical;
    const dirs = rowDirections(mode, texts, {
      auto,
      suppress,
      dominance: this.rtl.directionPolicy === "tui_dominance",
    });

    // 2026-08-19: `dir` alone cannot do what bidiOwnedByTui promises.
    //
    // `dir` sets a row's PARAGRAPH direction. It does not stop the browser
    // reordering an RTL run inside that paragraph — UAX #9 rule L2 reverses
    // every run at level >= 1 whichever way the paragraph faces. So forcing
    // dir="ltr" over Claude's already-visual output still double-reordered it,
    // and the setting looked like it did nothing. `unicode-bidi: bidi-override`
    // is the only thing that suppresses reordering outright: it makes every
    // character strong in `direction`, so the bytes are painted in the order
    // they arrived — which is exactly what a pre-reordered stream needs, and
    // exactly what the WebGL renderer gives for free in rtl_mode="off".
    // …and for the same reason, a normalised pane must NOT get the override:
    // its bytes are logical now, so painting them verbatim would show them
    // reversed. The override is only right while the buffer is still visual.
    // 2026-08-23: `force_rtl` must never take the override. `rowDirections`
    // already ignores `suppress` there and returns "rtl" for every row, so
    // leaving this on `suppress` alone would pair dir="rtl" with
    // bidi-override on a pane whose buffer is logical — bytes painted
    // verbatim right-to-left, i.e. every Hebrew word reversed. The override
    // is only ever right while the buffer is still VISUAL order, which is a
    // local/ConPTY condition and one `auto_per_line` alone can reach.
    const override = suppress && mode === "auto_per_line";
    for (let i = 0; i < children.length; i++) {
      const el = children[i];
      this.dirCache.set(el, texts[i]);
      const dir = dirs[i];
      if (el.getAttribute("dir") !== dir) el.setAttribute("dir", dir);
      const want = override ? "bidi-override" : "";
      if (el.style.unicodeBidi !== want) el.style.unicodeBidi = want;
    }
    this.logDirections(dirs, texts);
  }

  /**
   * 2026-08-19: report what the direction pass decided, so "the letters are
   * reversed" stops being a judgement call about a screenshot.
   *
   * Screenshots cannot settle this on their own: visual order cannot be
   * written down in a chat message, because the message is itself reordered by
   * whatever renders it. Counts and a direction can.
   *
   * Rule #1 — METADATA ONLY. What goes out is how many strong characters of
   * each script a row held and which way it resolved. The characters
   * themselves never appear, and a count cannot reconstruct them.
   *
   * Quiet by construction: this pass runs once per animation frame during
   * output, so it logs only when the resolved direction VECTOR changes, which
   * is the only time it says anything new.
   */
  private logDirections(dirs: ("ltr" | "rtl")[], texts: string[]): void {
    const sig = dirs.join("");
    if (sig === this.lastDirSignature) return;
    this.lastDirSignature = sig;
    let rtlRows = 0;
    for (const d of dirs) if (d === "rtl") rtlRows++;
    // One worked example per direction makes the counts actionable: it shows
    // WHICH ratio produced the answer, not just the tally.
    const sample = (want: "ltr" | "rtl"): string => {
      for (let i = 0; i < dirs.length; i++) {
        if (dirs[i] !== want) continue;
        const { rtl, ltr } = strongCounts(texts[i]);
        if (rtl === 0 && ltr === 0) continue; // blank row, tells us nothing
        return `#${i}(r${rtl}/l${ltr})`;
      }
      return "-";
    };
    // `tui=` is the EFFECTIVE answer — what actually gated this pass — not the
    // title-derived flag. Reporting the raw flag was actively misleading: the
    // 2026-08-19 log showed `tui=0` moments after `tui-signal explicit=1`,
    // which reads as "the signal did nothing" when in fact the signal had won
    // and a different gate (the per-profile setting) was the one saying no.
    // The three inputs are broken out so the next diagnosis is one line, not
    // an inference.
    termLog.info(
      `rtl-dirs pane=${this.paneId} profile=${this.profile} ` +
        `policy=${this.rtl.directionPolicy} tui=${this.bidiOwnedByTui ? 1 : 0} ` +
        `(explicit=${this.tuiExplicit === null ? "-" : this.tuiExplicit ? 1 : 0} ` +
        `title=${this.tuiOwnsBidi ? 1 : 0} setting=${this.rtl.tuiOwnsBidi ? 1 : 0}) ` +
        `rows=${dirs.length} rtl=${rtlRows} ltr=${dirs.length - rtlRows} ` +
        `firstRtl=${sample("rtl")} firstLtr=${sample("ltr")}`,
    );
  }

  attach(sessionId: string) {
    this.detach();
    this.sessionId = sessionId;
    this.dataDisposable = this.term.onData((data) => {
      let out = data;
      // Phase HH: on an RTL line, the visual Left/Right arrows map to the
      // opposite logical direction, so mirror them. Only the 4 horizontal
      // cursor-key sequences are considered, and only when the cursor's
      // line is predominantly RTL — LTR lines pass through untouched.
      if (
        this.rtl.mirrorArrowsRtl &&
        (data === "\x1b[C" ||
          data === "\x1b[D" ||
          data === "\x1bOC" ||
          data === "\x1bOD") &&
        this.isCurrentLineRtl()
      ) {
        out = swapArrowSeq(data);
      }
      if (this.sessionId)
        invoke("pty_write", { sessionId: this.sessionId, data: out }).catch(
          (err) => console.error("pty_write failed", err)
        );
    });
    // Phase 25.C: force a pty_resize on attach so tmux gets the
    // current dimensions immediately, even on a reconnect where
    // xterm.js's cols/rows happen to match the previous session.
    this.fitAndResize(true);
    // v0.4.4-beta.2: clear any stale mouse-tracking state inherited from a
    // previous session on this instance (a reconnect where an app left SGR
    // mouse mode on). Fresh instances start clean; this is the reconnect
    // safety net. Gated by the Settings toggle.
    if (g_autoResetOnConnect) this.resetMouseModes();
  }

  /** v0.4.4-beta.2: tell xterm.js to disable every mouse-tracking mode.
   *  Writes DECRST sequences to the DISPLAY (not the PTY), so xterm stops
   *  emitting mouse events even if the app that turned them on is gone. */
  resetMouseModes(): void {
    try {
      this.term.write(MOUSE_DISABLE_SEQ);
    } catch (e) {
      console.warn("resetMouseModes failed", e);
    }
  }

  /** v0.4.4-beta.2: manual "Reset terminal" — disable mouse tracking +
   *  reset text attributes (SGR). Used by the Ctrl+Alt+R command. Does NOT
   *  clear the screen/scrollback (no RIS), so it's safe to run any time. */
  resetTerminal(): void {
    try {
      this.term.write(MOUSE_DISABLE_SEQ + "\x1b[0m");
    } catch (e) {
      console.warn("resetTerminal failed", e);
    }
  }

  /** Phase HH: is the terminal line under the cursor predominantly RTL?
   *  Reads the live buffer line at the absolute cursor row. Best-effort —
   *  any xterm API hiccup falls back to "not RTL" (no mirroring). */
  private isCurrentLineRtl(): boolean {
    try {
      // Claude visual-order RTL: the TUI does its own line editing over
      // visual-order text — ymux must not second-guess its arrows.
      //
      // 2026-08-20: STILL CORRECT after `normaliseIncomingToLogical`, and the
      // reason is worth spelling out because the obvious reading is now wrong.
      // Normalising changes what OUR buffer holds; it does not change what
      // CLAUDE thinks it holds. Claude still edits its line in visual order and
      // still moves its own cursor in those columns, so mirroring the arrows we
      // send it would fight it, exactly as before. This bails on the signal,
      // not on the buffer.
      if (this.bidiOwnedByTui) return false;
      // 2026-08-23: `force_rtl` renders every row dir="rtl", so the arrow
      // mirroring has to agree with the paint or Left/Right fight what the
      // user sees. No buffer read needed — the answer cannot be anything else.
      if (this.rtlModeRendered === "force_rtl") return true;
      const buf = this.term.buffer.active;
      const line = buf.getLine(buf.baseY + buf.cursorY);
      if (!line) return false;
      // v0.4.4: use the SAME rule as the per-line display direction
      // (detectDirection) so the caret/arrow behavior matches what the user
      // sees — a line rendered RTL also gets its Left/Right arrows mirrored.
      // Candidate fix for the PARKED "RTL caret" item (verify live).
      return detectDirection(line.translateToString(true)) === "rtl";
    } catch {
      return false;
    }
  }

  detach() {
    this.dataDisposable?.dispose();
    this.dataDisposable = null;
    this.sessionId = null;
  }

  fitAndResize(force = false) {
    const prevCols = this.term.cols;
    const prevRows = this.term.rows;
    try {
      this.fit.fit();
    } catch {}
    const changed = this.term.cols !== prevCols || this.term.rows !== prevRows;
    // Phase 25.C: when `force` is set (the trailing settle fit after
    // a resize storm, or attach() on a reconnect), ALWAYS push
    // pty_resize to the remote even if xterm.js's own cols/rows
    // didn't change since the last fit. An intermediate fit during a
    // fast drag can update xterm.js to the final size and fire a
    // pty_resize that races / never reaches tmux; the settle fit
    // would then see `changed=false` and skip pty_resize, leaving
    // tmux painting at the stale width. Forcing the resize
    // guarantees tmux is told the final dimensions.
    if (this.sessionId && (changed || force)) {
      invoke("pty_resize", {
        sessionId: this.sessionId,
        cols: this.term.cols,
        rows: this.term.rows,
      }).catch(() => {});
    }
    // Phase 25: force a repaint after a real dimension change so the
    // renderer picks up the new grid metrics.
    //
    // NOTE: Phase 23.E also called `webglAddon.clearTextureAtlas()`
    // here. That turned out to be the cause of the "terminal goes
    // blank after resizing once a conversation has filled the
    // scrollback" bug — wiping the glyph atlas mid-resize, with a
    // large reflowed scrollback, could leave the WebGL canvas unable
    // to re-rasterize and stuck blank. The atlas is invalidated
    // internally by the WebGL addon on resize anyway, so the manual
    // call was both redundant and harmful. Removed. A plain
    // `term.refresh()` is enough to repaint the viewport, and the
    // onContextLoss handler above covers the case where WebGL does
    // die.
    // Phase 25.C: force=true also triggers a repaint so the settled
    // fit guarantees a fresh viewport even when grid metrics didn't
    // change.
    if (changed || force) {
      try {
        this.term.refresh(0, this.term.rows - 1);
      } catch {}
    }
  }

  /** One-shot guard for {@link scheduleInitialFontMeasure}. */
  private fontMeasured = false;

  private cs(): NonNullable<
    NonNullable<XtermInternals["_core"]>["_charSizeService"]
  > | undefined {
    return (this.term as unknown as XtermInternals)._core?._charSizeService;
  }

  /** Diagnostic: log this pane's measured cell + applied font (Rule #1:
   *  metrics only). Public so the module-level setTerminalFont can trace the
   *  manual font-swap path. */
  logFontSwap(label: string): void {
    const cs = this.cs();
    const fam = String(this.term.options.fontFamily ?? "").slice(0, 40);
    void invoke("diag_log", {
      level: "info",
      msg: `[font-swap] ${label} pane=${this.paneId} charSvc=${cs?.width}x${cs?.height} size=${this.term.options.fontSize} fam=${JSON.stringify(fam)}`,
    }).catch(() => {});
  }

  /** Apply a font family + size to THIS terminal exactly the way
   *  setTerminalFont does per-instance (option set + fit + refresh). */
  private applyFontOnce(family: string, px: number): void {
    this.term.options.fontFamily = family;
    this.term.options.fontSize = px;
    this.fitAndResize();
    try {
      this.term.refresh(0, this.term.rows - 1);
    } catch {}
  }

  /**
   * Re-measure the cell by REPLICATING the manual Settings font-swap — the
   * only thing proven to un-stick a bad measurement. Proven from the debug
   * log: a fresh pane on the bitmap font `Courier 10,12,15 (120)` measures the
   * cell "cold" at 9.44x11 (half-height → compressed). Re-applying the SAME
   * family, or calling CharSizeService.measure() directly, leaves it at
   * 9.44x11. But applying a DIFFERENT scalable family first and THEN restoring
   * the user's family lands on the correct 12.0x23 and stays there — the
   * intermediate scalable application forces the browser to re-resolve the
   * cached cell. The final family is always the user's own setting
   * (`g_fontFamily`); the intermediate is a throwaway unsticker, so whatever
   * the user's family resolves to is unchanged from the manual swap-back.
   *
   * Timing matters: the working manual swap had the intermediate font actually
   * painted before the swap-back, so we restore the real family after a DOUBLE
   * requestAnimationFrame (one full paint of the intermediate), not the next
   * frame. The self-verify loop in scheduleInitialFontMeasure retries if a
   * single pass doesn't stick.
   */
  remeasureFont(): void {
    const real =
      g_fontFamily ?? String(this.term.options.fontFamily ?? "monospace");
    const px = g_fontSizePx ?? Number(this.term.options.fontSize ?? 14);
    // Always-present Windows system scalable fonts → resolve synchronously,
    // no web-font load race. Only needs to differ from `real` to trigger the
    // re-resolution.
    const unsticker = '"Consolas", "Courier New", monospace';
    const intermediate = real === unsticker ? "monospace" : unsticker;
    this.applyFontOnce(intermediate, px);
    requestAnimationFrame(() => {
      if (!g_terminals.has(this)) return;
      // Second frame: the intermediate has now painted at least once.
      requestAnimationFrame(() => {
        if (!g_terminals.has(this)) return;
        this.applyFontOnce(real, px);
        const cs = this.cs();
        void invoke("diag_log", {
          level: "info",
          msg: `[font-fix] pane=${this.paneId} afterSwap charSvc=${cs?.width}x${cs?.height} size=${this.term.options.fontSize}`,
        }).catch(() => {});
      });
    });
  }

  /** True if the cell isn't measured yet, or is "compressed" — height well
   *  under the font size (the bug). A correct cell is ≈ `fontSize * 1.15+`. */
  private cellNeedsRemeasure(): boolean {
    const h = this.cs()?.height;
    if (typeof h !== "number") return true;
    const size = Number(this.term.options.fontSize ?? 14);
    return h < size * 0.9;
  }

  /**
   * Font-init fix: once this pane's container is in the DOM, re-measure the
   * font (see {@link remeasureFont}). Runs once when connected, then a few
   * more times over ~1s to beat the web-font load race — the intended face
   * may still be loading at first paint, and re-measuring after it lands
   * pins the correct cell size (the same thing re-picking the font in
   * Settings does). Extra passes are cheap and idempotent; the
   * `g_terminals` check stops them if the pane is disposed meanwhile.
   */
  private scheduleInitialFontMeasure(): void {
    const fonts =
      typeof document !== "undefined" ? document.fonts : undefined;

    // Force the real candidate faces to actually LOAD — document.fonts.ready
    // alone doesn't, since a face isn't "pending" until requested. Skips CSS
    // generics and malformed comma-split junk. Resolving means the intended
    // face is loaded, so the next measure() lands on it.
    const loadFaces = (): Promise<unknown> => {
      if (!fonts || typeof fonts.load !== "function") return Promise.resolve();
      const size = Number(this.term.options.fontSize ?? 14);
      const families = String(this.term.options.fontFamily ?? "")
        .split(",")
        .map((f) => f.trim().replace(/^["']|["']$/g, ""))
        .filter(
          (f) =>
            f.length > 0 &&
            !/^(monospace|serif|sans-serif|ui-monospace|system-ui|cursive|fantasy)$/i.test(
              f,
            ) &&
            /^[\w .-]+$/.test(f),
        );
      return Promise.allSettled(
        families.map((f) => fonts.load(`${size}px "${f}"`)),
      );
    };

    // Self-verifying loop: call xterm's own measure() until the cell is no
    // longer compressed, or a ceiling. Because measure() actually re-measures
    // and we CHECK the result (cellNeedsRemeasure via charSvc), this can't
    // silently no-op and it naturally waits out the font-load race. Stops the
    // instant the cell is correct.
    let attempts = 0;
    const MAX_ATTEMPTS = 15; // ~3s at 200ms
    const step = () => {
      if (!g_terminals.has(this) || !this.container.isConnected) return;
      if (!this.cellNeedsRemeasure()) return; // correct → done
      if (attempts >= MAX_ATTEMPTS) return;
      attempts += 1;
      this.remeasureFont();
      window.setTimeout(step, 200);
    };

    const run = () => {
      if (this.fontMeasured || !g_terminals.has(this)) return;
      if (!this.container.isConnected) {
        // PaneView hasn't appended the container to its slot yet.
        requestAnimationFrame(run);
        return;
      }
      this.fontMeasured = true;
      step();
      void loadFaces().finally(() => {
        if (g_terminals.has(this) && this.cellNeedsRemeasure()) step();
      });
    };
    requestAnimationFrame(run);
  }

  writeData(data: string) {
    // Phase 35: queue and coalesce. Merging chunks before the reorder
    // pipeline is also more correct than per-chunk - a chunk boundary
    // that splits a line or escape sequence now gets reassembled before
    // reorderRtlForDisplay sees it.
    this.pendingChunks.push(data);
    if (this.flushRafId === null) {
      this.flushRafId = requestAnimationFrame(() => this.flushPending());
    }
  }

  private flushPending() {
    this.flushRafId = null;
    if (this.pendingChunks.length === 0) return;
    const merged = this.pendingChunks.join("");
    this.pendingChunks = [];
    // Phase 62.C (J.1): record (once, metadata only - Rule #1) whether
    // OSC 8 hyperlink sequences (ESC ] 8 ;) actually reach this pane. If
    // the debug.log never shows this line while Claude prints file links,
    // the sequences are being stripped upstream (or Claude isn't emitting
    // them) - not a linkHandler bug.
    if (!this.oscHyperlinkLogged && merged.includes("]8;")) {
      this.oscHyperlinkLogged = true;
      void invoke("diag_log", {
        level: "info",
        msg: `OSC8 hyperlink sequence detected in pane ${this.paneId}`,
      }).catch(() => {});
    }
    // The reorder pipeline keys off the LIVE rtl mode for this pane's
    // a settings change takes effect on the very next flush - no
    // need to wait for a new pane. tuiOwnsBidi bypasses the reorder:
    // Claude's output is ALREADY visual order; reordering it again
    // scrambles it.
    // Keyed on the RENDERED mode, not the raw setting. They are kept equal by
    // `applyRenderer`; reading the setting directly here is what let the two
    // halves of the pipeline disagree and produce a pane with no bidi at all.
    if (this.normaliseIncomingToLogical) {
      // Visual -> logical on the way in, so everything downstream — the row
      // direction pass, the browser's bidi, selection, copy — sees the same
      // logical text a remote pane has always delivered.
      //
      // The STREAM variant, not the clipboard one: `merged` is dense with CSI
      // and OSC, and the clipboard variant is escape-blind by design.
      //
      // KNOWN LIMIT, inherited: `merged` is a rAF boundary, not a line
      // boundary, so a slow repaint can hand us a mid-line fragment that
      // resolves its own base direction. Same class of limit `bidi.ts` already
      // documents for the forward pass.
      this.term.write(visualToLogicalStream(merged));
    } else if (this.rtlModeRendered === "bidi_reorder" && !this.bidiOwnedByTui) {
      this.term.write(reorderRtlForDisplay(merged));
    } else {
      this.term.write(merged);
    }
  }

  notice(msg: string) {
    this.term.writeln(`\r\n\x1b[33m${msg}\x1b[0m`);
  }

  focus() {
    this.term.focus();
  }

  dispose() {
    // Phase 62.A (item E): close the right-click menu if it's open over
    // this terminal - its actions reference this.term.
    dismissTerminalMenu();
    // Phase 35: flush any queued PTY chunks synchronously before the
    // rAF can fire, so the last bytes aren't lost when a pane closes
    // mid-stream. Then cancel the pending frame.
    if (this.flushRafId != null) {
      cancelAnimationFrame(this.flushRafId);
      this.flushRafId = null;
    }
    try {
      this.flushPending();
    } catch {}
    if (this.resizeRafId != null) {
      cancelAnimationFrame(this.resizeRafId);
      this.resizeRafId = null;
    }
    // Phase 25.B: cancel the trailing settle timer so a freed
    // terminal doesn't fire a fit after disposal.
    if (this.resizeSettleTimer != null) {
      clearTimeout(this.resizeSettleTimer);
      this.resizeSettleTimer = null;
    }
    this.ro?.disconnect();
    this.ro = null;
    this.fileLinkProvider?.dispose();
    this.fileLinkProvider = null;
    this.dirObserver?.disconnect();
    this.dirObserver = null;
    // v0.4.4-beta.4 (RTL mouse fix): tear down the capture-phase listener
    // so a disposed pane doesn't keep intercepting document/element mouse
    // events for the rest of the app's lifetime.
    this.rtlMouseTeardown?.();
    this.rtlMouseTeardown = null;
    // v0.4.4: cancel a pending per-line direction pass so a freed terminal
    // doesn't touch detached DOM after disposal.
    if (this.dirRafId != null) {
      cancelAnimationFrame(this.dirRafId);
      this.dirRafId = null;
    }
    this.detach();
    g_terminals.delete(this);
    // Phase 25: release the WebGL addon BEFORE term.dispose() so we
    // can swallow any teardown error specific to the GPU canvas
    // without the rest of dispose() being skipped. Also reads the
    // field, which keeps tsc happy now that the in-flight
    // clearTextureAtlas reader is gone (Phase 23.E).
    try {
      this.webglAddon?.dispose();
    } catch {}
    this.webglAddon = null;
    this.term.dispose();
    if (this.container.parentElement) {
      this.container.parentElement.removeChild(this.container);
    }
  }
}
