//! Phase 103 — per-Claude-session context: the session's first prompt plus
//! a running "where we stand" log, persisted to disk. The Context Rail
//! renders it; Phase 103.C injects it back into the agent on
//! compact/resume (see docs/CONTEXT.md).
//!
//! Source: the existing `[ymux-brief]` (no LLM, zero tokens). Every Stop
//! appends a `Turn` built from the parsed brief — a degraded brief still
//! appends a (degraded) line — and SessionEnd appends `Closed`.
//!
//! Persistence: `<config_dir>/context/sessions/<session_id>.json`, atomic
//! tmp + rename (Rule #7). A file that fails to parse is NEVER overwritten:
//! that session id is poisoned for the process lifetime and mutations on it
//! are refused, the `notes.rs` poison-gate idea at per-file granularity.
//! Files untouched for 30 days are pruned at startup.
//!
//! This reverses BRIEF's 2026-09-01 "briefs live in memory only" for the
//! per-session log (docs/DECISIONS.md, 2026-10-05). The in-memory
//! `AppState.briefs` map that drives the Queue is unchanged.
//!
//! Rule #1: prompts and brief text are user/agent content. They live in
//! these files and the UI only; log lines carry session/pane ids, counts
//! and lengths.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::brief::{clip_chars, BriefStatus, PaneBrief};
use crate::{config_dir_pub, log_debug, log_info, log_warn, AppState};

/// The session's first prompt, as stored.
pub(crate) const FIRST_PROMPT_MAX_CHARS: usize = 2000;
/// Log entries kept per session; the oldest drop first.
pub(crate) const LOG_MAX_ENTRIES: usize = 200;
/// Session files untouched for this long are deleted at startup.
pub(crate) const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LogKind {
    /// One agent turn ended (Stop).
    Turn,
    /// The session ended (SessionEnd).
    Closed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LogEntry {
    pub(crate) ts_ms: u64,
    pub(crate) kind: LogKind,
    pub(crate) status: BriefStatus,
    #[serde(default)]
    pub(crate) task: Option<String>,
    #[serde(default)]
    pub(crate) delta: Option<String>,
    #[serde(default)]
    pub(crate) next: Option<String>,
    #[serde(default)]
    pub(crate) ask: Option<String>,
    #[serde(default)]
    pub(crate) rec: Option<String>,
    #[serde(default)]
    pub(crate) degraded: bool,
}

/// One Claude session's context — also the on-disk file shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SessionContext {
    #[serde(default = "schema_v1")]
    pub(crate) schema: u32,
    pub(crate) session_id: String,
    /// The screen workspace that holds the pane (latest seen).
    #[serde(default)]
    pub(crate) ws_id: Option<String>,
    #[serde(default)]
    pub(crate) pane_id: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) first_prompt: Option<String>,
    #[serde(default)]
    pub(crate) first_prompt_ms: Option<u64>,
    #[serde(default)]
    pub(crate) log: Vec<LogEntry>,
    /// Bumped on every mutation.
    #[serde(default)]
    pub(crate) version: u64,
}

fn schema_v1() -> u32 {
    SCHEMA_VERSION
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every mutation is clone → apply → persist → swap, so a poisoned lock
    // still guards consistent data; recover instead of panicking (Rule #4).
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A session id becomes a filename, so it must be one safe path component:
/// `[A-Za-z0-9_-]`, 1..=128 chars. Claude Code's ids are UUIDs.
pub(crate) fn validate_session_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("invalid session id".into());
    }
    Ok(())
}

fn opt_clip(s: Option<&str>, max: usize) -> Option<String> {
    s.map(|v| clip_chars(v, max)).filter(|v| !v.is_empty())
}

// ─── The pure model ─────────────────────────────────────────────────────────

impl SessionContext {
    fn new(session_id: &str) -> Self {
        SessionContext {
            schema: SCHEMA_VERSION,
            session_id: session_id.to_string(),
            ..Default::default()
        }
    }

    /// Latest activity — the rail's sort key.
    pub(crate) fn last_activity_ms(&self) -> u64 {
        self.log
            .last()
            .map(|e| e.ts_ms)
            .unwrap_or(0)
            .max(self.first_prompt_ms.unwrap_or(0))
    }

