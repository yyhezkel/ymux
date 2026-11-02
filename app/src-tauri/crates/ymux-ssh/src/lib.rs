//! Phase 51.H: SSH authentication primitives extracted from
//! `app/src-tauri/src/lib.rs`.
//!
//! Per the conversation: the broader SSH-flow code Yossi's spec
//! originally targeted for `ymux-ssh` (`connect_and_authenticate`,
//! `setup_workspace_reverse_tunnel`, `spawn_port_watcher`, etc.) is
//! orchestration that takes `&AppState + &AppHandle` and stitches many
//! helpers — same AppState-coupling that kept rpc / feed / workspaces
//! in `app`. This crate holds only the pure-auth subset, the functions
//! that take just a russh `Handle<SshClient>` plus credentials and
//! return a result. ~270 LOC.
//!
//! What ships here:
//!   AuthMethod                     — which auth method succeeded
//!   key_load_needs_passphrase      — error-message classifier
//!   pkwh / pkwh_pub                — RSA-aware PrivateKey wrapper
//!   try_agent_auth                 — OpenSSH agent + Pageant via NP
//!   try_authenticate               — full 4-step auth ladder
//!
//! No AppState, no tauri. Depends on `ymux-core` for `SshClient` +
//! `dlog`, plus `russh` / `russh-keys` / `tokio` (time + macros).

use std::path::Path;
use std::sync::Arc;

use russh::client;
use russh_keys::key::PrivateKeyWithHashAlg;
use russh_keys::{HashAlg, PrivateKey};

use ymux_core::{log_debug, log_error, log_warn, SshClient};

/// Which auth method succeeded for the active SSH session. Surfaced to
/// `spawn_ssh` (still in `app`) so a successful Password auth can prompt
/// the user to convert to key auth (faster + no password-typing on
/// future connects). Phase 32.B introduced this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    Agent,
    Key,
    Password,
}

/// Heuristic for "the key on disk is encrypted, give me a passphrase."
/// Used to route a passphrase-missing failure to the right UI prompt
/// instead of a generic "load key" error.
pub fn key_load_needs_passphrase(err: &str) -> bool {
    let s = err.to_lowercase();
    s.contains("encrypted")
        || s.contains("passphrase")
        || s.contains("pem")
        || s.contains("kdf")
        || s.contains("decrypt")
}

/// Public re-export of `pkwh` for the connect-wizard `test_ssh_connect`
/// path so it can share the same RSA hash-alg logic without duplicating it.
pub fn pkwh_pub(key: PrivateKey) -> Result<PrivateKeyWithHashAlg, String> {
    pkwh(key)
}

/// Wraps a `PrivateKey` for authentication. RSA keys get SHA-512;
/// everything else uses None.
pub fn pkwh(key: PrivateKey) -> Result<PrivateKeyWithHashAlg, String> {
    let key = Arc::new(key);
    let hash_alg = if key.algorithm().is_rsa() {
        Some(HashAlg::Sha512)
    } else {
        None
    };
    PrivateKeyWithHashAlg::new(key, hash_alg).map_err(|e| e.to_string())
}

