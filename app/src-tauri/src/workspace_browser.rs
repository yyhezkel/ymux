//! Phase 53 (rebased): workspace-level Browser singleton.
//!
//! Replaces the Phase 53.A per-pane Browser surface. Each workspace
//! owns AT MOST ONE child Webview attached to the main window. When
//! the user opens the floating Browser window for workspace `w_X`, the
//! frontend calls `workspace_browser_show(w_X, url, x, y, w, h)`:
//!
//! - If no Webview exists for `w_X` yet, we spawn one via
//!   `Window::add_child(WebviewBuilder, ...)`. Phase 62.A (item D):
//!   all browser webviews share the process-DEFAULT WebView2
//!   environment (same as the main window). We do NOT pass a
//!   per-workspace `--user-data-dir` — that forced a SEPARATE WebView2
//!   environment per workspace, and WebView2 does not support multiple
//!   environments in one process: the conflict surfaced as
//!   intermittent 0x8007139F (ERROR_INVALID_STATE) on creation. The
//!   supported shape is one environment with many webviews. Trade-off:
//!   workspaces share browser cookies / cache (acceptable — the
//!   browser only views tunneled localhost services).
//! - If one already exists, we reposition/resize it and call
//!   `.show()` (the floating-window pattern can hide a workspace's
//!   browser when the user closes the floating panel, then bring it
//!   back when they reopen it without losing the page state).
//!
//! Z-order: native Webview always paints above HTML, so any modal
//! opening in the SolidJS layer broadcasts `workspace_browser_hide`
//! for the active workspace and `workspace_browser_show` again on
//! close.
//!
//! Pinned tauri =2.10.3 with `features = ["unstable"]` — the
//! `Window::add_child(WebviewBuilder, ...)` API still lives behind
//! the unstable gate in this version. See CLAUDE.md "Pinned deps".

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tauri::webview::{PageLoadEvent, WebviewBuilder};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, Url, Webview, WebviewUrl,
};

use crate::{config_dir, log_debug, log_info, log_warn, AppState};

/// Per-workspace WKWebsiteDataStore id (Phase 62.A follow-up): macOS >= 14
/// otherwise puts every workspace Browser in the one default store, so a
/// delete could not clear one workspace's login state. Derived, not
/// stored, so show and delete agree without any persistence.
pub(crate) fn workspace_store_id(workspace_id: &str) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("ymux-browser-store:{workspace_id}").as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// URL scheme the Dev-Mode inspect script uses to hand a captured
/// element back to the app. See `install_ticket_bridge`.
const TICKET_SCHEME: &str = "ymux-ticket";

/// Event the main window listens on to open the ticket modal.
const TICKET_EVENT: &str = "browser:ticket-captured";

// ---------------------------------------------------------------------
// Phase 82.E: the macOS "site JS does not run" probe.
//
// On macOS a page renders in this webview but the site's own JavaScript
// is dead; the identical code path works on Windows. Rule #17 means we
// cannot build or attach a debugger locally, so the diagnosis ships in
// the binary and reports through the unified logger.
//
// The probe (`browser_diag.js`) reports by setting `document.title` to
// `ymux-diag:<base64 json>`. That is the ONE channel that behaves the
// same on both platforms: wry hooks `add_DocumentTitleChanged` on
// Windows and a KVO observer on the `title` keypath on macOS. It also
// needs no navigation, so it cannot perturb the page load we are
// measuring. See the header comment in `browser_diag.js` for why
// `fetch`, an iframe, and `location.href` were all rejected.
//
// Deliberately NOT `cfg`-gated to macOS: if the probe first executed on
// the one machine we cannot debug, "no beacon in the log" would be
// ambiguous between "the JS engine is dead" and "my probe is broken".
// Running it on Windows first makes that build the control experiment.
// ---------------------------------------------------------------------

/// Title prefix the probe uses to mark a beacon.
const DIAG_TITLE_PREFIX: &str = "ymux-diag:";

/// Log tag for everything this probe emits.
const DIAG_TAG: &str = "BROWSERDIAG";

/// The document-start probe itself.
const DIAG_SCRIPT: &str = include_str!("browser_diag.js");

/// Hard cap on one logged beacon. The page controls its own title.
const DIAG_MAX_LOG: usize = 900;

/// Hard cap on beacons logged per webview. The page controls its own
/// title, so this channel is attacker-influenced: without a budget a
/// hostile or merely chatty page could flood `debug.log`.
const DIAG_MAX_BEACONS: usize = 24;

/// How long to wait for the first beacon before saying so out loud.
const DIAG_WATCHDOG_SECS: u64 = 8;

/// Independent probe pushed from Rust after `PageLoadEvent::Finished`.
///
/// This is the line that makes "the JS engine is dead" falsifiable:
/// `eval` is `evaluateJavaScript` on macOS, a DIFFERENT injection
/// mechanism from the `WKUserScript` that carries `browser_diag.js`.
/// `us` reports whether the document-start script left its global
/// behind, so one beacon separates "engine dead" (nothing arrives at
/// all) from "user-script injection broken" (this arrives, `us` is 0).
const DIAG_EVAL_JS: &str = concat!(
    "try{var d=window.__ymuxDiag?1:0;var o=document.title;",
    "document.title='ymux-diag:'+btoa('{\"p\":\"eval\",\"ok\":1,\"us\":'+d+'}');",
    "setTimeout(function(){if(String(document.title).indexOf('ymux-diag:')===0)",
    "{document.title=o}},0)}catch(e){}"
);

