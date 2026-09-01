// Phase 9.A: settings type mirror + helpers (load/save/apply CSS vars).
// The Rust backend owns the canonical schema in src-tauri/src/settings.rs;
// this file is the typed mirror used by the frontend.

import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "./logger";

/** Rule #9: the unified logger, not console.*. */
const rtlLog = createLogger("SETTINGS");
import {
  setTerminalFont,
  setTerminalTheme,
  setRtlProfiles,
  setAutoResetOnConnect,
  type RtlMode,
  type RtlProfileSettings,
  type DirectionPolicy,
} from "./terminalInstance";
import type { RtlProfileKind } from "./types";
import type { ITheme } from "@xterm/xterm";

export interface AnsiPalette {
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  bright_black: string;
  bright_red: string;
  bright_green: string;
  bright_yellow: string;
  bright_blue: string;
  bright_magenta: string;
  bright_cyan: string;
  bright_white: string;
}

export interface Theme {
  preset: string;
  accent: string;
  background: string;
  surface: string;
  border: string;
  text_primary: string;
  text_secondary: string;
  success: string;
  warning: string;
  error: string;
  ansi: AnsiPalette;
}

export interface FontSettings {
  ui_family: string;
  ui_size_pt: number;
  terminal_family: string;
  terminal_size_pt: number;
  /** Stretch goal: load a web font sheet (e.g. Google Fonts) at runtime. */
  web_font_url?: string | null;
}

export interface TerminalSettings {
  /** Phase 15.A: how to render Hebrew / Arabic. */
  rtl_mode?: "auto_per_line" | "force_rtl" | "bidi_reorder" | "off";
  /** Phase tmux-conf: when true (default), ymux launches tmux with
   *  `-f ~/.ymux/tmux.conf` for sane scrollback / mouse behaviour.
   *  Set false to fall back to the user's own ~/.tmux.conf. */
  use_ymux_tmux_config?: boolean;
  /** Phase HH: mirror Left/Right arrows on RTL (Hebrew/Arabic) lines.
   *  Only active when the cursor's line is RTL; default true. */
  mirror_arrows_rtl?: boolean;
  /** v0.4.4 (RTL Approach C): auto-flip each terminal line's direction
   *  (mixed/pure-Hebrew → RTL, pure-Latin → LTR). Only affects the
   *  `auto_per_line` rtl_mode. Default true. */
  auto_direction?: boolean;
  /** 2026-08-18: force LTR while a self-bidi TUI holds the pane. Default false. */
  tui_owns_bidi?: boolean;
  /** 2026-08-19: the four RTL knobs above, split per pane class. The flat
   *  fields are deprecated and kept only so a pre-split settings.json still
   *  loads; the backend seeds this from them on first load. Optional here
   *  because that migration is what fills it in. */
  rtl?: RtlProfilesSettings;
  /** v0.4.4-beta.2: clear stale mouse-tracking modes on connect (fixes the
   *  `\e[<..M` mouse-escape leak from an unclean vim/fzf/less exit).
   *  Default true. */
  auto_reset_on_connect?: boolean;
}

/** One class of pane's RTL knobs. Mirrors `RtlProfile` in settings.rs. */
export interface RtlProfileFields {
  rtl_mode?: "auto_per_line" | "force_rtl" | "bidi_reorder" | "off";
  auto_direction?: boolean;
  mirror_arrows_rtl?: boolean;
  tui_owns_bidi?: boolean;
  /** 2026-08-19: which rule sets a row's paragraph direction. Absent reads as
   *  "any_rtl", the pre-2026-08-19 rule — see `directionPolicy` in
   *  terminalInstance.ts for why this is per pane class and not per TUI. */
  direction_policy?: DirectionPolicy;
}

/** `local` = native Windows ConPTY panes; `remote` = anything with a POSIX
 *  shell behind it, which includes WSL. See `profileFor` in types.ts. */
export interface RtlProfilesSettings {
  local?: RtlProfileFields;
  remote?: RtlProfileFields;
}

