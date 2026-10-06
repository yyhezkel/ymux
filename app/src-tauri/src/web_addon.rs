//! Phase 112 — the `ymux-web` add-on (WEB-DESIGN §7.1 option (b), Phase D).
//!
//! The browser frontend is the desktop's own vite build, and that build is
//! already inside this binary: Tauri embeds `frontendDist`. So the add-on
//! ships exactly the frontend the desktop is running — no second build, no
//! tarball in `resources/` (which `cargo test` would need staged), and
//! version-aligned by construction.
//!
//! - `init` (app setup) reads the embedded assets once: the file list from
//!   `AssetResolver::iter` (whose bytes are brotli-compressed), the contents
//!   from `AssetResolver::get` (decompressed; no CSP is configured, so
//!   index.html comes back untouched). Only what the daemon serves is kept:
//!   `index.html`, `assets/`, `fonts/`. The label is
//!   `<app version>-<sha256 of the set, 8 hex>`.
//! - install / update upload the set over one SFTP session into
//!   `~/.ymux/server/www/<label>.tmp-<pid>/`, move it to `<label>/`, swap the
//!   `current` symlink atomically, and keep one previous version.
//! - detect is `readlink current` → the installed label.
//! - Yossi, 2026-10-06: automatic on connect — when the add-on is installed
//!   on a host and its label differs from this desktop's, `spawn_auto_update`
//!   re-installs in the background, once per host and label per run.
//!
//! The daemon (2.9.0+) serves `www/current` at `/` (vault server-go §
//! webapp.go); without it `/` stays the diagnostic page.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use russh::client::Handle as SshHandle;
use russh_sftp::client::SftpSession;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::addons::{exec, remote_home};
use crate::SshClient;

/// The embedded frontend, as uploaded.
pub(crate) struct WebBundle {
    pub label: String,
    pub files: Vec<(String, Vec<u8>)>,
}

static BUNDLE: OnceLock<WebBundle> = OnceLock::new();

/// Hosts already brought to this desktop's label during this run.
static SYNCED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub(crate) fn bundle() -> Option<&'static WebBundle> {
    BUNDLE.get()
}

/// The label the add-on would install, if the bundle could be read.
pub(crate) fn label() -> Option<&'static str> {
    BUNDLE.get().map(|b| b.label.as_str())
}

/// Only what the daemon serves (webapp.go routes: `/`, `/assets/…`, `/fonts/…`).
fn shipped(rel: &str) -> bool {
    rel == "index.html" || rel.starts_with("assets/") || rel.starts_with("fonts/")
}

/// A relative path safe to put in a remote shell string and an SFTP path:
/// no parent segments, no leading slash, a plain character set.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.split('/').any(|s| s.is_empty() || s == "." || s == "..")
        && rel
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
}

/// `<version>-<8 hex>` over the sorted (path, bytes) set.
fn label_for(version: &str, files: &[(String, Vec<u8>)]) -> String {
    let mut h = Sha256::new();
    for (p, b) in files {
        h.update(p.as_bytes());
        h.update([0u8]);
        h.update((b.len() as u64).to_le_bytes());
        h.update(b);
    }
    let hex = format!("{:x}", h.finalize());
    format!("{version}-{}", &hex[..8])
}

/// Read the embedded frontend once, at app setup.
pub(crate) fn init<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let resolver = app.asset_resolver();
    let mut keys: Vec<String> = resolver
        .iter()
        .map(|(k, _)| k.trim_start_matches('/').to_string())
        .filter(|k| shipped(k) && safe_rel(k))
        .collect();
    keys.sort();
    keys.dedup();
    let mut files = Vec::with_capacity(keys.len());
    for k in keys {
        match resolver.get(k.clone()) {
            Some(a) => files.push((k, a.bytes)),
            None => crate::log_warn("WEBADDON", &format!("embedded asset unreadable: {k}")),
        }
    }
    if !files.iter().any(|(p, _)| p == "index.html") {
        // A dev build serving from devUrl has no embedded dist.
        crate::log_info("WEBADDON", "no embedded index.html — the ymux-web add-on is unavailable in this build");
        return;
    }
    let label = label_for(env!("CARGO_PKG_VERSION"), &files);
    let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
    crate::log_info(
        "WEBADDON",
        &format!("embedded web bundle {label}: {} files, {bytes} bytes", files.len()),
    );
    let _ = BUNDLE.set(WebBundle { label, files });
}

fn www(home: &str) -> String {
    format!("{home}/.ymux/server/www")
}

/// detect: the installed label (the `current` symlink's target), or "".
pub(crate) async fn detect(handle: &SshHandle<SshClient>, home: &str) -> Result<String, String> {
    let base = www(home);
    exec(
        handle,
        &format!(
            "if [ -f \"{base}/current/index.html\" ]; then basename \"$(readlink \"{base}/current\")\"; fi"
        ),
        8,
    )
    .await
}

