//! Phase 52 (BiDi 33B): opt-in PTY-stream filter that wraps Latin runs in
//! Unicode bidi isolates (FSI U+2068 / PDI U+2069) when they appear near
//! Hebrew/Arabic text. Default OFF — toggled per pane.
//!
//! Goals:
//!   - When ls/Claude Code output mixes Hebrew + Latin (`דוח-DEV.txt`,
//!     "the changes are in main"), Latin tokens render in their correct
//!     logical position. Bidi isolates make each Latin run an atomic unit
//!     that the renderer can't reorder against neighboring RTL text.
//!
//! Non-goals (Things the filter MUST NOT break):
//!   - ANSI/CSI/OSC/DCS escape sequences pass through verbatim. Inserting
//!     U+2068 inside `\x1b[31m` would break color rendering.
//!   - Cursor-positioning sequences (`\x1b[H`, `\x1b[<n>;<m>H`) preserved.
//!     Bidi marks would shift column positions.
//!   - Box-drawing chars (U+2500–U+257F, U+2580–U+259F) preserved. Claude
//!     Code's Ink-based TUI uses these for borders; we skip the whole
//!     text segment if box-drawing dominates.
//!
//! Streaming: PTY chunks arrive in chunks of arbitrary length. A single
//! escape sequence can split across chunks. The byte-level state
//! machine buffers in-progress escapes until they complete.
//!
//! Chunk boundary correctness: a Latin run that straddles two chunks
//! gets a redundant pair of FSI/PDI inserted. That's a visual no-op
//! (nested isolates render the same), so we accept it instead of
//! complicating the cross-chunk state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Bidi isolates we inject. The `char` values come straight from the
/// Unicode Bidirectional Algorithm spec (UBA, 9.0+):
///   FSI = U+2068 — "First Strong Isolate"
///   PDI = U+2069 — "Pop Directional Isolate"
const FSI: char = '\u{2068}';
const PDI: char = '\u{2069}';

/// Max bytes we'll buffer for a single in-progress escape sequence
/// before giving up and flushing. Real-world escapes are well under 64;
/// the cap stops a malformed/runaway sequence from eating unbounded
/// memory.
const ESCAPE_BUF_MAX: usize = 64;
/// OSC payloads can be longer (titles, hyperlinks). Generous cap.
const OSC_BUF_MAX: usize = 512;

/// How many non-RTL chars we treat as "still in RTL context" — i.e. the
/// wrap heuristic considers a Latin run worth isolating only if a
/// Hebrew/Arabic char appeared within the last N chars. 200 ≈ one
/// terminal line, which matches what users perceive as "this line is
/// in Hebrew context."
const RTL_CONTEXT_WINDOW: u32 = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
enum BidiFilterState {
    Normal,
    /// Saw `\x1b`. Next byte decides which sub-state we land in.
    Esc,
    /// Inside a CSI sequence (`\x1b[…`). Terminator is any byte in
    /// 0x40–0x7E.
    Csi,
    /// Inside an OSC sequence (`\x1b]…`). Terminator is BEL (`\x07`) or
    /// ST (`\x1b\x5C`).
    Osc,
    /// We just saw `\x1b` inside OSC — waiting on the `\x5C` (backslash)
    /// that would complete the ST terminator.
    OscEsc,
    /// Inside a DCS sequence (`\x1bP…`). Terminator is ST (`\x1b\x5C`).
    Dcs,
    /// We just saw `\x1b` inside DCS — waiting on the `\x5C`.
    DcsEsc,
}

/// Per-pane bidi filter state. Cheap to construct, cheap to bypass:
/// when `enabled` is false, `process` is a passthrough that doesn't
/// touch the bytes.
pub struct BidiFilter {
    pub enabled: bool,
    state: BidiFilterState,
    escape_buf: Vec<u8>,
    /// How many chars since we last saw an RTL char. Saturating —
    /// effectively "no RTL ever seen" once it caps. Reset to 0 on
    /// every Hebrew/Arabic char.
    chars_since_rtl: u32,
}

