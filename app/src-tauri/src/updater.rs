//! Phase 9.B: lightweight update checker.
//!
//! No actual download / install — that requires signing keys we don't have
//! yet. We just fetch a remote `manifest.json`, compare the version, and
//! emit `update:available` so the frontend can toast a "new version
//! available — release notes" link.
//!
//! The manifest URL lives in `settings.updates.manifest_url` so it can be
//! switched without recompiling. Until the repo goes public the default
//! placeholder URL will return 404 / DNS-fail; the failure is silent and
//! never blocks startup.
//!
//! Fetch path: v0.2.3 switched to `ureq` + rustls (native HTTPS in
//! process). The previous version shelled out to PowerShell, which
//! broke on machines where powershell.exe is intercepted by AV/EDR or
//! locked down by Constrained Language Mode — the parser-error output
//! (script source echoed back) surfaced as the user-facing error.

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

/// A manifest URL pointing at an example/placeholder host can never resolve;
/// treat it as "no manifest configured" so the hooks-check / updater don't log
/// a DNS failure on every tick (v0.3.1: this spammed debug.log).
fn is_placeholder_host(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("example.com")
        || lower.contains("example.org")
        || lower.contains("example.net")
        || lower.contains("your-domain")
        || lower.contains("changeme")
}

use crate::{log_debug, log_info, log_warn, AppState};

#[cfg(target_os = "macos")]
mod macos;
use ymux_core::http::get_with_retry;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Size keys (`msi_size`, `nsis_size`) are deliberately absent: the sha256
/// check on install already rejects any altered download.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub version: String,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub notes_url: Option<String>,
    #[serde(default)]
    pub msi_url: Option<String>,
    #[serde(default)]
    pub msi_sha256: Option<String>,
    // Phase 27: NSIS installer URL + sha256, used by
    // `download_and_install_update` for one-click auto-install.
    // NSIS is preferred over MSI because it handles "app is running"
    // gracefully (wait / retry / replace) without requiring elevation
    // every time. Older manifests without these fields fall back to
    // the manual download path (Release notes link).
    #[serde(default)]
    pub nsis_url: Option<String>,
    #[serde(default)]
    pub nsis_sha256: Option<String>,
    #[serde(default)]
    pub min_supported_version: Option<String>,
    // macOS per-arch dmg (keys already published in manifest.json).
    #[serde(default)]
    pub dmg_x64_url: Option<String>,
    #[serde(default)]
    pub dmg_x64_sha256: Option<String>,
    #[serde(default)]
    pub dmg_aarch64_url: Option<String>,
    #[serde(default)]
    pub dmg_aarch64_sha256: Option<String>,
    /// Phase 18: per-agent hook spec versions. Map keyed by agent
    /// id (`"claude-code"`, `"codex"`, `"gemini"`). Pre-18 manifests
    /// without this field load fine — the desktop's hooks-outdated
    /// check just no-ops.
    #[serde(default)]
    pub hooks: std::collections::BTreeMap<String, ManifestHook>,
}

impl Manifest {
    /// (url, sha256) of the dmg for an arch tag from `mac_dmg_arch_tag`;
    /// unknown tag → (None, None).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // macOS install arm only
    pub(crate) fn dmg_for_arch(&self, tag: &str) -> (Option<String>, Option<String>) {
        match tag {
            "x64" => (self.dmg_x64_url.clone(), self.dmg_x64_sha256.clone()),
            "aarch64" => (self.dmg_aarch64_url.clone(), self.dmg_aarch64_sha256.clone()),
            _ => (None, None),
        }
    }
}

