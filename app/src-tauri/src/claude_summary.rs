//! Phase 17: Claude session auto-summary.
//!
//! Take a recent Claude Code conversation (the JSONL transcript that
//! Claude itself maintains under `~/.claude/projects/<proj>/<session>.jsonl`),
//! pipe the last N exchanges through `claude -p "<prompt>"` on the
//! same machine that hosts those transcripts, and save the resulting
//! summary as a ymux Note tagged `summary`. Two entry points:
//!
//!   - Manual: Ctrl+Alt+B in the desktop, the "Summarize" button in
//!     the Settings → Claude tab, or this module's `claude_summarize`
//!     Tauri command directly.
//!
//!   - Automatic: when a Claude Code Stop hook arrives via `feed.push`
//!     AND `settings.claude.auto_summarize_on_stop` is true, the RPC
//!     dispatcher calls `summarize_session_for_pane` in the
//!     background. Failures are logged to debug.log — never fatal.
//!
//! Both paths run the actual `claude` CLI on the *remote* server
//! (via a fresh exec channel) because that's where the transcripts
//! live and the Claude binary is already authenticated. Local-only
//! workspaces fall through to a Windows-side claude.exe lookup if
//! present.

use russh::client::Handle as SshHandle;
use russh::ChannelMsg;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::notes;
use crate::{log_debug, log_info, log_warn, AppState, Session, SshClient};

/// Output of one summarize call. Mirrored to the frontend.
#[derive(Clone, Serialize)]
pub(crate) struct SummaryResult {
    pub text: String,
    pub session_id: String,
    pub messages_count: u32,
    pub generated_at: String,
    /// Set when we saved the summary as a Note — the frontend uses
    /// this to scroll/highlight that note in the Notes modal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn pick_handle(state: &AppState, workspace_id: &str) -> Option<std::sync::Arc<SshHandle<SshClient>>> {
    let sessions = state.core.sessions.lock().ok()?;
    sessions.values().find_map(|s| match s {
        Session::Ssh(ssh) if ssh.workspace_id == workspace_id => Some(ssh.handle.clone()),
        _ => None,
    })
}

/// Run a shell pipeline over an SSH exec channel and return its
/// captured stdout. Times out at 30s — `claude -p` typically finishes
/// in 2-5s with a small history window.
async fn ssh_exec(
    handle: &SshHandle<SshClient>,
    cmd: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

/// Escape a string for safe single-quoted bash inclusion. Used to
/// fold the user-configured prompt into the remote `claude -p`
/// invocation without giving them an accidental shell-injection
/// vector.
pub(crate) fn bash_squote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Find the most recently-modified Claude JSONL on the remote and
/// return its path + session_id. The format matches what
/// `pane_list_claude_sessions` already produces — same `find` +
/// `printf` style so the parsing reuses the same expectations.
async fn pick_latest_session(
    handle: &SshHandle<SshClient>,
) -> Result<(String, String), String> {
    let inner = "find \"$HOME/.claude/projects\" -maxdepth 4 -name '*.jsonl' \
                 -printf '%T@\\t%p\\n' 2>/dev/null | sort -rn | head -1";
    let raw = ssh_exec(handle, &wrap_login(inner), 8).await?;
    let line = raw.lines().next().unwrap_or("").trim();
    let parts: Vec<&str> = line.splitn(2, '\t').collect();
    if parts.len() < 2 {
        return Err("no Claude sessions found on remote".into());
    }
    let path = parts[1].to_string();
    let session_id = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    Ok((path, session_id))
}

const CLAUDE_DETECT_SCRIPT: &str = "\
command -v claude 2>/dev/null && exit 0; \
for p in \
  $HOME/.claude/local/claude \
  $HOME/.local/bin/claude \
  /usr/local/bin/claude \
  /opt/homebrew/bin/claude \
  $HOME/.nvm/versions/node/*/bin/claude \
  $HOME/.fnm/aliases/default/bin/claude; do \
  if [ -x \"$p\" ]; then echo \"$p\"; exit 0; fi; \
done; \
exit 127";

fn cache_key(workspace_id: &str) -> String {
    format!("{workspace_id}:ssh")
}

pub(crate) async fn resolve_claude_path(
    state: &AppState,
    workspace_id: &str,
    handle: &SshHandle<SshClient>,
) -> String {
    // Share the same cache `claude_chat` populates — once detected on
    // a chat-send the summarize hits the cache too.
    if let Ok(map) = state.claude_paths.lock() {
        if let Some(p) = map.get(&cache_key(workspace_id)) {
            return p.clone();
        }
    }
    let out = match ssh_exec(handle, &wrap_login(CLAUDE_DETECT_SCRIPT), 8).await {
        Ok(s) => s,
        Err(_) => return "claude".to_string(),
    };
    let path = out
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string());
    match path {
        Some(p) if !p.is_empty() && !p.starts_with("ERROR") => {
            log_debug("CLAUDE", &format!(
                "claude_summary: detected claude at {p} (ws={workspace_id})"
            ));
            if let Ok(mut map) = state.claude_paths.lock() {
                map.insert(cache_key(workspace_id), p.clone());
            }
            p
        }
        _ => {
            log_warn("CLAUDE", &format!(
                "claude_summary: claude path detection failed for ws={workspace_id} — falling back to bare `claude`"
            ));
            "claude".to_string()
        }
    }
}

