//! Per-pane agent briefs — the data layer behind the Queue panel and the
//! workspace Briefing card.
//!
//! An agent that cooperates ends its FINAL assistant message with a plain-
//! text block:
//!
//! ```text
//! [ymux-brief]
//! task: Windows file-lock installer
//! status: waiting-for-you
//! ask: Delete the legacy lock file on upgrade?
//! rec: Yes — it is regenerated on first run.
//! next: Wire lock acquisition into install_all()
//! delta: Lock module done + tested; installer not yet wired.
//! ```
//!
//! The Stop hook already forwards `last_assistant_message` verbatim to the
//! desktop (the CLI pushes the whole hook payload), so this module needs no
//! CLI cooperation at all: `parse_brief` runs desktop-side in `feed.push`'s
//! `stop` arm. A message with no marker degrades to a status-only brief —
//! never fabricate an `ask`.
//!
//! Format constraints the parser honors:
//! - The marker is a whole line; the LAST occurrence wins (`marker_offset`
//!   scans every line), so an agent quoting the format mid-message doesn't
//!   truncate its own brief.
//! - Keys are fixed ASCII words split on the FIRST `:`, so fully-RTL values
//!   (Hebrew briefs are the expected common case) cannot confuse it.
//! - Markdown decoration around keys/lines (`- `, `> `, `**`, backticks,
//!   code fences) is stripped; unknown keys are ignored for forward compat.
//!
//! Rule #1: brief and prompt CONTENT lives in memory and the UI only.
//! Nothing in this module logs; callers log metadata (pane id, degraded
//! flag, field count) at most.

use serde::Serialize;

/// Hard cap per parsed field — bounds both memory and what the UI must lay out.
const FIELD_MAX_CHARS: usize = 200;
/// Degraded-brief delta (first line of the assistant message).
const DEGRADED_DELTA_MAX_CHARS: usize = 160;
/// The user's last prompt, kept for the queue's "got from you: …" line.
pub(crate) const PROMPT_MAX_CHARS: usize = 160;

/// The marker line, compared case-insensitively after decoration stripping.
const MARKER: &str = "[ymux-brief]";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BriefStatus {
    Working,
    WaitingForYou,
    Stuck,
    Done,
}

impl BriefStatus {
    /// Tolerant parse; aliases cover the phrasings agents actually produce.
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "working" => Some(Self::Working),
            "waiting-for-you" | "waiting" | "waiting for you" => Some(Self::WaitingForYou),
            "stuck" | "blocked" => Some(Self::Stuck),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

/// One turn's brief, parsed from (or synthesized off) the final assistant
/// message. `degraded == true` means no marker was found and everything
/// here is inferred — the UI renders it dimmer and never shows an ask.
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct PaneBrief {
    pub(crate) task: Option<String>,
    pub(crate) status: BriefStatus,
    /// needs-you: one closed question…
    pub(crate) ask: Option<String>,
    /// …and the agent's recommendation for it.
    pub(crate) rec: Option<String>,
    pub(crate) next: Option<String>,
    pub(crate) delta: Option<String>,
    pub(crate) degraded: bool,
    /// Epoch ms of the Stop that produced it. Plain number on the wire —
    /// same idiom as `FeedItem::created_ms`.
    #[ts(type = "number")]
    pub(crate) updated_ms: u128,
}

/// Everything the desktop knows about one pane's agent conversation state,
/// beyond the traffic light. Held in `AppState.briefs`, keyed by RESOLVED
/// pane id, in memory only (the `agent_runs` precedent: a restart clears
/// it, and the first hook rebuilds the truth).
#[derive(Clone, Debug, Default, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct PaneBriefEntry {
    pub(crate) brief: Option<PaneBrief>,
    /// The user's last prompt (clipped) — "got from you: …" in the queue.
    /// Rule #1: display-only; must never reach debug.log.
    pub(crate) last_prompt: Option<String>,
    #[ts(type = "number | null")]
    pub(crate) prompt_ms: Option<u128>,
    /// SessionEnd arrived; the brief is kept (it summarizes finished work)
    /// but the queue buckets the pane as closed.
    pub(crate) session_ended: bool,
    /// Per-pane monotonic guard, bumped on every mutation of this entry.
    /// The frontend drops events whose seq is not newer than what it holds.
    pub(crate) seq: u32,
}