/// Rust `ARCH` const → dmg asset/manifest arch tag.
pub(crate) fn mac_dmg_arch_tag(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("x64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ManifestHook {
    pub version: String,
    #[serde(default)]
    pub url: Option<String>,
    /// Parsed for forward-compat, read nowhere: no code compares it to
    /// `APP_VERSION`. Enforcement would be a separate decision.
    #[serde(default)]
    pub min_ymux_version: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct UpdateInfo {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msi_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub last_check_iso: String,
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Compare two semver-ish version strings (`major.minor.patch[-suffix]`).
/// Returns Greater iff `a` is strictly newer than `b`. Suffixes are
/// compared lexically as a tiebreaker (`""` < `"alpha"` < `"beta"`...).
fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let split = |v: &str| -> (Vec<u64>, String) {
        let (head, tail) = match v.split_once('-') {
            Some((h, t)) => (h, t.to_string()),
            None => (v, String::new()),
        };
        let nums: Vec<u64> = head
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        (nums, tail)
    };
    let (an, asfx) = split(a);
    let (bn, bsfx) = split(b);
    for i in 0..an.len().max(bn.len()) {
        let x = *an.get(i).unwrap_or(&0);
        let y = *bn.get(i).unwrap_or(&0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    match (asfx.is_empty(), bsfx.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => asfx.cmp(&bsfx),
    }
}

async fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    // v0.2.3: native HTTPS client via `ureq` + rustls. Previously this
    // shelled out to `powershell.exe -Command 'Invoke-WebRequest ...'`,
    // which works on most Windows machines but breaks on installs where
    // PowerShell is locked down (Constrained Language Mode, AV/EDR that
    // intercept and mangle powershell.exe command lines, etc.). The
    // failure mode was opaque — PowerShell's parser-error output
    // (script source echoed back) became the user-visible error
    // message. Doing the HTTPS GET in-process eliminates that whole
    // class of breakage.
    let url = url.to_string();
    let body = tokio::task::spawn_blocking(move || -> Result<String, String> {
        // beta.3 (netfree): wrap in retry so a transient MITM-filter blip
        // or hotel-wifi glitch doesn't turn "Check for updates" red.
        let resp = get_with_retry(|| {
            ureq::get(&url)
                .set(
                    "User-Agent",
                    &format!("ymux/{}", env!("CARGO_PKG_VERSION")),
                )
                .timeout(std::time::Duration::from_secs(8))
        })
        .map_err(|e| format!("fetch manifest: {e}"))?;
        if resp.status() < 200 || resp.status() >= 300 {
            return Err(format!("manifest HTTP {}", resp.status()));
        }
        resp.into_string().map_err(|e| format!("read body: {e}"))
    })
    .await
    .map_err(|e| format!("join: {e}"))??;

    let m: Manifest = serde_json::from_str(body.trim_start_matches('\u{FEFF}').trim())
        .map_err(|e| format!("parse manifest: {e}"))?;
    Ok(m)
}

/// Check the manifest URL from settings, emit `update:available` if a
/// newer version is found, persist last_check_iso + last_seen_version.
/// Used both by the startup task and the manual `check_for_updates_now`
/// Tauri command.
pub(crate) async fn check(state: &AppState, app: &AppHandle) -> UpdateInfo {
    let (url, last_seen, skipped, remind_after) = {
        let s = state.settings.lock().unwrap();
        (
            s.updates.manifest_url.clone(),
            s.updates.last_seen_version.clone(),
            s.updates.skipped_versions.clone(),
            s.updates.remind_after_iso.clone(),
        )
    };
    let now = iso_now();

    let url = match url {
        Some(u) if !u.is_empty() => u,
        _ => {
            return UpdateInfo {
                current_version: APP_VERSION.into(),
                latest_version: None,
                available: false,
                notes_url: None,
                msi_url: None,
                released_at: None,
                manifest_url: None,
                error: Some("manifest_url not configured".into()),
                last_check_iso: now,
            };
        }
    };

    let info = match fetch_manifest(&url).await {
        Ok(m) => {
            let available = matches!(
                cmp_versions(&m.version, APP_VERSION),
                std::cmp::Ordering::Greater
            );
            UpdateInfo {
                current_version: APP_VERSION.into(),
                latest_version: Some(m.version.clone()),
                available,
                notes_url: m.notes_url.clone(),
                msi_url: m.msi_url.clone(),
                released_at: m.released_at.clone(),
                manifest_url: Some(url.clone()),
                error: None,
                last_check_iso: now.clone(),
            }
        }
        Err(e) => {
            log_warn("UPDATER", &format!("updater: fetch {url} failed: {e}"));
            UpdateInfo {
                current_version: APP_VERSION.into(),
                latest_version: None,
                available: false,
                notes_url: None,
                msi_url: None,
                released_at: None,
                manifest_url: Some(url.clone()),
                error: Some(e),
                last_check_iso: now.clone(),
            }
        }
    };

    {
        let mut s = state.settings.lock().unwrap();
        s.updates.last_check_iso = Some(now.clone());
        if let Some(v) = info.latest_version.clone() {
            s.updates.last_seen_version = Some(v);
        }
        let _ = crate::settings::save_to_disk_pub(&s);
    }

    // Phase 65 (U): respect the user's "skip this version" + "remind me
    // later" choices for the auto-banner. (The manual Settings check
    // still returns `info` directly, so an explicit check always shows
    // the result regardless of snooze/skip.)
    let snoozed = remind_after
        .as_deref()
        .and_then(|iso| chrono::DateTime::parse_from_rfc3339(iso).ok())
        .map(|t| chrono::Utc::now() < t.with_timezone(&chrono::Utc))
        .unwrap_or(false);
    let skipped_this = info
        .latest_version
        .as_ref()
        .map(|v| skipped.iter().any(|s| s == v))
        .unwrap_or(false);
    if info.available && info.latest_version != last_seen && !snoozed && !skipped_this {
        log_info("UPDATER", &format!(
            "updater: new version {} available (current {})",
            info.latest_version.clone().unwrap_or_default(),
            APP_VERSION
        ));
        let _ = app.emit("update:available", &info);
    } else if info.available && (snoozed || skipped_this) {
        log_debug("UPDATER", &format!(
            "updater: {} available but suppressed (snoozed={snoozed} skipped={skipped_this})",
            info.latest_version.clone().unwrap_or_default()
        ));
    }
    info
}

/// Phase 65 (U): mark a version as skipped — the auto-banner won't show
/// for it again until a newer version is published. Idempotent.
#[tauri::command]
pub(crate) async fn updater_skip_version(
    state: State<'_, AppState>,
    version: String,
) -> Result<(), String> {
    let mut s = state.settings.lock().map_err(|e| e.to_string())?;
    if !s.updates.skipped_versions.contains(&version) {
        s.updates.skipped_versions.push(version);
    }
    crate::settings::save_to_disk_pub(&s).map_err(|e| e.to_string())?;
    Ok(())
}

/// Phase 65 (U): snooze the update banner for `hours` hours.
#[tauri::command]
pub(crate) async fn updater_remind_later(
    state: State<'_, AppState>,
    hours: u32,
) -> Result<(), String> {
    let until = chrono::Utc::now() + chrono::Duration::hours(hours.max(1) as i64);
    let iso = until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut s = state.settings.lock().map_err(|e| e.to_string())?;
    s.updates.remind_after_iso = Some(iso);
    crate::settings::save_to_disk_pub(&s).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn check_for_updates_now(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<UpdateInfo, String> {
    Ok(check(&state, &app).await)
}

// ─── Phase 18: hooks-outdated probe ────────────────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct HooksOutdatedInfo {
    pub workspace_id: String,
    pub pane_id: String,
    pub agent: String,
    pub current: Option<String>,
    pub latest: String,
}

/// Read the cached manifest (fetching if absent), then over the
/// established SSH handle ask the remote what version it has
/// installed. Emit `hooks:outdated` when the remote's version is
/// older AND the user hasn't dismissed it.
pub(crate) async fn check_remote_hooks(
    state: &AppState,
    app: &AppHandle,
    handle: &std::sync::Arc<russh::client::Handle<crate::SshClient>>,
    workspace_id: &str,
    pane_id: &str,
) {
    // 1. Fetch the latest manifest (cached in last_check…). Cheap if
    //    cached, otherwise a single curl call.
    let manifest_url = match state
        .settings
        .lock()
        .ok()
        .and_then(|s| s.updates.manifest_url.clone())
    {
        Some(u) if !u.is_empty() => u,
        _ => return,
    };
    // v0.3.1: a placeholder / example manifest host (e.g. ymux.example.com)
    // can never resolve — skip silently instead of logging a DNS failure on
    // every hooks-check tick (it spammed debug.log ~50×/session).
    if is_placeholder_host(&manifest_url) {
        return;
    }
    let manifest = match fetch_manifest(&manifest_url).await {
        Ok(m) => m,
        Err(e) => {
            log_warn("UPDATER", &format!("hooks-check: fetch manifest failed: {e}"));
            return;
        }
    };
    let claude_latest = match manifest.hooks.get("claude-code") {
        Some(h) => h.version.clone(),
        None => return,
    };

    // 2. Ask the remote what `ymux_meta.hooks_version` is in
    //    ~/.claude/settings.json. `jq` does the lookup cleanly; we
    //    fall back to grep for hosts without jq.
    let cmd = "if [ -f \"$HOME/.claude/settings.json\" ]; then \
               if command -v jq >/dev/null; then \
                 jq -r '.ymux_meta.hooks_version // \"none\"' \"$HOME/.claude/settings.json\" 2>/dev/null; \
               else \
                 grep -oE '\"hooks_version\"\\s*:\\s*\"[^\"]+\"' \"$HOME/.claude/settings.json\" 2>/dev/null \
                   | head -1 | sed -E 's/.*\"([^\"]+)\"$/\\1/'; \
               fi; \
             else echo MISSING; fi";
    let current = match ssh_exec_simple(handle, cmd).await {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            log_warn("UPDATER", &format!("hooks-check: remote read failed: {e}"));
            return;
        }
    };
    let current_opt: Option<String> = match current.as_str() {
        "" | "MISSING" | "none" | "null" => None,
        other => Some(other.to_string()),
    };

    // 3. Compare. None → hooks never installed (caller may want
    //    "install now" prompt later; for outdated banner we skip).
    //    Older → emit. Same/newer → no-op.
    let need_banner = match &current_opt {
        Some(cur) => matches!(cmp_versions(&claude_latest, cur), std::cmp::Ordering::Greater),
        None => false,
    };
    if !need_banner {
        log_debug("UPDATER", &format!(
            "hooks-check: workspace={workspace_id} agent=claude-code current={current_opt:?} latest={claude_latest} → up-to-date or missing"
        ));
        return;
    }

    // 4. Honor the user's dismissed list.
    let dismissed = state
        .settings
        .lock()
        .ok()
        .and_then(|s| {
            s.hooks_updates
                .dismissed
                .get("claude-code")
                .cloned()
        })
        .unwrap_or_default();
    if dismissed.contains(&claude_latest) {
        log_debug("UPDATER", &format!(
            "hooks-check: workspace={workspace_id} claude-code v{claude_latest} silently dismissed"
        ));
        return;
    }
    let show_banners = state
        .settings
        .lock()
        .ok()
        .map(|s| s.hooks_updates.show_banners)
        .unwrap_or(true);
    if !show_banners {
        return;
    }

    let info = HooksOutdatedInfo {
        workspace_id: workspace_id.to_string(),
        pane_id: pane_id.to_string(),
        agent: "claude-code".to_string(),
        current: current_opt,
        latest: claude_latest,
    };
    let _ = app.emit("hooks:outdated", &info);
}

/// Phase 18: run an arbitrary shell command in a workspace's active
/// SSH session. Used by the hooks-banner "Update now" button to fire
/// `ymux setup-hooks --agent claude --force --source github` over
/// the tunnel without making the user open a pane and type it.
/// Errors when no SSH session is alive for the workspace.
#[tauri::command]
pub(crate) async fn ssh_exec_in_workspace(
    state: tauri::State<'_, AppState>,
    workspace_id: String,
    cmd: String,
) -> Result<String, String> {
    let handle = {
        let sessions = state.core.sessions.lock().map_err(|e| e.to_string())?;
        sessions.values().find_map(|s| match s {
            crate::Session::Ssh(ssh) if ssh.workspace_id == workspace_id => {
                Some(ssh.handle.clone())
            }
            _ => None,
        })
    }
    .ok_or_else(|| "no active SSH session for this workspace".to_string())?;
    ssh_exec_simple(&handle, &cmd).await
}

pub(crate) async fn ssh_exec_simple(
    handle: &std::sync::Arc<russh::client::Handle<crate::SshClient>>,
    cmd: &str,
) -> Result<String, String> {
    use russh::ChannelMsg;
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(6), async {
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

/// Helper for CLI / RPC dispatch — not a Tauri command.
pub(crate) async fn rpc_check_now(state: &AppState, app: &AppHandle) -> serde_json::Value {
    let info = check(state, app).await;
    json!({
        "current_version": info.current_version,
        "latest_version": info.latest_version,
        "available": info.available,
        "notes_url": info.notes_url,
        "msi_url": info.msi_url,
        "released_at": info.released_at,
        "last_check_iso": info.last_check_iso,
        "error": info.error,
    })
}

// ─── Phase 27: one-click auto-install ──────────────────────────────────────

/// v0.2.3: native HTTPS download via `ureq` (rustls). Replaces the
/// previous `powershell.exe -Command 'Invoke-WebRequest -OutFile ...'`
/// shell-out for the same reason as `fetch_manifest`: opaque failures
/// on machines where PowerShell is locked down. Streams the response
/// body to disk so multi-MB installer downloads don't sit in memory.
async fn http_download_to_file(
    url: &str,
    dest: &std::path::Path,
) -> Result<(), String> {
    let url = url.to_string();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        // beta.3 (netfree): retry on transport errors — worst case is 3×
        // the ~5MB NSIS download (15MB) which is still cheap. HTTP-status
        // failures fall through immediately without retry.
        let resp = get_with_retry(|| {
            ureq::get(&url)
                .set(
                    "User-Agent",
                    &format!("ymux/{}", env!("CARGO_PKG_VERSION")),
                )
                // Installer is ~5 MB; 180s tolerates slow networks /
                // corporate proxies without surfacing a confusing timeout.
                .timeout(std::time::Duration::from_secs(180))
        })
        .map_err(|e| format!("download {url}: {e}"))?;
        if resp.status() < 200 || resp.status() >= 300 {
            return Err(format!("download HTTP {}", resp.status()));
        }
        let mut reader = resp.into_reader();
        let mut file = std::fs::File::create(&dest)
            .map_err(|e| format!("create {}: {e}", dest.display()))?;
        std::io::copy(&mut reader, &mut file)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("join: {e}"))??;
    Ok(())
}

/// sha256 of a file, lowercase hex. Reads the whole file into memory
/// — fine for the ~5-7 MB NSIS installer; we'd stream for anything
/// bigger.
fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

/// Phase 27: re-fetch the manifest, download the NSIS installer
/// listed in `nsis_url`, verify its sha256 against
/// `manifest.nsis_sha256`, spawn the installer detached, then exit
/// the app ~800ms later so the spawned process settles before the
/// parent goes away.
///
/// Integrity: a wrong sha256 deletes the temp file and aborts. This
/// gives us download integrity but NOT publisher authenticity — the
/// installer is unsigned (SmartScreen will warn on first launch).
/// Code-signing is a separate future task; see RELEASING.md
/// "Caveats" section.
#[tauri::command]
pub(crate) async fn download_and_install_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Windows runs the NSIS installer; macOS mounts the per-arch .dmg and
    // swaps the running .app (`macos.rs`); any other OS refuses before
    // touching the network.
    #[cfg(target_os = "windows")]
    return install_nsis_update(app, state).await;
    #[cfg(target_os = "macos")]
    return install_macos_update(app, state).await;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (&app, &state);
        return Err("in-app update is Windows-only for now — download the latest .dmg from the releases page".into());
    }
}

/// macOS arm of `download_and_install_update`: manifest → dmg for the running
/// arch → mount, swap, relaunch. Every failure is an `Err` before the exit is
/// scheduled inside `macos::install_dmg_and_relaunch`.
#[cfg(target_os = "macos")]
async fn install_macos_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let url = {
        let s = state.settings.lock().map_err(|e| format!("settings lock: {e}"))?;
        s.updates
            .manifest_url
            .clone()
            .ok_or_else(|| "no manifest_url configured".to_string())?
    };
    let manifest = fetch_manifest(&url).await?;
    if cmp_versions(&manifest.version, APP_VERSION) != std::cmp::Ordering::Greater {
        return Err(format!(
            "manifest version {} is not newer than current {APP_VERSION} — nothing to install",
            manifest.version
        ));
    }
    let arch = std::env::consts::ARCH;
    let tag = mac_dmg_arch_tag(arch)
        .ok_or_else(|| format!("unsupported macOS architecture {arch}"))?;
    let (dmg_url, dmg_sha) = manifest.dmg_for_arch(tag);
    let dmg_url = dmg_url.ok_or_else(|| {
        format!("manifest has no dmg for {tag} — falling back to manual download")
    })?;
    let dmg_sha = dmg_sha.ok_or_else(|| {
        format!("manifest has no dmg sha256 for {tag} — refusing to install unverified")
    })?;
    macos::install_dmg_and_relaunch(app, dmg_url, Some(dmg_sha), manifest.version).await
}

#[cfg(target_os = "windows")]
async fn install_nsis_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Step 1: re-fetch the manifest (we don't trust the cached one
    // from the last check — the version might have moved on, and
    // we'd rather error than install something stale).
    let url = {
        let s = state.settings.lock().unwrap();
        s.updates
            .manifest_url
            .clone()
            .ok_or_else(|| "no manifest_url configured".to_string())?
    };
    let manifest = fetch_manifest(&url).await?;

    // Step 2: guards.
    let nsis_url = manifest
        .nsis_url
        .clone()
        .ok_or_else(|| "manifest has no nsis_url — falling back to manual download".to_string())?;
    let expected_sha = manifest
        .nsis_sha256
        .clone()
        .ok_or_else(|| "manifest has no nsis_sha256 — refusing to install unverified".to_string())?;
    if cmp_versions(&manifest.version, APP_VERSION) != std::cmp::Ordering::Greater {
        return Err(format!(
            "manifest version {} is not newer than current {APP_VERSION} — nothing to install",
            manifest.version
        ));
    }

    // Step 3: download to %TEMP%.
    let temp_dir = std::env::temp_dir();
    let dest = temp_dir.join(format!("ymux-update-{}.exe", manifest.version));
    log_info("UPDATER", &format!(
        "updater: downloading {} -> {:?} (expected sha256 {expected_sha})",
        nsis_url, dest
    ));
    http_download_to_file(&nsis_url, &dest)
        .await
        .map_err(|e| format!("download failed: {e}"))?;

    // Step 4: integrity check.
    let actual_sha = sha256_file(&dest)
        .map_err(|e| format!("hash failed: {e}"))?;
    if !actual_sha.eq_ignore_ascii_case(&expected_sha) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "downloaded installer failed integrity check — expected {expected_sha}, got {actual_sha} — aborting"
        ));
    }
    log_info("UPDATER", &format!("updater: sha256 verified ({actual_sha})"));

    // Step 5: spawn the installer detached and schedule app exit.
    // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS so the installer
    // survives our exit and isn't tied to our console.
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        std::process::Command::new(&dest)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("spawn installer: {e}"))?;
    }
    log_info("UPDATER", "updater: installer spawned — scheduling app exit in 800ms");

    // Step 6: schedule exit. We can't exit synchronously here because
    // the FE needs the Ok(()) return to fire before we go away
    // (otherwise the invoke promise rejects with a window-destroyed
    // error and the user sees a misleading toast).
    let app_clone = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        // Phase 80: the installer path is where losing the log tail costs
        // most — this is the last thing the old binary ever writes.
        ymux_core::flush_log();
        app_clone.exit(0);
    });
    Ok(())
}

