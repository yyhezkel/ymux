//! Phase 80: local smart setup — detection + install engine for the
//! "local → new" wizard flow, plus the shared hidden-console process
//! helpers every wsl.exe / winget / npm invocation in the app goes
//! through (nothing in src/ set CREATE_NO_WINDOW before this module;
//! spawning wsl.exe from a GUI app without it flashes a console).
//!
//! The engine mirrors provisioning.rs deliberately: same StepProgress
//! payload (so the wizard reuses the step-card UI verbatim), same
//! keep-going-on-failure per-step semantics, events
//! `local-setup:progress` / `local-setup:complete`.
//!
//! Everything here follows Rule #3: argv arrays only. Values interpolated
//! into POSIX scripts run inside a distro go through
//! `ymux_core::shell_quote` exclusively.
//!
//! macOS port: the same commands/events serve the mac wizard. There the
//! `winget` slot describes Homebrew, the WSL chain is replaced by the
//! tmux "persistence chain" (`InstallTmuxLocal` -> `DeployTmuxConfLocal`),
//! and the finalized workspace is `Connection::Local`. Windows-only
//! machinery is `cfg(target_os = "windows")`; the unix step runner is
//! `run_step_unix`. A Finder-launched app inherits a bare PATH
//! (/usr/bin:/bin:/usr/sbin:/sbin), so every unix probe/spawn also
//! consults `unix_tool_dirs()`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, State};
use ymux_core::shell_quote;

use crate::provisioning::{ProvisioningError, RunHandle, StepProgress};
use crate::{log_info, log_warn, AppState};

/// CREATE_NO_WINDOW — suppress the console window a GUI-subsystem parent
/// would otherwise flash when spawning a console-subsystem child.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ─── Elevation + WSL platform preflight ──────────────────────────────
//
// Enabling the WSL Windows features needs administrator rights. wsl.exe
// self-elevates via its own UAC prompt, which is why a declined prompt
// used to surface as a bare `exit 1` with no way to tell it apart from a
// genuine failure — and why matching the English word "elevat" in the
// output was the only signal available. Checking the process token up
// front removes the guesswork: we know before the run whether the UAC
// step is coming, and when we drive it ourselves ShellExecuteEx reports
// a *reliable* code (ERROR_CANCELLED on a dismissed prompt).

/// The user dismissed the UAC consent dialog. Reported by ShellExecuteEx
/// itself, not by the child — wsl.exe never gets to run.
const ERROR_CANCELLED: u32 = 1223;
/// Windows servicing succeeded but the features only come alive after a
/// restart. `wsl --install` returns this when it enables the optional
/// features on a machine that had none.
const ERROR_SUCCESS_REBOOT_REQUIRED: u32 = 3010;
/// ERROR_ELEVATION_REQUIRED. Not what a declined UAC prompt returns —
/// kept because a policy-blocked ShellExecuteEx can still surface it.
const ERROR_ELEVATION_REQUIRED: u32 = 740;
/// Matches the 1800s ceiling `run_capture` puts on the non-elevated
/// path, so neither route can hang the wizard indefinitely.
#[cfg(target_os = "windows")]
const WSL_INSTALL_TIMEOUT_MS: u32 = 1_800_000;

/// Is this process running with an elevated token?
#[cfg(target_os = "windows")]
pub(crate) fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: every pointer handed to the API points at a live local, the
    // token handle is closed on both paths, and we only read `elevation`
    // after GetTokenInformation reports success.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn is_elevated() -> bool {
    false
}

/// Distro names go into a ShellExecuteEx parameter *string*, which has no
/// argv array to hide behind (Rule #3). Nothing outside this shape is
/// ever allowed through — the value comes from `wsl -l -v` parsing or a
/// user field, so it is not trusted by construction.
// Pure + unit-tested on every OS; only the Windows install path calls it.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn valid_distro_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What the wizard needs to know before it offers to run the WSL chain.
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct WslPreflight {
    /// `wsl --status` answers — the Windows features are enabled.
    pub platform_ready: bool,
    /// At least one distro is registered.
    pub distro_present: bool,
    /// This ymux process holds an elevated token.
    pub elevated: bool,
    /// Installing the platform needs admin and we don't have it, so a UAC
    /// prompt is unavoidable. False once the platform is already up —
    /// adding a further distro to a live platform needs no elevation.
    pub needs_elevation: bool,
}

/// Read-only probe the wizard runs before showing the WSL group, so the
/// UAC/reboot cost is stated up front instead of discovered by failing.
#[tauri::command]
pub(crate) async fn local_setup_preflight() -> Result<WslPreflight, String> {
    #[cfg(target_os = "windows")]
    let pre = {
        let wsl = inspect_wsl(None).await;
        let elevated = is_elevated();
        let platform_ready = wsl.wsl_ready;
        WslPreflight {
            platform_ready,
            distro_present: !wsl.distros.is_empty(),
            elevated,
            needs_elevation: !platform_ready && !elevated,
        }
    };
    // macOS/Linux: there is no platform to enable and nothing in the
    // wizard needs elevation (Homebrew + native installers are per-user),
    // so the gate is always open.
    #[cfg(not(target_os = "windows"))]
    let pre = WslPreflight {
        platform_ready: true,
        distro_present: true,
        elevated: is_elevated(),
        needs_elevation: false,
    };
    log_info(
        "SETUP",
        &format!(
            "preflight: platform_ready={} distro_present={} elevated={} needs_elevation={}",
            pre.platform_ready, pre.distro_present, pre.elevated, pre.needs_elevation
        ),
    );
    Ok(pre)
}

/// A path is about to be pasted into a `cmd /s /c` command string, where
/// there is no argv array to hide behind (Rule #3, same reasoning as
/// `valid_distro_name`). Only a conservative Windows-path shape is let
/// through; anything else returns None and the caller silently drops the
/// redirect rather than building a command it cannot vouch for.
#[cfg(target_os = "windows")]
fn cmd_safe_path(p: &std::path::Path) -> Option<String> {
    let s = p.to_str()?;
    let ok = !s.is_empty()
        && s.len() <= 240
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '\\' | '/' | ':' | '.' | '-' | '_')
        });
    ok.then(|| s.to_string())
}

/// Build the `cmd.exe` parameter string for the elevated install.
///
/// `/s /c` is the documented way to hand cmd a command string whose own
/// quoting we control: it strips exactly the outer pair and takes the rest
/// verbatim, instead of cmd's usual quote-rebalancing.
///
/// Both interpolated values are pre-validated by the caller — `distro` by
/// `valid_distro_name`, `log` by `cmd_safe_path` — so neither can carry a
/// quote or a cmd metacharacter (Rule #3; ShellExecuteEx offers no argv
/// array to hide behind). `log = None` means the temp path was not
/// representable, so we install without capture rather than not at all.
///
/// No space before `&&`: `set WSL_UTF8=1 &&` would set the value to "1 ".
#[cfg(target_os = "windows")]
fn wsl_install_cmd_params(distro: &str, log: Option<&str>) -> String {
    let run = format!("set WSL_UTF8=1&& wsl.exe --install --no-launch -d {distro}");
    match log {
        Some(l) => format!("/s /c \"{run} > \"{l}\" 2>&1\""),
        None => format!("/s /c \"{run}\""),
    }
}

/// Run `wsl.exe --install --no-launch -d <distro>` through a UAC consent
/// prompt and wait for it. Returns (exit code, merged stdout+stderr).
///
/// ShellExecuteEx is the only way to request elevation for a child, and it
/// gives us no stdout/stderr pipes. Returning just the code was not good
/// enough: `wsl --install` reports a generic 1 for most real failures, so
/// the UI could only say "exit 1" and `classify_wsl_install`'s text
/// fallbacks were dead code on this path. Instead of calling wsl.exe
/// directly we elevate `cmd /s /c` and redirect into a temp file we own,
/// then read it back — the message survives the privilege boundary.
///
/// `WSL_UTF8=1` matters here for the same reason it does in `wsl_cmd`:
/// without it wsl.exe writes UTF-16 and any non-ASCII (a localized
/// Windows error) comes back mangled.
#[cfg(target_os = "windows")]
fn run_wsl_install_elevated(distro: &str) -> Result<(u32, String), ProvisioningError> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    if !valid_distro_name(distro) {
        return Err(ProvisioningError::Generic(format!(
            "refusing to elevate with an unsafe distro name: {distro:?}"
        )));
    }
    let wide = |s: &str| -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>()
    };
    // A per-run file so two wizards (or a stale run) can never read each
    // other's output. Nanos + pid is plenty — this is collision avoidance,
    // not a security boundary; the file lives in the user's own temp dir.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let log_path = std::env::temp_dir().join(format!(
        "ymux-wsl-install-{}-{stamp}.log",
        std::process::id()
    ));
    // Best-effort: a leftover from a crashed run would otherwise be read
    // back as if it were this run's output.
    let _ = std::fs::remove_file(&log_path);
    let safe_log = cmd_safe_path(&log_path);

    let verb = wide("runas");
    let file = wide("cmd.exe");
    let params = wide(&wsl_install_cmd_params(distro, safe_log.as_deref()));

    // SAFETY: the struct is fully initialised, every pointer refers to a
    // buffer that outlives the call, and hProcess is closed on all paths.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE as i32;

        if ShellExecuteExW(&mut info) == 0 {
            let err = GetLastError();
            let _ = std::fs::remove_file(&log_path);
            return Err(match err {
                ERROR_CANCELLED => ProvisioningError::ElevationRequired {
                    step: "InstallWsl".into(),
                    hint: "The Windows administrator prompt was dismissed. \
                           Re-run the step and approve it."
                        .into(),
                },
                ERROR_ELEVATION_REQUIRED => ProvisioningError::ElevationRequired {
                    step: "InstallWsl".into(),
                    hint: "Windows refused to elevate this process. Start ymux \
                           as administrator and re-run the step."
                        .into(),
                },
                other => ProvisioningError::Generic(format!(
                    "elevated wsl --install could not start (Windows error {other})"
                )),
            });
        }
        if info.hProcess.is_null() {
            let _ = std::fs::remove_file(&log_path);
            return Err(ProvisioningError::Generic(
                "elevated wsl --install returned no process handle".into(),
            ));
        }
        // Same 30-minute ceiling the non-elevated path gets from
        // run_capture — a feature enable plus a store download is slow,
        // but never waiting would hang the wizard on "running" forever.
        let waited = WaitForSingleObject(info.hProcess, WSL_INSTALL_TIMEOUT_MS);
        if waited != WAIT_OBJECT_0 {
            CloseHandle(info.hProcess);
            // Deliberately not killed: it holds an elevated token we
            // cannot re-acquire, and Windows servicing mid-flight is
            // worse to interrupt than to leave running.
            return Err(ProvisioningError::Generic(format!(
                "elevated wsl --install did not finish within {}s — it may still be \
                 running; re-run the step once it settles",
                WSL_INSTALL_TIMEOUT_MS / 1000
            )));
        }
        let mut code: u32 = 0;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        // Read before the exit-code check: if reading the code failed we
        // still want the file gone, and the message is worth nothing then.
        let output = safe_log
            .as_ref()
            .and_then(|_| std::fs::read(&log_path).ok())
            .map(|b| clean_wsl_output(&b).trim().to_string())
            .unwrap_or_default();
        let _ = std::fs::remove_file(&log_path);
        if got == 0 {
            return Err(ProvisioningError::Generic(
                "could not read the elevated wsl --install exit code".into(),
            ));
        }
        Ok((code, output))
    }
}

