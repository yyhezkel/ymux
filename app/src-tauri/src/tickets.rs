//! Dev-mode tickets.
//!
//! When a workspace browser has Dev Mode on, right-clicking an element
//! captures it as a "ticket": xpath + css selector + bounded HTML +
//! computed-style summary + optional screenshot + a user-written
//! description. All CRUD flows through the four Tauri commands at the
//! bottom.
//!
//! Tickets belong to a PROJECT, not just to a workspace — the whole
//! point is to hand one to Claude Code inside the right repo. So when
//! the workspace's project is reachable from this machine they are
//! written to `<project>/.ymux-tickets/`; otherwise they fall back to
//! `<config_dir>/tickets/<workspace_id>/` and still record the project
//! path as metadata, so nothing is ever orphaned.
//!
//! The project is DERIVED, not asked for (see `resolve_project`). The
//! workspace already carries everything needed: `git_worktree`, `cwd`
//! and `connection`. Phase 54 put a `project_path` on the `Workspace`
//! struct and made the user pick a folder through an SFTP browser —
//! that meant a workspaces.json migration and a dialog for something
//! the app already knew. Here `project_path` lives on the TICKET
//! instead: every ticket is self-describing and no schema moves.
//!
//! Writes are atomic (`<name>.<pid>.tmp` then rename) per Rule #7 —
//! never a partial ticket on disk.
//!
//! Privacy (Rule #1): captured element HTML can contain whatever the
//! page rendered, including user data. Nothing in this module logs
//! element content — only ids, workspace ids, and lengths.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::client::Handle as SshHandle;
use serde::{Deserialize, Serialize};

use crate::file_manager::{
    pick_ssh_handle_for_workspace, remote_exec, remote_read_bytes, remote_write_bytes,
};
use crate::{config_dir, log_debug, log_info, log_warn, SshClient};

/// Directory under `config_dir()` holding tickets that could not be
/// written into their project (remote workspace, folder missing).
const TICKETS_DIRNAME: &str = "tickets";

