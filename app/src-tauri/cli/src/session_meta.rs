// Server-side session metadata (multi-machine sync).
//
// `~/.ymux/session-meta.json` maps a tmux session name to the Claude
// session running inside it plus display metadata, so ANY ymux desktop
// connecting to this server (home, office, laptop) can label the same
// sessions identically. Written from two directions:
//   - the Claude `stop` hook (this CLI, inside the pane): claude_session_id
//     + claude_title extracted from the transcript, every turn;
//   - the desktop app over an SSH exec (`ymux session-meta set`): the
//     creating machine's `origin` id and the user's manual `label`.
//
// Schema:
//   { "version": 1, "sessions": { "<tmux_session_name>": {
//       "claude_session_id": "...", "claude_title": "...",
//       "auto_name": "...", "label": "...", "origin": "...",
//       "updated_at": "..." } } }
//
// `claude_title` vs `auto_name`: the title CHURNS (Claude rewrites its own
// summary as the conversation drifts, and the stop hook re-reads it every
// turn), which makes it a poor identity for a session you opened hours
// ago. `auto_name` is the stable one — derived ONCE, from the session's
// first real prompt, as "<two words> · <YYYY-MM-DD HH:MM>" — and is what
// the picker and the pane header show. Both are kept: the title still
// carries Claude's current read of the conversation.
//
// Concurrency: last-writer-wins over an atomic tmp+rename. Each key has a
// single effective writer (its own pane's hooks; origin/label writes are
// one-shot user actions), and the stop hook re-writes every turn, so a
// lost read-modify-write self-heals within one turn. No flock — home dirs
// are sometimes NFS where flock lies.
//
// Cleanup: no tmux session-closed hook (it wouldn't fire when the user
// disables the bundled tmux.conf). Instead every write prunes keys that
// no longer exist in `tmux ls`, and the desktop-side join ignores keys
// it can't match, so stale entries are invisible even before pruning.
//
// Rule #1: claude_title / auto_name / label contain user content. They
// live in the meta FILE by design (that's the feature) but must never be
// written to hook-debug.log — callers log error kinds and session names
// only.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SessionMetaEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_title: Option<String>,
    /// Stable session identity: "<two words> · <YYYY-MM-DD HH:MM>",
    /// derived once from the first real prompt (see `derive_session_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionMetaFile {
    pub version: u32,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionMetaEntry>,
}

impl Default for SessionMetaFile {
    fn default() -> Self {
        Self { version: 1, sessions: BTreeMap::new() }
    }
}

fn meta_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".ymux").join("session-meta.json"))
}

/// Missing / unreadable / corrupt file all degrade to an empty map — the
/// next save rebuilds it.
pub fn load_meta() -> SessionMetaFile {
    let Some(path) = meta_path() else { return SessionMetaFile::default() };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => SessionMetaFile::default(),
    }
}

/// Atomic write: `session-meta.<pid>.tmp` then rename over the target
/// (same pattern as the desktop's workspaces.json / tmux-labels.json).
pub fn save_meta_atomic(meta: &SessionMetaFile) -> Result<(), String> {
    let path = meta_path().ok_or("no HOME")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(meta).map_err(|e| format!("serialize: {e}"))?;
    let tmp = path.with_file_name(format!("session-meta.{}.tmp", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename: {e}")
    })
}

