//! Phase 24.A: backend for the (now-removed) ClaudeLog pane.
//!
//! Phase 24.D rolled back the ClaudeChat + ClaudeLog FE panes
//! ("three competing 'talk to claude' UIs felt fragmented"), but
//! Yossi explicitly asked to keep this backend module alive for a
//! future unified-view rebuild. The three sync/list/read tauri
//! commands stay registered in `invoke_handler!` — they just have
//! no FE caller right now. The `#![allow(dead_code)]` at the top
//! silences the cascade of warnings that the unused module +
//! response types would otherwise generate.
//!
//! Three tauri commands (registered, no current consumer):
//!   - claude_log_sync(workspace_id, session_id?) — SFTP-mirror new/
//!     changed files (mtime-gated, full-file fetch — no byte diffing)
//!   - claude_log_list(workspace_id) — pure local directory scan +
//!     per-file summary
//!   - claude_log_read(workspace_id, session_id) — parses the local
//!     jsonl into a structured ClaudeLogEntry stream (handles content
//!     as string OR block array; summarizes tool_use/tool_result)
//!
//! No background SSH reconnects — if there's no live handle, sync
//! errors cleanly and the user connects a terminal pane first.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use russh::client::Handle as SshHandle;
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use serde::Serialize;
use tauri::State;
use tokio::io::AsyncReadExt;

use crate::{config_dir_pub, log_debug, AppState, Session, SshClient};

// ─── public schemas ────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Debug, Default)]
pub(crate) struct ClaudeSyncResult {
    pub synced: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    pub total_bytes: u64,
}

#[derive(Clone, Serialize, Debug)]
pub(crate) struct ClaudeLogSummary {
    pub session_id: String,
    pub message_count: usize,
    pub first_user: Option<String>,
    pub last_assistant: Option<String>,
    pub project_path: Option<String>,
    pub file_size: u64,
    pub local_mtime: i64,
}

#[derive(Clone, Serialize, Debug)]
pub(crate) struct ClaudeLogEntry {
    pub line_no: usize,
    pub entry_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ClaudeLogUsage>,
}

/// Token counts from `message.usage` of an assistant/user line. Key
/// names mirror `claude_usage_local.rs`; missing keys count as 0.
#[derive(Clone, Serialize, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaudeLogUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

// ─── storage paths ─────────────────────────────────────────────────────────

fn claude_logs_dir(workspace_id: &str) -> Result<PathBuf, String> {
    let dir = config_dir_pub()?.join("claude-logs").join(workspace_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create {dir:?}: {e}"))?;
    Ok(dir)
}

fn local_jsonl_path(workspace_id: &str, session_id: &str) -> Result<PathBuf, String> {
    Ok(claude_logs_dir(workspace_id)?.join(format!("{session_id}.jsonl")))
}

// ─── SSH/SFTP helpers (parallel to file_manager's private versions) ────────

fn pick_ssh_handle(state: &AppState, workspace_id: &str) -> Option<Arc<SshHandle<SshClient>>> {
    let sessions = state.core.sessions.lock().ok()?;
    sessions.values().find_map(|s| match s {
        Session::Ssh(ssh) if ssh.workspace_id == workspace_id => Some(ssh.handle.clone()),
        _ => None,
    })
}

async fn open_sftp(handle: &SshHandle<SshClient>) -> Result<SftpSession, String> {
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open channel: {e}"))?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("request sftp: {e}"))?;
    let stream = chan.into_stream();
    SftpSession::new(stream)
        .await
        .map_err(|e| format!("sftp init: {e}"))
}

/// Run a one-shot exec channel and capture stdout. Used to enumerate
/// remote jsonl paths via `find`. Same shape as the snippets in
/// claude_summary.rs / pane_list_claude_sessions in lib.rs — kept
/// local here so the module stays self-contained.
async fn ssh_exec_capture(
    handle: &SshHandle<SshClient>,
    cmd: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
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

// ─── remote enumeration ────────────────────────────────────────────────────

/// One remote jsonl file, parsed from `find -printf '%T@\t%s\t%p\n'`.
/// Size column is parsed and discarded — total_bytes in the result
/// comes from actually-downloaded bytes, not the find-reported size.
struct RemoteJsonl {
    mtime: i64,
    path: String,
    session_id: String,
}

fn parse_find_output(text: &str) -> Vec<RemoteJsonl> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        // `%T@` is "seconds.nanos"; take just the seconds. Column 2
        // (parts[1]) is the reported size — we drop it.
        let mtime = parts[0]
            .split('.')
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let path = parts[2].to_string();
        let session_id = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() {
            continue;
        }
        out.push(RemoteJsonl {
            mtime,
            path,
            session_id,
        });
    }
    out
}