/// Fields as written by the agent; the caller stamps `updated_ms` and picks
/// degraded fallbacks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedBrief {
    pub(crate) task: Option<String>,
    pub(crate) status: Option<BriefStatus>,
    pub(crate) ask: Option<String>,
    pub(crate) rec: Option<String>,
    pub(crate) next: Option<String>,
    pub(crate) delta: Option<String>,
}

/// Clip to `max` characters on a char boundary, appending an ellipsis.
pub(crate) fn clip_chars(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Strip the markdown decoration agents wrap lines in: leading quote/bullet
/// markers and wrapping emphasis (`**bold**`, `` `code` ``).
fn clean_line(line: &str) -> &str {
    let mut s = line.trim();
    loop {
        let before = s;
        s = s.trim_start_matches(['>', '-', '*', '#']).trim_start();
        s = s.trim_matches(['*', '`']).trim();
        if s == before {
            break;
        }
    }
    s
}

/// Byte offset of the start of the LAST line that is exactly the marker.
fn marker_offset(msg: &str) -> Option<usize> {
    let mut found = None;
    let mut pos = 0usize;
    for line in msg.split('\n') {
        if clean_line(line).eq_ignore_ascii_case(MARKER) {
            found = Some(pos);
        }
        pos += line.len() + 1;
    }
    found
}

/// The message with the brief block removed — what feed cards should show
/// instead of the raw block. Returns the whole message when no marker.
pub(crate) fn pre_brief_text(msg: &str) -> &str {
    match marker_offset(msg) {
        Some(off) => msg[..off].trim_end(),
        None => msg,
    }
}

/// Parse the `[ymux-brief]` block out of an assistant message. `None` when
/// no marker line exists. Unknown keys are skipped; a repeated key keeps
/// the LAST value (agents that revise mid-block end up right).
pub(crate) fn parse_brief(msg: &str) -> Option<ParsedBrief> {
    let off = marker_offset(msg)?;
    let mut out = ParsedBrief {
        task: None,
        status: None,
        ask: None,
        rec: None,
        next: None,
        delta: None,
    };
    // Skip the marker line itself, then walk the tail.
    for line in msg[off..].split('\n').skip(1) {
        let line = clean_line(line);
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let value = clip_chars(value, FIELD_MAX_CHARS);
        // `- **task**: x` reaches here as `task**: x` (clean_line strips the
        // leading run but only wrapping pairs) — finish the job on the key.
        match key.trim().trim_matches(['*', '`', '_']).to_ascii_lowercase().as_str() {
            "task" => out.task = Some(value),
            "status" => out.status = BriefStatus::parse(&value).or(out.status),
            "ask" => out.ask = Some(value),
            "rec" => out.rec = Some(value),
            "next" => out.next = Some(value),
            "delta" => out.delta = Some(value),
            _ => {}
        }
    }
    Some(out)
}

/// Build the stored brief for a Stop. `auto_title` feeds the degraded
/// `task`; a missing/empty assistant message still yields a status-only
/// degraded brief (codex/gemini shims send none).
pub(crate) fn brief_from_stop(
    last_assistant_message: Option<&str>,
    auto_title: Option<&str>,
    updated_ms: u128,
) -> PaneBrief {
    let parsed = last_assistant_message.and_then(parse_brief);
    match parsed {
        Some(p) => PaneBrief {
            task: p.task.or_else(|| auto_title.map(|t| clip_chars(t, FIELD_MAX_CHARS))),
            status: p.status.unwrap_or(BriefStatus::Done),
            ask: p.ask,
            rec: p.rec,
            next: p.next,
            delta: p.delta,
            degraded: false,
            updated_ms,
        },
        None => PaneBrief {
            task: auto_title.map(|t| clip_chars(t, FIELD_MAX_CHARS)),
            status: BriefStatus::Done,
            ask: None,
            rec: None,
            next: None,
            delta: last_assistant_message
                .and_then(|m| m.lines().map(str::trim).find(|l| !l.is_empty()))
                .map(|l| clip_chars(l, DEGRADED_DELTA_MAX_CHARS)),
            degraded: true,
            updated_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "Some analysis first.\n\n[ymux-brief]\ntask: file-lock installer\nstatus: waiting-for-you\nask: Delete the legacy lock file?\nrec: Yes — regenerated on first run.\nnext: wire into install_all()\ndelta: lock module done + tested\n";

    #[test]
    fn parses_a_full_block() {
        let p = parse_brief(FULL).expect("marker present");
        assert_eq!(p.task.as_deref(), Some("file-lock installer"));
        assert_eq!(p.status, Some(BriefStatus::WaitingForYou));
        assert_eq!(p.ask.as_deref(), Some("Delete the legacy lock file?"));
        assert_eq!(p.rec.as_deref(), Some("Yes — regenerated on first run."));
        assert_eq!(p.next.as_deref(), Some("wire into install_all()"));
        assert_eq!(p.delta.as_deref(), Some("lock module done + tested"));
    }

    #[test]
    fn hebrew_values_survive() {
        let msg = "[ymux-brief]\nstatus: stuck\nask: למחוק את קובץ הנעילה הישן?\nrec: כן — הוא נוצר מחדש בריצה ראשונה.\n";
        let p = parse_brief(msg).expect("marker");
        assert_eq!(p.status, Some(BriefStatus::Stuck));
        assert_eq!(p.ask.as_deref(), Some("למחוק את קובץ הנעילה הישן?"));
        assert_eq!(p.rec.as_deref(), Some("כן — הוא נוצר מחדש בריצה ראשונה."));
    }

    #[test]
    fn last_marker_wins_over_a_quoted_one() {
        let msg = format!(
            "Use this format:\n[ymux-brief]\ntask: EXAMPLE\n\nDone explaining.\n{FULL}"
        );
        let p = parse_brief(&msg).expect("marker");
        assert_eq!(p.task.as_deref(), Some("file-lock installer"));
        // And the pre-brief text cuts at the REAL marker, keeping the quote.
        assert!(pre_brief_text(&msg).contains("task: EXAMPLE"));
        assert!(pre_brief_text(&msg).ends_with("Some analysis first."));
    }

    #[test]
    fn markdown_decoration_is_stripped() {
        let msg = "**[ymux-brief]**\n```\n- **task**: decorated\n> status: done\n```\n";
        let p = parse_brief(msg).expect("marker");
        assert_eq!(p.task.as_deref(), Some("decorated"));
        assert_eq!(p.status, Some(BriefStatus::Done));
    }

    #[test]
    fn unknown_keys_and_bad_status_are_tolerated() {
        let msg = "[ymux-brief]\nmood: excellent\nstatus: confused\ndelta: still fine\n";
        let p = parse_brief(msg).expect("marker");
        assert_eq!(p.status, None); // caller defaults to Done
        assert_eq!(p.delta.as_deref(), Some("still fine"));
    }

    #[test]
    fn status_aliases() {
        assert_eq!(BriefStatus::parse("waiting"), Some(BriefStatus::WaitingForYou));
        assert_eq!(BriefStatus::parse("Blocked"), Some(BriefStatus::Stuck));
        assert_eq!(BriefStatus::parse("WORKING"), Some(BriefStatus::Working));
    }

    #[test]
    fn no_marker_yields_none_and_full_pre_text() {
        assert!(parse_brief("just a normal answer").is_none());
        assert_eq!(pre_brief_text("just a normal answer"), "just a normal answer");
    }

    #[test]
    fn degraded_brief_from_plain_message() {
        let b = brief_from_stop(Some("Fixed the bug.\nDetails below."), Some("my session"), 42);
        assert!(b.degraded);
        assert_eq!(b.status, BriefStatus::Done);
        assert_eq!(b.delta.as_deref(), Some("Fixed the bug."));
        assert_eq!(b.task.as_deref(), Some("my session"));
        assert!(b.ask.is_none()); // never fabricate a question
    }

    #[test]
    fn degraded_brief_with_no_message_at_all() {
        let b = brief_from_stop(None, None, 7);
        assert!(b.degraded);
        assert_eq!(b.status, BriefStatus::Done);
        assert!(b.delta.is_none() && b.task.is_none() && b.ask.is_none());
    }

    #[test]
    fn parsed_status_defaults_to_done_when_missing() {
        let b = brief_from_stop(Some("[ymux-brief]\ndelta: shipped\n"), None, 1);
        assert!(!b.degraded);
        assert_eq!(b.status, BriefStatus::Done);
        assert_eq!(b.delta.as_deref(), Some("shipped"));
    }

    #[test]
    fn long_fields_are_clipped() {
        let long = "x".repeat(500);
        let msg = format!("[ymux-brief]\ndelta: {long}\n");
        let p = parse_brief(&msg).expect("marker");
        assert!(p.delta.expect("delta").chars().count() <= 201); // 200 + ellipsis
    }
}
