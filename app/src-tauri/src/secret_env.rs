//! Secret workspace env rows (`EnvVar.secret`).
//!
//! A secret row keeps its value out of `workspaces.json`, RPC output and
//! typed exports; it is delivered only at spawn (`cmd.env` locally,
//! `channel.set_env` over SSH). Values live here, keyed by env owner
//! (header id) then variable name. Windows persists them DPAPI-encrypted in
//! `<config>/secret-env.json`; other platforms hold them in memory only
//! (CLAUDE.md Rule #2). Nothing in this file logs or formats a value.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ymux_types::{EnvVar, Workspace};

const FILE_VERSION: u32 = 1;

/// owner id → variable name → plaintext value (memory) .
#[derive(Default, Clone)]
pub(crate) struct SecretEnvStore {
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DiskFile {
    version: u32,
    /// owner → key → base64 DPAPI blob
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

impl SecretEnvStore {
    /// Missing file → empty store. Non-Windows → always empty (memory only).
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        if !cfg!(windows) {
            return Ok(Self::default());
        }
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("read secret-env.json: {e}")),
        };
        let file: DiskFile =
            serde_json::from_str(&raw).map_err(|e| format!("parse secret-env.json: {e}"))?;
        let mut entries = BTreeMap::new();
        for (owner, rows) in file.entries {
            let mut out = BTreeMap::new();
            for (key, blob) in rows {
                out.insert(key, unprotect_b64(&blob)?);
            }
            entries.insert(owner, out);
        }
        Ok(Self { entries })
    }

    /// Atomic tmp + rename (Rule #7). Non-Windows → no-op.
    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        if !cfg!(windows) {
            return Ok(());
        }
        let mut entries = BTreeMap::new();
        for (owner, rows) in &self.entries {
            let mut out = BTreeMap::new();
            for (key, value) in rows {
                out.insert(key.clone(), protect_b64(value)?);
            }
            entries.insert(owner.clone(), out);
        }
        let json = serde_json::to_string_pretty(&DiskFile {
            version: FILE_VERSION,
            entries,
        })
        .map_err(|e| format!("encode secret-env.json: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("write secret-env.json.tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename secret-env.json: {e}"))
    }

    /// Names (never values) that have a stored value for `owner`.
    pub(crate) fn keys_for(&self, owner: &str) -> Vec<String> {
        self.entries
            .get(owner)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Single enforcement point for the invariant "no `secret: true` row in
    /// `workspaces` holds a value". Moves values into the store, blanks the
    /// rows, keeps the stored value when the row's value is empty, and prunes
    /// entries whose row was removed / un-flagged / whose owner is gone.
    /// Returns true when the store changed (caller saves it).
    pub(crate) fn reconcile(&mut self, workspaces: &mut [Workspace]) -> bool {
        let owners: Vec<Option<String>> = workspaces
            .iter()
            .map(|w| env_owner(workspaces, &w.id))
            .collect();
        let mut live: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut changed = false;
        for (w, owner) in workspaces.iter_mut().zip(owners) {
            let Some(owner) = owner else { continue };
            for row in w.env.iter_mut().filter(|r| r.secret) {
                live.entry(owner.clone()).or_default().insert(row.key.clone());
                if !row.value.is_empty() {
                    let value = std::mem::take(&mut row.value);
                    let slot = self.entries.entry(owner.clone()).or_default();
                    if slot.get(&row.key) != Some(&value) {
                        slot.insert(row.key.clone(), value);
                        changed = true;
                    }
                }
            }
        }
        let before: usize = self.entries.values().map(|m| m.len()).sum();
        self.entries.retain(|owner, rows| {
            let Some(keys) = live.get(owner) else { return false };
            rows.retain(|k, _| keys.contains(k));
            !rows.is_empty()
        });
        let after: usize = self.entries.values().map(|m| m.len()).sum();
        changed || before != after
    }

    /// Stored values for `keys` under `owner`; `Err(missing names)` when any
    /// has no stored value.
    pub(crate) fn resolve(
        &self,
        owner: &str,
        keys: &[String],
    ) -> Result<Vec<(String, String)>, Vec<String>> {
        let rows = self.entries.get(owner);
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for k in keys {
            match rows.and_then(|m| m.get(k)) {
                Some(v) => found.push((k.clone(), v.clone())),
                None => missing.push(k.clone()),
            }
        }
        if missing.is_empty() {
            Ok(found)
        } else {
            Err(missing)
        }
    }
}

/// Header owns its env; a screen uses its parent's. None → id unknown.
pub(crate) fn env_owner(workspaces: &[Workspace], id: &str) -> Option<String> {
    let w = workspaces.iter().find(|w| w.id == id)?;
    if crate::is_header(w) {
        Some(w.id.clone())
    } else {
        w.parent_id.clone()
    }
}

/// Blank every secret value in place.
pub(crate) fn redact(env: &mut [EnvVar]) {
    for row in env.iter_mut().filter(|r| r.secret) {
        row.value.clear();
    }
}

/// Redacted copy, for RPC serializers.
pub(crate) fn redact_workspace(w: &Workspace) -> Workspace {
    let mut out = w.clone();
    redact(&mut out.env);
    out
}

/// (plain rows for typed exports, secret names). Secret rows never reach
/// the typed `export K=V` path.
pub(crate) fn split_env(env: &[EnvVar]) -> (Vec<EnvVar>, Vec<String>) {
    let plain = env.iter().filter(|r| !r.secret).cloned().collect();
    let secret = env
        .iter()
        .filter(|r| r.secret)
        .map(|r| r.key.clone())
        .collect();
    (plain, secret)
}

/// Pane status text for names sshd (or a missing store entry) refused.
pub(crate) fn refused_message(keys: &[String]) -> String {
    format!("environment variable refused by sshd: {}", keys.join(", "))
}

/// sshd reply to a `want_reply` env request: `Some(refused)`, `None` while
/// the message is unrelated (window adjust, etc.).
fn reply_refused(msg: &russh::ChannelMsg) -> Option<bool> {
    match msg {
        russh::ChannelMsg::Success => Some(false),
        russh::ChannelMsg::Failure => Some(true),
        _ => None,
    }
}

/// Send each secret as an SSH `env` request that waits for sshd's reply and
/// return the names refused (AcceptEnv miss, send error, close, 5 s silence).
/// Names only: values never leave this fn except into `set_env`.
pub(crate) async fn deliver_ssh(
    channel: &mut russh::Channel<russh::client::Msg>,
    vars: &[(String, String)],
) -> Vec<String> {
    const REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
    let mut refused = Vec::new();
    for (k, v) in vars {
        if channel.set_env(true, k.as_str(), v.as_str()).await.is_err() {
            refused.push(k.clone());
            continue;
        }
        let reply = tokio::time::timeout(REPLY_WAIT, async {
            loop {
                match channel.wait().await {
                    Some(m) => {
                        if let Some(r) = reply_refused(&m) {
                            break r;
                        }
                    }
                    None => break true,
                }
            }
        })
        .await
        // AI-NOTE: timeout counts as refused so the user is told, never silently dropped
        .unwrap_or(true);
        if reply {
            refused.push(k.clone());
        }
    }
    refused
}

#[cfg(windows)]
fn protect_b64(plain: &str) -> Result<String, String> {
    use base64::Engine;
    let blob = dpapi(plain.as_bytes(), true)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

#[cfg(windows)]
fn unprotect_b64(b64: &str) -> Result<String, String> {
    use base64::Engine;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("secret-env blob base64: {e}"))?;
    let plain = dpapi(&blob, false)?;
    String::from_utf8(plain).map_err(|_| "secret-env blob is not utf-8".to_string())
}

#[cfg(not(windows))]
fn protect_b64(_plain: &str) -> Result<String, String> {
    Err("secret env is memory-only on this platform".to_string())
}

#[cfg(not(windows))]
fn unprotect_b64(_b64: &str) -> Result<String, String> {
    Err("secret env is memory-only on this platform".to_string())
}

/// CryptProtectData / CryptUnprotectData round trip. The only `unsafe` here.
#[cfg(windows)]
fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };
    let len = u32::try_from(input.len()).map_err(|_| "secret too large".to_string())?;
    let inb = CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `inb` points at `input` for the call's duration; DPAPI only
    // reads it. `out.pbData` is LocalAlloc'd by DPAPI, copied, then freed.
    unsafe {
        let ok = if protect {
            CryptProtectData(
                &inb,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &inb,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out,
            )
        };
        if ok == 0 || out.pbData.is_null() {
            return Err(format!(
                "DPAPI {} failed (os error {})",
                if protect { "protect" } else { "unprotect" },
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
            ));
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as *mut core::ffi::c_void);
        Ok(bytes)
    }
}