    /// Where the session lives now. `None` never erases a known value.
    /// Returns whether anything changed.
    pub(crate) fn touch(&mut self, ws_id: Option<&str>, pane_id: Option<&str>, cwd: Option<&str>) -> bool {
        let before = (self.ws_id.clone(), self.pane_id.clone(), self.cwd.clone());
        if let Some(w) = ws_id.filter(|s| !s.is_empty()) {
            self.ws_id = Some(w.to_string());
        }
        if let Some(p) = pane_id.filter(|s| !s.is_empty()) {
            self.pane_id = Some(p.to_string());
        }
        if let Some(c) = cwd.filter(|s| !s.is_empty()) {
            self.cwd = Some(clip_chars(c, 512));
        }
        before != (self.ws_id.clone(), self.pane_id.clone(), self.cwd.clone())
    }

    /// Set the first prompt — only once. Returns whether it was set.
    pub(crate) fn set_first_prompt(&mut self, prompt: &str, now: u64) -> bool {
        if self.first_prompt.is_some() {
            return false;
        }
        let p = clip_chars(prompt, FIRST_PROMPT_MAX_CHARS);
        if p.is_empty() {
            return false;
        }
        self.first_prompt = Some(p);
        self.first_prompt_ms = Some(now);
        true
    }

    fn push(&mut self, e: LogEntry) {
        self.log.push(e);
        if self.log.len() > LOG_MAX_ENTRIES {
            let drop = self.log.len() - LOG_MAX_ENTRIES;
            self.log.drain(..drop);
        }
    }

    /// A Stop: one Turn line from the brief (degraded included). The brief
    /// fields are already clipped by `brief.rs`.
    pub(crate) fn append_turn(&mut self, b: &PaneBrief, now: u64) {
        self.push(LogEntry {
            ts_ms: now,
            kind: LogKind::Turn,
            status: b.status,
            task: b.task.clone(),
            delta: b.delta.clone(),
            next: b.next.clone(),
            ask: b.ask.clone(),
            rec: b.rec.clone(),
            degraded: b.degraded,
        });
    }

    /// SessionEnd. `reason` is Claude Code's fixed enum (`clear`, `logout`,
    /// `prompt_input_exit`, …), kept as the line's delta.
    pub(crate) fn append_closed(&mut self, reason: Option<&str>, now: u64) {
        let task = self.log.iter().rev().find_map(|e| e.task.clone());
        self.push(LogEntry {
            ts_ms: now,
            kind: LogKind::Closed,
            status: BriefStatus::Done,
            task,
            delta: opt_clip(reason, 80),
            next: None,
            ask: None,
            rec: None,
            degraded: false,
        });
    }
}

// ─── Persistence ────────────────────────────────────────────────────────────

pub(crate) fn sessions_dir() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("context").join("sessions"))
}

fn file_path(dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(session_id)?;
    Ok(dir.join(format!("{session_id}.json")))
}

