//! The repo-committed worktree recipe, `.ymux/worktree.json`.
//!
//! ```json
//! { "copy": [".env", "config/local.json"], "setup": "npm ci" }
//! ```
//!
//! After ymux creates a worktree, the recipe copies the listed files from the
//! main checkout and runs the setup command in the new worktree.
//!
//! A repo file that runs commands on a machine is an attack path, so:
//!
//! - **Nothing runs without an approval.** `worktree_recipe_run` refuses a
//!   recipe that has no approval for this host + repo + sha256. A changed
//!   recipe has a new sha256, so it prompts again.
//! - **The user approves the bytes they saw.** The run re-reads the recipe and
//!   refuses it when the sha256 differs from the one the prompt showed.
//! - **The recipe comes from git, not from the filesystem:** the blob at
//!   `HEAD` of the new worktree. A symlink at that path is a blob that holds
//!   the link target, which fails to parse. A dirty edit cannot change it.
//! - **Paths are literal.** No globs, so the prompt shows exactly what gets
//!   copied. See `validate_copy_path`.
//! - **A symlink in any path component stops the copy**, on the source side
//!   and on the destination side. A tracked symlink in the new worktree must
//!   not redirect a write out of the repo.
//! - **Absolute Rule #1:** logs carry counts and exit codes only. Command
//!   output goes to the UI and to nowhere else.
//!
//! Local Windows has no `sh`. A recipe command is `sh` syntax, and `cmd.exe`
//! would give it a different meaning from the one the user approved. So local
//! Windows copies the files and does not run the command, and the prompt says
//! so first. macOS and Linux run it with `/bin/sh`.
//!
//! The setup command is the one string that Rule #3 cannot apply to: it is
//! shell source by definition. It goes through `shell_quote` as a single
//! argument to `sh -lc`, and the trust prompt shows it verbatim.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ymux_core::{log_info, shell_quote};

use crate::worktrees::{exec_script_over, parse_worktree_porcelain, run_git_over, run_git_raw};
use crate::{config_dir_pub, lock_or_recover, AppState, Connection};

const RECIPE_BLOB: &str = "HEAD:.ymux/worktree.json";
const APPROVALS_FILE: &str = "worktree-recipe-approvals.json";
const MAX_RECIPE_BYTES: usize = 64 * 1024;
const MAX_COPY_ENTRIES: usize = 200;
const MAX_PATH_CHARS: usize = 1024;
const MAX_SETUP_CHARS: usize = 4096;
const COPY_TIMEOUT_SECS: u64 = 120;
const SETUP_TIMEOUT_SECS: u64 = 20 * 60;
/// The tail of the setup output that reaches the UI. `tail -c` applies this
/// limit on the host, so a noisy command cannot fill the app's memory.
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;
const COPY_MARK: &str = "__YMUX_COPY ";
const SETUP_MARK: &str = "__YMUX_SETUP_EXIT=";

/// Serializes the read-modify-write of the approvals file in this process.
static APPROVALS_LOCK: Mutex<()> = Mutex::new(());

// ─── recipe parsing and validation ─────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    #[serde(default)]
    copy: Vec<String>,
    #[serde(default)]
    setup: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Recipe {
    copy: Vec<String>,
    setup: Option<String>,
}

/// Characters that make text look different from what it is: controls,
/// bidi overrides, zero-width marks, and the replacement character that
/// lossy UTF-8 decoding leaves. A trust prompt must show the exact text.
fn is_deceptive_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{FFFD}'
        )
}

/// Accept a repo-relative literal file path, on any host OS.
///
/// This is string-based on purpose: the app validates here, and the path is
/// used on the workspace's host, so `std::path` semantics of this OS are the
/// wrong ones.
fn validate_copy_path(p: &str) -> Result<(), String> {
    let reject = |why: &str| Err(format!("recipe copy path {p:?} rejected: {why}"));
    if p.is_empty() {
        return reject("empty path");
    }
    if p.chars().count() > MAX_PATH_CHARS {
        return reject("path too long");
    }
    if p.chars().any(is_deceptive_char) {
        return reject("control or invisible character");
    }
    if p.starts_with('/') {
        return reject("absolute path");
    }
    // A backslash is a separator on Windows and a name character elsewhere.
    if p.contains('\\') {
        return reject("use forward slashes");
    }
    // Drive letters ("C:") and NTFS alternate data streams (".env:x").
    if p.contains(':') {
        return reject("colon not allowed");
    }
    if p.contains(['*', '?', '[']) {
        return reject("list literal files, not patterns");
    }
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return reject("empty, '.' or '..' path component");
        }
        // Windows drops these, so "..." or ".git." would alias another name.
        if seg.ends_with('.') || seg.ends_with(' ') {
            return reject("component ends with a dot or a space");
        }
        if seg.eq_ignore_ascii_case(".git") {
            return reject("git metadata");
        }
        if is_windows_device_name(seg) {
            return reject("reserved device name");
        }
    }
    Ok(())
}

