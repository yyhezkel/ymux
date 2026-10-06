//! Phase 78 — Claude subscription-usage fetch.
//!
//! `claude -p "/usage" --output-format json` returns the user's REAL Pro/Max
//! subscription quota (session %, weekly %, per-model %, reset times, and a
//! "what's contributing" breakdown) in the JSON envelope's `result` string.
//! The call is FREE (`total_cost_usd: 0`, `num_turns: 0`) but ~8 s latency
//! (a real round-trip), so we cache per-workspace for 5 min and only fetch
//! on demand / on a slow auto-refresh — never fast-poll.
//!
//! Rule #1: we log only the workspace id + percentages, never the `/usage`
//! body (it names the user's subagents / skills / MCP servers).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::State;

use crate::AppState;

/// One model's weekly usage row (e.g. `Current week (Fable): 16% used …`).
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ModelUsage {
    pub name: String,
    pub pct: u8,
    pub reset: String,
}

/// Parsed `/usage` snapshot for one workspace's Claude account.
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ClaudeUsage {
    pub session_pct: u8,
    pub session_reset: String,
    pub week_pct: u8,
    pub week_reset: String,
    /// Per-model weekly rows (excludes the "all models" aggregate).
    pub models: Vec<ModelUsage>,
    /// Raw "Last 24h" contributing lines (header + indented bullets).
    pub contributing_24h: Vec<String>,
    /// Raw "Last 7d" contributing lines.
    pub contributing_7d: Vec<String>,
    pub fetched_unix: i64,
}

// ─── cache (per workspace, 5-min TTL) — mirrors updater.rs ───────────────────
static USAGE_CACHE: Mutex<Option<HashMap<String, (Instant, ClaudeUsage)>>> = Mutex::new(None);
const USAGE_CACHE_TTL: Duration = Duration::from_secs(300);

// ─── beta.3 safety nets (backoff + in-flight dedupe) ────────────────────────
//
// A misbehaving caller (frontend effect loop, third-party addon polling
// too hard, etc.) must NOT be able to open a new remote `claude -p
// /usage` every few seconds — each failed exec that hits the SSH
// timeout leaves an orphan `claude` process on the server and eventually
// exhausts sshd's MaxSessions, killing the whole session. Two defenses:
//
//   1. `USAGE_FAILED_AT` — after any failed fetch (parse error, exec
//      timeout, connection drop) we refuse further fetches for the same
//      workspace for `USAGE_BACKOFF`, serving stale cache if we have it.
//   2. `USAGE_INFLIGHT`  — at most one in-flight fetch per workspace;
//      concurrent callers fall through to stale cache or an error
//      without spawning a second exec channel. Guard is RAII (see
//      `InFlight`) so panics/aborts still release it.
static USAGE_FAILED_AT: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
const USAGE_BACKOFF: Duration = Duration::from_secs(300);
static USAGE_INFLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Try to claim the sole "fetch in flight" slot for `workspace_id`.
/// Returns `None` if another fetch is already running; the caller
/// should fall back to stale cache. The returned guard clears the
/// slot on drop.
struct InFlight(String);
impl InFlight {
    fn try_acquire(workspace_id: &str) -> Option<Self> {
        let mut guard = USAGE_INFLIGHT.lock().ok()?;
        let set = guard.get_or_insert_with(HashSet::new);
        if set.contains(workspace_id) {
            return None;
        }
        set.insert(workspace_id.to_string());
        Some(InFlight(workspace_id.to_string()))
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        if let Ok(mut guard) = USAGE_INFLIGHT.lock() {
            if let Some(set) = guard.as_mut() {
                set.remove(&self.0);
            }
        }
    }
}