// ─── Phase 71: version manager (list + install-specific-version) ──────────

/// One published release, as the Settings → Updates "version history" list
/// needs it. Built from the GitHub releases API. Plain `Serialize` (the TS
/// side defines the matching interface), consistent with `UpdateInfo`.
#[derive(Clone, Serialize)]
pub(crate) struct ReleaseInfo {
    pub version: String, // tag without a leading 'v'
    pub tag: String,
    pub published_at: Option<String>,
    pub notes_url: String,
    pub body_md: String,
    pub prerelease: bool,
    pub nsis_url: Option<String>,
    pub nsis_sha256: Option<String>,
    pub msi_url: Option<String>,
    pub msi_sha256: Option<String>,
    /// dmg for the RUNNING arch (`*_x64.dmg` / `*_aarch64.dmg`).
    pub dmg_url: Option<String>,
    pub dmg_sha256: Option<String>,
}

// GitHub releases API — only the fields we use.
#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>, // "sha256:<hex>" on newer GitHub
}

const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/yyhezkel/ymux/releases?per_page=50";

/// 5-minute cache so flicking between channels / re-opening Settings doesn't
/// hammer the unauthenticated GitHub API (60 req/hr).
static VERSIONS_CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<ReleaseInfo>)>> =
    std::sync::Mutex::new(None);
