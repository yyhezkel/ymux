//! Phase 14.A: server provisioning wizard.
//!
//! Bootstraps a fresh server beyond just "create a workspace":
//!   - inspects the remote (OS, package manager, disk)
//!   - applies a profile of steps (update, install basics, create user,
//!     deploy SSH key, harden sshd, install language runtimes, install
//!     Claude Code, run `ymux setup-hooks`)
//!   - streams progress to the frontend via `provisioning:progress`
//!     events so the wizard's live log feels native
//!   - persists profiles in `%APPDATA%\ymux\provisioning-profiles.json`
//!     and original credentials in `…\provisioning-secrets.json` (DPAPI
//!     wrap planned — see below) so a second pass can resume
//!
//! Connections are stateless within a provisioning run: we open one
//! russh `client::Handle` to the target and reuse it across every step's
//! exec channel. Failures don't abort the run — each step ends as one of
//! pending / running / done / failed, the wizard offers retry/skip per
//! step, and we save a checkpoint after every state change.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use russh::client;
use russh::ChannelMsg;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::{config_dir_pub, log_debug, log_error, log_info, log_warn, AppState};

// ─── profile + step model ──────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    UpdatePackages,
    InstallBasics,
    CreateUser,
    GenerateKeypair,
    DeployPubkey,
    TestNewKey,
    DisableRootSsh,
    DisablePasswordSsh,
    InstallNodejs,
    InstallPython,
    InstallDocker,
    /// Install Claude Code via Anthropic's official curl installer.
    /// Previously this step ran `npm install -g @anthropic-ai/claude-code`,
    /// which forced an npm + Node.js dep tree. The official installer
    /// (`curl … | bash`) is npm-agnostic — Anthropic ships a static
    /// binary launcher — and it's the version they want users on.
    InstallClaudeCode,
    /// New in Phase 14.A.2: Codex CLI (`npm i -g @openai/codex`).
    /// Needs Node.js — the step will fail with a clear hint if
    /// `npm` isn't on PATH yet.
    InstallCodex,
    /// New in Phase 14.A.2: Gemini CLI (`npm i -g @google/gemini-cli@latest`).
    /// Same Node.js dependency as Codex.
    InstallGemini,
    /// Phase 18: append `~/.ymux/bin` to the new user's shell rc
    /// file. Runs as the new user via `sudo -u`. Idempotent — the
    /// bootstrap auto-fires the same snippet on every connect, so
    /// running this step after a fresh provisioning typically
    /// reports "already configured" (no-op). The point is to make
    /// the action visible + checkable in the wizard UI.
    AddYmuxToPath,
    SetupYmuxHooks,
}

impl StepKind {
    pub fn label(&self) -> &'static str {
        match self {
            StepKind::UpdatePackages => "Update packages",
            StepKind::InstallBasics => "Install basics (tmux, curl, git…)",
            StepKind::CreateUser => "Create user with sudo",
            StepKind::GenerateKeypair => "Generate keypair locally",
            StepKind::DeployPubkey => "Deploy public key",
            StepKind::TestNewKey => "Test new key login",
            StepKind::DisableRootSsh => "Disable root SSH password login",
            StepKind::DisablePasswordSsh => "Disable password auth on SSH",
            StepKind::InstallNodejs => "Install Node.js LTS",
            StepKind::InstallPython => "Install Python 3 + pip + venv",
            StepKind::InstallDocker => "Install Docker (official repo)",
            StepKind::InstallClaudeCode => "Install Claude Code (Anthropic, curl installer)",
            StepKind::InstallCodex => "Install Codex CLI (OpenAI, npm)",
            StepKind::InstallGemini => "Install Gemini CLI (Google, npm)",
            StepKind::AddYmuxToPath => "Add ymux to PATH (~/.bashrc or equivalent)",
            StepKind::SetupYmuxHooks => "Run ymux setup-hooks",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct ProvisioningProfile {
    pub id: String,
    pub label: String,
    /// Ordered list of steps. Each step's args (user name, etc.) live on
    /// the run input — profiles are templates, not instances.
    pub steps: Vec<StepKind>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfilesFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub profiles: Vec<ProvisioningProfile>,
}

fn default_version() -> u32 {
    1
}

pub(crate) fn default_profiles() -> Vec<ProvisioningProfile> {
    // Note: the Claude Code installer doesn't need npm (its `curl
    // install.sh | bash` ships a self-contained launcher), so we
    // drop the previous `InstallNodejs` prerequisite from the default
    // profile. Profiles that include Codex / Gemini DO list
    // InstallNodejs explicitly first since both still install via npm.
    vec![
        ProvisioningProfile {
            id: "default".into(),
            label: "Default — basics + user + key + Claude Code".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
                StepKind::InstallClaudeCode,
                StepKind::AddYmuxToPath,
                StepKind::SetupYmuxHooks,
            ],
        },
        ProvisioningProfile {
            id: "hardened".into(),
            label: "Hardened — default + disable root + disable password".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
                StepKind::DisableRootSsh,
                StepKind::DisablePasswordSsh,
                StepKind::InstallClaudeCode,
                StepKind::AddYmuxToPath,
                StepKind::SetupYmuxHooks,
            ],
        },
        ProvisioningProfile {
            id: "minimal".into(),
            label: "Minimal — update + user + key only".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
            ],
        },
        ProvisioningProfile {
            id: "all-agents".into(),
            label: "All agents — Claude Code + Codex + Gemini".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
                // Codex + Gemini both ship via npm; Node.js must be on
                // PATH first.
                StepKind::InstallNodejs,
                StepKind::InstallClaudeCode,
                StepKind::InstallCodex,
                StepKind::InstallGemini,
                StepKind::AddYmuxToPath,
                StepKind::SetupYmuxHooks,
            ],
        },
    ]
}

fn profiles_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("provisioning-profiles.json"))
}

fn secrets_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("provisioning-secrets.json"))
}