pub(crate) fn load_from(dir: &Path, session_id: &str) -> Result<Option<SessionContext>, String> {
    let path = file_path(dir, session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
    let ctx: SessionContext = serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse: {e}"))?;
    if ctx.session_id != session_id {
        return Err("session id mismatch".into());
    }
    Ok(Some(ctx))
}

pub(crate) fn save_to(dir: &Path, ctx: &SessionContext) -> Result<(), String> {
    use std::io::Write as _;
    let path = file_path(dir, &ctx.session_id)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
    let tmp = dir.join(format!("{}.{}.tmp", ctx.session_id, std::process::id()));
    let text = serde_json::to_string_pretty(ctx).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("open tmp: {e}"))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Delete `*.json` / stale `*.tmp` files in `dir` whose mtime is older than
/// `max_age` relative to `now`. Returns how many were removed.
pub(crate) fn prune_dir(dir: &Path, max_age: Duration, now: SystemTime) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for ent in rd.flatten() {
        let p = ent.path();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "json" && ext != "tmp" {
            continue;
        }
        let Ok(mtime) = ent.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if now.duration_since(mtime).map(|age| age > max_age).unwrap_or(false)
            && std::fs::remove_file(&p).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Managed on `AppState.context`: every session file loaded so far, plus
/// the ids whose file could not be parsed (never written over).
#[derive(Clone, Default)]
pub(crate) struct ContextState {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    loaded: bool,
    sessions: HashMap<String, SessionContext>,
    poisoned: HashSet<String>,
}

impl ContextState {
    /// Load every session file in `dir` once. Unparsable files are poisoned.
    fn ensure_loaded(inner: &mut Inner, dir: &Path) {
        if inner.loaded {
            return;
        }
        inner.loaded = true;
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            match load_from(dir, &id) {
                Ok(Some(ctx)) => {
                    inner.sessions.insert(id, ctx);
                }
                Ok(None) => {}
                Err(e) => {
                    log_warn("CONTEXT", &format!("session file {id} unreadable ({e}); left untouched"));
                    inner.poisoned.insert(id);
                }
            }
        }
    }

    /// Prune old files, then load the rest. Called once from setup.
    pub(crate) fn startup_at(&self, dir: &Path, now: SystemTime) -> (usize, usize) {
        let pruned = prune_dir(dir, RETENTION, now);
        let mut inner = lock(&self.inner);
        Self::ensure_loaded(&mut inner, dir);
        (pruned, inner.sessions.len())
    }

    /// Clone → apply → bump `version` → persist → swap. A failed apply or
    /// save leaves memory and disk unchanged. `f` returns false for a
    /// no-op, in which case nothing is written. Returns the result snapshot
    /// when something changed.
    pub(crate) fn mutate_at(
        &self,
        dir: &Path,
        session_id: &str,
        f: impl FnOnce(&mut SessionContext) -> bool,
    ) -> Result<Option<SessionContext>, String> {
        validate_session_id(session_id)?;
        let mut inner = lock(&self.inner);
        Self::ensure_loaded(&mut inner, dir);
        if inner.poisoned.contains(session_id) {
            return Err(format!("session {session_id}: file unreadable, not overwriting"));
        }
        let mut ctx = inner
            .sessions
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| SessionContext::new(session_id));
        if !f(&mut ctx) {
            return Ok(None);
        }
        ctx.version = ctx.version.saturating_add(1);
        save_to(dir, &ctx)?;
        inner.sessions.insert(session_id.to_string(), ctx.clone());
        Ok(Some(ctx))
    }

    pub(crate) fn get_at(&self, dir: &Path, session_id: &str) -> Result<Option<SessionContext>, String> {
        validate_session_id(session_id)?;
        let mut inner = lock(&self.inner);
        Self::ensure_loaded(&mut inner, dir);
        Ok(inner.sessions.get(session_id).cloned())
    }

    /// Sessions of one workspace, most recent activity first.
    pub(crate) fn list_at(&self, dir: &Path, ws_id: &str) -> Vec<SessionContext> {
        let mut inner = lock(&self.inner);
        Self::ensure_loaded(&mut inner, dir);
        let mut out: Vec<SessionContext> = inner
            .sessions
            .values()
            .filter(|s| s.ws_id.as_deref() == Some(ws_id))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.last_activity_ms().cmp(&a.last_activity_ms()));
        out
    }
}

// ─── Hook wiring (called from rpc_server's feed.push arms) ──────────────────

/// Where a hook came from — everything is optional because older CLIs and
/// non-Claude agents send less.
pub(crate) struct HookOrigin<'a> {
    pub(crate) session_id: Option<&'a str>,
    pub(crate) ws_id: Option<&'a str>,
    pub(crate) pane_id: Option<&'a str>,
    pub(crate) cwd: Option<&'a str>,
}