const VERSIONS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn cached_versions() -> Option<Vec<ReleaseInfo>> {
    let guard = VERSIONS_CACHE.lock().ok()?;
    let (at, list) = guard.as_ref()?;
    if at.elapsed() < VERSIONS_CACHE_TTL {
        Some(list.clone())
    } else {
        None
    }
}

fn cached_versions_stale() -> Option<Vec<ReleaseInfo>> {
    let guard = VERSIONS_CACHE.lock().ok()?;
    guard.as_ref().map(|(_, list)| list.clone())
}

fn store_versions(list: &[ReleaseInfo]) {
    if let Ok(mut g) = VERSIONS_CACHE.lock() {
        *g = Some((std::time::Instant::now(), list.to_vec()));
    }
}

fn strip_sha256(digest: &Option<String>) -> Option<String> {
    digest
        .as_deref()
        .map(|d| d.strip_prefix("sha256:").unwrap_or(d).to_string())
}

/// Fetch + parse the releases list from GitHub. drafts are skipped; the API
/// already returns newest-first.
async fn fetch_releases() -> Result<Vec<ReleaseInfo>, String> {
    let body = tokio::task::spawn_blocking(|| -> Result<String, String> {
        // beta.3 (netfree): retry on transport errors. GitHub returns 4xx
        // for rate-limit (not a transport error), which is deliberately
        // surfaced immediately — retrying wouldn't help against a rate cap.
        let resp = get_with_retry(|| {
            ureq::get(GITHUB_RELEASES_API)
                .set("User-Agent", &format!("ymux/{}", env!("CARGO_PKG_VERSION")))
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .timeout(std::time::Duration::from_secs(10))
        })
        .map_err(|e| format!("github releases: {e}"))?;
        if resp.status() < 200 || resp.status() >= 300 {
            return Err(format!("github releases HTTP {}", resp.status()));
        }
        resp.into_string().map_err(|e| format!("read body: {e}"))
    })
    .await
    .map_err(|e| format!("join: {e}"))??;

    parse_releases(&body)
}