/// Payload delivered to the frontend for one captured element.
#[derive(Clone, serde::Serialize)]
struct TicketCapture {
    workspace_id: String,
    /// The raw object the inspect script built (xpath / selector /
    /// html / style / url). Round-tripped as-is — `tickets_create`
    /// is what gives it a schema.
    capture: serde_json::Value,
}

/// The child webview is a plain external page with no IPC surface, and
/// `WebviewBuilder` has no `on_ipc` in 2.10 anyway. So the Dev-Mode
/// inspect script talks back through the one hook that does exist —
/// navigation.
///
/// Correction (Phase 82.E): the injection is not what protects us.
/// `tauri::manager::webview` prepends `__TAURI_INTERNALS__` and the
/// invoke bootstrap to EVERY webview's init scripts, external URLs
/// included — so an `invoke` function does reach the tunneled page. What
/// denies it is the capability layer: a remote page's origin is
/// `Origin::Remote`, and every capability in `capabilities/` declares a
/// `Local` execution context, so `Origin::matches` fails and every
/// command is refused. Note `capabilities/default.json` is scoped to
/// `windows: ["main"]` and this webview lives in the `main` window, so
/// adding a `remote` context there would expose it. Don't.
///
/// The script sets `location.href = "ymux-ticket:<base64url>"`. This
/// handler decodes it, emits `browser:ticket-captured`, and returns
/// `false` so the navigation itself never happens. **Every other URL
/// returns `true`** — normal browsing, the port+path model, tabs and
/// `workspace_browser_navigate` all take the exact same code path they
/// did before this existed.
///
/// Note the scheme is used WITHOUT `//`: `ymux-ticket:<data>` parses
/// as a cannot-be-a-base URL, so the payload lands in `path()` verbatim.
/// The `//` form would put it in the *authority* slot instead —
/// `host_str()` gets the data and `path()` comes back empty — and routes
/// it through host normalization. Case happens to survive there (this is
/// a non-special scheme), but the payload is data, not an authority, so
/// the opaque form is the correct shape. See `ticket_bridge_tests`.
fn handle_ticket_navigation(app: &AppHandle, workspace_id: &str, url: &Url) -> bool {
    if url.scheme() != TICKET_SCHEME {
        return true; // not ours — let the webview navigate normally
    }
    // `ymux-ticket:AAAA` → path() is "AAAA" (cannot-be-a-base URL).
    let encoded = url.path();
    match decode_capture(encoded) {
        Ok(capture) => {
            let _ = app.emit(
                TICKET_EVENT,
                TicketCapture {
                    workspace_id: workspace_id.to_string(),
                    capture,
                },
            );
            // Rule #1: the capture holds page markup. Length only.
            log_info(
                "TICKETS",
                &format!(
                    "capture from browser ws={} payload_len={}",
                    workspace_id,
                    encoded.len()
                ),
            );
        }
        Err(e) => log_warn(
            "TICKETS",
            &format!("bad capture payload ws={workspace_id}: {e}"),
        ),
    }
    false // never actually navigate to the sentinel
}

fn decode_capture(encoded: &str) -> Result<serde_json::Value, String> {
    let bytes = crate::tickets::base64_decode(encoded)?;
    let text = String::from_utf8(bytes).map_err(|e| format!("capture is not utf-8: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("capture is not json: {e}"))
}

/// Handle one `document.title` change from the Browser webview.
///
/// Returns `true` if the title was one of our beacons — the caller uses
/// that to arm the watchdog, so "the log is empty" can never be confused
/// with "the user never opened a page".
///
/// Every other title is an ordinary page title and is ignored, not
/// logged: Rule #1's spirit is that page content stays out of the log.
fn handle_diag_title(workspace_id: &str, title: &str, budget: &AtomicUsize) -> bool {
    let encoded = match title.strip_prefix(DIAG_TITLE_PREFIX) {
        Some(e) => e,
        None => return false,
    };
    // Spend one unit of budget, or stay silent if it is gone.
    let remaining = match budget.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        if n == 0 {
            None
        } else {
            Some(n - 1)
        }
    }) {
        Ok(prev) => prev - 1,
        Err(_) => return true,
    };
    match decode_diag(encoded) {
        Ok(json) => log_warn(DIAG_TAG, &format!("ws={workspace_id} {json}")),
        Err(e) => log_warn(DIAG_TAG, &format!("ws={workspace_id} bad beacon: {e}")),
    }
    if remaining == 0 {
        log_warn(
            DIAG_TAG,
            &format!("ws={workspace_id} beacon budget spent — no further beacons will be logged"),
        );
    }
    true
}