/// Folder created inside a project to hold its tickets. Dot-prefixed so
/// it sorts out of the way; committing it or gitignoring it is the
/// user's call — we never touch their .gitignore.
const PROJECT_DIRNAME: &str = ".ymux-tickets";

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct TicketElement {
    #[serde(default)]
    pub xpath: String,
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub html: String,
    /// Free-form JSON blob of computed style / bounding box. The
    /// frontend fills whatever it can capture; the backend just
    /// round-trips it.
    #[serde(default)]
    #[ts(type = "unknown")]
    pub style: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct Ticket {
    pub id: String,
    /// ISO-8601 (UTC).
    pub created: String,
    pub url: String,
    pub element: TicketElement,
    /// Filename inside the workspace's tickets dir (relative), not an
    /// absolute path. `None` when no screenshot was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Persisted as one of `"open" | "resolved"`. Kept as a String so
    /// future statuses don't need a migration.
    #[serde(default = "default_status")]
    pub status: String,
    pub workspace_id: String,
    /// Project this ticket belongs to, as an absolute path. Recorded
    /// even when the ticket had to be stored app-locally (remote
    /// workspace), so it is always attributable to a repo. `None` only
    /// when the workspace has no cwd and no worktree at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    /// Placeholder for a later auto-fix hand-off (Claude Code hint).
    /// Always serialized so the schema is stable.
    #[serde(default)]
    pub source_hint: Option<String>,
}

/// Where a workspace's tickets live, and why. Returned to the frontend
/// so the capture modal can show the destination BEFORE saving — writing
/// into someone's repo should never be a surprise.
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ProjectResolution {
    /// Absolute project root, when one could be derived.
    pub project_path: Option<String>,
    /// Directory tickets are actually written to.
    pub tickets_dir: String,
    /// True when `tickets_dir` is inside the project (the good case);
    /// false when we fell back to app-local storage.
    pub in_project: bool,
    /// Which rung of the ladder produced `project_path`, for the UI to
    /// explain itself: "override" | "worktree" | "git" | "cwd" | "none".
    pub source: String,
    /// Why we fell back, when `in_project` is false. Empty otherwise.
    pub fallback_reason: String,
    /// Which machine `tickets_dir` names: "local" | "wsl" | "ssh".
    pub transport: String,
    /// Host for ssh, distro for wsl, empty for local — so the UI can say
    /// "srv-01:/home/y/proj/.ymux-tickets" rather than a bare path.
    pub host_label: String,
    /// True iff a write would succeed right now. When false the UI must
    /// not present Save as an ordinary action.
    pub writable: bool,
    /// "ok" | "disconnected" | "no_project" | "unreachable".
    pub status: String,
}

fn default_status() -> String {
    "open".to_string()
}

#[derive(Clone, Deserialize)]
pub struct NewTicket {
    pub url: String,
    pub element: TicketElement,
    /// Optional PNG data-url (e.g. `data:image/png;base64,…`). When
    /// present we decode and drop it next to the JSON.
    #[serde(default)]
    pub screenshot_data_url: Option<String>,
    #[serde(default)]
    pub description: String,
}

// ─── paths ──────────────────────────────────────────────────────────

/// Best-effort validation of a ticket id: it becomes a filename, so
/// reject anything that isn't shaped like one we generated.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Same guard for workspace ids, which become a directory name.
/// `new_workspace_id()` produces `w_<hex>`, so underscore is allowed
/// here but `.` and any separator still are not — no traversal.
fn valid_ws_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Choose the tmux session for the pane-cwd rung. `candidates` are
/// `(session_id, tmux_session)` pairs; the active pane's session wins,
/// otherwise the smallest tmux name so the choice never depends on map order.
fn pick_tmux_session(
    candidates: &[(String, String)],
    active_session_id: Option<&str>,
) -> Option<String> {
    if let Some(active) = active_session_id {
        if let Some((_, t)) = candidates.iter().find(|(sid, _)| sid == active) {
            return Some(t.clone());
        }
    }
    candidates.iter().map(|(_, t)| t).min().cloned()
}

/// tmux session for this workspace's pane-cwd lookup: the active pane's
/// session when known, else the smallest name. Headless sessions (the ones
/// `workspace_ensure_connected` makes to back the file manager) carry
/// `None`; only pane-backed ones name a session. One lock at a time.
fn tmux_session_for_workspace(state: &crate::AppState, workspace_id: &str) -> Option<String> {
    let active_pane = state
        .active_panes
        .lock()
        .ok()
        .and_then(|m| m.get(workspace_id).cloned());
    let active_session = active_pane.and_then(|pane| {
        state
            .core
            .pane_sessions
            .lock()
            .ok()
            .and_then(|m| m.get(&pane).cloned())
    });
    let candidates: Vec<(String, String)> = {
        let sessions = state.core.sessions.lock().ok()?;
        sessions
            .iter()
            .filter_map(|(sid, sess)| match sess {
                crate::Session::Ssh(s) if s.workspace_id == workspace_id => s
                    .tmux_session
                    .as_ref()
                    .map(|t| (sid.clone(), t.clone())),
                _ => None,
            })
            .collect()
    };
    pick_tmux_session(&candidates, active_session.as_deref())
}

/// App-local fallback: `<config_dir>/tickets/<workspace_id>/`.
fn fallback_dir(workspace_id: &str) -> Result<PathBuf, String> {
    if !valid_ws_id(workspace_id) {
        return Err(format!("invalid workspace id {workspace_id:?}"));
    }
    Ok(config_dir()?.join(TICKETS_DIRNAME).join(workspace_id))
}

// ─── remote command builders + parsers (pure, unit-tested) ──────────
//
// These build the shell one-liners the SSH backend runs, and parse what
// comes back. They are pure fn(&str)->String / fn(&str)->T on purpose:
// the shell-quoting here is the Rule #3 boundary for the whole tickets
// feature, so it has to be testable without a server.

/// Join a POSIX directory and a basename. `dir` is validated absolute by
/// `remote_project_dir`; `name` is always a `valid_id`-derived filename.
fn posix_join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// `<project>/.ymux-tickets` for an absolute POSIX project path.
fn remote_project_dir(project: &str) -> Result<String, String> {
    let p = project.trim_end_matches('/');
    if !p.starts_with('/') {
        return Err(format!("remote project path must be absolute: {project:?}"));
    }
    if p.contains('\\') {
        return Err(format!("remote project path must be POSIX: {project:?}"));
    }
    Ok(format!("{p}/{PROJECT_DIRNAME}"))
}

fn q(s: &str) -> String {
    ymux_core::shell_quote(s)
}

/// `git -C <cwd> rev-parse --show-toplevel`. One round trip, and it
/// resolves a linked worktree to that worktree's root, which is what we
/// want — unlike walking, which would be N SFTP round trips.
fn cmd_git_root(cwd: &str) -> String {
    format!("git -C {} rev-parse --show-toplevel 2>/dev/null", q(cwd))
}

fn cmd_mkdir_p(dir: &str) -> String {
    format!("mkdir -p {}", q(dir))
}

/// `mv -f` is rename(2): an atomic REPLACE. This is the only way to
/// overwrite an existing ticket — SFTP's rename fails when the
/// destination exists (russh-sftp negotiates no posix-rename).
fn cmd_mv_into_place(tmp: &str, dst: &str) -> String {
    format!("mv -f {} {}", q(tmp), q(dst))
}

/// One round trip for the whole listing. `tr -d` rather than
/// `base64 -w0` so BSD/macOS remotes work too; base64 so a CRLF-
/// translating server cannot corrupt the JSON framing.
fn cmd_list_json_b64(dir: &str) -> String {
    format!(
        "d={}; [ -d \"$d\" ] || exit 0; for f in \"$d\"/*.json; do [ -f \"$f\" ] || continue; printf '%s\\t' \"${{f##*/}}\"; base64 \"$f\" | tr -d '\\n'; printf '\\n'; done",
        q(dir)
    )
}

fn cmd_rm_f(paths: &[String]) -> String {
    let mut out = String::from("rm -f");
    for p in paths {
        out.push(' ');
        out.push_str(&q(p));
    }
    out
}

/// Ask tmux where the workspace's pane actually is.
///
/// An SSH workspace very often has NO recorded `cwd` — the user connects
/// and `cd`s inside the pane, and nothing writes that back to
/// workspaces.json. Observed live: a real workspace with `cwd: ""` and
/// `git_worktree: ""`, which made the whole ladder resolve to nothing and
/// dropped the ticket into the app-local fallback. The pane's live
/// directory is the project the user is actually working in.
fn cmd_tmux_pane_cwd(session: &str) -> String {
    format!(
        "tmux display-message -p -t {} '#{{pane_current_path}}' 2>/dev/null",
        q(session)
    )
}

/// `None` = no usable path. Never an error: the caller falls back to the
/// next rung, which is correct for a project that simply is not under
/// git. Shared with the tmux probe, which prints the same shape — an
/// exit code plus a single absolute path line.
fn parse_git_root(out: &str, code: i32) -> Option<String> {
    if code != 0 {
        return None;
    }
    let first = out.lines().next()?.trim_end_matches('\r').trim();
    (first.starts_with('/')).then(|| first.to_string())
}

/// Cap on the bulk-list payload. Tickets are a few KB each; anything
/// past this is a misconfigured directory, not a ticket list.
const LIST_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Parse `name	<base64>` lines. Bad lines are skipped with a warning
/// naming only the FILENAME — never the contents (Rule #1).
fn parse_list_b64(out: &str) -> Vec<(String, Vec<u8>)> {
    let mut res = Vec::new();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Some((name, b64)) = line.split_once('\t') else {
            log_warn("TICKETS", "skipping a list line with no separator");
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        if b64.is_empty() {
            log_warn(
                "TICKETS",
                &format!("skip {name}: empty payload (no base64 on the remote?)"),
            );
            continue;
        }
        match base64_decode(b64) {
            Ok(bytes) => res.push((name.to_string(), bytes)),
            Err(e) => log_warn("TICKETS", &format!("skip {name}: base64: {e}")),
        }
    }
    res
}

/// Map a `\wsl.localhost\<distro>\...` (or legacy `\wsl$\...`) path back
/// to the Linux path the agent inside the distro sees.
///
/// The two forms must NOT be conflated: `project_path` on a ticket has to
/// be the LINUX path (that is what Claude Code sees), while the store has
/// to be the UNC path (that is what std::fs can open).
fn wsl_linux_from_unc(unc: &Path, distro: &str) -> Option<String> {
    let s = unc.to_string_lossy().replace('/', "\\");
    for prefix in [
        format!("\\\\wsl.localhost\\{distro}\\"),
        format!("\\\\wsl$\\{distro}\\"),
    ] {
        if let Some(rest) = s.strip_prefix(&prefix) {
            return Some(format!("/{}", rest.replace('\\', "/")));
        }
    }
    None
}

/// Rungs 1-5 of the project ladder. No I/O — `git_root` is supplied by
/// the caller because where it comes from is transport-specific (a local
/// walk, a walk over the WSL share, or `git rev-parse` over SSH).
///
/// Returns (project, which rung). The override is rung 1 and is NEVER
/// discarded here; whether it is reachable is a separate question the
/// caller answers with a transport.
fn pick_project(
    override_path: Option<&str>,
    worktree: Option<&str>,
    cwd: Option<&str>,
    git_root: Option<&str>,
) -> (Option<String>, &'static str) {
    let clean = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(o) = override_path.and_then(clean) {
        return (Some(o), "override");
    }
    if let Some(w) = worktree.and_then(clean) {
        return (Some(w), "worktree");
    }
    if let Some(g) = git_root.and_then(clean) {
        return (Some(g), "git");
    }
    if let Some(c) = cwd.and_then(clean) {
        return (Some(c), "cwd");
    }
    (None, "none")
}

/// Walk up from `start` looking for a `.git` entry (file or directory —
/// a worktree's `.git` is a file). Bounded so a pathological path can't
/// spin. Returns the repo root, not the `.git` itself.
fn git_root_of(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    for _ in 0..64 {
        if cur.join(".git").exists() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
    None
}

/// Translate a WSL path to its Windows UNC form so tickets can be
/// written into a WSL project from the Windows side. Only returns a
/// path that actually exists — if the distro isn't running or the share
/// isn't reachable, the caller falls back to app-local storage rather
/// than failing the save.
#[cfg(windows)]
fn wsl_unc_path(distro: Option<&str>, linux_path: &str) -> Option<PathBuf> {
    if !linux_path.starts_with('/') {
        return None;
    }
    // Without an explicit distro we can't build a share name; the
    // default-distro case falls back to app-local.
    let distro = distro?;
    let rel = linux_path.trim_start_matches('/').replace('/', "\\");
    let unc = PathBuf::from(format!("\\\\wsl.localhost\\{distro}\\{rel}"));
    if unc.exists() {
        return Some(unc);
    }
    // Older Windows builds expose the same share as \\wsl$\<distro>.
    let legacy = PathBuf::from(format!("\\\\wsl$\\{distro}\\{rel}"));
    legacy.exists().then_some(legacy)
}

#[cfg(not(windows))]
fn wsl_unc_path(_distro: Option<&str>, _linux_path: &str) -> Option<PathBuf> {
    None
}

/// Reachability probe for a WSL project path, with one retry pass.
///
/// Measured against a live distro rather than assumed. What is actually
/// established:
///   - The share is robust in the normal case: `\\wsl.localhost\<d>\home`
///     resolved even immediately after `wsl --terminate`, because the
///     Windows-side redirector starts the distro on access.
///   - It nevertheless returned false for `\tmp` and `\home` twice during
///     testing while the distro reported Running. The mechanism was NOT
///     pinned down, so this deliberately does not claim one.
///
/// A miss is therefore not trustworthy on its own, and being wrong is
/// expensive: the ticket silently lands in the app-local fallback instead
/// of in the project — the exact behaviour this change exists to remove.
/// So a miss warms the distro and re-probes. Cheap by construction: none
/// of it runs unless the first probe already failed.
/// `local_setup.rs:1016` records the same unease about `\\wsl$` being
/// "flaky on cold distros".
///
/// Unrelated trap, found the hard way while testing this: WSL wipes /tmp
/// whenever the distro stops, and a vanished path is indistinguishable
/// from a dead share. Do not keep anything there you expect to persist.
async fn wsl_unc_reachable(distro: Option<&str>, linux_path: &str) -> Option<PathBuf> {
    if let Some(p) = wsl_unc_path(distro, linux_path) {
        return Some(p);
    }
    log_debug(
        "TICKETS",
        "WSL share missed on first probe — warming the distro and retrying",
    );
    let _ = crate::local_setup::wsl_exec(distro, None, "true").await;
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if let Some(p) = wsl_unc_path(distro, linux_path) {
            return Some(p);
        }
    }
    None
}

/// Derive the project for a workspace. Ladder, most-specific first:
///   1. explicit override (the user pointed us somewhere)
///   2. `git_worktree`
///   3. git root walked up from `cwd` — a pane usually sits in a subdir
///   4. `cwd` itself
///   5. nothing
///
/// Whether we can WRITE there is a separate question, answered by the
/// connection: Local is on this machine, Wsl may be via the UNC share,
/// Ssh never is.
/// Where a workspace's ticket files physically live.
///
/// `Remote` carries a live SSH handle, so it cannot be constructed
/// without an authenticated session — "can we write there" is answered
/// by the type rather than by a later check. That is what killed the old
/// hardcoded `Connection::Ssh => unwritable` gate, and with it the bug
/// where a user's explicit project override was discarded on SSH.
///
/// The handle is the SAME one the browser's port-forward already rides
/// on (`open_auto_forward` resolves it identically). SSH multiplexes, so
/// the SFTP and exec channels here cost no extra connection and no extra
/// authentication.
enum Store {
    /// Reachable with std::fs — Local, and WSL through the
    /// \\wsl.localhost share.
    Native(PathBuf),
    /// A POSIX directory on the workspace's SSH host.
    Remote {
        dir: String,
        handle: Arc<SshHandle<SshClient>>,
    },
}

/// One resolve answers both "where do tickets go" and "can I write".
struct Resolved {
    /// `None` => nothing is writable right now; `view.status` says why.
    store: Option<Store>,
    view: ProjectResolution,
}

/// Resolve is on the capture modal's critical path — a hung `git` on the
/// remote must not hang the UI.
const REMOTE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(8);
/// Ticket reads/writes are small but cross a network.
const REMOTE_IO_TIMEOUT: Duration = Duration::from_secs(30);

async fn with_timeout<T>(
    d: Duration,
    label: &str,
    f: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(d, f).await {
        Ok(r) => r,
        Err(_) => Err(format!("{label} timed out after {}s", d.as_secs())),
    }
}

impl Store {
    async fn ensure_dir(&self) -> Result<(), String> {
        match self {
            Store::Native(p) => ensure_dir(p),
            Store::Remote { dir, handle } => {
                let (out, code) = with_timeout(
                    REMOTE_IO_TIMEOUT,
                    "remote mkdir",
                    remote_exec(handle, &cmd_mkdir_p(dir)),
                )
                .await?;
                if code != 0 {
                    return Err(format!("mkdir -p {dir} failed (exit {code}): {out}"));
                }
                Ok(())
            }
        }
    }

    /// Atomic replace. Rule #7.
    ///
    /// Remotely this is a `.tmp` write followed by `mv -f`, NOT an SFTP
    /// rename: russh-sftp negotiates no posix-rename extension, so its
    /// rename is raw SSH_FXP_RENAME and FAILS when the destination
    /// exists — which is every ticket update. `mv -f` is rename(2), so
    /// a reader never sees a partial file. It does not give durability
    /// across a server power loss; SFTP exposes no fsync we can rely on.
    async fn write_atomic_named(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        match self {
            Store::Native(p) => write_atomic(&p.join(name), bytes),
            Store::Remote { dir, handle } => {
                let dst = posix_join(dir, name);
                let tmp = format!("{dst}.{}.tmp", std::process::id());
                with_timeout(
                    REMOTE_IO_TIMEOUT,
                    "remote write",
                    remote_write_bytes(handle, &tmp, bytes),
                )
                .await?;
                let (out, code) = with_timeout(
                    REMOTE_IO_TIMEOUT,
                    "remote mv",
                    remote_exec(handle, &cmd_mv_into_place(&tmp, &dst)),
                )
                .await?;
                if code != 0 {
                    // Never leave a dead .tmp lying in the user's repo.
                    let _ = remote_exec(handle, &cmd_rm_f(&[tmp])).await;
                    return Err(format!("mv into place failed (exit {code}): {out}"));
                }
                Ok(())
            }
        }
    }

    async fn read_named(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Store::Native(p) => {
                let path = p.join(name);
                if !path.exists() {
                    return Ok(None);
                }
                fs::read(&path)
                    .map(Some)
                    .map_err(|e| format!("read {name}: {e}"))
            }
            Store::Remote { dir, handle } => {
                let path = posix_join(dir, name);
                match with_timeout(
                    REMOTE_IO_TIMEOUT,
                    "remote read",
                    remote_read_bytes(handle, &path),
                )
                .await
                {
                    Ok(b) => Ok(Some(b)),
                    // A missing file is not an error to the caller; SFTP
                    // gives us no clean not-found discriminant, so treat
                    // any read failure of an optional file as absent and
                    // log it (filename only, Rule #1).
                    Err(e) => {
                        log_debug("TICKETS", &format!("remote read {name}: {e}"));
                        Ok(None)
                    }
                }
            }
        }
    }

    /// Every `*.json` in the directory, as (filename, bytes). A missing
    /// directory is an empty list, not an error.
    async fn list_json(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        match self {
            Store::Native(p) => {
                if !p.exists() {
                    return Ok(Vec::new());
                }
                let mut out = Vec::new();
                let iter = fs::read_dir(p).map_err(|e| format!("read_dir {:?}: {e}", p))?;
                for entry in iter {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            log_warn("TICKETS", &format!("skip dir entry: {e}"));
                            continue;
                        }
                    };
                    let path = entry.path();
                    if path.extension().and_then(|x| x.to_str()) != Some("json") {
                        continue;
                    }
                    let name = path
                        .file_name()
                        .and_then(|x| x.to_str())
                        .unwrap_or("<unnamed>")
                        .to_string();
                    match fs::read(&path) {
                        Ok(b) => out.push((name, b)),
                        Err(e) => log_warn("TICKETS", &format!("skip {name}: read: {e}")),
                    }
                }
                Ok(out)
            }
            Store::Remote { dir, handle } => {
                // ONE round trip for the whole listing — see
                // cmd_list_json_b64.
                let (out, code) = with_timeout(
                    REMOTE_IO_TIMEOUT,
                    "remote list",
                    remote_exec(handle, &cmd_list_json_b64(dir)),
                )
                .await?;
                if code != 0 {
                    return Err(format!("remote list failed (exit {code}): {out}"));
                }
                if out.len() > LIST_MAX_BYTES {
                    return Err(format!(
                        "remote ticket listing is {} bytes, over the {LIST_MAX_BYTES} cap",
                        out.len()
                    ));
                }
                Ok(parse_list_b64(&out))
            }
        }
    }

    async fn remove_named(&self, names: &[String]) -> Result<(), String> {
        match self {
            Store::Native(p) => {
                for n in names {
                    let path = p.join(n);
                    if path.exists() {
                        fs::remove_file(&path).map_err(|e| format!("remove {n}: {e}"))?;
                    }
                }
                Ok(())
            }
            Store::Remote { dir, handle } => {
                let paths: Vec<String> = names.iter().map(|n| posix_join(dir, n)).collect();
                let (out, code) = with_timeout(
                    REMOTE_IO_TIMEOUT,
                    "remote rm",
                    remote_exec(handle, &cmd_rm_f(&paths)),
                )
                .await?;
                if code != 0 {
                    return Err(format!("remote rm failed (exit {code}): {out}"));
                }
                Ok(())
            }
        }
    }
}