fn is_windows_device_name(seg: &str) -> bool {
    let stem = seg.split('.').next().unwrap_or(seg).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && bytes[3].is_ascii_digit()
}

/// Parse and validate. One bad entry refuses the whole recipe. `Ok(None)`
/// means the file asks for nothing.
fn parse_recipe(text: &str) -> Result<Option<Recipe>, String> {
    let raw: RawRecipe = serde_json::from_str(text).map_err(|e| format!("invalid recipe: {e}"))?;
    if raw.copy.len() > MAX_COPY_ENTRIES {
        return Err(format!("recipe lists more than {MAX_COPY_ENTRIES} files"));
    }
    for p in &raw.copy {
        validate_copy_path(p)?;
    }
    let setup = raw.setup.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(s) = &setup {
        if s.chars().count() > MAX_SETUP_CHARS {
            return Err(format!("recipe setup is longer than {MAX_SETUP_CHARS} characters"));
        }
        // One line, so a screen of blank lines cannot hide the real command.
        if s.chars().any(is_deceptive_char) {
            return Err("recipe setup must be one line of visible text".to_string());
        }
    }
    if raw.copy.is_empty() && setup.is_none() {
        return Ok(None);
    }
    Ok(Some(Recipe { copy: raw.copy, setup }))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ─── approvals ─────────────────────────────────────────────────────────

/// An approval holds for one host, one repo and one exact recipe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ApprovalKey {
    host: String,
    repo: String,
    sha256: String,
}

#[derive(Default, Serialize, Deserialize)]
struct ApprovalFile {
    #[serde(default)]
    approved: Vec<ApprovalKey>,
}

/// The identity of the machine that the recipe runs on. The credential
/// (`key_path`) is not part of it, as in `pick_ssh_handle_for_host`.
fn host_identity(conn: &Option<Connection>) -> String {
    match conn {
        None | Some(Connection::Local { .. }) => "local".to_string(),
        Some(Connection::Wsl { distro }) => format!("wsl:{}", distro.as_deref().unwrap_or("")),
        Some(Connection::Ssh { host, user, port, .. }) => format!("ssh:{user}@{host}:{port}"),
    }
}

/// An unreadable or corrupt file reads as "nothing approved": fail closed,
/// the user sees one more prompt.
fn load_approvals(path: &Path) -> ApprovalFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn is_approved_in(path: &Path, key: &ApprovalKey) -> bool {
    let _guard = lock_or_recover(&APPROVALS_LOCK);
    load_approvals(path).approved.contains(key)
}

/// Rule #7: write `<file>.<pid>.tmp`, then rename over the target.
fn store_approval_in(path: &Path, key: &ApprovalKey) -> Result<(), String> {
    let _guard = lock_or_recover(&APPROVALS_LOCK);
    let mut file = load_approvals(path);
    if file.approved.contains(key) {
        return Ok(());
    }
    file.approved.push(key.clone());
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    let dir = path.parent().ok_or_else(|| "no parent dir".to_string())?;
    fs::create_dir_all(dir).map_err(|e| format!("create config dir: {e}"))?;
    let tmp = dir.join(format!("{APPROVALS_FILE}.{}.tmp", std::process::id()));
    write_synced(&tmp, text.as_bytes()).map_err(|e| format!("write approvals: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("replace approvals: {e}"))
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn approvals_path() -> Result<std::path::PathBuf, String> {
    Ok(config_dir_pub()?.join(APPROVALS_FILE))
}

// ─── copy ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum WorktreeRecipeCopyStatus {
    Copied,
    /// The main checkout has no such file. Normal for an optional `.env`.
    Missing,
    /// The new worktree already has this path. Never overwritten.
    Exists,
    /// A directory or a device. Only regular files are copied.
    NotFile,
    /// A symlink in the path. The recipe stops here.
    Symlink,
    /// An I/O failure. The recipe stops here.
    Failed,
    /// An earlier entry stopped the recipe.
    NotAttempted,
}

impl WorktreeRecipeCopyStatus {
    fn stops_recipe(self) -> bool {
        matches!(self, Self::Symlink | Self::Failed)
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "copied" => Self::Copied,
            "missing" => Self::Missing,
            "exists" => Self::Exists,
            "not_file" => Self::NotFile,
            "symlink" => Self::Symlink,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

enum Chain {
    /// Every component exists and none is a symlink.
    Present,
    /// A component does not exist. No component before it is a symlink.
    Absent,
    Symlink,
    Error,
}

/// Walk `rel` from `root`, one component at a time, without following links.
/// `is_symlink` is also true for a Windows junction.
fn walk_chain(root: &Path, rel: &str) -> Chain {
    let mut p = root.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return Chain::Symlink,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
                return Chain::Absent
            }
            Err(_) => return Chain::Error,
        }
    }
    Chain::Present
}

/// `create_new` refuses an existing path and does not follow a symlink there.
fn copy_exclusive(src: &Path, dst: &Path) -> io::Result<()> {
    let mut input = fs::File::open(src)?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // Keep the mode: a 0600 `.env` must not arrive as 0644.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        opts.mode(input.metadata()?.permissions().mode());
    }
    let mut output = opts.open(dst)?;
    io::copy(&mut input, &mut output)?;
    Ok(())
}

