//! Unshipped-fivefer (#2): system tray icon + quick menu + taskbar badge.
//!
//! The tray is best-effort — if it fails to build we log and carry on (the
//! app is fully usable without it). `TRAY_ACTIVE` gates the close-to-tray
//! behaviour so a failed tray never traps the user with no way to reopen the
//! window.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

/// True once the tray icon exists. Read by the window CloseRequested handler
/// to decide "hide to tray" vs "let it close".
pub(crate) static TRAY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Build the tray icon + menu. Call once from `setup()`. Best-effort: returns
/// an error the caller should log-and-ignore rather than propagate.
pub(crate) fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "tray_show", "Show YMUX", true, None::<&str>)?;
    let new_ws =
        MenuItem::with_id(app, "tray_new_workspace", "New workspace", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "tray_settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray_quit", "Quit YMUX", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &new_ws, &settings, &quit])?;

    let mut builder = TrayIconBuilder::with_id("ymux-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("YMUX")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray_show" => show_main(app),
            "tray_new_workspace" => {
                show_main(app);
                let _ = app.emit("tray:action", "new_workspace");
            }
            "tray_settings" => {
                show_main(app);
                let _ = app.emit("tray:action", "settings");
            }
            // Phase 80: RunEvent::Exit covers this too, but the queued
            // tail is cheap insurance on an explicit quit.
            "tray_quit" => {
                ymux_core::flush_log();
                app.exit(0)
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click restores/focuses the main window.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    TRAY_ACTIVE.store(true, Ordering::Relaxed);
    Ok(())
}

/// Show + unminimize + focus the main window (from tray click / menu).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// `app.focus` RPC: a second launch asks this instance to come forward.
/// The `is_visible` getter waits for the event loop, so a hung UI cannot
/// report success. Blocking: call it off the async runtime.
pub(crate) fn focus_main(app: &AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "no main window".to_string())?;
    w.show().map_err(|e| e.to_string())?;
    let _ = w.unminimize();
    w.set_focus().map_err(|e| e.to_string())?;
    w.is_visible().map(|_| ()).map_err(|e| e.to_string())
}

/// Set the taskbar badge to `count` (Windows overlay). `0` clears it. Called
/// by the frontend whenever the unread notification count changes.
#[tauri::command]
pub(crate) fn set_tray_badge(app: AppHandle, count: i64) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        let value = if count > 0 { Some(count) } else { None };
        w.set_badge_count(value)
            .map_err(|e| format!("set_badge_count: {e}"))?;
    }
    Ok(())
}
