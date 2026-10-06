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