impl BidiFilter {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            state: BidiFilterState::Normal,
            escape_buf: Vec::new(),
            chars_since_rtl: RTL_CONTEXT_WINDOW, // start "out of RTL context"
        }
    }

    /// Feed a chunk in; get a chunk out. When disabled, the slice is
    /// returned as-owned with no transformation. When enabled, escape
    /// sequences pass through and plain-text runs get the bidi treatment.
    pub fn process(&mut self, input: &[u8]) -> Vec<u8> {
        if !self.enabled {
            return input.to_vec();
        }
        let mut out: Vec<u8> = Vec::with_capacity(input.len() + input.len() / 4);
        let mut text_accum: Vec<u8> = Vec::with_capacity(64);

        for &b in input {
            match self.state {
                BidiFilterState::Normal => self.on_byte_normal(b, &mut text_accum, &mut out),
                BidiFilterState::Esc => self.on_byte_esc(b, &mut out),
                BidiFilterState::Csi => self.on_byte_csi(b, &mut out),
                BidiFilterState::Osc => self.on_byte_osc(b, &mut out),
                BidiFilterState::OscEsc => self.on_byte_osc_esc(b, &mut out),
                BidiFilterState::Dcs => self.on_byte_dcs(b, &mut out),
                BidiFilterState::DcsEsc => self.on_byte_dcs_esc(b, &mut out),
            }
        }
        // Flush any trailing plain-text run.
        self.flush_text(&mut text_accum, &mut out);
        out
    }

    fn on_byte_normal(&mut self, b: u8, accum: &mut Vec<u8>, out: &mut Vec<u8>) {
        if b == 0x1B {
            // Flush pending text before the escape starts.
            self.flush_text(accum, out);
            self.escape_buf.clear();
            self.escape_buf.push(b);
            self.state = BidiFilterState::Esc;
        } else if b < 0x20 {
            // Control bytes (CR, LF, BS, TAB, BEL, …) flush the
            // pending text and pass through. Crucially this means the
            // RTL-distance counter resets per logical line via LF.
            self.flush_text(accum, out);
            out.push(b);
            if b == b'\n' {
                // New line — reset RTL context. Otherwise a Hebrew
                // word on line N would keep wrapping Latin on line N+5.
                self.chars_since_rtl = RTL_CONTEXT_WINDOW;
            }
        } else {
            accum.push(b);
        }
    }

    fn on_byte_esc(&mut self, b: u8, out: &mut Vec<u8>) {
        self.escape_buf.push(b);
        match b {
            b'[' => self.state = BidiFilterState::Csi,
            b']' => self.state = BidiFilterState::Osc,
            b'P' => self.state = BidiFilterState::Dcs,
            // Single-byte escapes (ESC c, ESC E, ESC D, ESC M, ESC 7,
            // ESC 8, etc.) — bytes 0x30..=0x7E close the sequence here.
            0x30..=0x7E => {
                out.extend_from_slice(&self.escape_buf);
                self.escape_buf.clear();
                self.state = BidiFilterState::Normal;
            }
            _ => {
                // Bail: unknown intro after ESC. Emit and reset.
                out.extend_from_slice(&self.escape_buf);
                self.escape_buf.clear();
                self.state = BidiFilterState::Normal;
            }
        }
    }

    fn on_byte_csi(&mut self, b: u8, out: &mut Vec<u8>) {
        self.escape_buf.push(b);
        if (0x40..=0x7E).contains(&b) {
            // CSI terminator. The escape is complete.
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        } else if self.escape_buf.len() > ESCAPE_BUF_MAX {
            // Sanity bail — abnormally long CSI.
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        }
    }

    fn on_byte_osc(&mut self, b: u8, out: &mut Vec<u8>) {
        self.escape_buf.push(b);
        if b == 0x07 {
            // BEL terminator.
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        } else if b == 0x1B {
            // Start of possible ST (ESC \).
            self.state = BidiFilterState::OscEsc;
        } else if self.escape_buf.len() > OSC_BUF_MAX {
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        }
    }

    fn on_byte_osc_esc(&mut self, b: u8, out: &mut Vec<u8>) {
        self.escape_buf.push(b);
        if b == 0x5C {
            // ST: ESC \ — complete the OSC.
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        } else {
            // Not ST — back to OSC (the ESC is part of OSC content).
            self.state = BidiFilterState::Osc;
        }
    }

    fn on_byte_dcs(&mut self, b: u8, out: &mut Vec<u8>) {
        self.escape_buf.push(b);
        if b == 0x1B {
            self.state = BidiFilterState::DcsEsc;
        } else if self.escape_buf.len() > OSC_BUF_MAX {
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        }
    }

    fn on_byte_dcs_esc(&mut self, b: u8, out: &mut Vec<u8>) {
        self.escape_buf.push(b);
        if b == 0x5C {
            out.extend_from_slice(&self.escape_buf);
            self.escape_buf.clear();
            self.state = BidiFilterState::Normal;
        } else {
            self.state = BidiFilterState::Dcs;
        }
    }

    /// Walk the accumulated text-run bytes as UTF-8 chars and inject
    /// FSI/PDI around Latin runs that are within RTL_CONTEXT_WINDOW
    /// chars of a recent Hebrew/Arabic char. If box-drawing dominates
    /// the chunk (>50%), pass through verbatim.
    fn flush_text(&mut self, accum: &mut Vec<u8>, out: &mut Vec<u8>) {
        if accum.is_empty() {
            return;
        }
        // Input is whole chars: `emit_data` decodes (pty_decode) before
        // filtering, so chunk-boundary splits never reach here. Lossy only
        // covers the escape-cap flush edge.
        let s = String::from_utf8_lossy(accum);

        // Count box-drawing chars vs total. If box dominates, bail.
        let (box_count, total) = s.chars().fold((0usize, 0usize), |(b, t), c| {
            let cp = c as u32;
            let is_box = (0x2500..=0x257F).contains(&cp) || (0x2580..=0x259F).contains(&cp);
            (if is_box { b + 1 } else { b }, t + 1)
        });
        if total > 0 && box_count * 2 > total {
            out.extend_from_slice(accum);
            // Advance chars_since_rtl by the chunk's char count (box
            // chars aren't RTL).
            self.chars_since_rtl = self
                .chars_since_rtl
                .saturating_add(total as u32);
            accum.clear();
            return;
        }

        // Walk chars, accumulating Latin runs and deciding wrap on
        // each run boundary. `run_start_distance` snapshots the
        // RTL-distance at the run's first char so the decision is
        // based on context at the start of the run.
        let mut result = String::with_capacity(s.len() + 16);
        let mut current_run = String::new();
        let mut run_start_distance: u32 = self.chars_since_rtl;

        for c in s.chars() {
            if is_latin_run_char(c) {
                if current_run.is_empty() {
                    run_start_distance = self.chars_since_rtl;
                }
                current_run.push(c);
                self.chars_since_rtl = self.chars_since_rtl.saturating_add(1);
            } else {
                if !current_run.is_empty() {
                    write_latin_run(&mut result, &current_run, run_start_distance);
                    current_run.clear();
                }
                result.push(c);
                if is_rtl(c) {
                    self.chars_since_rtl = 0;
                } else {
                    self.chars_since_rtl = self.chars_since_rtl.saturating_add(1);
                }
            }
        }
        // Flush a trailing Latin run.
        if !current_run.is_empty() {
            write_latin_run(&mut result, &current_run, run_start_distance);
        }

        out.extend_from_slice(result.as_bytes());
        accum.clear();
    }
}

