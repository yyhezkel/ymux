//! Phase 13.A: smart connect wizard.
//!
//! Affordances that turn the "New workspace → SSH" screen from a raw
//! 4-field form into something an OpenSSH user can drive in seconds:
//!
//!   1. Import from `~/.ssh/config` — parse Host blocks, surface
//!      aliases the user already configured. One click auto-fills
//!      host/user/port/key_path/proxy_command into the modal.
//!
//!   2. Auto-detect keys under `~/.ssh/` — anything starting with `id_`,
//!      ending in `.pem`, or whose first line matches an OpenSSH private
//!      key header. Surfaces filename, last-modified, and fingerprint
//!      (if we can parse the public-key half).
//!
//!   3. Check / fix Windows file permissions (`icacls`) — sshd-style
//!      "too open" private keys, with a one-click remediation.
//!
//!   4. Test connect — open a russh session, run the same auth ladder
//!      `try_authenticate` would (agent → key → password), report which
//!      method worked or the precise error, then close. No workspace
//!      side effects.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log_info;

// ─── ssh-config parsing ────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct SshConfigHost {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
}

fn ssh_config_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".ssh").join("config"))
}

fn expand_tilde(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    if let Some(home) = home_dir() {
        let rest = path.trim_start_matches('~').trim_start_matches(['/', '\\']);
        return home.join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}

/// Very small ssh_config parser — handles `Host`, `HostName`, `User`,
/// `Port`, `IdentityFile`, `ProxyCommand`, `ProxyJump`. Quoting and Match
/// blocks are ignored — anything fancier and the user is better off
/// editing the workspace directly.
pub(crate) fn parse_ssh_config_text(text: &str) -> Vec<SshConfigHost> {
    let mut hosts: Vec<SshConfigHost> = Vec::new();
    let mut cur: Option<SshConfigHost> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on whitespace OR `=` for tokens like `Port=2222`.
        let (kw, rest) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.to_lowercase(), v.trim_start_matches(['=', ' ', '\t']).trim()),
            None => continue,
        };
        match kw.as_str() {
            "host" => {
                if let Some(h) = cur.take() {
                    if !h.alias.is_empty() && h.alias != "*" {
                        hosts.push(h);
                    }
                }
                // Multiple aliases on one Host line — keep the first.
                let alias = rest.split_whitespace().next().unwrap_or("").to_string();
                cur = Some(SshConfigHost {
                    alias,
                    ..Default::default()
                });
            }
            "hostname" => {
                if let Some(h) = cur.as_mut() {
                    h.hostname = Some(rest.to_string());
                }
            }
            "user" => {
                if let Some(h) = cur.as_mut() {
                    h.user = Some(rest.to_string());
                }
            }
            "port" => {
                if let Some(h) = cur.as_mut() {
                    h.port = rest.parse().ok();
                }
            }
            "identityfile" => {
                if let Some(h) = cur.as_mut() {
                    // First match wins — sshd reads top-to-bottom.
                    if h.identity_file.is_none() {
                        h.identity_file = Some(expand_tilde(rest));
                    }
                }
            }
            "proxycommand" => {
                if let Some(h) = cur.as_mut() {
                    h.proxy_command = Some(rest.to_string());
                }
            }
            "proxyjump" => {
                if let Some(h) = cur.as_mut() {
                    h.proxy_jump = Some(rest.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(h) = cur.take() {
        if !h.alias.is_empty() && h.alias != "*" {
            hosts.push(h);
        }
    }
    hosts
}

#[tauri::command]
pub(crate) fn parse_ssh_config() -> Result<Vec<SshConfigHost>, String> {
    let path = ssh_config_path().ok_or_else(|| "no $USERPROFILE/$HOME".to_string())?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    Ok(parse_ssh_config_text(&text))
}

// ─── key discovery ─────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct DetectedKey {
    pub path: String,
    pub filename: String,
    pub modified_iso: Option<String>,
    pub size_bytes: u64,
    pub fingerprint: Option<String>,
    pub key_type: Option<String>,
    pub perms_ok: bool,
    pub perms_error: Option<String>,
}

fn is_likely_private_key(p: &Path) -> bool {
    let stem = p
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if stem.is_empty() {
        return false;
    }
    if stem.ends_with(".pub") || stem.ends_with("known_hosts") || stem == "config" {
        return false;
    }
    if stem.starts_with("id_") || stem.ends_with(".pem") || stem.ends_with(".key") {
        return true;
    }
    // Magic sniff: first line should be an OpenSSH or PEM private key header.
    if let Ok(text) = std::fs::read_to_string(p) {
        if let Some(first) = text.lines().next() {
            if first.contains("PRIVATE KEY") && first.starts_with("-----BEGIN") {
                return true;
            }
        }
    }
    false
}

fn iso_from_systemtime(st: std::time::SystemTime) -> Option<String> {
    let dt: chrono::DateTime<chrono::Utc> = st.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn read_pub_fingerprint(priv_path: &Path) -> (Option<String>, Option<String>) {
    let pub_path = priv_path.with_extension({
        let mut e = priv_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        if !e.is_empty() {
            e.push_str(".pub");
            e
        } else {
            "pub".to_string()
        }
    });
    // Most commonly: keys have no extension; .pub goes alongside.
    let candidate = if priv_path.extension().is_none() {
        priv_path.with_file_name(format!(
            "{}.pub",
            priv_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
        ))
    } else {
        pub_path
    };
    if !candidate.exists() {
        return (None, None);
    }
    let text = match std::fs::read_to_string(&candidate) {
        Ok(t) => t,
        Err(_) => return (None, None),
    };
    let line = text.lines().next().unwrap_or("");
    let mut it = line.split_whitespace();
    let key_type = it.next().map(|s| s.to_string());
    let key_b64 = it.next().unwrap_or("");
    if key_b64.is_empty() {
        return (key_type, None);
    }
    use base64::Engine;
    let decoded = match base64::engine::general_purpose::STANDARD.decode(key_b64) {
        Ok(b) => b,
        Err(_) => return (key_type, None),
    };
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&decoded);
    let digest = hasher.finalize();
    let b64 =
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    (key_type, Some(format!("SHA256:{b64}")))
}

#[tauri::command]
pub(crate) fn list_ssh_keys() -> Result<Vec<DetectedKey>, String> {
    let home = home_dir().ok_or_else(|| "no $USERPROFILE/$HOME".to_string())?;
    let dir = home.join(".ssh");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut keys: Vec<DetectedKey> = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => return Err(format!("read_dir {dir:?}: {e}")),
    };
    for ent in entries.flatten() {
        let p = ent.path();
        if !p.is_file() || !is_likely_private_key(&p) {
            continue;
        }
        let md = match p.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified_iso = md.modified().ok().and_then(iso_from_systemtime);
        let (key_type, fingerprint) = read_pub_fingerprint(&p);
        let (perms_ok, perms_error) = check_perms_inner(&p);
        keys.push(DetectedKey {
            path: p.to_string_lossy().to_string(),
            filename: p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            modified_iso,
            size_bytes: md.len(),
            fingerprint,
            key_type,
            perms_ok,
            perms_error,
        });
    }
    // Most recently modified first — matches what the user is most likely
    // hunting for.
    keys.sort_by(|a, b| b.modified_iso.cmp(&a.modified_iso));
    Ok(keys)
}

// ─── permissions ───────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct PermsResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Unix: sshd and russh-keys reject a private key that any group or other
/// user can read, so the check is the mode bits — the direct equivalent of
/// what the `icacls` parse below approximates on Windows.
///
/// This whole function used to be the `icacls` path unconditionally, so on
/// macOS the spawn failed and EVERY key in the wizard was reported as
/// having bad permissions, with "icacls spawn: No such file or directory"
/// as the explanation.
#[cfg(not(target_os = "windows"))]
fn check_perms_inner(p: &Path) -> (bool, Option<String>) {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) => return (false, Some(format!("stat {}: {e}", p.display()))),
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        (true, None)
    } else {
        (
            false,
            Some(format!("mode {mode:04o} — group/other can read it; want 0600")),
        )
    }
}