async fn list_remote_jsonls(
    handle: &SshHandle<SshClient>,
    session_id_filter: Option<&str>,
) -> Result<Vec<RemoteJsonl>, String> {
    let name_filter = match session_id_filter {
        Some(sid) => {
            // Defensive: session_id format is UUID-like
            // (alphanumerics + dashes). Reject anything weirder so the
            // raw value never reaches the shell.
            if !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(format!("invalid session_id {sid:?}"));
            }
            format!("'{sid}.jsonl'")
        }
        None => "'*.jsonl'".to_string(),
    };
    let script = format!(
        "find \"$HOME/.claude/projects\" -maxdepth 4 -name {name_filter} \
         -printf '%T@\\t%s\\t%p\\n' 2>/dev/null",
    );
    let raw = ssh_exec_capture(handle, &script, 10).await?;
    Ok(parse_find_output(&raw))
}

// ─── SFTP download with atomic-ish write ───────────────────────────────────

async fn fetch_jsonl(
    sftp: &SftpSession,
    remote_path: &str,
    local_path: &std::path::Path,
) -> Result<u64, String> {
    let mut file = sftp
        .open(remote_path)
        .await
        .map_err(|e| format!("sftp open {remote_path}: {e}"))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .await
        .map_err(|e| format!("sftp read {remote_path}: {e}"))?;
    drop(file);

    // Write to a sibling temp file then rename so the local jsonl is
    // never observed in a half-written state by claude_log_list /
    // claude_log_read calls that might race with sync.
    let parent = local_path
        .parent()
        .ok_or_else(|| format!("no parent for {local_path:?}"))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download"),
        std::process::id()
    ));
    std::fs::write(&tmp, &buf).map_err(|e| format!("write tmp {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, local_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {tmp:?} -> {local_path:?}: {e}")
    })?;
    Ok(buf.len() as u64)
}

fn local_mtime_secs(path: &std::path::Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_secs() as i64)
}

// ─── tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn claude_log_sync(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: Option<String>,
) -> Result<ClaudeSyncResult, String> {
    let handle = pick_ssh_handle(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session for this workspace — connect a terminal pane first".to_string())?;

    let remotes = list_remote_jsonls(&handle, session_id.as_deref()).await?;
    if remotes.is_empty() && session_id.is_some() {
        return Err(format!(
            "no jsonl found for session_id {:?}",
            session_id.unwrap()
        ));
    }

    // Single SFTP session for the whole batch — opening one channel
    // per file would be wasteful when syncing All.
    let sftp = open_sftp(&handle).await?;

    let mut result = ClaudeSyncResult::default();
    for remote in &remotes {
        let local = match local_jsonl_path(&workspace_id, &remote.session_id) {
            Ok(p) => p,
            Err(e) => {
                result.errors.push(format!("{}: {e}", remote.session_id));
                continue;
            }
        };
        let local_mt = local_mtime_secs(&local).unwrap_or(0);
        if local.exists() && local_mt >= remote.mtime {
            result.skipped += 1;
            continue;
        }
        match fetch_jsonl(&sftp, &remote.path, &local).await {
            Ok(bytes) => {
                result.synced += 1;
                result.total_bytes += bytes;
            }
            Err(e) => {
                result.errors.push(format!("{}: {e}", remote.session_id));
            }
        }
    }
    let _ = sftp.close().await;
    log_debug("CLAUDE", &format!(
        "claude_log_sync ws={workspace_id} sid={:?} synced={} skipped={} errors={}",
        session_id, result.synced, result.skipped, result.errors.len()
    ));
    Ok(result)
}

#[tauri::command]
pub(crate) fn claude_log_list(workspace_id: String) -> Result<Vec<ClaudeLogSummary>, String> {
    let dir = claude_logs_dir(&workspace_id)?;
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return Ok(vec![]),
    };
    let mut out: Vec<ClaudeLogSummary> = Vec::new();
    for ent in read.flatten() {
        let path = ent.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".jsonl") {
            continue;
        }
        let session_id = name.trim_end_matches(".jsonl").to_string();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_size = meta.len();
        let local_mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (message_count, first_user, last_assistant, project_path) = summarize_jsonl(&path);
        out.push(ClaudeLogSummary {
            session_id,
            message_count,
            first_user,
            last_assistant,
            project_path,
            file_size,
            local_mtime,
        });
    }
    out.sort_by(|a, b| b.local_mtime.cmp(&a.local_mtime));
    Ok(out)
}