export interface HooksSettings {
  /** Phase 18.1: which PreToolUse matcher to install when setup-hooks
   *  runs. "restrictive" (default) only catches risky tools; "all"
   *  matches `.*` (every tool surfaces a ymux card); "custom" leaves
   *  whatever the user hand-edited and never overwrites. */
  matcher_mode?: "restrictive" | "all" | "custom";
  /** Phase 66 (66.D): master switch for the 3-state policy engine
   *  (auto/gate/block) in the desktop feed.push handler. Default true. */
  policy_enabled?: boolean;
  /** Phase 66 (66.B): auto-run `ymux setup-hooks` on the remote during
   *  bootstrap so a fresh server starts surfacing cards. Default true. */
  auto_install?: boolean;
  /** Phase 66.F: user-defined BLOCK patterns (one per entry), merged into
   *  the built-in list by the desktop policy engine. Substring match,
   *  case/whitespace-insensitive, per chained segment. Desktop-side only. */
  custom_block?: string[];
  /** Phase 66.F: user-defined GATE patterns (see custom_block). Block
   *  beats gate. */
  custom_gate?: string[];
}

// beta.3: canonical Claude Code hook types. Wire strings are kebab-case
// (round-trips with rpc_server.rs subkind handling).
export type HookType =
  | "pre-tool-use"
  | "notification"
  | "stop"
  | "session-end"
  | "post-tool-use"
  | "subagent-stop"
  | "user-prompt-submit"
  | "pre-compact"
  | "session-start";

// beta.3: per-hook-type "processed?" + "play sound?" toggles + master.
// Mirrors the Rust `HookSettings` struct in settings.rs. Ts-rs also emits
// a binding under app/src/bindings/HookSettings.ts; this hand-mirrored
// type is what the app imports because HashSet<T> serialises as an array
// and we want a plain TS shape for the UI.
export interface HookNotificationSettings {
  enabled_types: HookType[];
  sound_types: HookType[];
  sound_master: boolean;
}

export const INTERACTIVE_HOOKS: HookType[] = [
  "pre-tool-use",
  "notification",
  "stop",
  "session-end",
];

export const OBSERVABILITY_HOOKS: HookType[] = [
  "post-tool-use",
  "subagent-stop",
  "user-prompt-submit",
  "pre-compact",
  "session-start",
];

export const DEFAULT_HOOK_NOTIFICATIONS: HookNotificationSettings = {
  enabled_types: [...INTERACTIVE_HOOKS],
  sound_types: ["pre-tool-use", "notification", "stop"],
  sound_master: true,
};

export interface NotificationSettings {
  toast_enabled: boolean;
  /** Phase 66 (KK): per-event toast toggles. Defaults: session start/end
   *  OFF; stop / notification / gate / block ON. */
  toast_session_start?: boolean;
  toast_session_end?: boolean;
  toast_stop?: boolean;
  toast_notification?: boolean;
  toast_gate?: boolean;
  toast_block?: boolean;
  /** cmux-A A1: pulse a pane's border on OSC 9/99/777. Default true. */
  pane_pulse_on_activity?: boolean;
}

export interface UpdatesSettings {
  check_on_startup: boolean;
  manifest_url?: string | null;
  last_check_iso?: string | null;
  last_seen_version?: string | null;
  skipped_versions: string[];
  remind_after_iso?: string | null;
  // Phase 71: "stable" | "beta".
  channel: string;
}

// Phase 87: the shortcut schema + defaults live in `shortcuts.ts`, which
// has NO imports — this file pulls in the Tauri bridge and the terminal,
// so anything defined here is unreachable from a plain `node --test`.
// Re-exported so existing `from "./settings"` call sites keep working.
// `export ... from` creates no LOCAL binding, so the Settings interface
// below needs its own import of the type it references.
import type { ShortcutsSettings as ShortcutsSettingsLocal } from "./shortcuts";
export {
  DEFAULT_SHORTCUTS,
  SHORTCUT_ACTION_IDS,
  type ShortcutActionId,
  type ShortcutsSettings,
} from "./shortcuts";

export interface ClaudeSettings {
  auto_summarize_on_stop: boolean;
  summary_history_count: number;
  summary_prompt: string;
}

export interface HooksUpdatesSettings {
  show_banners: boolean;
  /** Map of agent_id → list of dismissed version strings. */
  dismissed: Record<string, string[]>;
}

export const DEFAULT_HOOKS_UPDATES: HooksUpdatesSettings = {
  show_banners: true,
  dismissed: {},
};

export interface HooksOutdatedInfo {
  workspace_id: string;
  pane_id: string;
  agent: string;
  current?: string | null;
  latest: string;
}