/// Pure parse of a GitHub releases JSON body → ReleaseInfo list (drafts
/// dropped). Split out so it's unit-testable without a network call.
fn parse_releases(body: &str) -> Result<Vec<ReleaseInfo>, String> {
    parse_releases_for_arch(body, std::env::consts::ARCH)
}

/// `parse_releases` with the arch injected (platform-neutral tests).
fn parse_releases_for_arch(body: &str, arch: &str) -> Result<Vec<ReleaseInfo>, String> {
    let raw: Vec<GhRelease> =
        serde_json::from_str(body).map_err(|e| format!("parse releases: {e}"))?;
    let dmg_suffix = mac_dmg_arch_tag(arch).map(|t| format!("_{t}.dmg"));
    Ok(raw
        .into_iter()
        .filter(|r| !r.draft)
        .map(|r| {
            let nsis = r.assets.iter().find(|a| a.name.ends_with("-setup.exe"));
            let msi = r.assets.iter().find(|a| a.name.ends_with(".msi"));
            let dmg = dmg_suffix
                .as_deref()
                .and_then(|s| r.assets.iter().find(|a| a.name.ends_with(s)));
            ReleaseInfo {
                version: r.tag_name.trim_start_matches('v').to_string(),
                tag: r.tag_name.clone(),
                published_at: r.published_at,
                notes_url: r.html_url,
                body_md: r.body.unwrap_or_default(),
                prerelease: r.prerelease,
                nsis_url: nsis.map(|a| a.browser_download_url.clone()),
                nsis_sha256: nsis.and_then(|a| strip_sha256(&a.digest)),
                msi_url: msi.map(|a| a.browser_download_url.clone()),
                msi_sha256: msi.and_then(|a| strip_sha256(&a.digest)),
                dmg_url: dmg.map(|a| a.browser_download_url.clone()),
                dmg_sha256: dmg.and_then(|a| strip_sha256(&a.digest)),
            }
        })
        .collect())
}