// Phase 24.D: claude_log_pane_set was removed alongside the
// ClaudeLog pane kind (it persisted picker selection / filter to a
// `claudelog` field on LayoutNode::Pane that no longer exists). If
// the unified-view rebuild brings the pane back, restore both the
// field on LayoutNode::Pane and this command.

#[tauri::command]
pub(crate) fn claude_log_read(
    workspace_id: String,
    session_id: String,
) -> Result<Vec<ClaudeLogEntry>, String> {
    let path = local_jsonl_path(&workspace_id, &session_id)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {path:?}: {e}"))?;
    let mut out: Vec<ClaudeLogEntry> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines silently
        };
        if let Some(entry) = entry_from_json(&v, idx + 1) {
            out.push(entry);
        }
    }
    Ok(out)
}

// ─── jsonl parsing helpers ─────────────────────────────────────────────────

/// Light summary read — just enumerate `type`s and pull the first
/// user / last assistant text. Stops at first user found and keeps
/// updating last_assistant. Also pulls `cwd` from any line that has
/// one (first-found-wins).
fn summarize_jsonl(
    path: &std::path::Path,
) -> (usize, Option<String>, Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (0, None, None, None);
    };
    let mut count: usize = 0;
    let mut first_user: Option<String> = None;
    let mut last_assistant: Option<String> = None;
    let mut project_path: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if matches!(ty, "user" | "assistant") {
            count += 1;
        }
        if project_path.is_none() {
            if let Some(cwd) = v.get("cwd").and_then(|x| x.as_str()) {
                if !cwd.is_empty() {
                    project_path = Some(cwd.to_string());
                }
            }
        }
        let snippet = extract_text(&v);
        match ty {
            "user" if first_user.is_none() && !snippet.is_empty() => {
                first_user = Some(truncate(&snippet, 240));
            }
            "assistant" if !snippet.is_empty() => {
                last_assistant = Some(truncate(&snippet, 240));
            }
            _ => {}
        }
    }
    (count, first_user, last_assistant, project_path)
}

/// Build a ClaudeLogEntry from one parsed jsonl line. Returns None
/// for entries we don't recognize (so callers can silently skip).
fn entry_from_json(v: &serde_json::Value, line_no: usize) -> Option<ClaudeLogEntry> {
    let ty = v.get("type").and_then(|x| x.as_str())?.to_string();
    let timestamp = v
        .get("timestamp")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let session_id = v
        .get("sessionId")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let (text, tool_name) = match ty.as_str() {
        "user" | "assistant" => (extract_text(v), None),
        "system" => (
            v.get("content")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            None,
        ),
        "summary" => (
            v.get("summary")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            None,
        ),
        // tool_use / tool_result rarely appear at the top level —
        // they're usually nested inside message.content blocks. If
        // they DO appear top-level (some claude versions emit
        // tool_use as its own line), surface them too.
        "tool_use" => (
            extract_tool_use_summary(v),
            v.get("name").and_then(|x| x.as_str()).map(|s| s.to_string()),
        ),
        "tool_result" => (
            extract_tool_result_summary(v),
            v.get("tool_use_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        ),
        _ => return None, // unknown type — skip silently
    };
    let usage = match ty.as_str() {
        "user" | "assistant" => usage_from_json(v),
        _ => None,
    };
    Some(ClaudeLogEntry {
        line_no,
        entry_type: ty,
        text,
        tool_name,
        timestamp,
        session_id,
        usage,
    })
}

/// `message.usage` as counts; None when absent or not an object.
fn usage_from_json(v: &serde_json::Value) -> Option<ClaudeLogUsage> {
    let u = v.get("message")?.get("usage")?;
    if !u.is_object() {
        return None;
    }
    let num = |k: &str| u.get(k).and_then(|n| n.as_u64()).unwrap_or(0);
    Some(ClaudeLogUsage {
        input_tokens: num("input_tokens"),
        output_tokens: num("output_tokens"),
        cache_read_input_tokens: num("cache_read_input_tokens"),
        cache_creation_input_tokens: num("cache_creation_input_tokens"),
    })
}

/// Pull the text content out of a user/assistant entry. Handles
/// `message.content` as either a plain string OR an array of typed
/// blocks (text / tool_use / tool_result / image / etc.).
fn extract_text(v: &serde_json::Value) -> String {
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| v.get("content"));
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut buf = String::new();
        for block in arr {
            let bty = block.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match bty {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(t);
                    }
                }
                "tool_use" => {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    let name = block.get("name").and_then(|x| x.as_str()).unwrap_or("?");
                    buf.push_str(&format!("[Tool: {name}]"));
                }
                "tool_result" => {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    let snippet = extract_tool_result_summary(block);
                    buf.push_str(&format!("[Result: {}]", truncate(&snippet, 120)));
                }
                "image" => {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str("[Image]");
                }
                _ => {}
            }
        }
        return buf;
    }
    String::new()
}