export const DEFAULT_CLAUDE_SETTINGS: ClaudeSettings = {
  auto_summarize_on_stop: false,
  summary_history_count: 10,
  summary_prompt:
    "Summarize the last {N} exchanges in 2-3 sentences in the same language the conversation used.",
};

// Phase 78: Claude subscription-usage indicator (mirrors the Rust
// ClaudeUsageSettings struct in app/src-tauri/src/settings.rs).
export interface ClaudeUsageSettings {
  show_top_indicator: boolean;
  display_mode: "percent" | "bar" | string;
  auto_refresh_minutes: number;
}

export const DEFAULT_CLAUDE_USAGE_SETTINGS: ClaudeUsageSettings = {
  show_top_indicator: true,
  display_mode: "percent",
  auto_refresh_minutes: 10,
};

// BRIEF: Briefing-card triggers (mirrors the Rust BriefOptions struct in
// app/src-tauri/src/settings.rs). Everything opt-in; the manual shortcut
// works regardless of these.
export interface BriefSettings {
  entry_card_on_return: boolean;
  entry_card_on_idle: boolean;
  absence_minutes: number;
  idle_minutes: number;
}

export const DEFAULT_BRIEF_SETTINGS: BriefSettings = {
  entry_card_on_return: false,
  entry_card_on_idle: false,
  absence_minutes: 30,
  idle_minutes: 15,
};

export interface I18nSettings {
  language: "en" | "he" | "ar" | "ru" | string;
  direction: "auto" | "ltr" | "rtl" | string;
}

// Phase 58: speech-to-text settings (hand-mirrored from the Rust
// SttSettings struct in app/src-tauri/src/settings.rs). When the Rust
// side regenerates app/src/bindings/SttSettings.ts via ts-rs the
// types should stay structurally identical; this file is what the
// rest of the frontend imports historically, so we add the type
// here too.
export interface SttSettings {
  enabled: boolean;
  backend: "webspeech" | "local";
  local_endpoint: string | null;
  language: string;
  push_to_talk_hotkey: string;
}

export interface Settings {
  version: number;
  theme: Theme;
  font: FontSettings;
  terminal: TerminalSettings;
  hooks: HooksSettings;
  notifications: NotificationSettings;
  // beta.3: per-hook-type enable + sound toggles (Hooks & Notifications card).
  hook_notifications?: HookNotificationSettings;
  updates: UpdatesSettings;
  i18n: I18nSettings;
  shortcuts?: ShortcutsSettingsLocal;
  claude?: ClaudeSettings;
  // Phase 78: Claude usage % indicator display + auto-refresh.
  claude_usage?: ClaudeUsageSettings;
  // BRIEF: Briefing-card triggers. Backend defaults everything to opt-out
  // via serde(default); this mirror MUST round-trip through SettingsModal
  // or settings_save wipes the group (the terminal.rtl incident).
  brief?: BriefSettings;
  hooks_updates?: HooksUpdatesSettings;
  // Phase 41: auto-connect a background SSH session on workspace select.
  // Backend defaults to true; always serialized.
  auto_connect_on_workspace_select?: boolean;
  // Phase 80: re-attach the active workspace's SSH panes to their tmux
  // sessions at app start. Backend defaults to FALSE — opt-in, because it
  // makes startup do network work (one handshake per restored pane).
  restore_sessions_on_start?: boolean;
  // Phase 80.1: file manager reopens at the last directory each column was
  // showing, per workspace, instead of $HOME. Backend defaults to FALSE.
  file_manager_remember_path?: boolean;
  // Phase 49-C: optional auto-delete of empty workspaces older than N
  // days. null/undefined = disabled. Range 1-90 enforced by the UI.
  auto_destroy_empty_workspaces_days?: number | null;
  // Phase 58: voice input (speech-to-text) — opt-in. Defaults via
  // serde(default) on the Rust side, so older settings.json files
  // load with stt: { enabled: false, backend: "webspeech", ... }.
  stt?: SttSettings;
  // Phase 62.B (item I): sidebar display mode. Backend defaults to
  // "full" via serde(default). Phase 65.P: two modes only (full /
  // icons) — the old "hidden" value migrates to "icons" on read.
  sidebar_mode?: SidebarMode;
  // Phase 63: per-kind floating-window state (Browser / FileManager).
  floating_windows?: FloatingWindows;
  // Phase 75: debug-log retention.
  logs?: LogsSettings;
  // Unshipped-fivefer (#3): persist workspace-browser sessions (cookies/
  // logins) across restarts. Backend defaults to true.
  persist_browser_sessions?: boolean;
  // Design Pass 01 (#2): dark/light appearance axis. Backend defaults to
  // "system" via serde(default); older settings.json load unchanged.
  theme_mode?: ThemeMode;
  // Phase 81: tmux session picker scope. "shared" (backend default) =
  // every session on the server; "local" = only sessions this machine
  // created (origin-less sessions stay visible — fail-open).
  session_visibility?: SessionVisibility;
}

