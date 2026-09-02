//! Phase 87 — the active-sessions overview ("סשנים פעילים").
//!
//! One workspace, every multiplexer session on its machine, each with a
//! one-line agent summary and a status read off the screen. The LIST is the
//! existing `pane_list_tmux_sessions` (with `project_path: None`, plus the
//! `owner_cwd` field it grew for grouping); this module owns the two things
//! the picker never needed:
//!
//! - `sessions_overview_summarize` — capture the last screenful of each named
//!   session and run ONE `claude -p` over all of them, on the machine that
//!   holds the sessions. Over SSH the capture and the model call are a single
//!   remote pipeline, so screen bytes never cross to the desktop at all. On a
//!   local workspace (macOS tmux, Windows zellij) the captures are framed in
//!   memory and piped into the local `claude` on stdin.
//! - `sessions_kill_by_name` — kill a session that no pane is attached to,
//!   through the same `kill_target` verbs the pane-bound kill uses.
//!
//! Rename is `tmux_rename_session` in lib.rs, which Phase 87 extended.
//!
//! Rule #1 is the constraint that shaped every log line here: a capture IS
//! PTY content and the model's answer is derived from it. Nothing below logs
//! either — only counts, byte totals, durations and the envelope's own
//! `subtype` / `is_error` flags. The parse-failure path is the tempting one;
//! it logs the length of what it could not parse, never the text.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    kill_pane_session_inner, kill_target, log_debug, log_info, log_warn,
    release_session_owner, session_owner_host_key, shell_quote, AppState, Connection,
    KillSessionOutcome, KillTarget, Session,
};

/// Upper bound on sessions per model call. The frontend chunks above this;
/// a bigger prompt buys nothing but a slower answer.
const MAX_SESSIONS_PER_CALL: usize = 25;
/// Lines of scrollback per session. Enough to see a prompt, a permission
/// card or a stack trace; not enough to make the call expensive.
const CAPTURE_LINES: usize = 40;
/// Per-line cap. A minified bundle echoed to a terminal is one 40 KB line.
const CAPTURE_LINE_CHARS: usize = 240;
/// `claude -p` over 25 screens is typically 10-30 s. Generous on purpose.
const CLAUDE_TIMEOUT_SECS: u64 = 90;
/// One zellij `dump-screen` should be instant; this is a hang guard.
#[cfg(windows)]
const ZELLIJ_DUMP_TIMEOUT_SECS: u64 = 5;

/// The statuses the prompt asks for. Anything else the model says becomes
/// `unknown` — the UI has a pill for that and nothing for a free-form word.
const KNOWN_STATUSES: [&str; 4] = ["idle", "working", "waiting_input", "error"];

/// One row of the answer. `status` is one of `KNOWN_STATUSES` or `unknown`;
/// `summary` is empty when the model said nothing usable about the session.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub name: String,
    pub status: String,
    pub summary: String,
}

/// What the model is asked to emit, one object per session. Every field is
/// optional because a lenient parse is the whole point — one malformed row
/// must not cost the other twenty-four their summaries.
#[derive(Deserialize)]
struct ModelRow {
    i: Option<u64>,
    status: Option<String>,
    summary: Option<String>,
}

// ─── Prompt ──────────────────────────────────────────────────────────────────

/// Language code → the word the model understands. Unknown codes fall back
/// to English rather than to the raw code, which the model might ignore.
fn language_name(lang: &str) -> &'static str {
    match lang {
        "he" => "Hebrew",
        "ar" => "Arabic",
        "ru" => "Russian",
        _ => "English",
    }
}

/// The instruction handed to `claude -p`.
///
/// Sessions are INDEXED, not named, so the model never has to echo a session
/// name back (names can be anything tmux accepts). The text is deliberately
/// one line of plain ASCII with no double quotes and no percent signs: on
/// Windows an npm-installed `claude` is a `.cmd`, which std spawns through
/// `cmd.exe /c` with its own escaping rules, and those two characters are
/// the ones it refuses. `prompt_is_cmd_safe` asserts this in the tests.
fn summary_prompt(lang: &str) -> String {
    format!(
        "Below are terminal screens of several multiplexer sessions, separated by lines of the form \
         ### SESSION <i>. Reply with ONLY a JSON array, one object per session, each shaped like \
         {{i: <number>, status: <one of idle, working, waiting_input, error>, summary: <one short \
         sentence, at most 120 characters, in {lang}, saying what the session is doing>}}. \
         waiting_input means a prompt, question or permission request is waiting for the human. \
         working means a command or an agent is still running. error means the last thing on \
         screen is a failure. idle means a shell prompt with nothing running. No prose, no code fences.",
        lang = language_name(lang),
    )
}