#[cfg(target_os = "windows")]
fn check_perms_inner(p: &Path) -> (bool, Option<String>) {
    // We rely on `icacls <path>` and look for entries other than the
    // current user / SYSTEM / Administrators. Anything else means the
    // key is readable by another principal — sshd refuses such keys, and
    // so does russh-keys.
    let out = match std::process::Command::new("icacls").arg(p).output() {
        Ok(o) => o,
        Err(e) => return (false, Some(format!("icacls spawn: {e}"))),
    };
    if !out.status.success() {
        return (
            false,
            Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        );
    }
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    let username = whoami::username();
    let allowed = [
        "NT AUTHORITY\\SYSTEM",
        "BUILTIN\\Administrators",
        "Administrators",
    ];
    let mut foreign = Vec::new();
    for line in txt.lines() {
        // Lines like: "C:\…\key NT AUTHORITY\SYSTEM:(F)" or just
        // "    BUILTIN\Administrators:(F)". Look for a `:(` token, then
        // pull the part to the left as the principal.
        if let Some(idx) = line.find(":(") {
            let lhs = &line[..idx];
            // First line repeats the path; trim its prefix off if present.
            let principal = lhs
                .rsplit_once(|c: char| c.is_whitespace())
                .map(|(_, r)| r.trim())
                .unwrap_or(lhs.trim())
                .to_string();
            let is_self = principal
                .split('\\')
                .last()
                .map(|n| n.eq_ignore_ascii_case(&username))
                .unwrap_or(false);
            if is_self {
                continue;
            }
            if allowed.iter().any(|a| principal.eq_ignore_ascii_case(a)) {
                continue;
            }
            foreign.push(principal);
        }
    }
    if foreign.is_empty() {
        (true, None)
    } else {
        (
            false,
            Some(format!("readable by: {}", foreign.join(", "))),
        )
    }
}