#[cfg(test)]
mod secret_env_tests {
    use super::*;

    fn ev(k: &str, v: &str, secret: bool) -> EnvVar {
        EnvVar {
            key: k.into(),
            value: v.into(),
            secret,
        }
    }

    fn ws(id: &str, parent: Option<&str>, env: Vec<EnvVar>) -> Workspace {
        Workspace {
            id: id.into(),
            name: id.into(),
            parent_id: parent.map(str::to_string),
            env,
            ..Default::default()
        }
    }

    // Pins the core invariant: a secret value never stays on a workspace row.
    #[test]
    fn reconcile_moves_secret_values_out_of_workspaces() {
        let mut s = SecretEnvStore::default();
        let mut w = vec![ws("h", None, vec![ev("TOKEN", "abc", true), ev("A", "1", false)])];
        assert!(s.reconcile(&mut w));
        assert_eq!(w[0].env[0].value, "");
        assert_eq!(w[0].env[1].value, "1");
        assert_eq!(s.keys_for("h"), vec!["TOKEN".to_string()]);
        assert_eq!(
            s.resolve("h", &["TOKEN".into()]).ok(),
            Some(vec![("TOKEN".into(), "abc".into())])
        );
    }

    // Pins RPC redaction: every secret value blanked, plain rows untouched.
    #[test]
    fn redact_blanks_every_secret_value() {
        let w = ws("h", None, vec![ev("S1", "x", true), ev("S2", "y", true), ev("P", "z", false)]);
        let r = redact_workspace(&w);
        assert_eq!(r.env[0].value, "");
        assert_eq!(r.env[1].value, "");
        assert_eq!(r.env[2].value, "z");
        assert_eq!(w.env[0].value, "x");
    }