/// Derive the project for a workspace and pick a transport for it.
///
/// Two independent questions, in order:
///   1. WHICH directory is the project  -> `pick_project` (pure)
///   2. HOW do we reach it              -> the connection
///
/// Keeping them separate is what fixes the override bug: rung 1 is the
/// user's explicit choice and no transport may silently overrule it.
async fn resolve(
    state: &crate::AppState,
    workspace_id: &str,
    override_path: Option<&str>,
) -> Result<Resolved, String> {
    let fallback = fallback_dir(workspace_id)?;
    let fallback_s = fallback.to_string_lossy().to_string();

    // Snapshot under the lock, then drop it — MutexGuard is not Send and
    // everything below this point awaits.
    let (cwd, worktree, conn) = {
        let file = state.workspaces.lock().map_err(|e| e.to_string())?;
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        (
            ws.cwd.clone(),
            ws.git_worktree.clone(),
            ws.connection.clone(),
        )
    };
    let worktree_s = worktree.map(|w| w.to_string_lossy().to_string());
    let ov = override_path.map(str::trim).filter(|x| !x.is_empty());

    // A workspace with nothing to derive from. `transport` must still be
    // truthful — reporting "local" for an SSH workspace made the UI say
    // the wrong thing about where the ticket would have gone.
    let no_project_on = |source: &str, transport: &str, host: &str| Resolved {
        store: None,
        view: ProjectResolution {
            project_path: None,
            tickets_dir: fallback_s.clone(),
            in_project: false,
            source: source.to_string(),
            fallback_reason:
                "no project folder for this workspace — set one below, or cd into the project \
                 in a terminal pane"
                    .to_string(),
            transport: transport.to_string(),
            host_label: host.to_string(),
            writable: false,
            status: "no_project".to_string(),
        },
    };
    let no_project = |source: &str| no_project_on(source, "local", "");

    match conn {
        // ── SSH: the project lives on the host, and so must the tickets.
        Some(ymux_types::Connection::Ssh { ref host, .. }) => {
            let handle = pick_ssh_handle_for_workspace(state, workspace_id);
            // An override for an SSH workspace has to be a remote POSIX
            // path. Saying so beats a baffling remote mkdir error.
            if let Some(o) = ov {
                if !o.starts_with('/') {
                    return Err(format!(
                        "this workspace is on {host}, so the ticket folder must be an absolute \
                         POSIX path on that host (got {o:?})"
                    ));
                }
            }
            let Some(handle) = handle else {
                // Intended destination, computed without touching the
                // network, so the UI can still name it.
                let (project, source) =
                    pick_project(ov, worktree_s.as_deref(), cwd.as_deref(), None);
                return Ok(Resolved {
                    store: None,
                    view: ProjectResolution {
                        tickets_dir: project
                            .as_deref()
                            .and_then(|p| remote_project_dir(p).ok())
                            .unwrap_or_else(|| fallback_s.clone()),
                        project_path: project,
                        in_project: false,
                        source: source.to_string(),
                        fallback_reason: format!("not connected to {host}"),
                        transport: "ssh".to_string(),
                        host_label: host.clone(),
                        writable: false,
                        status: "disconnected".to_string(),
                    },
                });
            };

            // Where to start looking on the host. The workspace's own
            // `cwd` is preferred, but for SSH it is very often EMPTY —
            // the user connects and `cd`s inside the pane, and nothing
            // writes that back to workspaces.json. Observed live on a
            // real workspace: cwd "" and git_worktree "", so the ladder
            // resolved to nothing and the ticket fell into the app-local
            // fallback. Asking tmux where the pane actually is recovers
            // the project the user is genuinely working in.
            let mut start_dir = cwd
                .as_deref()
                .filter(|c| c.starts_with('/'))
                .map(str::to_string);
            let mut pane_cwd_used = false;
            if start_dir.is_none() && ov.is_none() && worktree_s.is_none() {
                if let Some(sess) = tmux_session_for_workspace(state, workspace_id) {
                    if let Ok((out, code)) = with_timeout(
                        REMOTE_RESOLVE_TIMEOUT,
                        "remote tmux pane cwd",
                        remote_exec(&handle, &cmd_tmux_pane_cwd(&sess)),
                    )
                    .await
                    {
                        // Same output shape as git rev-parse: exit code
                        // plus one absolute path line.
                        start_dir = parse_git_root(&out, code);
                        pane_cwd_used = start_dir.is_some();
                        if pane_cwd_used {
                            log_debug(
                                "TICKETS",
                                &format!("ws={workspace_id} using tmux pane cwd as the start dir"),
                            );
                        }
                    }
                }
            }

            // Only ask git when no worktree/override already decided it.
            let git_root = if ov.is_some() || worktree_s.is_some() {
                None
            } else {
                match start_dir.as_deref() {
                    None => None,
                    Some(c) => {
                        match with_timeout(
                            REMOTE_RESOLVE_TIMEOUT,
                            "remote git rev-parse",
                            remote_exec(&handle, &cmd_git_root(c)),
                        )
                        .await
                        {
                            Ok((out, code)) => parse_git_root(&out, code),
                            // A broken channel must not silently resolve
                            // to a DIFFERENT project.
                            Err(e) => {
                                return Ok(Resolved {
                                    store: None,
                                    view: ProjectResolution {
                                        project_path: None,
                                        tickets_dir: fallback_s.clone(),
                                        in_project: false,
                                        source: "none".to_string(),
                                        fallback_reason: format!("{host}: {e}"),
                                        transport: "ssh".to_string(),
                                        host_label: host.clone(),
                                        writable: false,
                                        status: "unreachable".to_string(),
                                    },
                                })
                            }
                        }
                    }
                }
            };

            let (project, source) = pick_project(
                ov,
                worktree_s.as_deref(),
                start_dir.as_deref(),
                git_root.as_deref(),
            );
            let Some(project) = project else {
                return Ok(no_project_on(source, "ssh", host));
            };
            // Name the pane as the source so the UI can explain where the
            // path came from when it did not come from the workspace.
            let source = if source == "cwd" && pane_cwd_used {
                "pane"
            } else {
                source
            };
            let dir = remote_project_dir(&project)?;
            Ok(Resolved {
                view: ProjectResolution {
                    project_path: Some(project),
                    tickets_dir: dir.clone(),
                    in_project: true,
                    source: source.to_string(),
                    fallback_reason: String::new(),
                    transport: "ssh".to_string(),
                    host_label: host.clone(),
                    writable: true,
                    status: "ok".to_string(),
                },
                store: Some(Store::Remote { dir, handle }),
            })
        }

        // ── WSL: reachable from Windows through the UNC share.
        Some(ymux_types::Connection::Wsl { ref distro }) => {
            let label = distro.clone().unwrap_or_default();
            // Translate BEFORE walking. git_root_of runs on the Windows
            // side, and a Linux path never exists there — walking first
            // is why tickets used to land in a SUBDIRECTORY of the repo
            // instead of its root.
            let start_linux = ov.map(str::to_string).or_else(|| {
                worktree_s
                    .clone()
                    .or_else(|| cwd.clone())
                    .filter(|c| c.starts_with('/'))
            });
            let unc = match start_linux.as_deref() {
                Some(lin) => wsl_unc_reachable(distro.as_deref(), lin).await,
                None => None,
            };
            let Some(unc) = unc else {
                let (project, source) =
                    pick_project(ov, worktree_s.as_deref(), cwd.as_deref(), None);
                return Ok(Resolved {
                    store: None,
                    view: ProjectResolution {
                        project_path: project,
                        tickets_dir: fallback_s.clone(),
                        in_project: false,
                        source: source.to_string(),
                        fallback_reason:
                            "the WSL project is not reachable from Windows right now".to_string(),
                        transport: "wsl".to_string(),
                        host_label: label,
                        writable: false,
                        status: "unreachable".to_string(),
                    },
                });
            };
            // Now the walk happens on a path that actually exists.
            let root_unc = git_root_of(&unc).unwrap_or(unc);
            // project_path must be the LINUX path (what the agent inside
            // the distro sees); the store must be the UNC path (what
            // std::fs can open). Conflating them IS the bug.
            let linux = distro
                .as_deref()
                .and_then(|d| wsl_linux_from_unc(&root_unc, d))
                .or(start_linux);
            let source = if ov.is_some() {
                "override"
            } else if worktree_s.is_some() {
                "worktree"
            } else {
                "git"
            };
            Ok(Resolved {
                view: ProjectResolution {
                    project_path: linux,
                    tickets_dir: root_unc.join(PROJECT_DIRNAME).to_string_lossy().to_string(),
                    in_project: true,
                    source: source.to_string(),
                    fallback_reason: String::new(),
                    transport: "wsl".to_string(),
                    host_label: label,
                    writable: true,
                    status: "ok".to_string(),
                },
                store: Some(Store::Native(root_unc.join(PROJECT_DIRNAME))),
            })
        }

        // ── Local, or a workspace with no connection recorded yet.
        _ => {
            let git_root = if ov.is_some() || worktree_s.is_some() {
                None
            } else {
                cwd.as_deref()
                    .and_then(|c| git_root_of(Path::new(c)))
                    .map(|p| p.to_string_lossy().to_string())
            };
            let (project, source) = pick_project(
                ov,
                worktree_s.as_deref(),
                cwd.as_deref(),
                git_root.as_deref(),
            );
            let Some(project) = project else {
                return Ok(no_project(source));
            };
            let root = PathBuf::from(&project);
            if !root.is_dir() {
                return Ok(Resolved {
                    store: None,
                    view: ProjectResolution {
                        project_path: Some(project),
                        tickets_dir: fallback_s.clone(),
                        in_project: false,
                        source: source.to_string(),
                        fallback_reason: "the project folder was not found on disk".to_string(),
                        transport: "local".to_string(),
                        host_label: String::new(),
                        writable: false,
                        status: "unreachable".to_string(),
                    },
                });
            }
            let dir = root.join(PROJECT_DIRNAME);
            Ok(Resolved {
                view: ProjectResolution {
                    project_path: Some(project),
                    tickets_dir: dir.to_string_lossy().to_string(),
                    in_project: true,
                    source: source.to_string(),
                    fallback_reason: String::new(),
                    transport: "local".to_string(),
                    host_label: String::new(),
                    writable: true,
                    status: "ok".to_string(),
                },
                store: Some(Store::Native(dir)),
            })
        }
    }
}