/// Map a `wsl --install` exit code onto a structured error. Split out so
/// the elevated and non-elevated paths classify identically, and unit
/// tested — this is the logic that used to be an English substring match.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn classify_wsl_install(code: u32, output: &str) -> Result<(), ProvisioningError> {
    let low = output.to_lowercase();
    match code {
        0 => Ok(()),
        ERROR_SUCCESS_REBOOT_REQUIRED => Err(ProvisioningError::RebootRequired {
            step: "InstallWsl".into(),
            hint: "Windows enabled the WSL features. Restart the machine, then \
                   run the install again to finish."
                .into(),
        }),
        ERROR_CANCELLED | ERROR_ELEVATION_REQUIRED => Err(ProvisioningError::ElevationRequired {
            step: "InstallWsl".into(),
            hint: "The step needs administrator rights. Re-run it and approve \
                   the Windows prompt."
                .into(),
        }),
        other => {
            // Last-resort text sniff for older wsl.exe builds that report a
            // generic 1. Locale-dependent, hence the fallback position.
            if low.contains("elevat") || low.contains("0x80070005") {
                Err(ProvisioningError::ElevationRequired {
                    step: "InstallWsl".into(),
                    hint: "Run `wsl --install` from an elevated terminal, reboot \
                           if prompted, then re-run this step."
                        .into(),
                })
            } else if low.contains("reboot") || low.contains("restart") {
                Err(ProvisioningError::RebootRequired {
                    step: "InstallWsl".into(),
                    hint: "Restart the machine, then run the install again to finish."
                        .into(),
                })
            } else {
                Err(ProvisioningError::StepFailed {
                    step: "InstallWsl".into(),
                    exit_code: other as i32,
                    stderr: output.to_string(),
                })
            }
        }
    }
}

/// Reboot the machine to finish a WSL feature enable. Only ever called
/// from an explicit user confirmation in the reboot card — never
/// automatically, since other apps may hold unsaved work.
/// Non-Windows: there is no WSL feature to finish enabling, so nothing
/// should ever reach here. It was reachable — the command was registered
/// unconditionally, so a mis-wired frontend would have tried to spawn
/// `shutdown.exe` on a Mac. Refuse rather than attempt a reboot.
#[cfg(not(target_os = "windows"))]
#[tauri::command]
pub(crate) async fn restart_windows() -> Result<(), String> {
    Err("restart_windows is Windows-only (it finishes a WSL feature enable)".into())
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub(crate) async fn restart_windows() -> Result<(), String> {
    log_warn("SETUP", "user confirmed restart to finish the WSL install");
    let mut c = hidden_cmd("shutdown.exe");
    c.args(["/r", "/t", "0", "/c", "ymux: finishing the WSL install"]);
    let out = c
        .output()
        .await
        .map_err(|e| format!("could not start shutdown.exe: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "shutdown.exe exited {}: {}",
            out.status.code().unwrap_or(-1),
            clean_wsl_output(&out.stderr).trim()
        ))
    }
}

/// Build a hidden-console tokio Command. ALL wsl.exe / winget / npm /
/// probe invocations go through this (or `wsl_cmd`).
pub(crate) fn hidden_cmd(program: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    // Every caller runs the child to completion, so kill_on_drop only
    // fires when a wall-clock timeout drops the output() future — tokio
    // does NOT kill the child on drop by default, and an orphaned
    // install.ps1 keeps %USERPROFILE%\.claude\downloads\claude-<ver>.exe
    // locked, so every retry of the step then fails with "The process
    // cannot access the file … being used by another process".
    c.kill_on_drop(true);
    c.stdin(std::process::Stdio::null());
    c.stdout(std::process::Stdio::piped());
    c.stderr(std::process::Stdio::piped());
    c
}

/// wsl.exe with UTF-8 output forced and a hidden console. wsl.exe emits
/// UTF-16LE by default; WSL_UTF8=1 (supported since WSL 0.64) makes the
/// management-command output parseable. Output parsing must still strip
/// stray NULs for very old inbox WSL builds that ignore the variable.
pub(crate) fn wsl_cmd() -> tokio::process::Command {
    let mut c = hidden_cmd("wsl.exe");
    c.env("WSL_UTF8", "1");
    c
}

/// Strip UTF-16 leftovers (NULs) + CRs from wsl.exe output. See wsl_cmd.
pub(crate) fn clean_wsl_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| *c != '\0' && *c != '\r')
        .collect()
}

/// Run a script inside a distro via `wsl.exe [-d <distro>] [-u <user>]
/// -- sh -s`, feeding the script on **stdin**, and return (exit_code,
/// merged stdout+stderr). `script` must be a static string or built
/// exclusively with `ymux_core::shell_quote` for interpolated values
/// (same discipline as the remote tmux path).
///
/// The script goes in on stdin rather than as an argv element because
/// passing it as an argument is not safe here: something between
/// `wsl.exe` and the distro expands the text once before our shell sees
/// it. Measured on 2026-08-10 against Ubuntu/WSL2:
///
///   sh -lc "X=hello; echo [$X]"   ->  []          (argv)
///   sh -s   <<< same script       ->  [hello]     (stdin)
///
/// `$HOME` survived (it exists in the expanding context too) and `$0`
/// came back as `/bin/bash`, which is what gave the mechanism away. The
/// practical effect was that EVERY shell variable a script assigned read
/// back empty — so `CreateWslUser` ran `useradd` with an empty username
/// and wrote `default=` (empty) into /etc/wsl.conf, which then made WSL
/// call getpwnam("") on every invocation and silently fall back to root.
/// That one line is the origin of the whole WSL setup saga.
pub(crate) async fn wsl_exec(
    distro: Option<&str>,
    user: Option<&str>,
    script: &str,
) -> Result<(i32, String), String> {
    use tokio::io::AsyncWriteExt;
    let mut c = wsl_cmd();
    if let Some(d) = distro {
        if !d.is_empty() {
            c.arg("-d").arg(d);
        }
    }
    if let Some(u) = user {
        c.arg("-u").arg(u);
    }
    // `-s` reads the program from stdin; `-l` still gives a login shell.
    c.arg("--").arg("sh").arg("-ls");
    c.stdin(std::process::Stdio::piped());
    c.stdout(std::process::Stdio::piped());
    c.stderr(std::process::Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("wsl.exe spawn: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "wsl stdin unavailable".to_string())?;
        stdin
            .write_all(script.as_bytes())
            .await
            .map_err(|e| format!("script write: {e}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("script close: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("wsl.exe wait: {e}"))?;
    let mut text = clean_wsl_output(&out.stdout);
    let err = clean_wsl_output(&out.stderr);
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    Ok((out.status.code().unwrap_or(-1), text))
}

/// Run any hidden command to completion with a wall-clock timeout,
/// returning (exit_code, merged output). A timeout surfaces as Err —
/// per-step handling turns it into a failed step, never a hang.
async fn run_capture(
    mut c: tokio::process::Command,
    what: &str,
    timeout_secs: u64,
) -> Result<(i32, String), String> {
    let fut = async {
        let out = c.output().await.map_err(|e| format!("{what}: spawn: {e}"))?;
        let mut text = clean_wsl_output(&out.stdout);
        let err = clean_wsl_output(&out.stderr);
        if !err.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&err);
        }
        Ok::<(i32, String), String>((out.status.code().unwrap_or(-1), text))
    };
    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut)
        .await
        .map_err(|_| format!("{what}: timed out after {timeout_secs}s"))?
}

// ─── WSL RPC bridge (hooks from inside WSL → app) ────────────────────
//
// The CLI inside a distro dials YMUX_SOCKET_ADDR over TCP (the same
// path a remote SSH server uses via the reverse tunnel); Windows named
// pipes are unreachable from WSL2 Linux. This is a lazy, app-lifetime
// TCP listener that HMAC-gates every connection with a per-run token
// (Rule #8: the token never reaches logs) and bridges accepted streams
// into the named-pipe RPC server via ymux_tunnel::bridge_stream_to_pipe.
//
// Bind surface: 0.0.0.0 — in NAT mode the distro reaches the host via
// the vEthernet gateway IP (which varies per boot), in mirrored mode via
// 127.0.0.1. The HMAC challenge-response is the gate, exactly like the
// SSH tunnel path.

static WSL_BRIDGE: tokio::sync::OnceCell<(u16, std::sync::Arc<String>)> =
    tokio::sync::OnceCell::const_new();

pub(crate) async fn ensure_wsl_bridge() -> Result<(u16, std::sync::Arc<String>), String> {
    WSL_BRIDGE
        .get_or_try_init(|| async {
            let token = std::sync::Arc::new(ymux_tunnel::generate_token());
            let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0))
                .await
                .map_err(|e| format!("wsl-bridge bind: {e}"))?;
            let port = listener
                .local_addr()
                .map_err(|e| format!("wsl-bridge local_addr: {e}"))?
                .port();
            let token_for_loop = token.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, peer)) => {
                            let tok = token_for_loop.clone();
                            let peer = peer.to_string();
                            tauri::async_runtime::spawn(async move {
                                if let Err(e) =
                                    ymux_tunnel::bridge_stream_to_pipe(stream, &tok, &peer).await
                                {
                                    // Handshake failures are expected noise from
                                    // port scanners — engineer-only log.
                                    tracing::debug!("wsl-bridge: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            log_warn("RPC", &format!("wsl-bridge accept error: {e}"));
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }
            });
            log_info("RPC", &format!("wsl-bridge: listening on 0.0.0.0:{port} (HMAC-gated)"));
            Ok((port, token))
        })
        .await
        .cloned()
}

/// Resolve the address a process INSIDE the distro should dial to reach
/// the Windows-side bridge: mirrored/wsl1 networking → 127.0.0.1, NAT →
/// the default-route gateway. `wslinfo --networking-mode` ships inside
/// every WSL2 distro on current WSL; absence falls back to NAT.
pub(crate) async fn resolve_wsl_host_addr(distro: Option<&str>, port: u16) -> Option<String> {
    let script = "echo \"MODE $(wslinfo --networking-mode 2>/dev/null || echo nat)\"; \
                  echo \"GW $(ip route list default 2>/dev/null | head -1 | cut -d' ' -f3)\"";
    let (code, out) = wsl_exec(distro, None, script).await.ok()?;
    if code != 0 {
        return None;
    }
    let mut mode = "nat".to_string();
    let mut gw = String::new();
    for line in out.lines() {
        if let Some(v) = line.trim().strip_prefix("MODE ") {
            mode = v.trim().to_string();
        } else if let Some(v) = line.trim().strip_prefix("GW ") {
            gw = v.trim().to_string();
        }
    }
    let ip = match mode.as_str() {
        "mirrored" | "wsl1" | "none" => "127.0.0.1".to_string(),
        _ if !gw.is_empty() => gw,
        _ => return None,
    };
    Some(format!("{ip}:{port}"))
}

/// Write `~/.ymux/run/last.env` inside the distro so the CLI picks up
/// the bridge address + token even outside the tmux environment (the
/// same file the remote bootstrap writes over SSH). Token never logged.
pub(crate) async fn write_wsl_env_file(
    distro: Option<&str>,
    socket_addr: &str,
    token: &str,
    pane_id: &str,
) -> Result<(), String> {
    let script = format!(
        "mkdir -p \"$HOME/.ymux/run\" && \
         printf 'YMUX_SOCKET_ADDR=%s\\nYMUX_TUNNEL_TOKEN=%s\\nYMUX_PANE_ID=%s\\n' {a} {t} {p} > \"$HOME/.ymux/run/last.env\" && \
         chmod 0600 \"$HOME/.ymux/run/last.env\"",
        a = shell_quote(socket_addr),
        t = shell_quote(token),
        p = shell_quote(pane_id)
    );
    let (code, out) = wsl_exec(distro, None, &script).await?;
    if code != 0 {
        return Err(format!("write last.env failed (exit {code}): {out}"));
    }
    Ok(())
}