// Phase 81: multi-machine session picker scope.
export type SessionVisibility = "shared" | "local";

// Design Pass 01 (#2): appearance polarity. "system" follows the OS.
export type ThemeMode = "dark" | "light" | "system";

// Phase 75: debug.log hygiene. Unified logging: level threshold + remote
// log sync into the single local debug.log.
export type LogLevelSetting = "debug" | "info";
export interface LogsSettings {
  retention_days: number;
  level: LogLevelSetting;
  remote_sync: boolean;
}

// Mirrors LogsSettings::default() in settings.rs.
export const DEFAULT_LOGS_SETTINGS: LogsSettings = {
  retention_days: 7,
  level: "info",
  remote_sync: true,
};

// Phase 65.P: dropped "hidden" — only full / icons. Old persisted
// "hidden" values are migrated to "icons" at read time (App.tsx).
export type SidebarMode = "full" | "icons";

// Phase 63: 3-mode floating windows.
export type FloatingWindowMode = "pane" | "float" | "popout";
export interface FloatingRect {
  x: number;
  y: number;
  width: number;
  height: number;
}
export interface FloatingWindowState {
  mode?: FloatingWindowMode;
  float_rect?: FloatingRect | null;
  popout_rect?: FloatingRect | null;
  popout_display?: number | null;
  pane_width?: number | null;
}
export interface FloatingWindows {
  browser?: FloatingWindowState;
  filemanager?: FloatingWindowState;
}

export interface SummaryResult {
  text: string;
  session_id: string;
  messages_count: number;
  generated_at: string;
  note_id?: string | null;
}

export interface PresetEntry {
  id: string;
  label: string;
  theme: Theme;
}

/**
 * One row in the Settings font picker. `installed` comes from the Rust-side
 * registry enumeration (`list_system_fonts`); false means picking it would
 * silently fall through `quoteFamily()`'s CSS fallback chain and change
 * nothing on screen, so the UI flags it rather than letting the user
 * conclude the setting is broken.
 */
export interface FontEntry {
  name: string;
  installed: boolean;
}

export interface FontFamilies {
  ui: FontEntry[];
  mono: FontEntry[];
}

/** A font ymux can download and install per-user (no admin needed). */
export interface FontCatalogItem {
  id: string;
  /** CSS family name — matches the picker row this install would satisfy. */
  family: string;
  description: string;
  homepage: string;
  license: string;
  download_bytes: number;
  /**
   * At least one face of this entry is present in the per-user font
   * directory. Derived from the directory on every call, not from a
   * record of past installs, so a font installed by an older build is
   * still removable.
   */
  installed: boolean;
}

export interface FontInstallResult {
  /** Face names actually written and registered. Empty when `guided`. */
  installed: string[];
  /**
   * True when the silent per-user install failed (locked-down box, AV) and
   * we handed the file to the shell instead — the font is NOT installed
   * yet; the user still has to click Install in the Windows font preview.
   */
  guided: boolean;
  guided_path: string | null;
  fallback_reason: string | null;
}

export interface FontUninstallResult {
  /** File names removed from the per-user font directory. */
  removed: string[];
  /** Registry values dropped. Windows only; empty elsewhere. */
  unregistered: string[];
  /**
   * Faces found but not removed, each with its reason. The everyday case
   * is a font file held open by a running app, which is why a partial
   * uninstall is a reported outcome rather than a thrown error.
   */
  failed: string[];
}

export interface UpdateInfo {
  current_version: string;
  latest_version?: string | null;
  available: boolean;
  notes_url?: string | null;
  msi_url?: string | null;
  released_at?: string | null;
  manifest_url?: string | null;
  error?: string | null;
  last_check_iso: string;
}

