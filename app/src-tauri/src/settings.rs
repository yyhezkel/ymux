//! Phase 9.A: app settings (theme + font + terminal + hooks + notifications +
//! updates). Persisted in `%APPDATA%\ymux\settings.json` next to
//! `workspaces.json` / `notes.json`. Same atomic-write + load-poison-gate
//! pattern. Mutations emit `settings:changed` to the frontend so live theme
//! updates from the CLI reflect into the UI without a reload.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::{config_dir_pub, log_debug, log_info, log_warn, AppState};

// ─── beta.3: hook-type enum + per-hook enable/sound settings ───────────────

/// beta.3: canonical list of Claude Code hook types. Serialized in the
/// kebab-case wire form ("pre-tool-use" etc.) so it round-trips with the
/// existing hook `subkind` strings the CLI emits (see rpc_server.rs).
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HookType {
    PreToolUse,
    Notification,
    Stop,
    SessionEnd,
    PostToolUse,
    SubagentStop,
    UserPromptSubmit,
    PreCompact,
    SessionStart,
}

/// beta.3: per-hook toggles: which types the backend actually processes and
/// which of those play a sound on the toast.
///
/// Migration policy (see `default_hook_notifications` / `migrate_settings`):
/// when an older settings.json has no `hook_notifications` object, the
/// defaults kick in — the interactive-4 (PreToolUse / Notification / Stop /
/// SessionEnd) are enabled; the interactive-3 (PreToolUse / Notification /
/// Stop) additionally get sound; sound_master starts on. The verbose
/// observability hooks (PostToolUse / SubagentStop / UserPromptSubmit /
/// PreCompact / SessionStart) are off across the board.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct HookSettings {
    #[serde(default = "default_enabled_types")]
    pub enabled_types: HashSet<HookType>,
    #[serde(default = "default_sound_types")]
    pub sound_types: HashSet<HookType>,
    #[serde(default = "default_true")]
    pub sound_master: bool,
}

fn default_enabled_types() -> HashSet<HookType> {
    let mut s = HashSet::new();
    s.insert(HookType::PreToolUse);
    s.insert(HookType::Notification);
    s.insert(HookType::Stop);
    s.insert(HookType::SessionEnd);
    s
}

fn default_sound_types() -> HashSet<HookType> {
    let mut s = HashSet::new();
    s.insert(HookType::PreToolUse);
    s.insert(HookType::Notification);
    s.insert(HookType::Stop);
    s
}

impl Default for HookSettings {
    fn default() -> Self {
        Self {
            enabled_types: default_enabled_types(),
            sound_types: default_sound_types(),
            sound_master: true,
        }
    }
}

/// beta.3: convert a wire subkind ("pre-tool-use", "notification", …) to the
/// enum. Returns None for the retired `session-start` on legacy CLI 1.1.0 or
/// anything unknown.
pub(crate) fn hook_type_from_subkind(s: &str) -> Option<HookType> {
    match s {
        "pre-tool-use" => Some(HookType::PreToolUse),
        "notification" => Some(HookType::Notification),
        "stop" => Some(HookType::Stop),
        "session-end" => Some(HookType::SessionEnd),
        "post-tool-use" => Some(HookType::PostToolUse),
        "subagent-stop" => Some(HookType::SubagentStop),
        "user-prompt-submit" => Some(HookType::UserPromptSubmit),
        "pre-compact" => Some(HookType::PreCompact),
        "session-start" => Some(HookType::SessionStart),
        _ => None,
    }
}