/// The store, or a clear error naming why there isn't one. Used by every
/// mutating command — nothing is ever written to a silent fallback.
fn require_store(r: &Resolved) -> Result<&Store, String> {
    r.store.as_ref().ok_or_else(|| {
        let v = &r.view;
        if v.status == "disconnected" {
            format!(
                "{} — this ticket belongs in {} on that host. Open a terminal pane for this \
                 workspace and try again.",
                v.fallback_reason, v.tickets_dir
            )
        } else {
            v.fallback_reason.clone()
        }
    })
}

fn png_filename_for(id: &str) -> String {
    format!("{id}.png")
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {:?}: {e}", dir))
}

/// Atomic write: `<name>.<pid>.tmp` then rename. Rule #7.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent dir for {:?}", path))?;
    ensure_dir(parent)?;
    let tmp = parent.join(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("ticket"),
        std::process::id()
    ));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open {:?}: {e}", tmp))?;
        f.write_all(bytes)
            .map_err(|e| format!("write {:?}: {e}", tmp))?;
        f.sync_all().map_err(|e| format!("fsync {:?}: {e}", tmp))?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("rename {:?} -> {:?}: {e}", tmp, path))
}

// ─── ids + time ─────────────────────────────────────────────────────

/// nanoid-ish: 6 chars from a URL-safe alphabet, seeded off wall-clock
/// nanos + a process-scoped counter. Not cryptographically random —
/// just needs to be collision-free per day per workspace.
fn short_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let bump = N.fetch_add(1, Ordering::Relaxed);
    let mut x = t.wrapping_mul(1_000_003).wrapping_add(bump);
    const ALPH: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = String::with_capacity(6);
    for _ in 0..6 {
        let idx = (x % ALPH.len() as u64) as usize;
        s.push(ALPH[idx] as char);
        x /= ALPH.len() as u64;
    }
    s
}