/// Decode a beacon payload into exactly one log-safe line.
///
/// Re-serializes through `serde_json` rather than logging the page's
/// bytes verbatim: raw newlines are legal JSON whitespace, so a page
/// could otherwise forge extra lines in `debug.log`.
fn decode_diag(encoded: &str) -> Result<String, String> {
    let bytes = crate::tickets::base64_decode(encoded)?;
    let text = String::from_utf8(bytes).map_err(|e| format!("beacon is not utf-8: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("beacon is not json: {e}"))?;
    let mut compact =
        serde_json::to_string(&value).map_err(|e| format!("beacon re-encode failed: {e}"))?;
    if compact.len() > DIAG_MAX_LOG {
        let mut cut = DIAG_MAX_LOG;
        while cut > 0 && !compact.is_char_boundary(cut) {
            cut -= 1;
        }
        compact.truncate(cut);
        compact.push_str("…(truncated)");
    }
    Ok(compact)
}

/// Map of `workspace_id -> Webview`. Exactly one entry per workspace
/// that has opened its Browser at least once this session. Cleared
/// by `workspace_browser_close` (user closed the floating window
/// explicitly) and by `cleanup_workspace_sessions` (user deleted the
/// workspace).
pub(crate) type WorkspaceBrowserMap = Arc<Mutex<HashMap<String, Webview>>>;

fn webview_label(workspace_id: &str) -> String {
    // Tauri webview labels are constrained to [a-zA-Z-/:_].
    // workspace_id is `w_<hex>` which is alnum+underscore — safe.
    format!("workspace-browser-{workspace_id}")
}

/// Phase 85.C: the OS window a popped-out Browser lives in.
///
/// `index.tsx` routes on this prefix, so it must NOT collide with the
/// terminal popout's `popout-` (glob matching is prefix-anchored, and
/// `browser-popout-x` does not start with `popout-`). `capabilities/
/// browser-popout.json` scopes IPC to `browser-popout-*`.
pub(crate) fn popout_window_label(workspace_id: &str) -> String {
    format!("browser-popout-{workspace_id}")
}

/// Workspace IDs are `w_<hex>` so this is defence-in-depth — any
/// `..`, `/`, `\` would already be rejected by the ID format.
fn sanitize_for_path(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Spawn (if absent) + reposition + show. Frontend calls this every
/// time the floating Browser window mounts or its rect changes.
///
/// `url` is only consulted when we actually spawn a new Webview —
/// re-shows leave the existing Webview's URL alone (Browser window
/// preserves page state across hide/show cycles).
///
/// Phase 85.C: `window_label` names the OS window to attach the child to
/// — `"main"` (the default) for the in-app floating panel, or
/// `browser-popout-<ws>` when the Browser has been popped out. It is only
/// read on the SLOW path: an existing Webview cannot be re-parented, so
/// `browser_popout_open` destroys and respawns rather than moving one.
#[tauri::command]
pub(crate) async fn workspace_browser_show(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    url: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    window_label: Option<String>,
) -> Result<(), String> {
    // Fast path: the Webview already exists. Reposition + .show().
    {
        let map = state.workspace_browsers.lock().unwrap();
        if let Some(webview) = map.get(&workspace_id).cloned() {
            drop(map);
            webview
                .set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("set_position: {e}"))?;
            webview
                .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
                .map_err(|e| format!("set_size: {e}"))?;
            webview.show().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    // Slow path: spawn a new child Webview. Phase 62.A (item D):
    // serialize creation across ALL workspaces — WebView2 dislikes
    // concurrent environment creation and returns 0x8007139F
    // (ERROR_INVALID_STATE). The guard is held across the whole creation
    // (including the retry backoff) so two rapid opens can't race.
    let _create_guard = state.browser_create_lock.lock().await;

    // Re-check under the creation lock: another call may have created
    // the webview while we were waiting on the lock. Without this, two
    // concurrent show() calls for the same workspace would both miss the
    // fast path above and both call add_child → a duplicate-label /
    // same-user-data-dir failure (a second ERROR_INVALID_STATE source).
    {
        let map = state.workspace_browsers.lock().unwrap();
        if let Some(webview) = map.get(&workspace_id).cloned() {
            drop(map);
            webview
                .set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("set_position: {e}"))?;
            webview
                .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
                .map_err(|e| format!("set_size: {e}"))?;
            webview.show().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }

    // Phase 85.C: the host window. `main` unless the Browser is popped
    // out, in which case the child attaches to the popout's own window.
    let host_label = window_label.as_deref().unwrap_or("main");
    let host_window = app
        .get_window(host_label)
        .ok_or_else(|| format!("host window {host_label:?} not found"))?;
    let parsed_url: Url = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let label = webview_label(&workspace_id);

    // Retry the transient WebView2 ERROR_INVALID_STATE a couple of times
    // with a short backoff (the builder is consumed by add_child, so we
    // rebuild it each attempt). A clean failure is surfaced to the FE
    // only after all attempts are exhausted. NOTE: the real fix for the
    // 0x8007139F Yossi hit is using the DEFAULT WebView2 environment (no
    // per-workspace --user-data-dir) — see the module doc. The retry /
    // creation lock stay as defense-in-depth for genuine transients.
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    let mut created = None;
    // Phase 82.E probe state, shared by the title handler and the
    // watchdog. Built outside the retry loop; the closures that capture
    // it are rebuilt per attempt because `add_child` consumes the builder.
    let diag_seen = Arc::new(AtomicBool::new(false));
    let diag_budget = Arc::new(AtomicUsize::new(DIAG_MAX_BEACONS));
    for attempt in 1..=MAX_ATTEMPTS {
        // Default environment — shared with the main window + every other
        // workspace browser. One WebView2 environment per process.
        // The ticket bridge is inert for every URL that isn't
        // `ymux-ticket:` (returns true = navigate as before). Phase 82.E
        // added the diagnostic probe below it — that one IS injected into
        // the page, at document start; see the const block at the top.
        let bridge_app = app.clone();
        let bridge_ws = workspace_id.clone();
        let title_ws = workspace_id.clone();
        let title_seen = diag_seen.clone();
        let title_budget = diag_budget.clone();
        let load_ws = workspace_id.clone();
        let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed_url.clone()))
            .on_navigation(move |url| handle_ticket_navigation(&bridge_app, &bridge_ws, url))
            // Own data store per workspace; a no-op off macOS >= 14, so
            // Windows keeps its shared WebView2 profile (never set a
            // data directory there — 0x8007139F).
            .data_store_identifier(workspace_store_id(&workspace_id))
            // ----- Phase 82.E diagnostics (see the const block above) -----
            .initialization_script(DIAG_SCRIPT)
            // Both platforms, on purpose (Yossi's call): the Windows
            // build is the control run for the macOS bug, and a control
            // you cannot inspect is worth much less. Without the
            // `devtools` feature wry never calls `setInspectable(true)`
            // on macOS, so Safari's Develop menu cannot attach at all.
            // This webview shows a tunneled third-party service, not
            // ymux's own UI, so an inspector here exposes nothing of
            // ours — the main and popout windows are explicitly opted
            // OUT in lib.rs, because enabling the feature flips the
            // runtime default for every webview to `true`.
            .devtools(true)
            // Handlers run on the WebView2 UI thread / the macOS KVO
            // thread. They may only decode and log — `log_*` just pushes
            // onto a channel. Anything that re-enters the webview (an
            // `eval`) goes through `async_runtime::spawn` instead.
            .on_document_title_changed(move |_wv, title| {
                if handle_diag_title(&title_ws, &title, &title_budget) {
                    title_seen.store(true, Ordering::Relaxed);
                }
            })
            .on_page_load(move |wv, payload| {
                // Fires from the navigation delegate with zero JS
                // involved — proof the page committed even when nothing
                // else reports in.
                log_warn(
                    DIAG_TAG,
                    &format!(
                        "ws={} page_load={:?} scheme={} host={:?}",
                        load_ws,
                        payload.event(),
                        payload.url().scheme(),
                        payload.url().host_str()
                    ),
                );
                if payload.event() == PageLoadEvent::Finished {
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        let _ = wv.eval(DIAG_EVAL_JS);
                    });
                }
            });
        match host_window.add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(w.max(1.0), h.max(1.0)),
        ) {
            Ok(wv) => {
                log_debug("BROWSER", &format!(
                    "[workspace_browser_show] add_child ws={} ok (attempt {}/{})",
                    workspace_id, attempt, MAX_ATTEMPTS
                ));
                created = Some(wv);
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                log_warn("BROWSER", &format!(
                    "[workspace_browser_show] add_child ws={} attempt {}/{} FAILED: {}",
                    workspace_id, attempt, MAX_ATTEMPTS, last_err
                ));
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
            }
        }
    }
    let webview =
        created.ok_or_else(|| format!("add_child failed after {MAX_ATTEMPTS} attempts: {last_err}"))?;

    state
        .workspace_browsers
        .lock()
        .unwrap()
        .insert(workspace_id.clone(), webview);

    log_info("BROWSER", &format!(
        "[workspace_browser_show] spawned ws={} url={} rect=({:.0},{:.0},{:.0},{:.0})",
        workspace_id, url, x, y, w, h
    ));

    // Phase 82.E watchdog: turn silence into an affirmative log line.
    // Without it, an empty log is ambiguous between "the probe never
    // ran" and "the user never actually opened a page".
    {
        let ws = workspace_id.clone();
        let seen = diag_seen.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(DIAG_WATCHDOG_SECS)).await;
            if !seen.load(Ordering::Relaxed) {
                log_warn(DIAG_TAG, &format!(
                    "ws={ws} NO BEACON within {DIAG_WATCHDOG_SECS}s — the document-start probe never reported (H1 candidate: JS is dead in the child webview)"
                ));
            }
        });
    }
    Ok(())
}