// ─── disk I/O via Tauri commands ─────────────────────────────────────────

export const loadSettings = (): Promise<Settings> =>
  invoke<Settings>("settings_load");

export const saveSettings = (settings: Settings): Promise<Settings> =>
  invoke<Settings>("settings_save", { settings });

export const getPresets = (): Promise<PresetEntry[]> =>
  invoke<PresetEntry[]>("settings_get_presets");

export const applyPreset = (preset: string): Promise<Settings> =>
  invoke<Settings>("settings_apply_preset", { preset });

export const resetSettings = (): Promise<Settings> =>
  invoke<Settings>("settings_reset");

export const listSystemFonts = (): Promise<FontFamilies> =>
  invoke<FontFamilies>("list_system_fonts");

export const fontCatalog = (): Promise<FontCatalogItem[]> =>
  invoke<FontCatalogItem[]>("font_catalog");

export const fontInstall = (id: string): Promise<FontInstallResult> =>
  invoke<FontInstallResult>("font_install", { id });

export const fontUninstall = (id: string): Promise<FontUninstallResult> =>
  invoke<FontUninstallResult>("font_uninstall", { id });

export const checkForUpdates = (): Promise<UpdateInfo> =>
  invoke<UpdateInfo>("check_for_updates_now");

// ─── theme apply ─────────────────────────────────────────────────────────

/**
 * Write the current theme into CSS variables on `<html>`. App.css reads
 * them (var(--w-bg) etc.) so the entire UI re-tints instantly. Called on
 * startup after load and on every `settings:changed` event.
 */
/**
 * 2026-08-19: settings → the per-profile RTL record the terminals read.
 *
 * Falls back to the deprecated flat fields per key, so this stays correct in
 * the window before the backend migration has run (and if a hand-edited
 * settings.json carries only one of the two shapes). `local` and `remote`
 * deliberately fall back to DIFFERENT defaults — they were measured to need
 * opposite modes; see `profileFor` in types.ts.
 */
export function resolveRtlProfiles(
  t: TerminalSettings,
): Record<RtlProfileKind, RtlProfileSettings> {
  const pick = (
    p: RtlProfileFields | undefined,
    fallbackMode: RtlMode,
    // 2026-08-19: local defaults this ON and remote OFF, because Claude Code on
    // Windows writes RTL already in visual order (measured with
    // `zellij action dump-screen`) while the remote path does not need it.
    fallbackTuiOwnsBidi: boolean,
  ): RtlProfileSettings => ({
    rtlMode: (p?.rtl_mode ?? t.rtl_mode ?? fallbackMode) as RtlMode,
    autoDirection: p?.auto_direction ?? t.auto_direction ?? true,
    mirrorArrowsRtl: p?.mirror_arrows_rtl ?? t.mirror_arrows_rtl ?? true,
    tuiOwnsBidi: p?.tui_owns_bidi ?? t.tui_owns_bidi ?? fallbackTuiOwnsBidi,
    // No flat-field fallback on purpose: `direction_policy` postdates the
    // split, so there is no deprecated global to inherit from, and an absent
    // value must mean the older rule rather than the newer one.
    directionPolicy: p?.direction_policy ?? "any_rtl",
  });
  const out = {
    local: pick(t.rtl?.local, "auto_per_line", true),
    remote: pick(t.rtl?.remote, "auto_per_line", false),
  };
  // 2026-08-19: state what was actually resolved, and whether the stored block
  // existed at all. Two separate bugs hid behind this — a partial profile write
  // collapsing unspecified fields to the wrong defaults, and a stale client
  // save dropping `terminal.rtl` entirely — and neither was visible from the
  // per-pane logs, which report the RESULT without saying where it came from.
  rtlLog.info(
    `rtl-profiles block=${t.rtl ? 1 : 0} ` +
      `local(mode=${out.local.rtlMode},tui=${out.local.tuiOwnsBidi ? 1 : 0},` +
      `policy=${out.local.directionPolicy}) ` +
      `remote(mode=${out.remote.rtlMode},tui=${out.remote.tuiOwnsBidi ? 1 : 0},` +
      `policy=${out.remote.directionPolicy})`,
  );
  return out;
}

