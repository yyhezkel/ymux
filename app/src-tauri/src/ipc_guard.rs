//! 2026-10-06: label-based invoke guard for the workspace Browser webview.
//!
//! Tauri 2.10.3 ACL-checks app (non-plugin) commands only when an app ACL
//! manifest exists; ymux has none (bare `tauri_build::build()`), so app
//! commands reach every origin, including the tunneled page in the
//! `workspace-browser-*` child webview. Capabilities gate plugin commands
//! only. This guard denies ALL app commands to that webview by label, at
//! the one choke point (`invoke_handler`), independent of capabilities.
//!
//! Metadata only (Rule #1): webview label and command NAME, never arguments.

use std::collections::BTreeSet;
use std::sync::Mutex;

use ymux_core::log_warn;

/// Labels already warned about — one line per label, not per call.
static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Refusal message when `label` may not call `command`; `None` = allowed.
pub(crate) fn deny_reason(label: &str, command: &str) -> Option<String> {
    if label.starts_with(crate::workspace_browser::WEBVIEW_LABEL_PREFIX) {
        return Some(format!(
            "ymux: command {command} is not available to the workspace Browser webview"
        ));
    }
    None
}

/// True the first time `label` is seen.
fn first_denial(label: &str) -> bool {
    match WARNED.lock() {
        Ok(mut set) => set.insert(label.to_string()),
        Err(_) => false,
    }
}

/// Wrap the dispatcher so denied webviews never reach it. Outermost layer:
/// a denied call is not counted by the meter either.
pub(crate) fn guarded<F>(
    handler: F,
) -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static
where
    F: Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static,
{
    move |invoke| {
        let label = invoke.message.webview_ref().label().to_string();
        match deny_reason(&label, invoke.message.command()) {
            Some(msg) => {
                if first_denial(&label) {
                    log_warn(
                        "IPC",
                        &format!(
                            "denied app command `{}` from webview {label} (further denials silent)",
                            invoke.message.command()
                        ),
                    );
                }
                invoke.resolver.reject(msg);
                true
            }
            None => handler(invoke),
        }
    }
}

#[cfg(test)]
mod ipc_guard_tests {
    use super::deny_reason;

    // Pins the deny: breaking it re-exposes every app command to the tunneled page.
    #[test]
    fn rejects_workspace_browser_webview() {
        let msg = deny_reason("workspace-browser-w_1", "pty_write").expect("denied");
        assert!(msg.contains("pty_write"));
        assert!(deny_reason("workspace-browser-", "x").is_some());
    }

    // Pins no over-blocking: first-party windows (and look-alike prefixes) must keep working.
    #[test]
    fn allows_trusted_webviews() {
        for label in ["main", "popout-w_1", "browser-popout-w_1", "my-workspace-browser-w_1", ""] {
            assert!(deny_reason(label, "pty_write").is_none(), "{label}");
        }
    }

    // Pins that no capability file grants a remote context or covers the child webview label.
    #[test]
    fn no_capability_grants_remote_context() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut found = 0;
        for entry in std::fs::read_dir(&dir).expect("capabilities dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            found += 1;
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
            assert!(v.get("remote").is_none(), "{path:?} grants a remote context");
            let windows = v["windows"].as_array().expect("windows array");
            for w in windows.iter().filter_map(|w| w.as_str()) {
                let pat = w.trim_end_matches('*');
                assert!(
                    !"workspace-browser-w_1".starts_with(pat),
                    "{path:?} window glob {w} matches the workspace Browser webview"
                );
            }
        }
        assert!(found >= 1, "no capability json found");
    }
}