fn in_backoff(workspace_id: &str) -> bool {
    let Ok(guard) = USAGE_FAILED_AT.lock() else { return false; };
    let Some(map) = guard.as_ref() else { return false; };
    match map.get(workspace_id) {
        Some(at) => at.elapsed() < USAGE_BACKOFF,
        None => false,
    }
}
/// Enter backoff for `workspace_id`. Logs once here (with the unix
/// instant it ends); the rejection path in `claude_usage_fetch` stays
/// silent so a tight caller loop cannot flood the log.
fn mark_failed(workspace_id: &str) {
    if let Ok(mut guard) = USAGE_FAILED_AT.lock() {
        guard.get_or_insert_with(HashMap::new)
            .insert(workspace_id.to_string(), Instant::now());
        let until = now_unix() + USAGE_BACKOFF.as_secs() as i64;
        crate::log_debug(
            "USAGE",
            &format!("workspace={workspace_id} backoff until {until}"),
        );
    }
}
fn clear_failed(workspace_id: &str) {
    if let Ok(mut guard) = USAGE_FAILED_AT.lock() {
        if let Some(map) = guard.as_mut() {
            map.remove(workspace_id);
        }
    }
}

fn cache_fresh(workspace_id: &str, force: bool) -> Option<ClaudeUsage> {
    if force {
        return None;
    }
    let guard = USAGE_CACHE.lock().ok()?;
    let (at, usage) = guard.as_ref()?.get(workspace_id)?;
    if at.elapsed() < USAGE_CACHE_TTL {
        Some(usage.clone())
    } else {
        None
    }
}

fn cache_stale(workspace_id: &str) -> Option<ClaudeUsage> {
    let guard = USAGE_CACHE.lock().ok()?;
    guard.as_ref()?.get(workspace_id).map(|(_, u)| u.clone())
}

fn cache_store(workspace_id: &str, usage: &ClaudeUsage) {
    if let Ok(mut guard) = USAGE_CACHE.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(workspace_id.to_string(), (Instant::now(), usage.clone()));
    }
}

// ─── parsing ─────────────────────────────────────────────────────────────────

/// `"33% used · resets Jul 8, 4:10am (Europe/Berlin)"` → `(33, "Jul 8, 4:10am (Europe/Berlin)")`.
fn pct_and_reset(rest: &str) -> (u8, String) {
    let pct = rest
        .split('%')
        .next()
        .unwrap_or("")
        .trim()
        .parse::<u8>()
        .unwrap_or(0);
    let reset = rest
        .split("resets ")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    (pct, reset)
}

/// Extract the `result` text from the JSON envelope, then line-scan it.
fn parse_usage(raw: &str, now_unix: i64) -> Result<ClaudeUsage, String> {
    let text = serde_json::from_str::<serde_json::Value>(raw.trim())
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
        .ok_or("could not parse /usage output (claude installed & authenticated?)")?;

    let mut session_pct = 0u8;
    let mut session_reset = String::new();
    let mut week_pct = 0u8;
    let mut week_reset = String::new();
    let mut models: Vec<ModelUsage> = Vec::new();
    let mut c24: Vec<String> = Vec::new();
    let mut c7: Vec<String> = Vec::new();
    let mut found_session = false;

    enum Mode {
        None,
        Day,
        Week,
    }
    let mut mode = Mode::None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Current session:") {
            let (p, r) = pct_and_reset(rest);
            session_pct = p;
            session_reset = r;
            found_session = true;
            mode = Mode::None;
        } else if let Some(rest) = trimmed.strip_prefix("Current week (all models):") {
            let (p, r) = pct_and_reset(rest);
            week_pct = p;
            week_reset = r;
            mode = Mode::None;
        } else if trimmed.starts_with("Current week (") {
            // Per-model: "Current week (<Name>): NN% used · resets …"
            if let Some(name) = trimmed
                .split_once('(')
                .and_then(|(_, r)| r.split_once("):"))
                .map(|(name, _)| name.to_string())
            {
                if let Some((_, rest)) = trimmed.split_once("): ") {
                    let (p, r) = pct_and_reset(rest);
                    models.push(ModelUsage { name, pct: p, reset: r });
                }
            }
            mode = Mode::None;
        } else if trimmed.starts_with("Last 24h") {
            mode = Mode::Day;
            c24.push(trimmed.to_string());
        } else if trimmed.starts_with("Last 7d") {
            mode = Mode::Week;
            c7.push(trimmed.to_string());
        } else if trimmed.is_empty() {
            mode = Mode::None;
        } else if line.starts_with("  ") {
            // Indented bullet under the active "Last …" header.
            match mode {
                Mode::Day => c24.push(trimmed.to_string()),
                Mode::Week => c7.push(trimmed.to_string()),
                Mode::None => {}
            }
        } else {
            mode = Mode::None;
        }
    }

    if !found_session {
        return Err("unexpected /usage format — no session line".into());
    }
    Ok(ClaudeUsage {
        session_pct,
        session_reset,
        week_pct,
        week_reset,
        models,
        contributing_24h: c24,
        contributing_7d: c7,
        fetched_unix: now_unix,
    })
}

