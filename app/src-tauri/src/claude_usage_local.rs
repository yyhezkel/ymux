//! Phase 84.E — the LOCAL half of `/claude-usage`.
//!
//! Deliberate mirror of `server/internal/insights/claudeusage.go`: same scan,
//! same JSON field names, same clamping. That is the pattern `insights_local`
//! already established for `/current` and friends — remote workspaces are
//! served by the Go daemon over SSH, local ones by this in-process
//! implementation, and `insights_fetch` routes between them so the frontend
//! never branches.
//!
//! Two implementations of one aggregation is real duplication, and it is the
//! cheaper of the two options: `~/.claude/projects` is routinely 240 MB across
//! 170 transcripts, so the alternative — SFTP-mirroring the remote tree to the
//! desktop and parsing it once, here — would pull hundreds of megabytes over
//! the wire every time the tab opens.
//!
//! Like the Go side, this counts tokens and does not price them. The price
//! table lives in `app/src/claudePricing.ts`, in one place.
//!
//! Rule #1: transcripts are the user's prompts and command output. Nothing
//! here reads message CONTENT — only `message.model`, `message.usage`, the
//! timestamp, the session id and the cwd — and nothing is logged but counts.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Serialize;

/// One transcript line's worth of tokens. Cache writes are split because a
/// 1-hour write costs 2x base input against a 5-minute write's 1.25x, and
/// collapsing them would understate a long session.
#[derive(Clone, Copy, Default, Serialize)]
pub struct ClaudeTokens {
    pub calls: u64,
    pub in_tokens: u64,
    pub out_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl ClaudeTokens {
    fn add(&mut self, o: &ClaudeTokens) {
        self.calls += o.calls;
        self.in_tokens += o.in_tokens;
        self.out_tokens += o.out_tokens;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
    }
    /// Sort weight — the desktop re-sorts by cost once it has applied prices.
    fn weight(&self) -> u64 {
        self.in_tokens + self.out_tokens + self.cache_write
    }
}

#[derive(Serialize)]
pub struct ClaudeBucket {
    pub t: i64,
    #[serde(flatten)]
    pub tokens: ClaudeTokens,
}

#[derive(Serialize)]
pub struct ClaudeRow {
    pub key: String,
    #[serde(flatten)]
    pub tokens: ClaudeTokens,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub speed: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub started: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub ended: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Serialize)]
pub struct ClaudeUsageReport {
    pub since: i64,
    pub until: i64,
    pub bucket_s: i64,
    pub scanned_files: u32,
    pub skipped_files: u32,
    pub parse_errors: u32,
    pub took_ms: u64,
    pub totals: ClaudeTokens,
    pub sidechain: ClaudeTokens,
    pub sessions: usize,
    pub projects: usize,
    pub first_ts: i64,
    pub last_ts: i64,
    pub series: Vec<ClaudeBucket>,
    pub by_model: Vec<ClaudeRow>,
    pub by_project: Vec<ClaudeRow>,
    pub by_session: Vec<ClaudeRow>,
}

/// Hourly buckets, always. The desktop rolls them up into LOCAL days itself,
/// which is the only way a day boundary lands where the reader expects it.
const SERIES_STEP: i64 = 3600;
const ROW_LIMIT: usize = 20;

fn projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Accumulator shared by the three rollups.
#[derive(Default)]
struct Agg {
    tok: HashMap<String, ClaudeTokens>,
    speed: HashMap<String, String>,
    project: HashMap<String, String>,
    first: HashMap<String, i64>,
    last: HashMap<String, i64>,
}

impl Agg {
    fn add(&mut self, key: &str, ts: i64, t: &ClaudeTokens) {
        let entry = self.tok.entry(key.to_string()).or_default();
        entry.add(t);
        let f = self.first.entry(key.to_string()).or_insert(ts);
        if ts < *f {
            *f = ts;
        }
        let l = self.last.entry(key.to_string()).or_insert(ts);
        if ts > *l {
            *l = ts;
        }
    }

    fn rows(&self, limit: usize) -> Vec<ClaudeRow> {
        let mut out: Vec<ClaudeRow> = self
            .tok
            .iter()
            .map(|(k, t)| ClaudeRow {
                key: k.clone(),
                tokens: *t,
                speed: self.speed.get(k).cloned().unwrap_or_default(),
                project: self.project.get(k).cloned().unwrap_or_default(),
                started: self.first.get(k).copied().unwrap_or(0),
                ended: self.last.get(k).copied().unwrap_or(0),
            })
            .collect();
        out.sort_by(|a, b| {
            b.tokens
                .weight()
                .cmp(&a.tokens.weight())
                .then_with(|| a.key.cmp(&b.key))
        });
        out.truncate(limit);
        out
    }
}

