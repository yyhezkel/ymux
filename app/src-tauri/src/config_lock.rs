// One live YMUX per config dir: an OS lock on `<dir>/ymux.lock` is the gate.
// `run()` calls `gate_for_process` before it touches anything in the dir. A
// second launch asks the holder to show its window (`app.focus` RPC) and
// exits. If the holder does not answer, the launch shows a dialog that names
// the holder and exits. A lock that cannot be attempted stays fail-open.
//
// The lock binds only builds that take it. A build older than the lock does
// not see the gate, so the three-way merge in `write_workspaces_text` stays.
//
// The owner record lives in a SEPARATE file (`ymux.owner.json`): Windows byte
// range locks block reads of the locked file by other handles, so a second
// instance could not read the holder's pid out of `ymux.lock` itself.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use ymux_core::{log_info, log_warn};

const TAG: &str = "CONFIG_LOCK";
const LOCK_FILE: &str = "ymux.lock";
const OWNER_FILE: &str = "ymux.owner.json";

// Long enough to outlast the holder's own boot: it takes the lock at the top
// of `run()` and binds the RPC endpoint only at the end of `setup()`.
const FOCUS_TIMEOUT: Duration = Duration::from_secs(4);
const FOCUS_RETRY: Duration = Duration::from_millis(250);
const FOCUS_REQUEST: &str = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"app.focus\",\"params\":{}}\n";

// The lock lives as long as this handle; dropping it would release the lock.
static HELD: OnceLock<File> = OnceLock::new();

/// Who holds the config dir. `exe` is a file name only: a full path would
/// carry the user's home directory into the log (Rule #1 spirit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Owner {
    pub pid: u32,
    pub started_at: String,
    pub exe: String,
    pub version: String,
}