// ─── command ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn claude_usage_fetch(
    state: State<'_, AppState>,
    workspace_id: String,
    force: bool,
) -> Result<ClaudeUsage, String> {
    if let Some(cached) = cache_fresh(&workspace_id, force) {
        return Ok(cached);
    }
    // beta.3 safety net #1 — refuse to spam remote after a recent failure.
    if in_backoff(&workspace_id) {
        return cache_stale(&workspace_id)
            .ok_or_else(|| "usage temporarily unavailable (backoff)".to_string());
    }
    // beta.3 safety net #2 — at most one in-flight fetch per workspace.
    let _guard = match InFlight::try_acquire(&workspace_id) {
        Some(g) => g,
        None => {
            crate::log_debug("USAGE", &format!("workspace={workspace_id} inflight"));
            return cache_stale(&workspace_id)
                .ok_or_else(|| "usage fetch already in progress".to_string());
        }
    };
    let out = match run_usage_probe(&state, &workspace_id).await {
        Ok(s) => s,
        Err(e) => {
            mark_failed(&workspace_id);
            crate::log_warn("USAGE", &format!("workspace={workspace_id} exec_failed"));
            return cache_stale(&workspace_id).ok_or(e);
        }
    };
    let now = now_unix();
    match parse_usage(&out, now) {
        Ok(usage) => {
            crate::log_debug(
                "USAGE",
                &format!(
                    "workspace={workspace_id} session={}% week={}%",
                    usage.session_pct, usage.week_pct
                ),
            );
            cache_store(&workspace_id, &usage);
            clear_failed(&workspace_id);
            Ok(usage)
        }
        // Serve stale on a transient parse/fetch miss before surfacing the error.
        Err(e) => {
            mark_failed(&workspace_id);
            crate::log_warn("USAGE", &format!("workspace={workspace_id} unavailable"));
            cache_stale(&workspace_id).ok_or(e)
        }
    }
}

/// `claude -p "/usage" --output-format json` on the host the WORKSPACE
/// lives on — the account shown must be the one this workspace's panes
/// actually use: the remote's for SSH, the distro's for WSL, and the local
/// login for a local workspace. Before this, a local workspace had no
/// probe at all, so the ticker kept showing whichever SSH account was
/// fetched last.
async fn run_usage_probe(state: &AppState, workspace_id: &str) -> Result<String, String> {
    const REMOTE_SCRIPT: &str =
        "bash -lc 'timeout 18 claude -p \"/usage\" --output-format json 2>/dev/null'";
    if let Some(handle) = crate::addons::pick_handle(state, workspace_id) {
        // Login shell so `claude` is on PATH in a non-interactive session;
        // stderr dropped so `result` stays clean JSON. `timeout 18` on the
        // REMOTE side guarantees the `claude` process dies even if our local
        // exec timeout fires and we drop the channel — no orphans left.
        return crate::addons::exec(&handle, REMOTE_SCRIPT, 20).await;
    }
    let conn = {
        let file = state.workspaces.lock().map_err(|_| "workspaces lock poisoned".to_string())?;
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| w.connection.clone())
    };
    match conn {
        Some(ymux_types::Connection::Ssh { .. }) => {
            Err("no active SSH session for this workspace".into())
        }
        Some(ymux_types::Connection::Wsl { distro }) => {
            // Same script, run inside the distro (its own claude login).
            let (_code, text) = crate::local_setup::wsl_exec(
                distro.as_deref(),
                None,
                "timeout 18 claude -p \"/usage\" --output-format json 2>/dev/null",
            )
            .await?;
            Ok(text)
        }
        Some(ymux_types::Connection::Local { .. }) | None => run_local_usage_probe().await,
    }
}