fn extract_tool_use_summary(v: &serde_json::Value) -> String {
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("?");
    // Show the first input field as a hint (e.g., `command` for Bash).
    let input_hint = v
        .get("input")
        .and_then(|i| i.as_object())
        .and_then(|m| {
            // Prefer common fields likely to be human-meaningful.
            for k in ["command", "pattern", "file_path", "path", "url", "prompt"] {
                if let Some(val) = m.get(k).and_then(|x| x.as_str()) {
                    return Some(format!("{k}: {}", truncate(val, 120)));
                }
            }
            None
        })
        .unwrap_or_default();
    if input_hint.is_empty() {
        format!("[Tool: {name}]")
    } else {
        format!("[Tool: {name}] {input_hint}")
    }
}

fn extract_tool_result_summary(v: &serde_json::Value) -> String {
    // tool_result.content may be a string or an array of {type, text}.
    let content = v.get("content");
    if let Some(s) = content.and_then(|x| x.as_str()) {
        return truncate(s, 240);
    }
    if let Some(arr) = content.and_then(|x| x.as_array()) {
        let mut buf = String::new();
        for block in arr {
            if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(t);
                if buf.len() > 240 {
                    break;
                }
            }
        }
        return truncate(&buf, 240);
    }
    String::new()
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // pins full extraction; breaking it drops token counts from entries
    #[test]
    fn assistant_usage_extracted() {
        let v = json!({"type":"assistant","message":{"content":"hi","usage":{
            "input_tokens":3,"output_tokens":5,
            "cache_read_input_tokens":7,"cache_creation_input_tokens":11}}});
        let e = entry_from_json(&v, 1).unwrap();
        assert_eq!(
            e.usage,
            Some(ClaudeLogUsage {
                input_tokens: 3,
                output_tokens: 5,
                cache_read_input_tokens: 7,
                cache_creation_input_tokens: 11
            })
        );
    }

    // pins omit-when-absent; breaking it adds a null usage key to the wire JSON
    #[test]
    fn absent_usage_is_none_and_omitted() {
        let v = json!({"type":"user","message":{"content":"hi"}});
        let e = entry_from_json(&v, 1).unwrap();
        assert!(e.usage.is_none());
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("usage"));
    }

    // pins missing sub-keys → 0 and empty object → Some(zeros)
    #[test]
    fn missing_subkeys_default_zero() {
        let v = json!({"type":"assistant","message":{"usage":{"output_tokens":9}}});
        let u = entry_from_json(&v, 1).unwrap().usage.unwrap();
        assert_eq!(u.output_tokens, 9);
        assert_eq!(u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens, 0);
        let v = json!({"type":"assistant","message":{"usage":{}}});
        assert_eq!(entry_from_json(&v, 1).unwrap().usage, Some(ClaudeLogUsage::default()));
    }

    // pins non-object usage → None; breaking it would misread malformed lines
    #[test]
    fn non_object_usage_is_none() {
        for bad in [json!("x"), json!(5), json!([1]), json!(null)] {
            let v = json!({"type":"assistant","message":{"usage":bad}});
            assert!(entry_from_json(&v, 1).unwrap().usage.is_none());
        }
    }

    // pins type gating; breaking it attributes usage to system/summary lines
    #[test]
    fn non_message_types_ignore_usage() {
        let v = json!({"type":"system","content":"s","message":{"usage":{"input_tokens":1}}});
        assert!(entry_from_json(&v, 1).unwrap().usage.is_none());
    }
}