fn copy_one_local(src_root: &Path, dst_root: &Path, rel: &str) -> WorktreeRecipeCopyStatus {
    use WorktreeRecipeCopyStatus as S;
    match walk_chain(src_root, rel) {
        Chain::Symlink => return S::Symlink,
        Chain::Error => return S::Failed,
        Chain::Absent => return S::Missing,
        Chain::Present => {}
    }
    match walk_chain(dst_root, rel) {
        Chain::Symlink => return S::Symlink,
        Chain::Error => return S::Failed,
        Chain::Present => return S::Exists,
        Chain::Absent => {}
    }
    let src = src_root.join(rel);
    match fs::symlink_metadata(&src) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return S::NotFile,
        Err(_) => return S::Failed,
    }
    let dst = dst_root.join(rel);
    if let Some(parent) = dst.parent() {
        if fs::create_dir_all(parent).is_err() {
            return S::Failed;
        }
    }
    match copy_exclusive(&src, &dst) {
        Ok(()) => S::Copied,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => S::Exists,
        Err(_) => S::Failed,
    }
}

fn copy_all_local(main: &str, worktree: &str, paths: &[String]) -> Vec<WorktreeRecipeCopyStatus> {
    let mut stopped = false;
    paths
        .iter()
        .map(|rel| {
            if stopped {
                return WorktreeRecipeCopyStatus::NotAttempted;
            }
            let status = copy_one_local(Path::new(main), Path::new(worktree), rel);
            stopped = status.stops_recipe();
            status
        })
        .collect()
}

/// POSIX `sh` copy for SSH and WSL. The same rules as `copy_one_local`.
const COPY_SCRIPT_BODY: &str = r#"
report() { printf '__YMUX_COPY %s %s\n' "$1" "$2"; }
chain() {
  p=$1; rest=$2
  while [ -n "$rest" ]; do
    case $rest in
      */*) seg=${rest%%/*}; rest=${rest#*/} ;;
      *) seg=$rest; rest= ;;
    esac
    p=$p/$seg
    if [ -L "$p" ]; then return 1; fi
  done
  return 0
}
one() {
  i=$1; rel=$2; s=$SRC/$rel; d=$DST/$rel
  if ! chain "$SRC" "$rel" || ! chain "$DST" "$rel"; then report "$i" symlink; exit 0; fi
  if [ ! -e "$s" ]; then report "$i" missing; return 0; fi
  if [ ! -f "$s" ]; then report "$i" not_file; return 0; fi
  if [ -e "$d" ]; then report "$i" exists; return 0; fi
  if mkdir -p -- "${d%/*}" && cp -p -- "$s" "$d"; then report "$i" copied; else report "$i" failed; exit 0; fi
}
"#;

/// Every interpolated value goes through `shell_quote` (Rule #3). The script
/// reports indexes, so no path or file content comes back in its output.
fn copy_script(main: &str, worktree: &str, paths: &[String]) -> String {
    let mut s = format!("SRC={}\nDST={}\n", shell_quote(main), shell_quote(worktree));
    s.push_str(COPY_SCRIPT_BODY);
    for (i, p) in paths.iter().enumerate() {
        s.push_str(&format!("one {i} {}\n", shell_quote(p)));
    }
    s
}