/// Scan the transcript tree for one window.
pub fn scan(root: &Path, since: i64, until: i64) -> ClaudeUsageReport {
    let t0 = std::time::Instant::now();
    let mut rep = ClaudeUsageReport {
        since,
        until,
        bucket_s: SERIES_STEP,
        scanned_files: 0,
        skipped_files: 0,
        parse_errors: 0,
        took_ms: 0,
        totals: ClaudeTokens::default(),
        sidechain: ClaudeTokens::default(),
        sessions: 0,
        projects: 0,
        first_ts: 0,
        last_ts: 0,
        series: Vec::new(),
        by_model: Vec::new(),
        by_project: Vec::new(),
        by_session: Vec::new(),
    };

    let mut buckets: HashMap<i64, ClaudeTokens> = HashMap::new();
    let mut by_model = Agg::default();
    let mut by_project = Agg::default();
    let mut by_session = Agg::default();

    // A machine that has never run Claude Code has no directory at all. That
    // is an empty report, not an error.
    let dirs_iter = match std::fs::read_dir(root) {
        Ok(d) => d,
        Err(_) => return finish(rep, buckets, &by_model, &by_project, &by_session, t0),
    };

    for dir in dirs_iter.flatten() {
        if !dir.path().is_dir() {
            continue;
        }
        let files = match std::fs::read_dir(dir.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // The mtime prune: a transcript's mtime is its LAST append, so a
            // file older than the window cannot hold an in-window line and is
            // never opened. This is what keeps a 240 MB tree affordable.
            let fresh = f
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64 >= since)
                .unwrap_or(false);
            if !fresh {
                rep.skipped_files += 1;
                continue;
            }
            rep.scanned_files += 1;
            scan_file(
                &path,
                since,
                until,
                &mut rep,
                &mut buckets,
                &mut by_model,
                &mut by_project,
                &mut by_session,
            );
        }
    }

    finish(rep, buckets, &by_model, &by_project, &by_session, t0)
}

fn finish(
    mut rep: ClaudeUsageReport,
    buckets: HashMap<i64, ClaudeTokens>,
    by_model: &Agg,
    by_project: &Agg,
    by_session: &Agg,
    t0: std::time::Instant,
) -> ClaudeUsageReport {
    let mut series: Vec<ClaudeBucket> = buckets
        .into_iter()
        .map(|(t, tokens)| ClaudeBucket { t, tokens })
        .collect();
    series.sort_by_key(|b| b.t);
    rep.series = series;

    rep.by_model = by_model.rows(ROW_LIMIT);
    // Split the model+speed key back apart — the Go side does the same, so the
    // frontend sees one shape.
    for row in &mut rep.by_model {
        let split = row
            .key
            .split_once(' ')
            .map(|(m, sp)| (m.to_string(), sp.to_string()));
        if let Some((model, speed)) = split {
            row.key = model;
            row.speed = speed;
        }
    }
    rep.by_project = by_project.rows(ROW_LIMIT);
    rep.by_session = by_session.rows(ROW_LIMIT);
    rep.sessions = by_session.tok.len();
    rep.projects = by_project.tok.len();
    rep.took_ms = t0.elapsed().as_millis() as u64;
    rep
}

/// Token field rule shared with Go's `tokenCount`: absent or null is 0, a JSON
/// non-negative integer is itself, anything else (1.5, -1, "7", true) is Err.
fn token_u64(obj: &serde_json::Value, key: &str) -> Result<u64, ()> {
    match obj.get(key) {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(n) => n.as_u64().ok_or(()),
    }
}

/// The six token fields of one `message.usage` block, validated.
#[derive(Default)]
struct UsageTokens {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    w5: u64,
    w1h: u64,
}

/// `usage` and `cache_creation` must each be an object when present and
/// non-null; every token field inside must pass `token_u64`.
fn parse_usage(usage: &serde_json::Value) -> Result<UsageTokens, ()> {
    if !usage.is_object() {
        return Err(());
    }
    let mut t = UsageTokens {
        input: token_u64(usage, "input_tokens")?,
        output: token_u64(usage, "output_tokens")?,
        cache_read: token_u64(usage, "cache_read_input_tokens")?,
        cache_write: token_u64(usage, "cache_creation_input_tokens")?,
        ..Default::default()
    };
    match usage.get("cache_creation") {
        None | Some(serde_json::Value::Null) => {}
        Some(cc) if cc.is_object() => {
            t.w5 = token_u64(cc, "ephemeral_5m_input_tokens")?;
            t.w1h = token_u64(cc, "ephemeral_1h_input_tokens")?;
        }
        Some(_) => return Err(()),
    }
    Ok(t)
}