impl Owner {
    fn current() -> Owner {
        Owner {
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            exe: std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum LockOutcome {
    /// We hold the lock. `stale` = the owner record of a dead holder we replaced.
    Acquired { stale: Option<Owner> },
    /// Another live process holds it; `None` when its owner file is unreadable.
    HeldBy(Option<Owner>),
    /// The lock could not be attempted (io error); the message is the io kind.
    Unavailable(String),
}

/// Missing or malformed → `None`: both mean "no usable record".
fn read_owner(dir: &Path) -> Option<Owner> {
    let text = std::fs::read_to_string(dir.join(OWNER_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// tmp + rename (Rule #7): a reader never sees a half-written owner record.
fn write_owner(dir: &Path, owner: &Owner) -> std::io::Result<()> {
    let json = serde_json::to_string(owner).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{OWNER_FILE}.tmp"));
    let mut f = File::create(&tmp)?;
    f.write_all(json.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, dir.join(OWNER_FILE))
}

/// Try the lock. The returned file keeps the lock alive; the caller must hold it.
pub(crate) fn acquire(dir: &Path) -> (LockOutcome, Option<File>) {
    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
    {
        Ok(f) => f,
        Err(e) => return (LockOutcome::Unavailable(format!("open: {:?}", e.kind())), None),
    };
    match file.try_lock() {
        Ok(()) => {
            // We won, so any owner file on disk belongs to a holder the OS
            // already released (crash / kill): stale by construction.
            let stale = read_owner(dir);
            if let Err(e) = write_owner(dir, &Owner::current()) {
                // AI-NOTE: fail-open — the lock is ours, only the diagnostics record is missing.
                log_warn(TAG, &format!("owner file not written: {:?}", e.kind()));
            }
            (LockOutcome::Acquired { stale }, Some(file))
        }
        Err(TryLockError::WouldBlock) => (LockOutcome::HeldBy(read_owner(dir)), None),
        Err(TryLockError::Error(e)) => {
            (LockOutcome::Unavailable(format!("lock: {:?}", e.kind())), None)
        }
    }
}

/// Case-insensitive: Windows image names are `Winmux.exe` / `winmux.exe`.
/// "winmux" does not contain "ymux", so this is the only spelling checked.
pub(crate) fn is_pre_rename_build(name: &str) -> bool {
    name.to_lowercase().contains("winmux")
}

fn log_outcome(outcome: &LockOutcome) {
    match outcome {
        LockOutcome::Acquired { stale: None } => log_info(
            TAG,
            &format!("config dir lock taken pid={} v{}", std::process::id(), env!("CARGO_PKG_VERSION")),
        ),
        LockOutcome::Acquired { stale: Some(old) } => log_warn(
            TAG,
            &format!(
                "stale owner record replaced (previous holder pid={} v{} gone) pid={}",
                old.pid,
                old.version,
                std::process::id()
            ),
        ),
        LockOutcome::HeldBy(Some(o)) => log_warn(
            TAG,
            &format!(
                "config dir already in use by pid={} exe={} v{}; this launch hands off and exits, set YMUX_CONFIG_DIR to give this build its own dir",
                o.pid, o.exe, o.version
            ),
        ),
        LockOutcome::HeldBy(None) => log_warn(
            TAG,
            "config dir already in use by another process (owner unknown); this launch hands off and exits, set YMUX_CONFIG_DIR to give this build its own dir",
        ),
        // AI-NOTE: fail-open — a lock we cannot take must not stop the app.
        LockOutcome::Unavailable(why) => log_warn(TAG, &format!("config dir lock unavailable: {why}")),
    }
}

/// Take the lock for the life of the process, or hand off to its holder and
/// exit. Returns only when this process may boot.
pub(crate) fn gate_for_process(dir: &Path) {
    let (outcome, mut file) = acquire(dir);
    log_outcome(&outcome);
    if let LockOutcome::HeldBy(owner) = &outcome {
        match ask_holder_to_focus() {
            Ok(()) => {
                log_info(TAG, "holder focused its window; this launch exits");
                ymux_core::flush_log();
                std::process::exit(0);
            }
            Err(why) => {
                log_warn(TAG, &format!("holder did not answer app.focus: {why}"));
                // The holder can exit during the wait (tray quit, update).
                let (retry, retried_file) = acquire(dir);
                if !matches!(retry, LockOutcome::Acquired { .. }) {
                    show_dialog(&holder_message(owner.as_ref()));
                    ymux_core::flush_log();
                    std::process::exit(1);
                }
                log_info(TAG, "holder exited during the hand-off; this launch continues");
                file = retried_file;
            }
        }
    }
    if let Some(f) = file {
        // set() only fails if called twice; the first handle keeps the lock.
        let _ = HELD.set(f);
    }
    spawn_pre_rename_scan();
}

fn holder_message(owner: Option<&Owner>) -> String {
    let who = match owner {
        Some(o) => format!("process {} (version {})", o.pid, o.version),
        None => "another process".to_string(),
    };
    format!(
        "YMUX is already running with this config folder, in {who}, and it did not respond.\n\n\
         This launch will close. Close the running YMUX, or set YMUX_CONFIG_DIR to give this build its own folder."
    )
}

/// Bounded: a hung holder can block a pipe read forever, and a Windows `File`
/// has no read timeout. The worker thread dies with the process.
fn ask_holder_to_focus() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("config-lock-focus".into())
        .spawn(move || {
            let _ = tx.send(focus_with_retry());
        })
        .map_err(|e| format!("thread: {:?}", e.kind()))?;
    rx.recv_timeout(FOCUS_TIMEOUT)
        .map_err(|_| "no answer before the timeout".to_string())?
}

fn focus_with_retry() -> Result<(), String> {
    let deadline = Instant::now() + FOCUS_TIMEOUT;
    loop {
        match connect_holder() {
            Ok(stream) => return focus_over(stream),
            Err(_) if Instant::now() + FOCUS_RETRY < deadline => std::thread::sleep(FOCUS_RETRY),
            Err(e) => return Err(e),
        }
    }
}

#[cfg(windows)]
fn connect_holder() -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(ymux_core::pipe_name())
        .map_err(|e| format!("open pipe: {:?}", e.kind()))
}

#[cfg(not(windows))]
fn connect_holder() -> Result<std::os::unix::net::UnixStream, String> {
    let mut last = "no socket path".to_string();
    for name in ymux_core::pipe_names() {
        match std::os::unix::net::UnixStream::connect(&name) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("connect: {:?}", e.kind()),
        }
    }
    Err(last)
}

fn focus_over<S: Read + Write>(mut stream: S) -> Result<(), String> {
    stream
        .write_all(FOCUS_REQUEST.as_bytes())
        .map_err(|e| format!("send: {:?}", e.kind()))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| format!("read: {:?}", e.kind()))?;
    focus_reply_ok(&line)
}

/// A holder older than `app.focus` answers with a JSON-RPC error.
fn focus_reply_ok(line: &str) -> Result<(), String> {
    let reply: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|_| "unreadable reply".to_string())?;
    if reply.get("result").is_some() {
        Ok(())
    } else {
        Err("the holder rejected app.focus".to_string())
    }
}

// The Tauri dialog plugin needs an AppHandle, and the gate runs before the
// builder exists, so each platform uses its native call here.
#[cfg(windows)]
fn show_dialog(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
    };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide("YMUX"));
    // SAFETY: both buffers are NUL-terminated and outlive the call. A null
    // owner window is documented as valid.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND,
        );
    }
}