export function applyTheme(s: Settings): void {
  const r = document.documentElement.style;
  const t = s.theme;
  r.setProperty("--w-bg", t.background);
  r.setProperty("--w-surface", t.surface);
  r.setProperty("--w-border", t.border);
  r.setProperty("--w-text", t.text_primary);
  r.setProperty("--w-text-dim", t.text_secondary);
  r.setProperty("--w-accent", t.accent);
  r.setProperty("--w-success", t.success);
  r.setProperty("--w-warning", t.warning);
  r.setProperty("--w-error", t.error);
  // Derive a couple of secondary tones from the base ones rather than
  // requiring users to set all of them.
  r.setProperty("--w-surface-hi", mix(t.surface, t.text_primary, 0.06));
  r.setProperty("--w-border-hi", mix(t.border, t.text_primary, 0.1));
  r.setProperty("--w-text-faint", mix(t.text_secondary, t.background, 0.4));
  r.setProperty("--w-accent-hi", mix(t.accent, "#ffffff", 0.18));

  // Redesign directions carry their own display font (Barlow / Source Serif /
  // Archivo / Lora), but SOFTLY: only when the user hasn't picked a custom UI
  // font. If ui_family is still the default ("system-ui"), we clear the inline
  // var so themes-redesign.css can supply the direction's font; otherwise the
  // user's explicit choice is written inline and wins over the theme.
  const REDESIGN_PRESETS = ["industry", "broadsheet", "modernist", "classical"];
  const presetBase = t.preset.replace(/-dark$/, "");
  const isRedesign = REDESIGN_PRESETS.includes(presetBase);
  const uiFontIsDefault = s.font.ui_family === "system-ui";
  if (isRedesign && uiFontIsDefault) {
    r.removeProperty("--w-font-ui");
  } else {
    r.setProperty("--w-font-ui", quoteFamily(s.font.ui_family));
  }
  r.setProperty("--w-font-mono", quoteFamily(s.font.terminal_family));
  // Phase 9.A live size apply. App.css now bases :root font-size on this
  // var, and the --w-fs-* size vars are in em — so changing this single pt
  // value rescales every UI element proportionally.
  r.setProperty("--w-font-size-ui", `${s.font.ui_size_pt}pt`);
  // Push terminal font + size into every live xterm instance. New panes
  // opened later inherit the cached values via the constructor.
  setTerminalFont(quoteFamily(s.font.terminal_family), s.font.terminal_size_pt);
  // Redesign pass 4: the terminal palette follows the theme — background,
  // foreground, cursor and the 16 ANSI colours all come from the preset
  // (Theme.ansi shipped since Phase 9.A but was never wired to xterm).
  setTerminalTheme(buildTerminalTheme(t));
  // Phase 15.A: push the RTL mode. The write pipeline flips immediately
  // on every live pane; the renderer choice (DOM vs WebGL) is sticky
  // per pane and only affects newly-opened terminals.
  setRtlProfiles(resolveRtlProfiles(s.terminal));
  // v0.4.4-beta.2: clear stale mouse-tracking modes on connect (default on).
  setAutoResetOnConnect(s.terminal.auto_reset_on_connect ?? true);

  // Design Pass 01 (#2): dark/light axis. Resolve "system" against the OS
  // and write data-theme-mode on <html>; tokens.css keys the Light chrome
  // palette off it. Independent of the colour preset.
  document.documentElement.dataset.themeMode = resolveThemeMode(s.theme_mode);

  // Redesign directions (Claude Design handoff): stamp the active preset id on
  // <html> so themes-redesign.css can key per-theme fonts + structural chrome
  // (registration marks, double rules, gold hairlines) and the waiting-ring
  // colour off it. Also lets tokens.css opt these light-ground presets out of
  // the daylight override so their inline --w-* palette always wins.
  document.documentElement.dataset.themePreset = t.preset;
  document.documentElement.dataset.themeFamily = isRedesign ? "redesign" : "";

  // Phase font-bug-fix v2 (stretch): if a web font URL is configured,
  // inject a single <link rel="stylesheet"> tag so that font becomes
  // available by family name. Removing or changing the URL replaces the
  // tag — we don't try to garbage-collect previously-loaded sheets.
  applyWebFont(s.font.web_font_url ?? "");
}

/**
 * Design Pass 01 (#2): resolve the appearance axis to a concrete polarity.
 * "system" (or a missing value) follows the OS `prefers-color-scheme`.
 */