/// Wrap a script in `bash -lc '<script>'` so the remote shell sources
/// the user's `~/.bash_profile` / `~/.bashrc`. SSH execs run a
/// non-interactive non-login shell by default, which leaves any
/// rc-file-extended PATH (claude installed via npm-global, fnm,
/// nvm, $HOME/.claude/local, …) invisible. `bash -lc` flips both
/// flags so `command -v claude` and `command -v jq` actually find
/// them. Mirrors claude_chat::wrap_login.
pub(crate) fn wrap_login(script: &str) -> String {
    format!("bash -lc {}", bash_squote(script))
}

/// Build the bash pipeline that pulls the last N user+assistant
/// messages from a session file and pipes them through `claude -p
/// "<prompt>"`. The text content is extracted via `jq` (preferred)
/// with a `grep`-based fallback for boxes that don't have jq.
///
/// `claude_path` is the absolute path to claude (resolved + cached
/// per workspace). When detection failed we fall back to bare
/// `claude` and let the login-shell PATH catch it.
fn summary_pipeline(
    claude_path: &str,
    jsonl_path: &str,
    history: u32,
    prompt: &str,
) -> String {
    let path_q = bash_squote(jsonl_path);
    let prompt_q = bash_squote(prompt);
    let claude_q = bash_squote(claude_path);
    let jq_program = "select(.type == \"user\" or .type == \"assistant\") | \
                      \"\\(.type | ascii_upcase): \\(.message.content[0].text // .message.content // \"\")\"";
    // Note: this script is the INNER body that wrap_login wraps in
    // `bash -lc '…'`. So `command -v claude` here sees the user's
    // full rc-extended PATH.
    let inner = format!(
        "if command -v {claude} >/dev/null 2>&1 || [ -x {claude} ]; then \
           if command -v jq >/dev/null 2>&1; then \
             jq -r {jq} < {path} 2>/dev/null | tail -n {n} | {claude} -p {prompt}; \
           else \
             tail -n {n2} {path} | {claude} -p {prompt}; \
           fi; \
         else \
           echo 'ERROR: claude CLI not found on remote (PATH issue or not installed)' >&2; exit 127; \
         fi",
        claude = claude_q,
        jq = bash_squote(jq_program),
        path = path_q,
        n = history,
        // When jq isn't available we just feed claude the last
        // <history * 4> lines of the raw JSONL; not pretty but
        // works — claude itself parses the structure well.
        n2 = history.saturating_mul(4),
        prompt = prompt_q,
    );
    wrap_login(&inner)
}