pub(crate) fn load_profiles_from_disk() -> Result<ProfilesFile, String> {
    let path = profiles_path()?;
    if !path.exists() {
        return Ok(ProfilesFile {
            version: 1,
            profiles: default_profiles(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut file: ProfilesFile = serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse {path:?}: {e}"))?;
    // Seed any missing built-ins so future built-in profiles light up
    // automatically without nuking the user's edits.
    let mut have: std::collections::HashSet<String> =
        file.profiles.iter().map(|p| p.id.clone()).collect();
    for d in default_profiles() {
        if !have.contains(&d.id) {
            have.insert(d.id.clone());
            file.profiles.push(d);
        }
    }
    Ok(file)
}

fn save_profiles_to_disk(file: &ProfilesFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = profiles_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("provisioning-profiles.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {tmp:?}: {e}"))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("write: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

// ─── secret storage (DPAPI-wrapped initial password) ───────────────────────

#[derive(Clone, Serialize, Deserialize, Default)]
struct SecretsFile {
    #[serde(default)]
    entries: std::collections::BTreeMap<String, String>, // workspace_id → b64(ciphertext)
}

/// Wrap secret bytes with Windows DPAPI. PowerShell shell-out keeps us
/// off the windows-rs dep tree. The encrypted blob is bound to the
/// current user's profile and machine — moving the JSON file to another
/// user account yields gibberish.
#[cfg(target_os = "windows")]
fn dpapi_protect(secret: &str) -> Result<String, String> {
    let secret = secret.to_string();
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference = 'Stop'; \
             Add-Type -AssemblyName System.Security; \
             $in = $env:YMUX_SECRET; \
             $bytes = [System.Text.Encoding]::UTF8.GetBytes($in); \
             $prot = [System.Security.Cryptography.ProtectedData]::Protect($bytes, $null, 'CurrentUser'); \
             [Convert]::ToBase64String($prot)",
        ])
        .env("YMUX_SECRET", &secret)
        .output()
        .map_err(|e| format!("dpapi spawn: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// No at-rest wrapper exists off Windows, so there is nothing to write.
///
/// This used to return `noprotect:<secret>`, and `save_workspace_secret`
/// wrote that verbatim into `provisioning-secrets.json` — i.e. the user's
/// SSH password in plaintext, at default permissions, in the config dir.
/// The premise in the old comment ("non-Windows builds are just the
/// cross-compile for ymux-linux-x64, provisioning runs UI-side on
/// Windows") stopped being true when the desktop shipped for macOS.
///
/// Rule #2 sanctions exactly this: DPAPI when persistence is possible,
/// "otherwise keep in memory only". Deliberately NOT a Keychain
/// integration — see the note on `save_workspace_secret`: nothing in the
/// tree ever reads this store back, so wiring one up would be building a
/// feature nobody asked for around a value nobody consumes.
#[cfg(not(target_os = "windows"))]
fn dpapi_protect(_secret: &str) -> Result<String, String> {
    Err("no at-rest secret store on this platform (Rule #2: memory only)".into())
}

/// Persist the initial password, wrapped, keyed by workspace.
///
/// NOTE: write-only. Nothing in the tree reads this file back — there is
/// no `dpapi_unprotect` and no loader — so today it is groundwork for a
/// "remember this password" feature that does not exist yet. The caller
/// already treats failure as non-fatal (it logs and continues), which is
/// what makes the unix `Err` above safe: provisioning is unaffected.
/// FOLLOWUPS P2 tracks giving it a reader or deleting it outright.
fn save_workspace_secret(workspace_id: &str, password: &str) -> Result<(), String> {
    // Wrap first: on a platform with no at-rest store this bails before we
    // touch the file at all, so nothing half-writes and no empty
    // provisioning-secrets.json appears in a macOS config dir.
    let wrapped = dpapi_protect(password)?;
    let path = secrets_path()?;
    let mut file: SecretsFile = if path.exists() {
        let t = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&t).unwrap_or_default()
    } else {
        SecretsFile::default()
    };
    file.entries.insert(workspace_id.to_string(), wrapped);
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(())
}

// ─── inspect (step 1 of wizard, before any mutation) ───────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct InspectResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_pretty_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whoami: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub df_h: Option<String>,
}

/// One-shot remote inspection. Opens a russh handle, runs a fixed bundle
/// of `bash -lc` commands, then drops the connection. The wizard shows
/// this to the user before they pick a profile so they know what OS
/// they're about to provision.
#[tauri::command]
pub(crate) async fn provisioning_inspect(
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    key_path: Option<String>,
    key_passphrase: Option<String>,
) -> InspectResult {
    match provisioning_inspect_inner(host, port, user, password, key_path, key_passphrase).await {
        Ok(r) => r,
        Err(e) => InspectResult {
            ok: false,
            message: Some(e),
            uname: None,
            os_pretty_name: None,
            os_id: None,
            os_version: None,
            package_manager: None,
            whoami: None,
            df_h: None,
        },
    }
}

async fn provisioning_inspect_inner(
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    key_path: Option<String>,
    key_passphrase: Option<String>,
) -> Result<InspectResult, String> {
    let mut handle = open_ssh(&host, port, &user, &password, &key_path, &key_passphrase).await?;
    let uname = exec_capture(&mut handle, "uname -a").await.ok();
    let os_release = exec_capture(&mut handle, "cat /etc/os-release 2>/dev/null || true")
        .await
        .ok();
    let whoami = exec_capture(&mut handle, "whoami").await.ok();
    let df_h = exec_capture(&mut handle, "df -h / 2>/dev/null | tail -n 1")
        .await
        .ok();
    let pm = exec_capture(
        &mut handle,
        "command -v apt >/dev/null && echo apt && exit; \
         command -v dnf >/dev/null && echo dnf && exit; \
         command -v yum >/dev/null && echo yum && exit; \
         command -v apk >/dev/null && echo apk && exit; \
         echo unknown",
    )
    .await
    .ok();

    let (pretty, id, version) = parse_os_release(os_release.as_deref().unwrap_or(""));
    Ok(InspectResult {
        ok: true,
        message: None,
        uname,
        os_pretty_name: pretty,
        os_id: id,
        os_version: version,
        package_manager: pm,
        whoami,
        df_h,
    })
}

fn parse_os_release(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut pretty = None;
    let mut id = None;
    let mut version = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            pretty = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("ID=") {
            id = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            version = Some(v.trim_matches('"').to_string());
        }
    }
    (pretty, id, version)
}

// ─── live run ──────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ProvisionInput {
    /// Stable id for the provisioning *run* — separate from the
    /// workspace it creates. Used to key the secret store and
    /// (optionally) re-use a sandbox id if the wizard was launched
    /// from a pre-existing workspace (right-click → "Run provisioning
    /// on this server", future).
    pub workspace_id: String,
    pub host: String,
    pub port: u16,
    pub initial_user: String,
    #[serde(default)]
    pub initial_password: Option<String>,
    #[serde(default)]
    pub initial_key_path: Option<String>,
    #[serde(default)]
    pub initial_key_passphrase: Option<String>,
    pub new_user: String,
    /// Local path to drop the freshly-generated keypair. If absent we
    /// compute `~/.ssh/ymux-<workspace>-<new_user>`.
    #[serde(default)]
    pub local_key_path: Option<String>,
    pub profile_id: String,
    /// Phase 14.A.2: name to give the auto-created workspace once the
    /// new key is verified to work. Defaults (frontend-side) to a
    /// host-derived label like "myserver" from "myserver.com". If
    /// blank we fall back to the host string as-is.
    #[serde(default)]
    pub workspace_name: Option<String>,
    /// Phase 14.A.2: when set, we attach the run to an existing
    /// workspace and *replace* its connection on success rather than
    /// creating a new one. Lets a right-click → "Run provisioning"
    /// flow upgrade root+password to runner+key in place. None means
    /// "create a fresh workspace".
    #[serde(default)]
    pub existing_workspace_id: Option<String>,
}

// Phase 32.A: structured error variant. Carried inside StepProgress
// alongside the legacy `message` field — old frontends keep working;
// new ones switch on `error.kind` for dedicated UIs. Tag/content
// representation gives clean TS unions (kind: "SudoRequired" | …).
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "kind", content = "details")]
pub(crate) enum ProvisioningError {
    /// Preflight detected the login user is neither root nor a
    /// passwordless sudoer. Provisioning aborts before any step runs.
    SudoRequired {
        user: String,
        raw_stderr: String,
    },
    /// A privileged step exited non-zero. `stderr` is the raw remote
    /// output (concatenation of stdout+stderr from exec_capture), NOT
    /// flattened into a "exit N: …" string — the frontend renders it
    /// expanded so the user can see exactly what went wrong.
    StepFailed {
        step: String,
        exit_code: i32,
        stderr: String,
    },
    /// Phase 80: a LOCAL install step (local_setup.rs engine) needs
    /// administrator elevation and/or a reboot (WSL feature enable).
    /// The wizard renders an instruction card; the run stays retryable.
    ElevationRequired {
        step: String,
        hint: String,
    },
    /// The step succeeded as far as Windows servicing is concerned
    /// (ERROR_SUCCESS_REBOOT_REQUIRED / 3010) but the features only come
    /// alive after a restart. Distinct from ElevationRequired: retrying
    /// without rebooting cannot possibly work, so the wizard offers a
    /// restart instead of a retry.
    RebootRequired {
        step: String,
        hint: String,
    },
    /// Fallback for failures that don't fit the structured cases
    /// (SSH connect, local key generation, etc.).
    Generic(String),
}

