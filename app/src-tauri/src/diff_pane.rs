// Phase 50 / 91.F: live diff pane.
//
// One background tokio task per mounted Diff pane. Each tick fetches a
// BUNDLE — `git status` (branch + changed files incl. untracked), `git
// diff` against the pane's source, and a `git diff --no-index` for each
// untracked file — and emits `diff-pane-updated` when the hash changes.
//
// Phase 91.F changed three things:
//   1. Git runs through `worktrees::run_git_raw` / `exec_script_over`, so
//      the pane works on SSH and WSL workspaces, not only Local. The old
//      `cwd.join(".git").exists()` pre-check is gone — `git -C` finds the
//      repo root itself and git's own "not a git repository" message
//      reaches the UI verbatim.
//   2. The default source is HEAD (staged + unstaged) and untracked files
//      show up, so "no changes" means no changes.
//   3. A `diff_cwd` override lets the worktree strip point the pane at a
//      sibling worktree without touching the workspace's own cwd.
//
// The watcher runs only while the DiffPane component is mounted
// (`diff_pane_start` / `diff_pane_stop`); it self-terminates if the pane
// leaves the layout.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::worktrees::{
    exec_script_over, git_error, parse_worktree_porcelain, run_git_local_raw, run_git_raw,
    WorktreeEntry,
};
use crate::{log_debug, AppState, Connection, DiffSource, LayoutNode};

const POLL_LOCAL_MS: u64 = 1000;
const POLL_REMOTE_MS: u64 = 3000;
/// Cap the per-tick untracked-file diffs: a repo with thousands of
/// untracked files (unignored build output) must not spawn thousands of
/// `--no-index` runs.
const UNTRACKED_CAP: usize = 40;
/// Truncate a single untracked file's diff (a 50 MB blob is not worth
/// streaming into the pane).
const UNTRACKED_BYTES_CAP: usize = 262_144;
/// Truncate the whole assembled diff so a giant change never floods IPC
/// or the DOM.
const DIFF_TEXT_CAP: usize = 2_000_000;

/// One `git status --porcelain=v1` record. `xy` is the two-letter status
/// (`" M"`, `"A "`, `"??"`, `"R "`…); `orig_path` is the pre-rename path
/// for an `R`/`C` entry. Serialized snake_case for the frontend.
#[derive(Clone, Serialize, PartialEq, Debug)]
pub(crate) struct StatusEntry {
    pub xy: String,
    pub path: String,
    pub orig_path: Option<String>,
}

#[derive(Clone, Serialize)]
struct DiffPaneUpdatedEvent {
    pane_id: String,
    diff_text: String,
    files: Vec<StatusEntry>,
    /// git's own message, verbatim, when the fetch failed. When set,
    /// `diff_text`/`files` are empty and the FE shows this instead.
    error: Option<String>,
    /// The context actually used (the `diff_cwd` override or the ws cwd).
    cwd: String,
    /// Current branch (`None` when detached or on an unborn HEAD).
    branch: Option<String>,
    truncated: bool,
}

struct PaneCtx {
    conn: Option<Connection>,
    cwd: Option<String>,
    source: DiffSource,
}

// ─── pure helpers (unit-tested) ──────────────────────────────────────

/// `-c` overrides prepended to every git invocation: stable, ASCII,
/// prefix-free output regardless of the user's gitconfig. Global `-c`
/// is legal between `--no-pager` and the subcommand.
fn git_global_args() -> [&'static str; 8] {
    [
        "-c",
        "core.quotepath=false",
        "-c",
        "color.ui=never",
        "-c",
        "diff.noprefix=false",
        "-c",
        "diff.mnemonicPrefix=false",
    ]
}

/// A ref must be a ref, not an option. `git diff --output=/etc/x` writes a
/// file; rejecting a leading `-` (and control chars) before git sees it
/// closes that hole. The caller always appends `--` as well.
fn validate_ref(git_ref: &str) -> Result<(), String> {
    let r = git_ref.trim();
    if r.is_empty() {
        return Err("empty ref".to_string());
    }
    if r.starts_with('-') {
        return Err(format!("refusing a ref that looks like an option: {r}"));
    }
    if r.chars().any(|c| c.is_control()) {
        return Err("ref contains a control character".to_string());
    }
    Ok(())
}