/// The core. Pass workspace_id + optional session_id; we look up the
/// SSH handle, find the session if needed, run the summary pipeline,
/// capture the answer, save as a note. Used by both the manual
/// frontend command and the auto-stop hook path.
pub(crate) async fn summarize_session_inner(
    state: &AppState,
    app: &AppHandle,
    workspace_id: &str,
    pane_id: Option<&str>,
    explicit_session_id: Option<&str>,
    history_count: Option<u32>,
    prompt_override: Option<&str>,
) -> Result<SummaryResult, String> {
    let handle = pick_handle(state, workspace_id)
        .ok_or_else(|| "no active SSH session — connect a terminal pane in this workspace first".to_string())?;

    let history = history_count.unwrap_or_else(|| {
        state
            .settings
            .lock()
            .ok()
            .map(|s| s.claude.summary_history_count.max(1))
            .unwrap_or(10)
    });
    let prompt = match prompt_override {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => state
            .settings
            .lock()
            .ok()
            .map(|s| s.claude.summary_prompt.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                "Summarize the last conversation in 2-3 sentences in the same language the conversation used.".to_string()
            }),
    };
    // Substitute {N} so users can write "Summarize the last {N} exchanges…".
    let prompt = prompt.replace("{N}", &history.to_string());

    let (jsonl_path, session_id) = if let Some(sid) = explicit_session_id {
        // We trust the caller for the session_id — but we still
        // need the file path. Walk projects/* to find it.
        let inner = format!(
            "find \"$HOME/.claude/projects\" -maxdepth 4 -name {q}.jsonl 2>/dev/null | head -1",
            q = bash_squote(sid)
        );
        let path = ssh_exec(&handle, &wrap_login(&inner), 6)
            .await?
            .trim()
            .to_string();
        if path.is_empty() {
            return Err(format!("session {sid} not found under ~/.claude/projects/"));
        }
        (path, sid.to_string())
    } else {
        pick_latest_session(&handle).await?
    };

    let claude_path = resolve_claude_path(state, workspace_id, &handle).await;
    let pipeline = summary_pipeline(&claude_path, &jsonl_path, history, &prompt);
    let text = ssh_exec(&handle, &pipeline, 45).await?.trim().to_string();
    if text.is_empty() {
        return Err("claude -p returned empty output".into());
    }

    // Save as a note.
    let note_id = match notes::rpc_add(
        state,
        app,
        text.clone(),
        Some("summary".to_string()),
        Some(workspace_id.to_string()),
        pane_id.map(|s| s.to_string()),
    ) {
        Ok(n) => Some(n.id),
        Err(e) => {
            log_warn("CLAUDE", &format!("claude_summary: notes::rpc_add failed: {e}"));
            None
        }
    };

    Ok(SummaryResult {
        text,
        session_id,
        messages_count: history,
        generated_at: iso_now(),
        note_id,
    })
}

// ─── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn claude_summarize(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: Option<String>,
    session_id: Option<String>,
    history_count: Option<u32>,
    prompt_override: Option<String>,
) -> Result<SummaryResult, String> {
    summarize_session_inner(
        &state,
        &app,
        &workspace_id,
        pane_id.as_deref(),
        session_id.as_deref(),
        history_count,
        prompt_override.as_deref(),
    )
    .await
}

/// Stop-hook entry point. Called by the RPC dispatcher when a
/// `feed.push` arrives with `subkind="stop"` and the user opted into
/// auto-summarize. Best-effort — failures log but don't surface to
/// the agent or block the hook return.
pub(crate) async fn auto_summarize_on_stop(
    state: &AppState,
    app: &AppHandle,
    workspace_id: &str,
    pane_id: Option<&str>,
) {
    // Bail out fast when the feature is off — avoids spinning up an
    // SSH channel for every Stop event in projects the user doesn't
    // care about.
    let enabled = state
        .settings
        .lock()
        .ok()
        .map(|s| s.claude.auto_summarize_on_stop)
        .unwrap_or(false);
    if !enabled {
        return;
    }
    match summarize_session_inner(state, app, workspace_id, pane_id, None, None, None).await {
        Ok(r) => log_info("CLAUDE", &format!(
            "claude_summary: auto-saved note {} for workspace {workspace_id}",
            r.note_id.unwrap_or_default()
        )),
        Err(e) => log_warn("CLAUDE", &format!(
            "claude_summary: auto-summarize failed for workspace {workspace_id}: {e}"
        )),
    }
}