/// Lines that are not ours (a login banner, for example) are ignored. An
/// entry with no report did not run.
fn parse_copy_output(text: &str, count: usize) -> Vec<WorktreeRecipeCopyStatus> {
    let mut out = vec![WorktreeRecipeCopyStatus::NotAttempted; count];
    for line in text.lines() {
        let Some(rest) = line.trim_end_matches('\r').strip_prefix(COPY_MARK) else {
            continue;
        };
        let Some((idx, status)) = rest.split_once(' ') else {
            continue;
        };
        if let (Ok(i), Some(st)) = (idx.parse::<usize>(), WorktreeRecipeCopyStatus::parse(status)) {
            if let Some(slot) = out.get_mut(i) {
                *slot = st;
            }
        }
    }
    out
}

fn is_local(conn: &Option<Connection>) -> bool {
    matches!(conn, None | Some(Connection::Local { .. }))
}

async fn run_copies(
    state: &AppState,
    ctx: &RecipeContext,
    paths: &[String],
) -> Result<Vec<WorktreeRecipeCopyStatus>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if is_local(&ctx.conn) {
        let (main, worktree, list) = (ctx.main.clone(), ctx.worktree.clone(), paths.to_vec());
        return tokio::task::spawn_blocking(move || copy_all_local(&main, &worktree, &list))
            .await
            .map_err(|e| format!("copy task failed: {e}"));
    }
    let script = copy_script(&ctx.main, &ctx.worktree, paths);
    let (_code, text) = exec_script_over(state, &ctx.conn, &script, COPY_TIMEOUT_SECS).await?;
    Ok(parse_copy_output(&text, paths.len()))
}

// ─── setup ─────────────────────────────────────────────────────────────

/// `setup_supported` is the one place that says where the setup can run.
fn setup_supported(conn: &Option<Connection>) -> bool {
    !(cfg!(windows) && is_local(conn))
}

/// Run the command in the worktree and report its exit code on the last line.
/// A pipeline would otherwise lose it: POSIX `sh` has no `pipefail`. `-l` gives
/// the login PATH, which a GUI process or an SSH exec channel does not have.
fn setup_script(worktree: &str, setup: &str) -> String {
    format!(
        "{{ cd -- {wt} && sh -lc {cmd} </dev/null; rc=$?; printf '\\n{SETUP_MARK}%s\\n' \"$rc\"; }} 2>&1 | tail -c {OUTPUT_TAIL_BYTES}",
        wt = shell_quote(worktree),
        cmd = shell_quote(setup),
    )
}

/// `(exit code, output)`. No marker means no exit code, which the caller
/// reports as a failure.
fn parse_setup_output(text: &str) -> (Option<i32>, String) {
    let found = text
        .rmatch_indices(SETUP_MARK)
        .find(|(pos, _)| *pos == 0 || text.as_bytes()[*pos - 1] == b'\n');
    let Some((pos, _)) = found else {
        return (None, text.trim_end().to_string());
    };
    let code = text[pos + SETUP_MARK.len()..]
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<i32>().ok());
    (code, text[..pos].trim_end().to_string())
}