/// `ticket-YYYY-MM-DD-<6char>`. Date is derived from unix seconds
/// without pulling `chrono` in for one caller — a tiny gmtime is fine.
fn make_ticket_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = gmdate(secs as i64);
    format!("ticket-{:04}-{:02}-{:02}-{}", y, m, d, short_id())
}

fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d) = gmdate(secs as i64);
    let day_secs = secs % 86_400;
    let h = day_secs / 3600;
    let mi = (day_secs / 60) % 60;
    let s = day_secs % 60;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

/// Days-from-unix-epoch → (year, month, day) via the civil-from-days
/// algorithm (Howard Hinnant, hh_date). Handles all Gregorian dates.
fn gmdate(secs: i64) -> (i32, u32, u32) {
    let days = secs.div_euclid(86_400);
    // Shift epoch to March 1, 0000: this makes the algorithm simple.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

// ─── screenshot decoding ────────────────────────────────────────────

fn decode_data_url_png(data_url: &str) -> Result<Vec<u8>, String> {
    // `data:image/png;base64,AAAA…`
    let prefix = "data:image/png;base64,";
    let b64 = data_url
        .strip_prefix(prefix)
        .ok_or_else(|| "screenshot data-url is not image/png;base64".to_string())?;
    base64_decode(b64)
}

/// Minimal base64 decoder — didn't want to pull in a crate for this.
/// Accepts both the standard (`+/`) and URL-safe (`-_`) alphabets and
/// tolerates missing padding, so the browser bridge can hand us
/// base64url straight out of the page. Shared with
/// `workspace_browser`'s `ymux-ticket:` navigation bridge.
pub(crate) fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn idx(b: u8) -> Result<u32, String> {
        match b {
            b'A'..=b'Z' => Ok(u32::from(b - b'A')),
            b'a'..=b'z' => Ok(u32::from(b - b'a' + 26)),
            b'0'..=b'9' => Ok(u32::from(b - b'0' + 52)),
            b'+' | b'-' => Ok(62),
            b'/' | b'_' => Ok(63),
            _ => Err(format!("bad base64 byte 0x{b:02x}")),
        }
    }
    let clean: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let mut i = 0;
    while i + 4 <= clean.len() {
        let a = idx(clean[i])?;
        let b = idx(clean[i + 1])?;
        let c = idx(clean[i + 2])?;
        let d = idx(clean[i + 3])?;
        let v = (a << 18) | (b << 12) | (c << 6) | d;
        out.push(((v >> 16) & 0xff) as u8);
        out.push(((v >> 8) & 0xff) as u8);
        out.push((v & 0xff) as u8);
        i += 4;
    }
    match clean.len() - i {
        0 => {}
        2 => {
            let a = idx(clean[i])?;
            let b = idx(clean[i + 1])?;
            let v = (a << 18) | (b << 12);
            out.push(((v >> 16) & 0xff) as u8);
        }
        3 => {
            let a = idx(clean[i])?;
            let b = idx(clean[i + 1])?;
            let c = idx(clean[i + 2])?;
            let v = (a << 18) | (b << 12) | (c << 6);
            out.push(((v >> 16) & 0xff) as u8);
            out.push(((v >> 8) & 0xff) as u8);
        }
        _ => return Err("truncated base64".into()),
    }
    Ok(out)
}