#[cfg(target_os = "macos")]
fn show_dialog(text: &str) {
    // The message goes in as argv, so nothing is quoted into the script (Rule #3).
    let shown = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "activate",
            "-e",
            "display alert \"YMUX\" message (item 1 of argv) as warning",
            "-e",
            "end run",
            text,
        ])
        .status();
    if let Err(e) = shown {
        log_warn(TAG, &format!("dialog not shown: {:?}", e.kind()));
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn show_dialog(text: &str) {
    log_warn(TAG, text);
}

/// Background scan so a slow process listing cannot delay boot.
fn spawn_pre_rename_scan() {
    let spawned = std::thread::Builder::new()
        .name("config-lock-scan".into())
        .spawn(|| {
            let mut sys = System::new();
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new());
            for (pid, p) in sys.processes() {
                let name = p.name().to_string_lossy();
                if is_pre_rename_build(&name) {
                    log_warn(
                        TAG,
                        &format!(
                            "pre-rename build running pid={} image={}; it shares the old config dir, set YMUX_CONFIG_DIR to separate the two",
                            pid.as_u32(),
                            name
                        ),
                    );
                }
            }
        });
    if let Err(e) = spawned {
        // AI-NOTE: fail-open — the scan is advisory.
        log_warn(TAG, &format!("pre-rename scan not started: {:?}", e.kind()));
    }
}

#[cfg(test)]
mod config_lock_tests {
    use super::*;

    // Pins: the winner records its own pid; breaking it blames the wrong process.
    #[test]
    fn first_acquire_takes_the_lock_and_records_our_pid() {
        let dir = tempfile::tempdir().unwrap();
        let (outcome, file) = acquire(dir.path());
        assert_eq!(outcome, LockOutcome::Acquired { stale: None });
        assert!(file.is_some());
        assert_eq!(read_owner(dir.path()).unwrap().pid, std::process::id());
    }

    // Pins: a live holder is reported with its record; breaking it loses the diagnostic.
    #[test]
    fn second_acquire_while_held_reports_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let (_, first) = acquire(dir.path());
        let (outcome, file) = acquire(dir.path());
        assert!(file.is_none());
        match outcome {
            LockOutcome::HeldBy(Some(o)) => assert_eq!(o.pid, std::process::id()),
            other => panic!("expected HeldBy(Some), got {other:?}"),
        }
        drop(first);
    }

    // Pins: a crash leaves the owner file but frees the lock → replaced, flagged stale.
    #[test]
    fn a_stale_owner_file_from_a_crash_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let dead = Owner {
            pid: 1,
            started_at: "2020-01-01T00:00:00Z".into(),
            exe: "ymux.exe".into(),
            version: "0.0.0".into(),
        };
        write_owner(dir.path(), &dead).unwrap();
        let (outcome, _file) = acquire(dir.path());
        assert_eq!(outcome, LockOutcome::Acquired { stale: Some(dead) });
        assert_eq!(read_owner(dir.path()).unwrap().pid, std::process::id());
    }

    // Pins: garbage in the owner file must not wedge startup or count as a holder.
    #[test]
    fn a_malformed_owner_file_is_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OWNER_FILE), "{not json").unwrap();
        assert!(read_owner(dir.path()).is_none());
        let (outcome, _file) = acquire(dir.path());
        assert_eq!(outcome, LockOutcome::Acquired { stale: None });
        assert_eq!(read_owner(dir.path()).unwrap().pid, std::process::id());
    }

    // Pins: only a JSON-RPC result counts; breaking it makes an old holder look focused.
    #[test]
    fn a_focus_reply_counts_only_with_a_result() {
        assert!(focus_reply_ok("{\"id\":1,\"result\":{\"ok\":true}}\n").is_ok());
        assert!(focus_reply_ok("{\"id\":1,\"error\":{\"code\":-32000,\"message\":\"unknown\"}}\n").is_err());
        assert!(focus_reply_ok("").is_err());
        assert!(focus_reply_ok("not json").is_err());
    }

    // Pins: the dialog names the holder; breaking it leaves the user nothing to kill.
    #[test]
    fn the_dialog_names_the_holders_pid_and_version() {
        let holder = Owner {
            pid: 4242,
            started_at: "2020-01-01T00:00:00Z".into(),
            exe: "ymux.exe".into(),
            version: "9.8.7".into(),
        };
        let text = holder_message(Some(&holder));
        assert!(text.contains("4242") && text.contains("9.8.7"), "{text}");
        assert!(holder_message(None).contains("YMUX_CONFIG_DIR"));
    }

    // Pins: both image-name spellings and cases match; breaking it hides old builds.
    #[test]
    fn pre_rename_build_names_are_detected() {
        assert!(is_pre_rename_build("winmux.exe"));
        assert!(is_pre_rename_build("WinMux.EXE"));
        assert!(is_pre_rename_build("winmux-cli"));
    }

    // Pins: "ymux" must not trip the check, or every run warns about itself.
    #[test]
    fn our_own_name_is_not_a_pre_rename_build() {
        assert!(!is_pre_rename_build("ymux.exe"));
        assert!(!is_pre_rename_build("YMUX"));
        assert!(!is_pre_rename_build(""));
    }
}