#[cfg(test)]
mod version_manager_tests {
    use super::{mac_dmg_arch_tag, parse_releases, parse_releases_for_arch, strip_sha256, Manifest};

    const DMG_RELEASES: &str = r#"[{
      "tag_name": "v0.5.0", "html_url": "https://x", "assets": [
        {"name": "ymux_0.5.0_x64.dmg", "browser_download_url": "https://x/x64.dmg", "digest": "sha256:aa11"},
        {"name": "ymux_0.5.0_aarch64.dmg", "browser_download_url": "https://x/arm.dmg", "digest": "sha256:bb22"}
      ]}]"#;

    // Pins arch→asset selection: x64 host must never get the arm dmg.
    #[test]
    fn picks_x64_dmg() {
        let r = &parse_releases_for_arch(DMG_RELEASES, "x86_64").unwrap()[0];
        assert_eq!(r.dmg_url.as_deref(), Some("https://x/x64.dmg"));
        assert_eq!(r.dmg_sha256.as_deref(), Some("aa11"));
    }

    // Pins the aarch64 arm of the selection.
    #[test]
    fn picks_aarch64_dmg() {
        let r = &parse_releases_for_arch(DMG_RELEASES, "aarch64").unwrap()[0];
        assert_eq!(r.dmg_url.as_deref(), Some("https://x/arm.dmg"));
        assert_eq!(r.dmg_sha256.as_deref(), Some("bb22"));
    }

    // Pins: unknown arch or release without a dmg → None, never a wrong asset.
    #[test]
    fn missing_or_unknown_arch_dmg_is_none() {
        let r = &parse_releases_for_arch(DMG_RELEASES, "riscv64").unwrap()[0];
        assert!(r.dmg_url.is_none() && r.dmg_sha256.is_none());
        let none = r#"[{"tag_name": "v1.0.0", "html_url": "https://x", "assets": []}]"#;
        let r = &parse_releases_for_arch(none, "x86_64").unwrap()[0];
        assert!(r.dmg_url.is_none() && r.dmg_sha256.is_none());
    }

    // Pins the ARCH→tag map the installer depends on.
    #[test]
    fn arch_tag_map() {
        assert_eq!(mac_dmg_arch_tag("x86_64"), Some("x64"));
        assert_eq!(mac_dmg_arch_tag("aarch64"), Some("aarch64"));
        assert_eq!(mac_dmg_arch_tag("arm"), None);
    }

    // Pins backward compat: pre-dmg manifests must still deserialize.
    #[test]
    fn old_manifest_without_dmg_keys_loads() {
        let m: Manifest = serde_json::from_str(r#"{"version": "0.4.0"}"#).unwrap();
        assert_eq!(m.dmg_for_arch("x64"), (None, None));
        assert_eq!(m.dmg_for_arch("aarch64"), (None, None));
    }

    // Pins Manifest::dmg_for_arch picks the matching pair only.
    #[test]
    fn manifest_dmg_for_arch() {
        let m: Manifest = serde_json::from_str(
            r#"{"version": "0.5.0", "dmg_x64_url": "u1", "dmg_x64_sha256": "s1",
                "dmg_aarch64_url": "u2", "dmg_aarch64_sha256": "s2"}"#,
        )
        .unwrap();
        assert_eq!(m.dmg_for_arch("x64"), (Some("u1".into()), Some("s1".into())));
        assert_eq!(m.dmg_for_arch("aarch64"), (Some("u2".into()), Some("s2".into())));
        assert_eq!(m.dmg_for_arch("x86"), (None, None));
    }

    #[test]
    fn strips_sha256_prefix() {
        assert_eq!(strip_sha256(&Some("sha256:abc".into())), Some("abc".into()));
        assert_eq!(strip_sha256(&Some("abc".into())), Some("abc".into()));
        assert_eq!(strip_sha256(&None), None);
    }

    #[test]
    fn parses_github_releases() {
        let json = r#"[
          {
            "tag_name": "v0.3.1", "published_at": "2026-06-30T17:32:00Z",
            "html_url": "https://github.com/yyhezkel/ymux/releases/tag/v0.3.1",
            "body": "notes here", "prerelease": false, "draft": false,
            "assets": [
              {"name": "ymux_0.3.1_x64-setup.exe", "browser_download_url": "https://x/setup.exe", "digest": "sha256:deadbeef"},
              {"name": "ymux_0.3.1_x64_en-US.msi", "browser_download_url": "https://x/app.msi", "digest": "sha256:cafef00d"}
            ]
          },
          {
            "tag_name": "v0.4.0-beta1", "published_at": null, "html_url": "https://x",
            "body": null, "prerelease": true, "draft": false, "assets": []
          },
          {
            "tag_name": "v9.9.9-draft", "html_url": "https://x",
            "prerelease": false, "draft": true, "assets": []
          }
        ]"#;
        let list = parse_releases(json).expect("should parse");
        assert_eq!(list.len(), 2, "draft must be dropped");

        let r0 = &list[0];
        assert_eq!(r0.version, "0.3.1");
        assert_eq!(r0.tag, "v0.3.1");
        assert!(!r0.prerelease);
        assert_eq!(r0.nsis_url.as_deref(), Some("https://x/setup.exe"));
        assert_eq!(r0.nsis_sha256.as_deref(), Some("deadbeef"));
        assert_eq!(r0.msi_sha256.as_deref(), Some("cafef00d"));

        let r1 = &list[1];
        assert_eq!(r1.version, "0.4.0-beta1");
        assert!(r1.prerelease, "beta must be flagged for channel filtering");
        assert!(r1.nsis_url.is_none());
        assert_eq!(r1.body_md, "");
    }
}

