//! Phase 70.D — mobile pairing Tauri commands.
//!
//! Drives the `nginx-proxy` add-on install (with the domain + Cloudflare
//! token) and the daemon's `/api/pairing/*` endpoints (curled over the
//! workspace SSH session, like `insights_fetch`). Returns JSON strings the
//! Mobile tab parses — no ts-rs bindings needed.
//!
//! Rule #2: the Cloudflare token is `Zeroize`d desktop-side after the install
//! returns. It persists remote-side only in `/etc/ymux/cloudflare.ini`
//! (mode-600 root) because certbot's auto-renew needs it.

use serde_json::json;
use tauri::State;
use zeroize::Zeroize;

use crate::addons::{exec, exec_stdin, nginx_proxy_install, pick_handle, remote_home};
use crate::AppState;

// Phase 77 S5 renamed the daemon data dir ~/.ymux/insights → ~/.ymux/server
// (migrated in place on first 2.0 boot). The domain marker + token live there.
const DOMAIN_FILE: &str = ".ymux/server/mobile-domain";

/// Validate a device id coming back from the daemon before it lands in a URL
/// path (defence in depth — the daemon mints `dev_<hex>`).
fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `/api/v2/devices/{id}/scopes` — the one non-pairing route the desktop may
/// call (Phase 113, the owner's scope grants), for one valid device id.
fn is_device_scopes_path(path: &str) -> bool {
    path.strip_prefix("/api/v2/devices/")
        .and_then(|rest| rest.strip_suffix("/scopes"))
        .is_some_and(valid_device_id)
}

/// The scope list with `shell:attach` added or removed. `current` is the
/// daemon's GET body, `{"scopes":[...]}` — already expanded from "all", so no
/// copy of the Go scope vocabulary is needed here; the daemon's PUT
/// normalizes whatever comes back.
fn with_shell(current: &str, enabled: bool) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(current.trim()).map_err(|_| format!("unexpected scopes reply: {}", current.trim()))?;
    let mut scopes: Vec<String> = v
        .get("scopes")
        .and_then(|s| s.as_array())
        .ok_or_else(|| format!("unexpected scopes reply: {}", current.trim()))?
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .filter(|s| s != SHELL_ATTACH)
        .collect();
    if enabled {
        scopes.push(SHELL_ATTACH.to_string());
    }
    Ok(scopes)
}

const SHELL_ATTACH: &str = "shell:attach";

/// curl the daemon's pairing API over SSH using the admin (insights) token.
/// The request body, when present, is fed on stdin (never in the command
/// string) so user-supplied fields can't break out (Rule #3).
async fn daemon_curl(
    state: &State<'_, AppState>,
    workspace_id: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    if !(path.starts_with("/api/pairing/") || is_device_scopes_path(path))
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-?=&.".contains(&b))
    {
        return Err("invalid pairing path".into());
    }
    let handle = pick_handle(state, workspace_id).ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let token = format!("$(cat '{home}/.ymux/server/token' 2>/dev/null)");
    let base = format!(
        "curl -s --max-time 8 -X {method} -H \"Authorization: Bearer {token}\" "
    );
    if let Some(b) = body {
        let cmd = format!(
            "{base}-H 'Content-Type: application/json' --data-binary @- 'http://127.0.0.1:7879{path}'"
        );
        exec_stdin(&handle, &cmd, b.as_bytes(), 12).await
    } else {
        let cmd = format!("{base}'http://127.0.0.1:7879{path}'");
        exec(&handle, &cmd, 12).await
    }
}

/// Install nginx + Let's Encrypt cert (Cloudflare DNS) for `domain`, then
/// persist the domain remote-side so QR generation can read it back.
#[tauri::command]
pub(crate) async fn mobile_pairing_init(
    state: State<'_, AppState>,
    workspace_id: String,
    domain: String,
    mut cf_token: String,
) -> Result<String, String> {
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let result = nginx_proxy_install(&handle, &home, &domain, &cf_token).await;
    cf_token.zeroize(); // Rule #2 — drop the secret desktop-side immediately
    let status = result?;
    // Record the domain (validated by nginx_proxy_install) for QR generation.
    let _ = exec(
        &handle,
        &format!("printf %s '{domain}' > \"{home}/{DOMAIN_FILE}\""),
        8,
    )
    .await;
    Ok(json!({ "ok": true, "domain": domain, "status": status }).to_string())
}