/// Names of the tmux sessions currently alive on this server.
/// `None` = tmux binary couldn't even be spawned (skip pruning rather
/// than wrongly wiping the file). A running spawn with nonzero exit
/// means "no server / no sessions" — an accurate empty set.
fn live_tmux_sessions() -> Option<Vec<String>> {
    let out = std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return Some(Vec::new());
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// Drop entries whose tmux session no longer exists. Returns true when
/// anything was removed.
pub fn prune(meta: &mut SessionMetaFile) -> bool {
    let Some(live) = live_tmux_sessions() else { return false };
    let before = meta.sessions.len();
    meta.sessions.retain(|name, _| live.iter().any(|l| l == name));
    meta.sessions.len() != before
}

/// Copy of the desktop's `sanitize_tmux_session_name` (lib.rs) — keep in
/// sync. Pane ids are `p_<hex>_<n>` so this is a no-op in practice.
fn sanitize_tmux_session_name(pane_id: &str) -> String {
    let cleaned: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("ymux-{}", cleaned)
}

/// The tmux session this process runs inside. Preferred: ask tmux itself
/// (exact even when YMUX_PANE_ID is stale — `set-environment -g` is
/// tmux-server-global, so on multi-machine servers the env var is
/// last-connector-wins). Fallback: derive from YMUX_PANE_ID.
pub fn resolve_session_name() -> Option<String> {
    if std::env::var_os("TMUX").is_some() {
        let mut cmd = std::process::Command::new("tmux");
        cmd.arg("display-message").arg("-p");
        if let Ok(pane) = std::env::var("TMUX_PANE") {
            if !pane.is_empty() {
                cmd.arg("-t").arg(pane);
            }
        }
        cmd.arg("#S");
        if let Ok(out) = cmd.output() {
            if out.status.success() {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    let pane_id = std::env::var("YMUX_PANE_ID").ok()?;
    if pane_id.is_empty() {
        return None;
    }
    Some(sanitize_tmux_session_name(&pane_id))
}

/// Cap for anything that lands in a display field — shared by the
/// transcript title and the derived session name.
const MAX_TITLE_CHARS: usize = 80;

/// Best title for a Claude session, from its transcript JSONL: the LAST
/// `{"type":"summary","summary":...}` line wins (Claude Code's own session
/// title, updated over time); fallback is the first real user message,
/// truncated. Scans only the head of the file — summaries live at the
/// top, and a bounded read keeps the per-turn hook cheap on huge logs.
pub fn extract_transcript_title(path: &str) -> Option<String> {
    scan_transcript(path)?.title()
}

/// What one bounded pass over a transcript yields. Kept as a struct so
/// the `stop` hook can rebuild a lost `auto_name` (which needs the first
/// user message AND its timestamp) from the same single read that
/// produces the title.
pub struct TranscriptScan {
    pub summary: Option<String>,
    pub first_user: Option<String>,
    /// ISO timestamp Claude Code stamps on the first user entry — the
    /// session's true start. Absent on very old transcripts.
    pub first_user_at: Option<String>,
}

impl TranscriptScan {
    /// The display title: last summary, else the first user message.
    fn title(&self) -> Option<String> {
        let title = self.summary.as_deref().or(self.first_user.as_deref())?;
        Some(truncate_chars(title, MAX_TITLE_CHARS))
    }

    /// See `recover_session_name`.
    fn session_name(&self) -> Option<String> {
        let prompt = self.first_user.as_deref()?;
        if !prompt_can_name(prompt) {
            return None;
        }
        Some(match self.first_user_at.as_deref().and_then(parse_stamp) {
            Some(stamp) => compose_session_name(prompt, &stamp),
            None => derive_session_name(prompt),
        })
    }
}

fn scan_transcript(path: &str) -> Option<TranscriptScan> {
    const MAX_LINES: usize = 250;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut summary: Option<String> = None;
    let mut first_user: Option<String> = None;
    let mut first_user_at: Option<String> = None;
    for line in reader.lines().take(MAX_LINES) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("summary") => {
                if let Some(s) = v.get("summary").and_then(|s| s.as_str()) {
                    if !s.trim().is_empty() {
                        summary = Some(s.trim().to_string());
                    }
                }
            }
            Some("user") if first_user.is_none() => {
                if let Some(text) = extract_user_text(&v) {
                    first_user = Some(text);
                    first_user_at = v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string());
                }
            }
            _ => {}
        }
    }
    Some(TranscriptScan { summary, first_user, first_user_at })
}

/// Rebuild the session name from the transcript, for a session that lost
/// (or never got) its `auto_name`.
///
/// Why this exists: `auto_name` is written ONCE, but the meta file is
/// last-writer-wins over tmp+rename. That store's own comment says a lost
/// read-modify-write "self-heals within one turn" — true for
/// `claude_title`, which every `stop` rewrites, and FALSE for a
/// write-once field: one lost race erases the name permanently. Observed
/// live on 2026-08-18, with several panes writing concurrently.
///
/// The timestamp comes from the first user entry, NOT from `now()` —
/// otherwise each heal would stamp a different time and the "stable"
/// name would churn, which is the whole thing this feature exists to
/// avoid. It also gives sessions that predate the feature their real
/// opening line instead of a mid-conversation prompt.
pub fn recover_session_name(transcript_path: &str) -> Option<String> {
    scan_transcript(transcript_path)?.session_name()
}

/// ISO-8601 (what Claude Code writes) → the same local `%Y-%m-%d %H:%M`
/// shape `derive_session_name` produces. `None` on anything unparseable,
/// so the caller falls back to wall-clock rather than inventing a time.
fn parse_stamp(iso: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
}

/// First-user-message fallback text. Skips Claude Code's synthetic
/// user entries (slash-command echoes etc. start with `<`).
fn extract_user_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else {
        content
            .as_array()?
            .iter()
            .find_map(|b| {
                (b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .then(|| b.get("text").and_then(|t| t.as_str()))
                    .flatten()
            })?
            .to_string()
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    Some(text.replace(['\n', '\r'], " "))
}

/// Filler words that carry no topic. Deliberately small — the goal is to
/// skip "בא נעבור על..." / "can you please fix..." openers, NOT to build a
/// linguistics-grade stoplist. Verbs stay in: "fix", "add", "תעדכן" ARE
/// what the session is about.
const STOPWORDS: &[&str] = &[
    // English
    "the", "a", "an", "to", "for", "of", "in", "on", "at", "and", "or", "is", "are", "it",
    "this", "that", "my", "me", "i", "we", "us", "you", "your", "please", "can", "could",
    "would", "should", "let", "lets", "so", "just", "with", "from", "be", "do", "does",
    "did", "have", "has", "had", "will", "want", "need", "there", "here", "what", "hey",
    "ok", "okay", "now", "then",
    // Hebrew
    "את", "של", "אני", "לי", "לנו", "לך", "זה", "זאת", "הזה", "הזאת", "הוא", "היא", "הם",
    "יש", "עם", "על", "אל", "כי", "גם", "אבל", "או", "אם", "בבקשה", "תודה", "בא", "נו",
    "אז", "רק", "כל", "כמו", "מה", "וגם", "אחר", "כדי", "לא", "כן", "היי",
];

/// Longest token we keep. Long enough for `authentication`, short enough
/// that one runaway word can't eat the whole name.
const MAX_WORD_CHARS: usize = 20;

/// Is this token a filler? Compares case-folded and stripped of a leading
/// Hebrew conjunction/definite prefix (ו/ה/ב/ל/ש + כש), so "והתהליך" and
/// "בבקשה" are judged on their stem.
fn is_stopword(word: &str) -> bool {
    let lower = word.to_lowercase();
    if STOPWORDS.contains(&lower.as_str()) {
        return true;
    }
    let mut chars = lower.chars();
    match chars.next() {
        Some('ו') | Some('ה') | Some('ב') | Some('ל') | Some('ש') | Some('כ') | Some('מ') => {
            let stem: String = chars.collect();
            !stem.is_empty() && STOPWORDS.contains(&stem.as_str())
        }
        _ => false,
    }
}

/// The two most topical-looking words of a prompt, joined by a space.
///
/// Strips markdown/punctuation, drops fillers, pure numbers, and 1-char
/// tokens, then takes the first two survivors. `None` when the prompt has
/// nothing usable (empty, punctuation only, all fillers) — the caller then
/// falls back to a timestamp-only name.
///
/// Deliberately heuristic and offline: this runs inside the
/// UserPromptSubmit hook, which gates the START of a turn. An LLM call
/// here would put seconds of latency in front of every first prompt.
fn two_words(prompt: &str) -> Option<String> {
    let picked: Vec<String> = prompt
        .split(|c: char| c.is_whitespace())
        .map(|tok| {
            tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .to_string()
        })
        .filter(|w| {
            let n = w.chars().count();
            n >= 2 && !w.chars().all(|c| c.is_ascii_digit()) && !is_stopword(w)
        })
        .take(2)
        .map(|w| truncate_chars(&w, MAX_WORD_CHARS))
        .collect();

    if picked.is_empty() {
        None
    } else {
        Some(picked.join(" "))
    }
}

/// A session's stable display name: `"<two words> · <YYYY-MM-DD HH:MM>"`,
/// degrading to the timestamp alone when the prompt yields no usable
/// words. Local server time — the hook runs on the machine hosting tmux,
/// so this reads as "when I started this" to whoever is sitting there.
///
/// Rule #1: the result embeds user content. It belongs in the meta file,
/// never in a log line.
pub fn derive_session_name(prompt: &str) -> String {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    compose_session_name(prompt, &stamp)
}

/// The one place the name's shape is defined, so the live path and the
/// recovery path can never drift into producing different strings for
/// the same session.
fn compose_session_name(prompt: &str, stamp: &str) -> String {
    match two_words(prompt) {
        Some(words) => truncate_chars(&format!("{words} · {stamp}"), MAX_TITLE_CHARS),
        None => stamp.to_string(),
    }
}

/// Prompts that shouldn't name a session: empty ones, and slash commands
/// (`/rename`, `/clear`) — the command is chrome, not the session's
/// subject. The next real prompt names it instead.
fn prompt_can_name(prompt: &str) -> bool {
    let t = prompt.trim();
    !t.is_empty() && !t.starts_with('/')
}

/// Should this prompt (re)name the session? Tri-state, same convention as
/// the desktop's `update_pane_in`: `None` = leave `auto_name` alone,
/// `Some(None)` = clear it, `Some(Some(n))` = set it to `n`.
///
/// The rule is "once per CLAUDE session", not "once per tmux session" —
/// a pane outlives `claude`, so its meta key gets reused and a second
/// `claude` run in the same pane must earn its own name. That's why the
/// recorded session id is compared rather than just testing for absence.
///
/// Pure so the state machine is testable without HOME, tmux, or a clock.
fn decide_auto_name(
    existing: Option<&str>,
    recorded_sid: Option<&str>,
    current_sid: Option<&str>,
    prompt: &str,
) -> Option<Option<String>> {
    let is_new_session = match (recorded_sid, current_sid) {
        (Some(recorded), Some(current)) => recorded != current,
        // No id on either side: can't tell sessions apart, so fall back
        // to "name it if it has no name".
        _ => false,
    };
    if !(existing.is_none() || is_new_session) {
        return None;
    }
    if prompt_can_name(prompt) {
        return Some(Some(derive_session_name(prompt)));
    }
    // A new Claude session opening with a slash command: drop the
    // previous conversation's name rather than let it mislabel this one.
    // The next real prompt claims it.
    if is_new_session {
        Some(None)
    } else {
        None
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Decode `--label-hex` (hex-encoded UTF-8). Hex sidesteps every shell /
/// SSH-exec quoting hazard for Hebrew/RTL labels.
pub fn decode_hex_utf8(hex: &str) -> Result<String, String> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let chars: Vec<char> = hex.chars().collect();
    for pair in chars.chunks(2) {
        let hi = pair[0].to_digit(16).ok_or("bad hex digit")?;
        let lo = pair[1].to_digit(16).ok_or("bad hex digit")?;
        bytes.push(((hi << 4) | lo) as u8);
    }
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8".into())
}

/// Called from the Claude `stop` / `session-end` hooks (env-gated to
/// ymux panes by the caller). Best-effort: returns what it learned so
/// the caller can enrich its feed.push params; `Err` carries an error
/// KIND string safe for hook-debug.log (no user content).
///
/// Returns `(display_title, tmux_session_name)`. The display title is
/// `auto_name` when the session has one (stable, set at the first prompt)
/// and the churning transcript title otherwise — so a session named on
/// turn 1 keeps that name on the pane header for its whole life.
pub fn handle_hook(
    subcommand: &str,
    payload: &serde_json::Value,
) -> Result<(Option<String>, Option<String>), String> {
    let name = resolve_session_name();
    match subcommand {
        // Fires at the start of EVERY turn, in every permission mode. We
        // only act on the first one of each Claude session: that prompt
        // is what the session is about, and the name must not move
        // afterwards.
        "user-prompt-submit" => {
            let Some(name) = name else { return Err("no-session-name".into()) };
            let session_id = payload.get("session_id").and_then(|v| v.as_str());
            let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");

            let mut meta = load_meta();
            let entry = meta.sessions.entry(name.clone()).or_default();

            if let Some(next) = decide_auto_name(
                entry.auto_name.as_deref(),
                entry.claude_session_id.as_deref(),
                session_id,
                prompt,
            ) {
                entry.auto_name = next;
            }
            if let Some(sid) = session_id {
                entry.claude_session_id = Some(sid.to_string());
            }
            entry.updated_at = Some(now_rfc3339());
            let display = entry.auto_name.clone();
            // Phase 86: no prune here — it forks `tmux list-sessions`, and
            // `stop` (every turn) plus `session-end` already cover it.
            save_meta_atomic(&meta).map_err(|_| "save-failed".to_string())?;
            Ok((display, Some(name)))
        }
        "stop" => {
            let Some(name) = name else { return Err("no-session-name".into()) };
            let session_id = payload.get("session_id").and_then(|v| v.as_str());
            // Phase 86: ONE bounded read of the transcript feeds both the
            // title and the auto_name heal below.
            let scan = payload
                .get("transcript_path")
                .and_then(|v| v.as_str())
                .and_then(scan_transcript);
            let title = scan.as_ref().and_then(TranscriptScan::title);
            let mut meta = load_meta();
            let entry = meta.sessions.entry(name.clone()).or_default();
            if let Some(sid) = session_id {
                entry.claude_session_id = Some(sid.to_string());
            }
            if title.is_some() {
                entry.claude_title = title.clone();
            }
            // Phase 81.G: heal a missing auto_name from the transcript.
            // `stop` fires every turn, so this is what makes a write-once
            // field survive a lost read-modify-write in a last-writer-wins
            // store — the same self-healing claude_title has always had.
            // Idempotent: the name is rebuilt from the first user message
            // and ITS timestamp, so re-healing reproduces the same string.
            if entry.auto_name.is_none() {
                entry.auto_name = scan.as_ref().and_then(TranscriptScan::session_name);
            }
            entry.updated_at = Some(now_rfc3339());
            // The stable name wins over the freshly-extracted title —
            // see this function's doc comment.
            let display = entry.auto_name.clone().or(title);
            prune(&mut meta);
            save_meta_atomic(&meta).map_err(|_| "save-failed".to_string())?;
            Ok((display, Some(name)))
        }
        "session-end" => {
            let mut meta = load_meta();
            if prune(&mut meta) {
                save_meta_atomic(&meta).map_err(|_| "save-failed".to_string())?;
            }
            Ok((None, name))
        }
        _ => Ok((None, name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_matches_desktop() {
        assert_eq!(sanitize_tmux_session_name("p_18f3a2c_1"), "ymux-p_18f3a2c_1");
        assert_eq!(sanitize_tmux_session_name("a.b:c d"), "ymux-a_b_c_d");
    }

    #[test]
    fn hex_roundtrip_hebrew() {
        let label = "מחקר X";
        let hex: String = label.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(decode_hex_utf8(&hex).unwrap(), label);
        assert!(decode_hex_utf8("zz").is_err());
        assert!(decode_hex_utf8("abc").is_err());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "א".repeat(100);
        let t = truncate_chars(&s, 80);
        assert!(t.chars().count() <= 80);
        assert!(t.ends_with('…'));
        assert_eq!(truncate_chars("short", 80), "short");
    }

    #[test]
    fn transcript_title_prefers_last_summary() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ymux-meta-test-{}.jsonl", std::process::id()));
        let lines = [
            r#"{"type":"summary","summary":"Old title","leafUuid":"a"}"#,
            r#"{"type":"summary","summary":"Fix auth bug","leafUuid":"b"}"#,
            r#"{"type":"user","message":{"role":"user","content":"hello world"}}"#,
        ]
        .join("\n");
        std::fs::write(&path, lines).unwrap();
        let title = extract_transcript_title(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert_eq!(title.as_deref(), Some("Fix auth bug"));
    }

    /// The `<two words> · ` prefix, or None when the name is
    /// timestamp-only. Keeps the assertions off the wall clock.
    fn words_of(name: &str) -> Option<&str> {
        name.split_once(" · ").map(|(w, _)| w)
    }

    fn stamp_of(name: &str) -> &str {
        name.split_once(" · ").map(|(_, s)| s).unwrap_or(name)
    }

    #[test]
    fn session_name_picks_two_topical_hebrew_words() {
        let name = derive_session_name("בא נעבור על התהליך שקובע את השם של ה-PANE");
        // "בא" is a filler; "נעבור" + "התהליך" carry the topic.
        assert_eq!(words_of(&name), Some("נעבור התהליך"));
    }

    #[test]
    fn session_name_skips_english_filler_openers() {
        assert_eq!(
            words_of(&derive_session_name("can you please fix the auth bug in login flow")),
            Some("fix auth")
        );
        // Verbs are topical and must survive.
        assert_eq!(
            words_of(&derive_session_name("add rename support")),
            Some("add rename")
        );
    }

    #[test]
    fn session_name_is_timestamp_only_when_nothing_survives() {
        for prompt in ["", "   ", "???  !!!", "please can you", "5 42"] {
            let name = derive_session_name(prompt);
            assert_eq!(words_of(&name), None, "prompt {prompt:?} should yield no words");
            assert_eq!(stamp_of(&name).len(), "2026-08-18 20:15".len());
        }
    }

    #[test]
    fn session_name_stamp_shape_and_cap() {
        let name = derive_session_name("refactor pipeline");
        let stamp = stamp_of(&name);
        assert_eq!(stamp.len(), 16, "expected YYYY-MM-DD HH:MM, got {stamp:?}");
        assert_eq!(stamp.chars().nth(4), Some('-'));
        assert_eq!(stamp.chars().nth(13), Some(':'));
        // One runaway token can't eat the name.
        let long = derive_session_name(&format!("{} tail", "x".repeat(60)));
        assert!(words_of(&long).unwrap().chars().count() <= MAX_WORD_CHARS * 2 + 1);
        assert!(long.chars().count() <= MAX_TITLE_CHARS);
    }

    #[test]
    fn slash_commands_do_not_name_a_session() {
        assert!(!prompt_can_name("/rename add_rename"));
        assert!(!prompt_can_name("   "));
        assert!(prompt_can_name("fix the bug"));
    }

    #[test]
    fn one_char_and_numeric_tokens_are_skipped() {
        assert_eq!(words_of(&derive_session_name("a 42 deploy script")), Some("deploy script"));
    }

    #[test]
    fn hebrew_prefixed_fillers_are_stripped() {
        // "וגם"/"שזה" are fillers under a conjunction prefix; "לתקן" is not.
        assert_eq!(words_of(&derive_session_name("וגם שזה לתקן באג")), Some("לתקן באג"));
    }

    #[test]
    fn first_prompt_names_an_unnamed_session() {
        let d = decide_auto_name(None, None, Some("sid-1"), "fix auth bug");
        assert_eq!(words_of(d.unwrap().unwrap().as_str()), Some("fix auth"));
    }

    #[test]
    fn later_prompts_never_rename_the_same_session() {
        // Same Claude session id → the name set on turn 1 stands, no
        // matter how the conversation drifts.
        assert!(decide_auto_name(
            Some("fix auth · 2026-08-18 20:15"),
            Some("sid-1"),
            Some("sid-1"),
            "actually now refactor the parser"
        )
        .is_none());
    }

    #[test]
    fn a_new_claude_session_in_the_same_pane_earns_a_new_name() {
        let d = decide_auto_name(
            Some("fix auth · 2026-08-18 20:15"),
            Some("sid-1"),
            Some("sid-2"),
            "deploy the staging box",
        );
        assert_eq!(words_of(d.unwrap().unwrap().as_str()), Some("deploy staging"));
    }

    #[test]
    fn slash_command_defers_naming_instead_of_burning_the_slot() {
        // Unnamed session, slash command → leave it unnamed…
        assert!(decide_auto_name(None, None, Some("sid-1"), "/rename foo").is_none());
        // …and the next real prompt gets to name it.
        let d = decide_auto_name(None, Some("sid-1"), Some("sid-1"), "rename support");
        assert_eq!(words_of(d.unwrap().unwrap().as_str()), Some("rename support"));
    }

    #[test]
    fn a_new_session_opening_with_a_slash_command_clears_the_stale_name() {
        assert_eq!(
            decide_auto_name(Some("old name · 2026-08-18 20:15"), Some("sid-1"), Some("sid-2"), "/clear"),
            Some(None)
        );
    }

    #[test]
    fn missing_session_ids_degrade_to_name_if_absent() {
        // No id anywhere: name it once, then leave it alone.
        assert!(decide_auto_name(None, None, None, "fix the thing").is_some());
        assert!(decide_auto_name(Some("x · y"), None, None, "fix the thing").is_none());
    }

    /// Write a transcript and return its path (caller removes it).
    fn write_transcript(tag: &str, lines: &[&str]) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("ymux-recover-{tag}-{}.jsonl", std::process::id()));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn recovery_rebuilds_the_name_from_the_first_message_and_its_timestamp() {
        let path = write_transcript(
            "ok",
            &[
                r#"{"type":"summary","summary":"a much later summary"}"#,
                r#"{"type":"user","timestamp":"2026-08-18T18:47:13Z","message":{"role":"user","content":"deploy the staging box now"}}"#,
                r#"{"type":"user","timestamp":"2026-08-18T19:30:00Z","message":{"role":"user","content":"actually refactor everything"}}"#,
            ],
        );
        let name = recover_session_name(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        // First message wins over the summary AND over later prompts.
        assert_eq!(words_of(&name), Some("deploy staging"));
        // Stamp comes from the transcript, not the wall clock, so the
        // date is the session's, not today's.
        assert!(stamp_of(&name).starts_with("2026-08-18"), "got {name:?}");
    }

    #[test]
    fn recovery_is_idempotent() {
        let path = write_transcript(
            "idem",
            &[r#"{"type":"user","timestamp":"2026-08-18T18:47:13Z","message":{"role":"user","content":"fix the auth bug"}}"#],
        );
        let a = recover_session_name(path.to_str().unwrap()).unwrap();
        let b = recover_session_name(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        // The whole point: healing twice must not produce a new name.
        assert_eq!(a, b);
        assert_eq!(words_of(&a), Some("fix auth"));
    }

    #[test]
    fn recovery_declines_a_slash_command_opener() {
        let path = write_transcript(
            "slash",
            &[r#"{"type":"user","timestamp":"2026-08-18T18:47:13Z","message":{"role":"user","content":"/clear"}}"#],
        );
        let got = recover_session_name(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, None);
    }

    #[test]
    fn recovery_without_a_timestamp_still_names() {
        let path = write_transcript(
            "nots",
            &[r#"{"type":"user","message":{"role":"user","content":"fix the auth bug"}}"#],
        );
        let name = recover_session_name(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(words_of(&name), Some("fix auth"));
        assert_eq!(stamp_of(&name).len(), 16);
    }

    #[test]
    fn recovery_on_a_missing_transcript_is_none() {
        assert_eq!(recover_session_name("/nonexistent/transcript.jsonl"), None);
    }

    #[test]
    fn parse_stamp_matches_the_live_format() {
        let s = parse_stamp("2026-08-18T18:47:13Z").unwrap();
        assert_eq!(s.len(), 16);
        assert_eq!(s.chars().nth(4), Some('-'));
        assert_eq!(s.chars().nth(13), Some(':'));
        assert_eq!(parse_stamp("not a timestamp"), None);
    }

    #[test]
    fn transcript_title_falls_back_to_first_user_message() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ymux-meta-test2-{}.jsonl", std::process::id()));
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"real question here"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}"#,
        ]
        .join("\n");
        std::fs::write(&path, lines).unwrap();
        let title = extract_transcript_title(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert_eq!(title.as_deref(), Some("real question here"));
    }
}