// ─── data model ────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct AnsiPalette {
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl AnsiPalette {
    fn tokyo_night() -> Self {
        Self {
            black: "#15161e".into(),
            red: "#f7768e".into(),
            green: "#9ece6a".into(),
            yellow: "#e0af68".into(),
            blue: "#7aa2f7".into(),
            magenta: "#bb9af7".into(),
            cyan: "#7dcfff".into(),
            white: "#a9b1d6".into(),
            bright_black: "#414868".into(),
            bright_red: "#ff7a93".into(),
            bright_green: "#b9f27c".into(),
            bright_yellow: "#ff9e64".into(),
            bright_blue: "#7da6ff".into(),
            bright_magenta: "#bb9af7".into(),
            bright_cyan: "#0db9d7".into(),
            bright_white: "#c0caf5".into(),
        }
    }
    fn dracula() -> Self {
        Self {
            black: "#21222c".into(),
            red: "#ff5555".into(),
            green: "#50fa7b".into(),
            yellow: "#f1fa8c".into(),
            blue: "#bd93f9".into(),
            magenta: "#ff79c6".into(),
            cyan: "#8be9fd".into(),
            white: "#f8f8f2".into(),
            bright_black: "#6272a4".into(),
            bright_red: "#ff6e6e".into(),
            bright_green: "#69ff94".into(),
            bright_yellow: "#ffffa5".into(),
            bright_blue: "#d6acff".into(),
            bright_magenta: "#ff92df".into(),
            bright_cyan: "#a4ffff".into(),
            bright_white: "#ffffff".into(),
        }
    }
    fn solarized_dark() -> Self {
        Self {
            black: "#073642".into(),
            red: "#dc322f".into(),
            green: "#859900".into(),
            yellow: "#b58900".into(),
            blue: "#268bd2".into(),
            magenta: "#d33682".into(),
            cyan: "#2aa198".into(),
            white: "#eee8d5".into(),
            bright_black: "#002b36".into(),
            bright_red: "#cb4b16".into(),
            bright_green: "#586e75".into(),
            bright_yellow: "#657b83".into(),
            bright_blue: "#839496".into(),
            bright_magenta: "#6c71c4".into(),
            bright_cyan: "#93a1a1".into(),
            bright_white: "#fdf6e3".into(),
        }
    }
    fn nord() -> Self {
        Self {
            black: "#3b4252".into(),
            red: "#bf616a".into(),
            green: "#a3be8c".into(),
            yellow: "#ebcb8b".into(),
            blue: "#81a1c1".into(),
            magenta: "#b48ead".into(),
            cyan: "#88c0d0".into(),
            white: "#e5e9f0".into(),
            bright_black: "#4c566a".into(),
            bright_red: "#bf616a".into(),
            bright_green: "#a3be8c".into(),
            bright_yellow: "#ebcb8b".into(),
            bright_blue: "#81a1c1".into(),
            bright_magenta: "#b48ead".into(),
            bright_cyan: "#8fbcbb".into(),
            bright_white: "#eceff4".into(),
        }
    }
    fn solarized_light() -> Self {
        Self {
            black: "#073642".into(),
            red: "#dc322f".into(),
            green: "#859900".into(),
            yellow: "#b58900".into(),
            blue: "#268bd2".into(),
            magenta: "#d33682".into(),
            cyan: "#2aa198".into(),
            white: "#eee8d5".into(),
            bright_black: "#002b36".into(),
            bright_red: "#cb4b16".into(),
            bright_green: "#586e75".into(),
            bright_yellow: "#657b83".into(),
            bright_blue: "#839496".into(),
            bright_magenta: "#6c71c4".into(),
            bright_cyan: "#93a1a1".into(),
            bright_white: "#fdf6e3".into(),
        }
    }

    /// Redesign pass 6: ANSI ramp for LIGHT-ground terminals (GitHub-Light
    /// derived). Unlike solarized_light, the white/bright slots are mid
    /// grays — apps that print "white" text (Claude Code bullets, dimmed
    /// output) stay readable on a light background.
    /// Redesign dark ramps — per-direction 16-colour tuning (the dark
    /// variants used to recycle tokyo_night()). ANSI semantics stay
    /// functional (red=error, green=ok); the direction's identity lives in
    /// the cast of the neutrals and in which slots are allowed to shout.
    /// All values sit comfortably above the xterm minimumContrastRatio 4.5
    /// floor on their direction's ground.
    ///
    /// Industry dark — cool steel cast, engineering restraint.
    fn industry_dark() -> Self {
        Self {
            black: "#1c232b".into(),
            red: "#e0716b".into(),
            green: "#74b585".into(),
            yellow: "#d9b26a".into(),
            blue: "#6f9fce".into(),
            magenta: "#9a8fd0".into(),
            cyan: "#6fc3ce".into(),
            white: "#c3ccd6".into(),
            bright_black: "#3d4a58".into(),
            bright_red: "#f28b85".into(),
            bright_green: "#8fd0a0".into(),
            bright_yellow: "#ecc985".into(),
            bright_blue: "#8fb8e0".into(),
            bright_magenta: "#b3a8e6".into(),
            bright_cyan: "#8fd9e3".into(),
            bright_white: "#e2e9f0".into(),
        }
    }
    /// Broadsheet dark — neutral ink ramp; cyan + magenta are the only
    /// slots allowed to be loud (the direction's two spot colours).
    fn broadsheet_dark() -> Self {
        Self {
            black: "#221f26".into(),
            red: "#ff6b8a".into(),
            green: "#7cb98a".into(),
            yellow: "#d4b370".into(),
            blue: "#6ea3c9".into(),
            magenta: "#ff2f86".into(),
            cyan: "#35b6df".into(),
            white: "#cfc8c6".into(),
            bright_black: "#494349".into(),
            bright_red: "#ff8ba3".into(),
            bright_green: "#97d0a4".into(),
            bright_yellow: "#e6c98d".into(),
            bright_blue: "#8ebcdd".into(),
            bright_magenta: "#ff5ea1".into(),
            bright_cyan: "#5ecbf0".into(),
            bright_white: "#ece7e4".into(),
        }
    }
    /// Modernist dark — near-monochrome ramp, one dominant red.
    fn modernist_dark() -> Self {
        Self {
            black: "#232120".into(),
            red: "#ff563c".into(),
            green: "#86a98c".into(),
            yellow: "#d6c08a".into(),
            blue: "#8fa3b8".into(),
            magenta: "#c39ab0".into(),
            cyan: "#93b8ba".into(),
            white: "#d9d6d4".into(),
            bright_black: "#4a4644".into(),
            bright_red: "#ff7a5f".into(),
            bright_green: "#a0c2a6".into(),
            bright_yellow: "#e8d4a2".into(),
            bright_blue: "#aabccd".into(),
            bright_magenta: "#d7b2c6".into(),
            bright_cyan: "#adcfd1".into(),
            bright_white: "#f3f2f2".into(),
        }
    }
    /// Classical dark — warm library ramp, gold in the yellow slots.
    fn classical_dark() -> Self {
        Self {
            black: "#262118".into(),
            red: "#d9736a".into(),
            green: "#94ab6e".into(),
            yellow: "#cd9a45".into(),
            blue: "#7f9cc0".into(),
            magenta: "#b58ab8".into(),
            cyan: "#7fb3a8".into(),
            white: "#d8cfc0".into(),
            bright_black: "#4d4436".into(),
            bright_red: "#e88f86".into(),
            bright_green: "#aec283".into(),
            bright_yellow: "#e6b562".into(),
            bright_blue: "#9cb5d6".into(),
            bright_magenta: "#cca4cf".into(),
            bright_cyan: "#99c9be".into(),
            bright_white: "#efe9df".into(),
        }
    }
    fn daylight() -> Self {
        Self {
            black: "#24292f".into(),
            red: "#cf222e".into(),
            green: "#116329".into(),
            yellow: "#9a6700".into(),
            blue: "#0969da".into(),
            magenta: "#8250df".into(),
            cyan: "#1b7c83".into(),
            white: "#57606a".into(),
            bright_black: "#656d76".into(),
            bright_red: "#a40e26".into(),
            bright_green: "#1a7f37".into(),
            bright_yellow: "#bf8700".into(),
            bright_blue: "#218bff".into(),
            bright_magenta: "#a475f9".into(),
            bright_cyan: "#3192aa".into(),
            bright_white: "#8c959f".into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Theme {
    pub preset: String,
    pub accent: String,
    pub background: String,
    pub surface: String,
    pub border: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub ansi: AnsiPalette,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Font {
    pub ui_family: String,
    pub ui_size_pt: u32,
    pub terminal_family: String,
    pub terminal_size_pt: u32,
    /// Stretch goal: optional URL to a CSS stylesheet (e.g. Google Fonts)
    /// that the frontend injects via <link rel="stylesheet"> so the user
    /// can pick a non-installed family and have it fetched at runtime.
    /// Empty / None = no extra fonts loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_font_url: Option<String>,
}

/// 2026-08-19: RTL handling for ONE class of pane. The four knobs used to be
/// flat on `TerminalSettings`, i.e. global, which made the two classes
/// mutually exclusive — see `RtlProfiles`.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct RtlProfile {
    /// "auto_per_line" (DOM renderer + per-row dir), "force_rtl" (DOM
    /// renderer, every row dir="rtl" with no detection at all),
    /// "bidi_reorder" (WebGL + our logical→visual reorder), or "off"
    /// (WebGL, raw).
    ///
    /// A `String` and not an enum on purpose (the sidebar_mode pattern), so
    /// adding a mode costs nothing here and an unknown value read from an
    /// older/newer settings.json degrades to whatever the frontend does with
    /// it rather than failing the whole deserialise.
    ///
    /// 2026-08-23: `force_rtl` was added for REMOTE panes — "RTL מלא, ולא
    /// שורה שורה". It is opt-in: `default_rtl_mode` is unchanged, and
    /// neither profile default moves.
    #[serde(default = "default_rtl_mode")]
    pub rtl_mode: String,
    #[serde(default = "default_true")]
    pub auto_direction: bool,
    #[serde(default = "default_true")]
    pub mirror_arrows_rtl: bool,
    #[serde(default)]
    pub tui_owns_bidi: bool,
    /// Which rule decides a row's paragraph direction: `"any_rtl"` (any Hebrew
    /// on the row takes it RTL — what every version before 2026-08-19 did, and
    /// what remote panes are known to render correctly) or `"tui_dominance"`
    /// (the RTL_DOMINANCE vote in `app/src/textDirection.ts`, which stops a TUI
    /// status bar from mirroring its own layout).
    ///
    /// Keyed on the PANE CLASS on purpose. The vote first shipped keyed on
    /// whether Claude Code held the pane, and since the OSC title propagates
    /// over SSH it fired on remote panes and broke a working path. Absent from
    /// an existing settings.json this reads as `"any_rtl"`, so an upgrade
    /// cannot silently move a pane onto the newer rule.
    #[serde(default = "default_direction_policy")]
    pub direction_policy: String,
}

/// See `RtlProfile::direction_policy`. The pre-2026-08-19 rule is the default
/// for BOTH profiles; `tui_dominance` is opt-in, per profile.
fn default_direction_policy() -> String {
    "any_rtl".to_string()
}

impl Default for RtlProfile {
    fn default() -> Self {
        Self {
            rtl_mode: default_rtl_mode(),
            auto_direction: true,
            mirror_arrows_rtl: true,
            tui_owns_bidi: false,
            direction_policy: default_direction_policy(),
        }
    }
}

/// 2026-08-19: RTL settings, split by what the pane is talking to. Measured
/// live on 2026-08-19, both directions, same app, same build:
///
///   - A **remote** pane (SSH to Linux) delivers Hebrew in LOGICAL order, so
///     `auto_per_line` renders it correctly and `off` renders it reversed.
///   - A **local** pane (native Windows ConPTY) delivers Hebrew ALREADY IN
///     VISUAL order — `auto_per_line` right-aligns it correctly but reverses
///     the letters, because the browser's bidi pass is then the SECOND one.
///     Windows console keeps RTL text visually ordered in its screen buffer
///     and ConPTY re-emits that buffer; Linux just passes bytes through.
///
/// So the two classes need OPPOSITE modes, not different tuning of one mode,
/// and a single global setting could only ever satisfy one of them. Yossi hit
/// this as "ההגדרה הזו עובדת או למקומי או למרוחק".
///
/// The split axis is `ConnCaps.posixExec` (`app/src/types.ts`), not a
/// local/remote boolean: **WSL counts as remote**, because it is Linux with
/// tmux and a Linux Claude, and is "local" only geographically.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct RtlProfiles {
    /// Native Windows panes (ConPTY: cmd, PowerShell). Not WSL.
    #[serde(default = "default_local_rtl")]
    pub local: RtlProfile,
    /// Anything with a POSIX shell behind it — SSH and WSL.
    #[serde(default)]
    pub remote: RtlProfile,
}

/// 2026-08-19, SUPERSEDED SAME DAY. The first measurement had a native
/// Windows pane needing `bidi_reorder`, because raw ConPTY hands over
/// Hebrew already in visual order. Then zellij went in front of local panes
/// — and it NORMALISES the stream to logical order, the way a Linux pty
/// does. Re-measured inside a live zellij pane: `off` renders reversed
/// (so the stream is logical) and `auto_per_line` renders correctly.
///
/// So the two profiles converged, and `bidi_reorder` on local would now be
/// a second reorder of already-logical text. The split still earns its
/// keep — a user can still tune the two apart — but it no longer needs
/// different defaults, and pretending otherwise would ship a wrong one.
/// 2026-08-19, third revision of this default and the first one MEASURED
/// rather than inferred. `zellij action dump-screen` on
/// Yossi's live local pane, which was running Claude Code, held exactly one
/// Hebrew run and it was `U+05E8 U+05D1 U+05E6` -- the cwd `\u05e6\u05d1\u05e8` in VISUAL order,
/// reversed against logical `U+05E6 U+05D1 U+05E8`.
///
/// So Claude Code on Windows writes RTL PRE-REORDERED, exactly as the header of
/// textDirection.ts says it does. Every second bidi pass on top of that is a
/// double reorder, which is why all THREE rtl_modes were reported broken on a
/// local pane while remote was fine:
///   off             leaves the bytes alone -> letters CORRECT, left aligned
///   auto_per_line   the browser reverses the run -> reversed
///   bidi_reorder    our logical->visual pass reverses it again -> reversed
///
/// `tui_owns_bidi` is the switch built for exactly this -- render the pane with
/// no bidi while a self-reordering TUI holds it -- but THE DIAGNOSTIC LOG SHOWS
/// IT NEVER FIRES. Every `rtl-dirs` line in Yossi's debug.log reports `tui=0`,
/// on every pane and both profiles, and the file holds not one
/// `tui-owns-bidi` transition. The detector reads the terminal TITLE, and
/// inside zellij the title we receive is zellij's own, never Claude's. So the
/// dynamic switch is inert in practice, and the checkbox promised something
/// that could not happen.
///
/// Which leaves the static answer: local runs "off". No reorder, no `dir`, no
/// browser bidi -- the terminal Claude already assumes it is talking to.
/// Confirmed by Yossi on the build that added the diagnostic.
///
/// THE COST, said plainly: shell output is LOGICAL order, so Hebrew at a plain
/// PowerShell prompt renders reversed under "off". One static mode cannot serve
/// both a logical shell and a pre-reordered TUI; only the dynamic switch can,
/// and it needs a signal that survives zellij. The ymux Claude hooks already
/// run per pane with YMUX_PANE_ID set and would be exactly that -- logged as a
/// follow-up rather than guessed at here.
///
/// KNOWN TRADE-OFF, not a bug to chase: with a pre-reordered stream you cannot
/// have correct letters AND right alignment. Right alignment needs dir="rtl",
/// which invokes the browser's bidi, which reverses an already-reversed run.
/// Getting both requires un-reversing the stream to logical first and then
/// rendering RTL -- a visual->logical pass, and the piece of work that drags in
/// the cursor/partial-repaint problem already logged in FOLLOWUPS.
///
/// REMOTE IS DELIBERATELY NOT CHANGED. It renders correctly today and Yossi's
/// instruction was to work on local without touching it.
fn default_local_rtl() -> RtlProfile {
    RtlProfile {
        // `tui_owns_bidi` ON is the whole local story: a local SHELL is logical
        // order and renders correctly under auto_per_line — Yossi's
        // side-by-side screenshot has cmd.exe right-aligned with the "?" at the
        // left end — while Claude Code is pre-reordered and must not be bidi'd
        // again. Only the second case needs the switch, and it is now driven
        // per pane by the connect wizard and the Claude hooks rather than by
        // the terminal title, which zellij consumes.
        //
        // This briefly shipped as rtl_mode="off", which fixed Claude by
        // breaking the shell. It is a profile-wide hammer for a per-pane
        // problem; the mode stays auto_per_line and the switch does the work.
        tui_owns_bidi: true,
        ..RtlProfile::default() // auto_per_line, as remote
    }
}

impl Default for RtlProfiles {
    fn default() -> Self {
        Self {
            local: default_local_rtl(),
            remote: RtlProfile::default(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct TerminalSettings {
    /// Phase 15.A: how to handle Hebrew / Arabic in the terminal.
    /// One of "auto_per_line" (default, Termius-style — DOM renderer
    /// + dir="auto" on every row), "force_rtl" (DOM renderer, every row
    /// forced RTL), "bidi_reorder" (legacy v1, WebGL + bidi-js
    /// logical→visual reorder), or "off" (WebGL, no reorder).
    /// New panes pick up the renderer immediately; live mode swaps
    /// affect the reorder pipeline on currently-open panes.
    #[serde(default = "default_rtl_mode")]
    pub rtl_mode: String,
    /// Phase tmux-conf: when true (default), tmux is launched with
    /// `-f ~/.ymux/tmux.conf` so the bundled scrollback-friendly
    /// config applies (wheel scrolls the scrollback ring instead of
    /// shell history, 50k-line buffer, mouse on, sane truecolour).
    /// Set false to fall back to the user's own `~/.tmux.conf`. The
    /// conf file is uploaded by the bootstrap regardless, so the
    /// toggle takes effect on the NEXT pane connect.
    /// `alias`: settings.json files written before the winmux → ymux
    /// rename carry the old key. Without it a user who had explicitly
    /// turned this OFF would silently get it back on after upgrading,
    /// because the unknown key falls through to `default_true`.
    #[serde(default = "default_true", alias = "use_winmux_tmux_config")]
    pub use_ymux_tmux_config: bool,
    /// Phase HH: mirror the physical Left/Right arrow keys when the
    /// terminal line under the cursor is right-to-left (Hebrew/Arabic).
    /// In an RTL line the visual "right" is logical "left", so without
    /// this the arrows feel inverted. Only takes effect on RTL lines —
    /// LTR lines are unaffected — so it's safe to leave on (default true).
    #[serde(default = "default_true")]
    pub mirror_arrows_rtl: bool,
    /// v0.4.4 (RTL Approach C): auto-flip each terminal line's paragraph
    /// direction from its text — a line with any Hebrew/Arabic char renders
    /// RTL (mixed or pure), a pure-Latin line renders LTR. Only affects the
    /// `auto_per_line` rtl_mode. Default true; set false for classic
    /// LTR-only terminal behaviour.
    #[serde(default = "default_true")]
    pub auto_direction: bool,
    /// 2026-08-18: force every row LTR while a self-bidi TUI (Claude Code)
    /// holds the pane. Written in 2026-07 because Claude 2.1.74–2.1.210
    /// emitted Hebrew ALREADY in visual order, so the per-line RTL pass
    /// bidi'd it a second time and scrambled it.
    ///
    /// DEFAULT FALSE, and that is the whole point: current Claude
    /// (verified live against claude-fable-5) emits Hebrew in LOGICAL
    /// order like any other program, so forcing LTR is now what breaks
    /// it. Measured on a live local pane — Hebrew scrambled with this on,
    /// correct with it off, while SSH panes were never affected because
    /// tmux swallows the OSC title the detector depends on.
    ///
    /// Kept rather than deleted because the visual-order behaviour was
    /// real one month ago and may still be, on an older Claude or another
    /// self-bidi TUI. Detection and its `tui-owns-bidi` log line run
    /// regardless of this flag; only the LTR forcing is gated.
    #[serde(default)]
    pub tui_owns_bidi: bool,
    /// v0.4.4-beta.2: on connect/attach, clear stale mouse-tracking modes an
    /// unclean app exit (vim/fzf/less/htop killed) can leave on — which makes
    /// the bare shell print `\e[<..M` mouse escapes as text. Default true; a
    /// manual "Reset terminal" (Ctrl+Alt+R) is always available regardless.
    #[serde(default = "default_true")]
    pub auto_reset_on_connect: bool,
    /// 2026-08-19: the RTL knobs above, split per pane class. See
    /// `RtlProfiles` for the measurement that forced the split.
    ///
    /// The four flat fields (`rtl_mode`, `auto_direction`,
    /// `mirror_arrows_rtl`, `tui_owns_bidi`) are DEPRECATED and kept only so
    /// an existing settings.json still loads and can be migrated from. On
    /// load, when `rtl` is absent, `migrate_rtl_profiles` seeds the profiles:
    /// each takes its own measured `rtl_mode` (a single pre-split value was
    /// necessarily wrong for one of the two classes), while the other three
    /// knobs carry over from the flat fields. Delete the flat fields a
    /// release later, not here.
    ///
    /// `Option` on purpose: **absent is the migration signal.** A fresh
    /// install gets `Some(RtlProfiles::default())` — the measured per-class
    /// defaults — while an existing settings.json has no `rtl` key at all,
    /// deserialises to `None`, and goes through the migration instead. The
    /// two cases want different answers and a plain `#[serde(default)]`
    /// could not tell them apart.
    #[serde(default)]
    pub rtl: Option<RtlProfiles>,
}

/// Seed `terminal.rtl` from the pre-split flat fields the first time a
/// settings.json without it is loaded. Both profiles get the SAME values —
/// the user's current behaviour, preserved exactly — and they diverge only
/// when the user actually changes one. Returns true when it changed anything,
/// so the caller can persist.
pub(crate) fn migrate_rtl_profiles(t: &mut TerminalSettings) -> bool {
    if t.rtl.is_some() {
        return false;
    }
    // `rtl_mode` is deliberately NOT carried over: a single pre-split value
    // was necessarily wrong for at least one of the two classes, because they
    // need opposite modes. There is nothing worth preserving there, so each
    // profile takes its own measured default instead. Verified live on
    // 2026-08-19 before this was made the migration's behaviour.
    //
    // The other three ARE carried over — they are orthogonal to the
    // local/remote split and the user may have tuned them deliberately.
    let defaults = RtlProfiles::default();
    let carry = |d: RtlProfile| RtlProfile {
        rtl_mode: d.rtl_mode,
        auto_direction: t.auto_direction,
        mirror_arrows_rtl: t.mirror_arrows_rtl,
        // 2026-08-19: the flat field defaults to FALSE, so a false here is the
        // ABSENCE of a choice, not a choice — carrying it verbatim silently
        // overrode the per-profile default and local never received
        // tui_owns_bidi=true. Only a true is a real user decision, so only a
        // true is carried; otherwise the profile's own default wins.
        tui_owns_bidi: t.tui_owns_bidi || d.tui_owns_bidi,
        // Not carried from anything: `direction_policy` postdates the split,
        // so there is no deprecated flat field to inherit. A migrating install
        // lands on the pre-2026-08-19 rule, which is what it was already
        // running.
        direction_policy: default_direction_policy(),
    };
    t.rtl = Some(RtlProfiles {
        local: carry(defaults.local),
        remote: carry(defaults.remote),
    });
    true
}

fn default_rtl_mode() -> String {
    "auto_per_line".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Hooks {
    /// Phase 18.1: which PreToolUse matcher to install in the agent's
    /// settings.json. `"restrictive"` (default) only matches risky tools
    /// (`Bash|Write|Edit|MultiEdit|NotebookEdit|Task`); `"all"` matches
    /// every tool (`.*`) so EVERY action surfaces a ymux card; `"custom"`
    /// keeps whatever the user hand-edited locally and is never overwritten
    /// by `ymux setup-hooks`. The setting is consumed by the desktop's
    /// remote-side setup-hooks call (Phase 18 wraps `agent.setup_hooks`).
    #[serde(default = "default_matcher_mode")]
    pub matcher_mode: String,
    /// Phase 66 (66.D): master switch for the 3-state policy engine
    /// (auto / gate / block) that runs in the desktop `feed.push` handler.
    /// When false, every pre-tool-use request becomes a blocking card (the
    /// pre-66 behavior). Default true. Older settings.json without the
    /// field loads with the engine ON.
    #[serde(default = "default_true")]
    pub policy_enabled: bool,
    /// Phase 66 (66.B): when true (default), the SSH bootstrap auto-runs
    /// `ymux setup-hooks` on the remote after deploying the CLI, so a
    /// fresh server starts surfacing permission cards without the user
    /// invoking setup-hooks by hand. No-op if Claude Code isn't installed
    /// remotely. Older settings.json loads with auto-install ON.
    #[serde(default = "default_true")]
    pub auto_install: bool,
    /// Phase 66.F: user-defined BLOCK patterns, one per entry, merged into
    /// the built-in list by the desktop policy engine (rpc_server
    /// feed.push). Same matching semantics as the built-ins: lowercased,
    /// whitespace-collapsed substring match against the whole command and
    /// each chained segment. Desktop-side enforcement only — the CLI's
    /// static fallback keeps the built-ins. `#[serde(default)]` so older
    /// settings.json loads with empty lists.
    #[serde(default)]
    pub custom_block: Vec<String>,
    /// Phase 66.F: user-defined GATE patterns (see `custom_block`). Block
    /// beats gate when a command matches both.
    #[serde(default)]
    pub custom_gate: Vec<String>,
}

fn default_matcher_mode() -> String {
    "restrictive".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Notifications {
    /// Master switch — when false, no hook toasts at all.
    pub toast_enabled: bool,
    // Phase 66 (KK): per-event toast toggles. Defaults chosen to cut noise
    // — lifecycle session events are silent; "needs you" / "finished" /
    // security events surface. Older settings.json loads with these
    // serde defaults (so an upgrade picks the sane set automatically).
    /// Claude session started — noisy, default OFF.
    #[serde(default)]
    pub toast_session_start: bool,
    /// Claude session ended — default OFF.
    #[serde(default)]
    pub toast_session_end: bool,
    /// Claude finished a task (Stop) — useful, default ON.
    #[serde(default = "default_true")]
    pub toast_stop: bool,
    /// Claude needs you (Notification event) — critical, default ON.
    #[serde(default = "default_true")]
    pub toast_notification: bool,
    /// A tool needs approval (PreToolUse gate) — must respond, default ON.
    #[serde(default = "default_true")]
    pub toast_gate: bool,
    /// A dangerous tool was blocked — security insight, default ON.
    #[serde(default = "default_true")]
    pub toast_block: bool,
    /// cmux-A A1: pulse a pane's border when an OSC 9/99/777 terminal
    /// notification arrives for it. Cleared when the user focuses the
    /// pane. Default ON — degrades to a static solid ring under
    /// prefers-reduced-motion: reduce.
    #[serde(default = "default_true")]
    pub pane_pulse_on_activity: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Updates {
    pub check_on_startup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_iso: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_version: Option<String>,
    /// Phase 65 (U): versions the user chose to skip — the
    /// `update:available` banner stays suppressed for these until a
    /// newer version appears. Older settings.json without this field
    /// load with an empty list.
    #[serde(default)]
    pub skipped_versions: Vec<String>,
    /// Phase 65 (U): "remind me later" — suppress the banner until this
    /// ISO timestamp passes. None = no active snooze.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_after_iso: Option<String>,
    /// Phase 71: update channel — "stable" (only `MAJOR.MINOR.PATCH`
    /// releases) or "beta" (also shows pre-releases like `0.4.0-beta1`).
    /// Older settings.json without this field default to stable.
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_channel() -> String {
    "stable".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct I18n {
    pub language: String,
    pub direction: String,
}

impl Default for I18n {
    fn default() -> Self {
        Self {
            language: "en".into(),
            direction: "auto".into(),
        }
    }
}

/// Phase 16: configurable keyboard shortcuts. Stored as human-readable
/// `Ctrl+Shift+X` strings — parsed in the frontend (see
/// `src/shortcuts.ts`) so users can hand-edit settings.json and the
/// next launch picks up the change.
///
/// Phase 87: `#[serde(default)]` at the CONTAINER level, so `impl Default`
/// below is the single source of truth and any field missing from an older
/// settings.json falls back to it. That is what lets the table grow from 8
/// bindings to 28 without 20 near-identical `fn default_*` helpers.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(default)]
pub(crate) struct Shortcuts {
    pub copy: String,
    pub paste: String,
    pub select_all: String,
    pub find: String,
    pub new_workspace: String,
    pub toggle_notes: String,
    pub toggle_settings: String,
    /// Phase 17: trigger a manual Claude session summary. Default
    /// Ctrl+Alt+B (B for "brief"). #[serde(default)] so pre-17
    /// settings.json files don't need to be touched.
    pub summarize_claude: String,
    // Phase 87: these twenty were hardcoded in the frontend's keydown
    // handler until now, with no way to rebind them. A settings.json
    // written by an older build simply lacks them and picks up the
    // Default impl below, via the container-level #[serde(default)].
    pub command_palette: String,
    pub toggle_sidebar: String,
    /// Plain Ctrl+B. Only fires when focus is OUTSIDE a terminal — inside
    /// one, Ctrl+b is tmux's prefix and has to reach the PTY.
    pub toggle_sidebar_soft: String,
    pub toggle_maximize: String,
    pub focus_zoom: String,
    pub reset_terminal: String,
    pub distribute_evenly: String,
    pub split_horizontal: String,
    pub split_vertical: String,
    pub close_pane: String,
    pub split_or_move_left: String,
    pub split_or_move_right: String,
    pub split_or_move_up: String,
    pub split_or_move_down: String,
    pub quadrant_top_left: String,
    pub quadrant_top_right: String,
    pub quadrant_bottom_left: String,
    pub quadrant_bottom_right: String,
    /// Tab cycling. Only fires in a tabs-mode workspace.
    pub tab_next: String,
    pub tab_prev: String,
    /// BRIEF: toggle the cross-workspace agent Queue panel.
    pub toggle_queue: String,
    /// BRIEF: show the Briefing card for the active workspace.
    pub show_briefing: String,
    /// When true and the terminal has a selection, plain Ctrl+C copies
    /// to clipboard instead of sending SIGINT. Matches Windows Terminal
    /// + most modern terminal apps. Set to false to always send SIGINT.
    pub copy_on_select_with_ctrl_c: bool,
}

fn default_summarize_claude() -> String {
    "Ctrl+Alt+B".to_string()
}


/// Phase 17: Claude-specific options.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ClaudeOptions {
    pub auto_summarize_on_stop: bool,
    pub summary_history_count: u32,
    pub summary_prompt: String,
}

impl Default for ClaudeOptions {
    fn default() -> Self {
        Self {
            auto_summarize_on_stop: false,
            summary_history_count: 10,
            summary_prompt: "Summarize the last {N} exchanges in 2-3 sentences in the same language the conversation used.".to_string(),
        }
    }
}

/// BRIEF: the Briefing-card options. Everything opt-in (defaults false) —
/// the card's automatic triggers must never surprise a user who didn't
/// ask for them; the manual shortcut works regardless of these.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(default)]
pub(crate) struct BriefOptions {
    /// Show the card when switching into a workspace not visited for
    /// `absence_minutes`.
    pub entry_card_on_return: bool,
    /// Show the card on the first input after `idle_minutes` with no
    /// keyboard/mouse activity in the app.
    pub entry_card_on_idle: bool,
    pub absence_minutes: u32,
    pub idle_minutes: u32,
}

impl Default for BriefOptions {
    fn default() -> Self {
        Self {
            entry_card_on_return: false,
            entry_card_on_idle: false,
            absence_minutes: 30,
            idle_minutes: 15,
        }
    }
}

/// Phase 78: Claude subscription-usage display options. The usage data
/// itself comes from `claude -p "/usage"` over SSH (see claude_usage.rs);
/// these settings only control how the global % indicator is shown and how
/// often a live connection auto-refreshes it.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ClaudeUsageSettings {
    /// Show the compact usage % indicator at the top of the sidebar.
    pub show_top_indicator: bool,
    /// `"percent"` (colored NN% text) or `"bar"`. A String (not an enum) to
    /// match the sidebar_mode / rtl_mode pattern → plain TS union.
    pub display_mode: String,
    /// Auto-refresh cadence for the active *live* (non-headless) workspace,
    /// in minutes. `0` = off (manual refresh only). The calls are free, so a
    /// modest interval keeps the indicator fresh without user action.
    pub auto_refresh_minutes: u32,
}

impl Default for ClaudeUsageSettings {
    fn default() -> Self {
        Self {
            show_top_indicator: true,
            display_mode: "percent".into(),
            auto_refresh_minutes: 10,
        }
    }
}

/// Phase 18: per-user state for the hooks-outdated banner. Tracked
/// separately from `Hooks` (which is per-agent enablement) because
/// the dismiss list belongs to the UI layer, not to the hook spec.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct HooksUpdates {
    pub show_banners: bool,
    /// `agent → [version-strings-the-user-said-skip]`. Empty entries
    /// are tolerated so a Clear-from-Settings can keep the agent key
    /// around without re-listing every dismissed version.
    pub dismissed: std::collections::BTreeMap<String, Vec<String>>,
}

impl Default for HooksUpdates {
    fn default() -> Self {
        Self {
            show_banners: true,
            dismissed: Default::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            copy: "Ctrl+Shift+C".into(),
            paste: "Ctrl+Shift+V".into(),
            select_all: "Ctrl+Shift+A".into(),
            find: "Ctrl+F".into(),
            new_workspace: "Ctrl+N".into(),
            toggle_notes: "Ctrl+Shift+N".into(),
            toggle_settings: "Ctrl+,".into(),
            summarize_claude: default_summarize_claude(),
            command_palette: "Ctrl+Shift+P".into(),
            toggle_sidebar: "Ctrl+Shift+B".into(),
            toggle_sidebar_soft: "Ctrl+B".into(),
            toggle_maximize: "Ctrl+Enter".into(),
            focus_zoom: "Ctrl+Shift+Z".into(),
            reset_terminal: "Ctrl+Alt+R".into(),
            distribute_evenly: "Ctrl+Alt+=".into(),
            split_horizontal: "Ctrl+Shift+D".into(),
            split_vertical: "Ctrl+Shift+E".into(),
            close_pane: "Ctrl+Shift+W".into(),
            split_or_move_left: "Ctrl+Alt+ArrowLeft".into(),
            split_or_move_right: "Ctrl+Alt+ArrowRight".into(),
            split_or_move_up: "Ctrl+Alt+ArrowUp".into(),
            split_or_move_down: "Ctrl+Alt+ArrowDown".into(),
            quadrant_top_left: "Ctrl+Alt+I".into(),
            quadrant_top_right: "Ctrl+Alt+O".into(),
            quadrant_bottom_left: "Ctrl+Alt+K".into(),
            quadrant_bottom_right: "Ctrl+Alt+L".into(),
            tab_next: "Ctrl+Tab".into(),
            tab_prev: "Ctrl+Shift+Tab".into(),
            toggle_queue: "Ctrl+Shift+Q".into(),
            show_briefing: "Ctrl+Alt+Q".into(),
            copy_on_select_with_ctrl_c: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Settings {
    pub version: u32,
    pub theme: Theme,
    pub font: Font,
    pub terminal: TerminalSettings,
    pub hooks: Hooks,
    pub notifications: Notifications,
    pub updates: Updates,
    // Phase 12.A — defaults to en/auto. `#[serde(default)]` so older
    // settings.json files load without the field.
    #[serde(default)]
    pub i18n: I18n,
    /// Phase 16. `#[serde(default)]` so pre-16 settings.json files
    /// load with the built-in defaults.
    #[serde(default)]
    pub shortcuts: Shortcuts,
    /// Phase 17. Claude session summary options.
    #[serde(default)]
    pub claude: ClaudeOptions,
    /// Phase 78. Claude subscription-usage indicator display + auto-refresh.
    #[serde(default)]
    pub claude_usage: ClaudeUsageSettings,
    /// BRIEF. Briefing-card triggers (all opt-in).
    #[serde(default)]
    pub brief: BriefOptions,
    /// Phase 18. Hooks-outdated banner show/skip state.
    #[serde(default)]
    pub hooks_updates: HooksUpdates,
    /// Phase 32.B. When true, suppress the "set up SSH key
    /// authentication?" offer after a password-auth connect. Persisted
    /// when the user ticks "Don't show again" in the offer modal.
    /// `#[serde(default)]` so older settings.json loads cleanly.
    #[serde(default)]
    pub ssh_key_offer_disabled: bool,
    /// Phase 41. When true (default), activating an SSH workspace
    /// establishes a background SSH session so the tmux session picker and
    /// the remote file manager populate without the user opening a
    /// terminal pane first. Disable to defer the connection until a pane
    /// connects. `default = "default_true"` keeps pre-41 settings.json
    /// backwards-compatible (missing field → true).
    #[serde(default = "default_true")]
    pub auto_connect_on_workspace_select: bool,
    /// Phase 80. When true, app start re-attaches the active workspace's SSH
    /// panes to the tmux sessions they were on when it closed. OFF by default
    /// and deliberately opt-in: it makes startup do network work — one SSH
    /// handshake per restored pane, each re-running the workspace's
    /// `setup_command` — which a rate-limited or fail2ban-fronted host will
    /// notice. `#[serde(default)]` → missing field is false, so an existing
    /// settings.json keeps the old startup behavior until the user asks.
    #[serde(default)]
    pub restore_sessions_on_start: bool,
    /// Phase 80.1. When true, the file manager reopens at the last directory
    /// each column was showing (per workspace) instead of `$HOME`. OFF by
    /// default so the pre-80.1 behavior is what an untouched install gets.
    #[serde(default)]
    pub file_manager_remember_path: bool,
    /// Phase 49-C: optional auto-delete of empty + stale workspaces at
    /// startup. `None` (default) = disabled. Range 1-90 days enforced
    /// by the UI; the backend sweep treats any non-zero positive value
    /// as a valid TTL. A workspace is "empty" for sweep purposes when
    /// it has no live SSH sessions and its `last_active_at` is older
    /// than the TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_destroy_empty_workspaces_days: Option<u32>,
    /// Phase 39.B. One-time data migrations that have already run.
    #[serde(default)]
    pub migrations: MigrationFlags,
    /// Phase 58. Voice input (speech-to-text). Default backend is the
    /// browser-native Web Speech API; users with privacy / offline
    /// needs can point at a local Whisper-compatible endpoint.
    /// `#[serde(default)]` so older settings.json files load with
    /// `enabled = false` + the default backend.
    #[serde(default)]
    pub stt: SttSettings,
    /// Phase 62.B (item I): sidebar display mode — "full" | "icons" |
    /// "hidden". A String (not an enum) to match the rtl_mode /
    /// matcher_mode pattern and keep the TS binding a plain union.
    /// Persisted here (atomic settings write, Rule #7) so the choice
    /// survives restarts. `default = "full"` keeps older settings.json
    /// loading unchanged. Phase 65.P: only "full" / "icons" are written
    /// now; a legacy "hidden" value is migrated to "icons" on the
    /// frontend at read time (App.tsx sidebarMode()).
    #[serde(default = "default_sidebar_mode")]
    pub sidebar_mode: String,
    /// Phase 63: per-kind (browser / filemanager) floating-window state —
    /// which of the 3 modes each is in, plus the remembered geometry for
    /// Float / Pop-out / Pane. `#[serde(default)]` so older settings.json
    /// loads with both kinds defaulting to Float (current behavior).
    #[serde(default)]
    pub floating_windows: FloatingWindows,
    /// Phase 75: debug-log hygiene (retention). `#[serde(default)]` so older
    /// settings.json files load with the built-in defaults.
    #[serde(default)]
    pub logs: LogsSettings,
    /// Unshipped-fivefer (#3): keep workspace-browser cookies/logins across
    /// restarts. Backed by a single app-wide WebView2 profile folder (NOT a
    /// per-workspace `--user-data-dir` — that reintroduces the 0x8007139F
    /// crash). When false, the profile folder is wiped on the next launch.
    /// `default = true`.
    #[serde(default = "default_true")]
    pub persist_browser_sessions: bool,
    /// beta.3: which hook types the backend processes, and which of those
    /// play a sound on the toast. Kept in its own struct so the settings.rs
    /// `Hooks` block (policy engine / matcher_mode / auto_install) stays
    /// scoped to CLI-side hook installation, while this new struct is
    /// purely about desktop-side event routing + sound feedback.
    ///
    /// **Naming note:** the task brief called this field `hooks`, but that
    /// name is taken by the existing policy-engine struct. Named
    /// `hook_notifications` here to preserve backwards-compat without
    /// migrating the old field. `#[serde(default)]` fills defaults when a
    /// pre-beta.3 settings.json loads.
    #[serde(default)]
    pub hook_notifications: HookSettings,
    /// Design Pass 01 (#2): dark/light appearance axis — "dark" | "light" |
    /// "system". Independent of the colour preset: dark reuses the preset
    /// engine as-is, "light" applies ymux's daylight chrome palette,
    /// "system" follows the OS (`prefers-color-scheme`). A String (not an
    /// enum) to match the sidebar_mode / rtl_mode pattern and keep the TS
    /// binding a plain union. `default = "system"` keeps older settings.json
    /// loading unchanged.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
    /// Phase 81: tmux session picker scope — "shared" (default) shows every
    /// session on the server (multi-machine: home sees office sessions and
    /// vice versa); "local" hides sessions whose recorded origin is another
    /// machine (origin-less legacy sessions stay visible — fail-open). A
    /// String (not an enum) per the sidebar_mode / theme_mode pattern.
    #[serde(default = "default_session_visibility")]
    pub session_visibility: String,
}

fn default_session_visibility() -> String {
    "shared".to_string()
}

fn default_sidebar_mode() -> String {
    "full".to_string()
}

/// Unshipped-fivefer (#3): read just the `persist_browser_sessions` flag as
/// early as possible in `run()` — before the WebView2 environment is created —
/// without needing app state. Missing file / parse error → default true.
pub(crate) fn persist_browser_sessions_flag() -> bool {
    load_from_disk()
        .map(|s| s.persist_browser_sessions)
        .unwrap_or(true)
}

fn default_theme_mode() -> String {
    "system".to_string()
}

/// Current persisted log level ("debug"/"info"/…), for callers without
/// AppState access (addon install paths pushing `~/.ymux/log-level`).
pub(crate) fn log_level_setting() -> String {
    load_from_disk()
        .map(|s| s.logs.level)
        .unwrap_or_else(|_| default_log_level())
}

/// Phase 75: debug.log retention. The log auto-rotates at a size cap and is
/// pruned on startup once older than `retention_days`.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct LogsSettings {
    /// Delete debug logs untouched for this many days (0 = keep forever).
    #[serde(default = "default_log_retention_days")]
    pub retention_days: u32,
    /// Unified-logger threshold: "debug" | "info" (internally warn/error
    /// always pass). Applied via `ymux_core::set_log_level` on every load
    /// and save, and pushed to connected remote hosts (`~/.ymux/log-level`).
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Pull the remote logs (server / hooks / install) into the local
    /// debug.log every sync cycle so users read ONE file.
    #[serde(default = "default_true")]
    pub remote_sync: bool,
}

fn default_log_retention_days() -> u32 {
    7
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LogsSettings {
    fn default() -> Self {
        Self {
            retention_days: default_log_retention_days(),
            level: default_log_level(),
            remote_sync: true,
        }
    }
}

/// Phase 63: display mode for a per-workspace Browser / File-Manager
/// window. `Pane` = docked to the side; `Float` = the modal-style window
/// over the workspace (the pre-63 behavior); `PopOut` = a standalone OS
/// window. Lowercased in JSON → TS union "pane" | "float" | "popout".
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub(crate) enum FloatingWindowMode {
    Pane,
    #[default]
    Float,
    PopOut,
}

/// Phase 63: a window rectangle in logical pixels.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Phase 63: persisted state for ONE floating-window kind, shared across
/// workspaces (the window is per-workspace, but its mode + geometry
/// preferences are global per kind — matches Yossi's spec).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct FloatingWindowState {
    #[serde(default)]
    pub mode: FloatingWindowMode,
    /// Last Float-mode rect (in-app, over the workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub float_rect: Option<Rect>,
    /// Last Pop-out OS-window rect (screen coordinates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popout_rect: Option<Rect>,
    /// Monitor index the Pop-out last lived on; if that monitor is gone
    /// next launch, fall back to the main window's display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popout_display: Option<i32>,
    /// Last Pane-mode width (px).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_width: Option<u32>,
}

/// Phase 63: both floating-window kinds.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct FloatingWindows {
    #[serde(default)]
    pub browser: FloatingWindowState,
    #[serde(default)]
    pub filemanager: FloatingWindowState,
}

/// Phase 58: speech-to-text settings.
///
/// - `Webspeech` uses `window.SpeechRecognition` directly in the
///   frontend (Chromium / WebView2 ships with it; Firefox does not —
///   not a concern for Tauri's WebView2-only Windows build, but worth
///   flagging if we ever target Linux's WebKitGTK).
/// - `Local` POSTs the recorded audio bytes to a user-configurable
///   HTTP endpoint (whisper.cpp's server, faster-whisper-server,
///   OpenAI-compatible local proxies). Field shape mirrors OpenAI's
///   /v1/audio/transcriptions: multipart with `file` (audio bytes) +
///   `language`.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct SttSettings {
    /// Master on/off. Default off — opt-in feature, no mic access
    /// requested until the user flips this.
    #[serde(default)]
    pub enabled: bool,
    /// Which backend to use when recording. Defaults to Webspeech.
    #[serde(default)]
    pub backend: SttBackend,
    /// Required when `backend = Local`. Skipped when None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<String>,
    /// BCP-47 tag or "auto". Defaults to "auto" — the Web Speech API
    /// accepts it and most Whisper servers default to language
    /// detection when the param is missing or "auto".
    #[serde(default = "default_stt_lang")]
    pub language: String,
    /// Push-to-talk keybinding. Parsed by the existing shortcut-table
    /// helpers (Ctrl/Shift/Alt + key). Default Ctrl+Shift+M (M for
    /// microphone).
    #[serde(default = "default_stt_hotkey")]
    pub push_to_talk_hotkey: String,
}

/// Phase 58: backend choice. ts-rs lowercases via the serde attr so
/// the TS union is `"webspeech" | "local"`, matching the JSON.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub(crate) enum SttBackend {
    #[default]
    Webspeech,
    Local,
}

fn default_stt_lang() -> String {
    "auto".to_string()
}
fn default_stt_hotkey() -> String {
    "Ctrl+Shift+M".to_string()
}

/// Phase 39.B: tracks one-time data migrations so they run exactly
/// once. Each field is a "has-run" boolean defaulting to false.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct MigrationFlags {
    /// Phase 39: the auto_port_forward default flipped true→false. This
    /// migration flips all EXISTING workspaces' value to false to stop
    /// the post-connect auto-forward storm. Users re-enable per
    /// workspace; the flag keeps that choice from being undone.
    #[serde(default)]
    pub phase_39_auto_port_forward_default_flipped: bool,
    /// Phase 53 (rebased): the per-pane Browser / FileManager pane
    /// kinds were folded into workspace-level singleton windows. Any
    /// PaneKind::Browser or ::FileManager pane in a loaded
    /// workspaces.json is rewritten to PaneKind::Terminal on first
    /// load after upgrade. The flag stops the rewrite from running on
    /// every subsequent load (a Terminal pane that the user explicitly
    /// chose post-migration should NOT be touched).
    #[serde(default)]
    pub phase_53_remove_browser_filemanager_panes: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            preset: "tokyo-night".into(),
            accent: "#7aa2f7".into(),
            background: "#0e1116".into(),
            surface: "#161b22".into(),
            border: "#21262d".into(),
            text_primary: "#e6edf3".into(),
            text_secondary: "#7d8590".into(),
            success: "#4ec9b0".into(),
            warning: "#e0af68".into(),
            error: "#f7768e".into(),
            ansi: AnsiPalette::tokyo_night(),
        }
    }
}

impl Default for Font {
    fn default() -> Self {
        Self {
            ui_family: "system-ui".into(),
            ui_size_pt: 13,
            terminal_family: "Cascadia Mono".into(),
            terminal_size_pt: 13,
            web_font_url: None,
        }
    }
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            rtl_mode: default_rtl_mode(),
            use_ymux_tmux_config: true,
            mirror_arrows_rtl: true,
            auto_direction: true,
            tui_owns_bidi: false,
            auto_reset_on_connect: true,
            rtl: Some(RtlProfiles::default()),
        }
    }
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            matcher_mode: default_matcher_mode(),
            policy_enabled: true,
            auto_install: true,
            custom_block: Vec::new(),
            custom_gate: Vec::new(),
        }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            toast_enabled: true,
            toast_session_start: false,
            // v0.4.4: SessionEnd ("session closed") is a rare, meaningful
            // signal — default it ON so the user actually learns a session
            // ended. (SessionStart stays OFF; that hook is no longer even
            // registered.)
            toast_session_end: true,
            toast_stop: true,
            toast_notification: true,
            toast_gate: true,
            toast_block: true,
            pane_pulse_on_activity: true,
        }
    }
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            // Real manifest served as a static file from the repo's main
            // branch via raw.githubusercontent.com — no GitHub Pages, no
            // API rate limits. Updated as part of each release flow
            // (see RELEASING.md). A power user can override the URL
            // here without recompiling.
            manifest_url: Some(DEFAULT_MANIFEST_URL.into()),
            last_check_iso: None,
            last_seen_version: None,
            skipped_versions: Vec::new(),
            remind_after_iso: None,
            channel: default_channel(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::default(),
            font: Font::default(),
            terminal: TerminalSettings::default(),
            hooks: Hooks::default(),
            notifications: Notifications::default(),
            updates: Updates::default(),
            i18n: I18n::default(),
            shortcuts: Shortcuts::default(),
            claude: ClaudeOptions::default(),
            claude_usage: ClaudeUsageSettings::default(),
            brief: BriefOptions::default(),
            hooks_updates: HooksUpdates::default(),
            ssh_key_offer_disabled: false,
            auto_connect_on_workspace_select: true,
            restore_sessions_on_start: false,
            file_manager_remember_path: false,
            auto_destroy_empty_workspaces_days: None,
            migrations: MigrationFlags::default(),
            stt: SttSettings::default(),
            sidebar_mode: default_sidebar_mode(),
            floating_windows: FloatingWindows::default(),
            logs: LogsSettings::default(),
            persist_browser_sessions: true,
            hook_notifications: HookSettings::default(),
            theme_mode: default_theme_mode(),
            session_visibility: default_session_visibility(),
        }
    }
}