/// What happened, per hook subkind.
pub(crate) enum HookEvent<'a> {
    Prompt(&'a str),
    Stop(&'a PaneBrief),
    End(Option<&'a str>),
}

/// Apply one hook to its session's record and emit `context:changed`.
/// Best-effort: a failure is logged (ids + error only) and never affects
/// the hook's own handling.
pub(crate) fn on_hook(state: &AppState, app: &AppHandle, origin: HookOrigin<'_>, ev: HookEvent<'_>) {
    let Some(sid) = origin.session_id.filter(|s| !s.is_empty()) else {
        return;
    };
    let dir = match sessions_dir() {
        Ok(d) => d,
        Err(e) => {
            log_warn("CONTEXT", &format!("no config dir: {e}"));
            return;
        }
    };
    let now = now_ms();
    let res = state.context.mutate_at(&dir, sid, |c| {
        let moved = c.touch(origin.ws_id, origin.pane_id, origin.cwd);
        match ev {
            // Every prompt arrives here; only the first one (or a pane
            // move) is worth a write.
            HookEvent::Prompt(p) => c.set_first_prompt(p, now) || moved,
            HookEvent::Stop(b) => {
                c.append_turn(b, now);
                true
            }
            HookEvent::End(reason) => {
                c.append_closed(reason, now);
                true
            }
        }
    });
    match res {
        Ok(Some(ctx)) => {
            log_debug(
                "CONTEXT",
                &format!(
                    "session={sid} pane={} log={} v={}",
                    origin.pane_id.unwrap_or("-"),
                    ctx.log.len(),
                    ctx.version
                ),
            );
            let _ = app.emit(
                "context:changed",
                serde_json::json!({ "session_id": sid, "ws_id": ctx.ws_id }),
            );
        }
        Ok(None) => {}
        Err(e) => log_warn("CONTEXT", &format!("session={sid} update failed: {e}")),
    }
}

/// Setup: prune month-old files and warm the cache, off the main thread.
pub(crate) fn startup(state: &AppState) {
    let st = state.context.clone();
    std::thread::spawn(move || match sessions_dir() {
        Ok(dir) => {
            let (pruned, loaded) = st.startup_at(&dir, SystemTime::now());
            log_info("CONTEXT", &format!("startup: pruned={pruned} loaded={loaded}"));
        }
        Err(e) => log_warn("CONTEXT", &format!("startup: no config dir: {e}")),
    });
}

// ─── Phase 103.C: context injection (SessionStart) ─────────────────────────

/// Hard cap on what is handed back to the agent, in BYTES (UTF-8).
pub(crate) const INJECT_MAX_BYTES: usize = 1536;
/// Log lines included on compact / resume.
pub(crate) const INJECT_LOG_LINES: usize = 8;
/// Sibling sessions listed on startup.
const INJECT_MAX_SIBLINGS: usize = 8;
/// The first prompt is clipped harder here than in storage, so the log
/// still fits under the byte cap.
const INJECT_PROMPT_CHARS: usize = 500;
pub(crate) const INJECT_HEADER: &str = "[ymux-context]";

/// One line, no control characters — what an agent reads must not carry
/// stray escapes or fake structure from a multi-line field.
fn flat(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c };
        if !c.is_control() {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn status_word(s: BriefStatus) -> &'static str {
    match s {
        BriefStatus::Working => "working",
        BriefStatus::WaitingForYou => "waiting-for-you",
        BriefStatus::Stuck => "stuck",
        BriefStatus::Done => "done",
    }
}

fn log_line(e: &LogEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if e.kind == LogKind::Closed {
        parts.push("session closed".into());
    } else {
        parts.push(format!("[{}]", status_word(e.status)));
    }
    if let Some(t) = e.task.as_deref().filter(|t| !t.is_empty()) {
        parts.push(flat(t));
    }
    if let Some(d) = e.delta.as_deref().filter(|d| !d.is_empty()) {
        parts.push(format!("— {}", flat(d)));
    }
    if let Some(n) = e.next.as_deref().filter(|n| !n.is_empty()) {
        parts.push(format!("→ next: {}", flat(n)));
    }
    if let Some(a) = e.ask.as_deref().filter(|a| !a.is_empty()) {
        match e.rec.as_deref().filter(|r| !r.is_empty()) {
            Some(r) => parts.push(format!("(asked: {} · rec: {})", flat(a), flat(r))),
            None => parts.push(format!("(asked: {})", flat(a))),
        }
    }
    format!("- {}", parts.join(" "))
}

/// Byte-cap on a char boundary.
fn clip_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let ell = "…";
    let mut end = max.saturating_sub(ell.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ell}", &s[..end])
}

/// Join a fixed head with as many `items` as fit under the cap, dropping
/// from the FRONT (the oldest) first; a still-too-long result is clipped.
fn assemble(head: &[String], items: &[String]) -> String {
    let mut start = 0;
    loop {
        let mut lines: Vec<&str> = vec![INJECT_HEADER];
        lines.extend(head.iter().map(String::as_str));
        lines.extend(items[start..].iter().map(String::as_str));
        let text = lines.join("\n");
        if text.len() <= INJECT_MAX_BYTES || start >= items.len() {
            return clip_bytes(&text, INJECT_MAX_BYTES);
        }
        start += 1;
    }
}

/// The `additionalContext` for a SessionStart hook. Pure: the caller
/// resolves the session, the workspace intent and the siblings.
///
/// - `compact` / `resume`: this session's first prompt + its last
///   `INJECT_LOG_LINES` log lines (oldest → newest).
/// - `startup`: the workspace intent + one line per OTHER open session in
///   the same workspace (task + status), most recent first.
/// - anything else (`clear`, an unknown future value): nothing.
///
/// Returns "" when there is nothing worth saying — the hook then prints
/// nothing at all.
pub(crate) fn build_injection(
    source: &str,
    this: Option<&SessionContext>,
    intent: Option<&str>,
    siblings: &[SessionContext],
) -> String {
    match source {
        "compact" | "resume" => {
            let Some(c) = this else { return String::new() };
            let mut head = Vec::new();
            if let Some(p) = c.first_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
                head.push(format!(
                    "This session's first prompt: {}",
                    flat(&clip_chars(p, INJECT_PROMPT_CHARS))
                ));
            }
            let n = c.log.len();
            let tail = &c.log[n.saturating_sub(INJECT_LOG_LINES)..];
            let items: Vec<String> = tail.iter().map(log_line).collect();
            if head.is_empty() && items.is_empty() {
                return String::new();
            }
            if !items.is_empty() {
                head.push("Where this session stands (oldest → newest):".into());
            }
            assemble(&head, &items)
        }
        "startup" => {
            let mut head = Vec::new();
            if let Some(i) = intent.map(flat).filter(|i| !i.is_empty()) {
                head.push(format!("Workspace goal: {i}"));
            }
            let mut sibs: Vec<&SessionContext> = siblings
                .iter()
                .filter(|s| this.map_or(true, |t| t.session_id != s.session_id))
                .filter(|s| s.log.last().map_or(false, |e| e.kind == LogKind::Turn))
                .collect();
            sibs.sort_by(|a, b| b.last_activity_ms().cmp(&a.last_activity_ms()));
            let items: Vec<String> = sibs
                .iter()
                .take(INJECT_MAX_SIBLINGS)
                .filter_map(|s| {
                    let last = s.log.last()?;
                    let task = s
                        .log
                        .iter()
                        .rev()
                        .find_map(|e| e.task.as_deref())
                        .map(flat)
                        .unwrap_or_else(|| "(untitled session)".into());
                    Some(format!("- {task} — {}", status_word(last.status)))
                })
                .collect();
            if head.is_empty() && items.is_empty() {
                return String::new();
            }
            if !items.is_empty() {
                head.push("Other agent sessions in this workspace:".into());
            }
            assemble(&head, &items)
        }
        _ => String::new(),
    }
}