/// install / update: upload, swap `current`, keep one previous version.
pub(crate) async fn install(handle: &SshHandle<SshClient>, home: &str) -> Result<String, String> {
    let b = bundle().ok_or("this desktop build has no embedded web bundle")?;
    let base = www(home);
    let tmp = format!("{base}/{}.tmp-{}", b.label, std::process::id());
    let dirs: HashSet<String> = b
        .files
        .iter()
        .filter_map(|(p, _)| p.rsplit_once('/').map(|(d, _)| d.to_string()))
        .collect();
    let mut mk = format!("mkdir -p \"{tmp}\"");
    for d in &dirs {
        mk.push_str(&format!(" \"{tmp}/{d}\""));
    }
    exec(handle, &format!("{mk} && echo OK"), 15)
        .await
        .and_then(|o| if o.contains("OK") { Ok(()) } else { Err(format!("mkdir failed: {}", o.trim())) })?;

    if let Err(e) = upload_all(handle, &tmp, &b.files).await {
        let _ = exec(handle, &format!("rm -rf \"{tmp}\""), 15).await;
        return Err(e);
    }

    // Swap atomically (`mv -T` of a fresh symlink over `current`), then keep
    // `current` plus the newest other version. Labels are [0-9a-z.-] (checked
    // by safe_rel's charset on the files; the label itself is ours).
    let label = &b.label;
    let swap = format!(
        "set -e; cd \"{base}\"; rm -rf \"{label}\"; mv \"{tmp}\" \"{label}\"; \
         ln -sfn \"{label}\" current.new; mv -T current.new current; \
         ls -1t | grep -v -e '^current$' -e '^{label}$' -e '\\.tmp-' | tail -n +2 | while read -r d; do rm -rf -- \"$d\"; done; \
         echo SWAPPED"
    );
    let out = exec(handle, &swap, 30).await?;
    if !out.contains("SWAPPED") {
        return Err(format!("could not switch the web bundle: {}", out.trim()));
    }
    let now = detect(handle, home).await.unwrap_or_default();
    if now.trim() != label {
        return Err(format!("web bundle not active after install (current = {:?})", now.trim()));
    }
    crate::log_info("WEBADDON", &format!("installed {label} ({} files)", b.files.len()));
    Ok(format!("web bundle {label} installed"))
}

/// uninstall: the daemon falls back to its diagnostic page at `/`.
pub(crate) async fn uninstall(handle: &SshHandle<SshClient>, home: &str) -> Result<String, String> {
    let base = www(home);
    let out = exec(handle, &format!("rm -rf \"{base}\" && echo REMOVED"), 20).await?;
    if out.contains("REMOVED") {
        Ok("web bundle removed".into())
    } else {
        Err(format!("could not remove {base}: {}", out.trim()))
    }
}

/// One SFTP session for the whole set (each file is small; ~20 of them).
async fn upload_all(handle: &SshHandle<SshClient>, dir: &str, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open channel: {e}"))?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("request sftp: {e}"))?;
    let sftp = SftpSession::new(chan.into_stream())
        .await
        .map_err(|e| format!("sftp init: {e}"))?;
    sftp.set_timeout(60).await;
    let r = async {
        for (rel, bytes) in files {
            let path = format!("{dir}/{rel}");
            let mut f = sftp
                .create(&path)
                .await
                .map_err(|e| format!("sftp create {rel}: {e}"))?;
            f.write_all(bytes).await.map_err(|e| format!("sftp write {rel}: {e}"))?;
            f.flush().await.ok();
            f.shutdown().await.ok();
        }
        Ok::<(), String>(())
    }
    .await;
    let _ = sftp.close().await;
    r
}

/// After an SSH connect: if this host has the add-on and it is not on our
/// label, bring it there in the background (Yossi: automatic on connect).
/// Never blocks the pane; once per host and label per run; failures only log.
pub(crate) fn spawn_auto_update(handle: Arc<SshHandle<SshClient>>, host_key: String) {
    let Some(want) = label() else { return };
    let key = format!("{host_key}|{want}");
    {
        let set = SYNCED.get_or_init(|| Mutex::new(HashSet::new()));
        let Ok(mut set) = set.lock() else { return };
        if !set.insert(key.clone()) {
            return;
        }
    }
    tauri::async_runtime::spawn(async move {
        let home = remote_home(&handle).await;
        if home.is_empty() {
            return;
        }
        let have = detect(&handle, &home).await.unwrap_or_default();
        let have = have.trim();
        if have.is_empty() || have == want {
            return; // not installed here, or already aligned
        }
        crate::log_info("WEBADDON", &format!("auto-update {have} → {want}"));
        if let Err(e) = install(&handle, &home).await {
            crate::log_warn("WEBADDON", &format!("auto-update failed: {e}"));
            // Let a later connect try again.
            if let Some(set) = SYNCED.get() {
                if let Ok(mut s) = set.lock() {
                    s.remove(&key);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_the_daemon_serves_is_shipped() {
        assert!(shipped("index.html"));
        assert!(shipped("assets/index-abc.js"));
        assert!(shipped("fonts/x.woff2"));
        assert!(!shipped("tauri.svg"));
        assert!(!shipped("vite.svg"));
    }

    #[test]
    fn unsafe_paths_are_refused() {
        assert!(safe_rel("assets/index-Ab_9.js"));
        for bad in ["", "/etc/passwd", "assets/../x", "a//b", "a b", "a;b", "a$(x)", "./a"] {
            assert!(!safe_rel(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn label_tracks_content() {
        let a = vec![("index.html".to_string(), b"<html>".to_vec())];
        let b = vec![("index.html".to_string(), b"<html >".to_vec())];
        let la = label_for("0.5.2", &a);
        assert!(la.starts_with("0.5.2-") && la.len() == "0.5.2-".len() + 8);
        assert_eq!(la, label_for("0.5.2", &a));
        assert_ne!(la, label_for("0.5.2", &b));
    }
}