/// The `diff` argument vector for a source (without the global `-c` block).
fn diff_args(source: &DiffSource) -> Vec<String> {
    let mut v = vec![
        "diff".to_string(),
        "--no-color".to_string(),
        "--no-ext-diff".to_string(),
    ];
    match source {
        DiffSource::Working => {}
        DiffSource::Head => v.push("HEAD".to_string()),
        DiffSource::Ref { git_ref } => v.push(git_ref.trim().to_string()),
    }
    v.push("--".to_string());
    v
}

/// Parse `git status --porcelain=v1 -z` output. Records are NUL- OR
/// newline-separated (the WSL/SSH bundle runs `tr '\0' '\n'` because the
/// WSL transport strips NULs). Returns the branch from the `## …` header
/// and one `StatusEntry` per change; an `R`/`C` entry consumes the next
/// record as its `orig_path`.
fn parse_status_z(text: &str) -> (Option<String>, Vec<StatusEntry>) {
    let mut branch: Option<String> = None;
    let mut files: Vec<StatusEntry> = Vec::new();
    let records: Vec<&str> = text
        .split(|c| c == '\0' || c == '\n')
        .filter(|r| !r.is_empty())
        .collect();
    let mut i = 0;
    while i < records.len() {
        let rec = records[i];
        if let Some(rest) = rec.strip_prefix("## ") {
            branch = parse_branch_header(rest);
            i += 1;
            continue;
        }
        // A porcelain-v1 record is `XY<space><path>` — two status chars,
        // one space, then the path.
        if rec.len() < 4 {
            i += 1;
            continue;
        }
        let xy = rec[..2].to_string();
        let path = rec[3..].to_string();
        let mut orig_path = None;
        let is_move = xy.starts_with('R') || xy.starts_with('C');
        if is_move && i + 1 < records.len() {
            orig_path = Some(records[i + 1].to_string());
            i += 2;
        } else {
            i += 1;
        }
        files.push(StatusEntry { xy, path, orig_path });
    }
    (branch, files)
}