#[allow(clippy::too_many_arguments)]
fn scan_file(
    path: &Path,
    since: i64,
    until: i64,
    rep: &mut ClaudeUsageReport,
    buckets: &mut HashMap<i64, ClaudeTokens>,
    by_model: &mut Agg,
    by_project: &mut Agg,
    by_session: &mut Agg,
) {
    let fh = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            rep.skipped_files += 1;
            return;
        }
    };
    for line in BufReader::new(fh).lines() {
        let line = match line {
            Ok(l) => l,
            // A single unreadable line (invalid UTF-8, torn write) must not
            // abort the file.
            Err(_) => {
                rep.parse_errors += 1;
                continue;
            }
        };
        // Cheap reject before the JSON parser. Most lines are user turns,
        // attachments and tool results with no usage block at all.
        if !line.contains("\"usage\"") {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                rep.parse_errors += 1;
                continue;
            }
        };
        // Validate before the type filter, as Go's typed decode does: a bad
        // token field on any line containing "usage" is one parse error.
        let parsed = match v.get("message").and_then(|m| m.get("usage")) {
            None | Some(serde_json::Value::Null) => UsageTokens::default(),
            Some(u) => match parse_usage(u) {
                Ok(t) => t,
                Err(()) => {
                    rep.parse_errors += 1;
                    continue;
                }
            },
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let model = match msg.get("model").and_then(|m| m.as_str()) {
            Some(m) if !m.is_empty() => m.to_string(),
            _ => continue,
        };
        let usage = match msg.get("usage") {
            Some(u) => u,
            None => continue,
        };
        let ts = match v.get("timestamp").and_then(|t| t.as_str()) {
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(d) => d.timestamp(),
                Err(_) => {
                    rep.parse_errors += 1;
                    continue;
                }
            },
            None => continue,
        };
        if ts < since || ts > until {
            continue;
        }

        let (mut w5, w1h) = (parsed.w5, parsed.w1h);
        let cw = parsed.cache_write;
        // Older transcripts carry only the flat total. Attribute it to the
        // CHEAPER bucket, so an unknown split under-states rather than
        // over-states what the user is told they spent.
        if w5 == 0 && w1h == 0 && cw > 0 {
            w5 = cw;
        }
        let tok = ClaudeTokens {
            calls: 1,
            in_tokens: parsed.input,
            out_tokens: parsed.output,
            cache_read: parsed.cache_read,
            cache_write: cw,
            cache_write_5m: w5,
            cache_write_1h: w1h,
        };

        rep.totals.add(&tok);
        if v.get("isSidechain").and_then(|s| s.as_bool()).unwrap_or(false) {
            rep.sidechain.add(&tok);
        }
        if rep.first_ts == 0 || ts < rep.first_ts {
            rep.first_ts = ts;
        }
        if ts > rep.last_ts {
            rep.last_ts = ts;
        }

        buckets
            .entry((ts / SERIES_STEP) * SERIES_STEP)
            .or_default()
            .add(&tok);

        // Keyed model+speed: fast mode is priced apart, so it cannot share a
        // row. A space is a safe separator — a model id never contains one.
        let speed = usage
            .get("speed")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("standard")
            .to_string();
        let mk = format!("{model} {speed}");
        by_model.add(&mk, ts, &tok);
        by_model.speed.insert(mk, speed);

        if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()).filter(|s| !s.is_empty()) {
            by_project.add(cwd, ts, &tok);
            if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) {
                by_session.project.insert(sid.to_string(), cwd.to_string());
            }
        }
        if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
            by_session.add(sid, ts, &tok);
        }
    }
}

/// `/claude-usage?since=&until=` for a local workspace. Same clamping contract
/// as the Go handler: the desktop drew the range picker, so a nonsense value is
/// corrected rather than turned into an error.
pub fn route(query: &str) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    let mut until = parse_i64(query, "until").unwrap_or(0);
    if until <= 0 || until > now {
        until = now;
    }
    let mut since = parse_i64(query, "since").unwrap_or(0);
    if since <= 0 {
        since = until - 24 * 3600;
    }
    let oldest = until - 365 * 86400;
    if since < oldest {
        since = oldest;
    }
    let newest = until - 300;
    if since > newest {
        since = newest;
    }

    let rep = match projects_dir() {
        Some(root) => scan(&root, since, until),
        None => scan(Path::new(""), since, until),
    };
    crate::log_debug(
        "MONITOR",
        &format!(
            "claude usage local files={} skipped={} calls={} took_ms={}",
            rep.scanned_files, rep.skipped_files, rep.totals.calls, rep.took_ms
        ),
    );
    serde_json::to_string(&rep).map_err(|e| format!("serialize claude usage: {e}"))
}