export function resolveThemeMode(mode: string | undefined): "dark" | "light" {
  if (mode === "light") return "light";
  if (mode === "dark") return "dark";
  return window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

/**
 * Re-apply the theme when the OS scheme flips while the user is on
 * "system". Registered once at startup with a live settings getter.
 */
export function watchSystemTheme(getSettings: () => Settings): void {
  const mq = window.matchMedia?.("(prefers-color-scheme: light)");
  if (!mq) return;
  mq.addEventListener("change", () => {
    if ((getSettings().theme_mode ?? "system") === "system") {
      applyTheme(getSettings());
    }
  });
}

function applyWebFont(url: string): void {
  const existing = document.getElementById("ymux-web-font") as
    | HTMLLinkElement
    | null;
  const trimmed = (url || "").trim();
  if (!trimmed) {
    if (existing) existing.remove();
    return;
  }
  // Don't reload the same URL.
  if (existing && existing.href === trimmed) return;
  if (existing) existing.remove();
  const link = document.createElement("link");
  link.id = "ymux-web-font";
  link.rel = "stylesheet";
  link.href = trimmed;
  link.crossOrigin = "anonymous";
  document.head.appendChild(link);
}

function quoteFamily(family: string): string {
  // Wrap with single quotes if the family has a space and isn't already
  // quoted; append safe fallbacks so a missing font doesn't break layout.
  const trimmed = family.trim();
  const isMono =
    /mono|consolas|cascadia|courier|menlo|fira|jetbrains|iosevka|hack|source code|lucida console/i.test(
      trimmed
    );
  const head = trimmed && !/[",']/.test(trimmed) && /\s/.test(trimmed)
    ? `"${trimmed}"`
    : trimmed;
  const fallback = isMono
    ? '"Cascadia Mono", "JetBrains Mono", Consolas, ui-monospace, monospace'
    : '-apple-system, "Segoe UI Variable", "Segoe UI", system-ui, sans-serif';
  return `${head}, ${fallback}`;
}

// Minimal hex color blender (#rrggbb only). Best-effort — non-hex values
// pass through unchanged, which still works because CSS will fall back
// when it sees an invalid value.
/** Redesign pass 4: map our Theme onto xterm's ITheme. */
function buildTerminalTheme(t: Theme): ITheme {
  const a = t.ansi;
  return {
    background: t.background,
    foreground: t.text_primary,
    cursor: t.accent,
    cursorAccent: t.background,
    selectionBackground: alpha(t.accent, 0.35),
    black: a.black,
    red: a.red,
    green: a.green,
    yellow: a.yellow,
    blue: a.blue,
    magenta: a.magenta,
    cyan: a.cyan,
    white: a.white,
    brightBlack: a.bright_black,
    brightRed: a.bright_red,
    brightGreen: a.bright_green,
    brightYellow: a.bright_yellow,
    brightBlue: a.bright_blue,
    brightMagenta: a.bright_magenta,
    brightCyan: a.bright_cyan,
    brightWhite: a.bright_white,
  };
}

/** Hex colour → rgba() string with the given alpha (falls back to the hex). */
function alpha(hex: string, a: number): string {
  const c = parseHex(hex);
  return c ? `rgba(${c[0]}, ${c[1]}, ${c[2]}, ${a})` : hex;
}

function mix(base: string, with_: string, amount: number): string {
  const a = parseHex(base);
  const b = parseHex(with_);
  if (!a || !b) return base;
  const t = Math.max(0, Math.min(1, amount));
  const m = (i: number) => Math.round(a[i] * (1 - t) + b[i] * t);
  return `rgb(${m(0)}, ${m(1)}, ${m(2)})`;
}

function parseHex(c: string): [number, number, number] | null {
  const s = c.trim().replace(/^#/, "");
  if (s.length === 3) {
    const r = parseInt(s[0] + s[0], 16);
    const g = parseInt(s[1] + s[1], 16);
    const b = parseInt(s[2] + s[2], 16);
    if ([r, g, b].some((v) => Number.isNaN(v))) return null;
    return [r, g, b];
  }
  if (s.length === 6) {
    const r = parseInt(s.slice(0, 2), 16);
    const g = parseInt(s.slice(2, 4), 16);
    const b = parseInt(s.slice(4, 6), 16);
    if ([r, g, b].some((v) => Number.isNaN(v))) return null;
    return [r, g, b];
  }
  return null;
}