// ─── presets ───────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct PresetEntry {
    pub id: String,
    pub label: String,
    pub theme: Theme,
}

pub(crate) fn list_presets() -> Vec<PresetEntry> {
    vec![
        PresetEntry {
            id: "tokyo-night".into(),
            label: "Tokyo Night".into(),
            theme: Theme::default(),
        },
        PresetEntry {
            id: "dracula".into(),
            label: "Dracula".into(),
            theme: Theme {
                preset: "dracula".into(),
                accent: "#bd93f9".into(),
                background: "#282a36".into(),
                surface: "#21222c".into(),
                border: "#44475a".into(),
                text_primary: "#f8f8f2".into(),
                text_secondary: "#6272a4".into(),
                success: "#50fa7b".into(),
                warning: "#f1fa8c".into(),
                error: "#ff5555".into(),
                ansi: AnsiPalette::dracula(),
            },
        },
        PresetEntry {
            id: "solarized-dark".into(),
            label: "Solarized Dark".into(),
            theme: Theme {
                preset: "solarized-dark".into(),
                accent: "#268bd2".into(),
                background: "#002b36".into(),
                surface: "#073642".into(),
                border: "#586e75".into(),
                text_primary: "#eee8d5".into(),
                text_secondary: "#93a1a1".into(),
                success: "#859900".into(),
                warning: "#b58900".into(),
                error: "#dc322f".into(),
                ansi: AnsiPalette::solarized_dark(),
            },
        },
        PresetEntry {
            id: "nord".into(),
            label: "Nord".into(),
            theme: Theme {
                preset: "nord".into(),
                accent: "#88c0d0".into(),
                background: "#2e3440".into(),
                surface: "#3b4252".into(),
                border: "#4c566a".into(),
                text_primary: "#eceff4".into(),
                text_secondary: "#d8dee9".into(),
                success: "#a3be8c".into(),
                warning: "#ebcb8b".into(),
                error: "#bf616a".into(),
                ansi: AnsiPalette::nord(),
            },
        },
        PresetEntry {
            id: "solarized-light".into(),
            label: "Solarized Light".into(),
            theme: Theme {
                preset: "solarized-light".into(),
                accent: "#268bd2".into(),
                background: "#fdf6e3".into(),
                surface: "#eee8d5".into(),
                border: "#93a1a1".into(),
                text_primary: "#073642".into(),
                text_secondary: "#586e75".into(),
                success: "#859900".into(),
                warning: "#b58900".into(),
                error: "#dc322f".into(),
                ansi: AnsiPalette::solarized_light(),
            },
        },
        // ── Redesign directions (Claude Design handoff, 2026-07-15) ──────────
        // Four light-ground design systems shipped as themes. Colours are the
        // chrome palette; per-theme fonts + structural chrome (registration
        // marks, double rules, gold hairlines) and the waiting-ring colour live
        // in themes-redesign.css, keyed on [data-theme-preset]. These are
        // light-ground; the redesign CSS layer opts them out of the daylight
        // (data-theme-mode="light") override so the palette below always wins.
        PresetEntry {
            id: "industry".into(),
            label: "Industry".into(),
            theme: Theme {
                preset: "industry".into(),
                accent: "#5980a6".into(),
                background: "#f2f2f3".into(),
                surface: "#f5f5f8".into(),
                border: "#d4d4d7".into(),
                text_primary: "#1d1f20".into(),
                text_secondary: "#5d5d60".into(),
                success: "#3d7a54".into(),
                warning: "#9a6a00".into(),
                error: "#b3392f".into(),
                ansi: AnsiPalette { blue: "#3d6a94".into(), bright_blue: "#4d7fae".into(), ..AnsiPalette::daylight() },
            },
        },
        PresetEntry {
            id: "broadsheet".into(),
            label: "Broadsheet".into(),
            theme: Theme {
                preset: "broadsheet".into(),
                accent: "#0088b0".into(),
                background: "#f3f2f2".into(),
                surface: "#f8f4f4".into(),
                border: "#d7d3d3".into(),
                text_primary: "#201e1d".into(),
                text_secondary: "#605d5d".into(),
                success: "#2f7d4f".into(),
                warning: "#9a6a00".into(),
                error: "#c02d3c".into(),
                ansi: AnsiPalette { magenta: "#d6006c".into(), bright_magenta: "#aa0b56".into(), cyan: "#0e7a9b".into(), bright_cyan: "#006786".into(), ..AnsiPalette::daylight() },
            },
        },
        PresetEntry {
            id: "modernist".into(),
            label: "Modernist".into(),
            theme: Theme {
                preset: "modernist".into(),
                accent: "#ec3013".into(),
                background: "#f3f2f2".into(),
                surface: "#f8f4f4".into(),
                border: "#201e1d".into(),
                text_primary: "#201e1d".into(),
                text_secondary: "#605d5d".into(),
                success: "#2f7d4f".into(),
                warning: "#9a6a00".into(),
                error: "#ec3013".into(),
                ansi: AnsiPalette { red: "#c22a10".into(), bright_red: "#a3230d".into(), ..AnsiPalette::daylight() },
            },
        },
        PresetEntry {
            id: "classical".into(),
            label: "Classical".into(),
            theme: Theme {
                preset: "classical".into(),
                accent: "#b68235".into(),
                background: "#f3f2f2".into(),
                surface: "#f8f4f4".into(),
                border: "#d7d3d3".into(),
                text_primary: "#201f1d".into(),
                text_secondary: "#605d5d".into(),
                success: "#4a7a4a".into(),
                warning: "#b68235".into(),
                error: "#a3392f".into(),
                ansi: AnsiPalette { yellow: "#8a6215".into(), bright_yellow: "#7d5411".into(), ..AnsiPalette::daylight() },
            },
        },
        // ── Dark variants of the four directions (same identity, night ground).
        // Each keeps its direction's accent hue lifted for contrast on dark;
        // the redesign CSS layer shares fonts/radius/chrome via [data-theme-preset^=]
        // prefix selectors, so only the palette differs from the light sibling.
        PresetEntry {
            id: "industry-dark".into(),
            label: "Industry Dark".into(),
            theme: Theme {
                preset: "industry-dark".into(),
                accent: "#6f9fce".into(),
                background: "#12151a".into(),
                surface: "#1a1e25".into(),
                border: "#2b3742".into(),
                text_primary: "#dfe6ee".into(),
                text_secondary: "#8b97a4".into(),
                success: "#4ec9b0".into(),
                warning: "#e0af68".into(),
                error: "#f7768e".into(),
                ansi: AnsiPalette::industry_dark(),
            },
        },
        PresetEntry {
            id: "broadsheet-dark".into(),
            label: "Broadsheet Dark".into(),
            theme: Theme {
                preset: "broadsheet-dark".into(),
                accent: "#35b6df".into(),
                background: "#17161a".into(),
                surface: "#201e24".into(),
                border: "#37333b".into(),
                text_primary: "#ece7e4".into(),
                text_secondary: "#a49d9d".into(),
                success: "#4ec9b0".into(),
                warning: "#e0af68".into(),
                error: "#ff6b8a".into(),
                ansi: AnsiPalette::broadsheet_dark(),
            },
        },
        PresetEntry {
            id: "modernist-dark".into(),
            label: "Modernist Dark".into(),
            theme: Theme {
                preset: "modernist-dark".into(),
                accent: "#ff563c".into(),
                background: "#141312".into(),
                surface: "#1d1b1a".into(),
                border: "#e7e3e1".into(),
                text_primary: "#f3f2f2".into(),
                text_secondary: "#a49d9d".into(),
                success: "#4ec9b0".into(),
                warning: "#e0af68".into(),
                error: "#ff563c".into(),
                ansi: AnsiPalette::modernist_dark(),
            },
        },
        PresetEntry {
            id: "classical-dark".into(),
            label: "Classical Dark".into(),
            theme: Theme {
                preset: "classical-dark".into(),
                accent: "#c99a4a".into(),
                background: "#171512".into(),
                surface: "#201d18".into(),
                border: "#38322a".into(),
                text_primary: "#efe9df".into(),
                text_secondary: "#a99f90".into(),
                success: "#7fae6a".into(),
                warning: "#d8b06a".into(),
                error: "#d9736a".into(),
                ansi: AnsiPalette::classical_dark(),
            },
        },
    ]
}