impl ProvisioningError {
    pub fn user_message(&self) -> String {
        match self {
            ProvisioningError::SudoRequired { user, .. } => format!(
                "User '{user}' needs passwordless sudo. Log in as root or add a NOPASSWD sudoers entry."
            ),
            ProvisioningError::StepFailed { step, exit_code, .. } => {
                format!("Step '{step}' failed (exit {exit_code})")
            }
            ProvisioningError::ElevationRequired { step, hint } => {
                format!("Step '{step}' needs administrator elevation. {hint}")
            }
            ProvisioningError::RebootRequired { step, hint } => {
                format!("Step '{step}' needs a restart to finish. {hint}")
            }
            ProvisioningError::Generic(s) => s.clone(),
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct StepProgress {
    pub run_id: String,
    pub step_index: usize,
    pub step_kind: String,
    pub state: &'static str, // "running" | "done" | "failed" | "skipped"
    pub log_chunk: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    // Phase 32.A: structured error. When present, takes precedence
    // over `message` in the UI — frontend switches on `error.kind`
    // for dedicated rendering (SudoRequired modal, StepFailed pre).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProvisioningError>,
    pub timestamp_iso: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct RunHandle {
    pub run_id: String,
}

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
fn new_run_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("prov_{t:x}_{n:x}")
}

// Phase 80: pub(crate) — the local_setup engine stamps the same
// StepProgress payloads.
pub(crate) fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Spawn a provisioning task and return a handle. The task emits
/// `provisioning:progress` events with `StepProgress` payloads. The
/// initial password (if provided) is DPAPI-wrapped and saved alongside
/// the workspace so a later "View server info" can recover it.
#[tauri::command]
pub(crate) async fn provisioning_start(
    state: State<'_, AppState>,
    app: AppHandle,
    input: ProvisionInput,
) -> Result<RunHandle, String> {
    let _ = state;
    let run_id = new_run_id();
    let profile = {
        let pf = load_profiles_from_disk()?;
        pf.profiles
            .iter()
            .find(|p| p.id == input.profile_id)
            .cloned()
            .ok_or_else(|| format!("unknown profile {}", input.profile_id))?
    };
    if let Some(pw) = input.initial_password.as_ref() {
        if let Err(e) = save_workspace_secret(&input.workspace_id, pw) {
            log_warn("PROVISION", &format!("provisioning: save secret failed: {e}"));
        }
    }

    let app_for_task = app.clone();
    let run_id_clone = run_id.clone();
    // Clone AppState into the task so the workspace-creation step at
    // the end of the run can persist directly through the same
    // workspaces.json + locks as the rest of the app.
    let state_for_task: AppState = (*state).clone();
    tauri::async_runtime::spawn(async move {
        run_provisioning(app_for_task, state_for_task, run_id_clone, input, profile).await;
    });

    Ok(RunHandle { run_id })
}

async fn run_provisioning(
    app: AppHandle,
    state: AppState,
    run_id: String,
    input: ProvisionInput,
    profile: ProvisioningProfile,
) {
    let mut handle = match open_ssh(
        &input.host,
        input.port,
        &input.initial_user,
        &input.initial_password,
        &input.initial_key_path,
        &input.initial_key_passphrase,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            emit(
                &app,
                &run_id,
                0,
                &StepKind::UpdatePackages,
                "failed",
                String::new(),
                Some(format!("SSH connect failed: {e}")),
                Some(ProvisioningError::Generic(format!("SSH connect failed: {e}"))),
            );
            return;
        }
    };

    // Phase 32.A: sudo preflight. Fails fast with a dedicated
    // SudoRequired event if the login user lacks passwordless sudo
    // and isn't root — saves the user from a confusing "exit 1" on
    // the first apt-get step.
    if let Err(err) = preflight_sudo(&mut handle, &input.initial_user).await {
        emit_preflight_failure(&app, &run_id, err);
        return;
    }

    // Detect package manager once and cache.
    let pm = exec_capture(
        &mut handle,
        "command -v apt >/dev/null && echo apt && exit; \
         command -v dnf >/dev/null && echo dnf && exit; \
         command -v yum >/dev/null && echo yum && exit; \
         command -v apk >/dev/null && echo apk && exit; \
         echo unknown",
    )
    .await
    .unwrap_or_else(|_| "unknown".into())
    .trim()
    .to_string();

    let local_key_path = input.local_key_path.clone().unwrap_or_else(|| {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        // Host separator, not a literal backslash: the old
        // `format!("{home}\\.ssh\\ymux-…")` produced
        // `/Users/yossi\.ssh\ymux-…` on macOS — one bogus file name
        // in $HOME instead of a key under ~/.ssh.
        std::path::Path::new(&home)
            .join(".ssh")
            .join(format!("ymux-{}-{}", input.workspace_id, input.new_user))
            .to_string_lossy()
            .into_owned()
    });

    // Phase 14.A.2 outcome tracking. Three milestones must all hit OK
    // for the auto-workspace path to fire at the end. Anything else is
    // just informational — the workspace can still be created even if,
    // say, the Docker install failed, because that doesn't affect
    // whether `runner@host` can SSH in with the new key.
    let mut keypair_ok = false;
    let mut deploy_ok = false;
    let mut test_ok = false;
    let mut claude_installed = false;

    for (idx, kind) in profile.steps.iter().enumerate() {
        emit(&app, &run_id, idx, kind, "running", String::new(), None, None);
        let result = match kind {
            StepKind::UpdatePackages => {
                let cmd = match pm.as_str() {
                    "apt" => "sudo apt-get update -y && sudo DEBIAN_FRONTEND=noninteractive apt-get upgrade -y",
                    "dnf" => "sudo dnf -y upgrade",
                    "yum" => "sudo yum -y update",
                    "apk" => "sudo apk update && sudo apk upgrade",
                    _ => "echo unknown package manager; exit 1",
                };
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallBasics => {
                let pkgs = "tmux curl wget git build-essential vim htop ca-certificates";
                let cmd = match pm.as_str() {
                    "apt" => format!("sudo DEBIAN_FRONTEND=noninteractive apt-get install -y {pkgs}"),
                    "dnf" => format!("sudo dnf -y install tmux curl wget git make gcc gcc-c++ vim htop ca-certificates"),
                    "yum" => format!("sudo yum -y install tmux curl wget git make gcc gcc-c++ vim htop ca-certificates"),
                    "apk" => "sudo apk add tmux curl wget git build-base vim htop ca-certificates".to_string(),
                    _ => "echo unknown package manager; exit 1".to_string(),
                };
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::CreateUser => {
                let u = shell_escape(&input.new_user);
                // Create user + add to sudo + passwordless sudo. Idempotent
                // via `id -u` short-circuit.
                let cmd = format!(
                    "if id -u {u} >/dev/null 2>&1; then \
                       echo 'user already exists'; \
                     else \
                       sudo useradd -m -s /bin/bash {u} && \
                       sudo usermod -aG sudo {u} 2>/dev/null || sudo usermod -aG wheel {u}; \
                     fi; \
                     echo '{u} ALL=(ALL) NOPASSWD:ALL' | sudo tee /etc/sudoers.d/90-ymux-{u} >/dev/null && \
                     sudo chmod 0440 /etc/sudoers.d/90-ymux-{u}"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::GenerateKeypair => {
                // Local step — uses ssh-keygen.exe (ships with Windows 10+).
                let r = local_step_generate_keypair(&local_key_path, &input.new_user).await;
                match r {
                    Ok(out) => {
                        emit(&app, &run_id, idx, kind, "done", out, None, None);
                        Ok(())
                    }
                    Err(e) => {
                        let err = ProvisioningError::Generic(e.clone());
                        emit(&app, &run_id, idx, kind, "failed", String::new(), Some(e), Some(err));
                        Err(())
                    }
                }
            }
            StepKind::DeployPubkey => {
                let pub_path = format!("{}.pub", local_key_path);
                match std::fs::read_to_string(&pub_path) {
                    Ok(pub_text) => {
                        let u = shell_escape(&input.new_user);
                        let key_line = shell_escape(pub_text.trim());
                        let cmd = format!(
                            "sudo install -d -m 700 -o {u} -g {u} /home/{u}/.ssh && \
                             echo {key_line} | sudo tee -a /home/{u}/.ssh/authorized_keys >/dev/null && \
                             sudo chown {u}:{u} /home/{u}/.ssh/authorized_keys && \
                             sudo chmod 600 /home/{u}/.ssh/authorized_keys"
                        );
                        run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
                    }
                    Err(e) => {
                        let msg = format!("read {pub_path}: {e}");
                        let err = ProvisioningError::Generic(msg.clone());
                        emit(
                            &app,
                            &run_id,
                            idx,
                            kind,
                            "failed",
                            String::new(),
                            Some(msg),
                            Some(err),
                        );
                        Err(())
                    }
                }
            }
            StepKind::TestNewKey => {
                // Open a SECOND SSH handle with the just-deployed key.
                let r = open_ssh(
                    &input.host,
                    input.port,
                    &input.new_user,
                    &None,
                    &Some(local_key_path.clone()),
                    &None,
                )
                .await;
                match r {
                    Ok(mut h2) => {
                        let who = exec_capture(&mut h2, "whoami").await.unwrap_or_default();
                        emit(
                            &app,
                            &run_id,
                            idx,
                            kind,
                            "done",
                            format!("connected as {who}"),
                            None,
                            None,
                        );
                        Ok(())
                    }
                    Err(e) => {
                        let err = ProvisioningError::Generic(e.clone());
                        emit(&app, &run_id, idx, kind, "failed", String::new(), Some(e), Some(err));
                        Err(())
                    }
                }
            }
            StepKind::DisableRootSsh => {
                let cmd = "sudo sed -i -E 's/^[# ]*PermitRootLogin.*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config && \
                           (sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd 2>/dev/null || sudo service ssh reload)";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::DisablePasswordSsh => {
                let cmd = "sudo sed -i -E 's/^[# ]*PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config && \
                           (sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd 2>/dev/null || sudo service ssh reload)";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallNodejs => {
                let cmd = "curl -fsSL https://deb.nodesource.com/setup_lts.x | sudo -E bash - && \
                           sudo DEBIAN_FRONTEND=noninteractive apt-get install -y nodejs";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallPython => {
                let cmd = match pm.as_str() {
                    "apt" => "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y python3 python3-pip python3-venv",
                    "dnf" => "sudo dnf -y install python3 python3-pip",
                    "yum" => "sudo yum -y install python3 python3-pip",
                    "apk" => "sudo apk add python3 py3-pip",
                    _ => "echo unsupported pm; exit 1",
                };
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallDocker => {
                let cmd = "curl -fsSL https://get.docker.com | sudo sh && \
                           sudo usermod -aG docker $(whoami)";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallClaudeCode => {
                // Official Anthropic installer (https://claude.ai/install.sh).
                // Self-contained — no npm prerequisite. Runs as the new
                // user so `~/.claude` lands in the right home directory.
                // `su -l` would re-evaluate the shell rc files (which the
                // installer relies on for PATH bumps) but isn't available
                // in all minimal images; we fall back to `sudo -u … bash
                // -lc` which has the same effect.
                let u = shell_escape(&input.new_user);
                let cmd = format!(
                    "sudo -u {u} bash -lc 'curl -fsSL https://claude.ai/install.sh | bash' && \
                     echo '✓ Claude Code installed. Run `claude` on the server to authenticate (browser-based) \
                     or set ANTHROPIC_API_KEY for headless mode.'"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::InstallCodex => {
                // Codex ships through npm — fail fast with a clear hint
                // if Node isn't on PATH yet rather than letting npm's
                // error spill into the log.
                let u = shell_escape(&input.new_user);
                let cmd = format!(
                    "if ! command -v npm >/dev/null; then echo 'ERROR: npm not found. Enable the \"Install Node.js LTS\" step first, or install Node manually.' && exit 1; fi; \
                     sudo npm install -g @openai/codex && \
                     sudo chown -R {u}:{u} /home/{u}/.npm /home/{u}/.codex 2>/dev/null || true; \
                     echo '✓ Codex installed. Run `codex login --device-auth` on the server for headless auth, or `codex login` if you have a desktop browser.'"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::InstallGemini => {
                let u = shell_escape(&input.new_user);
                let cmd = format!(
                    "if ! command -v npm >/dev/null; then echo 'ERROR: npm not found. Enable the \"Install Node.js LTS\" step first, or install Node manually.' && exit 1; fi; \
                     sudo npm install -g @google/gemini-cli@latest && \
                     sudo chown -R {u}:{u} /home/{u}/.npm /home/{u}/.gemini 2>/dev/null || true; \
                     echo '✓ Gemini CLI installed. Run `gemini` to sign in with Google, or set GEMINI_API_KEY for headless mode.'"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::AddYmuxToPath => {
                // Phase 18: append `~/.ymux/bin` to the new user's
                // shell rc. Runs the bootstrap's PATH_RC_SNIPPET
                // under `sudo -u <new_user>` so the rc that gets
                // touched is the runner's, not root's.
                //
                // The snippet outputs ADDED <rc> / EXISTS <rc> /
                // ERROR <msg>. We pretty-print it so the wizard
                // log shows "PATH configured in /home/runner/.bashrc"
                // or "already configured" rather than the raw token.
                let u = shell_escape(&input.new_user);
                let snippet = crate::remote_bootstrap::PATH_RC_SNIPPET;
                // -H sets HOME to the target user's home so $HOME
                // expansions inside the snippet point at the
                // runner's directory. The single-quoted heredoc tag
                // (`<<'YMUX_PATH_EOF'`) means bash doesn't expand
                // anything while READING the body — the inner bash
                // does the expansion when it executes.
                let cmd = format!(
                    "sudo -Hu {u} bash <<'YMUX_PATH_EOF'\n{snippet}\nYMUX_PATH_EOF\n\
                     # Translate the snippet's machine-readable output to a\n\
                     # user-friendly status line. Last line is what the\n\
                     # wizard log surfaces.\n\
                     :"
                );
                // Run + post-process the captured output.
                let r = match exec_capture(&mut handle, &cmd).await {
                    Ok(raw) => {
                        let line = raw.trim().lines().last().unwrap_or("").trim().to_string();
                        let pretty = if let Some(rc) = line.strip_prefix("ADDED ") {
                            format!("✓ PATH configured in {rc}")
                        } else if let Some(rc) = line.strip_prefix("EXISTS ") {
                            format!("✓ already configured in {rc}")
                        } else if let Some(msg) = line.strip_prefix("ERROR ") {
                            format!("✗ {msg}")
                        } else {
                            raw.trim().to_string()
                        };
                        emit(&app, &run_id, idx, kind, "done", pretty, None, None);
                        Ok(())
                    }
                    Err(e) => {
                        let err = ProvisioningError::Generic(e.clone());
                        emit(&app, &run_id, idx, kind, "failed", String::new(), Some(e), Some(err));
                        Err(())
                    }
                };
                r
            }
            StepKind::SetupYmuxHooks => {
                // Best-effort — ymux CLI on remote is bootstrapped on
                // first SSH connection from the desktop app, so it may
                // not be there yet. Wrap in `command -v`.
                let cmd = "if command -v ymux >/dev/null; then ymux setup-hooks --agent claude || true; else echo 'ymux CLI not yet bootstrapped — connect once to install, then re-run'; fi";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
        };
        // Track outcomes for the auto-workspace step at the end of
        // the run. We only flip flags when a step *succeeded* —
        // `result` is Err on any non-zero exit / pipe failure.
        if result.is_ok() {
            match kind {
                StepKind::GenerateKeypair => keypair_ok = true,
                StepKind::DeployPubkey => deploy_ok = true,
                StepKind::TestNewKey => test_ok = true,
                StepKind::InstallClaudeCode => claude_installed = true,
                _ => {}
            }
        } else {
            log_warn("PROVISION", &format!(
                "provisioning {run_id}: step {idx} {kind:?} failed — leaving run paused for retry"
            ));
            // Don't auto-abort; the wizard surfaces a retry button.
            // We continue here to mirror the spec ("retry OR skip").
        }
    }

    // Phase 14.A.2: auto-create / upgrade the workspace when the key
    // pipeline succeeded end-to-end.
    let mut created_workspace_id: Option<String> = None;
    let mut created_workspace_name: Option<String> = None;
    if keypair_ok && deploy_ok && test_ok {
        match finalize_workspace(&state, &app, &input, &local_key_path) {
            Ok((id, name)) => {
                created_workspace_id = Some(id);
                created_workspace_name = Some(name);
            }
            Err(e) => {
                log_error("PROVISION", &format!(
                    "provisioning {run_id}: workspace creation failed: {e}"
                ));
            }
        }
    } else {
        log_warn("PROVISION", &format!(
            "provisioning {run_id}: skipping workspace creation (keypair_ok={keypair_ok} deploy_ok={deploy_ok} test_ok={test_ok})"
        ));
    }

    // Final progress event marks the run as fully done. The frontend
    // listens for `state == "done"` with `step_kind == "Complete"` to
    // pick up the created workspace id and offer the "Open it now"
    // buttons.
    emit(
        &app,
        &run_id,
        profile.steps.len(),
        &StepKind::UpdatePackages,
        "done",
        String::new(),
        Some("provisioning complete".into()),
        None,
    );
    let _ = app.emit(
        "provisioning:complete",
        ProvisionResult {
            run_id: run_id.clone(),
            workspace_id: created_workspace_id,
            workspace_name: created_workspace_name,
            claude_installed,
            host: input.host.clone(),
        },
    );
}

/// Payload emitted to the frontend right after the per-step loop ends.
/// `workspace_id` is None when the key pipeline didn't complete — the
/// UI uses that to decide whether to show the "Open it now" CTAs.
#[derive(Clone, Serialize)]
pub(crate) struct ProvisionResult {
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    pub claude_installed: bool,
    pub host: String,
}

/// Build the SSH connection out of the freshly-deployed credentials
/// and either (a) replace the connection on the existing workspace
/// the wizard was attached to, or (b) create a brand new workspace.
/// Returns (workspace_id, display_name) on success.
fn finalize_workspace(
    state: &AppState,
    app: &AppHandle,
    input: &ProvisionInput,
    local_key_path: &str,
) -> Result<(String, String), String> {
    use crate::{
        new_pane_id, new_workspace_id, persist, Connection, LayoutNode, PaneKind, Workspace,
    };

    let new_conn = Connection::Ssh {
        host: input.host.clone(),
        user: input.new_user.clone(),
        port: input.port,
        key_path: Some(local_key_path.to_string()),
    };

    // Branch 1: caller pointed us at an existing workspace — upgrade
    // its connection in place. We rewrite the FIRST pane's connection
    // so existing splits, browser panes, file-manager panes etc. all
    // pick up the new auth on next connect.
    if let Some(existing_id) = input.existing_workspace_id.as_ref() {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| &w.id == existing_id)
            .ok_or_else(|| format!("existing workspace {existing_id} not found"))?;
        // Rewrite the top-level connection legacy field + the first
        // pane's connection — both are kept in sync on load.
        ws.connection = Some(new_conn.clone());
        if let Some(layout) = ws.layout.as_mut() {
            rewrite_first_terminal_conn(layout, &new_conn);
        }
        let display_name = ws.name.clone();
        let id_out = ws.id.clone();
        drop(file);
        persist(state)?;
        let _ = app.emit("workspaces:changed", ());
        return Ok((id_out, display_name));
    }

    // Branch 2: fresh workspace. Name = either the user's chosen
    // workspace_name, or a host-derived label fallback.
    let display_name = input
        .workspace_name
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| derive_workspace_name(&input.host));

    let pane_id = new_pane_id();
    let layout = LayoutNode::Pane {
        pane_id,
        pane_kind: PaneKind::Terminal,
        connection: Some(new_conn.clone()),
        browser: None,
        title: None,
        auto_title: None,
        annotation: None,
        color: None,
        emoji: None,
        help_topic: None,
        diff_source: None,
        smart_bidi: None,
        diff_cwd: None,
    };
    let ws = Workspace {
        id: new_workspace_id(),
        name: display_name.clone(),
        // Cycle through a small accent palette so each newly-provisioned
        // workspace gets a distinct sidebar dot rather than all sharing
        // the same colour. Choice is deterministic by host so the same
        // server gets the same colour across re-provisions.
        color: Some(workspace_color_for_host(&input.host)),
        connection: Some(new_conn),
        layout: Some(layout),
        ..Default::default()
    };
    let id_out = ws.id.clone();
    {
        let mut file = state.workspaces.lock().unwrap();
        file.active_workspace_id = Some(id_out.clone());
        file.workspaces.push(ws);
    }
    persist(state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok((id_out, display_name))
}

/// Walk a layout tree and rewrite the connection on the first terminal
/// pane we encounter. Used by the in-place upgrade path so existing
/// splits inherit the new key without losing their layout.
fn rewrite_first_terminal_conn(node: &mut crate::LayoutNode, new_conn: &crate::Connection) {
    use crate::{LayoutNode, PaneKind};
    match node {
        LayoutNode::Pane {
            pane_kind,
            connection,
            ..
        } => {
            if matches!(pane_kind, PaneKind::Terminal) {
                *connection = Some(new_conn.clone());
            }
        }
        LayoutNode::Split { first, second, .. } => {
            rewrite_first_terminal_conn(first, new_conn);
            rewrite_first_terminal_conn(second, new_conn);
        }
    }
}

fn derive_workspace_name(host: &str) -> String {
    // "myserver.example.com" → "myserver"
    // "203.0.113.5" → "203.0.113.5" (no useful slicing on IPs)
    let trimmed = host.trim();
    let first_label = trimmed.split('.').next().unwrap_or(trimmed);
    if first_label.chars().all(|c| c.is_ascii_digit()) {
        trimmed.to_string()
    } else {
        first_label.to_string()
    }
}

// Phase 80: pub(crate) — local_setup's WSL-workspace finalize reuses it.
pub(crate) fn workspace_color_for_host(host: &str) -> String {
    const PALETTE: &[&str] = &[
        "#7aa2f7", // accent blue
        "#4ec9b0", // success teal
        "#bb9af7", // purple
        "#e0af68", // warning amber
        "#f7768e", // error pink
        "#9ece6a", // green
        "#7dcfff", // cyan
    ];
    let h = host.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h as usize) % PALETTE.len()].to_string()
}

async fn local_step_generate_keypair(local_path: &str, comment: &str) -> Result<String, String> {
    if std::path::Path::new(local_path).exists() {
        return Ok(format!("reusing existing local key {local_path}"));
    }
    if let Some(parent) = std::path::Path::new(local_path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    let out = std::process::Command::new("ssh-keygen")
        .args([
            "-t", "ed25519", "-N", "", "-C",
        ])
        .arg(format!("ymux/{comment}"))
        .arg("-f")
        .arg(local_path)
        .output()
        .map_err(|e| format!("ssh-keygen spawn: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    Ok(format!("generated {local_path} (ed25519)"))
}

fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '@' | ':' | '=' | ',' | '%'))
    {
        return s.into();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

async fn run_step(
    handle: &mut client::Handle<crate::SshClient>,
    app: &AppHandle,
    run_id: &str,
    idx: usize,
    kind: &StepKind,
    cmd: &str,
) -> Result<(), ()> {
    // Phase 32.A: use exec_status so we get the exit code separately
    // and can emit StepFailed { step, exit_code, stderr } instead of
    // the flattened "exit N: <stderr>" string.
    match exec_status(handle, cmd).await {
        Ok((0, out)) => {
            emit(app, run_id, idx, kind, "done", out, None, None);
            Ok(())
        }
        Ok((exit_code, stderr)) => {
            let err = ProvisioningError::StepFailed {
                step: format!("{kind:?}"),
                exit_code,
                stderr: stderr.clone(),
            };
            let msg = err.user_message();
            emit(
                app,
                run_id,
                idx,
                kind,
                "failed",
                stderr,
                Some(msg),
                Some(err),
            );
            Err(())
        }
        Err(e) => {
            // SSH channel itself failed — bucket as Generic.
            let err = ProvisioningError::Generic(e.clone());
            emit(app, run_id, idx, kind, "failed", String::new(), Some(e), Some(err));
            Err(())
        }
    }
}

fn emit(
    app: &AppHandle,
    run_id: &str,
    idx: usize,
    kind: &StepKind,
    state: &'static str,
    log_chunk: String,
    message: Option<String>,
    error: Option<ProvisioningError>,
) {
    let _ = app.emit(
        "provisioning:progress",
        StepProgress {
            run_id: run_id.to_string(),
            step_index: idx,
            step_kind: format!("{kind:?}"),
            state,
            log_chunk,
            message,
            error,
            timestamp_iso: iso_now(),
        },
    );
}

/// Phase 32.A: emit a non-step preflight failure. Used by sudo
/// preflight before any actual step runs. Reuses the StepProgress
/// channel so the wizard's existing event listener handles it.
fn emit_preflight_failure(
    app: &AppHandle,
    run_id: &str,
    err: ProvisioningError,
) {
    let _ = app.emit(
        "provisioning:progress",
        StepProgress {
            run_id: run_id.to_string(),
            step_index: 0,
            step_kind: "Preflight".to_string(),
            state: "failed",
            log_chunk: String::new(),
            message: Some(err.user_message()),
            error: Some(err),
            timestamp_iso: iso_now(),
        },
    );
}

// ─── ssh exec helpers ──────────────────────────────────────────────────────

/// Open a russh client::Handle using the project's SshClient + the same
/// password/key/agent ladder as `try_authenticate`. We don't reuse the
/// existing one in lib.rs to keep this module self-contained — the cost
/// is a duplicate ~30 lines of auth ladder.
async fn open_ssh(
    host: &str,
    port: u16,
    user: &str,
    password: &Option<String>,
    key_path: &Option<String>,
    key_passphrase: &Option<String>,
) -> Result<client::Handle<crate::SshClient>, String> {
    // Phase 38: keepalive every 30s — provisioning sessions can sit
    // idle between steps; don't let a NAT timeout drop them.
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let target = format!("{host}:{port}");
    let connect = client::connect(
        config,
        (host, port),
        crate::SshClient::new_anonymous(target.clone()),
    );
    let mut handle = tokio::time::timeout(std::time::Duration::from_secs(15), connect)
        .await
        .map_err(|_| "ssh connect timed out".to_string())?
        .map_err(|e| format!("ssh connect: {e}"))?;

    // Key auth (if key_path provided) using the std::fs::read_to_string +
    // decode_secret_key path we adopted in lib.rs's recent fix.
    if let Some(p) = key_path {
        let text = std::fs::read_to_string(p).map_err(|e| format!("read {p}: {e}"))?;
        let key = russh_keys::decode_secret_key(&text, key_passphrase.as_deref())
            .map_err(|e| format!("decode key {p}: {e}"))?;
        let pkwh = crate::pkwh_pub(key)?;
        if handle
            .authenticate_publickey(user, pkwh)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(handle);
        }
    }
    if let Some(pw) = password {
        if handle
            .authenticate_password(user, pw)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(handle);
        }
    }
    Err("authentication failed".to_string())
}

async fn exec_capture(
    handle: &mut client::Handle<crate::SshClient>,
    cmd: &str,
) -> Result<String, String> {
    let (code, text) = exec_status(handle, cmd).await?;
    if code != 0 {
        return Err(format!("exit {code}: {text}"));
    }
    Ok(text)
}

/// Phase 32.A: like `exec_capture`, but always returns (exit_code, output)
/// — even on non-zero exit. The Err arm is reserved for SSH channel
/// failures (lost connection, channel open refused). Callers that need
/// to distinguish "command ran and reported X" from "couldn't talk to
/// the host" use this; everything else stays on `exec_capture`.
async fn exec_status(
    handle: &mut client::Handle<crate::SshClient>,
    cmd: &str,
) -> Result<(i32, String), String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, cmd).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExtendedData { data, .. }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    Ok((exit_code, text))
}

/// Phase 32.A: probe whether the login user can run privileged
/// commands. Returns Ok(()) if root OR passwordless sudo works.
/// Returns SudoRequired with the exact stderr from `sudo -n true` so
/// the frontend can surface "incorrect password attempts will be logged"
/// vs "sudo: a password is required" vs "user not in sudoers".
async fn preflight_sudo(
    handle: &mut client::Handle<crate::SshClient>,
    user: &str,
) -> Result<(), ProvisioningError> {
    // Already root? sudo is irrelevant.
    if let Ok((code, out)) = exec_status(handle, "id -u").await {
        if code == 0 && out.trim() == "0" {
            log_debug("PROVISION", "provisioning: preflight — user is root, skipping sudo check");
            return Ok(());
        }
    }
    // Try passwordless sudo. The 2>&1 keeps stderr (the actual
    // diagnostic) on the same stream we capture.
    match exec_status(handle, "sudo -n true 2>&1").await {
        Ok((0, _)) => {
            log_debug("PROVISION", "provisioning: preflight — passwordless sudo works");
            Ok(())
        }
        Ok((_, stderr)) => Err(ProvisioningError::SudoRequired {
            user: user.to_string(),
            raw_stderr: stderr.trim().to_string(),
        }),
        Err(e) => Err(ProvisioningError::SudoRequired {
            user: user.to_string(),
            raw_stderr: format!("ssh channel error while probing sudo: {e}"),
        }),
    }
}

// ─── Phase 65: connect-to-existing-server discovery ────────────────────────

/// Phase 65: what a one-shot password probe learned about a server, used
/// to drive the "Connect to existing server" wizard's choice step.
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ServerDiscovery {
    /// The login user is uid 0.
    pub is_root: bool,
    /// Login user is root OR has passwordless sudo (i.e. can create a
    /// new user + grant sudo). Drives whether "create new user" is
    /// offered.
    pub can_sudo: bool,
    /// Admin group to add a new user to for sudo — "sudo" on
    /// Debian/Ubuntu, "wheel" on RHEL/Fedora. Defaults to "sudo".
    pub sudo_group: String,
    /// Real interactive accounts (uid 0 or 1000..65000, with a login
    /// shell — not nologin/false/sync), sorted + deduped.
    pub users: Vec<String>,
}

async fn connect_existing_discover_inner(
    host: &str,
    port: u16,
    user: &str,
    password: &Option<String>,
) -> Result<ServerDiscovery, String> {
    let mut handle = open_ssh(host, port, user, password, &None, &None).await?;

    let is_root = matches!(
        exec_status(&mut handle, "id -u").await,
        Ok((0, ref o)) if o.trim() == "0"
    );
    let can_sudo = is_root
        || matches!(exec_status(&mut handle, "sudo -n true 2>&1").await, Ok((0, _)));

    // Pick the admin group that exists on this distro.
    let sudo_group = if matches!(exec_status(&mut handle, "getent group sudo").await, Ok((0, _))) {
        "sudo".to_string()
    } else if matches!(exec_status(&mut handle, "getent group wheel").await, Ok((0, _))) {
        "wheel".to_string()
    } else {
        "sudo".to_string()
    };

    // Read-only account enumeration. Static command — no user input in
    // the shell string (Rule #3).
    let passwd = exec_capture(&mut handle, "getent passwd")
        .await
        .unwrap_or_default();
    let mut users: Vec<String> = Vec::new();
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 {
            continue;
        }
        let uid: u32 = f[2].parse().unwrap_or(u32::MAX);
        let shell = f[6];
        let bad_shell = shell.ends_with("nologin")
            || shell.ends_with("false")
            || shell.ends_with("/sync");
        // Phase 65.R-fix: only REAL login accounts are pickable as the
        // target. root (uid 0) is deliberately EXCLUDED — the whole point
        // of this flow is to set up a dedicated non-root key user, not to
        // re-connect as root. On a fresh VPS (root-only) this leaves the
        // list empty, which the UI uses to force "create a new user". The
        // `is_root` / `can_sudo` flags above still tell the UI that
        // creating a user is possible.
        let real = uid >= 1000 && uid < 65000;
        if real && !bad_shell && !f[0].is_empty() {
            users.push(f[0].to_string());
        }
    }
    users.sort();
    users.dedup();

    log_debug("PROVISION", &format!(
        "connect_existing_discover: host={host} user={user} is_root={is_root} can_sudo={can_sudo} group={sudo_group} accounts={}",
        users.len()
    ));
    Ok(ServerDiscovery {
        is_root,
        can_sudo,
        sudo_group,
        users,
    })
}

/// Phase 65: connect once with a password and discover what we can do on
/// the server (create users? which accounts exist?). The password is
/// used ONLY for this probe connection and is zeroized before returning;
/// it is NEVER written to disk (Rule #2). Every probe command is
/// read-only.
#[tauri::command]
pub(crate) async fn connect_existing_discover(
    host: String,
    port: u16,
    user: String,
    password: String,
) -> Result<ServerDiscovery, String> {
    use zeroize::Zeroize;
    let pw_opt = Some(password);
    let result = connect_existing_discover_inner(&host, port, &user, &pw_opt).await;
    if let Some(mut s) = pw_opt {
        s.zeroize();
    }
    result
}

// ─── Phase 65.B: connect-to-existing-server execute ────────────────────────

/// Phase 65.B: the local machine's name — used in the per-machine key
/// filename and the `authorized_keys` comment so multiple devices joining
/// the same account on the same server never collide.
fn local_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "ymux".to_string())
}

/// Phase 65.B: filesystem-safe token (keeps alnum / `-` / `_` / `.`).
fn sanitize_path_token(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Phase 65.B: 8 hex chars of uniqueness for the key filename (no rand
/// dep — low 32 bits of the wall clock; collisions across machines/times
/// are vanishingly unlikely for this use).
fn short_uuid() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:08x}", (n as u64) & 0xffff_ffff)
}

/// Phase 65.B: local path for the freshly-generated keypair —
/// `~/.ssh/ymux-<user>@<host>-<localhost>-<uuid>`. Ensures `~/.ssh`
/// exists (ssh-keygen won't create it).
fn build_connect_key_path(user: &str, host: &str) -> Result<String, String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "cannot resolve local home directory".to_string())?;
    let ssh_dir = std::path::Path::new(&home).join(".ssh");
    std::fs::create_dir_all(&ssh_dir).map_err(|e| format!("mkdir ~/.ssh: {e}"))?;
    let name = format!(
        "ymux-{}@{}-{}-{}",
        sanitize_path_token(user),
        sanitize_path_token(host),
        sanitize_path_token(&local_hostname()),
        short_uuid()
    );
    Ok(ssh_dir.join(name).to_string_lossy().to_string())
}

/// Phase 65.B: inputs for the "Connect to existing server" execute step.
#[derive(Clone, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ConnectExistingInput {
    pub host: String,
    pub port: u16,
    /// The account we connect AS with the password to do the setup work.
    pub auth_user: String,
    /// One-shot password — used only for the setup connection, zeroized
    /// after, never written to disk (Rule #2).
    pub password: String,
    /// The account we set up key-only access FOR (existing or new).
    pub target_user: String,
    /// true → create `target_user` (requires can_sudo); false → install
    /// the key for an existing account.
    pub create_new_user: bool,
    /// New-user only: add to the admin group for sudo.
    #[serde(default)]
    pub grant_sudo: bool,
    /// Admin group from discovery ("sudo" / "wheel").
    #[serde(default)]
    pub sudo_group: String,
    /// New workspace name (Branch 2), or None.
    #[serde(default)]
    pub workspace_name: Option<String>,
    /// When set, add this machine to an existing workspace instead of
    /// creating one (Branch 1).
    #[serde(default)]
    pub existing_workspace_id: Option<String>,
}

/// Phase 65.B: result of a successful connect-existing run.
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ConnectExistingResult {
    pub workspace_id: String,
    pub workspace_name: String,
    pub key_path: String,
}

async fn connect_existing_execute_inner(
    state: &AppState,
    app: &AppHandle,
    input: &ConnectExistingInput,
    auth_pw: &Option<String>,
) -> Result<ConnectExistingResult, String> {
    let host = &input.host;
    let port = input.port;
    let target = input.target_user.trim();
    if target.is_empty() {
        return Err("target user is empty".to_string());
    }

    // 1. Connect as the auth user with the password.
    let mut handle = open_ssh(host, port, &input.auth_user, auth_pw, &None, &None).await?;

    // Privilege prefix: nothing if we're already root, else `sudo `.
    let is_root = matches!(
        exec_status(&mut handle, "id -u").await,
        Ok((0, ref o)) if o.trim() == "0"
    );
    let sp = if is_root { "" } else { "sudo " };

    // 2. Create the user if requested (key-only: useradd + passwd -l;
    //    optional admin-group membership). Idempotent on an existing name.
    if input.create_new_user {
        let u = shell_escape(target);
        let grant = if input.grant_sudo {
            let grp = shell_escape(if input.sudo_group.is_empty() {
                "sudo"
            } else {
                &input.sudo_group
            });
            format!("{sp}usermod -aG {grp} {u}")
        } else {
            "true".to_string()
        };
        let cmd = format!(
            "if id -u {u} >/dev/null 2>&1; then echo 'user exists, reusing'; \
             else {sp}useradd -m -s /bin/bash {u} && {sp}passwd -l {u} >/dev/null; fi && {grant}"
        );
        exec_capture(&mut handle, &cmd)
            .await
            .map_err(|e| format!("create user '{target}': {e}"))?;
        log_info("PROVISION", &format!(
            "connect_existing: ensured user '{target}' (grant_sudo={}) on {host}",
            input.grant_sudo
        ));
    }

    // 3. Generate the per-machine keypair locally.
    let local_key_path = build_connect_key_path(target, host)?;
    local_step_generate_keypair(&local_key_path, &format!("ymux {}", local_hostname()))
        .await
        .map_err(|e| format!("generate keypair: {e}"))?;

    // 4. Install the public key into the target's authorized_keys. Resolve
    //    the real home dir + primary group on the remote so this works for
    //    ANY account (incl. root / non-UPG users). DATE comes from the
    //    remote `date` so we don't need a local date formatter. Every
    //    interpolated value is shell-escaped (Rule #3).
    let pub_path = format!("{local_key_path}.pub");
    let pub_text =
        std::fs::read_to_string(&pub_path).map_err(|e| format!("read {pub_path}: {e}"))?;
    let key_esc = shell_escape(pub_text.trim());
    let lhost_esc = shell_escape(&local_hostname());
    let u = shell_escape(target);
    let install = format!(
        "U={u}; \
         HOME_DIR=$(getent passwd \"$U\" | cut -d: -f6); \
         GRP=$(id -gn \"$U\"); \
         DATE=$(date +%F 2>/dev/null || echo unknown); \
         [ -n \"$HOME_DIR\" ] || {{ echo 'could not resolve home dir'; exit 1; }}; \
         {sp}install -d -m 700 -o \"$U\" -g \"$GRP\" \"$HOME_DIR/.ssh\" && \
         printf '# ymux: added by %s on %s\\n%s\\n' {lhost_esc} \"$DATE\" {key_esc} \
           | {sp}tee -a \"$HOME_DIR/.ssh/authorized_keys\" >/dev/null && \
         {sp}chown \"$U:$GRP\" \"$HOME_DIR/.ssh/authorized_keys\" && \
         {sp}chmod 600 \"$HOME_DIR/.ssh/authorized_keys\""
    );
    exec_capture(&mut handle, &install)
        .await
        .map_err(|e| format!("install key for '{target}': {e}"))?;
    drop(handle);

    // 5. Validate: reconnect KEY-ONLY (no password) as the target user.
    let mut h2 = open_ssh(host, port, target, &None, &Some(local_key_path.clone()), &None)
        .await
        .map_err(|e| {
            format!("the key was installed but key-only login as '{target}' failed: {e}")
        })?;
    let _ = exec_capture(&mut h2, "true").await;
    drop(h2);
    log_info("PROVISION", &format!(
        "connect_existing: key-only validation OK for '{target}'@{host}"
    ));

    // 6. Create / update the workspace via the shared finalizer.
    let prov_input = ProvisionInput {
        workspace_id: String::new(),
        host: host.clone(),
        port,
        initial_user: input.auth_user.clone(),
        initial_password: None,
        initial_key_path: None,
        initial_key_passphrase: None,
        new_user: target.to_string(),
        local_key_path: Some(local_key_path.clone()),
        profile_id: String::new(),
        workspace_name: input.workspace_name.clone(),
        existing_workspace_id: input.existing_workspace_id.clone(),
    };
    let (workspace_id, workspace_name) =
        finalize_workspace(state, app, &prov_input, &local_key_path)?;
    log_info("PROVISION", &format!(
        "connect_existing: workspace '{workspace_name}' ({workspace_id}) ready for '{target}'@{host}"
    ));
    Ok(ConnectExistingResult {
        workspace_id,
        workspace_name,
        key_path: local_key_path,
    })
}

/// Phase 65.B: run the connect-to-existing-server flow — optionally create
/// a key-only user, generate + install a per-machine SSH key, validate it,
/// and create/update the workspace. The password is zeroized after use and
/// never persisted (Rule #2).
#[tauri::command]
pub(crate) async fn connect_existing_execute(
    state: State<'_, AppState>,
    app: AppHandle,
    mut input: ConnectExistingInput,
) -> Result<ConnectExistingResult, String> {
    use zeroize::Zeroize;
    // Move the password out so `input` can be borrowed immutably by the
    // inner fn, and zeroize it regardless of outcome.
    let mut auth_pw = Some(std::mem::take(&mut input.password));
    let result = connect_existing_execute_inner(&state, &app, &input, &auth_pw).await;
    if let Some(s) = auth_pw.as_mut() {
        s.zeroize();
    }
    result
}

// ─── Tauri profile management ─────────────────────────────────────────────

#[tauri::command]
pub(crate) fn provisioning_profiles_list() -> Result<ProfilesFile, String> {
    load_profiles_from_disk()
}

#[tauri::command]
pub(crate) fn provisioning_profile_save(profile: ProvisioningProfile) -> Result<ProfilesFile, String> {
    let mut file = load_profiles_from_disk()?;
    if let Some(existing) = file.profiles.iter_mut().find(|p| p.id == profile.id) {
        *existing = profile;
    } else {
        file.profiles.push(profile);
    }
    save_profiles_to_disk(&file)?;
    Ok(file)
}

#[tauri::command]
pub(crate) fn provisioning_profile_delete(id: String) -> Result<ProfilesFile, String> {
    let mut file = load_profiles_from_disk()?;
    file.profiles.retain(|p| p.id != id);
    save_profiles_to_disk(&file)?;
    Ok(file)
}

#[tauri::command]
pub(crate) fn provisioning_step_catalog() -> Vec<(String, String)> {
    let all = [
        StepKind::UpdatePackages,
        StepKind::InstallBasics,
        StepKind::CreateUser,
        StepKind::GenerateKeypair,
        StepKind::DeployPubkey,
        StepKind::TestNewKey,
        StepKind::DisableRootSsh,
        StepKind::DisablePasswordSsh,
        StepKind::InstallNodejs,
        StepKind::InstallPython,
        StepKind::InstallDocker,
        StepKind::InstallClaudeCode,
        StepKind::InstallCodex,
        StepKind::InstallGemini,
        StepKind::AddYmuxToPath,
        StepKind::SetupYmuxHooks,
    ];
    all.into_iter()
        .map(|k| (format!("{k:?}"), k.label().to_string()))
        .collect()
}