/// List published versions (newest-first). `force` bypasses the 5-min cache.
/// On a fetch failure (rate limit / offline) we fall back to the stale cache
/// if present, so the list still renders (71.E).
#[tauri::command]
pub(crate) async fn updater_list_versions(force: bool) -> Result<Vec<ReleaseInfo>, String> {
    if !force {
        if let Some(c) = cached_versions() {
            return Ok(c);
        }
    }
    match fetch_releases().await {
        Ok(list) => {
            store_versions(&list);
            Ok(list)
        }
        Err(e) => {
            if let Some(stale) = cached_versions_stale() {
                log_warn("UPDATER", &format!(
                    "updater: list_versions fetch failed ({e}) — serving stale cache"
                ));
                Ok(stale)
            } else {
                Err(e)
            }
        }
    }
}

/// Back up settings.json before a downgrade (71.C). Best-effort copy to a
/// sibling so a newer-version-written settings.json isn't lost if an older
/// ymux rewrites it.
fn backup_settings_file(tag: &str) -> Result<String, String> {
    let dir = crate::config_dir_pub()?;
    let src = dir.join("settings.json");
    if !src.exists() {
        return Ok(String::new());
    }
    let safe_tag: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let dest = dir.join(format!("settings.backup-before-{safe_tag}.json"));
    std::fs::copy(&src, &dest).map_err(|e| format!("backup settings: {e}"))?;
    log_info("UPDATER", &format!("updater: backed up settings.json -> {}", dest.display()));
    Ok(dest.to_string_lossy().to_string())
}