/// RPC `context.inject` body: resolve the pane's workspace + this session,
/// build the text. Returns "" when injection is off or there is nothing.
/// Rule #1: logs carry ids, the source and the byte count — never text.
pub(crate) fn injection_for_hook(
    state: &AppState,
    pane_id: Option<&str>,
    session_id: Option<&str>,
    source: &str,
) -> String {
    let enabled = lock(&state.settings).brief.inject_context;
    if !enabled {
        return String::new();
    }
    let Ok(dir) = sessions_dir() else {
        return String::new();
    };
    let this = session_id
        .filter(|s| !s.is_empty())
        .and_then(|s| state.context.get_at(&dir, s).ok().flatten());
    let (ws_id, intent) = {
        let file = lock(&state.workspaces);
        let ws_id = pane_id
            .and_then(|p| crate::find_workspace_for_pane(&file, p))
            .or_else(|| this.as_ref().and_then(|t| t.ws_id.clone()));
        let intent = ws_id.as_deref().and_then(|w| {
            file.workspaces
                .iter()
                .find(|x| x.id == w)
                .and_then(|x| x.intent.clone())
        });
        (ws_id, intent)
    };
    let siblings = match (source, ws_id.as_deref()) {
        ("startup", Some(w)) => state.context.list_at(&dir, w),
        _ => Vec::new(),
    };
    let text = build_injection(source, this.as_ref(), intent.as_deref(), &siblings);
    log_debug(
        "CONTEXT",
        &format!(
            "inject source={source} session={} pane={} bytes={}",
            session_id.unwrap_or("-"),
            pane_id.unwrap_or("-"),
            text.len()
        ),
    );
    text
}

