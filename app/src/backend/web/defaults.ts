// The Settings a browser starts from (Phase 109) — Rust's `Settings::default()`
// (settings.rs) for every required group, so a first visit looks like a fresh
// desktop install. Optional groups are left out: every reader already falls
// back on `?? default` for them, exactly as it does for an old settings.json.
//
// Two deliberate differences from the desktop, both about reload:
// `restore_sessions_on_start` is ON (re-attaching after F5 is the point of a
// browser tab; the desktop keeps it opt-in because startup there reaches over
// SSH), and there is no update checking (a browser runs what the daemon serves).

import type { Settings } from "../../settings";

export const WEB_DEFAULT_SETTINGS: Settings = {
  version: 1,
  theme: {
    preset: "tokyo-night",
    accent: "#7aa2f7",
    background: "#0e1116",
    surface: "#161b22",
    border: "#21262d",
    text_primary: "#e6edf3",
    text_secondary: "#7d8590",
    success: "#4ec9b0",
    warning: "#e0af68",
    error: "#f7768e",
    ansi: {
      black: "#15161e",
      red: "#f7768e",
      green: "#9ece6a",
      yellow: "#e0af68",
      blue: "#7aa2f7",
      magenta: "#bb9af7",
      cyan: "#7dcfff",
      white: "#a9b1d6",
      bright_black: "#414868",
      bright_red: "#ff7a93",
      bright_green: "#b9f27c",
      bright_yellow: "#ff9e64",
      bright_blue: "#7da6ff",
      bright_magenta: "#bb9af7",
      bright_cyan: "#0db9d7",
      bright_white: "#c0caf5",
    },
  },
  font: {
    ui_family: "system-ui",
    ui_size_pt: 13,
    terminal_family: "Cascadia Mono",
    terminal_size_pt: 13,
    web_font_url: null,
  },
  terminal: {
    rtl_mode: "auto_per_line",
    use_ymux_tmux_config: true,
    mirror_arrows_rtl: true,
    auto_direction: true,
    tui_owns_bidi: false,
    auto_reset_on_connect: true,
  },
  hooks: { policy_enabled: true, auto_install: true, custom_block: [], custom_gate: [] },
  notifications: {
    toast_enabled: true,
    toast_session_start: false,
    toast_session_end: true,
    toast_stop: true,
    toast_notification: true,
    toast_gate: true,
    toast_block: true,
    pane_pulse_on_activity: true,
  },
  updates: { check_on_startup: false, skipped_versions: [], channel: "stable" },
  i18n: { language: "en", direction: "auto" },
  auto_connect_on_workspace_select: true,
  restore_sessions_on_start: true,
  persist_browser_sessions: false,
};

/**
 * A stored document over the defaults: top-level groups that are objects are
 * merged one level deep, so a setting added after the document was saved
 * still gets its default. A stored value whose shape does not match an object
 * default (a hand-written or foreign document) is ignored rather than allowed
 * to replace a whole group — `applyTheme` reading `theme.text_primary` off a
 * string is exactly how a bad document would otherwise blank the UI.
 */
export function withDefaults(stored: unknown): Settings {
  if (typeof stored !== "object" || stored === null || Array.isArray(stored)) {
    return structuredClone(WEB_DEFAULT_SETTINGS);
  }
  const out: Record<string, unknown> = structuredClone(WEB_DEFAULT_SETTINGS) as unknown as Record<string, unknown>;
  for (const [k, v] of Object.entries(stored as Record<string, unknown>)) {
    const d = out[k];
    const isObj = (x: unknown) => typeof x === "object" && x !== null && !Array.isArray(x);
    if (isObj(d)) {
      if (isObj(v)) out[k] = { ...(d as Record<string, unknown>), ...(v as Record<string, unknown>) };
      continue; // keep the default group
    }
    out[k] = v;
  }
  return out as unknown as Settings;
}
