//! Grok Build sessions on disk: `~/.grok/sessions/<group>/<id>/`.
//!
//! `summary.json` is the list row. `updates.jsonl` is the transcript.
//! `chat_history.jsonl` is model context and is not read. A session id
//! that goes into `grok --resume` has to be a single shell token.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

const PREVIEW_BYTES: usize = 8 * 1024;
const PREVIEW_CHARS: usize = 120;

/// A resume id we are willing to place in a shell command. One token,
/// no quotes, no spaces, no `$`.
pub fn safe_session_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 80
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

/// `grok --resume <id>` when the id is a single safe token.
pub fn resume_command(id: &str) -> Option<String> {
    safe_session_id(id).then(|| format!("grok --resume {id}"))
}

#[derive(Clone, Debug)]
pub struct GrokSessionRow {
    pub session_id: String,
    pub cwd: String,
    pub updates_path: String,
    pub mtime_unix: i64,
    pub title: Option<String>,
    pub first_user: Option<String>,
    pub last_assistant: Option<String>,
    pub is_subagent: bool,
}

/// Newest sessions under `root` (`…/sessions`). A missing directory is
/// an empty list. `scope`, when set, keeps rows whose `info.cwd` matches.
pub fn list_local(root: &Path, limit: usize, scope: Option<&str>) -> Vec<GrokSessionRow> {
    let mut rows = Vec::new();
    let groups = match fs::read_dir(root) {
        Ok(it) => it,
        Err(_) => return rows,
    };
    for group in groups.flatten() {
        let gpath = group.path();
        if !is_real_dir(&gpath) {
            continue;
        }
        let sessions = match fs::read_dir(&gpath) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for session in sessions.flatten() {
            let spath = session.path();
            if !is_real_dir(&spath) {
                continue;
            }
            let Some(row) = row_from_dir(&spath) else {
                continue;
            };
            if let Some(want) = scope {
                if !super::paths_equal(&row.cwd, want) {
                    continue;
                }
            }
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| b.mtime_unix.cmp(&a.mtime_unix).then_with(|| a.session_id.cmp(&b.session_id)));
    rows.truncate(limit);
    rows
}

fn is_real_dir(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    meta.is_dir() && !meta.file_type().is_symlink()
}

fn row_from_dir(dir: &Path) -> Option<GrokSessionRow> {
    let id = dir.file_name()?.to_str()?.to_string();
    if !safe_session_id(&id) {
        return None;
    }
    let summary_path = dir.join("summary.json");
    let updates = dir.join("updates.jsonl");
    if !updates.is_file() {
        return None;
    }
    let summary = fs::read_to_string(&summary_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    if summary.get("hidden").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let kind = summary
        .get("session_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let cwd = summary
        .pointer("/info/cwd")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    let title = summary
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mtime = summary_mtime(&summary).unwrap_or_else(|| file_mtime(&updates));
    let first_user = preview_edge(&updates, true, "user_message_chunk");
    let last_assistant = preview_edge(&updates, false, "agent_message_chunk");
    Some(GrokSessionRow {
        session_id: id,
        cwd,
        updates_path: updates.to_string_lossy().into_owned(),
        mtime_unix: mtime,
        title,
        first_user,
        last_assistant,
        is_subagent: kind.contains("subagent") || kind.contains("sub-agent"),
    })
}

fn summary_mtime(summary: &Value) -> Option<i64> {
    for key in ["updated_at", "last_active_at", "created_at"] {
        let Some(s) = summary.get(key).and_then(|v| v.as_str()) else {
            continue;
        };
        if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(t.timestamp());
        }
    }
    None
}

fn file_mtime(path: &Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// First (`head`) or last (`tail`) matching chunk, from a short edge of
/// the file. The middle is not read.
fn preview_edge(path: &Path, head: bool, kind: &str) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let chunk = if head {
        let mut buf = vec![0u8; PREVIEW_BYTES];
        let n = file.take(PREVIEW_BYTES as u64).read(&mut buf).ok()?;
        buf.truncate(n);
        buf
    } else {
        let len = file.metadata().ok()?.len();
        let start = len.saturating_sub(PREVIEW_BYTES as u64);
        let mut buf = vec![0u8; PREVIEW_BYTES];
        let mut file = file;
        file.seek(SeekFrom::Start(start)).ok()?;
        let n = file.read(&mut buf).ok()?;
        buf.truncate(n);
        buf
    };
    let text = String::from_utf8_lossy(&chunk);
    let lines: Vec<&str> = text.lines().collect();
    let iter: Box<dyn Iterator<Item = &&str>> = if head {
        Box::new(lines.iter())
    } else {
        Box::new(lines.iter().rev())
    };
    for line in iter {
        if let Some(found) = chunk_text(line, kind) {
            return Some(clip(&found));
        }
    }
    None
}

fn chunk_text(line: &str, kind: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    let update = v.pointer("/params/update")?;
    if update.get("sessionUpdate").and_then(|s| s.as_str()) != Some(kind) {
        return None;
    }
    let content = update.get("content")?;
    let text = content_text(content);
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn content_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(content_text).collect::<Vec<_>>().join(""),
        Value::Object(_) => {
            if let Some(s) = value.get("text").and_then(|t| t.as_str()) {
                return s.to_string();
            }
            if let Some(inner) = value.get("content") {
                return content_text(inner);
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn clip(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(PREVIEW_CHARS).collect()
}

/// One remote listing line: `mtime \t id \t cwd \t title \t sub`.
/// Empty title and cwd are allowed. A bad id is dropped.
pub fn parse_remote_line(line: &str) -> Option<GrokSessionRow> {
    let parts: Vec<&str> = line.splitn(5, '\t').collect();
    if parts.len() < 5 {
        return None;
    }
    let id = parts[1].trim();
    if !safe_session_id(id) {
        return None;
    }
    let mtime = parts[0]
        .split('.')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let cwd = parts[2].trim().to_string();
    let title = {
        let t = parts[3].trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    Some(GrokSessionRow {
        session_id: id.to_string(),
        cwd,
        updates_path: String::new(),
        mtime_unix: mtime,
        title,
        first_user: None,
        last_assistant: None,
        is_subagent: parts[4].trim() == "1",
    })
}

/// GNU find script for a Linux remote. `limit` is a clamped integer, the
/// only value interpolated. Paths stay inside `$HOME/.grok/sessions`.
pub fn remote_list_script(limit: usize) -> String {
    let n = limit.clamp(1, 200);
    format!(
        "find \"$HOME/.grok/sessions\" -mindepth 3 -maxdepth 3 -name summary.json \
         -printf '%T@\\t%p\\n' 2>/dev/null | sort -rn | head -{n} | \
         while IFS=$'\\t' read -r mt path; do \
           hidden=$(grep -m1 -oE '\"hidden\"[[:space:]]*:[[:space:]]*true' \"$path\" >/dev/null && echo 1 || echo 0); \
           if [ \"$hidden\" = 1 ]; then continue; fi; \
           cwd=$(grep -m1 -oE '\"cwd\"[[:space:]]*:[[:space:]]*\"[^\"]*\"' \"$path\" | sed -E 's/.*:[[:space:]]*\"([^\"]*)\".*/\\1/'); \
           title=$(grep -m1 -oE '\"title\"[[:space:]]*:[[:space:]]*\"[^\"]*\"' \"$path\" | sed -E 's/.*:[[:space:]]*\"([^\"]*)\".*/\\1/'); \
           kind=$(grep -m1 -oE '\"session_kind\"[[:space:]]*:[[:space:]]*\"[^\"]*\"' \"$path\" | sed -E 's/.*:[[:space:]]*\"([^\"]*)\".*/\\1/'); \
           case \"$kind\" in *subagent*|*sub-agent*) sub=1 ;; *) sub=0 ;; esac; \
           id=$(basename \"$(dirname \"$path\")\"); \
           printf '%s\\t%s\\t%s\\t%s\\t%s\\n' \"$mt\" \"$id\" \"$cwd\" \"$title\" \"$sub\"; \
         done"
    )
}

pub fn sessions_root_from_home(home: &Path) -> PathBuf {
    home.join(".grok").join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_id_is_one_shell_token() {
        assert!(safe_session_id("abc-DEF_1"));
        assert!(resume_command("abc").as_deref() == Some("grok --resume abc"));
        assert!(!safe_session_id(""));
        assert!(!safe_session_id("a b"));
        assert!(!safe_session_id("a;rm"));
        assert!(!safe_session_id("-flag"));
        assert!(resume_command("a;rm").is_none());
    }

    #[test]
    fn remote_line_drops_a_bad_id() {
        let ok = parse_remote_line("100\tabc-1\t/srv/app\tHello\t0").unwrap();
        assert_eq!(ok.session_id, "abc-1");
        assert_eq!(ok.cwd, "/srv/app");
        assert_eq!(ok.title.as_deref(), Some("Hello"));
        assert!(!ok.is_subagent);
        assert!(parse_remote_line("100\ta b\t/srv\t\t0").is_none());
    }

    #[test]
    fn list_reads_summary_and_skips_hidden() {
        let root = std::env::temp_dir().join(format!("ymux-grok-sess-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("group").join("sess-1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("summary.json"),
            r#"{"title":"Fix the pane","updated_at":"2026-11-18T12:00:00Z","info":{"cwd":"/srv/app"}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("updates.jsonl"),
            "{\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"user_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hello pane\"}}}}\n",
        )
        .unwrap();
        let hidden = root.join("group").join("sess-2");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("summary.json"), r#"{"hidden":true,"info":{"cwd":"/srv/app"}}"#).unwrap();
        fs::write(hidden.join("updates.jsonl"), "{}\n").unwrap();

        let rows = list_local(&root, 10, Some("/srv/app"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "sess-1");
        assert_eq!(rows[0].title.as_deref(), Some("Fix the pane"));
        assert_eq!(rows[0].first_user.as_deref(), Some("hello pane"));
        assert!(list_local(&root, 10, Some("/other")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn remote_script_interpolates_only_the_limit() {
        let s = remote_list_script(9999);
        assert!(s.contains("head -200"));
        assert!(!s.contains("9999"));
        assert!(s.contains("$HOME/.grok/sessions"));
    }
}