// ─── Detection ───────────────────────────────────────────────────────

#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ToolStatus {
    pub present: bool,
    pub version: Option<String>,
    pub path: Option<String>,
}

impl ToolStatus {
    fn missing() -> Self {
        Self {
            present: false,
            version: None,
            path: None,
        }
    }
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct WslInspect {
    pub wsl_present: bool,
    pub wsl_ready: bool,
    pub distros: Vec<String>,
    pub default_distro: Option<String>,
    pub tmux_installed: Option<bool>,
    /// "ok" | "stale" | "missing" — ymux CLI inside the distro vs the
    /// embedded manifest sha.
    pub ymux_cli_state: Option<String>,
    pub tmux_conf_ok: Option<bool>,
    /// Is the `claude` BINARY runnable inside the distro?
    ///
    /// This used to probe `[ -d "$HOME/.claude" ]`, which is the wrong
    /// question twice over: `ymux setup-hooks` creates that directory
    /// itself, so after one wizard run the field read `true` on a distro
    /// with no Claude Code at all — and the install step that depends on it
    /// would then skip forever. `command -v claude` is what the user
    /// actually experiences when they type `claude`.
    pub claude_inside: Option<bool>,
    pub hooks_version_inside: Option<String>,
}

impl WslInspect {
    /// No WSL at all — wsl.exe missing on Windows, or any non-Windows OS.
    fn absent() -> Self {
        Self {
            wsl_present: false,
            wsl_ready: false,
            distros: Vec::new(),
            default_distro: None,
            tmux_installed: None,
            ymux_cli_state: None,
            tmux_conf_ok: None,
            claude_inside: None,
            hooks_version_inside: None,
        }
    }
}

// macOS-only detection the wizard's persistence chain is gated on.
// (Plain `//` comments on purpose: the ts-rs binding was hand-written
// and carries no doc blocks — keep the two in sync.)
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[cfg_attr(target_os = "windows", allow(dead_code))] // never constructed there
pub(crate) struct MacInspect {
    // tmux on PATH or in a canonical dir (Homebrew).
    pub tmux: ToolStatus,
    // `~/.ymux/tmux.conf` exists AND is byte-identical to the bundled
    // conf (same embedded payload DeployTmuxConfToWsl ships).
    pub tmux_conf_ok: bool,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct LocalSetupInspect {
    // Windows: winget. macOS/Linux: Homebrew (`brew`) — the package
    // manager the InstallGit/InstallNodejs/InstallTmuxLocal steps use.
    pub winget: ToolStatus,
    pub git: ToolStatus,
    pub node: ToolStatus,
    pub npm: ToolStatus,
    pub claude: ToolStatus,
    pub codex: ToolStatus,
    pub gemini: ToolStatus,
    /// 2026-08-19: the multiplexer that gives native Windows panes session
    /// persistence - the job WSL used to do here. Always `missing()` off
    /// Windows, where tmux plays that role - see `mac`.
    pub zellij: ToolStatus,
    // Always `WslInspect::absent()` off Windows.
    pub wsl: WslInspect,
    pub local_hooks_version: Option<String>,
    pub local_claude_dir: bool,
    // None on Windows, Some on macOS/Linux.
    pub mac: Option<MacInspect>,
}

/// Probe one tool: PATH lookup over the candidate exe names (PATHEXT-
/// aware via local_wizard::which), then optional canonical fallback
/// paths, then a best-effort `--version` with a short timeout. Never
/// fails — a broken tool just reads as missing/version-less.
async fn probe_tool(candidates: &[&str], canonical: &[PathBuf]) -> ToolStatus {
    let mut found: Option<PathBuf> = None;
    for c in candidates {
        if let Some(p) = crate::local_wizard::which(c) {
            found = Some(p);
            break;
        }
    }
    if found.is_none() {
        for p in canonical {
            if p.is_file() {
                found = Some(p.clone());
                break;
            }
        }
    }
    let Some(path) = found else {
        return ToolStatus::missing();
    };
    let mut vc = hidden_cmd(&path.to_string_lossy());
    vc.arg("--version");
    // npm/codex/gemini are `#!/usr/bin/env node` scripts — from a
    // Finder-launched app node is not on PATH, so the probe would fail
    // even though the tool is right there.
    #[cfg(not(target_os = "windows"))]
    {
        vc.env("PATH", unix_path_env());
    }
    let version = run_capture(vc, "version probe", 5)
        .await
        .ok()
    .and_then(|(code, out)| {
        if code == 0 {
            out.lines().next().map(|l| l.trim().to_string())
        } else {
            None
        }
    });
    ToolStatus {
        present: true,
        version,
        path: Some(path.to_string_lossy().to_string()),
    }
}

#[cfg(target_os = "windows")]
fn program_files() -> PathBuf {
    PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into()))
}

/// The user's home: %USERPROFILE% on Windows, $HOME elsewhere.
fn home_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    PathBuf::from(home)
}

/// Exe names `probe_tool` tries on PATH for npm.
#[cfg(target_os = "windows")]
const NPM_CANDIDATES: &[&str] = &["npm.cmd", "npm.exe"];
#[cfg(not(target_os = "windows"))]
const NPM_CANDIDATES: &[&str] = &["npm"];

/// Canonical install locations probed AFTER PATH — a tool winget just
/// installed isn't on our process's (stale) PATH.
#[cfg(target_os = "windows")]
fn canonical_git() -> Vec<PathBuf> {
    vec![program_files().join("Git").join("cmd").join("git.exe")]
}
#[cfg(target_os = "windows")]
fn canonical_node() -> Vec<PathBuf> {
    vec![program_files().join("nodejs").join("node.exe")]
}
#[cfg(target_os = "windows")]
fn canonical_npm() -> Vec<PathBuf> {
    vec![program_files().join("nodejs").join("npm.cmd")]
}
#[cfg(target_os = "windows")]
fn canonical_claude() -> Vec<PathBuf> {
    // The official install.ps1 target (verified against
    // code.claude.com/docs/en/setup).
    vec![home_dir().join(".local").join("bin").join("claude.exe")]
}

// ─── unix (macOS first, Linux shares) canonical locations ───────────
//
// A Finder-launched GUI app gets PATH=/usr/bin:/bin:/usr/sbin:/sbin — no
// Homebrew, no ~/.local/bin, no npm -g prefix. So on unix a PATH lookup
// alone is NOT enough: every probe also checks these dirs, and every
// spawn gets `unix_path_env()` so the child's own subprocesses (brew →
// git/curl, npm → node) resolve too.

/// Directories a wizard-installed tool lands in, most specific first.
#[cfg(not(target_os = "windows"))]
fn unix_tool_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        PathBuf::from("/opt/homebrew/bin"),               // brew, Apple Silicon
        PathBuf::from("/usr/local/bin"),                  // brew, Intel + node pkg
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin"),  // brew, Linux
        home.join(".local").join("bin"),                  // claude native installer
        home.join(".npm-global").join("bin"),             // npm -g with a user prefix
        PathBuf::from("/usr/bin"),                        // git via Xcode CLT
    ]
}

/// `<dir>/<name>` for every `unix_tool_dirs()` entry.
#[cfg(not(target_os = "windows"))]
fn unix_canonical(name: &str) -> Vec<PathBuf> {
    unix_tool_dirs().into_iter().map(|d| d.join(name)).collect()
}

/// PATH for children we spawn on unix: the canonical dirs prepended to
/// whatever we inherited. An env value, not a shell string (Rule #3).
#[cfg(not(target_os = "windows"))]
pub(crate) fn unix_path_env() -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = unix_tool_dirs();
    dirs.extend(std::env::split_paths(&inherited));
    // join_paths only fails on a dir containing ':' — then keep what we had.
    std::env::join_paths(dirs).unwrap_or(inherited)
}

#[cfg(not(target_os = "windows"))]
fn canonical_brew() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/opt/homebrew/bin/brew"),
        PathBuf::from("/usr/local/bin/brew"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/brew"),
    ]
}
#[cfg(not(target_os = "windows"))]
fn canonical_npm() -> Vec<PathBuf> {
    unix_canonical("npm")
}
#[cfg(not(target_os = "windows"))]
fn canonical_claude() -> Vec<PathBuf> {
    // The official install.sh target (~/.local/bin/claude) plus the
    // usual dirs a `npm i -g @anthropic-ai/claude-code` lands in.
    unix_canonical("claude")
}

/// The local Claude Code binary, if installed: PATH first, then the
/// canonical install dirs (a Finder-launched app has a minimal PATH).
/// Shared by the usage indicator and anything else that shells out to a
/// local `claude`.
pub(crate) fn resolve_claude_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "claude.exe" } else { "claude" };
    crate::local_wizard::which(name)
        .or_else(|| canonical_claude().into_iter().find(|p| p.is_file()))
        // Phase 90: an npm-global install on Windows is `claude.cmd`, which
        // `which("claude.exe")` cannot see (it only appends PATHEXT to the
        // name it is given). `probe_tool` already knew both spellings; the
        // one-shot `claude -p` callers resolve through here and did not.
        .or_else(|| {
            if cfg!(windows) {
                crate::local_wizard::which("claude.cmd")
            } else {
                None
            }
        })
}

/// The winget MSI's target. It also puts this directory on the USER PATH,
/// but that only reaches processes started afterwards — so probing the path
/// directly is what makes zellij visible to a ymux that was already running
/// when the install happened.
#[cfg(target_os = "windows")]
fn canonical_zellij() -> Vec<PathBuf> {
    match std::env::var_os("LOCALAPPDATA") {
        Some(d) => vec![PathBuf::from(d).join("Zellij").join("zellij.exe")],
        None => Vec::new(),
    }
}

/// Parse `wsl -l -v` — the default distro carries a `*` marker (stable
/// across Windows display languages, unlike `wsl --status` text).
#[cfg(target_os = "windows")]
fn parse_wsl_list_verbose(text: &str) -> (Vec<String>, Option<String>) {
    let mut distros = Vec::new();
    let mut default = None;
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue; // header
        }
        let starred = line.trim_start().starts_with('*');
        let name = line
            .trim_start()
            .trim_start_matches('*')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        if name.is_empty() || name.to_ascii_lowercase().starts_with("docker-desktop") {
            continue;
        }
        if starred {
            default = Some(name.clone());
        }
        distros.push(name);
    }
    (distros, default)
}