// ─── Capture framing ─────────────────────────────────────────────────────────

/// Drop CSI / OSC / two-byte escape sequences. Only the local path needs
/// this (`capture-pane -p` without `-e` is already plain; zellij's
/// `dump-screen` without `--ansi` should be, but is not documented to be).
/// A hand-rolled state machine because `regex` is not a dependency of this
/// crate and a 20-line loop is not worth adding one for.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: ESC [ … final byte in 0x40..=0x7E
            Some('[') => {
                for n in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: ESC ] … BEL or ESC \
            Some(']') => {
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if n == '\x07' || (prev == '\x1b' && n == '\\') {
                        break;
                    }
                    prev = n;
                }
            }
            // Two-byte escapes (ESC 7, ESC =, …): the byte was consumed.
            Some(_) | None => {}
        }
    }
    out
}

/// Last `CAPTURE_LINES` non-trailing-blank lines, each cut to
/// `CAPTURE_LINE_CHARS` chars, ANSI stripped. Same shape the remote `cut`
/// produces, so the model sees one format from every backend.
fn clip_capture(raw: &str) -> String {
    let clean = strip_ansi(raw);
    let mut lines: Vec<&str> = clean.lines().map(|l| l.trim_end_matches('\r')).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(CAPTURE_LINES);
    lines[start..]
        .iter()
        .map(|l| l.chars().take(CAPTURE_LINE_CHARS).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The stdin the local `claude` gets: every capture behind its 1-based
/// `### SESSION <i>` header, in the order of `names`.
fn frame_captures(captures: &[String]) -> String {
    let mut out = String::new();
    for (idx, text) in captures.iter().enumerate() {
        out.push_str(&format!("\n### SESSION {}\n", idx + 1));
        out.push_str(text);
        out.push('\n');
    }
    out
}

/// The single remote pipeline: capture every session in order, feed the lot
/// to `claude -p` under a login shell (so an npm / nvm / fnm `claude` is on
/// PATH — see `claude_summary::wrap_login`). Names go through `shell_quote`;
/// the prompt through `bash_squote` inside `wrap_login`.
///
/// **The target is `=name:`, colon included.** `=` pins an exact session match
/// (a bare `-t` prefix-matches, and `tax` would hit `tax-contine`), but
/// `capture-pane` takes a PANE target, and tmux 3.4 reads a bare `=name` there
/// as a pane name and answers `can't find pane` — verified live on 2026-09-02.
/// The trailing colon makes it `session:window`, which resolves. Session-level
/// verbs (`rename-session`, `kill-session`) take `=name` as-is.
///
/// `2>/dev/null` on both `tmux` and the model call matters: `addons::exec`
/// merges stderr into the output it returns, and the JSON envelope has to
/// come back clean.
fn build_ssh_summary_script(names: &[String], claude_path: &str, prompt: &str) -> String {
    let quoted: Vec<String> = names.iter().map(|n| shell_quote(n)).collect();
    let model = crate::claude_summary::wrap_login(&format!(
        "{} -p {} --output-format json",
        crate::claude_summary::bash_squote(claude_path),
        crate::claude_summary::bash_squote(prompt),
    ));
    format!(
        "i=0; for s in {names}; do i=$((i+1)); printf '\\n### SESSION %s\\n' \"$i\"; \
         tmux capture-pane -p -t \"=$s:\" -S -{lines} 2>/dev/null | cut -c1-{cols}; done \
         | {model} 2>/dev/null",
        names = quoted.join(" "),
        lines = CAPTURE_LINES,
        cols = CAPTURE_LINE_CHARS,
        model = model,
    )
}

// ─── Envelope parsing ────────────────────────────────────────────────────────

/// The text the model wrote, out of `--output-format json`'s envelope
/// (`{"type":"result","result":"<string>",…}`). A body that is not an
/// envelope at all is returned as-is, so a `claude` old enough to ignore the
/// flag still parses.
fn envelope_result(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(r) = v.get("result").and_then(|r| r.as_str()) {
            return r.to_string();
        }
        if v.get("is_error").and_then(|b| b.as_bool()) == Some(true) {
            return String::new();
        }
    }
    trimmed.to_string()
}

/// First `[` .. last `]`, which also skips ``` fences and any sentence the
/// model put around the array despite being told not to.
fn extract_json_array(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    (end > start).then(|| &text[start..=end])
}

fn normalize_status(s: Option<String>) -> String {
    let s = s.unwrap_or_default().trim().to_ascii_lowercase();
    if KNOWN_STATUSES.contains(&s.as_str()) {
        s
    } else {
        "unknown".to_string()
    }
}

/// Turn the model's answer into one row per requested name, in the caller's
/// order. Lenient by design: a row with a bad index is dropped, a missing
/// row is `unknown`, and a body that is not JSON at all yields `unknown` for
/// everyone — never an `Err`, because the list is still worth showing.
fn parse_summary_envelope(raw: &str, names: &[String]) -> Vec<SessionSummary> {
    let text = envelope_result(raw);
    let rows: Vec<ModelRow> = extract_json_array(&text)
        .and_then(|a| serde_json::from_str::<Vec<ModelRow>>(a).ok())
        .unwrap_or_default();
    let mut out: Vec<SessionSummary> = names
        .iter()
        .map(|n| SessionSummary {
            name: n.clone(),
            status: "unknown".to_string(),
            summary: String::new(),
        })
        .collect();
    for row in rows {
        let Some(i) = row.i.and_then(|i| usize::try_from(i).ok()) else {
            continue;
        };
        let Some(slot) = i.checked_sub(1).and_then(|k| out.get_mut(k)) else {
            continue;
        };
        slot.status = normalize_status(row.status);
        slot.summary = row
            .summary
            .unwrap_or_default()
            .trim()
            .chars()
            .take(200)
            .collect();
    }
    out
}

// ─── Local execution ─────────────────────────────────────────────────────────

/// Run the local `claude -p` with the framed captures on stdin. Mirrors
/// `claude_usage::run_local_usage_probe`: hidden console, `kill_on_drop`
/// (set by `hidden_cmd`), the canonical PATH on unix, a wall-clock timeout.
/// `.stdin(piped())` overrides the `null` `hidden_cmd` sets — builder, last
/// wins.
async fn run_local_claude(prompt: &str, input: &[u8]) -> Result<String, String> {
    use tokio::io::AsyncWriteExt as _;
    let claude = crate::local_setup::resolve_claude_binary()
        .ok_or_else(|| "claude is not installed on this machine".to_string())?;
    let mut c = crate::local_setup::hidden_cmd(&claude.to_string_lossy());
    c.args(["-p", prompt, "--output-format", "json"]);
    c.stdin(std::process::Stdio::piped());
    #[cfg(not(target_os = "windows"))]
    c.env("PATH", crate::local_setup::unix_path_env());
    let mut child = c
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", claude.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "claude stdin unavailable".to_string())?;
    stdin
        .write_all(input)
        .await
        .map_err(|e| format!("claude stdin: {e}"))?;
    stdin
        .shutdown()
        .await
        .map_err(|e| format!("claude stdin close: {e}"))?;
    drop(stdin);
    let out = tokio::time::timeout(
        Duration::from_secs(CLAUDE_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "claude -p timed out".to_string())?
    .map_err(|e| format!("claude: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// macOS: one `capture-pane` per session, argv only (Rule #3). A session
/// that vanished between the list and now captures as empty, not as an
/// error — the model then reports it `unknown`, which is the truth. `=name:`
/// for the same reason as the SSH script: a bare `=name` is not a pane target.
#[cfg(not(windows))]
async fn capture_local_tmux(names: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let target = format!("={name}:");
        let lines = format!("-{CAPTURE_LINES}");
        let text = match crate::local_tmux_output(&["capture-pane", "-p", "-t", &target, "-S", &lines])
            .await
        {
            Ok((Some(0), stdout)) => stdout,
            Ok((code, _)) => {
                log_debug("SESSIONS", &format!("capture-pane: tmux exited {code:?}"));
                String::new()
            }
            Err(e) => {
                log_warn("SESSIONS", &format!("capture-pane: {e}"));
                String::new()
            }
        };
        out.push(clip_capture(&text));
    }
    out
}

/// Windows: `zellij -s <name> action dump-screen`, the viewport on stdout
/// (docs/ZELLIJ.md §4). `-s` is a ROOT option and comes before `action`, the
/// same targeting `zellij_args_write_chars` uses. An EXITED session has no
/// running server to answer, so the frontend does not send those.
#[cfg(windows)]
async fn capture_zellij(names: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let mut c = crate::local_setup::hidden_cmd(&crate::zellij_exe());
        for a in crate::zellij_args_dump_screen(name) {
            c.arg(a);
        }
        let text = match tokio::time::timeout(
            Duration::from_secs(ZELLIJ_DUMP_TIMEOUT_SECS),
            c.output(),
        )
        .await
        {
            Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(Ok(o)) => {
                log_debug(
                    "SESSIONS",
                    &format!("dump-screen: zellij exited {:?}", o.status.code()),
                );
                String::new()
            }
            Ok(Err(e)) => {
                log_warn("SESSIONS", &format!("dump-screen: spawn failed: {e}"));
                String::new()
            }
            Err(_) => {
                log_warn("SESSIONS", "dump-screen: timed out");
                String::new()
            }
        };
        out.push(clip_capture(&text));
    }
    out
}

// ─── Commands ────────────────────────────────────────────────────────────────

fn workspace_connection(state: &AppState, workspace_id: &str) -> Result<Option<Connection>, String> {
    let file = state
        .workspaces
        .lock()
        .map_err(|_| "workspaces lock poisoned".to_string())?;
    file.workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .map(|w| w.connection.clone())
        .ok_or_else(|| "workspace not found".to_string())
}

/// Summarise `names` on the workspace's machine. Returns one row per name in
/// the same order. `Err` only when the call could not be made at all (no
/// live SSH handle, no `claude`, timeout, WSL); a model answer that does not
/// parse is `unknown` rows, not an error.
#[tauri::command]
pub(crate) async fn sessions_overview_summarize(
    state: State<'_, AppState>,
    workspace_id: String,
    names: Vec<String>,
    lang: String,
) -> Result<Vec<SessionSummary>, String> {
    let mut seen = std::collections::HashSet::new();
    let names: Vec<String> = names
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && seen.insert(n.clone()))
        .take(MAX_SESSIONS_PER_CALL)
        .collect();
    if names.is_empty() {
        return Ok(vec![]);
    }
    let lang = if lang.len() == 2 && lang.bytes().all(|b| b.is_ascii_lowercase()) {
        lang
    } else {
        "en".to_string()
    };
    let prompt = summary_prompt(&lang);
    let conn = workspace_connection(&state, &workspace_id)?;
    let started = Instant::now();

    let (backend, raw) = match &conn {
        Some(Connection::Ssh { .. }) => {
            let handle = crate::addons::pick_handle(&state, &workspace_id)
                .ok_or_else(|| "no live SSH session for this workspace".to_string())?;
            let claude = crate::claude_summary::resolve_claude_path(&state, &workspace_id, &handle).await;
            let script = build_ssh_summary_script(&names, &claude, &prompt);
            let raw = crate::addons::exec(&handle, &script, CLAUDE_TIMEOUT_SECS + 10).await?;
            ("tmux/ssh", raw)
        }
        Some(Connection::Wsl { .. }) => {
            return Err("summaries are not available for WSL workspaces".into());
        }
        Some(Connection::Local { .. }) | None => {
            #[cfg(windows)]
            let (backend, captures) = ("zellij", capture_zellij(&names).await);
            #[cfg(not(windows))]
            let (backend, captures) = ("tmux/local", capture_local_tmux(&names).await);
            let input = frame_captures(&captures);
            let raw = run_local_claude(&prompt, input.as_bytes()).await?;
            (backend, raw)
        }
    };

    let rows = parse_summary_envelope(&raw, &names);
    let parsed = rows.iter().filter(|r| r.status != "unknown").count();
    // Rule #1: counts and flags only. `raw` and `rows` are screen-derived.
    let (subtype, is_error) = serde_json::from_str::<serde_json::Value>(raw.trim())
        .map(|v| {
            (
                v.get("subtype").and_then(|s| s.as_str()).unwrap_or("-").to_string(),
                v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false),
            )
        })
        .unwrap_or_else(|_| ("not-json".to_string(), false));
    log_info(
        "SESSIONS",
        &format!(
            "summarize: ws={workspace_id} backend={backend} sessions={} parsed={parsed} \
             reply_bytes={} subtype={subtype} is_error={is_error} took={}ms",
            names.len(),
            raw.len(),
            started.elapsed().as_millis()
        ),
    );
    Ok(rows)
}

/// The pane, if any, currently mounted on `name` for THIS workspace's host.
/// `find_pane_by_tmux_session` matches by name alone, and two hosts can each
/// have a `dev` — so the connection is checked too.
fn pane_holding_session(state: &AppState, workspace_id: &str, conn: Option<&Connection>, name: &str) -> Option<String> {
    let host_is_local = !matches!(conn, Some(Connection::Ssh { .. }));
    let session_id = {
        let sessions = state.core.sessions.lock().ok()?;
        sessions
            .iter()
            .find(|(_, s)| match s {
                Session::Ssh(ss) => {
                    ss.workspace_id == workspace_id && ss.tmux_session.as_deref() == Some(name)
                }
                Session::Local(ls) => host_is_local && ls.tmux_session.as_deref() == Some(name),
            })
            .map(|(sid, _)| sid.clone())?
    };
    let pane_sessions = state.core.pane_sessions.lock().ok()?;
    pane_sessions
        .iter()
        .find(|(_, sid)| sid.as_str() == session_id)
        .map(|(pane_id, _)| pane_id.clone())
}

/// Kill `name` on the workspace's machine. A session one of our panes is
/// attached to goes through `kill_pane_session_inner`, so the pane's PTY and
/// bookkeeping are torn down the tested way; anything else goes straight to
/// `kill_target` and releases the ownership claim on success.
#[tauri::command]
pub(crate) async fn sessions_kill_by_name(
    state: State<'_, AppState>,
    workspace_id: String,
    name: String,
) -> Result<KillSessionOutcome, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("session name cannot be empty".into());
    }
    let conn = workspace_connection(&state, &workspace_id)?;
    if let Some(pane_id) = pane_holding_session(&state, &workspace_id, conn.as_ref(), &name) {
        log_debug(
            "SESSIONS",
            &format!("kill_by_name: ws={workspace_id} routed through pane {pane_id}"),
        );
        return Ok(kill_pane_session_inner(&state, &pane_id).await);
    }
    let target = match &conn {
        Some(Connection::Ssh { .. }) => {
            let handle = crate::addons::pick_handle(&state, &workspace_id)
                .ok_or_else(|| "no live SSH session for this workspace".to_string())?;
            KillTarget::Ssh(handle, name.clone())
        }
        Some(Connection::Wsl { distro }) => KillTarget::Wsl(distro.clone(), name.clone()),
        #[cfg(windows)]
        Some(Connection::Local { .. }) | None => KillTarget::Zellij(name.clone()),
        #[cfg(not(windows))]
        Some(Connection::Local { .. }) | None => KillTarget::LocalUnix(name.clone()),
    };
    let outcome = kill_target(target).await;
    log_info(
        "SESSIONS",
        &format!(
            "kill_by_name: ws={workspace_id} backend={} result={}",
            outcome.backend, outcome.result
        ),
    );
    if matches!(outcome.result.as_str(), "killed" | "already_gone") {
        release_session_owner(&session_owner_host_key(conn.as_ref()), &name);
    }
    Ok(outcome)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prompt_is_cmd_safe_and_one_line() {
        for lang in ["en", "he", "ar", "ru", "zz"] {
            let p = summary_prompt(lang);
            assert!(p.is_ascii(), "{lang}: non-ASCII");
            assert!(!p.contains('"'), "{lang}: double quote");
            assert!(!p.contains('%'), "{lang}: percent");
            assert!(!p.contains('\n') && !p.contains('\r'), "{lang}: newline");
        }
        assert!(summary_prompt("he").contains("in Hebrew"));
        assert!(summary_prompt("zz").contains("in English"));
    }

    #[test]
    fn strip_ansi_drops_csi_and_osc() {
        let s = "\x1b[32mok\x1b[0m \x1b]0;title\x07done\x1b7x";
        assert_eq!(strip_ansi(s), "ok donex");
    }

    #[test]
    fn clip_capture_keeps_the_tail_and_cuts_long_lines() {
        let long = "x".repeat(CAPTURE_LINE_CHARS + 50);
        let mut lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
        lines.push(long.clone());
        lines.push(String::new());
        lines.push("   ".into());
        let raw = lines.join("\n");
        let out = clip_capture(&raw);
        let got: Vec<&str> = out.lines().collect();
        assert_eq!(got.len(), CAPTURE_LINES);
        assert_eq!(got[0], "line 21");
        assert_eq!(got.last().map(|l| l.len()), Some(CAPTURE_LINE_CHARS));
    }

    #[test]
    fn frame_captures_is_one_based_and_ordered() {
        let f = frame_captures(&names(&["a", "b"]));
        assert_eq!(f, "\n### SESSION 1\na\n\n### SESSION 2\nb\n");
    }

    #[test]
    fn ssh_script_quotes_every_name() {
        let s = build_ssh_summary_script(
            &names(&["dev", "it's; rm -rf /"]),
            "/home/u/.local/bin/claude",
            "prompt",
        );
        assert!(s.starts_with("i=0; for s in 'dev' 'it'\\''s; rm -rf /'; do"));
        // `=name:` — a bare `=name` is a PANE target for capture-pane and fails
        // on tmux 3.4 (`can't find pane`). Verified live; do not "simplify".
        assert!(s.contains("tmux capture-pane -p -t \"=$s:\" -S -40 2>/dev/null | cut -c1-240"));
        assert!(s.contains("bash -lc '"));
        assert!(s.contains("--output-format json"));
        assert!(s.ends_with("2>/dev/null"));
        // The raw injection never appears unquoted.
        assert!(!s.contains(" it's; "));
    }

    #[test]
    fn parses_a_clean_envelope() {
        let raw = r#"{"type":"result","subtype":"success","is_error":false,"result":"[{\"i\":1,\"status\":\"idle\",\"summary\":\"Shell prompt\"},{\"i\":2,\"status\":\"WORKING\",\"summary\":\" build running \"}]"}"#;
        let rows = parse_summary_envelope(raw, &names(&["a", "b"]));
        assert_eq!(rows[0].status, "idle");
        assert_eq!(rows[0].summary, "Shell prompt");
        assert_eq!(rows[1].status, "working");
        assert_eq!(rows[1].summary, "build running");
    }

    #[test]
    fn parses_fenced_prose_and_fills_missing_rows() {
        let body = "Here you go:\n```json\n[{\"i\":2,\"status\":\"waiting_input\",\"summary\":\"asks\"}]\n```";
        let raw = serde_json::json!({ "type": "result", "result": body }).to_string();
        let rows = parse_summary_envelope(&raw, &names(&["a", "b", "c"]));
        assert_eq!(rows[0].status, "unknown");
        assert_eq!(rows[0].summary, "");
        assert_eq!(rows[1].status, "waiting_input");
        assert_eq!(rows[1].summary, "asks");
        assert_eq!(rows[2].status, "unknown");
    }

    #[test]
    fn garbage_yields_unknown_for_everyone_not_an_error() {
        let rows = parse_summary_envelope("not json at all", &names(&["a", "b"]));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.status == "unknown" && r.summary.is_empty()));
        let rows = parse_summary_envelope("", &names(&["a"]));
        assert_eq!(rows[0].status, "unknown");
    }

    #[test]
    fn bad_index_and_bad_status_are_tolerated() {
        let body = r#"[{"i":0,"status":"idle"},{"i":9,"status":"idle"},{"i":1,"status":"sleeping","summary":"zzz"},{"status":"idle"}]"#;
        let rows = parse_summary_envelope(body, &names(&["only"]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "unknown");
        assert_eq!(rows[0].summary, "zzz");
    }

    #[test]
    fn bare_array_without_envelope_still_parses() {
        let rows = parse_summary_envelope(r#"[{"i":1,"status":"error","summary":"boom"}]"#, &names(&["a"]));
        assert_eq!(rows[0].status, "error");
        assert_eq!(rows[0].summary, "boom");
    }

    #[test]
    fn error_envelope_is_unknown_rows() {
        let raw = r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#;
        let rows = parse_summary_envelope(raw, &names(&["a"]));
        assert_eq!(rows[0].status, "unknown");
    }
}