/// Try ssh-agent auth via OpenSSH-for-Windows agent and Pageant in turn.
/// Returns `Some(true)` if any identity authenticated; `Some(false)` if
/// an agent was found but no identity authenticated; `None` if no agent
/// was reachable at all.
///
/// Both backends speak the OpenSSH agent protocol over a Windows named
/// pipe — they only differ in pipe name (`openssh-ssh-agent` vs
/// `pageant`). We hit them through
/// `russh_keys::agent::client::AgentClient::connect_named_pipe`, which
/// returns `Result` cleanly. We deliberately do NOT use
/// `connect_pageant()`, because the `pageant-0.0.1` crate it uses
/// internally has an `unwrap()` at `pageant_impl.rs:64` that panics on
/// benign Windows API return codes when Pageant isn't running.
pub async fn try_agent_auth(
    handle: &mut client::Handle<SshClient>,
    user: &str,
) -> Option<bool> {
    let mut any_agent_seen = false;

    #[cfg(windows)]
    for (label, pipe_path) in [
        ("openssh-ssh-agent", r"\\.\pipe\openssh-ssh-agent"),
        ("pageant", r"\\.\pipe\pageant"),
    ] {
        log_debug("SSH", &format!("ssh.auth: agent probe {label} ({pipe_path})"));
        // Hard 2-second cap on the connect — if Pageant's pipe is alive but
        // its server is wedged, `connect_named_pipe` can block indefinitely.
        let connect_fut =
            russh_keys::agent::client::AgentClient::connect_named_pipe(pipe_path);
        let mut agent = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect_fut,
        )
        .await
        {
            Ok(Ok(a)) => {
                log_debug("SSH", &format!("ssh.auth: agent probe {label} CONNECTED"));
                a
            }
            Ok(Err(e)) => {
                log_debug("SSH", &format!("ssh.auth: agent probe {label} not reachable: {e}"));
                continue;
            }
            Err(_) => {
                log_warn("SSH", &format!(
                    "ssh.auth: agent probe {label} TIMED OUT after 2s — skipping"
                ));
                continue;
            }
        };
        let identities = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            agent.request_identities(),
        )
        .await
        {
            Ok(Ok(ids)) => {
                log_debug("SSH", &format!("ssh.auth: agent {label} offered {} identit(y/ies)", ids.len()));
                ids
            }
            Ok(Err(e)) => {
                log_warn("SSH", &format!("ssh.auth: agent {label} request_identities: {e}"));
                continue;
            }
            Err(_) => {
                log_warn("SSH", &format!(
                    "ssh.auth: agent {label} request_identities TIMED OUT after 2s — skipping"
                ));
                continue;
            }
        };
        if identities.is_empty() {
            continue;
        }
        any_agent_seen = true;
        for id in identities {
            log_debug("SSH", &format!("ssh.auth: agent {label} attempting authenticate_publickey_with"));
            match handle.authenticate_publickey_with(user, id, &mut agent).await {
                Ok(true) => {
                    log_debug("SSH", &format!("ssh.auth: agent {label} authenticated OK"));
                    return Some(true);
                }
                Ok(false) => {
                    log_debug("SSH", &format!("ssh.auth: agent {label} key not accepted by server"));
                    continue;
                }
                Err(e) => {
                    log_warn("SSH", &format!("ssh.auth: agent {label} auth error: {e}"));
                    continue;
                }
            }
        }
    }

    // Unix/macOS: the one agent transport is the Unix socket in
    // SSH_AUTH_SOCK (covers ssh-agent, gpg-agent, 1Password, …).
    // Same 2s caps as the Windows probes.
    #[cfg(not(windows))]
    'unix_agent: {
        let label = "ssh-agent (SSH_AUTH_SOCK)";
        if std::env::var("SSH_AUTH_SOCK").map(|v| v.is_empty()).unwrap_or(true) {
            log_debug("SSH", "ssh.auth: SSH_AUTH_SOCK not set — no agent");
            break 'unix_agent;
        }
        log_debug("SSH", &format!("ssh.auth: agent probe {label}"));
        let connect_fut = russh_keys::agent::client::AgentClient::connect_env();
        let mut agent = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect_fut,
        )
        .await
        {
            Ok(Ok(a)) => {
                log_debug("SSH", &format!("ssh.auth: agent probe {label} CONNECTED"));
                a
            }
            Ok(Err(e)) => {
                log_debug("SSH", &format!("ssh.auth: agent probe {label} not reachable: {e}"));
                break 'unix_agent;
            }
            Err(_) => {
                log_warn("SSH", &format!(
                    "ssh.auth: agent probe {label} TIMED OUT after 2s — skipping"
                ));
                break 'unix_agent;
            }
        };
        let identities = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            agent.request_identities(),
        )
        .await
        {
            Ok(Ok(ids)) => {
                log_debug("SSH", &format!("ssh.auth: agent {label} offered {} identit(y/ies)", ids.len()));
                ids
            }
            Ok(Err(e)) => {
                log_warn("SSH", &format!("ssh.auth: agent {label} request_identities: {e}"));
                break 'unix_agent;
            }
            Err(_) => {
                log_warn("SSH", &format!(
                    "ssh.auth: agent {label} request_identities TIMED OUT after 2s — skipping"
                ));
                break 'unix_agent;
            }
        };
        if identities.is_empty() {
            break 'unix_agent;
        }
        any_agent_seen = true;
        for id in identities {
            log_debug("SSH", &format!("ssh.auth: agent {label} attempting authenticate_publickey_with"));
            match handle.authenticate_publickey_with(user, id, &mut agent).await {
                Ok(true) => {
                    log_debug("SSH", &format!("ssh.auth: agent {label} authenticated OK"));
                    return Some(true);
                }
                Ok(false) => {
                    log_debug("SSH", &format!("ssh.auth: agent {label} key not accepted by server"));
                    continue;
                }
                Err(e) => {
                    log_warn("SSH", &format!("ssh.auth: agent {label} auth error: {e}"));
                    continue;
                }
            }
        }
    }

    if any_agent_seen {
        log_debug("SSH", "ssh.auth: agent probes done — no agent identity worked");
        Some(false)
    } else {
        log_debug("SSH", "ssh.auth: no agent reachable on any pipe");
        None
    }
}