#[cfg(target_os = "windows")]
async fn inspect_wsl(distro_override: Option<&str>) -> WslInspect {
    let mut w = WslInspect::absent();
    w.wsl_present = crate::local_wizard::which("wsl.exe").is_some();
    if !w.wsl_present {
        return w;
    }
    // `wsl --status` exits 0 with non-empty output when the platform is
    // installed (same semantics local_wizard::wsl_available pinned).
    let mut status_cmd = wsl_cmd();
    status_cmd.arg("--status");
    if let Ok((code, out)) = run_capture(status_cmd, "wsl --status", 15).await {
        w.wsl_ready = code == 0 && !out.trim().is_empty();
    }
    if !w.wsl_ready {
        return w;
    }
    if let Ok((code, out)) = run_capture(
        {
            let mut c = wsl_cmd();
            c.arg("-l").arg("-v");
            c
        },
        "wsl -l -v",
        15,
    )
    .await
    {
        if code == 0 {
            let (distros, default) = parse_wsl_list_verbose(&out);
            w.distros = distros;
            w.default_distro = default;
        }
    }
    if w.distros.is_empty() {
        return w;
    }
    let target = distro_override
        .map(|s| s.to_string())
        .or_else(|| w.default_distro.clone());
    // One batched probe — a single wsl.exe spawn instead of five keeps
    // cold-distro latency tolerable. Machine-readable lines, parsed here.
    let script = r#"
echo "TMUX $(command -v tmux >/dev/null 2>&1 && echo yes || echo no)"
if [ -f "$HOME/.ymux/bin/ymux-linux-x64" ]; then echo "CLI $(sha256sum "$HOME/.ymux/bin/ymux-linux-x64" | cut -d' ' -f1)"; else echo "CLI missing"; fi
if [ -f "$HOME/.ymux/tmux.conf" ]; then echo "CONF $(sha256sum "$HOME/.ymux/tmux.conf" | cut -d' ' -f1)"; else echo "CONF missing"; fi
echo "CLAUDE $(command -v claude >/dev/null 2>&1 && echo yes || echo no)"
echo "HOOKSV $(sed -n 's/.*"hooks_version"[: ]*"\([^"]*\)".*/\1/p' "$HOME/.claude/settings.json" 2>/dev/null | head -1)"
"#;
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        wsl_exec(target.as_deref(), None, script),
    )
    .await;
    let Ok(Ok((0, out))) = probe else {
        return w;
    };
    let manifest = crate::remote_bootstrap::embedded_manifest().ok();
    let cli_sha = manifest
        .as_ref()
        .and_then(|m| m.get("x86_64-linux").map(|e| e.sha256.clone()));
    let conf_sha = manifest
        .as_ref()
        .and_then(|m| m.get("tmux-conf").map(|e| e.sha256.clone()));
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("TMUX ") {
            w.tmux_installed = Some(v == "yes");
        } else if let Some(v) = line.strip_prefix("CLI ") {
            w.ymux_cli_state = Some(if v == "missing" {
                "missing".into()
            } else if cli_sha.as_deref() == Some(v) {
                "ok".into()
            } else {
                "stale".into()
            });
        } else if let Some(v) = line.strip_prefix("CONF ") {
            w.tmux_conf_ok = Some(v != "missing" && conf_sha.as_deref() == Some(v));
        } else if let Some(v) = line.strip_prefix("CLAUDE ") {
            w.claude_inside = Some(v == "yes");
        } else if let Some(v) = line.strip_prefix("HOOKSV ") {
            if !v.is_empty() {
                w.hooks_version_inside = Some(v.to_string());
            }
        }
    }
    w
}

fn local_hooks_version() -> Option<String> {
    let p = home_dir().join(".claude").join("settings.json");
    let text = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("ymux_meta")?
        .get("hooks_version")?
        .as_str()
        .map(|s| s.to_string())
}

/// Detect everything the "local → new" wizard offers to install. Never
/// fails as a whole — each probe degrades independently.
#[tauri::command]
pub(crate) async fn local_setup_inspect(distro: Option<String>) -> Result<LocalSetupInspect, String> {
    #[cfg(target_os = "windows")]
    let inspect = {
        let winget = probe_tool(&["winget.exe"], &[]).await;
        let git = probe_tool(&["git.exe"], &canonical_git()).await;
        let node = probe_tool(&["node.exe"], &canonical_node()).await;
        let npm = probe_tool(NPM_CANDIDATES, &canonical_npm()).await;
        let claude = probe_tool(&["claude.exe", "claude.cmd"], &canonical_claude()).await;
        let codex = probe_tool(&["codex.cmd", "codex.exe"], &[]).await;
        let gemini = probe_tool(&["gemini.cmd", "gemini.exe"], &[]).await;
        let zellij = probe_tool(&["zellij.exe"], &canonical_zellij()).await;
        let wsl = inspect_wsl(distro.as_deref()).await;
        LocalSetupInspect {
            winget,
            git,
            node,
            npm,
            claude,
            codex,
            gemini,
            zellij,
            wsl,
            local_hooks_version: local_hooks_version(),
            local_claude_dir: home_dir().join(".claude").is_dir(),
            mac: None,
        }
    };
    #[cfg(not(target_os = "windows"))]
    let inspect = {
        let _ = distro; // WSL-only input; nothing to target here.
        // The `winget` slot carries Homebrew on unix (see the struct).
        let winget = probe_tool(&["brew"], &canonical_brew()).await;
        let git = probe_tool(&["git"], &unix_canonical("git")).await;
        let node = probe_tool(&["node"], &unix_canonical("node")).await;
        let npm = probe_tool(NPM_CANDIDATES, &canonical_npm()).await;
        let claude = probe_tool(&["claude"], &canonical_claude()).await;
        let codex = probe_tool(&["codex"], &unix_canonical("codex")).await;
        let gemini = probe_tool(&["gemini"], &unix_canonical("gemini")).await;
        let tmux = probe_tool(&["tmux"], &unix_canonical("tmux")).await;
        LocalSetupInspect {
            winget,
            git,
            node,
            npm,
            claude,
            codex,
            gemini,
            zellij: ToolStatus::missing(),
            wsl: WslInspect::absent(),
            local_hooks_version: local_hooks_version(),
            local_claude_dir: home_dir().join(".claude").is_dir(),
            mac: Some(MacInspect {
                tmux,
                tmux_conf_ok: local_tmux_conf_ok(),
            }),
        }
    };
    Ok(inspect)
}

/// The bundled tmux.conf — the same embedded payload DeployTmuxConfToWsl
/// pipes into a distro. Returns (bytes, manifest sha256).
#[cfg(not(target_os = "windows"))]
fn bundled_tmux_conf() -> Result<(Vec<u8>, String), String> {
    let manifest = crate::remote_bootstrap::embedded_manifest()?;
    let entry = manifest
        .get("tmux-conf")
        .ok_or_else(|| "manifest missing tmux-conf".to_string())?;
    let bytes = crate::remote_bootstrap::embedded_payload(&entry.path)?;
    Ok((bytes, entry.sha256.clone()))
}

/// Where the local (non-WSL) tmux reads our conf from — the same
/// `~/.ymux/tmux.conf` the WSL/remote deploys use.
#[cfg(not(target_os = "windows"))]
pub(crate) fn local_tmux_conf_path() -> PathBuf {
    home_dir().join(".ymux").join("tmux.conf")
}

/// True when a local tmux.conf is in place for `-f $HOME/.ymux/tmux.conf`.
///
/// winmux -> ymux rename: a Mac provisioned by a pre-rename build has the
/// file under `~/.winmux`, and nothing migrates it locally — the CLI's
/// `migrate_legacy_home_dir` runs on the *remote*. Fold it over once here
/// rather than dual-reading: `build_tmux_attach_script` hardcodes the
/// `.ymux` path (it is shared with the WSL/SSH deploys), so merely
/// *detecting* the legacy file would point tmux at one that isn't there.
/// Copy, not rename, so a downgrade still finds its own conf.
#[cfg(not(target_os = "windows"))]
pub(crate) fn local_tmux_conf_ready() -> bool {
    let current = local_tmux_conf_path();
    if current.is_file() {
        return true;
    }
    let legacy = home_dir().join(".winmux").join("tmux.conf");
    if !legacy.is_file() {
        return false;
    }
    let Some(dir) = current.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    match std::fs::copy(&legacy, &current) {
        Ok(_) => {
            crate::log_info(
                "SETUP",
                &format!("folded legacy {} into {}", legacy.display(), current.display()),
            );
            true
        }
        Err(e) => {
            crate::log_warn("SETUP", &format!("copy {} failed: {e}", legacy.display()));
            false
        }
    }
}

/// True when `~/.ymux/tmux.conf` exists and is byte-identical to the
/// bundled conf. Byte compare instead of a sha: both sides are in memory.
#[cfg(not(target_os = "windows"))]
fn local_tmux_conf_ok() -> bool {
    let Ok((want, _)) = bundled_tmux_conf() else {
        return false;
    };
    std::fs::read(local_tmux_conf_path())
        .map(|have| have == want)
        .unwrap_or(false)
}

/// Write the bundled tmux.conf to `~/.ymux/tmux.conf`. Atomic in the
/// Rule #7 spirit: `.tmp` sibling then rename, so a crash mid-write
/// never leaves tmux reading half a conf.
#[cfg(not(target_os = "windows"))]
fn deploy_tmux_conf_local() -> Result<String, String> {
    let (bytes, sha) = bundled_tmux_conf()?;
    let dest = local_tmux_conf_path();
    let dir = dest
        .parent()
        .ok_or_else(|| "tmux.conf path has no parent dir".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let tmp = dest.with_file_name("tmux.conf.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename into {}: {e}", dest.display()));
    }
    Ok(format!(
        "wrote {} ({} bytes, sha256 {sha})",
        dest.display(),
        bytes.len()
    ))
}

// ─── Install engine ──────────────────────────────────────────────────

#[derive(Clone, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct LocalSetupInput {
    /// LocalStepKind names, in execution order (the wizard sends only
    /// the needed ones; unknown names fail their step, not the run).
    pub steps: Vec<String>,
    #[serde(default)]
    pub distro: Option<String>,
    #[serde(default)]
    pub wsl_username: Option<String>,
    #[serde(default)]
    pub workspace_name: Option<String>,
    #[serde(default)]
    pub create_workspace: bool,
    #[serde(default)]
    pub workspace_cwd: Option<String>,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct LocalSetupResult {
    pub run_id: String,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
    /// Steps that errored, in run order. The done screen must not claim
    /// success while this is non-empty — a failed WSL chain silently
    /// skips workspace creation, which used to render as a clean finish.
    pub failed_steps: Vec<String>,
    /// Chain steps abandoned because an earlier chain step failed.
    pub skipped_steps: Vec<String>,
    /// True when the WSL chain ran and came out clean. When false with a
    /// non-empty `failed_steps`, no WSL workspace exists.
    /// macOS: same flag, but the "chain" is the tmux persistence chain
    /// (`InstallTmuxLocal` → `DeployTmuxConfLocal`) — true means it ran
    /// clean; a Local workspace is still created when no chain step was
    /// requested at all.
    pub wsl_chain_ok: bool,
}

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
fn new_run_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("local_{t:x}_{n:x}")
}

fn emit_step(
    app: &AppHandle,
    run_id: &str,
    idx: usize,
    kind: &str,
    state: &'static str,
    log_chunk: String,
    message: Option<String>,
    error: Option<ProvisioningError>,
) {
    let _ = app.emit(
        "local-setup:progress",
        StepProgress {
            run_id: run_id.to_string(),
            step_index: idx,
            step_kind: kind.to_string(),
            state,
            log_chunk,
            message,
            error,
            timestamp_iso: crate::provisioning::iso_now(),
        },
    );
}

/// Spawn a local-setup run. Emits `local-setup:progress` per step and a
/// final `local-setup:complete` (carrying the created workspace id when
/// the WSL chain succeeded and `create_workspace` was requested).
#[tauri::command]
pub(crate) async fn local_setup_start(
    state: State<'_, AppState>,
    app: AppHandle,
    input: LocalSetupInput,
) -> Result<RunHandle, String> {
    let run_id = new_run_id();
    let run_id_clone = run_id.clone();
    let state_for_task: AppState = (*state).clone();
    tauri::async_runtime::spawn(async move {
        run_local_setup(app, state_for_task, run_id_clone, input).await;
    });
    Ok(RunHandle { run_id })
}

/// Sanitize a Linux username: lowercase, [a-z0-9-], must start with a
/// letter, max 32 chars. Falls back to "ymux" when nothing survives.
#[cfg(target_os = "windows")]
fn sanitize_linux_username(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.to_lowercase().chars() {
        if out.is_empty() {
            if c.is_ascii_lowercase() {
                out.push(c);
            }
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            out.push(c);
        }
        if out.len() >= 32 {
            break;
        }
    }
    if out.is_empty() {
        "ymux".into()
    } else {
        out
    }
}