fn write_latin_run(out: &mut String, run: &str, start_distance: u32) {
    if start_distance < RTL_CONTEXT_WINDOW {
        out.push(FSI);
        out.push_str(run);
        out.push(PDI);
    } else {
        out.push_str(run);
    }
}

/// What counts as part of a Latin run worth isolating. ASCII alphanum
/// plus a handful of common path/identifier punctuation. NOT spaces —
/// they break a run so adjacent Latin tokens get separate isolates,
/// which keeps the visual atomicity tighter.
fn is_latin_run_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '-' | '\\' | ':' | '+' | '@' | '~')
}

/// Hebrew (U+0590–05FF), Arabic (U+0600–06FF), Syriac (U+0700–074F),
/// Arabic Supplement (U+0750–077F). The common RTL scripts users will
/// mix with Latin in technical contexts.
fn is_rtl(c: char) -> bool {
    let cp = c as u32;
    (0x0590..=0x05FF).contains(&cp)
        || (0x0600..=0x06FF).contains(&cp)
        || (0x0700..=0x074F).contains(&cp)
        || (0x0750..=0x077F).contains(&cp)
}

// ─── per-pane filter map ─────────────────────────────────────────────

/// Shared state stored on CoreState. Keyed by pane_id; the entry's
/// `enabled` field gets flipped by `pane_set_smart_bidi`. The PTY read
/// loop calls `apply_to_pane` before handing bytes to xterm.js.
pub type BidiFilterMap = Arc<Mutex<HashMap<String, BidiFilter>>>;