/// Hide the workspace's Browser Webview if one exists. No-op if not
/// (modal effect may broadcast hide for every workspace; spurious
/// hides for never-opened workspaces are silent).
#[tauri::command]
pub(crate) async fn workspace_browser_hide(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned();
    if let Some(w) = webview {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_navigate(
    state: State<'_, AppState>,
    workspace_id: String,
    url: String,
) -> Result<(), String> {
    let parsed: Url = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    // Phase 62.C (F.1): log the destination so a "browser isn't reaching
    // the service" report can be checked against the actual URL (should
    // be http://127.0.0.1:<port>, never localhost / an external IP).
    log_info("BROWSER", &format!(
        "[workspace_browser_navigate] ws={} url={}",
        workspace_id, url
    ));
    webview.navigate(parsed).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_eval(
    state: State<'_, AppState>,
    workspace_id: String,
    js: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    webview.eval(js).map_err(|e| e.to_string())?;
    Ok(())
}

/// Open the inspector on the workspace's Browser webview.
///
/// The one-click alternative to Safari → Develop → <machine> → <webview>,
/// and the only way in at all on Windows, where WebView2's own F12 is not
/// wired up for a child webview. Only this webview is inspectable — see
/// the `.devtools(...)` call in `workspace_browser_show`.
#[tauri::command]
pub(crate) async fn workspace_browser_open_devtools(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    // Called unconditionally, on purpose. `Webview::open_devtools` is
    // gated on the TAURI crate's `any(debug_assertions, feature =
    // "devtools")`, and that feature is on for every build — see
    // Cargo.toml:52. If anyone ever drops it from the dependency's
    // feature list, this line stops compiling, which is exactly the
    // failure we want: loud, at build time.
    //
    // Phase 85.A: this used to be wrapped in the same
    // `#[cfg(any(debug_assertions, feature = "devtools"))]`, which reads
    // as if it mirrors the tauri gate but does NOT — inside this crate
    // `feature = "devtools"` resolves against `app`'s own feature set,
    // and `app` declares no features at all (Cargo.toml has no
    // `[features]` section). So in a release build, where
    // `debug_assertions` is off, the whole condition was false and the
    // paired `#[cfg(not(...))]` arm compiled instead: the button
    // returned `Err("this build was compiled without devtools support")`
    // 100% of the time, in every shipped build, and the frontend logged
    // it to a file nobody reads. Right-click → Inspect kept working the
    // whole time because `.devtools(true)` in `workspace_browser_show`
    // is not gated.
    webview.open_devtools();
    log_info(
        "BROWSER",
        &format!("[workspace_browser_open_devtools] ws={workspace_id}"),
    );
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_close(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .remove(&workspace_id);
    if let Some(w) = webview {
        let _ = w.close();
        log_debug("BROWSER", &format!(
            "[workspace_browser_close] dropped webview ws={}",
            workspace_id
        ));
    }
    Ok(())
}

/// Phase 85.C: pop the workspace Browser out into its own OS window.
///
/// **The child Webview cannot be re-parented.** `Window::add_child` binds
/// it to the host window for life, so "popping out" is destroy-and-
/// respawn: we drop the child under `main` here, and the popout's own
/// frontend calls `workspace_browser_show` with its label to spawn a
/// fresh one under the new window. The page therefore RELOADS. That is
/// inherent to the chosen shape, not an oversight — the alternative
/// (keeping the child under `main` and faking a window) is not a
/// separate OS window at all.
///
/// The window loads ymux's own UI, so it gets the full chrome — tabs,
/// port bar, Go, the Web Inspector button — via the `browser-popout-`
/// branch in `index.tsx`, which reads the workspace id from the LABEL.
// IMPORTANT: `async`, for the same reason as `popout_pane` — on Windows
// `WebviewWindowBuilder` deadlocks when driven from a SYNCHRONOUS
// command: the window shell appears but the webview never initializes
// (blank white).
#[tauri::command]
pub(crate) async fn browser_popout_open(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    title: String,
) -> Result<(), String> {
    let label = popout_window_label(&workspace_id);

    // Already popped out? Focus it rather than erroring on the duplicate
    // label.
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }

    // Drop the child under `main` first — see the doc comment. Doing it
    // before the window exists means that if the build below fails, the
    // frontend's show effect simply respawns it under `main` on the next
    // tick; we never end up with two.
    {
        let webview = state
            .workspace_browsers
            .lock()
            .unwrap()
            .remove(&workspace_id);
        if let Some(w) = webview {
            let _ = w.close();
            log_debug(
                "BROWSER",
                &format!("[browser_popout_open] dropped main-hosted webview ws={workspace_id}"),
            );
        }
    }

    // Restore the remembered rect. `popout_display` is advisory: if that
    // monitor is gone we simply center on the current one rather than
    // placing the window off-screen where the user cannot reach it.
    let (saved_rect, saved_display) = crate::settings::browser_popout_geometry(&state);
    let monitor_count = app
        .available_monitors()
        .map(|m| m.len() as i32)
        .unwrap_or(1);
    let display_ok = saved_display.map(|d| d >= 0 && d < monitor_count).unwrap_or(true);
    let rect = saved_rect.filter(|_| display_ok);
    if saved_rect.is_some() && !display_ok {
        log_info(
            "BROWSER",
            &format!(
                "[browser_popout_open] saved display {:?} is gone ({monitor_count} monitors) — centering",
                saved_display
            ),
        );
    }

    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    let mut built = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let mut builder =
            tauri::WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
                .title(title.clone())
                // Rule #1 + the Cargo.toml:44-51 warning: the `devtools`
                // feature flips wry's runtime default to `true` for EVERY
                // webview, and this one is ymux's own UI. The explicit
                // opt-out is what keeps it uninspectable. The CHILD
                // webview inside it keeps `.devtools(true)` — that one
                // shows a tunneled third-party page, nothing of ours.
                .devtools(false);
        builder = match rect {
            Some(r) => builder
                .inner_size(f64::from(r.width.max(320)), f64::from(r.height.max(240)))
                .position(f64::from(r.x), f64::from(r.y)),
            None => builder.inner_size(1000.0, 720.0).center(),
        };
        log_debug(
            "BROWSER",
            &format!(
                "[browser_popout_open] building {label} (attempt {attempt}/{MAX_ATTEMPTS})"
            ),
        );
        match builder.build() {
            Ok(w) => {
                built = Some(w);
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                log_warn(
                    "BROWSER",
                    &format!(
                        "[browser_popout_open] attempt {attempt}/{MAX_ATTEMPTS} FAILED: {last_err}"
                    ),
                );
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
            }
        }
    }
    let win = built.ok_or_else(|| {
        // Leave the mode alone on failure — the frontend keeps the
        // in-app panel and respawns the child under `main`.
        format!("browser popout window failed after {MAX_ATTEMPTS} attempts: {last_err}")
    })?;
    log_info("BROWSER", &format!("[browser_popout_open] {label} built ok"));

    crate::settings::set_browser_popout(&state, &app, true, None, None);

    // `Destroyed`, not `CloseRequested` — matches `popout_pane`, and it
    // is the only event that also covers a programmatic close (workspace
    // deletion). Saves the rect, clears the mode, and drops the child
    // Webview: it belongs to a window that no longer exists, and leaving
    // it in the map would make the next `workspace_browser_show` take
    // the fast path onto a dead handle.
    let app2 = app.clone();
    let ws2 = workspace_id.clone();
    let win2 = win.clone();
    win.on_window_event(move |e| {
        if !matches!(e, tauri::WindowEvent::Destroyed) {
            return;
        }
        // Saved in LOGICAL pixels, because that is what the builder's
        // `.inner_size()` / `.position()` take. `outer_position` and
        // `inner_size` report PHYSICAL, so on any HiDPI display a
        // straight round-trip would rescale the window by the scale
        // factor on every open/close cycle.
        let scale = win2.scale_factor().unwrap_or(1.0);
        let rect = win2
            .outer_position()
            .ok()
            .zip(win2.inner_size().ok())
            .map(|(pos, size)| {
                let pos = pos.to_logical::<f64>(scale);
                let size = size.to_logical::<f64>(scale);
                crate::settings::Rect {
                    x: pos.x.round() as i32,
                    y: pos.y.round() as i32,
                    width: size.width.round().max(1.0) as u32,
                    height: size.height.round().max(1.0) as u32,
                }
            });
        if let Some(state) = app2.try_state::<AppState>() {
            {
                let webview = state.workspace_browsers.lock().unwrap().remove(&ws2);
                if let Some(w) = webview {
                    let _ = w.close();
                }
            }
            crate::settings::set_browser_popout(&state, &app2, false, rect, None);
        }
        let _ = app2.emit("browser-popout:closed", ws2.clone());
        log_info("BROWSER", &format!("[browser_popout] closed ws={ws2}"));
    });

    Ok(())
}

/// Close a popped-out Browser window from the app side (workspace
/// deletion). The `Destroyed` handler above does the cleanup.
pub(crate) fn close_popout_window(app: &AppHandle, workspace_id: &str) {
    if let Some(w) = app.get_webview_window(&popout_window_label(workspace_id)) {
        let _ = w.close();
    }
}

#[tauri::command]
pub(crate) async fn workspace_browser_resize(
    state: State<'_, AppState>,
    workspace_id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    webview
        .set_position(LogicalPosition::new(x, y))
        .map_err(|e| format!("set_position: {e}"))?;
    webview
        .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
        .map_err(|e| format!("set_size: {e}"))?;
    Ok(())
}

/// `workspace_delete` hooks here to wipe the workspace's Browser state
/// (the user explicitly deleted it — they don't want logins surviving).
/// Removes the legacy `browser-sessions/<ws>` dir synchronously, then
/// spawns the real clear: a tunnel-host cookie sweep (Windows shares one
/// WebView2 profile, so this also drops other workspaces' 127.0.0.1
/// cookies) and, on macOS >= 14, removal of the per-workspace data store.
/// Returns immediately; every failure is logged, never raised.
pub(crate) fn cleanup_workspace_sessions(app: &AppHandle, workspace_id: &str) {
    remove_legacy_sessions_dir(workspace_id);
    let app = app.clone();
    let ws = workspace_id.to_string();
    tauri::async_runtime::spawn(async move {
        let sweep_app = app.clone();
        let sweep_ws = ws.clone();
        // `cookies()` deadlocks on Windows from a sync command / event
        // handler, so it only ever runs on a blocking thread.
        match tauri::async_runtime::spawn_blocking(move || {
            sweep_tunnel_cookies(&sweep_app, &sweep_ws)
        })
        .await
        {
            Ok(()) => {}
            Err(e) => log_warn("BROWSER", &format!(
                "[workspace_browser] cookie sweep join FAILED ws={ws}: {e}"
            )),
        }
        #[cfg(target_os = "macos")]
        remove_workspace_data_store(&app, &ws).await;
    });
}

fn remove_legacy_sessions_dir(workspace_id: &str) {
    let Ok(base) = config_dir() else {
        return;
    };
    let dir = base
        .join("browser-sessions")
        .join(sanitize_for_path(workspace_id));
    if !dir.exists() {
        return;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => log_info("BROWSER", &format!(
            "[workspace_browser] cleaned sessions dir for ws={} at {}",
            workspace_id,
            dir.display()
        )),
        Err(e) => log_warn("BROWSER", &format!(
            "[workspace_browser] FAILED to clean sessions dir {}: {}",
            dir.display(),
            e
        )),
    }
}

/// Browser only ever loads `http://127.0.0.1:<port>` tunnels, so those
/// hosts are the only cookies a workspace Browser can have set.
fn is_tunnel_cookie_domain(domain: Option<&str>) -> bool {
    let Some(d) = domain else {
        return false;
    };
    let d = d.trim_start_matches('.');
    d.eq_ignore_ascii_case("127.0.0.1") || d.eq_ignore_ascii_case("localhost")
}

/// Blocking: deletes tunnel-host cookies via the `main` webview (shares
/// the default WebView2 environment with the Browser webviews). Logs
/// counts only — never cookie names or values.
fn sweep_tunnel_cookies(app: &AppHandle, workspace_id: &str) {
    let Some(main) = app.get_webview("main") else {
        log_warn("BROWSER", &format!(
            "[workspace_browser] cookie sweep FAILED ws={workspace_id}: no main webview"
        ));
        return;
    };
    let cookies = match main.cookies() {
        Ok(c) => c,
        Err(e) => {
            log_warn("BROWSER", &format!(
                "[workspace_browser] cookies() FAILED ws={workspace_id}: {e}"
            ));
            return;
        }
    };
    let (mut deleted, mut failed) = (0usize, 0usize);
    for c in cookies.into_iter().filter(|c| is_tunnel_cookie_domain(c.domain())) {
        match main.delete_cookie(c) {
            Ok(()) => deleted += 1,
            Err(_) => failed += 1,
        }
    }
    if failed > 0 {
        log_warn("BROWSER", &format!(
            "[workspace_browser] delete_cookie FAILED ws={workspace_id}: {failed} of {}",
            deleted + failed
        ));
    }
    log_info("BROWSER", &format!(
        "[workspace_browser] cookie sweep ws={workspace_id} deleted={deleted}"
    ));
}

/// Major version from `sw_vers -productVersion` output ("14.5\n").
#[cfg(any(test, target_os = "macos"))]
fn macos_major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// macOS >= 14 only (older systems ignore `data_store_identifier`, so
/// there is no per-workspace store to remove). The store reports
/// `DataStoreInUse` until the closed webview releases it, hence retries.
#[cfg(target_os = "macos")]
async fn remove_workspace_data_store(app: &AppHandle, workspace_id: &str) {
    let out = match std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log_warn("BROWSER", &format!(
                "[workspace_browser] sw_vers FAILED ws={workspace_id}: {e}"
            ));
            return;
        }
    };
    match macos_major(&String::from_utf8_lossy(&out.stdout)) {
        Some(m) if m >= 14 => {}
        _ => return,
    }
    let id = workspace_store_id(workspace_id);
    let mut last_err = String::new();
    for _ in 0..5 {
        match app.remove_data_store(id).await {
            Ok(()) => {
                log_info("BROWSER", &format!(
                    "[workspace_browser] removed data store ws={workspace_id}"
                ));
                return;
            }
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    log_warn("BROWSER", &format!(
        "[workspace_browser] remove_data_store FAILED ws={workspace_id}: {last_err}"
    ));
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    /// Tunnel hosts are what the sweep deletes; breaking this leaves logins behind.
    #[test]
    fn tunnel_domains_match() {
        assert!(is_tunnel_cookie_domain(Some("127.0.0.1")));
        assert!(is_tunnel_cookie_domain(Some(".LocalHost")));
    }

    /// Must not delete unrelated cookies (main window / other hosts).
    #[test]
    fn other_domains_do_not_match() {
        assert!(!is_tunnel_cookie_domain(None));
        assert!(!is_tunnel_cookie_domain(Some("")));
        assert!(!is_tunnel_cookie_domain(Some("example.com")));
        assert!(!is_tunnel_cookie_domain(Some("127.0.0.10")));
    }

    /// The >= 14 gate depends on this parse; junk must be None, not 0.
    #[test]
    fn macos_major_parses() {
        assert_eq!(macos_major("14.5\n"), Some(14));
        assert_eq!(macos_major("13"), Some(13));
        assert_eq!(macos_major(""), None);
        assert_eq!(macos_major("abc"), None);
    }
}

#[cfg(test)]
mod store_id_tests {
    use super::*;

    /// Show and delete both derive the id; if it drifted between calls the
    /// delete would target a different store and leave the login behind.
    #[test]
    fn store_id_is_stable() {
        assert_eq!(workspace_store_id("ws-1"), workspace_store_id("ws-1"));
    }

    /// Distinct workspaces must not share a store, or isolation is void.
    #[test]
    fn store_id_differs_per_workspace() {
        assert_ne!(workspace_store_id("ws-1"), workspace_store_id("ws-2"));
        assert_ne!(workspace_store_id(""), workspace_store_id("ws-1"));
    }
}

#[cfg(test)]
mod ticket_bridge_tests {
    use super::*;

    /// The whole bridge rests on this: `ymux-ticket:<data>` must parse
    /// as an opaque URL that hands the payload back byte-for-byte. Base64
    /// is case-sensitive, so any host-style normalization would corrupt it.
    #[test]
    fn opaque_scheme_preserves_base64_case() {
        let payload = "SGVsbG8tV29ybGRfMTIz"; // mixed case + `-` + `_`
        let url: Url = format!("{TICKET_SCHEME}:{payload}")
            .parse()
            .expect("sentinel url parses");
        assert_eq!(url.scheme(), TICKET_SCHEME);
        assert!(url.cannot_be_a_base(), "must be opaque, not host-based");
        assert_eq!(url.path(), payload, "payload must survive verbatim");
    }

    /// Contrast, and the actual reason for the opaque form: with `//`
    /// the payload is parsed as the authority, so `path()` — what the
    /// handler reads — comes back EMPTY and the capture would be lost.
    /// (Case survives either way; this is a non-special scheme.)
    #[test]
    fn authority_form_would_strand_the_payload_in_the_host() {
        let url: Url = format!("{TICKET_SCHEME}://SGVsbG8=")
            .parse()
            .expect("parses");
        assert_eq!(url.host_str(), Some("SGVsbG8="));
        assert_eq!(url.path(), "", "handler reads path() — would get nothing");
    }

    /// base64 padding and the standard alphabet's `/` and `+` must also
    /// survive the opaque form, so the bridge is not silently dependent
    /// on the JS side stripping padding.
    #[test]
    fn opaque_form_survives_padding_and_standard_alphabet() {
        let url: Url = format!("{TICKET_SCHEME}:a/b+c=").parse().expect("parses");
        assert_eq!(url.path(), "a/b+c=");
    }

    #[test]
    fn decode_capture_round_trips_json() {
        // base64url of {"selector":"#a","xpath":"/html[1]"}
        // r##…##: the payload contains `"#`, which would close an r#"…"#.
        let json = r##"{"selector":"#a","xpath":"/html[1]"}"##;
        let b64 = base64_encode_for_test(json.as_bytes());
        let v = decode_capture(&b64).expect("decodes");
        assert_eq!(v["selector"], "#a");
        assert_eq!(v["xpath"], "/html[1]");
    }

    #[test]
    fn decode_capture_rejects_garbage() {
        assert!(decode_capture("!!!not-base64!!!").is_err());
        // valid base64, but not JSON
        assert!(decode_capture(&base64_encode_for_test(b"hello")).is_err());
    }

    /// Non-ticket URLs must be left completely alone. We can't build an
    /// AppHandle in a unit test, so assert the scheme check directly —
    /// it is the first line of `handle_ticket_navigation`.
    #[test]
    fn ordinary_urls_are_not_ours() {
        for raw in [
            "http://127.0.0.1:5173/",
            "https://example.com/a?b=c",
            "about:blank",
        ] {
            let url: Url = raw.parse().expect("parses");
            assert_ne!(url.scheme(), TICKET_SCHEME, "{raw} must pass through");
        }
    }

    fn base64_encode_for_test(bytes: &[u8]) -> String {
        const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            let n = chunk.len();
            out.push(ALPH[(v >> 18) as usize & 63] as char);
            out.push(ALPH[(v >> 12) as usize & 63] as char);
            if n > 1 {
                out.push(ALPH[(v >> 6) as usize & 63] as char);
            }
            if n > 2 {
                out.push(ALPH[v as usize & 63] as char);
            }
        }
        out
    }
}


/// Phase 82.E: the `ymux-diag:` beacon channel. `browser_diag.js` reports
/// through `document.title`, which the PAGE also controls — so the two
/// things worth pinning down are that an ordinary title is inert, and
/// that a hostile one cannot forge extra lines in `debug.log`.
#[cfg(test)]
mod diag_title_tests {
    use super::*;

    fn b64(s: &str) -> String {
        // Mirrors what `btoa` emits: standard alphabet, `=` padding.
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let b = s.as_bytes();
        let mut out = String::new();
        for c in b.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            out.push(A[(n >> 18) as usize & 63] as char);
            out.push(A[(n >> 12) as usize & 63] as char);
            out.push(if c.len() > 1 {
                A[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if c.len() > 2 {
                A[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    #[test]
    fn ordinary_page_titles_are_inert() {
        let budget = AtomicUsize::new(DIAG_MAX_BEACONS);
        assert!(!handle_diag_title("w_1", "Grafana - Home", &budget));
        assert!(!handle_diag_title("w_1", "", &budget));
        // Close but not ours — the prefix must match exactly.
        assert!(!handle_diag_title("w_1", "ymux-ticket:AAAA", &budget));
        assert_eq!(
            budget.load(Ordering::Relaxed),
            DIAG_MAX_BEACONS,
            "a non-beacon title must not spend budget"
        );
    }

    #[test]
    fn btoa_output_round_trips_through_the_decoder() {
        // `btoa` uses the standard alphabet with padding; `base64_decode`
        // was written for the URL-safe, unpadded form the ticket bridge
        // sends. Both dialects have to work or the beacon is silent.
        let payload = r#"{"p":"boot","g0":42}"#;
        let decoded = decode_diag(&b64(payload)).expect("btoa output decodes");
        assert!(decoded.contains("\"p\":\"boot\""));
        assert!(decoded.contains("\"g0\":42"));
    }

    #[test]
    fn beacon_is_normalized_to_a_single_line() {
        // Raw newlines are legal JSON whitespace, so a page could
        // otherwise forge extra lines in debug.log. Re-serializing kills
        // that: whitespace between tokens is dropped, and a newline
        // inside a string comes back escaped.
        // Raw string for the body so the two kinds of newline stay
        // distinguishable: the ones added by format! are real LF bytes
        // between tokens (legal JSON whitespace, and the forging vector),
        // while the `\n` inside the value is a two-character JSON escape.
        let hostile = format!("{{\n{}\n}}", r#""p":"x","m":"a\nb [ERROR] forged""#);
        let decoded = decode_diag(&b64(&hostile)).expect("decodes");
        assert!(!decoded.contains('\n'), "must be one line: {decoded}");
        assert!(
            decoded.contains(r"a\nb"),
            "the escape must survive as an escape: {decoded}"
        );
    }

    #[test]
    fn non_json_and_non_base64_are_rejected_not_logged() {
        assert!(decode_diag(&b64("not json at all")).is_err());
        assert!(decode_diag("!!!not base64!!!").is_err());
    }

    #[test]
    fn oversized_beacons_are_truncated_on_a_char_boundary() {
        let big = format!(r#"{{"p":"x","m":"{}"}}"#, "\u{5e9}".repeat(2000));
        let decoded = decode_diag(&b64(&big)).expect("decodes");
        assert!(decoded.len() <= DIAG_MAX_LOG + "…(truncated)".len());
        assert!(decoded.ends_with("…(truncated)"));
    }

    #[test]
    fn the_budget_is_spent_and_then_the_channel_goes_quiet() {
        let budget = AtomicUsize::new(2);
        let beacon = format!("{DIAG_TITLE_PREFIX}{}", b64(r#"{"p":"boot"}"#));
        // Both are ours and both are logged...
        assert!(handle_diag_title("w_1", &beacon, &budget));
        assert!(handle_diag_title("w_1", &beacon, &budget));
        assert_eq!(budget.load(Ordering::Relaxed), 0);
        // ...and from here it still reports "mine" (so the watchdog stays
        // disarmed) but writes nothing more.
        assert!(handle_diag_title("w_1", &beacon, &budget));
        assert_eq!(
            budget.load(Ordering::Relaxed),
            0,
            "must saturate at zero, never wrap"
        );
    }
}