/// Pipe embedded bytes into a file inside the distro (default user's
/// home), then sha-verify + atomically rename into place. stdin-pipe is
/// the transport — `\\wsl$` UNC is flaky on cold distros and wrong for
/// non-default users.
#[cfg(target_os = "windows")]
async fn wsl_pipe_to_file(
    distro: Option<&str>,
    bytes: &[u8],
    dest_rel: &str, // path under $HOME, pre-quoted here
    sha256: &str,
    mode: &str,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let tmp_name = format!("{dest_rel}.ymux-upload.tmp");
    // Stage 1: cat stdin into the temp file. The script only embeds
    // shell_quote()d values (Rule #3 discipline).
    let cat_script = format!(
        "mkdir -p \"$(dirname \"$HOME\"/{d})\" && cat > \"$HOME\"/{t}",
        d = shell_quote(dest_rel),
        t = shell_quote(&tmp_name)
    );
    let mut c = wsl_cmd();
    if let Some(d) = distro {
        if !d.is_empty() {
            c.arg("-d").arg(d);
        }
    }
    c.arg("--").arg("sh").arg("-c").arg(&cat_script);
    c.stdin(std::process::Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("wsl.exe spawn: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "wsl stdin unavailable".to_string())?;
        stdin
            .write_all(bytes)
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
        stdin.shutdown().await.map_err(|e| format!("stdin close: {e}"))?;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| "upload timed out".to_string())?
        .map_err(|e| format!("wsl wait: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "upload failed: {}",
            clean_wsl_output(&out.stderr)
        ));
    }
    // Stage 2: verify + chmod + atomic rename.
    let fin_script = format!(
        "S=$(sha256sum \"$HOME\"/{t} | cut -d' ' -f1); \
         if [ \"$S\" = {sha} ]; then chmod {mode} \"$HOME\"/{t} && mv -f \"$HOME\"/{t} \"$HOME\"/{d} && echo OK; \
         else echo \"SHA MISMATCH got $S\"; rm -f \"$HOME\"/{t}; exit 1; fi",
        t = shell_quote(&tmp_name),
        d = shell_quote(dest_rel),
        sha = shell_quote(sha256),
        mode = mode
    );
    let (code, out) = wsl_exec(distro, None, &fin_script).await?;
    if code != 0 {
        return Err(format!("finalize failed: {out}"));
    }
    Ok(out)
}

/// True when winget's exit code means "nothing to do" rather than a
/// real failure (0x8A15002B: no applicable installer/upgrade found —
/// typically "already installed").
#[cfg(target_os = "windows")]
fn winget_already_ok(code: i32) -> bool {
    code == 0x8A15_002Bu32 as i32
}

#[cfg(target_os = "windows")]
async fn winget_install(id: &str) -> Result<(i32, String), String> {
    let mut c = hidden_cmd("winget");
    c.args([
        "install",
        "--id",
        id,
        "-e",
        "--source",
        "winget",
        "--accept-source-agreements",
        "--accept-package-agreements",
        "--disable-interactivity",
    ]);
    run_capture(c, "winget install", 900).await
}

async fn npm_install_global(pkg: &str) -> Result<(i32, String), String> {
    let npm = probe_tool(NPM_CANDIDATES, &canonical_npm()).await;
    let Some(path) = npm.path else {
        return Err("npm not found — enable the Node.js step first".into());
    };
    let mut c = hidden_cmd(&path);
    c.args(["install", "-g", pkg]);
    // npm is a node script: without node on the child's PATH it dies at
    // the shebang from a Finder-launched app.
    #[cfg(not(target_os = "windows"))]
    {
        c.env("PATH", unix_path_env());
    }
    run_capture(c, "npm install -g", 600).await
}

/// File name of the bundled host CLI under the Tauri resource dir.
const YMUX_CLI_NAME: &str = if cfg!(target_os = "windows") {
    "ymux-cli.exe"
} else {
    "ymux-cli"
};

/// Resolve the bundled host CLI (ymux-cli.exe / ymux-cli). Installed
/// builds carry it under the Tauri resource dir; dev builds fall back to
/// the path the build script stages it at. None on a build that does not
/// bundle it (the macOS bundle does not yet — see tauri.macos.conf.json).
fn resolve_ymux_cli(app: &AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    let candidates = app
        .path()
        .resource_dir()
        .ok()
        .into_iter()
        .flat_map(|r| [r.join("resources").join(YMUX_CLI_NAME), r.join(YMUX_CLI_NAME)])
        .collect::<Vec<_>>();
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

// ─── unix step runner (macOS first, Linux shares) ────────────────────

/// Map a captured (exit_code, output) onto the step result the loop
/// expects: 0 → done with the output as the log, else StepFailed.
#[cfg(not(target_os = "windows"))]
fn step_outcome(kind: &str, r: Result<(i32, String), String>) -> Result<String, ProvisioningError> {
    match r {
        Ok((0, out)) => Ok(out),
        Ok((code, out)) => Err(ProvisioningError::StepFailed {
            step: kind.into(),
            exit_code: code,
            stderr: out,
        }),
        Err(e) => Err(ProvisioningError::Generic(e)),
    }
}

/// `brew install <formula>` through the resolved brew binary. Homebrew
/// itself is never installed here — its installer wants sudo and a TTY,
/// neither of which a GUI app can offer; the user gets a clear pointer.
/// `brew install` of an already-installed formula exits 0 with a
/// warning, so no winget-style "already ok" code is needed.
#[cfg(not(target_os = "windows"))]
async fn brew_install(kind: &str, formula: &str) -> Result<String, ProvisioningError> {
    let brew = probe_tool(&["brew"], &canonical_brew()).await;
    let Some(path) = brew.path else {
        return Err(ProvisioningError::Generic(
            "Homebrew not installed — install it from https://brew.sh, then re-run this step"
                .into(),
        ));
    };
    let mut c = hidden_cmd(&path);
    c.args(["install", formula]);
    // No `brew update` first: it can take minutes and needs nothing we
    // ship. PATH so brew's own children (git, curl, tar) resolve from a
    // Finder-launched app.
    c.env("HOMEBREW_NO_AUTO_UPDATE", "1");
    c.env("HOMEBREW_NO_INSTALL_CLEANUP", "1");
    c.env("PATH", unix_path_env());
    step_outcome(kind, run_capture(c, "brew install", 900).await)
}

/// One local-setup step on macOS/Linux. Same step-kind vocabulary as
/// the Windows arms where the tool is the same (git/node/claude/codex/
/// gemini/hooks); `InstallTmuxLocal` + `DeployTmuxConfLocal` replace
/// the WSL chain. WSL kinds fail loudly — the mac UI never sends them.
#[cfg(not(target_os = "windows"))]
async fn run_step_unix(app: &AppHandle, kind: &str) -> Result<String, ProvisioningError> {
    match kind {
        "InstallGit" => brew_install(kind, "git").await,
        "InstallNodejs" => brew_install(kind, "node").await,
        "InstallTmuxLocal" => brew_install(kind, "tmux").await,
        "InstallClaudeCode" => {
            // Official native installer (verified against
            // code.claude.com/docs/en/setup) — no sudo, auto-updates,
            // lands at ~/.local/bin/claude. Static command string:
            // Rule #3 satisfied (mirrors the Windows install.ps1 arm).
            let mut c = hidden_cmd("/bin/bash");
            c.args(["-c", "curl -fsSL https://claude.ai/install.sh | bash"]);
            c.env("PATH", unix_path_env());
            let installed = || {
                canonical_claude().iter().any(|p| p.is_file())
                    || crate::local_wizard::which("claude").is_some()
            };
            match run_capture(c, "claude install", 600).await {
                Ok((0, out)) => {
                    if installed() {
                        Ok(out)
                    } else {
                        Err(ProvisioningError::StepFailed {
                            step: kind.into(),
                            exit_code: 0,
                            stderr: format!(
                                "installer exited 0 but claude was not found\n{out}"
                            ),
                        })
                    }
                }
                // Mirrors the Windows arm: a failed installer with a working
                // claude already on disk (auto-updater race on ~/.claude)
                // still means the step's goal is met.
                Ok((_, out)) if installed() => Ok(out),
                other => step_outcome(kind, other),
            }
        }
        "InstallCodex" => step_outcome(kind, npm_install_global("@openai/codex").await),
        "InstallGemini" => {
            step_outcome(kind, npm_install_global("@google/gemini-cli@latest").await)
        }
        "InstallLocalHooks" => match resolve_ymux_cli(app) {
            Some(cli) => {
                let mut c = hidden_cmd(&cli.to_string_lossy());
                c.args(["setup-hooks", "--agent", "claude", "--source", "bundled"]);
                c.env("PATH", unix_path_env());
                step_outcome(kind, run_capture(c, "setup-hooks", 120).await)
            }
            None => Err(ProvisioningError::Generic(
                "winmux CLI is not bundled in the macOS build yet — hooks unavailable".into(),
            )),
        },
        "DeployTmuxConfLocal" => deploy_tmux_conf_local().map_err(ProvisioningError::Generic),
        "InstallWsl"
        | "CreateWslUser"
        | "EnsureDistroReady"
        | "InstallTmuxInWsl"
        | "DeployYmuxCliToWsl"
        | "DeployTmuxConfToWsl"
        | "InstallHooksInWsl" => Err(ProvisioningError::StepFailed {
            step: kind.into(),
            exit_code: -1,
            stderr: format!("{kind} is a Windows/WSL step — not available on macOS"),
        }),
        other => Err(ProvisioningError::Generic(format!(
            "unknown local step {other:?}"
        ))),
    }
}

/// Windows chain: the WSL steps. Each assumes the distro state the
/// previous one produced, so one failure invalidates all later ones.
// Pure + unit-tested on every OS; only the Windows loop consults it.
#[allow(dead_code)]
pub(crate) fn is_wsl_chain(kind: &str) -> bool {
    matches!(
        kind,
        "InstallWsl"
            | "CreateWslUser"
            | "EnsureDistroReady"
            | "InstallTmuxInWsl"
            | "DeployYmuxCliToWsl"
            | "DeployTmuxConfToWsl"
            | "InstallClaudeCodeInWsl"
            // Was missing: it ran against a non-existent distro after
            // a broken chain and — being the last card — ended the
            // run on a green check.
            | "InstallHooksInWsl"
    )
}

/// macOS chain: the tmux persistence steps. The workspace only survives
/// an app restart once tmux is installed AND our conf is in place, so a
/// failed install makes deploying the conf pointless.
#[allow(dead_code)]
pub(crate) fn is_persistence_chain(kind: &str) -> bool {
    matches!(kind, "InstallTmuxLocal" | "DeployTmuxConfLocal")
}

/// The chain this OS runs — see `is_wsl_chain` / `is_persistence_chain`.
fn is_chain_step(kind: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        is_wsl_chain(kind)
    }
    #[cfg(not(target_os = "windows"))]
    {
        is_persistence_chain(kind)
    }
}

#[cfg(target_os = "windows")]
const CHAIN_SKIPPED_MSG: &str = "skipped — an earlier WSL step failed";
#[cfg(not(target_os = "windows"))]
const CHAIN_SKIPPED_MSG: &str = "skipped — an earlier tmux step failed";