/// Local `claude -p /usage`. argv array (Rule #3), hidden window on
/// Windows, wall-clock timeout with kill_on_drop so a hung CLI can't pile
/// up (the local twin of the remote `timeout 18`).
async fn run_local_usage_probe() -> Result<String, String> {
    let claude = crate::local_setup::resolve_claude_binary()
        .ok_or_else(|| "claude is not installed on this machine".to_string())?;
    let mut c = crate::local_setup::hidden_cmd(&claude.to_string_lossy());
    c.args(["-p", "/usage", "--output-format", "json"]);
    c.kill_on_drop(true);
    #[cfg(not(target_os = "windows"))]
    c.env("PATH", crate::local_setup::unix_path_env());
    let out = tokio::time::timeout(Duration::from_secs(20), c.output())
        .await
        .map_err(|_| "local claude /usage timed out".to_string())?
        .map_err(|e| format!("spawn {}: {e}", claude.display()))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"You are currently using your subscription to power your Claude Code usage\n\nCurrent session: 33% used · resets Jul 8, 4:10am (Europe/Berlin)\nCurrent week (all models): 11% used · resets Jul 14, 10pm (Europe/Berlin)\nCurrent week (Fable): 16% used · resets Jul 14, 10pm (Europe/Berlin)\n\nWhat's contributing to your limits usage?\nApproximate, based on local sessions on this machine.\n\nLast 24h · 3466 requests · 10 sessions\n  94% of your usage came from subagent-heavy sessions\n  Top subagents: implementer 40%, loop 8%\n\nLast 7d · 13897 requests · 26 sessions\n  99% of your usage came from subagent-heavy sessions","total_cost_usd":0}"#;

    // pins: entering backoff flips in_backoff, clear_failed lifts it, and a
    // poisoned lock degrades to "not in backoff" without panicking. One test
    // because poisoning the static is permanent for the process.
    #[test]
    fn backoff_enter_clear_and_poisoned_lock() {
        mark_failed("t-backoff-a");
        assert!(in_backoff("t-backoff-a"));
        assert!(!in_backoff("t-backoff-other"));
        clear_failed("t-backoff-a");
        assert!(!in_backoff("t-backoff-a"));
        let _ = std::thread::spawn(|| {
            let _g = USAGE_FAILED_AT.lock();
            panic!("poison");
        })
        .join();
        mark_failed("t-backoff-b");
        assert!(!in_backoff("t-backoff-b"));
    }

    #[test]
    fn parses_real_usage() {
        let u = parse_usage(SAMPLE, 1_700_000_000).expect("should parse");
        assert_eq!(u.session_pct, 33);
        assert_eq!(u.session_reset, "Jul 8, 4:10am (Europe/Berlin)");
        assert_eq!(u.week_pct, 11);
        assert_eq!(u.models.len(), 1);
        assert_eq!(u.models[0].name, "Fable");
        assert_eq!(u.models[0].pct, 16);
        assert_eq!(u.contributing_24h.len(), 3); // header + 2 bullets
        assert_eq!(u.contributing_7d.len(), 2); // header + 1 bullet
        assert_eq!(u.fetched_unix, 1_700_000_000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_usage("not json", 0).is_err());
        assert!(parse_usage(r#"{"result":"hello world"}"#, 0).is_err());
    }
}