// ─── Tauri commands ─────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn session_context_list(
    state: State<'_, AppState>,
    ws_id: String,
) -> Result<Vec<SessionContext>, String> {
    let dir = sessions_dir()?;
    Ok(state.context.list_at(&dir, &ws_id))
}

#[tauri::command]
pub(crate) fn session_context_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Option<SessionContext>, String> {
    let dir = sessions_dir()?;
    state.context.get_at(&dir, &session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief(task: &str, delta: &str, degraded: bool) -> PaneBrief {
        PaneBrief {
            task: Some(task.into()),
            status: BriefStatus::Working,
            ask: None,
            rec: None,
            next: Some("next".into()),
            delta: Some(delta.into()),
            degraded,
            updated_ms: 0,
        }
    }

    #[test]
    fn first_prompt_is_set_once() {
        let mut c = SessionContext::new("s1");
        assert!(!c.set_first_prompt("   ", 1), "blank is ignored");
        assert!(c.set_first_prompt("build the installer", 2));
        assert!(!c.set_first_prompt("something else", 3));
        assert_eq!(c.first_prompt.as_deref(), Some("build the installer"));
        assert_eq!(c.first_prompt_ms, Some(2));
        let mut long = SessionContext::new("s2");
        long.set_first_prompt(&"א".repeat(5000), 1);
        assert_eq!(
            long.first_prompt.as_deref().map(|p| p.chars().count()),
            Some(FIRST_PROMPT_MAX_CHARS + 1),
            "clipped + ellipsis"
        );
    }

    #[test]
    fn log_is_capped_oldest_first() {
        let mut c = SessionContext::new("s1");
        for i in 0..(LOG_MAX_ENTRIES + 25) {
            c.append_turn(&brief("t", &format!("d{i}"), false), i as u64);
        }
        assert_eq!(c.log.len(), LOG_MAX_ENTRIES);
        assert_eq!(c.log[0].delta.as_deref(), Some("d25"));
        c.append_closed(Some("clear"), 999);
        let last = c.log.last().expect("closed");
        assert_eq!(last.kind, LogKind::Closed);
        assert_eq!(last.task.as_deref(), Some("t"), "closed line carries the last task");
        assert_eq!(c.last_activity_ms(), 999);
    }

    #[test]
    fn degraded_turn_still_logs() {
        let mut c = SessionContext::new("s1");
        c.append_turn(&brief("t", "first line", true), 5);
        assert!(c.log[0].degraded);
    }

    #[test]
    fn round_trip_and_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = ContextState::default();
        let a = st
            .mutate_at(dir.path(), "abc-123", |c| {
                assert!(c.touch(Some("w_1"), Some("p1"), Some("/repo")));
                assert!(!c.touch(None, Some("p1"), None), "same placement is a no-op");
                c.set_first_prompt("hello", 1)
            })
            .expect("m1")
            .expect("changed");
        assert_eq!(a.version, 1);
        // A no-op writes nothing and keeps the version.
        assert!(st.mutate_at(dir.path(), "abc-123", |_| false).expect("noop").is_none());
        st.mutate_at(dir.path(), "abc-123", |c| {
            c.append_turn(&brief("t", "d", false), 2);
            true
        })
        .expect("m2");
        let fresh = ContextState::default();
        let got = fresh.get_at(dir.path(), "abc-123").expect("get").expect("present");
        assert_eq!(got.version, 2);
        assert_eq!(got.first_prompt.as_deref(), Some("hello"));
        assert_eq!(got.log.len(), 1);
        assert_eq!(fresh.list_at(dir.path(), "w_1").len(), 1);
        assert!(fresh.list_at(dir.path(), "w_2").is_empty());
    }

    #[test]
    fn unreadable_file_is_not_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("bad.json");
        std::fs::write(&p, "{ nope").expect("write");
        let st = ContextState::default();
        assert!(st.mutate_at(dir.path(), "bad", |_| true).is_err());
        assert_eq!(std::fs::read_to_string(&p).expect("read"), "{ nope");
    }

    #[test]
    fn session_id_validation() {
        assert!(validate_session_id("0b6e3a1c-4f2d-4c55-9d1e-2a7b8c9d0e1f").is_ok());
        for bad in ["", "..", "../x", "a/b", "a\\b", "x.json", "s 1", "C:"] {
            assert!(validate_session_id(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(validate_session_id(&"a".repeat(129)).is_err());
    }

    fn ctx_with(id: &str, prompt: Option<&str>, turns: usize) -> SessionContext {
        let mut c = SessionContext::new(id);
        if let Some(p) = prompt {
            c.set_first_prompt(p, 1);
        }
        for i in 0..turns {
            c.append_turn(&brief(&format!("task{i}"), &format!("delta{i}"), false), 10 + i as u64);
        }
        c
    }

    #[test]
    fn inject_compact_has_prompt_and_last_eight() {
        let c = ctx_with("s1", Some("build the\ninstaller"), 12);
        let out = build_injection("compact", Some(&c), None, &[]);
        assert!(out.starts_with(INJECT_HEADER));
        assert!(out.contains("first prompt: build the installer"), "newline flattened: {out}");
        assert!(!out.contains("delta3"), "only the last 8: {out}");
        assert!(out.contains("delta4") && out.contains("delta11"));
        assert!(out.find("delta4") < out.find("delta11"), "oldest → newest");
        assert_eq!(build_injection("resume", Some(&c), None, &[]), out);
    }

    #[test]
    fn inject_startup_lists_intent_and_open_siblings() {
        let me = ctx_with("me", Some("x"), 1);
        let a = ctx_with("a", None, 2);
        let mut closed = ctx_with("b", None, 1);
        closed.append_closed(Some("clear"), 99);
        let out = build_injection("startup", Some(&me), Some("ship v2"), &[me.clone(), a, closed]);
        assert!(out.contains("Workspace goal: ship v2"));
        assert!(out.contains("- task1 — working"), "{out}");
        assert!(!out.contains("task0 — "), "only the latest task per sibling: {out}");
        assert_eq!(out.matches("\n- ").count(), 1, "self and closed sessions excluded: {out}");
    }

    #[test]
    fn inject_empty_cases_produce_nothing() {
        assert_eq!(build_injection("compact", None, Some("goal"), &[]), "");
        assert_eq!(build_injection("compact", Some(&SessionContext::new("s")), None, &[]), "");
        assert_eq!(build_injection("startup", None, None, &[]), "");
        assert_eq!(build_injection("startup", None, Some("   "), &[]), "");
        let c = ctx_with("s1", Some("p"), 3);
        assert_eq!(build_injection("clear", Some(&c), Some("g"), &[]), "");
        assert_eq!(build_injection("whatever", Some(&c), Some("g"), &[]), "");
    }

    #[test]
    fn inject_respects_byte_cap_dropping_oldest() {
        let mut c = SessionContext::new("s1");
        c.set_first_prompt(&"פ".repeat(3000), 1);
        for i in 0..8 {
            let mut b = brief(&"מ".repeat(20), &format!("D{i}{}", "ד".repeat(60)), false);
            b.next = Some("נ".repeat(20));
            c.append_turn(&b, i);
        }
        let out = build_injection("compact", Some(&c), None, &[]);
        assert!(out.len() <= INJECT_MAX_BYTES, "len {}", out.len());
        assert!(out.contains("D7"), "the newest line survives: {}", out.len());
        assert!(!out.contains("D0"), "the oldest goes first");
        // A single huge field is still hard-clipped on a char boundary.
        let mut big = SessionContext::new("s2");
        big.set_first_prompt(&"ש".repeat(2000), 1);
        let out = build_injection("compact", Some(&big), None, &[]);
        assert!(out.len() <= INJECT_MAX_BYTES);
    }

    #[test]
    fn prune_removes_only_old_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("old.json"), "{}").expect("w");
        std::fs::write(dir.path().join("keep.txt"), "x").expect("w");
        // Judge from 31 days in the future: the fresh .json is "old",
        // the non-session file is ignored.
        let later = SystemTime::now() + Duration::from_secs(31 * 24 * 60 * 60);
        assert_eq!(prune_dir(dir.path(), RETENTION, later), 1);
        assert!(!dir.path().join("old.json").exists());
        assert!(dir.path().join("keep.txt").exists());
        // From "now", nothing is old.
        std::fs::write(dir.path().join("new.json"), "{}").expect("w");
        assert_eq!(prune_dir(dir.path(), RETENTION, SystemTime::now()), 0);
        assert_eq!(prune_dir(&dir.path().join("missing"), RETENTION, later), 0);
    }
}