/// Standard-alphabet base64 WITH padding — the inverse of
/// `base64_decode`, used to hand a stored PNG back to the webview as a
/// `data:` URL. Kept here for the same reason as the decoder: one
/// caller, not worth a crate.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let v = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPH[(v >> 18) as usize & 63] as char);
        out.push(ALPH[(v >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPH[(v >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPH[v as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}


// ─── Tauri commands ─────────────────────────────────────────────────
//
// Every command takes an optional `project_override`: the user's
// per-workspace escape hatch, kept in localStorage on the frontend so no
// schema moves. It is fed through the same `resolve` ladder as
// everything else — the frontend never picks the directory itself.

/// Where this workspace's tickets go, and why. The capture modal calls
/// this before saving, so the destination is visible up front.
#[tauri::command]
pub async fn tickets_resolve_project(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
) -> Result<ProjectResolution, String> {
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    log_debug(
        "TICKETS",
        &format!(
            "resolve ws={} transport={} status={} src={}",
            workspace_id, r.view.transport, r.view.status, r.view.source
        ),
    );
    Ok(r.view)
}

#[tauri::command]
pub async fn tickets_list(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
) -> Result<Vec<Ticket>, String> {
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    // Listing a workspace we cannot reach is an empty list, not an
    // error: the panel should open and explain itself, not throw.
    let Some(store) = r.store.as_ref() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (name, bytes) in store.list_json().await? {
        match serde_json::from_slice::<Ticket>(&bytes) {
            Ok(t) => out.push(t),
            Err(e) => log_warn("TICKETS", &format!("skip {name}: parse: {e}")),
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    log_debug(
        "TICKETS",
        &format!("list ws={} count={}", workspace_id, out.len()),
    );
    Ok(out)
}

/// Absolute path of the tickets folder, for "reveal" / "copy path".
///
/// The directory is created best-effort: revealing is navigational, and
/// it should not start failing because a remote is momentarily down.
#[tauri::command]
pub async fn tickets_dir_path(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
) -> Result<String, String> {
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    if let Some(store) = r.store.as_ref() {
        if let Err(e) = store.ensure_dir().await {
            log_debug("TICKETS", &format!("ensure_dir (best effort): {e}"));
        }
    }
    Ok(r.view.tickets_dir)
}

#[tauri::command]
pub async fn tickets_create(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
    data: NewTicket,
) -> Result<Ticket, String> {
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    let store = require_store(&r)?;
    store.ensure_dir().await?;
    let id = make_ticket_id();

    let mut screenshot_rel: Option<String> = None;
    if let Some(url) = data.screenshot_data_url.as_deref() {
        match decode_data_url_png(url) {
            Ok(bytes) => {
                let name = png_filename_for(&id);
                store.write_atomic_named(&name, &bytes).await?;
                screenshot_rel = Some(name);
            }
            Err(e) => log_warn("TICKETS", &format!("screenshot decode skipped: {e}")),
        }
    }

    let ticket = Ticket {
        id: id.clone(),
        created: iso8601_now(),
        url: data.url,
        element: data.element,
        screenshot_path: screenshot_rel,
        description: data.description,
        status: default_status(),
        workspace_id: workspace_id.clone(),
        project_path: r.view.project_path.clone(),
        source_hint: None,
    };
    let json = serde_json::to_vec_pretty(&ticket).map_err(|e| format!("serialize ticket: {e}"))?;
    store
        .write_atomic_named(&format!("{id}.json"), &json)
        .await?;

    // Rule #1: routing and lengths, never the captured markup.
    log_info(
        "TICKETS",
        &format!(
            "created id={} ws={} transport={} src={} selector_len={} html_len={} shot={}",
            id,
            workspace_id,
            r.view.transport,
            r.view.source,
            ticket.element.selector.len(),
            ticket.element.html.len(),
            ticket.screenshot_path.is_some()
        ),
    );
    Ok(ticket)
}

/// Hand a stored screenshot back as a `data:image/png;base64,…` URL.
///
/// Not the asset protocol: for a remote workspace the PNG does not live
/// on this machine at all, and routing it through the same Store as the
/// JSON is what makes local and remote behave identically.
/// `Ok(None)` = this ticket has no screenshot.
#[tauri::command]
pub async fn tickets_screenshot(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
    id: String,
) -> Result<Option<String>, String> {
    if !valid_id(&id) {
        return Err(format!("invalid ticket id {id:?}"));
    }
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    let Some(store) = r.store.as_ref() else {
        return Ok(None);
    };
    let Some(bytes) = store.read_named(&png_filename_for(&id)).await? else {
        return Ok(None);
    };
    log_debug(
        "TICKETS",
        &format!("screenshot id={id} ws={workspace_id} bytes={}", bytes.len()),
    );
    Ok(Some(format!(
        "data:image/png;base64,{}",
        base64_encode(&bytes)
    )))
}

#[tauri::command]
pub async fn tickets_update(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
    id: String,
    status: String,
) -> Result<(), String> {
    if status != "open" && status != "resolved" {
        return Err(format!("invalid status {status:?}"));
    }
    if !valid_id(&id) {
        return Err(format!("invalid ticket id {id:?}"));
    }
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    let store = require_store(&r)?;
    let name = format!("{id}.json");
    let bytes = store
        .read_named(&name)
        .await?
        .ok_or_else(|| format!("ticket {id} not found"))?;
    let mut ticket: Ticket =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse ticket {id}: {e}"))?;
    ticket.status = status.clone();
    let json = serde_json::to_vec_pretty(&ticket).map_err(|e| format!("serialize ticket: {e}"))?;
    // NOTE: remotely this replaces an EXISTING file, which is the case
    // SFTP rename cannot do. See Store::write_atomic_named.
    store.write_atomic_named(&name, &json).await?;
    log_info(
        "TICKETS",
        &format!("update id={id} ws={workspace_id} status={status}"),
    );
    Ok(())
}

#[tauri::command]
pub async fn tickets_delete(
    state: tauri::State<'_, crate::AppState>,
    workspace_id: String,
    project_override: Option<String>,
    id: String,
) -> Result<(), String> {
    if !valid_id(&id) {
        return Err(format!("invalid ticket id {id:?}"));
    }
    let r = resolve(&state, &workspace_id, project_override.as_deref()).await?;
    let store = require_store(&r)?;
    // JSON + the sibling screenshot, in one operation.
    store
        .remove_named(&[format!("{id}.json"), png_filename_for(&id)])
        .await?;
    log_info("TICKETS", &format!("delete id={id} ws={workspace_id}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: pick_tmux_session tests pin active-pane preference + determinism.
    fn cands() -> Vec<(String, String)> {
        vec![
            ("s2".into(), "ymux-b".into()),
            ("s1".into(), "ymux-a".into()),
            ("s3".into(), "ymux-c".into()),
        ]
    }

    #[test]
    fn pick_tmux_session_prefers_active_pane_session() {
        // breaking it: ticket follows a non-active pane's directory
        assert_eq!(pick_tmux_session(&cands(), Some("s3")), Some("ymux-c".into()));
    }

    #[test]
    fn pick_tmux_session_follows_active_pane_change() {
        // breaking it: active switch ignored
        assert_eq!(pick_tmux_session(&cands(), Some("s2")), Some("ymux-b".into()));
        assert_eq!(pick_tmux_session(&cands(), Some("s3")), Some("ymux-c".into()));
    }

    #[test]
    fn pick_tmux_session_is_deterministic_without_active() {
        // breaking it: choice depends on map iteration order
        let mut rev = cands();
        rev.reverse();
        assert_eq!(pick_tmux_session(&cands(), None), Some("ymux-a".into()));
        assert_eq!(pick_tmux_session(&rev, None), Some("ymux-a".into()));
        assert_eq!(pick_tmux_session(&cands(), Some("gone")), Some("ymux-a".into()));
    }

    #[test]
    fn pick_tmux_session_single_session_unchanged() {
        // breaking it: single-pane workspaces regress
        let one = vec![("s1".to_string(), "ymux-a".to_string())];
        assert_eq!(pick_tmux_session(&one, None), Some("ymux-a".into()));
        assert_eq!(pick_tmux_session(&one, Some("s1")), Some("ymux-a".into()));
    }

    #[test]
    fn pick_tmux_session_empty_is_none() {
        // breaking it: headless-only workspace yields a bogus session
        assert_eq!(pick_tmux_session(&[], None), None);
        assert_eq!(pick_tmux_session(&[], Some("s1")), None);
    }

    #[test]
    fn base64_roundtrip_small() {
        let decoded = base64_decode("aGVsbG8gd29ybGQ").unwrap();
        assert_eq!(decoded, b"hello world");
    }

    #[test]
    fn base64_encode_round_trips_through_the_decoder() {
        // Covers every padding case (len % 3 == 0, 1, 2) plus binary
        // bytes, since this carries PNG data.
        for raw in [
            &b""[..],
            &b"a"[..],
            &b"ab"[..],
            &b"abc"[..],
            &b"abcd"[..],
            &[0u8, 255, 128, 1, 2, 3][..],
        ] {
            let enc = base64_encode(raw);
            assert_eq!(enc.len() % 4, 0, "padded output must be 4-aligned");
            let back = base64_decode(&enc).expect("decodes");
            assert_eq!(back, raw, "round trip failed for {raw:?}");
        }
    }

    #[test]
    fn base64_encode_matches_a_known_vector() {
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
        assert_eq!(base64_encode(b"hi"), "aGk=");
    }

    #[test]
    fn short_id_is_six_chars() {
        let s = short_id();
        assert_eq!(s.len(), 6);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn ticket_id_shape() {
        let id = make_ticket_id();
        assert!(id.starts_with("ticket-"));
        assert!(valid_id(&id));
    }

    #[test]
    fn valid_id_rejects_traversal() {
        assert!(!valid_id("../foo"));
        assert!(!valid_id("hello/world"));
        assert!(!valid_id(""));
        // ticket ids never contain underscores
        assert!(!valid_id("w_1a2b"));
    }

    #[test]
    fn valid_ws_id_accepts_generated_shape_but_not_traversal() {
        assert!(valid_ws_id("w_18f3a9c2d"));
        assert!(!valid_ws_id("../../etc"));
        assert!(!valid_ws_id("a/b"));
        assert!(!valid_ws_id("a.b"));
        assert!(!valid_ws_id(""));
    }

    #[test]
    fn git_root_walks_up_from_a_subdir() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("app").join("src").join("components");
        fs::create_dir_all(&deep).unwrap();
        // A pane sitting three levels in must still resolve to the repo
        // root — this is the whole reason we walk instead of using cwd.
        assert_eq!(git_root_of(&deep).as_deref(), Some(root));
        assert_eq!(git_root_of(root).as_deref(), Some(root));
    }

    #[test]
    fn git_root_finds_worktree_dot_git_file() {
        // In a linked worktree `.git` is a FILE, not a directory.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path();
        fs::write(root.join(".git"), "gitdir: /somewhere/.git/worktrees/x").unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(git_root_of(&sub).as_deref(), Some(root));
    }

    #[test]
    fn git_root_is_none_outside_a_repo() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let deep = tmp.path().join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        // tempdir lives under the system temp root, which is not a repo.
        assert!(git_root_of(&deep).is_none());
    }

    #[test]
    fn wsl_unc_rejects_relative_and_missing_distro() {
        // Non-absolute linux path is never translatable.
        assert!(wsl_unc_path(Some("Ubuntu"), "home/u/p").is_none());
        // Default distro (None) has no share name to build.
        assert!(wsl_unc_path(None, "/home/u/p").is_none());
        // A distro that cannot exist resolves to a path that does not
        // exist, so translation declines rather than handing back a
        // bogus destination.
        assert!(wsl_unc_path(Some("no-such-distro-zzz"), "/home/u/p").is_none());
    }

    // ─── Rule #3 regression guard ───────────────────────────────────
    //
    // Every remote command interpolates a caller-controlled path. If a
    // metacharacter ever escapes the quoting, these fail. This is the
    // highest-value test in the module.

    /// Paths that would be catastrophic if the quoting broke.
    fn nasty_paths() -> Vec<String> {
        vec![
            "/home/u/my project".to_string(),
            "/home/u/it's".to_string(),
            "/home/u/x; rm -rf /".to_string(),
            "/home/u/$(id)".to_string(),
            "/home/u/`id`".to_string(),
            "/home/u/x&&whoami".to_string(),
            "/home/u/x|tee /tmp/pwn".to_string(),
            "/home/u/new\nline".to_string(),
            "/home/u/$HOME".to_string(),
            "/home/u/*".to_string(),
        ]
    }

    /// Everything after the first quote must live inside single quotes,
    /// with the only escape being the '"'"'\''"'"' idiom shell_quote emits.
    fn assert_neutralized(cmd: &str, raw: &str) {
        let quoted = ymux_core::shell_quote(raw);
        assert!(
            cmd.contains(&quoted),
            "path was not shell-quoted into the command.\n  cmd={cmd}\n  want={quoted}"
        );
        // The raw form must never appear unquoted next to a metachar.
        for meta in [";", "&&", "|", "$(", "`"] {
            if raw.contains(meta) {
                let bare = format!(" {raw}");
                assert!(
                    !cmd.contains(&bare),
                    "raw path with {meta:?} leaked into {cmd}"
                );
            }
        }
    }

    #[test]
    fn cmd_builders_quote_every_hostile_path() {
        for raw in nasty_paths() {
            assert_neutralized(&cmd_git_root(&raw), &raw);
            assert_neutralized(&cmd_tmux_pane_cwd(&raw), &raw);
            assert_neutralized(&cmd_mkdir_p(&raw), &raw);
            assert_neutralized(&cmd_list_json_b64(&raw), &raw);
            assert_neutralized(&cmd_rm_f(&[raw.clone()]), &raw);
            let dst = format!("{raw}/t.json");
            let mv = cmd_mv_into_place(&format!("{dst}.tmp"), &dst);
            assert_neutralized(&mv, &dst);
        }
    }

    #[test]
    fn cmd_rm_f_quotes_each_path_separately() {
        let cmd = cmd_rm_f(&["/a/x.json".to_string(), "/a/x; reboot.png".to_string()]);
        assert!(cmd.starts_with("rm -f "));
        assert!(cmd.contains(&ymux_core::shell_quote("/a/x.json")));
        assert!(cmd.contains(&ymux_core::shell_quote("/a/x; reboot.png")));
        assert!(!cmd.contains("; reboot.png'") || cmd.matches("'").count() >= 4);
    }

    #[test]
    fn cmd_tmux_pane_cwd_asks_for_the_pane_path() {
        let cmd = cmd_tmux_pane_cwd("ymux-p_abc_1");
        // The tmux format literal must survive Rust's brace escaping.
        assert!(
            cmd.contains("'#{pane_current_path}'"),
            "tmux format was mangled: {cmd}"
        );
        assert!(cmd.contains("display-message -p -t"));
        assert!(cmd.contains(&ymux_core::shell_quote("ymux-p_abc_1")));
    }

    #[test]
    fn cmd_list_uses_portable_base64() {
        let cmd = cmd_list_json_b64("/p/.ymux-tickets");
        // -w0 is GNU-only; BSD/macOS remotes need the tr form.
        assert!(!cmd.contains("-w0"), "must not depend on GNU base64");
        assert!(cmd.contains("tr -d"));
        // A missing directory must be a clean exit, not an error.
        assert!(cmd.contains("|| exit 0"));
    }

    // ─── parse_git_root ─────────────────────────────────────────────

    #[test]
    fn parse_git_root_accepts_a_clean_path() {
        assert_eq!(
            parse_git_root("/home/u/proj\n", 0).as_deref(),
            Some("/home/u/proj")
        );
    }

    #[test]
    fn parse_git_root_tolerates_crlf() {
        assert_eq!(
            parse_git_root("/home/u/proj\r\n", 0).as_deref(),
            Some("/home/u/proj")
        );
    }

    #[test]
    fn parse_git_root_rejects_failure_and_junk() {
        // not a repo
        assert!(parse_git_root("fatal: not a git repository", 128).is_none());
        // git missing -> shell reports 127
        assert!(parse_git_root("", 127).is_none());
        // success but empty
        assert!(parse_git_root("", 0).is_none());
        // success but not an absolute POSIX path (never trust it)
        assert!(parse_git_root("C:/proj", 0).is_none());
        assert!(parse_git_root("relative/path", 0).is_none());
    }

    // ─── parse_list_b64 ─────────────────────────────────────────────

    fn b64u(bytes: &[u8]) -> String {
        const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPH[(v >> 18) as usize & 63] as char);
            out.push(ALPH[(v >> 12) as usize & 63] as char);
            if chunk.len() > 1 { out.push(ALPH[(v >> 6) as usize & 63] as char); }
            if chunk.len() > 2 { out.push(ALPH[v as usize & 63] as char); }
        }
        out
    }

    #[test]
    fn parse_list_b64_reads_a_good_line() {
        let payload = b"{\"id\":\"ticket-1\"}";
        let line = format!("ticket-1.json\t{}", b64u(payload));
        let got = parse_list_b64(&line);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "ticket-1.json");
        assert_eq!(got[0].1, payload);
    }

    #[test]
    fn parse_list_b64_skips_malformed_lines_without_failing_the_batch() {
        let good = format!("ok.json\t{}", b64u(b"{}"));
        let out = [
            "no-separator-here",           // no tab
            "notes.txt\tYWJj",             // not .json
            "empty.json\t",                // no base64 binary on the remote
            "bad.json\t!!!!not-base64!!!!", // garbage
            &good,
            "",                            // blank
        ]
        .join("\n");
        let got = parse_list_b64(&out);
        // One bad file must not lose the rest of the list.
        assert_eq!(got.len(), 1, "only the good line should survive");
        assert_eq!(got[0].0, "ok.json");
    }

    #[test]
    fn parse_list_b64_tolerates_crlf_from_the_remote() {
        let line = format!("a.json\t{}\r", b64u(b"{}"));
        assert_eq!(parse_list_b64(&line).len(), 1);
    }

    // ─── posix paths ────────────────────────────────────────────────

    #[test]
    fn remote_project_dir_requires_an_absolute_posix_path() {
        assert_eq!(
            remote_project_dir("/home/u/proj").unwrap(),
            "/home/u/proj/.ymux-tickets"
        );
        // trailing slash must not double up
        assert_eq!(
            remote_project_dir("/home/u/proj/").unwrap(),
            "/home/u/proj/.ymux-tickets"
        );
        assert!(remote_project_dir("proj").is_err());
        // A Windows path on an SSH workspace is a user mistake worth naming.
        assert!(remote_project_dir("C:\\proj").is_err());
    }

    #[test]
    fn posix_join_does_not_double_the_separator() {
        assert_eq!(posix_join("/a/b", "c.json"), "/a/b/c.json");
        assert_eq!(posix_join("/a/b/", "c.json"), "/a/b/c.json");
    }

    // ─── WSL path translation ───────────────────────────────────────

    /// End-to-end over a REAL WSL distro, not a mock: translate a Linux
    /// path to UNC, walk up to the repo root the way `resolve` does, and
    /// map that root back to the Linux path the agent inside the distro
    /// sees. This is the exact chain that made tickets land in a
    /// SUBDIRECTORY instead of the repo root.
    ///
    /// Uses `wsl_unc_reachable`, not the bare `wsl_unc_path`, because
    /// that is what `resolve` calls — and because a bare probe fails on a
    /// cold distro, which is how the warm-and-retry got written in the
    /// first place. Running this immediately after the distro has gone
    /// idle is the interesting case.
    ///
    /// Ignored by default — needs a distro named Ubuntu. Set up with:
    ///   wsl -d Ubuntu -- bash -lc 'mkdir -p ~/wmx-tk/app/src/components
    ///     && cd ~/wmx-tk && git init -q'
    /// then: cargo test --lib wsl_live -- --ignored --nocapture
    /// The fixture lives under $HOME, not /tmp: WSL wipes /tmp whenever
    /// the distro stops, which it does constantly, and a vanished
    /// fixture looks exactly like a broken share.
    #[test]
    #[ignore = "needs a live WSL distro; see the doc comment"]
    #[cfg(windows)]
    fn wsl_live_subdir_resolves_to_the_repo_root() {
        const DISTRO: &str = "Ubuntu";
        const SUBDIR: &str = "/home/mlastudent371/wmx-tk/app/src/components";
        const ROOT: &str = "/home/mlastudent371/wmx-tk";

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let unc = rt
            .block_on(wsl_unc_reachable(Some(DISTRO), SUBDIR))
            .expect("the WSL share must resolve the subdir after warming");
        println!("subdir UNC = {}", unc.display());

        // The fix: walk the TRANSLATED path. Walking the Linux path on
        // the Windows side always missed, which is why the ticket used to
        // be written into the subdirectory.
        let root_unc = git_root_of(&unc).expect("walking up must find the repo root");
        println!("root   UNC = {}", root_unc.display());
        assert!(
            root_unc.join(".git").exists(),
            "the found root must actually contain .git"
        );
        assert_ne!(root_unc, unc, "must climb ABOVE the subdir");

        // project_path has to be the LINUX path — that is what Claude
        // Code on the other side of the share sees.
        let linux = wsl_linux_from_unc(&root_unc, DISTRO)
            .expect("the repo root must map back to a Linux path");
        println!("root Linux = {linux}");
        assert_eq!(linux, ROOT);
    }

    #[test]
    fn wsl_unc_and_linux_round_trip() {
        for prefix in ["wsl.localhost", "wsl$"] {
            let unc = std::path::PathBuf::from(format!(
                "\\\\{prefix}\\Ubuntu\\home\\u\\proj"
            ));
            assert_eq!(
                wsl_linux_from_unc(&unc, "Ubuntu").as_deref(),
                Some("/home/u/proj"),
                "{prefix} form must map back to the Linux path"
            );
        }
    }

    #[test]
    fn wsl_linux_from_unc_rejects_another_distro() {
        let unc = std::path::PathBuf::from(format!(
            "\\\\wsl.localhost\\Debian\\home\\u"
        ));
        assert!(wsl_linux_from_unc(&unc, "Ubuntu").is_none());
    }

    // ─── the ladder (bug #1 lives here) ─────────────────────────────

    #[test]
    fn pick_project_prefers_the_override_above_everything() {
        // THE regression guard for the bug where an override was silently
        // discarded on SSH workspaces.
        let (p, src) = pick_project(
            Some("/srv/chosen"),
            Some("/srv/worktree"),
            Some("/srv/cwd"),
            Some("/srv/gitroot"),
        );
        assert_eq!(p.as_deref(), Some("/srv/chosen"));
        assert_eq!(src, "override");
    }

    #[test]
    fn pick_project_ladder_order() {
        let cases: Vec<(Option<&str>, Option<&str>, Option<&str>, Option<&str>, &str, &str)> = vec![
            (None, Some("/w"), Some("/c"), Some("/g"), "/w", "worktree"),
            (None, None, Some("/c"), Some("/g"), "/g", "git"),
            (None, None, Some("/c"), None, "/c", "cwd"),
        ];
        for (o, w, c, g, want, want_src) in cases {
            let (p, src) = pick_project(o, w, c, g);
            assert_eq!(p.as_deref(), Some(want));
            assert_eq!(src, want_src);
        }
        let (p, src) = pick_project(None, None, None, None);
        assert!(p.is_none());
        assert_eq!(src, "none");
    }

    #[test]
    fn pick_project_treats_blank_strings_as_absent() {
        // workspace_update writes "" to clear cwd, so empties must not
        // win a rung and produce a path of "".
        let (p, src) = pick_project(Some("  "), Some(""), Some("/c"), None);
        assert_eq!(p.as_deref(), Some("/c"));
        assert_eq!(src, "cwd");
    }

    #[test]
    fn gmdate_matches_known() {
        // 2026-07-13 00:00:00 UTC
        let (y, m, d) = gmdate(1_783_900_800);
        assert_eq!((y, m, d), (2026, 7, 13));
    }
}