#[cfg(not(windows))]
async fn run_setup_local(script: &str) -> Result<String, String> {
    let out = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("spawn sh: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(windows)]
async fn run_setup_local(_script: &str) -> Result<String, String> {
    Err("recipe setup does not run on local Windows".to_string())
}

async fn exec_setup(
    state: &AppState,
    conn: &Option<Connection>,
    script: &str,
) -> Result<String, String> {
    let run = async {
        if is_local(conn) {
            run_setup_local(script).await
        } else {
            exec_script_over(state, conn, script, SETUP_TIMEOUT_SECS)
                .await
                .map(|(_code, text)| text)
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(SETUP_TIMEOUT_SECS), run)
        .await
        .map_err(|_| format!("setup timed out after {SETUP_TIMEOUT_SECS} seconds"))?
}

// ─── commands ──────────────────────────────────────────────────────────

/// Everything the trust prompt shows.
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct WorktreeRecipePreview {
    pub host: String,
    /// The main checkout: the copy source and the repo in the approval key.
    pub repo: String,
    pub worktree: String,
    pub copy: Vec<String>,
    pub setup: Option<String>,
    pub sha256: String,
    pub approved: bool,
    /// False on local Windows, where the setup command does not run.
    pub setup_supported: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct WorktreeRecipeCopyResult {
    pub path: String,
    pub status: WorktreeRecipeCopyStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum WorktreeRecipeSetupStatus {
    /// The recipe has no setup command.
    None,
    Succeeded,
    Failed,
    /// Local Windows: no `sh`.
    Unsupported,
    /// A copy entry stopped the recipe first.
    NotRun,
}

/// A failing setup is a result, not an `Err`: the worktree stays in place and
/// the dialog shows what happened.
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct WorktreeRecipeResult {
    pub copies: Vec<WorktreeRecipeCopyResult>,
    pub setup_status: WorktreeRecipeSetupStatus,
    pub setup_exit_code: Option<i32>,
    /// The tail of the command's output. For the dialog only (Rule #1).
    pub setup_output: Option<String>,
    /// Set when the command could not run or did not report (timeout, lost
    /// connection).
    pub setup_error: Option<String>,
}

struct RecipeContext {
    conn: Option<Connection>,
    main: String,
    worktree: String,
}

/// Find the worktree by its branch, in git's own list. The frontend never
/// names a path, so this cannot be pointed at an arbitrary directory.
async fn resolve_context(
    state: &AppState,
    workspace_id: &str,
    branch: &str,
) -> Result<RecipeContext, String> {
    let (conn, cwd) = {
        let file = lock_or_recover(&state.workspaces);
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        let cwd = ws
            .cwd
            .clone()
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| "this workspace has no project directory".to_string())?;
        (ws.connection.clone(), cwd)
    };
    let text = run_git_over(state, &conn, &cwd, &["worktree", "list", "--porcelain"]).await?;
    let entries = parse_worktree_porcelain(&text);
    let main = entries
        .iter()
        .find(|e| e.is_main)
        .map(|e| e.path.clone())
        .ok_or_else(|| "git reported no main working tree".to_string())?;
    let target = entries
        .iter()
        .find(|e| e.branch.as_deref() == Some(branch))
        .ok_or_else(|| format!("no worktree has branch {branch}"))?;
    if target.is_main {
        return Err("the recipe does not run on the main working tree".to_string());
    }
    Ok(RecipeContext { conn, main, worktree: target.path.clone() })
}

/// Non-zero from git means this commit has no recipe. A dead transport is an
/// `Err` from `run_git_raw`, so it does not read as "no recipe".
async fn read_recipe(state: &AppState, ctx: &RecipeContext) -> Result<Option<String>, String> {
    let r = run_git_raw(state, &ctx.conn, &ctx.worktree, &["cat-file", "blob", RECIPE_BLOB]).await?;
    if r.code != 0 {
        return Ok(None);
    }
    if r.out.len() > MAX_RECIPE_BYTES {
        return Err(format!("recipe is larger than {MAX_RECIPE_BYTES} bytes"));
    }
    Ok(Some(r.out))
}

fn approval_key(ctx: &RecipeContext, sha256: String) -> ApprovalKey {
    ApprovalKey { host: host_identity(&ctx.conn), repo: ctx.main.clone(), sha256 }
}

/// Read the new worktree's recipe. `None` means there is nothing to do.
#[tauri::command]
pub(crate) async fn worktree_recipe_preview(
    state: tauri::State<'_, AppState>,
    workspace_id: String,
    branch: String,
) -> Result<Option<WorktreeRecipePreview>, String> {
    let ctx = resolve_context(&state, &workspace_id, branch.trim()).await?;
    let Some(raw) = read_recipe(&state, &ctx).await? else {
        return Ok(None);
    };
    let Some(recipe) = parse_recipe(&raw)? else {
        return Ok(None);
    };
    let key = approval_key(&ctx, sha256_hex(raw.as_bytes()));
    let approved = is_approved_in(&approvals_path()?, &key);
    Ok(Some(WorktreeRecipePreview {
        setup_supported: setup_supported(&ctx.conn),
        host: key.host,
        repo: key.repo,
        worktree: ctx.worktree,
        copy: recipe.copy,
        setup: recipe.setup,
        sha256: key.sha256,
        approved,
    }))
}

/// Run the recipe the user saw. `sha256` comes from the preview; `approve` is
/// true only when the user pressed Approve in the trust prompt.
#[tauri::command]
pub(crate) async fn worktree_recipe_run(
    state: tauri::State<'_, AppState>,
    workspace_id: String,
    branch: String,
    sha256: String,
    approve: bool,
) -> Result<WorktreeRecipeResult, String> {
    let ctx = resolve_context(&state, &workspace_id, branch.trim()).await?;
    let raw = read_recipe(&state, &ctx)
        .await?
        .ok_or_else(|| "the recipe is no longer in this worktree".to_string())?;
    let actual = sha256_hex(raw.as_bytes());
    if actual != sha256 {
        return Err("the recipe changed after the prompt: review it again".to_string());
    }
    let recipe = parse_recipe(&raw)?.ok_or_else(|| "the recipe is empty".to_string())?;

    let key = approval_key(&ctx, actual);
    let approvals = approvals_path()?;
    if approve {
        // Before any side effect: an approval that cannot be stored stops here.
        store_approval_in(&approvals, &key)?;
    } else if !is_approved_in(&approvals, &key) {
        return Err("this recipe is not approved".to_string());
    }

    let statuses = run_copies(&state, &ctx, &recipe.copy).await?;
    let stopped = statuses.iter().any(|s| s.stops_recipe());
    let copied = statuses
        .iter()
        .filter(|s| **s == WorktreeRecipeCopyStatus::Copied)
        .count();
    let copies = recipe
        .copy
        .into_iter()
        .zip(statuses)
        .map(|(path, status)| WorktreeRecipeCopyResult { path, status })
        .collect();

    let mut result = WorktreeRecipeResult {
        copies,
        setup_status: WorktreeRecipeSetupStatus::None,
        setup_exit_code: None,
        setup_output: None,
        setup_error: None,
    };
    if let Some(setup) = recipe.setup {
        if stopped {
            result.setup_status = WorktreeRecipeSetupStatus::NotRun;
        } else if !setup_supported(&ctx.conn) {
            result.setup_status = WorktreeRecipeSetupStatus::Unsupported;
        } else {
            match exec_setup(&state, &ctx.conn, &setup_script(&ctx.worktree, &setup)).await {
                Ok(text) => {
                    let (code, output) = parse_setup_output(&text);
                    result.setup_status = if code == Some(0) {
                        WorktreeRecipeSetupStatus::Succeeded
                    } else {
                        WorktreeRecipeSetupStatus::Failed
                    };
                    result.setup_exit_code = code;
                    result.setup_output = Some(output);
                }
                Err(e) => {
                    result.setup_status = WorktreeRecipeSetupStatus::Failed;
                    result.setup_error = Some(e);
                }
            }
        }
    }
    // Rule #1: counts and the exit code only.
    log_info(
        "WORKTREE",
        &format!(
            "[recipe] ws={workspace_id} copied={copied}/{} setup={:?} exit={:?}",
            result.copies.len(),
            result.setup_status,
            result.setup_exit_code,
        ),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorktreeRecipeCopyStatus as S;

    #[test]
    fn copy_path_accepts_plain_relative_files() {
        for ok in [".env", "config/local.json", "a b/c.txt", ".ymux/x", "console/app"] {
            assert_eq!(validate_copy_path(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn copy_path_rejects_escapes_and_absolutes() {
        for bad in [
            "", "/etc/passwd", "../secret", "a/../../b", "a/..", "./a", "a//b", "a/",
            "C:/x", "C:\\x", "\\\\srv\\share", "a\\..\\b", ".env:stream",
        ] {
            assert!(validate_copy_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn copy_path_rejects_patterns_git_dir_and_aliases() {
        for bad in [
            "*.env", "a/?", "[ab]", ".git/config", ".GIT/hooks/x", "x/.git/y",
            "...", ".git.", "a. /b", "NUL", "con.txt", "x/com1", "lpt9.log",
        ] {
            assert!(validate_copy_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn copy_path_rejects_deceptive_text() {
        for bad in ["a\nb", "a\u{0}b", "a\u{202E}b", "a\u{200B}b", "a\u{FFFD}b"] {
            assert!(validate_copy_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn device_name_check_is_exact() {
        assert!(is_windows_device_name("aux.h"));
        assert!(is_windows_device_name("COM3"));
        assert!(!is_windows_device_name("console"));
        assert!(!is_windows_device_name("comx"));
        assert!(!is_windows_device_name("com12"));
    }

    #[test]
    fn parse_recipe_reads_both_fields() {
        let r = parse_recipe(r#"{"copy":[".env"],"setup":"  npm ci  "}"#).unwrap().unwrap();
        assert_eq!(r.copy, vec![".env".to_string()]);
        assert_eq!(r.setup.as_deref(), Some("npm ci"));
    }

    #[test]
    fn parse_recipe_refuses_the_whole_file_for_one_bad_entry() {
        assert!(parse_recipe(r#"{"copy":[".env","../x"],"setup":"true"}"#).is_err());
        assert!(parse_recipe(r#"{"setup":"true","teardown":"rm -rf ."}"#).is_err());
        assert!(parse_recipe("not json").is_err());
        assert!(parse_recipe(r#"{"setup":"a\nb"}"#).is_err());
        let long = format!(r#"{{"setup":"{}"}}"#, "x".repeat(MAX_SETUP_CHARS + 1));
        assert!(parse_recipe(&long).is_err());
    }

    #[test]
    fn parse_recipe_with_nothing_to_do_is_none() {
        assert_eq!(parse_recipe("{}"), Ok(None));
        assert_eq!(parse_recipe(r#"{"copy":[],"setup":"   "}"#), Ok(None));
    }

    #[test]
    fn sha256_is_known_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn key(host: &str, repo: &str, sha: &str) -> ApprovalKey {
        ApprovalKey { host: host.into(), repo: repo.into(), sha256: sha.into() }
    }

    #[test]
    fn approval_matches_host_repo_and_sha_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(APPROVALS_FILE);
        let k = key("ssh:y@box:22", "/srv/repo", "aaa");
        assert!(!is_approved_in(&path, &k));
        store_approval_in(&path, &k).unwrap();
        assert!(is_approved_in(&path, &k));
        assert!(!is_approved_in(&path, &key("ssh:y@box:22", "/srv/repo", "bbb")));
        assert!(!is_approved_in(&path, &key("ssh:y@box:22", "/srv/other", "aaa")));
        assert!(!is_approved_in(&path, &key("ssh:y@box2:22", "/srv/repo", "aaa")));
        assert!(!is_approved_in(&path, &key("local", "/srv/repo", "aaa")));
    }

    #[test]
    fn storing_is_idempotent_and_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(APPROVALS_FILE);
        let k = key("local", "/r", "a");
        store_approval_in(&path, &k).unwrap();
        store_approval_in(&path, &k).unwrap();
        store_approval_in(&path, &key("local", "/r", "b")).unwrap();
        assert_eq!(load_approvals(&path).approved.len(), 2);
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn corrupt_approvals_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(APPROVALS_FILE);
        fs::write(&path, "{not json").unwrap();
        assert!(!is_approved_in(&path, &key("local", "/r", "a")));
    }

    #[test]
    fn host_identity_separates_transports_and_ignores_the_credential() {
        let ssh = |key_path: Option<&str>| {
            Some(Connection::Ssh {
                host: "box".into(),
                user: "y".into(),
                port: 22,
                key_path: key_path.map(Into::into),
            })
        };
        assert_eq!(host_identity(&None), "local");
        assert_eq!(host_identity(&Some(Connection::Local { shell: None })), "local");
        assert_eq!(host_identity(&ssh(None)), "ssh:y@box:22");
        assert_eq!(host_identity(&ssh(Some("/k"))), host_identity(&ssh(None)));
    }

    fn two_roots() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("main"), dir.path().join("wt"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        (dir, src, dst)
    }

    #[test]
    fn copies_a_nested_file() {
        let (_d, src, dst) = two_roots();
        fs::create_dir_all(src.join("cfg")).unwrap();
        fs::write(src.join("cfg/a"), "v").unwrap();
        assert_eq!(copy_one_local(&src, &dst, "cfg/a"), S::Copied);
        assert_eq!(fs::read_to_string(dst.join("cfg/a")).unwrap(), "v");
    }

    #[test]
    fn missing_directory_and_existing_targets_do_not_copy() {
        let (_d, src, dst) = two_roots();
        assert_eq!(copy_one_local(&src, &dst, ".env"), S::Missing);
        fs::create_dir_all(src.join("dir")).unwrap();
        assert_eq!(copy_one_local(&src, &dst, "dir"), S::NotFile);
        fs::write(src.join("x"), "new").unwrap();
        fs::write(dst.join("x"), "old").unwrap();
        assert_eq!(copy_one_local(&src, &dst, "x"), S::Exists);
        assert_eq!(fs::read_to_string(dst.join("x")).unwrap(), "old");
    }

    #[test]
    fn a_missing_file_does_not_stop_later_entries() {
        let (_d, src, dst) = two_roots();
        fs::write(src.join("a"), "1").unwrap();
        let got = copy_all_local(
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            &["ok".to_string(), "a".to_string()],
        );
        assert_eq!(got, vec![S::Missing, S::Copied]);
    }

    #[cfg(unix)]
    mod symlinks {
        use super::*;
        use std::os::unix::fs::{symlink, PermissionsExt};

        #[test]
        fn source_symlink_leaf_and_parent_stop() {
            let (d, src, dst) = two_roots();
            let outside = d.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("secret"), "s").unwrap();
            symlink(outside.join("secret"), src.join("leaf")).unwrap();
            symlink(&outside, src.join("dir")).unwrap();
            assert_eq!(copy_one_local(&src, &dst, "leaf"), S::Symlink);
            assert_eq!(copy_one_local(&src, &dst, "dir/secret"), S::Symlink);
            assert!(!dst.join("leaf").exists());
        }

        #[test]
        fn destination_symlink_cannot_redirect_the_write() {
            let (d, src, dst) = two_roots();
            let outside = d.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            fs::write(src.join("leaf"), "x").unwrap();
            fs::create_dir_all(src.join("dir")).unwrap();
            fs::write(src.join("dir/f"), "x").unwrap();
            symlink(outside.join("victim"), dst.join("leaf")).unwrap();
            symlink(&outside, dst.join("dir")).unwrap();
            assert_eq!(copy_one_local(&src, &dst, "leaf"), S::Symlink);
            assert_eq!(copy_one_local(&src, &dst, "dir/f"), S::Symlink);
            assert!(!outside.join("victim").exists());
            assert!(!outside.join("f").exists());
        }

        #[test]
        fn a_symlink_stops_the_remaining_entries() {
            let (d, src, dst) = two_roots();
            symlink(d.path(), src.join("link")).unwrap();
            fs::write(src.join("later"), "x").unwrap();
            let got = copy_all_local(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["link/x".to_string(), "later".to_string()],
            );
            assert_eq!(got, vec![S::Symlink, S::NotAttempted]);
            assert!(!dst.join("later").exists());
        }

        #[test]
        fn file_mode_survives_the_copy() {
            let (_d, src, dst) = two_roots();
            fs::write(src.join(".env"), "k=v").unwrap();
            fs::set_permissions(src.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(copy_one_local(&src, &dst, ".env"), S::Copied);
            let mode = fs::metadata(dst.join(".env")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn copy_script_quotes_every_interpolation() {
        let s = copy_script("/m'x", "/w y", &["a b'c".to_string()]);
        assert!(s.contains(r"SRC='/m'\''x'"));
        assert!(s.contains("DST='/w y'"));
        assert!(s.contains(r"one 0 'a b'\''c'"));
    }

    #[test]
    fn setup_script_quotes_the_command_as_one_argument() {
        let s = setup_script("/w y", "echo 'hi'; rm x");
        assert!(s.contains("cd -- '/w y' && sh -lc "));
        assert!(s.contains(r"'echo '\''hi'\''; rm x'"));
        assert!(s.ends_with(&format!("| tail -c {OUTPUT_TAIL_BYTES}")));
    }

    #[test]
    fn copy_output_ignores_noise_and_marks_silence_as_not_attempted() {
        let text = "motd line\n__YMUX_COPY 0 copied\r\n__YMUX_COPY 9 copied\n__YMUX_COPY 1 bogus\n";
        assert_eq!(parse_copy_output(text, 2), vec![S::Copied, S::NotAttempted]);
    }

    #[test]
    fn setup_output_takes_the_last_line_marker() {
        let (code, out) = parse_setup_output("build ok\n\n__YMUX_SETUP_EXIT=0\n");
        assert_eq!(code, Some(0));
        assert_eq!(out, "build ok");
        let (code, _) = parse_setup_output("__YMUX_SETUP_EXIT=0\nmore\n\n__YMUX_SETUP_EXIT=3\n");
        assert_eq!(code, Some(3));
    }

    #[test]
    fn setup_output_without_a_marker_has_no_exit_code() {
        let (code, out) = parse_setup_output("cut off");
        assert_eq!(code, None);
        assert_eq!(out, "cut off");
        // Mid-line text is not a marker.
        assert_eq!(parse_setup_output("x__YMUX_SETUP_EXIT=0\n").0, None);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn local_setup_reports_exit_code_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = setup_script(dir.path().to_str().unwrap(), "echo out; exit 7");
        let text = run_setup_local(&script).await.unwrap();
        let (code, out) = parse_setup_output(&text);
        assert_eq!(code, Some(7));
        assert!(out.ends_with("out"));
    }
}