/// Default private-key paths under `home`; empty when no home dir is known.
fn default_key_paths(home: Option<String>) -> Vec<String> {
    match home {
        None => Vec::new(),
        Some(h) => ["id_ed25519", "id_ecdsa", "id_rsa"]
            .iter()
            .map(|n| format!("{h}/.ssh/{n}"))
            .collect(),
    }
}

/// 4-step auth ladder: agent → explicit-key → default-keys → password.
/// Returns the method that succeeded (or `None` if all 4 are exhausted).
/// On a passphrase-encrypted explicit key that lacks the right
/// passphrase, returns an Err with the framed
/// `KEY_PASSPHRASE_REQUIRED:<path>` or `KEY_PASSPHRASE_BAD:<path>:<reason>`
/// strings the FE parses to drive the passphrase prompt.
pub async fn try_authenticate(
    handle: &mut client::Handle<SshClient>,
    user: &str,
    key_path: Option<&str>,
    key_passphrase: Option<&str>,
    password: Option<&str>,
) -> Result<Option<AuthMethod>, String> {
    log_debug("SSH", &format!(
        "ssh.auth: begin user={} key_path={:?} key_passphrase={} password={}",
        user,
        key_path,
        if key_passphrase.is_some() { "yes" } else { "no" },
        if password.is_some() { "yes" } else { "no" }
    ));

    // 1) ssh-agent (OpenSSH agent / Pageant via named pipe).
    log_debug("SSH", "ssh.auth: step 1 — try_agent_auth");
    if let Some(true) = try_agent_auth(handle, user).await {
        log_debug("SSH", "ssh.auth: step 1 OK (agent)");
        return Ok(Some(AuthMethod::Agent));
    }

    // 2) Explicit key file (with optional passphrase).
    //
    // SSH-key-load Windows fix: `russh_keys::load_secret_key` opens the file
    // through its own internal helper that, on certain russh-keys versions,
    // funnels the path through Win32 in a way that rejects perfectly valid
    // Windows paths with `os error 123` (ERROR_INVALID_NAME) — even when the
    // exact same path opens fine via `ssh -i`. We sidestep the bug by reading
    // the file with std::fs ourselves (which uses CreateFileW correctly) and
    // handing the bytes to russh-keys' in-memory parser, `decode_secret_key`.
    if let Some(p) = key_path {
        log_debug("SSH", &format!(
            "ssh.auth: step 2 — explicit key file {p:?} bytes={:?} len={}",
            p.as_bytes(),
            p.len()
        ));
        let key_text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                let s = e.to_string();
                log_error("SSH", &format!("ssh.auth: read {p} ERR: {s}"));
                return Err(format!("load key {p}: {s}"));
            }
        };
        log_debug("SSH", &format!(
            "ssh.auth: read {p} OK ({} bytes, head={:?})",
            key_text.len(),
            key_text.lines().next().unwrap_or("")
        ));
        match russh_keys::decode_secret_key(&key_text, key_passphrase) {
            Ok(key) => {
                log_debug("SSH", &format!(
                    "ssh.auth: key {p} decoded — attempting authenticate_publickey"
                ));
                let pkwh = pkwh(key)?;
                let r = handle
                    .authenticate_publickey(user, pkwh)
                    .await
                    .map_err(|e| e.to_string())?;
                log_debug("SSH", &format!("ssh.auth: step 2 publickey result = {r}"));
                if r {
                    return Ok(Some(AuthMethod::Key));
                }
            }
            Err(e) => {
                let s = e.to_string();
                log_warn("SSH", &format!("ssh.auth: decode_secret_key {p} ERR: {s}"));
                if key_load_needs_passphrase(&s) {
                    if key_passphrase.is_none() {
                        return Err(format!("KEY_PASSPHRASE_REQUIRED:{}", p));
                    }
                    return Err(format!("KEY_PASSPHRASE_BAD:{}:{}", p, s));
                }
                return Err(format!("load key {p}: {s}"));
            }
        }
    }

    // 3) Default key paths (tried without passphrase; encrypted keys silently skipped).
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let default_paths = default_key_paths(home);
    if default_paths.is_empty() {
        log_debug("SSH", "ssh.auth: step 3 skipped, no home dir");
    } else {
        log_debug("SSH", "ssh.auth: step 3 — default key paths under ~/.ssh/");
    }
    for p in default_paths {
        if !Path::new(&p).exists() {
            continue;
        }
        log_debug("SSH", &format!("ssh.auth: step 3 trying {p}"));
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => {
                log_debug("SSH", &format!("ssh.auth: step 3 read {p} skip: {e}"));
                continue;
            }
        };
        if let Ok(key) = russh_keys::decode_secret_key(&text, None) {
            if let Ok(pkwh) = pkwh(key) {
                let r = handle
                    .authenticate_publickey(user, pkwh)
                    .await
                    .map_err(|e| e.to_string())?;
                log_debug("SSH", &format!("ssh.auth: step 3 {p} result = {r}"));
                if r {
                    return Ok(Some(AuthMethod::Key));
                }
            }
        }
    }

    // 4) Password (sent to remote, not key passphrase).
    if let Some(pw) = password {
        log_debug("SSH", "ssh.auth: step 4 — password");
        let r = handle
            .authenticate_password(user, pw)
            .await
            .map_err(|e| e.to_string())?;
        log_debug("SSH", &format!("ssh.auth: step 4 password result = {r}"));
        if r {
            return Ok(Some(AuthMethod::Password));
        }
    }

    log_warn("SSH", "ssh.auth: ALL methods exhausted, no auth succeeded");
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::default_key_paths;

    // pins: no home dir → no default key paths (step 3 skipped, not Err)
    #[test]
    fn no_home_yields_no_paths() {
        assert!(default_key_paths(None).is_empty());
    }

    // pins: key order ed25519, ecdsa, rsa under {home}/.ssh
    #[test]
    fn home_yields_three_paths_in_order() {
        assert_eq!(
            default_key_paths(Some("/h".into())),
            vec!["/h/.ssh/id_ed25519", "/h/.ssh/id_ecdsa", "/h/.ssh/id_rsa"]
        );
    }
}