    // Pins: secrets never join the typed `export` path.
    #[test]
    fn split_env_keeps_secret_rows_out_of_typed_exports() {
        let (plain, secret) = split_env(&[ev("S", "v", true), ev("P", "1", false)]);
        assert!(plain.iter().all(|r| !r.secret && r.key != "S"));
        assert_eq!(secret, vec!["S".to_string()]);
    }

    // Pins: non-secret rows still flow to typed exports unchanged.
    #[test]
    fn split_env_returns_plain_rows_for_typed_exports() {
        let (plain, secret) = split_env(&[ev("A", "1", false), ev("B", "2", false)]);
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[1].value, "2");
        assert!(secret.is_empty());
    }

    // Pins pane status text: user must see which variable was refused, no value.
    #[test]
    fn refused_message_names_the_variable() {
        let m = refused_message(&["API_KEY".into(), "TOK".into()]);
        assert_eq!(m, "environment variable refused by sshd: API_KEY, TOK");
    }

    // Pins edit semantics: empty value on a secret row keeps the stored value.
    #[test]
    fn empty_secret_value_keeps_stored_value() {
        let mut s = SecretEnvStore::default();
        let mut w = vec![ws("h", None, vec![ev("K", "v1", true)])];
        s.reconcile(&mut w);
        assert!(!s.reconcile(&mut w));
        assert_eq!(s.resolve("h", &["K".into()]).ok().map(|v| v[0].1.clone()), Some("v1".into()));
    }

    // Pins pruning: un-flagged rows, removed rows and deleted owners drop.
    #[test]
    fn reconcile_prunes_removed_rows_and_deleted_owners() {
        let mut s = SecretEnvStore::default();
        let mut w = vec![
            ws("h", None, vec![ev("A", "1", true), ev("B", "2", true)]),
            ws("g", None, vec![ev("C", "3", true)]),
        ];
        s.reconcile(&mut w);
        w[0].env[0].secret = false; // flipped
        w[0].env.remove(1); // removed
        w.remove(1); // owner deleted
        assert!(s.reconcile(&mut w));
        assert!(s.keys_for("h").is_empty());
        assert!(s.keys_for("g").is_empty());
    }

    // Pins owner rule: a screen resolves secrets from its header, not itself.
    #[test]
    fn screen_resolves_secret_from_its_header() {
        let mut s = SecretEnvStore::default();
        let mut w = vec![ws("h", None, vec![ev("K", "v", true)]), ws("s", Some("h"), vec![])];
        s.reconcile(&mut w);
        let owner = env_owner(&w, "s");
        assert_eq!(owner.as_deref(), Some("h"));
        assert!(s.resolve(owner.as_deref().unwrap_or(""), &["K".into()]).is_ok());
        assert_eq!(s.resolve("h", &["MISSING".into()]).err(), Some(vec!["MISSING".to_string()]));
    }

    // Pins sshd reply mapping: Success=accepted, Failure=refused, others ignored.
    #[test]
    fn sshd_reply_maps_to_refused() {
        assert_eq!(reply_refused(&russh::ChannelMsg::Success), Some(false));
        assert_eq!(reply_refused(&russh::ChannelMsg::Failure), Some(true));
        assert_eq!(reply_refused(&russh::ChannelMsg::Eof), None);
    }

    // Pins the DPAPI path: protect→unprotect returns the original, blob differs.
    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let b = protect_b64("s3cret").unwrap();
        assert_ne!(b, "s3cret");
        assert_eq!(unprotect_b64(&b).unwrap(), "s3cret");
    }
}