/// The branch out of a `## …` header line (the `## ` already stripped).
/// `HEAD (no branch)` (detached) → None; `No commits yet on X` (unborn)
/// → X; otherwise the name before `...upstream` or ` [ahead …]`.
fn parse_branch_header(rest: &str) -> Option<String> {
    if rest.starts_with("HEAD (no branch)") {
        return None;
    }
    if let Some(x) = rest.strip_prefix("No commits yet on ") {
        let name = x.trim();
        return if name.is_empty() { None } else { Some(name.to_string()) };
    }
    let end = rest.find("...").unwrap_or_else(|| rest.find(" [").unwrap_or(rest.len()));
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn untracked_paths(files: &[StatusEntry], cap: usize) -> Vec<String> {
    files
        .iter()
        .filter(|e| e.xy == "??")
        .take(cap)
        .map(|e| e.path.clone())
        .collect()
}

/// The one-round-trip `sh` script for WSL/SSH. Only `cwd`, the diff
/// source and the marker are interpolated, each shell-quoted; untracked
/// paths are read by the remote shell from git's own output and never
/// touch this string (Rule #3).
fn bundle_script(cwd: &str, source: &DiffSource, marker: &str) -> String {
    use ymux_core::shell_quote;
    let q = shell_quote(cwd);
    let m = shell_quote(marker);
    // The diff source tokens, shell-quoted (the ref is user input).
    let src = match source {
        DiffSource::Working => String::new(),
        DiffSource::Head => "HEAD".to_string(),
        DiffSource::Ref { git_ref } => shell_quote(git_ref.trim()),
    };
    let cap = UNTRACKED_CAP;
    let bytes = UNTRACKED_BYTES_CAP;
    format!(
        r#"top=$(git -C {q} --no-pager -c core.quotepath=false rev-parse --show-toplevel 2>&1) || {{ printf '%s\n' "$top"; exit 128; }}
printf '%s status\n' {m}
git -C "$top" --no-pager -c core.quotepath=false status --porcelain=v1 -z --branch --untracked-files=all 2>&1 | tr '\0' '\n'
printf '\n%s diff\n' {m}
git -C "$top" --no-pager -c core.quotepath=false -c color.ui=never diff --no-color --no-ext-diff {src} -- 2>&1
git -C "$top" --no-pager -c core.quotepath=false status --porcelain=v1 -z --untracked-files=all 2>/dev/null | tr '\0' '\n' | {{ n=0; while IFS= read -r l; do case "$l" in '?? '*) n=$((n+1)); [ "$n" -gt {cap} ] && continue; p=${{l#'?? '}}; printf '\n%s untracked\n' {m}; git -C "$top" --no-pager -c core.quotepath=false diff --no-color --no-ext-diff --no-index -- /dev/null "$p" 2>/dev/null | head -c {bytes};; esac; done; }}
printf '\n%s end\n' {m}
"#
    )
}

#[derive(Debug)]
struct Bundle {
    status_text: String,
    diff_text: String,
}

/// Split the bundle stdout by its marker lines. A missing `end` marker
/// means git failed before the script reached it (e.g. `rev-parse`
/// exiting 128) — the whole text is the error.
fn split_bundle(marker: &str, text: &str) -> Result<Bundle, String> {
    let status_tag = format!("{marker} status");
    let diff_tag = format!("{marker} diff");
    let untracked_tag = format!("{marker} untracked");
    let end_tag = format!("{marker} end");
    #[derive(PartialEq)]
    enum Sec {
        None,
        Status,
        Diff,
    }
    let mut sec = Sec::None;
    let mut status = String::new();
    let mut diff = String::new();
    let mut saw_end = false;
    for line in text.split('\n') {
        if line == status_tag {
            sec = Sec::Status;
            continue;
        }
        if line == diff_tag || line == untracked_tag {
            sec = Sec::Diff;
            continue;
        }
        if line == end_tag {
            saw_end = true;
            break;
        }
        match sec {
            Sec::Status => {
                status.push_str(line);
                status.push('\n');
            }
            Sec::Diff => {
                diff.push_str(line);
                diff.push('\n');
            }
            Sec::None => {}
        }
    }
    if !saw_end {
        return Err(git_error(text));
    }
    Ok(Bundle {
        status_text: status,
        diff_text: diff,
    })
}

fn hash_bundle(diff_text: &str, files: &[StatusEntry], branch: &Option<String>) -> u64 {
    let mut h = DefaultHasher::new();
    diff_text.hash(&mut h);
    for f in files {
        f.xy.hash(&mut h);
        f.path.hash(&mut h);
        f.orig_path.hash(&mut h);
    }
    branch.hash(&mut h);
    h.finish()
}

fn cap_diff(mut text: String) -> (String, bool) {
    if text.len() <= DIFF_TEXT_CAP {
        return (text, false);
    }
    text.truncate(DIFF_TEXT_CAP);
    text.push_str("\n… ymux: output truncated\n");
    (text, true)
}

// ─── fetch ───────────────────────────────────────────────────────────

struct Fetched {
    diff_text: String,
    files: Vec<StatusEntry>,
    branch: Option<String>,
    truncated: bool,
}

async fn fetch_bundle(state: &AppState, ctx: &PaneCtx) -> Result<Fetched, String> {
    let cwd = ctx
        .cwd
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| "this workspace has no project directory".to_string())?;
    let is_remote = matches!(
        ctx.conn,
        Some(Connection::Ssh { .. }) | Some(Connection::Wsl { .. })
    );
    if is_remote {
        let marker = format!(
            "__YMUX_DIFF_{:016x}__",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        );
        let script = bundle_script(&cwd, &ctx.source, &marker);
        let (_code, text) = exec_script_over(state, &ctx.conn, &script, 20).await?;
        let bundle = split_bundle(&marker, &text)?;
        let (branch, files) = parse_status_z(&bundle.status_text);
        let (diff_text, truncated) = cap_diff(bundle.diff_text);
        Ok(Fetched { diff_text, files, branch, truncated })
    } else {
        // Local has no script transport — sequential calls, all state-free
        // (`run_git_local_raw` never touches AppState), which is what lets
        // the integration test run without one.
        fetch_bundle_local(&cwd, &ctx.source).await
    }
}

async fn fetch_bundle_local(cwd: &str, source: &DiffSource) -> Result<Fetched, String> {
    // Repo root first, so untracked paths and `--no-index` resolve the
    // same way whether the pane's cwd is the root or a subdirectory.
    let rev_args: Vec<&str> = git_global_args()
        .iter()
        .copied()
        .chain(["rev-parse", "--show-toplevel"])
        .collect();
    let r = run_git_local_raw(cwd, &rev_args).await?;
    if r.code != 0 {
        return Err(git_error(if r.err.is_empty() { &r.out } else { &r.err }));
    }
    let top = r.out.trim().to_string();
    if top.is_empty() {
        return Err("not a git repository".to_string());
    }

    // status
    let status_args: Vec<&str> = git_global_args()
        .iter()
        .copied()
        .chain([
            "status",
            "--porcelain=v1",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])
        .collect();
    let sr = run_git_local_raw(&top, &status_args).await?;
    if sr.code != 0 {
        return Err(git_error(if sr.err.is_empty() { &sr.out } else { &sr.err }));
    }
    let (branch, files) = parse_status_z(&sr.out);

    // diff
    let da = diff_args(source);
    let diff_args_ref: Vec<&str> = git_global_args()
        .iter()
        .copied()
        .chain(da.iter().map(|s| s.as_str()))
        .collect();
    let dr = run_git_local_raw(&top, &diff_args_ref).await?;
    // `git diff` exits 0 normally; a real failure (bad ref) is non-zero
    // with a message on stderr.
    if dr.code != 0 && !dr.err.trim().is_empty() {
        return Err(git_error(&dr.err));
    }
    let mut diff_text = dr.out;

    // untracked, one --no-index per file (exit 0/1 both fine)
    for path in untracked_paths(&files, UNTRACKED_CAP) {
        let ua: Vec<&str> = git_global_args()
            .iter()
            .copied()
            .chain([
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-index",
                "--",
                "/dev/null",
                path.as_str(),
            ])
            .collect();
        if let Ok(ur) = run_git_local_raw(&top, &ua).await {
            let mut chunk = ur.out;
            if chunk.len() > UNTRACKED_BYTES_CAP {
                chunk.truncate(UNTRACKED_BYTES_CAP);
            }
            diff_text.push_str(&chunk);
        }
    }

    let (diff_text, truncated) = cap_diff(diff_text);
    Ok(Fetched { diff_text, files, branch, truncated })
}

// ─── layout lookup ───────────────────────────────────────────────────

fn lookup_pane_context(state: &AppState, pane_id: &str) -> Result<Option<PaneCtx>, String> {
    let file = state
        .workspaces
        .lock()
        .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
    for ws in &file.workspaces {
        let Some(layout) = ws.layout.as_ref() else {
            continue;
        };
        if let Some((source, diff_cwd)) = find_diff_pane(layout, pane_id) {
            let cwd = diff_cwd.or_else(|| ws.cwd.clone());
            return Ok(Some(PaneCtx {
                conn: ws.connection.clone(),
                cwd,
                source,
            }));
        }
    }
    Ok(None)
}

fn find_diff_pane(node: &LayoutNode, target: &str) -> Option<(DiffSource, Option<String>)> {
    match node {
        LayoutNode::Pane {
            pane_id,
            diff_source,
            diff_cwd,
            ..
        } if pane_id == target => {
            Some((diff_source.clone().unwrap_or_default(), diff_cwd.clone()))
        }
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_diff_pane(first, target).or_else(|| find_diff_pane(second, target))
        }
    }
}

fn set_diff_source_in_layout(node: &mut LayoutNode, target: &str, src: DiffSource) -> bool {
    match node {
        LayoutNode::Pane { pane_id, diff_source, .. } if pane_id == target => {
            *diff_source = Some(src);
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            set_diff_source_in_layout(first, target, src.clone())
                || set_diff_source_in_layout(second, target, src)
        }
    }
}

fn set_diff_cwd_in_layout(node: &mut LayoutNode, target: &str, cwd: Option<String>) -> bool {
    match node {
        LayoutNode::Pane { pane_id, diff_cwd, .. } if pane_id == target => {
            *diff_cwd = cwd;
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            set_diff_cwd_in_layout(first, target, cwd.clone())
                || set_diff_cwd_in_layout(second, target, cwd)
        }
    }
}

// ─── watcher ─────────────────────────────────────────────────────────

async fn emit_once(app: &AppHandle, state: &AppState, pane_id: &str) {
    let ctx = match lookup_pane_context(state, pane_id) {
        Ok(Some(c)) => c,
        _ => return,
    };
    let cwd_str = ctx.cwd.clone().unwrap_or_default();
    match fetch_bundle(state, &ctx).await {
        Ok(f) => {
            let _ = app.emit(
                "diff-pane-updated",
                DiffPaneUpdatedEvent {
                    pane_id: pane_id.to_string(),
                    diff_text: f.diff_text,
                    files: f.files,
                    error: None,
                    cwd: cwd_str,
                    branch: f.branch,
                    truncated: f.truncated,
                },
            );
        }
        Err(e) => {
            let _ = app.emit(
                "diff-pane-updated",
                DiffPaneUpdatedEvent {
                    pane_id: pane_id.to_string(),
                    diff_text: String::new(),
                    files: Vec::new(),
                    error: Some(e),
                    cwd: cwd_str,
                    branch: None,
                    truncated: false,
                },
            );
        }
    }
}

pub(crate) fn start_watcher(app: AppHandle, state: AppState, pane_id: String) {
    stop_watcher_inner(&state, &pane_id);
    let app2 = app.clone();
    let pid = pane_id.clone();
    let state_for_task = state.clone();
    let handle = tokio::spawn(async move {
        let state = state_for_task;
        let mut last_hash: Option<u64> = None;
        let mut last_error: Option<String> = None;
        loop {
            let ctx = match lookup_pane_context(&state, &pid) {
                Ok(Some(c)) => c,
                Ok(None) => {
                    log_debug("DIFF", &format!("[diff_pane] watcher exiting: pane {pid} gone"));
                    break;
                }
                Err(e) => {
                    log_debug("DIFF", &format!("[diff_pane] watcher exiting: {e}"));
                    break;
                }
            };
            let is_remote = matches!(
                ctx.conn,
                Some(Connection::Ssh { .. }) | Some(Connection::Wsl { .. })
            );
            let cwd_str = ctx.cwd.clone().unwrap_or_default();
            match fetch_bundle(&state, &ctx).await {
                Ok(f) => {
                    last_error = None;
                    let h = hash_bundle(&f.diff_text, &f.files, &f.branch);
                    if Some(h) != last_hash {
                        last_hash = Some(h);
                        let _ = app2.emit(
                            "diff-pane-updated",
                            DiffPaneUpdatedEvent {
                                pane_id: pid.clone(),
                                diff_text: f.diff_text,
                                files: f.files,
                                error: None,
                                cwd: cwd_str,
                                branch: f.branch,
                                truncated: f.truncated,
                            },
                        );
                    }
                }
                Err(e) => {
                    if last_error.as_deref() != Some(e.as_str()) {
                        last_error = Some(e.clone());
                        last_hash = None;
                        let _ = app2.emit(
                            "diff-pane-updated",
                            DiffPaneUpdatedEvent {
                                pane_id: pid.clone(),
                                diff_text: String::new(),
                                files: Vec::new(),
                                error: Some(e),
                                cwd: cwd_str,
                                branch: None,
                                truncated: false,
                            },
                        );
                    }
                }
            }
            let interval = if is_remote { POLL_REMOTE_MS } else { POLL_LOCAL_MS };
            tokio::time::sleep(Duration::from_millis(interval)).await;
        }
    });
    if let Ok(mut w) = state.core.diff_pane_watchers.lock() {
        w.insert(pane_id, handle);
    }
}

fn stop_watcher_inner(state: &AppState, pane_id: &str) {
    if let Ok(mut w) = state.core.diff_pane_watchers.lock() {
        if let Some(h) = w.remove(pane_id) {
            h.abort();
        }
    }
}

pub(crate) fn stop_watcher(state: &AppState, pane_id: &str) {
    stop_watcher_inner(state, pane_id)
}

// ─── tauri commands ──────────────────────────────────────────────────

/// Mount path: start the watcher without persisting (nothing changed on
/// disk — the pane already carries its source/cwd).
#[tauri::command]
pub(crate) async fn diff_pane_start(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    start_watcher(app, (*state).clone(), pane_id);
    Ok(())
}

/// Unmount path: stop the watcher (idempotent).
#[tauri::command]
pub(crate) async fn diff_pane_stop(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    stop_watcher(&state, &pane_id);
    Ok(())
}

#[tauri::command]
pub(crate) async fn diff_pane_set_source(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    source: DiffSource,
) -> Result<(), String> {
    if let DiffSource::Ref { git_ref } = &source {
        validate_ref(git_ref)?;
    }
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let mut found = false;
        for ws in file.workspaces.iter_mut() {
            if let Some(layout) = ws.layout.as_mut() {
                if set_diff_source_in_layout(layout, &pane_id, source.clone()) {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return Err(format!("no Diff pane with id {pane_id}"));
        }
    }
    crate::persist(&state)?;
    start_watcher(app, (*state).clone(), pane_id);
    Ok(())
}

/// Point the pane at a worktree (the strip), or back at the workspace's
/// own cwd (`None`).
#[tauri::command]
pub(crate) async fn diff_pane_set_cwd(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    cwd: Option<String>,
) -> Result<(), String> {
    let cwd = cwd
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let mut found = false;
        for ws in file.workspaces.iter_mut() {
            if let Some(layout) = ws.layout.as_mut() {
                if set_diff_cwd_in_layout(layout, &pane_id, cwd.clone()) {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return Err(format!("no Diff pane with id {pane_id}"));
        }
    }
    crate::persist(&state)?;
    start_watcher(app, (*state).clone(), pane_id);
    Ok(())
}

#[tauri::command]
pub(crate) async fn diff_pane_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    if lookup_pane_context(&state, &pane_id)?.is_none() {
        return Err(format!("no Diff pane with id {pane_id}"));
    }
    emit_once(&app, &state, &pane_id).await;
    Ok(())
}

/// The worktrees of the pane's repo, for the strip. Works from any
/// worktree — `git worktree list` returns them all, main included.
#[tauri::command]
pub(crate) async fn diff_pane_worktrees(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<Vec<WorktreeEntry>, String> {
    let ctx = lookup_pane_context(&state, &pane_id)?
        .ok_or_else(|| format!("no Diff pane with id {pane_id}"))?;
    let cwd = ctx
        .cwd
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| "this workspace has no project directory".to_string())?;
    let args: Vec<&str> = git_global_args()
        .iter()
        .copied()
        .chain(["worktree", "list", "--porcelain"])
        .collect();
    let r = run_git_raw(&state, &ctx.conn, &cwd, &args).await?;
    if r.code != 0 {
        return Err(git_error(if r.err.is_empty() { &r.out } else { &r.err }));
    }
    Ok(parse_worktree_porcelain(&r.out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t.test")
            .status()
            .expect("git available for test");
        assert!(st.success(), "git {:?} failed", args);
    }

    #[test]
    fn diff_args_shape() {
        assert_eq!(diff_args(&DiffSource::Working), ["diff", "--no-color", "--no-ext-diff", "--"]);
        assert_eq!(diff_args(&DiffSource::Head), ["diff", "--no-color", "--no-ext-diff", "HEAD", "--"]);
        assert_eq!(
            diff_args(&DiffSource::Ref { git_ref: "main".into() }),
            ["diff", "--no-color", "--no-ext-diff", "main", "--"]
        );
    }

    #[test]
    fn validate_ref_rejects_options_and_empty() {
        assert!(validate_ref("HEAD~1").is_ok());
        assert!(validate_ref("main").is_ok());
        assert!(validate_ref("--output=/tmp/x").is_err());
        assert!(validate_ref("").is_err());
        assert!(validate_ref("  ").is_err());
        assert!(validate_ref("a\nb").is_err());
    }

    #[test]
    fn parse_status_z_branch_and_untracked() {
        // NUL-separated (local transport).
        let t = "## main...origin/main\0 M src/a.rs\0?? new.txt\0";
        let (branch, files) = parse_status_z(t);
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].xy, " M");
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!(files[1].xy, "??");
        assert_eq!(files[1].path, "new.txt");
        // Newline-separated (WSL/SSH bundle after `tr`).
        let (branch2, files2) = parse_status_z("## dev\n A x.rs\n");
        assert_eq!(branch2.as_deref(), Some("dev"));
        assert_eq!(files2[0].xy, " A");
    }

    #[test]
    fn parse_status_z_rename_consumes_orig() {
        let t = "## main\0R  new.rs\0old.rs\0 M other.rs\0";
        let (_b, files) = parse_status_z(t);
        assert_eq!(files.len(), 2);
        assert!(files[0].xy.starts_with('R'));
        assert_eq!(files[0].path, "new.rs");
        assert_eq!(files[0].orig_path.as_deref(), Some("old.rs"));
        assert_eq!(files[1].path, "other.rs");
    }

    #[test]
    fn parse_branch_header_edges() {
        assert_eq!(parse_branch_header("HEAD (no branch)"), None);
        assert_eq!(parse_branch_header("No commits yet on main").as_deref(), Some("main"));
        assert_eq!(parse_branch_header("feat/x...origin/feat/x [ahead 1]").as_deref(), Some("feat/x"));
        assert_eq!(parse_branch_header("solo").as_deref(), Some("solo"));
    }

    #[test]
    fn untracked_paths_caps() {
        let files: Vec<StatusEntry> = (0..100)
            .map(|i| StatusEntry { xy: "??".into(), path: format!("f{i}"), orig_path: None })
            .collect();
        assert_eq!(untracked_paths(&files, 40).len(), 40);
        let mixed = vec![
            StatusEntry { xy: " M".into(), path: "a".into(), orig_path: None },
            StatusEntry { xy: "??".into(), path: "b".into(), orig_path: None },
        ];
        assert_eq!(untracked_paths(&mixed, 40), vec!["b".to_string()]);
    }

    #[test]
    fn bundle_script_quotes_and_marks() {
        let s = bundle_script("/home/y/my repo", &DiffSource::Head, "__YMUX_DIFF_00__");
        assert!(s.contains("'/home/y/my repo'"));
        // The marker is shell-quoted in the script; printf's format carries
        // the section word. `split_bundle` reassembles `<marker> status`
        // etc. from the OUTPUT, which is covered by its own test.
        assert!(s.contains("'__YMUX_DIFF_00__'"));
        assert!(s.contains("printf '%s status"));
        assert!(s.contains("printf '\\n%s end"));
        assert!(s.contains("--no-index"));
        // A path that tries to break out stays inert.
        let evil = bundle_script("/x'; rm -rf /; echo '", &DiffSource::Working, "__M__");
        assert!(evil.contains("'\\''"));
        // A ref is quoted too.
        let r = bundle_script("/x", &DiffSource::Ref { git_ref: "v1.0".into() }, "__M__");
        assert!(r.contains("'v1.0'"));
    }

    #[test]
    fn split_bundle_sections_and_missing_end() {
        let m = "__M__";
        let text = "__M__ status\n## main\0 M a\n__M__ diff\ndiff --git a/a b/a\n+x\n__M__ untracked\ndiff --git a/n b/n\n+y\n__M__ end\ntrailing";
        let b = split_bundle(m, text).expect("ok");
        assert!(b.status_text.contains("## main"));
        assert!(b.diff_text.contains("diff --git a/a b/a"));
        assert!(b.diff_text.contains("+y")); // untracked folded into diff
        assert!(!b.diff_text.contains("trailing")); // after end
        // No end marker → the whole text is the error.
        let err = split_bundle(m, "fatal: not a git repository").unwrap_err();
        assert!(err.contains("not a git repository"));
    }

    #[test]
    fn cap_diff_truncates() {
        let big = "x".repeat(DIFF_TEXT_CAP + 10);
        let (out, trunc) = cap_diff(big);
        assert!(trunc);
        assert!(out.contains("output truncated"));
        let (small, trunc2) = cap_diff("hello".to_string());
        assert!(!trunc2);
        assert_eq!(small, "hello");
    }

    // Real git: the local bundle sees an unstaged change AND an untracked
    // file against HEAD, and the rename parse matches real `status -z`.
    // English CI runners, so the message match in the non-repo case holds.
    #[tokio::test]
    async fn fetch_bundle_local_sees_unstaged_and_untracked() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let dir = tmp.path();
        run_git(dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("hello.txt"), "one\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "init"]);
        fs::write(dir.join("hello.txt"), "two\n").unwrap();
        fs::write(dir.join("new.txt"), "fresh\n").unwrap();

        let f = fetch_bundle_local(dir.to_str().unwrap(), &DiffSource::Head)
            .await
            .expect("bundle ok");
        assert_eq!(f.branch.as_deref(), Some("main"));
        assert!(f.diff_text.contains("-one"));
        assert!(f.diff_text.contains("+two"));
        assert!(f.diff_text.contains("+fresh"), "untracked file diff missing");
        assert!(f.files.iter().any(|e| e.path == "new.txt" && e.xy == "??"));
    }
}