fn parse_i64(q: &str, key: &str) -> Option<i64> {
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
            return it.next().and_then(|v| v.parse().ok());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_transcript(root: &Path, dir: &str, session: &str, lines: &[String]) -> PathBuf {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).expect("mkdir");
        let p = d.join(format!("{session}.jsonl"));
        let mut f = File::create(&p).expect("create");
        for l in lines {
            writeln!(f, "{l}").expect("write");
        }
        p
    }

    fn line(ts: i64, sid: &str, cwd: &str, model: &str, i: u64, o: u64, w5: u64, w1h: u64) -> String {
        let iso = chrono::DateTime::from_timestamp(ts, 0)
            .map(|d| d.to_rfc3339())
            .unwrap_or_default();
        format!(
            r#"{{"type":"assistant","timestamp":"{iso}","sessionId":"{sid}","cwd":"{cwd}","isSidechain":false,"message":{{"model":"{model}","usage":{{"input_tokens":{i},"output_tokens":{o},"cache_read_input_tokens":0,"cache_creation_input_tokens":{cw},"cache_creation":{{"ephemeral_5m_input_tokens":{w5},"ephemeral_1h_input_tokens":{w1h}}},"speed":"standard"}}}}}}"#,
            cw = w5 + w1h
        )
    }

    #[test]
    fn counts_tokens_and_splits_cache_writes() {
        let tmp = std::env::temp_dir().join(format!("ymux-cu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let now = chrono::Utc::now().timestamp();
        write_transcript(
            &tmp,
            "proj",
            "s1",
            &[
                line(now - 600, "s1", "/home/y/a", "claude-opus-5", 100, 200, 400, 500),
                line(now - 300, "s1", "/home/y/a", "claude-opus-5", 10, 20, 40, 0),
            ],
        );
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!(rep.totals.calls, 2);
        assert_eq!(rep.totals.in_tokens, 110);
        assert_eq!(rep.totals.out_tokens, 220);
        assert_eq!(rep.totals.cache_write_5m, 440);
        assert_eq!(rep.totals.cache_write_1h, 500);
        assert_eq!(rep.totals.cache_write, 940);
        assert_eq!(rep.sessions, 1);
        assert_eq!(rep.projects, 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn model_rows_split_the_speed_out_of_the_key() {
        let tmp = std::env::temp_dir().join(format!("ymux-cu2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let now = chrono::Utc::now().timestamp();
        write_transcript(&tmp, "p", "s", &[line(now - 60, "s", "/p", "claude-opus-5", 1, 1, 0, 0)]);
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!(rep.by_model.len(), 1);
        assert_eq!(rep.by_model[0].key, "claude-opus-5");
        assert_eq!(rep.by_model[0].speed, "standard");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_root_is_an_empty_report_not_a_panic() {
        let rep = scan(Path::new("/definitely/not/here"), 0, 1);
        assert_eq!(rep.totals.calls, 0);
        assert!(rep.series.is_empty());
        // Empty vectors must serialize as [] so the client can map without a guard.
        let json = serde_json::to_string(&rep).expect("serialize");
        assert!(json.contains(r#""series":[]"#), "{json}");
        assert!(json.contains(r#""by_model":[]"#), "{json}");
    }

    #[test]
    fn range_is_clamped_never_rejected() {
        let out = route("since=1&until=0").expect("route");
        let v: serde_json::Value = serde_json::from_str(&out).expect("json");
        let since = v["since"].as_i64().unwrap_or(0);
        let until = v["until"].as_i64().unwrap_or(0);
        assert!(since < until, "empty window {since}..{until}");
        assert!(until - since <= 365 * 86400);
    }

    // Past the old 2000-file cap every in-window file must still be scanned.
    #[test]
    fn scan_has_no_file_cap() {
        let tmp = std::env::temp_dir().join(format!("ymux-cu-nocap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let now = chrono::Utc::now().timestamp();
        for i in 0..2001 {
            write_transcript(&tmp, "p", &format!("s{i}"), &[line(now - 60, "s", "/p", "claude-opus-5", 1, 1, 0, 0)]);
        }
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!(rep.scanned_files, 2001);
        assert_eq!(rep.totals.calls, 2001);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // mtime-pruned files still land in skipped_files once the cap is gone.
    #[test]
    fn scan_counts_every_file_in_window() {
        let tmp = std::env::temp_dir().join(format!("ymux-cu-every-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let now = chrono::Utc::now().timestamp();
        for i in 0..2001 {
            let p = write_transcript(&tmp, "p", &format!("s{i}"), &[line(now - 60, "s", "/p", "claude-opus-5", 1, 1, 0, 0)]);
            if i % 2 == 1 {
                let old = std::time::SystemTime::now() - std::time::Duration::from_secs(72 * 3600);
                File::options().write(true).open(&p).expect("open").set_modified(old).expect("mtime");
            }
        }
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!(rep.scanned_files, 1001);
        assert_eq!(rep.skipped_files, 1000);
        assert_eq!(rep.totals.calls, 1001);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Parity pin with Go: a line past 8 MiB must not hide the usage line after it.
    #[test]
    fn scan_keeps_lines_after_oversized_line() {
        let tmp = std::env::temp_dir().join(format!("ymux-cu-bigline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let now = chrono::Utc::now().timestamp();
        let big = format!(r#"{{"type":"user","content":"{}"}}"#, "x".repeat(9 * 1024 * 1024));
        write_transcript(&tmp, "p", "s1", &[big, line(now - 60, "s1", "/p", "claude-opus-5", 5, 6, 0, 0)]);
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!(rep.totals.calls, 1);
        assert_eq!(rep.totals.in_tokens, 5);
        assert_eq!(rep.parse_errors, 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Parity pin with Go: an invalid-UTF-8 line is one parse error, dropped;
    // the usage line after it still counts.
    #[test]
    fn invalid_utf8_line_is_one_parse_error() {
        let tmp = std::env::temp_dir().join(format!("ymux-cu-utf8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let now = chrono::Utc::now().timestamp();
        let p = write_transcript(&tmp, "p", "s1", &[]);
        let mut f = File::options().append(true).open(&p).expect("open");
        f.write_all(b"{\"type\":\"assistant\",\"usage\":\"\xff\xfe\"}\n").expect("write");
        writeln!(f, "{}", line(now - 60, "s1", "/p", "claude-opus-5", 5, 6, 0, 0)).expect("write");
        drop(f);
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!(rep.parse_errors, 1);
        assert_eq!(rep.totals.calls, 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // D4 parity with Go: a non-uint token field is one parse error and no call;
    // null and absent are valid zeros.
    #[test]
    fn rejects_non_uint_token_fields() {
        let now = chrono::Utc::now().timestamp();
        let iso = chrono::DateTime::from_timestamp(now - 60, 0).expect("ts").to_rfc3339();
        let mk = |usage: &str| {
            format!(r#"{{"type":"assistant","timestamp":"{iso}","sessionId":"s","cwd":"/p","message":{{"model":"claude-opus-5","usage":{usage}}}}}"#)
        };
        let bad = [
            r#"{"input_tokens":1.5}"#,
            r#"{"output_tokens":-1}"#,
            r#"{"cache_read_input_tokens":"7"}"#,
            r#"{"cache_creation_input_tokens":true}"#,
            r#"{"cache_creation":{"ephemeral_5m_input_tokens":1.5}}"#,
            r#"{"cache_creation":{"ephemeral_1h_input_tokens":-1}}"#,
            r#"{"input_tokens":1e3}"#,
            r#"{"cache_creation":"x"}"#,
            r#""x""#,
        ];
        for (n, u) in bad.iter().enumerate() {
            let tmp = std::env::temp_dir().join(format!("ymux-cu-uint-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            write_transcript(&tmp, "p", "s1", &[mk(u)]);
            let rep = scan(&tmp, now - 3600, now);
            assert_eq!((rep.parse_errors, rep.totals.calls), (1, 0), "{u}");
            let _ = std::fs::remove_dir_all(&tmp);
        }
        let tmp = std::env::temp_dir().join(format!("ymux-cu-uint-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write_transcript(
            &tmp,
            "p",
            "s1",
            &[mk(r#"{"input_tokens":null,"cache_creation":null}"#), mk(r#"{"output_tokens":4}"#)],
        );
        let rep = scan(&tmp, now - 3600, now);
        assert_eq!((rep.parse_errors, rep.totals.calls, rep.totals.out_tokens), (0, 2, 4));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