/// Status for the Mobile tab's setup section: configured domain + nginx state.
#[tauri::command]
pub(crate) async fn mobile_pairing_status(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<String, String> {
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let domain = exec(&handle, &format!("cat \"{home}/{DOMAIN_FILE}\" 2>/dev/null || true"), 8)
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    let nginx = exec(&handle, "systemctl is-active nginx 2>/dev/null || true", 8)
        .await
        .unwrap_or_default();
    Ok(json!({
        "domain": domain,
        "nginx_active": nginx.trim() == "active",
        "configured": !domain.is_empty(),
    })
    .to_string())
}

/// Disconnect the mobile proxy: drop the persisted domain marker so the Mobile
/// tab returns to its setup form. nginx + the cert stay installed on the remote
/// (a re-install reconfigures them) — this just forgets the linked domain.
#[tauri::command]
pub(crate) async fn mobile_pairing_disconnect(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<String, String> {
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let _ = exec(&handle, &format!("rm -f \"{home}/{DOMAIN_FILE}\""), 8).await;
    Ok(json!({ "ok": true }).to_string())
}

/// Issue a one-shot pairing token and assemble the QR payload (§3.2:
/// WebPKI-trusted, no cert pinning).
#[tauri::command]
pub(crate) async fn mobile_pairing_generate_qr(
    state: State<'_, AppState>,
    workspace_id: String,
    device_name: String,
) -> Result<String, String> {
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let domain = exec(&handle, &format!("cat \"{home}/{DOMAIN_FILE}\" 2>/dev/null || true"), 8)
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    if domain.is_empty() {
        return Err("set up the Mobile proxy (domain + cert) first".into());
    }
    let body = json!({ "device_name": device_name }).to_string();
    let resp = daemon_curl(&state, &workspace_id, "POST", "/api/pairing/issue", Some(&body)).await?;
    let v: serde_json::Value =
        serde_json::from_str(resp.trim()).map_err(|e| format!("bad daemon response: {e}"))?;
    let device_id = v.get("device_id").and_then(|x| x.as_str()).unwrap_or_default();
    let token = v.get("one_shot_token").and_then(|x| x.as_str()).unwrap_or_default();
    let expires_at = v.get("expires_at").and_then(|x| x.as_i64()).unwrap_or(0);
    if device_id.is_empty() || token.is_empty() {
        return Err(format!("daemon did not issue a token: {}", resp.trim()));
    }
    Ok(json!({
        "version": 1,
        "host": domain,
        "port": 443,
        "tls": true,
        "device_id": device_id,
        "token": token,
        "expires_at": expires_at,
    })
    .to_string())
}

#[tauri::command]
pub(crate) async fn mobile_pairing_list_devices(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<String, String> {
    daemon_curl(&state, &workspace_id, "GET", "/api/pairing/devices", None).await
}

#[tauri::command]
pub(crate) async fn mobile_pairing_revoke(
    state: State<'_, AppState>,
    workspace_id: String,
    device_id: String,
) -> Result<String, String> {
    if !valid_device_id(&device_id) {
        return Err("invalid device id".into());
    }
    daemon_curl(
        &state,
        &workspace_id,
        "DELETE",
        &format!("/api/pairing/devices/{device_id}"),
        None,
    )
    .await
}

#[tauri::command]
pub(crate) async fn mobile_pairing_rename(
    state: State<'_, AppState>,
    workspace_id: String,
    device_id: String,
    name: String,
) -> Result<String, String> {
    if !valid_device_id(&device_id) {
        return Err("invalid device id".into());
    }
    let body = json!({ "name": name }).to_string();
    daemon_curl(
        &state,
        &workspace_id,
        "PUT",
        &format!("/api/pairing/devices/{device_id}/name"),
        Some(&body),
    )
    .await
}

/// Phase 113 (Web & devices): grant or withdraw a paired device's terminal
/// access (`shell:attach`). Read-modify-write of the owner scopes endpoint,
/// then checked against what the daemon stored.
#[tauri::command]
pub(crate) async fn mobile_pairing_set_shell(
    state: State<'_, AppState>,
    workspace_id: String,
    device_id: String,
    enabled: bool,
) -> Result<String, String> {
    if !valid_device_id(&device_id) {
        return Err("invalid device id".into());
    }
    let path = format!("/api/v2/devices/{device_id}/scopes");
    let current = daemon_curl(&state, &workspace_id, "GET", &path, None).await?;
    let scopes = with_shell(&current, enabled)?;
    let body = json!({ "scopes": scopes }).to_string();
    let out = daemon_curl(&state, &workspace_id, "PUT", &path, Some(&body)).await?;
    let stored = with_shell(&out, false).map(|_| out.contains(SHELL_ATTACH))?;
    if stored != enabled {
        return Err(format!("the daemon did not store the change: {}", out.trim()));
    }
    crate::log_info(
        "PAIRING",
        &format!("device {device_id} terminal access {}", if enabled { "granted" } else { "withdrawn" }),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{is_device_scopes_path, valid_device_id, with_shell};

    #[test]
    fn only_one_device_route_is_allowed() {
        assert!(is_device_scopes_path("/api/v2/devices/dev_ab12/scopes"));
        assert!(!is_device_scopes_path("/api/v2/devices//scopes"));
        assert!(!is_device_scopes_path("/api/v2/devices/a/b/scopes"));
        assert!(!is_device_scopes_path("/api/v2/devices/dev_ab12"));
        assert!(!is_device_scopes_path("/api/v2/term/sessions"));
    }

    #[test]
    fn shell_is_added_once_and_removed_cleanly() {
        let on = with_shell(r#"{"scopes":["files:read","insights:read"]}"#, true).unwrap();
        assert_eq!(on, vec!["files:read", "insights:read", "shell:attach"]);
        let again = with_shell(r#"{"scopes":["files:read","shell:attach"]}"#, true).unwrap();
        assert_eq!(again.iter().filter(|s| *s == "shell:attach").count(), 1);
        let off = with_shell(r#"{"scopes":["shell:attach","files:read"]}"#, false).unwrap();
        assert_eq!(off, vec!["files:read"]);
        assert!(with_shell("not json", true).is_err());
        assert!(with_shell(r#"{"nope":1}"#, true).is_err());
    }

    #[test]
    fn device_id_validation() {
        assert!(valid_device_id("dev_ab12cd"));
        assert!(!valid_device_id(""));
        assert!(!valid_device_id("dev/../../etc"));
        assert!(!valid_device_id("dev id"));
        assert!(!valid_device_id("dev;rm"));
    }
}