/// Look up (or lazily create) the per-pane filter and run the chunk
/// through it. Returns owned text — when the filter is disabled the
/// allocation is skipped: the text is returned as-is.
/// Input is already-decoded text (whole chars).
pub fn apply_to_pane(filters: &BidiFilterMap, pane_id: &str, text: String) -> String {
    let mut map = filters.lock().unwrap();
    let filter = map
        .entry(pane_id.to_string())
        .or_insert_with(|| BidiFilter::new(false));
    if !filter.enabled {
        return text;
    }
    let out = filter.process(text.as_bytes());
    match String::from_utf8(out) {
        Ok(s) => s,
        // AI-NOTE: escape-cap flush (OSC_BUF_MAX) can cut a char; pre-existing edge
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// Idempotent toggle. Creates the entry if missing.
pub fn set_pane_enabled(filters: &BidiFilterMap, pane_id: &str, enabled: bool) {
    let mut map = filters.lock().unwrap();
    let filter = map
        .entry(pane_id.to_string())
        .or_insert_with(|| BidiFilter::new(false));
    filter.enabled = enabled;
    // Reset transient state on toggle so a freshly-enabled filter
    // doesn't carry stale escape-sequence buffering from a phantom
    // half-parsed past.
    filter.state = BidiFilterState::Normal;
    filter.escape_buf.clear();
    filter.chars_since_rtl = RTL_CONTEXT_WINDOW;
}

// ─── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run(filter: &mut BidiFilter, input: &str) -> String {
        let bytes = filter.process(input.as_bytes());
        String::from_utf8(bytes).expect("filter output must be valid UTF-8")
    }

    #[test]
    fn disabled_filter_is_passthrough() {
        let mut f = BidiFilter::new(false);
        let s = "שלום DEV מהעולם";
        assert_eq!(run(&mut f, s), s);
    }

    #[test]
    fn ascii_only_chunk_unchanged() {
        let mut f = BidiFilter::new(true);
        let s = "the quick brown fox";
        // No RTL ever seen → no wrapping.
        assert_eq!(run(&mut f, s), s);
    }

    #[test]
    fn pure_hebrew_chunk_unchanged() {
        let mut f = BidiFilter::new(true);
        let s = "שלום עולם";
        // RTL present but no Latin runs.
        assert_eq!(run(&mut f, s), s);
    }

    #[test]
    fn hebrew_plus_latin_wraps_latin() {
        let mut f = BidiFilter::new(true);
        // "Hebrew DEV more-Hebrew" — DEV should get FSI/PDI.
        let out = run(&mut f, "שלום DEV עולם");
        assert!(out.contains('\u{2068}'), "expected FSI in output: {out:?}");
        assert!(out.contains('\u{2069}'), "expected PDI in output: {out:?}");
        assert!(out.contains("\u{2068}DEV\u{2069}"), "DEV wrap missing: {out:?}");
    }

    #[test]
    fn ansi_escape_preserved_and_latin_inside_wrapped() {
        let mut f = BidiFilter::new(true);
        // \x1b[31mDEV\x1b[0m embedded after Hebrew.
        let out = run(&mut f, "שלום \x1b[31mDEV\x1b[0m עולם");
        // Both escape sequences must appear untouched, in the same order.
        assert!(out.contains("\x1b[31m"), "color-on lost: {out:?}");
        assert!(out.contains("\x1b[0m"), "reset lost: {out:?}");
        // DEV itself wrapped with isolates.
        assert!(out.contains("\u{2068}DEV\u{2069}"), "DEV not wrapped: {out:?}");
        // Bytes 0x1B and 0x5B inside the escapes must NOT have isolates
        // injected between them.
        assert!(!out.contains("\u{2068}\x1b"), "isolate adjacent to ESC: {out:?}");
    }

    #[test]
    fn box_drawing_dominant_chunk_passes_through() {
        let mut f = BidiFilter::new(true);
        // Hebrew + Latin embedded in heavy box-drawing → skip wrap.
        let s = "┏━━━━━━━━━━━━ DEV ━━━━━━━━━━━━┓ שלום";
        let out = run(&mut f, s);
        // No isolates injected; whole chunk untouched.
        assert!(!out.contains('\u{2068}'), "isolate injected in box-heavy chunk: {out:?}");
        assert!(!out.contains('\u{2069}'), "isolate injected in box-heavy chunk: {out:?}");
    }

    #[test]
    fn escape_split_across_chunks_reassembled() {
        let mut f = BidiFilter::new(true);
        // First chunk ends mid-CSI.
        let a = f.process(b"\xd7\xa9\xd7\x9c\xd7\x95\xd7\x9d \x1b[3"); // "שלום \x1b[3"
        // Second chunk completes the CSI and adds DEV.
        let b = f.process(b"1mDEV\x1b[0m");
        let combined = String::from_utf8([a, b].concat()).unwrap();
        assert!(combined.contains("\x1b[31m"), "split CSI not reassembled: {combined:?}");
        assert!(combined.contains("\u{2068}DEV\u{2069}"), "DEV not wrapped post-split: {combined:?}");
    }

    #[test]
    fn osc_sequence_passes_through_untouched() {
        let mut f = BidiFilter::new(true);
        // OSC 9 (iTerm2-style "command finished" notification) with BEL terminator.
        let s = "\x1b]9;Build finished\x07";
        let out = run(&mut f, s);
        assert_eq!(out, s, "OSC payload modified");
    }

    #[test]
    fn osc_with_st_terminator_passes_through() {
        let mut f = BidiFilter::new(true);
        // OSC 8 hyperlink with ESC \\ (ST) terminator.
        let s = "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        let out = run(&mut f, s);
        assert_eq!(out, s, "OSC-ST sequence modified");
    }

    #[test]
    fn dcs_sequence_preserved() {
        let mut f = BidiFilter::new(true);
        // Synthetic DCS (Sixel-style payload). Filter must pass through.
        let s = "\x1bPq#0;2;100;100;100#1~~@@vv\x1b\\";
        let out = run(&mut f, s);
        assert_eq!(out, s, "DCS payload modified");
    }

    #[test]
    fn newline_resets_rtl_context() {
        let mut f = BidiFilter::new(true);
        // Hebrew, then newline, then Latin many chars later — Latin
        // should NOT be wrapped because LF resets the context.
        let mut s = String::from("שלום\n");
        s.push_str(&"x".repeat(50));
        s.push_str(" DEV");
        let out = run(&mut f, &s);
        // We pushed past RTL_CONTEXT_WINDOW via the newline reset, so
        // DEV stays bare.
        assert!(!out.contains("\u{2068}DEV\u{2069}"), "post-LF Latin should not wrap: {out:?}");
    }

    #[test]
    fn cursor_positioning_sequence_preserved() {
        let mut f = BidiFilter::new(true);
        // \x1b[10;5H positions cursor at row 10, col 5. Filter must not
        // inject isolates inside the parameter bytes.
        let s = "שלום \x1b[10;5HDEV\x1b[0m";
        let out = run(&mut f, s);
        assert!(out.contains("\x1b[10;5H"), "cursor seq mangled: {out:?}");
    }

    fn enabled_map() -> BidiFilterMap {
        let m: BidiFilterMap = Arc::new(Mutex::new(HashMap::new()));
        set_pane_enabled(&m, "p", true);
        m
    }

    #[test]
    fn hebrew_split_across_chunks_decode_then_filter() {
        // Pins decode-before-filter: a Hebrew char cut mid-byte across two
        // reads must survive intact; filtering raw bytes gave U+FFFD.
        let m = enabled_map();
        let mut st = crate::pty_decode::Utf8Stream::new();
        let full = "שלום".as_bytes();
        let mut out = String::new();
        for chunk in [&full[..1], &full[1..]] {
            let d = st.push(chunk);
            out.push_str(&apply_to_pane(&m, "p", d.text));
        }
        assert_eq!(out, "שלום");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn mixed_hebrew_latin_split_chunks_wrap_without_fffd() {
        // Pins wrap behaviour on a split stream: Latin after Hebrew is
        // isolated and no replacement char appears.
        let m = enabled_map();
        let mut st = crate::pty_decode::Utf8Stream::new();
        let full = "שלום main.rs".as_bytes();
        let mut out = String::new();
        for chunk in [&full[..3], &full[3..]] {
            let d = st.push(chunk);
            out.push_str(&apply_to_pane(&m, "p", d.text));
        }
        assert_eq!(out, "שלום \u{2068}main.rs\u{2069}");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn disabled_pane_apply_is_passthrough() {
        // Pins the off fast path: a disabled pane returns text unchanged.
        let m: BidiFilterMap = Arc::new(Mutex::new(HashMap::new()));
        let s = "שלום main.rs".to_string();
        assert_eq!(apply_to_pane(&m, "q", s.clone()), s);
    }
}