pub(crate) fn get_preset(id: &str) -> Option<Theme> {
    list_presets().into_iter().find(|p| p.id == id).map(|p| p.theme)
}

// ─── disk I/O ──────────────────────────────────────────────────────────────

fn settings_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("settings.json"))
}

fn save_to_disk(file: &Settings) -> Result<(), String> {
    use std::io::Write as _;
    let path = settings_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("settings.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    // Every settings write funnels through here (mutate / RPC patch / reset),
    // so this is the one choke point where the logger threshold tracks the
    // persisted value.
    ymux_core::set_log_level(ymux_core::LogLevel::from_str(&file.logs.level));
    log_debug("SETTINGS", &format!("settings save: {} bytes -> {:?}", text.len(), path));
    Ok(())
}

/// Public wrapper used by other modules (updater) that want to atomically
/// persist the current settings without going through `mutate` (e.g. they
/// already hold the lock).
pub(crate) fn save_to_disk_pub(file: &Settings) -> Result<(), String> {
    save_to_disk(file)
}

/// The canonical update manifest (raw.githubusercontent — no API rate limit).
const DEFAULT_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/yyhezkel/ymux/main/manifest.json";

/// One-shot fixups for an on-disk settings.json written by an older ymux.
/// Returns true if anything changed (so the caller re-persists). Phase 71:
/// an early default shipped a placeholder `ymux.example.com` manifest URL
/// that can never resolve — it caused the recurring `hooks-check: fetch
/// manifest failed` DNS spam. Replace any example/placeholder host with the
/// real default so update checks (and the version banner) work.
fn migrate_settings(s: &mut Settings) -> bool {
    let mut changed = false;
    let is_placeholder = s
        .updates
        .manifest_url
        .as_deref()
        .map(|u| {
            let l = u.to_ascii_lowercase();
            u.trim().is_empty()
                || l.contains("example.com")
                || l.contains("example.org")
                || l.contains("your-domain")
                || l.contains("changeme")
        })
        .unwrap_or(true);
    if is_placeholder {
        s.updates.manifest_url = Some(DEFAULT_MANIFEST_URL.to_string());
        changed = true;
        log_info("SETTINGS", "settings: migrated placeholder manifest_url → default");
    }
    if migrate_rtl_profiles(&mut s.terminal) {
        changed = true;
        log_info(
            "SETTINGS",
            "settings: seeded terminal.rtl {local,remote} from the pre-split flat RTL fields",
        );
    }
    changed
}

pub(crate) fn load_from_disk() -> Result<Settings, String> {
    let path = settings_path()?;
    if !path.exists() {
        let s = Settings::default();
        // Write the defaults so a power user can hand-edit without first
        // discovering it in the UI. Best-effort — don't fail load if the
        // initial write hits a permissions issue.
        if let Err(e) = save_to_disk(&s) {
            log_warn("SETTINGS", &format!("settings: initial save failed: {e}"));
        }
        return Ok(s);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {:?}: {e}", path))?;
    let parsed: Result<Settings, _> = serde_json::from_str(text.trim_start_matches('\u{FEFF}'));
    match parsed {
        Ok(mut s) => {
            if migrate_settings(&mut s) {
                // Persist the migrated values so the fix sticks (and the
                // placeholder-URL spam stops for good). Best-effort.
                let _ = save_to_disk(&s);
            }
            Ok(s)
        }
        Err(e) => {
            // Forward-compat: if the schema grew, fall back to defaults rather
            // than refusing to start. The user can re-save from the UI to
            // upgrade their on-disk file.
            log_warn("SETTINGS", &format!(
                "settings: parse {:?} failed ({e}) — using defaults",
                path
            ));
            Ok(Settings::default())
        }
    }
}

fn persist(state: &AppState) -> Result<(), String> {
    let s = state.settings.lock().unwrap().clone();
    save_to_disk(&s)
}

fn mutate<F: FnOnce(&mut Settings) -> Result<(), String>>(
    state: &AppState,
    app: &AppHandle,
    f: F,
) -> Result<Settings, String> {
    let old_level;
    {
        let mut s = state.settings.lock().unwrap();
        old_level = s.logs.level.clone();
        f(&mut s)?;
    }
    persist(state)?;
    let s = state.settings.lock().unwrap().clone();
    // Level changed → converge the remote fleet (best-effort, background).
    // The local threshold is already applied inside save_to_disk.
    if s.logs.level != old_level {
        crate::log_sync::push_log_level_to_all(state, s.logs.level.clone());
    }
    let _ = app.emit("settings:changed", &s);
    Ok(s)
}

/// Phase 85.C: record whether the workspace Browser is currently popped
/// out into its own OS window, and where that window was.
///
/// `floating_windows` is **Rust-owned** — no UI writes it (see the carry
/// in `settings_save`), so this is the only writer. Best-effort by
/// design: failing to remember a window position must never fail the
/// window operation itself.
pub(crate) fn set_browser_popout(
    state: &AppState,
    app: &AppHandle,
    popped_out: bool,
    rect: Option<Rect>,
    display: Option<i32>,
) {
    let res = mutate(state, app, |s| {
        s.floating_windows.browser.mode = if popped_out {
            FloatingWindowMode::PopOut
        } else {
            FloatingWindowMode::Float
        };
        if rect.is_some() {
            s.floating_windows.browser.popout_rect = rect;
        }
        if display.is_some() {
            s.floating_windows.browser.popout_display = display;
        }
        Ok(())
    });
    if let Err(e) = res {
        log_warn("SETTINGS", &format!("set_browser_popout failed: {e}"));
    }
}

/// The remembered Pop-out geometry for the workspace Browser, if any.
pub(crate) fn browser_popout_geometry(state: &AppState) -> (Option<Rect>, Option<i32>) {
    let s = state.settings.lock().unwrap();
    (
        s.floating_windows.browser.popout_rect,
        s.floating_windows.browser.popout_display,
    )
}

// ─── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn settings_load(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn settings_save(
    state: State<'_, AppState>,
    app: AppHandle,
    mut settings: Settings,
) -> Result<Settings, String> {
    mutate(&state, &app, |s| {
        // 2026-08-19: a save REPLACES the whole document, so a client holding a
        // copy from before `terminal.rtl` was seeded wipes it. Yossi's
        // settings.json went 3870 -> 3521 bytes and lost the block entirely,
        // while his running UI still showed the profile it had chosen — so a
        // good part of a day's RTL testing ran against settings that were not
        // actually in force.
        //
        // The UI has no "remove the rtl block" action: it is only ever ADDED,
        // by `migrate_rtl_profiles` or by editing a profile. So `None` from a
        // client cannot mean "delete it" — it can only mean "my copy predates
        // it", and the stored value is the better answer. Keeping it is not a
        // merge policy for the whole document, just for the one field where
        // absence is provably not a decision (same reasoning as the
        // `tui_owns_bidi` carry in `migrate_rtl_profiles`).
        if settings.terminal.rtl.is_none() {
            if let Some(existing) = s.terminal.rtl.clone() {
                log_info(
                    "SETTINGS",
                    "settings_save: client sent no terminal.rtl — keeping the stored profiles",
                );
                settings.terminal.rtl = Some(existing);
            }
        }
        // Phase 85.C: `floating_windows` is Rust-owned — the only writer
        // is `set_browser_popout`, driven by the window itself opening
        // and closing. No UI surface edits it, so a value arriving from
        // a client can only be a stale echo of a `settings_load`, never
        // a decision. Always keep the stored one. Without this, popping
        // the Browser out and then hitting Save anywhere in Settings
        // would silently reset the mode to `float` and drop the
        // remembered rect — the same shape of bug as the `terminal.rtl`
        // wipe above, which cost most of a day.
        settings.floating_windows = s.floating_windows.clone();
        *s = settings;
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn settings_get_presets() -> Vec<PresetEntry> {
    list_presets()
}

#[tauri::command]
pub(crate) fn settings_apply_preset(
    state: State<'_, AppState>,
    app: AppHandle,
    preset: String,
) -> Result<Settings, String> {
    let theme = get_preset(&preset).ok_or_else(|| format!("unknown preset {preset}"))?;
    mutate(&state, &app, |s| {
        s.theme = theme;
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn settings_reset(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Settings, String> {
    mutate(&state, &app, |s| {
        *s = Settings::default();
        Ok(())
    })
}

/// One entry in the Settings font picker.
///
/// `installed` is the whole point: the picker always offers a curated
/// baseline (so it is never empty and so the defaults are pickable before
/// enumeration succeeds), but a baseline name is NOT necessarily present on
/// the machine — `JetBrains Mono` and `Inter` ship with nothing. Picking a
/// missing family used to silently fall through the CSS fallback chain in
/// `quoteFamily()` back to Cascadia Mono, i.e. the default, i.e. "nothing
/// happened". The frontend renders this flag as ✅ / ⚠️ so the user can see
/// why, instead of concluding the setting is broken.
#[derive(Clone, Serialize)]
pub(crate) struct FontEntry {
    pub name: String,
    pub installed: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct FontFamilies {
    pub ui: Vec<FontEntry>,
    pub mono: Vec<FontEntry>,
}

/// CSS generic families. They always resolve (the browser picks something),
/// so they are reported installed regardless of what the registry says —
/// flagging them ⚠️ would be a lie.
const CSS_GENERIC_FAMILIES: &[&str] = &[
    "system-ui",
    "ui-monospace",
    "ui-sans-serif",
    "ui-serif",
    "monospace",
    "sans-serif",
    "serif",
    "cursive",
    "fantasy",
];

/// Weight / style words that a registry face name appends to its family.
/// Deliberately does NOT include words that start a genuinely different
/// family: "New" ("Courier New"), "Variable" ("Segoe UI Variable"), "Code"
/// / "Mono" ("Cascadia Code" vs "Cascadia Mono"), "Nerd" ("JetBrainsMono
/// Nerd Font" is its own CSS family and must stay separately selectable).
const FONT_STYLE_WORDS: &[&str] = &[
    "regular",
    "bold",
    "italic",
    "oblique",
    "light",
    "extralight",
    "ultralight",
    "semilight",
    "thin",
    "medium",
    "semibold",
    "demibold",
    "extrabold",
    "ultrabold",
    "black",
    "heavy",
    "book",
    "roman",
    "retina",
    "condensed",
    "semicondensed",
    "extracondensed",
    // Nerd Font patched faces abbreviate the style word to fit the 31-char
    // name-table limit — "FiraCode Nerd Font Mono Reg", not "... Regular".
    // Without these the family reads as uninstalled even right after a
    // successful install.
    "reg",
    "med",
    "ret",
    "bd",
    "sembd",
    "semibd",
    "extbd",
    "exbd",
    "blk",
    "lt",
    "xlt",
    "extlt",
    "th",
    "obl",
    "ital",
    "cond",
];

/// True if `name` is `base` plus trailing style/weight words only — i.e.
/// `name` is a variant FACE of the family `base` ("JetBrains Mono ExtraBold"
/// vs "JetBrains Mono"), not a different family that merely shares a prefix.
///
/// The style-word check is load-bearing, not decoration: a plain
/// prefix-plus-space rule makes "Courier New" answer a request for
/// "Courier", which would mark an uninstalled family as installed — exactly
/// the class of silent lie this whole change exists to remove.
fn extends_family(name: &str, base: &str) -> bool {
    if base.is_empty() || base.len() >= name.len() {
        return false;
    }
    if !name.is_char_boundary(base.len()) || !name[..base.len()].eq_ignore_ascii_case(base) {
        return false;
    }
    let rest = &name[base.len()..];
    if !rest.starts_with(' ') {
        return false;
    }
    let mut words = rest.split_whitespace().peekable();
    if words.peek().is_none() {
        return false; // trailing whitespace only — not a distinct face
    }
    words.all(|w| {
        FONT_STYLE_WORDS
            .iter()
            .any(|s| s.eq_ignore_ascii_case(w))
    })
}

/// Heuristic split of the enumerated set into the terminal picker (mono) and
/// the UI picker. Name-based — the registry doesn't tell us pitch — so it is
/// a best guess, deliberately generous on the mono side.
pub(crate) fn looks_monospace(name: &str) -> bool {
    let lower = name.to_lowercase();
    const HINTS: &[&str] = &[
        "mono",
        "consolas",
        "cascadia",
        "courier",
        "menlo",
        // "meslo" and "nerd" are load-bearing, not padding: MesloLGS NF
        // matches none of the other hints, so without them an installed
        // Nerd Font lands in the UI list and stays unpickable as a terminal
        // font — install succeeds, ⚠️ never clears. `fonts::tests` guards
        // this for every catalog family.
        "meslo",
        "nerd",
        "fira",
        "jetbrains",
        "iosevka",
        "hack",
        "source code",
        "lucida console",
    ];
    // "Monotype Corsiva" is a script face, not a mono one — it only matched
    // because "Monotype" contains "mono".
    if lower.contains("monotype") && !lower.contains("mono ") {
        return false;
    }
    HINTS.iter().any(|h| lower.contains(h))
}

/// True if `family` is present in the enumerated set.
///
/// Registry face names carry weight/style suffixes (`JetBrains Mono
/// ExtraBold`) while CSS wants the family (`JetBrains Mono`), so an exact
/// match is not enough: a prefix match on a word boundary counts too. The
/// boundary check is what stops `Courier` from matching `Courier New`.
pub(crate) fn family_is_installed(family: &str, enumerated: &[String]) -> bool {
    let want = family.trim();
    if want.is_empty() {
        return false;
    }
    if CSS_GENERIC_FAMILIES
        .iter()
        .any(|g| g.eq_ignore_ascii_case(want))
    {
        return true;
    }
    enumerated
        .iter()
        // "JetBrains Mono ExtraBold" satisfies a request for "JetBrains Mono".
        .any(|have| have.eq_ignore_ascii_case(want) || extends_family(have, want))
}

/// Best-effort enumeration of installed font families on Windows. Reads both
/// `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts` (machine-wide)
/// and the `HKCU` counterpart (per-user installs — the no-admin path, and
/// the one our own installer writes to). If anything fails (non-Windows,
/// registry locked, etc.) we fall back to a curated baseline so the picker
/// is always usable; in that case nothing can be verified, so every entry is
/// reported installed rather than drowning the list in false ⚠️.
#[tauri::command]
pub(crate) fn list_system_fonts() -> Result<FontFamilies, String> {
    let baseline_ui = vec![
        "system-ui".to_string(),
        "Segoe UI Variable".to_string(),
        "Segoe UI".to_string(),
        "Inter".to_string(),
        "Roboto".to_string(),
        "Tahoma".to_string(),
        "Arial".to_string(),
    ];
    // Every family the install catalog can supply is listed here even when
    // absent, so it shows up flagged ⚠️ with an Install button next to it.
    // A font that only appears once it is installed can never be discovered
    // — which is the situation MesloLGS NF was in.
    let mut baseline_mono = vec![
        "Cascadia Mono".to_string(),
        "Cascadia Code".to_string(),
        "Consolas".to_string(),
        "Courier New".to_string(),
        "ui-monospace".to_string(),
        "monospace".to_string(),
    ];
    for family in crate::fonts::installable_families() {
        if !baseline_mono.iter().any(|b| b.eq_ignore_ascii_case(family)) {
            baseline_mono.push(family.to_string());
        }
    }
    // `all` is the MATCH set — it deliberately keeps weight-suffixed faces
    // ("JetBrains Mono ExtraBold") so `family_is_installed` can satisfy a
    // family whose regular face isn't separately registered.
    let mut all: Vec<String> = enumerate_windows_fonts().unwrap_or_default();
    all.sort();
    all.dedup();
    // Enumeration failed (or non-Windows): we cannot tell installed from
    // missing, so claim everything installed — a picker full of ⚠️ that we
    // cannot substantiate is worse than no badges at all.
    if all.is_empty() {
        let assume_installed = |names: Vec<String>| -> Vec<FontEntry> {
            names
                .into_iter()
                .map(|name| FontEntry {
                    name,
                    installed: true,
                })
                .collect()
        };
        return Ok(FontFamilies {
            ui: assume_installed(baseline_ui),
            mono: assume_installed(baseline_mono),
        });
    }
    // `display` is what the picker SHOWS: drop any face that is just a
    // weight/style variant of a family already in the set, so the user sees
    // "JetBrains Mono" once instead of once per weight.
    let display: Vec<String> = all
        .iter()
        .filter(|n| !all.iter().any(|base| extends_family(n, base)))
        .cloned()
        .collect();
    let mono: Vec<String> = display.iter().filter(|n| looks_monospace(n)).cloned().collect();
    let ui: Vec<String> = display
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            !looks_monospace(n)
                && !lower.contains("symbol")
                && !lower.contains("emoji")
                && !lower.contains("wingdings")
        })
        .cloned()
        .collect();
    // `head` is the curated baseline (may contain names that are NOT on this
    // machine — those get flagged); `tail` came out of the registry, so it is
    // installed by construction.
    let merge = |head: Vec<String>, tail: Vec<String>| -> Vec<FontEntry> {
        let mut out: Vec<FontEntry> = head
            .into_iter()
            .map(|name| {
                let installed = family_is_installed(&name, &all);
                FontEntry { name, installed }
            })
            .collect();
        for t in tail {
            if !out.iter().any(|h| h.name.eq_ignore_ascii_case(&t)) {
                out.push(FontEntry {
                    name: t,
                    installed: true,
                });
            }
        }
        out
    };
    Ok(FontFamilies {
        ui: merge(baseline_ui, ui),
        mono: merge(baseline_mono, mono),
    })
}

/// Static PowerShell source for {@link enumerate_windows_fonts}. Both font
/// hives, machine-wide first. HKCU is where a per-user (no-admin) install
/// lands — including the one this app performs — and reading only HKLM used
/// to make those fonts invisible to the picker forever.
#[cfg(target_os = "windows")]
const ENUM_FONTS_PS: &str = "\
$ErrorActionPreference = 'SilentlyContinue'; \
foreach ($k in @( \
  'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts', \
  'HKCU:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts' \
)) { \
  if (Test-Path $k) { \
    Get-ItemProperty $k | Get-Member -MemberType NoteProperty | \
    Where-Object { $_.Name -notmatch '^PS' } | ForEach-Object { $_.Name } \
  } \
}";

#[cfg(target_os = "windows")]
fn enumerate_windows_fonts() -> Option<Vec<String>> {
    // Spawn a tiny PowerShell call rather than pulling in winreg as a dep.
    // Output is one registry value name per line, e.g. "Cascadia Code
    // (TrueType)". Best-effort: errors → None. The command is a fixed
    // literal — no interpolation of anything user-supplied (Rule #3).
    use std::process::Command;
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ENUM_FONTS_PS])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut families: Vec<String> = Vec::new();
    for line in text.lines() {
        // Strip the format tag the registry appends to the value name.
        let mut name = line.trim();
        for tag in [
            " (TrueType)",
            " (OpenType)",
            " (VGA res)",
            " (All res)",
            " (8514/a res)",
        ] {
            if let Some(stripped) = name.strip_suffix(tag) {
                name = stripped.trim_end();
            }
        }
        if name.is_empty() {
            continue;
        }
        // Bitmap .fon entries pack several faces into one value name
        // ("MS Sans Serif 8,10,12 & MS Serif"); split them back apart.
        for part in name.split('&') {
            let part = part.trim();
            if !part.is_empty() {
                families.push(part.to_string());
            }
        }
    }
    // Add the bare family for every variant face ("Cascadia Code Regular" →
    // "Cascadia Code") so the picker can show one row per family. The
    // suffixed form is kept too: it is what `family_is_installed` matches
    // against for a family whose regular face isn't separately registered.
    let stripped_forms: Vec<String> = families
        .iter()
        .filter_map(|name| strip_style_suffix(name))
        .collect();
    families.extend(stripped_forms);
    Some(families)
}

/// Drop trailing style/weight words from a face name, yielding the family
/// ("Segoe UI Semibold" → "Segoe UI"). None when there is nothing to strip,
/// or when stripping would consume the whole name.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn strip_style_suffix(name: &str) -> Option<String> {
    let mut base = name.trim_end();
    // Note: a missing space must BREAK, not return — "Consolas Bold Italic"
    // strips down to a single word and still has a family to report.
    while let Some(cut) = base.rfind(' ') {
        let word = &base[cut + 1..];
        if !FONT_STYLE_WORDS.iter().any(|s| s.eq_ignore_ascii_case(word)) {
            break;
        }
        base = base[..cut].trim_end();
    }
    if base.is_empty() || base.len() == name.trim_end().len() {
        None
    } else {
        Some(base.to_string())
    }
}

/// Unix counterpart of the registry sweep above. There is no font registry
/// here, so scan the per-user and system font directories and read each
/// face's `name` table — reusing `fonts::full_font_name`, the same parser
/// the installer already uses to label what it writes.
///
/// Returning `None` (the old behaviour) means "enumeration is impossible",
/// and the caller responds by assuming every baseline family is installed.
/// So on macOS the picker only ever offered the hardcoded list and could
/// not see a single font the user actually had.
///
/// Non-recursive and capped: this runs on a settings-panel open, and a
/// deep walk of /System/Library/Fonts is not worth the latency. `None` on
/// a total miss keeps the old "assume installed" fallback rather than
/// badging every family as missing on the strength of an empty scan.
#[cfg(not(target_os = "windows"))]
fn enumerate_windows_fonts() -> Option<Vec<String>> {
    const MAX_FACES: usize = 2000;
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        roots.push(home.join("Library").join("Fonts"));
        #[cfg(not(target_os = "macos"))]
        roots.push(home.join(".local").join("share").join("fonts"));
        #[cfg(not(target_os = "macos"))]
        roots.push(home.join(".fonts"));
    }
    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/Library/Fonts"));
        roots.push(PathBuf::from("/System/Library/Fonts"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        roots.push(PathBuf::from("/usr/share/fonts"));
        roots.push(PathBuf::from("/usr/local/share/fonts"));
    }

    let mut names: Vec<String> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if names.len() >= MAX_FACES {
                break;
            }
            let path = entry.path();
            let is_font = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let e = e.to_ascii_lowercase();
                    e == "ttf" || e == "otf" || e == "ttc"
                })
                .unwrap_or(false);
            if !is_font {
                continue;
            }
            // Only the head of the file is needed for the name table, but
            // font files are small enough that a full read is simpler than
            // a seeking parser — and `full_font_name` wants a slice.
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Some(name) = crate::fonts::full_font_name(&bytes) {
                names.push(name);
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

// ─── helpers exposed to RPC dispatch ───────────────────────────────────────

/// Apply a partial JSON patch (object) on top of the current settings,
/// merging recursively. Fields absent from the patch are preserved.
pub(crate) fn rpc_patch(
    state: &AppState,
    app: &AppHandle,
    patch: Value,
) -> Result<Settings, String> {
    mutate(state, app, |s| {
        let mut as_value = serde_json::to_value(&*s).map_err(|e| e.to_string())?;
        merge_in_place(&mut as_value, &patch);
        let next: Settings =
            serde_json::from_value(as_value).map_err(|e| format!("merged settings invalid: {e}"))?;
        *s = next;
        Ok(())
    })
}

/// Apply a single dotted-path setting (e.g. `theme.preset = "dracula"`).
/// Strings, numbers, and booleans are accepted; everything else falls back
/// to JSON parsing of the string value.
pub(crate) fn rpc_set_path(
    state: &AppState,
    app: &AppHandle,
    path: &str,
    value: &str,
) -> Result<Settings, String> {
    let parsed: Value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into()));
    let mut patch = Value::Object(Default::default());
    insert_at_path(&mut patch, path, parsed)?;
    rpc_patch(state, app, patch)
}

pub(crate) fn rpc_apply_preset(
    state: &AppState,
    app: &AppHandle,
    preset: &str,
) -> Result<Settings, String> {
    let theme = get_preset(preset).ok_or_else(|| format!("unknown preset {preset}"))?;
    mutate(state, app, |s| {
        s.theme = theme;
        Ok(())
    })
}

fn merge_in_place(into: &mut Value, from: &Value) {
    if let (Value::Object(a), Value::Object(b)) = (&mut *into, from) {
        for (k, v) in b {
            match a.get_mut(k) {
                Some(existing) if existing.is_object() && v.is_object() => {
                    merge_in_place(existing, v);
                }
                _ => {
                    a.insert(k.clone(), v.clone());
                }
            }
        }
    } else {
        *into = from.clone();
    }
}

fn insert_at_path(root: &mut Value, path: &str, leaf: Value) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("empty path".into());
    }
    let mut cur = root;
    for (i, p) in parts.iter().enumerate() {
        if !cur.is_object() {
            *cur = Value::Object(Default::default());
        }
        let obj = cur.as_object_mut().unwrap();
        if i == parts.len() - 1 {
            obj.insert((*p).into(), leaf.clone());
            return Ok(());
        }
        cur = obj
            .entry((*p).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    Ok(())
}

#[cfg(test)]
mod font_tests {
    use super::{
        extends_family, family_is_installed, list_system_fonts, looks_monospace,
        strip_style_suffix,
    };

    fn installed() -> Vec<String> {
        vec![
            "Consolas".to_string(),
            "Courier New".to_string(),
            "JetBrains Mono ExtraBold".to_string(),
            "Segoe UI".to_string(),
        ]
    }

    #[test]
    fn exact_match_counts() {
        assert!(family_is_installed("Consolas", &installed()));
        assert!(family_is_installed("consolas", &installed()), "case-insensitive");
    }

    #[test]
    fn variant_face_satisfies_its_family() {
        // Only the ExtraBold face is registered, but the family IS present.
        assert!(family_is_installed("JetBrains Mono", &installed()));
    }

    #[test]
    fn missing_family_is_reported_missing() {
        assert!(!family_is_installed("Fira Code", &installed()));
        assert!(!family_is_installed("Inter", &installed()));
    }

    #[test]
    fn sibling_family_is_not_a_variant() {
        // "Courier New" is its own family, NOT a face of "Courier" — so
        // having it installed must not satisfy a request for "Courier".
        assert!(!family_is_installed("Courier", &installed()));
        assert!(!extends_family("Courier New", "Courier"));
        assert!(!extends_family("CourierNew", "Courier"));
        // Same trap, real cases from the picker's own baseline.
        assert!(!extends_family("Cascadia Code", "Cascadia"));
        assert!(!extends_family("Segoe UI Variable", "Segoe UI"));
        assert!(
            !extends_family("JetBrainsMono Nerd Font", "JetBrainsMono"),
            "Nerd Font builds are a separate CSS family and must stay pickable"
        );
    }

    #[test]
    fn style_suffixes_are_variants() {
        assert!(extends_family("Segoe UI Semibold", "Segoe UI"));
        assert!(extends_family("Fira Code Retina", "Fira Code"));
        assert!(extends_family("Consolas Bold Italic", "Consolas"));
        assert!(extends_family("Consolas bold", "Consolas"), "case-insensitive");
        assert!(!extends_family("Consolas ", "Consolas"), "whitespace only");
    }

    #[test]
    fn css_generics_always_resolve() {
        for g in ["monospace", "system-ui", "ui-monospace", "sans-serif"] {
            assert!(
                family_is_installed(g, &[]),
                "{g} is a CSS generic and always resolves"
            );
        }
    }

    #[test]
    fn empty_family_is_not_installed() {
        assert!(!family_is_installed("", &installed()));
        assert!(!family_is_installed("   ", &installed()));
    }

    #[test]
    fn strips_style_words_down_to_the_family() {
        assert_eq!(
            strip_style_suffix("Cascadia Code Regular").as_deref(),
            Some("Cascadia Code")
        );
        assert_eq!(
            strip_style_suffix("Consolas Bold Italic").as_deref(),
            Some("Consolas"),
            "consumes a run of style words"
        );
        // Nothing to strip → None, so the caller adds no redundant entry.
        assert_eq!(strip_style_suffix("Consolas"), None);
        assert_eq!(strip_style_suffix("Courier New"), None);
        assert_eq!(strip_style_suffix("Bold"), None, "would consume the whole name");
    }

    #[test]
    fn monospace_heuristic_skips_monotype() {
        // "Monotype Corsiva" is a script face that only matched because
        // "Monotype" contains "mono".
        assert!(!looks_monospace("Monotype Corsiva"));
        assert!(looks_monospace("Cascadia Mono"));
        assert!(looks_monospace("JetBrainsMono Nerd Font"));
        assert!(looks_monospace("Fira Code"));
        assert!(!looks_monospace("Segoe UI"));
    }

    /// Live check against the real machine: the command must succeed, must
    /// offer the baseline, and must not claim a family is installed unless
    /// enumeration actually backs it.
    #[test]
    fn list_system_fonts_is_self_consistent() {
        let f = list_system_fonts().expect("command is infallible by design");
        assert!(!f.mono.is_empty() && !f.ui.is_empty());
        // Baseline is always offered so the picker is never empty.
        assert!(f.mono.iter().any(|e| e.name == "Consolas"));
        // One row per family — no duplicate names within a list.
        for list in [&f.ui, &f.mono] {
            let mut names: Vec<String> = list.iter().map(|e| e.name.to_lowercase()).collect();
            names.sort();
            let before = names.len();
            names.dedup();
            assert_eq!(before, names.len(), "picker must not list a family twice");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        migrate_rtl_profiles, migrate_settings, HookType, MigrationFlags, RtlProfile, RtlProfiles,
        Settings, TerminalSettings, DEFAULT_MANIFEST_URL,
    };

    #[test]
    fn migrates_placeholder_manifest_url() {
        let mut s = Settings::default();
        s.updates.manifest_url = Some("https://ymux.example.com/manifest.json".into());
        assert!(migrate_settings(&mut s), "should report a change");
        assert_eq!(s.updates.manifest_url.as_deref(), Some(DEFAULT_MANIFEST_URL));
    }

    #[test]
    fn migrates_empty_manifest_url() {
        let mut s = Settings::default();
        s.updates.manifest_url = Some("".into());
        assert!(migrate_settings(&mut s));
        assert_eq!(s.updates.manifest_url.as_deref(), Some(DEFAULT_MANIFEST_URL));
    }

    #[test]
    fn leaves_real_manifest_url_alone() {
        let mut s = Settings::default();
        s.updates.manifest_url = Some("https://example-server.io/my.json".into());
        // "example-server.io" is NOT a placeholder host (no "example.com").
        assert!(!migrate_settings(&mut s));
        assert_eq!(s.updates.manifest_url.as_deref(), Some("https://example-server.io/my.json"));
    }

    #[test]
    fn migration_flags_default_is_not_run() {
        let f = MigrationFlags::default();
        assert!(!f.phase_39_auto_port_forward_default_flipped);
        // Settings default carries an un-run MigrationFlags.
        assert_eq!(Settings::default().migrations, f);
    }

    #[test]
    fn migration_flag_round_trips() {
        let mut s = Settings::default();
        s.migrations.phase_39_auto_port_forward_default_flipped = true;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(back.migrations.phase_39_auto_port_forward_default_flipped);
    }

    #[test]
    fn pre_39b_settings_json_defaults_flag_false() {
        // A settings.json written before 39.B has no `migrations` key.
        let json = r##"{"version":1,"theme":{"preset":"x","accent":"#000","background":"#000","surface":"#000","border":"#000","text_primary":"#000","text_secondary":"#000","success":"#000","warning":"#000","error":"#000","ansi":{"black":"#000","red":"#000","green":"#000","yellow":"#000","blue":"#000","magenta":"#000","cyan":"#000","white":"#000","bright_black":"#000","bright_red":"#000","bright_green":"#000","bright_yellow":"#000","bright_blue":"#000","bright_magenta":"#000","bright_cyan":"#000","bright_white":"#000"}},"font":{"ui_family":"x","ui_size_pt":13,"terminal_family":"x","terminal_size_pt":13},"terminal":{"cursor_style":"bar","scrollback_lines":1000,"bidi_enabled":true,"allow_proposed_api":true},"hooks":{"enabled":true,"agents":[],"policy_preset":"default"},"notifications":{"toast_enabled":true,"sound_enabled":false},"updates":{"check_on_startup":true,"auto_download":false}}"##;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.migrations.phase_39_auto_port_forward_default_flipped);
        // Phase 41: the same pre-41 JSON has no auto_connect field either —
        // serde(default = "default_true") must fill it in as true.
        assert!(
            s.auto_connect_on_workspace_select,
            "missing auto_connect_on_workspace_select must default to true"
        );
    }

    #[test]
    fn auto_connect_default_is_true_and_round_trips() {
        assert!(Settings::default().auto_connect_on_workspace_select);
        let mut s = Settings::default();
        s.auto_connect_on_workspace_select = false;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(!back.auto_connect_on_workspace_select);
    }

    #[test]
    fn phase80_restore_flags_default_off_and_round_trip() {
        // Both are opt-in: an install that never opens Settings must keep the
        // pre-80 startup behavior, and an existing settings.json (no such
        // keys) must not start restoring sessions because of an update.
        let d = Settings::default();
        assert!(!d.restore_sessions_on_start);
        assert!(!d.file_manager_remember_path);

        let mut s = Settings::default();
        s.restore_sessions_on_start = true;
        s.file_manager_remember_path = true;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(back.restore_sessions_on_start);
        assert!(back.file_manager_remember_path);
    }

    #[test]
    fn beta3_hook_settings_default_is_interactive_four() {
        let s = Settings::default().hook_notifications;
        // Interactive-4 enabled.
        assert!(s.enabled_types.contains(&HookType::PreToolUse));
        assert!(s.enabled_types.contains(&HookType::Notification));
        assert!(s.enabled_types.contains(&HookType::Stop));
        assert!(s.enabled_types.contains(&HookType::SessionEnd));
        // Observability off by default.
        assert!(!s.enabled_types.contains(&HookType::PostToolUse));
        assert!(!s.enabled_types.contains(&HookType::SubagentStop));
        assert!(!s.enabled_types.contains(&HookType::UserPromptSubmit));
        assert!(!s.enabled_types.contains(&HookType::PreCompact));
        assert!(!s.enabled_types.contains(&HookType::SessionStart));
        // Interactive-3 sound-on.
        assert!(s.sound_types.contains(&HookType::PreToolUse));
        assert!(s.sound_types.contains(&HookType::Notification));
        assert!(s.sound_types.contains(&HookType::Stop));
        // SessionEnd enabled but silent by default.
        assert!(!s.sound_types.contains(&HookType::SessionEnd));
        assert!(s.sound_master);
    }

    #[test]
    fn beta3_pre_hook_notifications_settings_json_populates_defaults() {
        // A settings.json written before beta.3 has no `hook_notifications`
        // key. The migration must fill it with the interactive-4 default
        // (see `default_hook_notifications`).
        let json = r##"{"version":1,"theme":{"preset":"x","accent":"#000","background":"#000","surface":"#000","border":"#000","text_primary":"#000","text_secondary":"#000","success":"#000","warning":"#000","error":"#000","ansi":{"black":"#000","red":"#000","green":"#000","yellow":"#000","blue":"#000","magenta":"#000","cyan":"#000","white":"#000","bright_black":"#000","bright_red":"#000","bright_green":"#000","bright_yellow":"#000","bright_blue":"#000","bright_magenta":"#000","bright_cyan":"#000","bright_white":"#000"}},"font":{"ui_family":"x","ui_size_pt":13,"terminal_family":"x","terminal_size_pt":13},"terminal":{"cursor_style":"bar","scrollback_lines":1000,"bidi_enabled":true,"allow_proposed_api":true},"hooks":{"enabled":true,"agents":[],"policy_preset":"default"},"notifications":{"toast_enabled":true,"sound_enabled":false},"updates":{"check_on_startup":true,"auto_download":false}}"##;
        let s: Settings = serde_json::from_str(json).unwrap();
        // Serde default kicked in — sound_master + interactive-4 enabled.
        assert!(s.hook_notifications.sound_master);
        assert!(s.hook_notifications.enabled_types.contains(&HookType::Stop));
        assert!(!s
            .hook_notifications
            .enabled_types
            .contains(&HookType::PostToolUse));
    }

    #[test]
    fn beta3_hook_type_wire_form_is_kebab_case() {
        // The enum serializes back to the same kebab-case strings the CLI
        // emits (see rpc_server.rs subkind dispatch). Locked here so a
        // rename can't silently break the wire protocol.
        assert_eq!(
            serde_json::to_string(&HookType::PreToolUse).unwrap(),
            "\"pre-tool-use\""
        );
        assert_eq!(
            serde_json::to_string(&HookType::SessionEnd).unwrap(),
            "\"session-end\""
        );
        assert_eq!(
            serde_json::to_string(&HookType::UserPromptSubmit).unwrap(),
            "\"user-prompt-submit\""
        );
    }

    // ---- 2026-08-19: RTL profile split (local vs remote) ----------------

    #[test]
    fn rtl_migration_gives_each_profile_its_own_measured_mode() {
        // A single pre-split rtl_mode was necessarily wrong for one of the
        // two classes, so the migration does NOT carry it over — each profile
        // takes its own measured default. The other three knobs are
        // orthogonal to the split and ARE preserved.
        //
        // The fixture uses `bidi_reorder` deliberately: it is now neither
        // profile's default, so "did not inherit the flat field" and "took its
        // own default" cannot be confused. It used to be "off", which stopped
        // distinguishing them the moment local's default BECAME "off".
        let mut t = TerminalSettings {
            rtl_mode: "bidi_reorder".into(),
            auto_direction: false,
            mirror_arrows_rtl: false,
            tui_owns_bidi: true,
            rtl: None,
            ..TerminalSettings::default()
        };
        assert!(migrate_rtl_profiles(&mut t), "absent rtl must migrate");
        let r = t.rtl.expect("seeded");
        assert_eq!(r.local.rtl_mode, "auto_per_line", "local takes its own default");
        assert_eq!(r.remote.rtl_mode, "auto_per_line", "remote takes its own");
        for p in [&r.local, &r.remote] {
            assert_ne!(p.rtl_mode, "bidi_reorder", "the flat mode is not carried");
        }
        for p in [&r.local, &r.remote] {
            assert!(!p.auto_direction, "auto_direction must carry over");
            assert!(!p.mirror_arrows_rtl, "mirror_arrows_rtl must carry over");
            assert!(p.tui_owns_bidi, "tui_owns_bidi must carry over");
        }
    }

    #[test]
    fn remote_direction_policy_is_the_pre_2026_08_19_rule() {
        // THE REGRESSION GUARD. On 2026-08-19 the RTL_DOMINANCE vote shipped
        // keyed on "is Claude Code in front", which is detected from the OSC
        // title -- and that title propagates over SSH, so the vote fired on
        // remote panes and broke a path that had been working since main.
        // Remote's rule is `any_rtl` and it does not drift silently.
        assert_eq!(RtlProfile::default().direction_policy, "any_rtl");
        let d = RtlProfiles::default();
        assert_eq!(d.remote.direction_policy, "any_rtl", "remote must stay on any_rtl");
        assert_eq!(d.local.direction_policy, "any_rtl", "tui_dominance is opt-in");
    }

    #[test]
    fn migration_does_not_clobber_the_local_tui_owns_bidi_default() {
        // Yossi's settings.json had no `rtl` block and no `tui_owns_bidi`, so
        // the migration ran with the flat field at its serde default of false
        // and wrote that into BOTH profiles — silently overriding the local
        // default and leaving the pane doing a second bidi pass over Claude's
        // already-visual output. An absent flat field is not a decision.
        let mut t = TerminalSettings { rtl: None, ..TerminalSettings::default() };
        t.tui_owns_bidi = false; // as serde produces it when the key is absent
        assert!(migrate_rtl_profiles(&mut t));
        let r = t.rtl.expect("seeded");
        assert!(r.local.tui_owns_bidi, "local keeps its own default");
        assert!(!r.remote.tui_owns_bidi, "remote keeps its own default");
    }

    #[test]
    fn migration_still_carries_an_explicit_tui_owns_bidi() {
        // A true IS a decision — it can only have come from the user ticking
        // the box — so it reaches both profiles.
        let mut t = TerminalSettings { rtl: None, ..TerminalSettings::default() };
        t.tui_owns_bidi = true;
        assert!(migrate_rtl_profiles(&mut t));
        let r = t.rtl.expect("seeded");
        assert!(r.local.tui_owns_bidi);
        assert!(r.remote.tui_owns_bidi, "an explicit opt-in reaches remote too");
    }

    #[test]
    fn local_owns_bidi_by_default_and_remote_does_not() {
        // Measured 2026-08-19 with `zellij action dump-screen` on a live local
        // pane: Claude Code on Windows writes the cwd's Hebrew in VISUAL order
        // (U+05E8 U+05D1 U+05E6, reversed against logical), so any second bidi
        // pass double-reorders it. That is why all three rtl_modes read as
        // broken on local while remote was fine.
        //
        // Remote's knob stays OFF: it renders correctly as it is, and the
        // standing instruction is not to touch it while working on local.
        //
        // NOTE the detector behind this knob is currently blind inside zellij
        // (see `default_local_rtl`), so local does not lean on it — it runs
        // rtl_mode="off", which needs no detection. The opt-in is kept so the
        // day the detection works, local is already on the right side of it.
        let d = RtlProfiles::default();
        assert!(d.local.tui_owns_bidi, "local must not bidi Claude's output twice");
        assert!(!d.remote.tui_owns_bidi, "remote is deliberately unchanged");
        // The MODE is the same on both — both render logical-order text. The
        // switch is the only difference, because only a local pane can have a
        // pre-reordered TUI in front of it. Asserted here so a future change
        // that splits the modes again has to come past this line.
        assert_eq!(d.local.rtl_mode, d.remote.rtl_mode);
    }

    #[test]
    fn absent_direction_policy_reads_as_any_rtl() {
        // An existing settings.json has no `direction_policy`. An upgrade must
        // not move a pane onto the newer rule behind the user's back.
        let p: RtlProfile = serde_json::from_str(r#"{"rtl_mode":"auto_per_line"}"#)
            .expect("partial RtlProfile deserializes");
        assert_eq!(p.direction_policy, "any_rtl");
    }

    #[test]
    fn rtl_migration_is_idempotent_and_never_clobbers_a_tuned_split() {
        // Once the user has split the two apart, a later load must not
        // flatten them back onto the deprecated fields.
        let mut t = TerminalSettings {
            rtl_mode: "off".into(),
            rtl: Some(RtlProfiles {
                local: RtlProfile { rtl_mode: "bidi_reorder".into(), ..RtlProfile::default() },
                remote: RtlProfile { rtl_mode: "auto_per_line".into(), ..RtlProfile::default() },
            }),
            ..TerminalSettings::default()
        };
        assert!(!migrate_rtl_profiles(&mut t), "present rtl must be left alone");
        let r = t.rtl.unwrap();
        assert_eq!(r.local.rtl_mode, "bidi_reorder");
        assert_eq!(r.remote.rtl_mode, "auto_per_line");
    }

    #[test]
    fn fresh_install_defaults_share_the_mode_and_differ_on_the_switch() {
        // Both render logical-order text, so both use auto_per_line. What
        // differs is that a LOCAL pane can have Claude Code in front of it
        // writing VISUAL order (`zellij action dump-screen` returned the cwd
        // as U+05E8 U+05D1 U+05E6), and `tui_owns_bidi` is what stands that
        // down — per pane, while Claude holds it, not profile-wide.
        let r = RtlProfiles::default();
        assert_eq!(r.local.rtl_mode, "auto_per_line", "a local SHELL is logical order");
        assert_eq!(r.remote.rtl_mode, "auto_per_line", "remote is unchanged");
        // The difference between the two classes is the switch, not the mode:
        // only local has a pre-reordered TUI to protect against.
        assert!(r.local.tui_owns_bidi);
        assert!(!r.remote.tui_owns_bidi);
    }

    #[test]
    fn an_existing_settings_json_without_rtl_deserialises_to_none() {
        // The migration signal itself. If serde ever starts filling this in,
        // upgrading users would silently get the fresh-install defaults
        // instead of their own values.
        let json = r#"{"rtl_mode":"off","use_ymux_tmux_config":true,
            "mirror_arrows_rtl":true,"auto_direction":true,
            "tui_owns_bidi":false,"auto_reset_on_connect":true}"#;
        let t: TerminalSettings = serde_json::from_str(json).unwrap();
        assert!(t.rtl.is_none());
        assert_eq!(t.rtl_mode, "off");
    }
}