async fn run_local_setup(app: AppHandle, state: AppState, run_id: String, input: LocalSetupInput) {
    // The distro the WSL-chain steps target. A fresh `wsl --install`
    // registers Ubuntu; an existing setup passes the inspect's default.
    #[cfg(target_os = "windows")]
    let mut distro: Option<String> = input.distro.clone().filter(|s| !s.is_empty());
    // Chain health (WSL chain on Windows, tmux persistence chain on
    // macOS) — decides whether we finalize a workspace.
    let mut chain_failed = false;
    let mut ran_chain = false;
    let mut failed_steps: Vec<String> = Vec::new();
    let mut skipped_steps: Vec<String> = Vec::new();

    for (idx, step) in input.steps.iter().enumerate() {
        let kind = step.as_str();
        emit_step(&app, &run_id, idx, kind, "running", String::new(), None, None);
        let is_chain = is_chain_step(kind);
        if is_chain {
            ran_chain = true;
            // A broken chain makes every later chain step meaningless —
            // skip them explicitly instead of cascading noise.
            if chain_failed {
                skipped_steps.push(kind.to_string());
                emit_step(
                    &app,
                    &run_id,
                    idx,
                    kind,
                    "skipped",
                    String::new(),
                    Some(CHAIN_SKIPPED_MSG.into()),
                    None,
                );
                continue;
            }
        }
        #[cfg(not(target_os = "windows"))]
        let result: Result<String, ProvisioningError> = run_step_unix(&app, kind).await;
        #[cfg(target_os = "windows")]
        let result: Result<String, ProvisioningError> = match kind {
            "InstallGit" => match winget_install("Git.Git").await {
                Ok((code, out)) if code == 0 || winget_already_ok(code) => Ok(out),
                Ok((code, out)) => Err(ProvisioningError::StepFailed {
                    step: kind.into(),
                    exit_code: code,
                    stderr: out,
                }),
                Err(e) => Err(ProvisioningError::Generic(e)),
            },
            // 2026-08-19: the official upstream package — v0.44.3 really does
            // ship windows-msvc artifacts, so this is NOT the third-party
            // `arndawg.zellij-windows` fork. Same winget shape as InstallGit;
            // winget_already_ok covers a re-run on a machine that has it.
            "InstallZellij" => match winget_install("Zellij.Zellij").await {
                Ok((code, out)) if code == 0 || winget_already_ok(code) => Ok(out),
                Ok((code, out)) => Err(ProvisioningError::StepFailed {
                    step: kind.into(),
                    exit_code: code,
                    stderr: out,
                }),
                Err(e) => Err(ProvisioningError::Generic(e)),
            },
            "InstallNodejs" => match winget_install("OpenJS.NodeJS.LTS").await {
                Ok((code, out)) if code == 0 || winget_already_ok(code) => Ok(out),
                Ok((code, out)) => Err(ProvisioningError::StepFailed {
                    step: kind.into(),
                    exit_code: code,
                    stderr: out,
                }),
                Err(e) => Err(ProvisioningError::Generic(e)),
            },
            "InstallClaudeCode" => {
                // Official native installer (verified against
                // code.claude.com/docs/en/setup) — no admin, auto-updates,
                // lands at %USERPROFILE%\.local\bin\claude.exe. Static
                // command string: Rule #3 satisfied.
                let mut c = hidden_cmd("powershell");
                c.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "irm https://claude.ai/install.ps1 | iex",
                ]);
                let installed = || {
                    canonical_claude().iter().any(|p| p.is_file())
                        || crate::local_wizard::which("claude.exe").is_some()
                };
                match run_capture(c, "claude install", 600).await {
                    Ok((0, out)) => {
                        if installed() {
                            Ok(out)
                        } else {
                            Err(ProvisioningError::StepFailed {
                                step: kind.into(),
                                exit_code: 0,
                                stderr: format!(
                                    "installer exited 0 but claude.exe was not found\n{out}"
                                ),
                            })
                        }
                    }
                    // The installer shares ~\.claude\downloads with claude's
                    // own auto-updater and with any earlier (possibly
                    // orphaned) install attempt; a locked download file fails
                    // it even when a working claude.exe is already on disk —
                    // in that case the step's goal is met.
                    Ok((code, out)) => {
                        if installed() {
                            Ok(out)
                        } else {
                            Err(ProvisioningError::StepFailed {
                                step: kind.into(),
                                exit_code: code,
                                stderr: out,
                            })
                        }
                    }
                    Err(e) => Err(ProvisioningError::Generic(e)),
                }
            }
            "InstallCodex" => match npm_install_global("@openai/codex").await {
                Ok((0, out)) => Ok(out),
                Ok((code, out)) => Err(ProvisioningError::StepFailed {
                    step: kind.into(),
                    exit_code: code,
                    stderr: out,
                }),
                Err(e) => Err(ProvisioningError::Generic(e)),
            },
            "InstallGemini" => match npm_install_global("@google/gemini-cli@latest").await {
                Ok((0, out)) => Ok(out),
                Ok((code, out)) => Err(ProvisioningError::StepFailed {
                    step: kind.into(),
                    exit_code: code,
                    stderr: out,
                }),
                Err(e) => Err(ProvisioningError::Generic(e)),
            },
            "InstallLocalHooks" => match resolve_ymux_cli(&app) {
                Some(cli) => {
                    let mut c = hidden_cmd(&cli.to_string_lossy());
                    c.args(["setup-hooks", "--agent", "claude", "--source", "bundled"]);
                    match run_capture(c, "setup-hooks", 120).await {
                        Ok((0, out)) => Ok(out),
                        Ok((code, out)) => Err(ProvisioningError::StepFailed {
                            step: kind.into(),
                            exit_code: code,
                            stderr: out,
                        }),
                        Err(e) => Err(ProvisioningError::Generic(e)),
                    }
                }
                None => Err(ProvisioningError::Generic(
                    "bundled ymux-cli.exe not found (dev build without staged resources?)"
                        .into(),
                )),
            },
            "InstallWsl" => {
                let d = distro.clone().unwrap_or_else(|| "Ubuntu".into());
                if !valid_distro_name(&d) {
                    Err(ProvisioningError::Generic(format!(
                        "unsafe distro name: {d:?}"
                    )))
                } else {
                    // Enabling the Windows features needs admin. When we
                    // already hold an elevated token, run wsl.exe inline;
                    // otherwise drive it through a UAC prompt. Both paths
                    // return the real output — the elevated one via a temp
                    // file, since ShellExecuteEx has no pipes to give us.
                    let outcome = if is_elevated() {
                        let mut c = wsl_cmd();
                        c.args(["--install", "--no-launch", "-d", &d]);
                        match run_capture(c, "wsl --install", 1800).await {
                            Ok((code, out)) => Ok((code.max(0) as u32, out)),
                            Err(e) => Err(ProvisioningError::Generic(e)),
                        }
                    } else {
                        log_info(
                            "SETUP",
                            "InstallWsl: no elevated token — requesting UAC consent",
                        );
                        let d2 = d.clone();
                        match tokio::task::spawn_blocking(move || {
                            run_wsl_install_elevated(&d2)
                        })
                        .await
                        {
                            Ok(Ok((code, out))) => {
                                // Log it here rather than only on failure:
                                // a 3010 or a partial success is just as
                                // worth having in debug.log after the fact.
                                // Rule #1 does not apply — this is
                                // wsl.exe's own installer output, not PTY
                                // content.
                                log_info(
                                    "SETUP",
                                    &format!(
                                        "InstallWsl: elevated wsl --install exited {code}{}",
                                        if out.is_empty() {
                                            " (no output captured)".to_string()
                                        } else {
                                            format!(" — {out}")
                                        }
                                    ),
                                );
                                Ok((code, out))
                            }
                            Ok(Err(e)) => Err(e),
                            Err(e) => Err(ProvisioningError::Generic(format!(
                                "elevated install task failed: {e}"
                            ))),
                        }
                    };
                    match outcome {
                        Ok((code, out)) => match classify_wsl_install(code, &out) {
                            Ok(()) => {
                                distro = Some(d);
                                Ok(out)
                            }
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
            }
            "CreateWslUser" => {
                // Bypass Ubuntu's interactive OOBE entirely: create the
                // uid-1000 user as root + set it as the wsl.conf default,
                // then terminate the distro so the default applies.
                let user = sanitize_linux_username(
                    input
                        .wsl_username
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            std::env::var("USERNAME").unwrap_or_else(|_| "ymux".into())
                        })
                        .as_str(),
                );
                // Two things this script must NOT do, both learned the hard
                // way on 2026-08-10 (see PROGRESS):
                //
                // 1. Treat the presence of a `[user]` section as proof that
                //    a default user is set. A section with an empty
                //    `default=` is exactly the broken state we have to
                //    repair — WSL then calls getpwnam("") on every single
                //    invocation, fails, and silently falls back to root, so
                //    every later step runs with HOME=/root.
                // 2. Let a failed `useradd` pass. The old chain ended with a
                //    second `if`, whose exit status became the script's, so
                //    a failure to create the user was swallowed and the step
                //    reported success — the same class of bug 6753759 fixed
                //    for the install itself.
                //
                // `set -e` plus an explicit verify at the end means the step
                // fails loudly if the user or the default is not actually in
                // place when it returns.
                let script = format!(
                    "set -e; U={u}; \
                     if getent passwd 1000 >/dev/null 2>&1; then U=\"$(id -nu 1000)\"; echo \"EXISTS $U\"; \
                     else useradd -m -u 1000 -s /bin/bash \"$U\"; \
                          (usermod -aG sudo \"$U\" 2>/dev/null || usermod -aG wheel \"$U\" 2>/dev/null || true); \
                          echo \"CREATED $U\"; fi; \
                     [ -n \"$U\" ] || {{ echo 'could not determine the linux username'; exit 1; }}; \
                     touch /etc/wsl.conf; \
                     CUR=\"$(sed -n 's/^[[:space:]]*default[[:space:]]*=[[:space:]]*//p' /etc/wsl.conf | tail -n1)\"; \
                     if [ \"$CUR\" = \"$U\" ]; then echo \"wsl.conf default already $U\"; \
                     elif [ -n \"$CUR\" ]; then echo \"wsl.conf default is '$CUR', leaving it\"; U=\"$CUR\"; \
                     else \
                       sed -i '/^[[:space:]]*default[[:space:]]*=/d' /etc/wsl.conf; \
                       if grep -q '^[[:space:]]*\\[user\\]' /etc/wsl.conf; then \
                         sed -i \"0,/^[[:space:]]*\\\\[user\\\\]/s//[user]\\\\ndefault=$U/\" /etc/wsl.conf; \
                       else printf '\\n[user]\\ndefault=%s\\n' \"$U\" >> /etc/wsl.conf; fi; \
                       echo \"wsl.conf default set to $U\"; fi; \
                     getent passwd \"$U\" >/dev/null || {{ echo \"user $U does not exist after setup\"; exit 1; }}; \
                     FIN=\"$(sed -n 's/^[[:space:]]*default[[:space:]]*=[[:space:]]*//p' /etc/wsl.conf | tail -n1)\"; \
                     [ -n \"$FIN\" ] || {{ echo 'wsl.conf still has no default user'; exit 1; }}; \
                     echo \"VERIFIED user=$U default=$FIN\"",
                    u = shell_quote(&user)
                );
                match tokio::time::timeout(
                    std::time::Duration::from_secs(180),
                    wsl_exec(distro.as_deref(), Some("root"), &script),
                )
                .await
                {
                    Ok(Ok((0, out))) => {
                        // Apply the default-user change.
                        if let Some(d) = distro.as_deref() {
                            let mut c = wsl_cmd();
                            c.args(["--terminate", d]);
                            let _ = run_capture(c, "wsl --terminate", 30).await;
                        }
                        Ok(out)
                    }
                    Ok(Ok((code, out))) => Err(ProvisioningError::StepFailed {
                        step: kind.into(),
                        exit_code: code,
                        stderr: out,
                    }),
                    Ok(Err(e)) => Err(ProvisioningError::Generic(e)),
                    Err(_) => Err(ProvisioningError::Generic(
                        "CreateWslUser timed out".into(),
                    )),
                }
            }
            "EnsureDistroReady" => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(180),
                    wsl_exec(distro.as_deref(), None, "echo READY"),
                )
                .await
                {
                    Ok(Ok((0, out))) if out.contains("READY") => Ok(out),
                    Ok(Ok((code, out))) => Err(ProvisioningError::StepFailed {
                        step: kind.into(),
                        exit_code: code,
                        stderr: out,
                    }),
                    Ok(Err(e)) => Err(ProvisioningError::Generic(e)),
                    Err(_) => Err(ProvisioningError::Generic(
                        "distro did not become ready within 180s (first boot can be slow — retry)"
                            .into(),
                    )),
                }
            }
            "InstallTmuxInWsl" => {
                // `-u root` sidesteps sudo/password entirely (WSL allows
                // it by design) — no SudoRequired equivalent needed.
                let script = "if command -v tmux >/dev/null 2>&1; then echo 'tmux already installed'; \
                              elif command -v apt-get >/dev/null 2>&1; then apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y tmux; \
                              elif command -v dnf >/dev/null 2>&1; then dnf -y install tmux; \
                              elif command -v yum >/dev/null 2>&1; then yum -y install tmux; \
                              elif command -v apk >/dev/null 2>&1; then apk add tmux; \
                              else echo 'no known package manager'; exit 1; fi";
                match tokio::time::timeout(
                    std::time::Duration::from_secs(600),
                    wsl_exec(distro.as_deref(), Some("root"), script),
                )
                .await
                {
                    Ok(Ok((0, out))) => Ok(out),
                    Ok(Ok((code, out))) => Err(ProvisioningError::StepFailed {
                        step: kind.into(),
                        exit_code: code,
                        stderr: out,
                    }),
                    Ok(Err(e)) => Err(ProvisioningError::Generic(e)),
                    Err(_) => Err(ProvisioningError::Generic(
                        "tmux install timed out (slow mirror?) — retry".into(),
                    )),
                }
            }
            "DeployYmuxCliToWsl" => {
                if std::env::consts::ARCH != "x86_64" {
                    Err(ProvisioningError::Generic(format!(
                        "unsupported host arch {} — only x86_64 WSL deploys are bundled",
                        std::env::consts::ARCH
                    )))
                } else {
                    let deploy = async {
                        let manifest = crate::remote_bootstrap::embedded_manifest()?;
                        let entry = manifest
                            .get("x86_64-linux")
                            .ok_or_else(|| "manifest missing x86_64-linux".to_string())?;
                        let bytes = crate::remote_bootstrap::embedded_payload(&entry.path)?;
                        let mut log = wsl_pipe_to_file(
                            distro.as_deref(),
                            &bytes,
                            ".ymux/bin/ymux-linux-x64",
                            &entry.sha256,
                            "0755",
                        )
                        .await?;
                        let (code, out) = wsl_exec(
                            distro.as_deref(),
                            None,
                            "ln -sf \"$HOME/.ymux/bin/ymux-linux-x64\" \"$HOME/.ymux/bin/ymux\" && echo SYMLINK-OK",
                        )
                        .await?;
                        if code != 0 {
                            return Err(format!("symlink failed: {out}"));
                        }
                        log.push('\n');
                        log.push_str(&out);
                        // PATH rc snippet — the same idempotent script the
                        // remote bootstrap runs; safe as a single argv.
                        let (code, out) = wsl_exec(
                            distro.as_deref(),
                            None,
                            crate::remote_bootstrap::PATH_RC_SNIPPET,
                        )
                        .await?;
                        if code != 0 {
                            return Err(format!("PATH setup failed: {out}"));
                        }
                        log.push('\n');
                        log.push_str(&out);
                        Ok::<String, String>(log)
                    };
                    match deploy.await {
                        Ok(out) => Ok(out),
                        Err(e) => Err(ProvisioningError::Generic(e)),
                    }
                }
            }
            "DeployTmuxConfToWsl" => {
                let deploy = async {
                    let manifest = crate::remote_bootstrap::embedded_manifest()?;
                    let entry = manifest
                        .get("tmux-conf")
                        .ok_or_else(|| "manifest missing tmux-conf".to_string())?;
                    let bytes = crate::remote_bootstrap::embedded_payload(&entry.path)?;
                    wsl_pipe_to_file(
                        distro.as_deref(),
                        &bytes,
                        ".ymux/tmux.conf",
                        &entry.sha256,
                        "0644",
                    )
                    .await
                };
                match deploy.await {
                    Ok(out) => Ok(out),
                    Err(e) => Err(ProvisioningError::Generic(e)),
                }
            }
            "InstallClaudeCodeInWsl" => {
                // The gap Yossi hit: the chain installed tmux, the CLI, the
                // tmux conf and ymux's Claude HOOKS into the distro — but
                // never Claude Code itself. `claude: command not found` in a
                // brand-new WSL pane, with hooks registered for an agent that
                // was not there.
                //
                // provisioning.rs:823 already does this for a remote host with
                // the official installer; this is the same thing one transport
                // over. Not `-u root`: the installer puts its binary and state
                // under $HOME, and root's $HOME is not the user's.
                //
                // curl is not guaranteed on a minimal image (Ubuntu's WSL
                // rootfs has it, other distros may not), so ensure it as root
                // first — same package-manager ladder as InstallTmuxInWsl.
                let ensure_curl = "if command -v curl >/dev/null 2>&1; then echo 'curl present'; \
                                   elif command -v apt-get >/dev/null 2>&1; then apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y curl; \
                                   elif command -v dnf >/dev/null 2>&1; then dnf -y install curl; \
                                   elif command -v yum >/dev/null 2>&1; then yum -y install curl; \
                                   elif command -v apk >/dev/null 2>&1; then apk add curl; \
                                   else echo 'no known package manager'; exit 1; fi";
                // Idempotent: an existing install short-circuits, so re-running
                // the wizard costs one `command -v`.
                let install = "if command -v claude >/dev/null 2>&1; then echo \"claude already installed: $(command -v claude)\"; exit 0; fi; \
                               curl -fsSL https://claude.ai/install.sh | bash; \
                               for d in \"$HOME/.local/bin\" \"$HOME/.claude/bin\"; do [ -d \"$d\" ] && PATH=\"$d:$PATH\"; done; \
                               if command -v claude >/dev/null 2>&1; then echo \"claude installed: $(command -v claude)\"; \
                               else echo 'installer finished but claude is still not on PATH'; exit 1; fi";
                let run = async {
                    let (code, out) =
                        wsl_exec(distro.as_deref(), Some("root"), ensure_curl).await?;
                    if code != 0 {
                        return Ok::<_, String>((code, out));
                    }
                    wsl_exec(distro.as_deref(), None, install).await
                };
                // The installer downloads a release; 10 minutes matches the
                // tmux step's allowance for a slow mirror.
                match tokio::time::timeout(std::time::Duration::from_secs(600), run).await {
                    Ok(Ok((0, out))) => Ok(out),
                    Ok(Ok((code, out))) => Err(ProvisioningError::StepFailed {
                        step: kind.into(),
                        exit_code: code,
                        stderr: out,
                    }),
                    Ok(Err(e)) => Err(ProvisioningError::Generic(e)),
                    Err(_) => Err(ProvisioningError::Generic(
                        "Claude Code install timed out — check the distro's network and retry"
                            .into(),
                    )),
                }
            }
            "InstallHooksInWsl" => {
                // Best-effort, mirroring the remote bootstrap's posture —
                // a distro without ~/.claude just reports and moves on.
                let script = "\"$HOME/.ymux/bin/ymux\" setup-hooks --agent claude --source bundled 2>&1 || true";
                match tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    wsl_exec(distro.as_deref(), None, script),
                )
                .await
                {
                    // The script ends in `|| true`, so a distro without
                    // ~/.claude still exits 0 — best-effort posture kept.
                    // A non-zero code here means wsl.exe itself failed
                    // (e.g. no such distro), which is not best-effort.
                    Ok(Ok((0, out))) => Ok(out),
                    Ok(Ok((code, out))) => Err(ProvisioningError::StepFailed {
                        step: kind.into(),
                        exit_code: code,
                        stderr: out,
                    }),
                    Ok(Err(e)) => Err(ProvisioningError::Generic(e)),
                    Err(_) => Err(ProvisioningError::Generic("hooks install timed out".into())),
                }
            }
            other => Err(ProvisioningError::Generic(format!(
                "unknown local step {other:?}"
            ))),
        };
        match result {
            Ok(log) => {
                emit_step(&app, &run_id, idx, kind, "done", log, None, None);
            }
            Err(err) => {
                if is_chain {
                    chain_failed = true;
                }
                failed_steps.push(kind.to_string());
                let msg = err.user_message();
                // user_message() drops StepFailed's stderr — without it a
                // user-reported install failure is undiagnosable after the
                // fact (the UI card has it, the log did not).
                let detail = match &err {
                    ProvisioningError::StepFailed { stderr, .. } if !stderr.trim().is_empty() => {
                        format!(" — {}", stderr.trim())
                    }
                    _ => String::new(),
                };
                log_warn(
                    "SETUP",
                    &format!("local-setup[{run_id}] step {kind} failed: {msg}{detail}"),
                );
                emit_step(
                    &app,
                    &run_id,
                    idx,
                    kind,
                    "failed",
                    String::new(),
                    Some(msg),
                    Some(err),
                );
            }
        }
    }

    // Finalize: create the workspace when the chain came out clean.
    // Windows needs the WSL chain to have RUN (a WSL workspace without a
    // distro is useless); macOS creates a Local workspace whenever no
    // chain step failed — a run without the tmux steps is fine too.
    let chain_ok = ran_chain && !chain_failed;
    #[cfg(target_os = "windows")]
    let finalize = input.create_workspace && chain_ok;
    #[cfg(not(target_os = "windows"))]
    let finalize = input.create_workspace && !chain_failed;
    let mut result = LocalSetupResult {
        run_id: run_id.clone(),
        workspace_id: None,
        workspace_name: None,
        failed_steps: failed_steps.clone(),
        skipped_steps,
        wsl_chain_ok: chain_ok,
    };
    if !failed_steps.is_empty() {
        log_warn(
            "SETUP",
            &format!(
                "local-setup[{run_id}] finished with {} failed step(s): {} — workspace {}",
                failed_steps.len(),
                failed_steps.join(", "),
                if input.create_workspace { "created" } else { "NOT created" }
            ),
        );
    }
    // 2026-08-19: the wizard finishes by creating a real workspace to land
    // in. It used to require the WSL chain to have succeeded; the workspace
    // is now a native Windows one, so it depends on nothing but the request.
    // Phase 82: the same holds on macOS - the tmux chain only changes how
    // that workspace's panes survive a restart, not what it connects to.
    if input.create_workspace {
        let finalized = finalize_local_workspace(
            &state,
            &app,
            input.workspace_name.clone(),
            input.workspace_cwd.clone(),
        );
        match finalized {
            Ok((id, name)) => {
                result.workspace_id = Some(id);
                result.workspace_name = Some(name);
            }
            Err(e) => log_warn("SETUP", &format!("local-setup[{run_id}] finalize failed: {e}")),
        }
    }
    let _ = app.emit("local-setup:complete", result);
}