#[tauri::command]
pub(crate) fn check_key_permissions(path: String) -> PermsResult {
    let p = PathBuf::from(&path);
    let (ok, err) = check_perms_inner(&p);
    PermsResult { ok, error: err }
}

/// Unix counterpart of the icacls dance below: `chmod 0600`, which is all
/// "only the owner may read it" means here. Previously this command ran
/// `icacls` on every platform, so the wizard's "Fix permissions" button
/// hard-errored on macOS.
#[cfg(not(target_os = "windows"))]
#[tauri::command]
pub(crate) fn fix_key_permissions(path: String) -> Result<PermsResult, String> {
    use std::os::unix::fs::PermissionsExt;
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("file not found: {path}"));
    }
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod 0600 {path}: {e}"))?;
    log_info("WIZARD", &format!("connect_wizard: chmod 0600 {path}"));
    let (ok, err) = check_perms_inner(&p);
    Ok(PermsResult { ok, error: err })
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub(crate) fn fix_key_permissions(path: String) -> Result<PermsResult, String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("file not found: {path}"));
    }
    // Strategy: disable inheritance, remove all explicit grants, grant
    // the current user full control. Equivalent to `chmod 600` on Unix.
    let user = whoami::username();
    let run = |args: &[&str]| -> Result<(), String> {
        let out = std::process::Command::new("icacls")
            .arg(&p)
            .args(args)
            .output()
            .map_err(|e| format!("icacls spawn: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "icacls {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    };
    run(&["/inheritance:r"])?;
    // Nuke any pre-existing explicit grants (best-effort — ignore failure).
    let _ = std::process::Command::new("icacls")
        .arg(&p)
        .args(["/remove:g", "*S-1-1-0"]) // Everyone
        .output();
    run(&["/grant:r", &format!("{}:(F)", user)])?;
    log_info("WIZARD", &format!("connect_wizard: fixed perms on {path} (user={user})"));
    let (ok, err) = check_perms_inner(&p);
    Ok(PermsResult { ok, error: err })
}

// ─── test connect ──────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct TestResult {
    pub ok: bool,
    pub stage: String, // "tcp" | "banner" | "auth" | "channel" | "complete"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>, // "agent" | "key" | "password"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub elapsed_ms: u64,
}

/// Try a one-shot SSH connect using the same auth ladder as the real
/// session path. We use `std::net::TcpStream::connect_timeout` for the
/// pre-flight so a misconfigured firewall reports `Connection refused`
/// or `Timeout` cleanly instead of getting lost in the russh layers.
#[tauri::command]
pub(crate) async fn test_ssh_connect(
    host: String,
    user: String,
    port: u16,
    key_path: Option<String>,
    key_passphrase: Option<String>,
    password: Option<String>,
) -> TestResult {
    use std::net::ToSocketAddrs;
    use std::time::Instant;

    let started = Instant::now();
    let target = format!("{host}:{port}");
    let addrs: Vec<_> = match target.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            return TestResult {
                ok: false,
                stage: "dns".into(),
                method: None,
                server_version: None,
                message: Some(format!("dns: {e}")),
                hint: Some("Check the host name spelling.".into()),
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
    };
    let addr = match addrs.into_iter().next() {
        Some(a) => a,
        None => {
            return TestResult {
                ok: false,
                stage: "dns".into(),
                method: None,
                server_version: None,
                message: Some("no addresses resolved".into()),
                hint: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
    };
    // Pre-flight TCP probe with a short timeout — distinguishes
    // "host unreachable" from "ssh handshake failed".
    if let Err(e) = std::net::TcpStream::connect_timeout(
        &addr,
        std::time::Duration::from_secs(6),
    ) {
        let msg = e.to_string();
        let hint = if msg.contains("refused") {
            Some("SSH not listening on this port — verify sshd is up or the right port.".into())
        } else if msg.to_lowercase().contains("timed out") {
            Some("Network path blocked — check firewall / VPN.".into())
        } else {
            None
        };
        return TestResult {
            ok: false,
            stage: "tcp".into(),
            method: None,
            server_version: None,
            message: Some(msg),
            hint,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
    }

    // Try the russh handshake. A tolerant client handler accepts any
    // server key — this is a TEST, not a real session; TOFU enforcement
    // is the real `try_authenticate` path's concern.
    use russh::client;
    // Phase 38: keepalive on the test connection too (consistency).
    let config = std::sync::Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let connect_fut = client::connect(config, (host.as_str(), port), TestClient);
    let mut handle = match tokio::time::timeout(std::time::Duration::from_secs(10), connect_fut)
        .await
    {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            return TestResult {
                ok: false,
                stage: "banner".into(),
                method: None,
                server_version: None,
                message: Some(e.to_string()),
                hint: Some("SSH handshake failed — wrong port or non-SSH service.".into()),
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        Err(_) => {
            return TestResult {
                ok: false,
                stage: "banner".into(),
                method: None,
                server_version: None,
                message: Some("SSH handshake timed out".into()),
                hint: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
    };
    let server_version = None; // russh doesn't expose the banner in this version.

    // 1) Agent. Reuse the same approach as try_authenticate.
    let agent_ok = try_agent_auth(&mut handle, &user).await;
    if let Some(true) = agent_ok {
        return TestResult {
            ok: true,
            stage: "complete".into(),
            method: Some("agent".into()),
            server_version,
            message: Some(format!("Connected via ssh-agent as {user}")),
            hint: None,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
    }

    // 2) Explicit key.
    if let Some(p) = key_path {
        // Same path-bypass we ship in try_authenticate v2 — read with
        // std::fs ourselves, hand the bytes to decode_secret_key.
        match std::fs::read_to_string(&p) {
            Ok(text) => match russh::keys::decode_secret_key(&text, key_passphrase.as_deref()) {
                Ok(key) => match crate::pkwh_pub(key) {
                    Ok(pkwh) => match handle.authenticate_publickey(&user, pkwh).await.map(|r| r.success()) {
                        Ok(true) => {
                            return TestResult {
                                ok: true,
                                stage: "complete".into(),
                                method: Some("key".into()),
                                server_version,
                                message: Some(format!("Connected with key {p}")),
                                hint: None,
                                elapsed_ms: started.elapsed().as_millis() as u64,
                            };
                        }
                        Ok(false) => { /* fall through */ }
                        Err(e) => {
                            return TestResult {
                                ok: false,
                                stage: "auth".into(),
                                method: Some("key".into()),
                                server_version,
                                message: Some(e.to_string()),
                                hint: Some(
                                    "Server rejected this key — confirm authorized_keys."
                                        .into(),
                                ),
                                elapsed_ms: started.elapsed().as_millis() as u64,
                            };
                        }
                    },
                    Err(e) => {
                        return TestResult {
                            ok: false,
                            stage: "auth".into(),
                            method: Some("key".into()),
                            server_version,
                            message: Some(e),
                            hint: None,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        };
                    }
                },
                Err(e) => {
                    let s = e.to_string();
                    let hint = if s.to_lowercase().contains("passphrase") {
                        Some("Key is encrypted — provide a passphrase.".into())
                    } else {
                        None
                    };
                    return TestResult {
                        ok: false,
                        stage: "key-load".into(),
                        method: Some("key".into()),
                        server_version,
                        message: Some(s),
                        hint,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    };
                }
            },
            Err(e) => {
                return TestResult {
                    ok: false,
                    stage: "key-load".into(),
                    method: Some("key".into()),
                    server_version,
                    message: Some(format!("read {p}: {e}")),
                    hint: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                };
            }
        }
    }

    // 3) Password.
    if let Some(pw) = password {
        match handle.authenticate_password(&user, &pw).await.map(|r| r.success()) {
            Ok(true) => {
                return TestResult {
                    ok: true,
                    stage: "complete".into(),
                    method: Some("password".into()),
                    server_version,
                    message: Some(format!("Connected with password as {user}")),
                    hint: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                };
            }
            Ok(false) => {}
            Err(e) => {
                return TestResult {
                    ok: false,
                    stage: "auth".into(),
                    method: Some("password".into()),
                    server_version,
                    message: Some(e.to_string()),
                    hint: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                };
            }
        }
    }

    TestResult {
        ok: false,
        stage: "auth".into(),
        method: None,
        server_version,
        message: Some("All auth methods refused".into()),
        hint: Some(
            "Try providing a key path or password, or add the key to ssh-agent.".into(),
        ),
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

/// Wizard-scoped agent auth — mirrors `lib.rs::try_agent_auth` but does
/// not touch shared state. Tries the OpenSSH-for-Windows agent and
/// Pageant pipes in turn; first identity that authenticates wins.
async fn try_agent_auth(
    handle: &mut russh::client::Handle<TestClient>,
    user: &str,
) -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        for pipe_path in [r"\\.\pipe\openssh-ssh-agent", r"\\.\pipe\pageant"] {
            let connect_fut =
                russh::keys::agent::client::AgentClient::connect_named_pipe(pipe_path);
            let mut agent = match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                connect_fut,
            )
            .await
            {
                Ok(Ok(a)) => a,
                _ => continue,
            };
            let identities = match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                agent.request_identities(),
            )
            .await
            {
                Ok(Ok(ids)) => ids,
                _ => continue,
            };
            if identities.is_empty() {
                continue;
            }
            for id in identities {
                if let Ok(true) =
                    ymux_ssh::auth_with_agent_identity(handle, user, id, &mut agent).await
                {
                    return Some(true);
                }
            }
        }
        Some(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (handle, user);
        None
    }
}

pub(crate) struct TestClient;
impl russh::client::Handler for TestClient {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

// We expose `pkwh` from lib.rs as `pkwh_pub` for re-use. See the helper
// added there.
#[allow(dead_code)]
fn _ignore() -> Value {
    Value::Null
}