/// Download + verify + install a SPECIFIC published version (71.B/C). Unlike
/// `download_and_install_update`, this has NO newer-than guard, so it powers
/// downgrades too. `backup_settings` copies settings.json first.
#[tauri::command]
pub(crate) async fn updater_install_version(
    app: AppHandle,
    version: String,
    backup_settings: bool,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    return install_nsis_version(app, version, backup_settings).await;
    #[cfg(target_os = "macos")]
    return install_macos_version(app, version, backup_settings).await;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (&app, &version, backup_settings);
        return Err("installing a specific version is Windows-only for now — download that release's .dmg from the releases page".into());
    }
}

/// macOS arm of `updater_install_version`: the release's dmg for the running
/// arch; the checksum is optional (GitHub digest), as on the NSIS path.
#[cfg(target_os = "macos")]
async fn install_macos_version(
    app: AppHandle,
    version: String,
    backup_settings: bool,
) -> Result<(), String> {
    let list = match cached_versions() {
        Some(c) => c,
        None => fetch_releases().await?,
    };
    let rel = list
        .iter()
        .find(|r| r.version == version || r.tag == version)
        .ok_or_else(|| format!("version {version} not found in releases"))?
        .clone();
    let dmg_url = rel.dmg_url.clone().ok_or_else(|| {
        format!(
            "release {} has no macOS dmg for {}",
            rel.tag,
            std::env::consts::ARCH
        )
    })?;
    if backup_settings {
        let _ = backup_settings_file(&rel.tag); // best-effort, never blocks install
    }
    macos::install_dmg_and_relaunch(app, dmg_url, rel.dmg_sha256.clone(), rel.version).await
}

#[cfg(target_os = "windows")]
async fn install_nsis_version(
    app: AppHandle,
    version: String,
    backup_settings: bool,
) -> Result<(), String> {
    // Resolve the release (prefer cache; fall back to a fresh fetch).
    let list = match cached_versions() {
        Some(c) => c,
        None => fetch_releases().await?,
    };
    let rel = list
        .iter()
        .find(|r| r.version == version || r.tag == version)
        .ok_or_else(|| format!("version {version} not found in releases"))?
        .clone();
    let nsis_url = rel
        .nsis_url
        .clone()
        .ok_or_else(|| format!("release {} has no NSIS installer asset", rel.tag))?;

    if backup_settings {
        let _ = backup_settings_file(&rel.tag); // best-effort, never blocks install
    }

    let dest = std::env::temp_dir().join(format!("ymux-install-{}.exe", rel.version));
    log_info("UPDATER", &format!(
        "updater: installing {} from {} -> {:?}",
        rel.tag, nsis_url, dest
    ));
    http_download_to_file(&nsis_url, &dest)
        .await
        .map_err(|e| format!("download failed: {e}"))?;

    // Verify the published checksum when present; GitHub HTTPS is the floor.
    match &rel.nsis_sha256 {
        Some(expected) => {
            let actual = sha256_file(&dest).map_err(|e| format!("hash failed: {e}"))?;
            if !actual.eq_ignore_ascii_case(expected) {
                let _ = std::fs::remove_file(&dest);
                return Err(format!(
                    "integrity check failed — expected {expected}, got {actual}"
                ));
            }
            log_info("UPDATER", &format!("updater: sha256 verified for {} ({actual})", rel.tag));
        }
        None => log_warn("UPDATER", &format!(
            "updater: {} has no published checksum — relying on GitHub TLS",
            rel.tag
        )),
    }

    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        std::process::Command::new(&dest)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("spawn installer: {e}"))?;
    }
    log_info("UPDATER", &format!(
        "updater: installer for {} spawned — exiting in 800ms",
        rel.tag
    ));
    let app_clone = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        // Phase 80: the installer path is where losing the log tail costs
        // most — this is the last thing the old binary ever writes.
        ymux_core::flush_log();
        app_clone.exit(0);
    });
    Ok(())
}