/// Create a fresh workspace with a single terminal pane on
/// `Connection::Local` - the local twin of provisioning::finalize_workspace
/// branch 2. Was `finalize_wsl_workspace` until 2026-08-19; the pane is a
/// native Windows shell now, and zellij gives it the persistence that used
/// to be the only reason to put it inside a distro. Phase 82: on macOS tmux
/// plays that role, and the connection type is the same either way.
fn finalize_local_workspace(
    state: &AppState,
    app: &AppHandle,
    workspace_name: Option<String>,
    cwd: Option<String>,
) -> Result<(String, String), String> {
    finalize_workspace(
        state,
        app,
        crate::Connection::Local { shell: None },
        "Local".into(),
        workspace_name,
        cwd,
    )
}

/// Shared tail of the two finalizers: build the workspace around `conn`,
/// persist (atomically, via `persist`), notify the UI. Returns (id, name).
fn finalize_workspace(
    state: &AppState,
    app: &AppHandle,
    conn: crate::Connection,
    default_name: String,
    workspace_name: Option<String>,
    cwd: Option<String>,
) -> Result<(String, String), String> {
    use crate::{new_pane_id, new_workspace_id, persist, LayoutNode, PaneKind, Workspace};

    let display_name = workspace_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_name);
    let pane_id = new_pane_id();
    let layout = LayoutNode::Pane {
        pane_id,
        pane_kind: PaneKind::Terminal,
        connection: Some(conn.clone()),
        browser: None,
        title: None,
        annotation: None,
        color: None,
        emoji: None,
        help_topic: None,
        diff_source: None,
        smart_bidi: None,
        auto_title: None,
    };
    let ws = Workspace {
        id: new_workspace_id(),
        name: display_name.clone(),
        color: Some(crate::provisioning::workspace_color_for_host(&display_name)),
        cwd: cwd.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        connection: Some(conn),
        layout: Some(layout),
        ..Default::default()
    };
    let id_out = ws.id.clone();
    {
        let mut file = state.workspaces.lock().unwrap();
        file.active_workspace_id = Some(id_out.clone());
        file.workspaces.push(ws);
    }
    persist(state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok((id_out, display_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_chain_is_exactly_the_mac_tmux_steps() {
        // The mac loop uses this to decide what a failed InstallTmuxLocal
        // must skip; a plain tool install must never be swept along.
        assert!(is_persistence_chain("InstallTmuxLocal"));
        assert!(is_persistence_chain("DeployTmuxConfLocal"));
        for k in [
            "InstallGit",
            "InstallNodejs",
            "InstallClaudeCode",
            "InstallCodex",
            "InstallGemini",
            "InstallLocalHooks",
            "InstallWsl",
            "DeployTmuxConfToWsl",
        ] {
            assert!(!is_persistence_chain(k), "{k} is not a persistence-chain step");
        }
    }

    #[test]
    fn wsl_chain_and_persistence_chain_are_disjoint() {
        for k in [
            "InstallWsl",
            "CreateWslUser",
            "EnsureDistroReady",
            "InstallTmuxInWsl",
            "DeployYmuxCliToWsl",
            "DeployTmuxConfToWsl",
            "InstallHooksInWsl",
        ] {
            assert!(is_wsl_chain(k), "{k} is a WSL-chain step");
            assert!(!is_persistence_chain(k), "{k} must not be a persistence-chain step");
        }
        for k in ["InstallTmuxLocal", "DeployTmuxConfLocal", "InstallGit"] {
            assert!(!is_wsl_chain(k), "{k} must not be a WSL-chain step");
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unix_path_env_prepends_the_canonical_dirs() {
        // A Finder-launched app inherits a bare PATH; brew/npm children
        // must still see Homebrew + ~/.local/bin first.
        let path = unix_path_env();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs.first(), Some(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        assert!(dirs.contains(&home_dir().join(".local").join("bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unix_canonical_covers_every_tool_dir() {
        let paths = unix_canonical("git");
        assert_eq!(paths.len(), unix_tool_dirs().len());
        assert!(paths.iter().all(|p| p.file_name().and_then(|n| n.to_str()) == Some("git")));
        assert!(canonical_claude().contains(&home_dir().join(".local").join("bin").join("claude")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn linux_username_sanitized() {
        assert_eq!(sanitize_linux_username("Yossi Hezkel"), "yossihezkel");
        assert_eq!(sanitize_linux_username("123abc"), "abc");
        assert_eq!(sanitize_linux_username("!!!"), "ymux");
        assert_eq!(sanitize_linux_username("a-b-c"), "a-b-c");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wsl_list_verbose_parses_default_marker() {
        let text = "  NAME            STATE           VERSION\n* Ubuntu          Running         2\n  docker-desktop  Stopped         2\n  Debian          Stopped         2\n";
        let (distros, default) = parse_wsl_list_verbose(text);
        assert_eq!(distros, vec!["Ubuntu".to_string(), "Debian".to_string()]);
        assert_eq!(default.as_deref(), Some("Ubuntu"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn winget_no_upgrade_code_is_ok() {
        assert!(winget_already_ok(0x8A15_002Bu32 as i32));
        assert!(!winget_already_ok(1));
    }
}

#[cfg(test)]
mod wsl_elevation_tests {
    // The old code decided "does this need admin?" by looking for the
    // English word "elevat" in wsl.exe's output, so a localized Windows
    // reported a UAC problem as a generic failure. These pin the
    // replacement: classify on the exit code first, text only as a
    // last-resort fallback for old builds that always return 1.
    use super::{classify_wsl_install, valid_distro_name};
    #[cfg(target_os = "windows")]
    use super::{cmd_safe_path, wsl_install_cmd_params};
    use crate::provisioning::ProvisioningError;

    #[test]
    fn success_is_success() {
        assert!(classify_wsl_install(0, "").is_ok());
    }

    #[test]
    fn code_3010_asks_for_a_restart_not_a_retry() {
        // The whole point of the variant: retrying without rebooting
        // cannot work, so it must not look like ElevationRequired.
        match classify_wsl_install(3010, "") {
            Err(ProvisioningError::RebootRequired { step, .. }) => assert_eq!(step, "InstallWsl"),
            other => panic!("expected RebootRequired, got {other:?}"),
        }
    }

    #[test]
    fn cancelled_uac_is_an_elevation_problem() {
        for code in [1223, 740] {
            match classify_wsl_install(code, "") {
                Err(ProvisioningError::ElevationRequired { .. }) => {}
                other => panic!("code {code} should be ElevationRequired, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_localized_failure_no_longer_masquerades_as_generic() {
        // Hebrew Windows: nothing in this string contains "elevat", which
        // is exactly how Yossi's run produced a bare StepFailed.
        let hebrew = "הפעולה דורשת הרשאות מנהל";
        match classify_wsl_install(1223, hebrew) {
            Err(ProvisioningError::ElevationRequired { .. }) => {}
            other => panic!("expected ElevationRequired from the code, got {other:?}"),
        }
    }

    #[test]
    fn unknown_code_still_carries_the_raw_output() {
        match classify_wsl_install(1, "something went wrong") {
            Err(ProvisioningError::StepFailed { exit_code, stderr, .. }) => {
                assert_eq!(exit_code, 1);
                assert_eq!(stderr, "something went wrong");
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
    }

    // ─── elevated-install output capture (the "exit 1 and nothing else"
    //     failure Yossi hit on 2026-08-09) ──────────────────────────────

    #[cfg(target_os = "windows")]
    #[test]
    fn cmd_safe_path_accepts_a_real_temp_path() {
        let p = std::path::Path::new(r"C:\Users\some.user\AppData\Local\Temp\ymux-wsl-1-2.log");
        assert_eq!(cmd_safe_path(p).as_deref(), p.to_str());
        // A space is legitimate in a Windows path and must survive — the
        // command string quotes the value, so it needs no escaping.
        let spaced = std::path::Path::new(r"C:\Documents and Settings\t\a.log");
        assert!(cmd_safe_path(spaced).is_some());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn cmd_safe_path_rejects_anything_cmd_could_reinterpret() {
        // Each of these would break out of, or corrupt, the redirect.
        for bad in [
            r#"C:\tmp\a"b.log"#,     // closes our quote
            r"C:\tmp\a&calc.log",    // command separator
            r"C:\tmp\a|b.log",       // pipe
            r"C:\tmp\a^b.log",       // cmd escape char
            r"C:\tmp\%TEMP%.log",    // variable expansion
            r"C:\tmp\a>b.log",       // extra redirect
            r"C:\tmp\a<b.log",
            r"C:\tmp\שלום.log",      // non-ASCII: cmd's codepage is not ours
            "",
        ] {
            assert!(
                cmd_safe_path(std::path::Path::new(bad)).is_none(),
                "should have been rejected: {bad:?}"
            );
        }
        // Length ceiling, so the command string cannot be pushed past
        // what cmd will accept.
        let long: String = "C:\\".chars().chain(std::iter::repeat('a').take(300)).collect();
        assert!(cmd_safe_path(std::path::Path::new(&long)).is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn install_params_redirect_into_the_log_and_set_utf8() {
        let p = wsl_install_cmd_params("Ubuntu", Some(r"C:\t\w.log"));
        // /s so cmd strips exactly the outer pair and leaves ours alone.
        assert!(p.starts_with("/s /c \""), "{p}");
        assert!(p.ends_with('"'), "{p}");
        // No space before && — otherwise WSL_UTF8 becomes "1 " and
        // wsl.exe falls back to UTF-16, mangling localized errors.
        assert!(p.contains("set WSL_UTF8=1&& wsl.exe"), "{p}");
        // Both streams captured, not just stdout: wsl.exe reports real
        // failures on stderr.
        assert!(p.contains(r#"> "C:\t\w.log" 2>&1"#), "{p}");
        assert!(p.contains("--install --no-launch -d Ubuntu"), "{p}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn install_params_without_a_log_still_installs() {
        // An unrepresentable temp path must not block the install; it
        // only costs us the message.
        let p = wsl_install_cmd_params("Ubuntu", None);
        assert!(!p.contains('>'), "{p}");
        assert!(p.contains("--install --no-launch -d Ubuntu"), "{p}");
        assert!(p.contains("set WSL_UTF8=1&&"), "{p}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn captured_output_reaches_classification() {
        // The regression this whole change exists for: before it, the
        // elevated path handed classify an empty string, so a reboot
        // request that only says so in words became a bare exit 1.
        match classify_wsl_install(1, "The operation requires a restart to complete.") {
            Err(ProvisioningError::RebootRequired { .. }) => {}
            other => panic!("expected RebootRequired, got {other:?}"),
        }
    }

    #[test]
    fn english_text_fallback_still_works_for_old_builds() {
        match classify_wsl_install(1, "Error: the requested operation requires elevation") {
            Err(ProvisioningError::ElevationRequired { .. }) => {}
            other => panic!("expected the text fallback to fire, got {other:?}"),
        }
        match classify_wsl_install(1, "Please reboot to complete the installation") {
            Err(ProvisioningError::RebootRequired { .. }) => {}
            other => panic!("expected the reboot fallback to fire, got {other:?}"),
        }
    }

    #[test]
    fn distro_names_are_allowlisted_before_reaching_shellexecute() {
        for ok in ["Ubuntu", "Ubuntu-22.04", "openSUSE-Leap-15.6", "kali_linux"] {
            assert!(valid_distro_name(ok), "{ok} should be accepted");
        }
        // ShellExecuteEx takes a parameter *string*, so anything that could
        // add an argument or a quote has to be refused (Rule #3).
        for bad in [
            "",
            "Ubuntu -d Other",
            "Ubuntu\"",
            "Ubuntu&calc",
            "Ubuntu;calc",
            "Ubuntu\ncalc",
            "../evil",
        ] {
            assert!(!valid_distro_name(bad), "{bad:?} should be refused");
        }
        assert!(!valid_distro_name(&"A".repeat(65)));
    }
}
