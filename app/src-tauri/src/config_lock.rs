// Two YMUX builds sharing one config dir: an OS lock on `<dir>/ymux.lock`
// names the holder, so the second instance can log WHO has the dir. It never
// refuses to start; `write_workspaces_text` + the three-way merge already make
// concurrent saves safe, the lock is diagnostics only.
//
// The owner record lives in a SEPARATE file (`ymux.owner.json`): Windows byte
// range locks block reads of the locked file by other handles, so a second
// instance could not read the holder's pid out of `ymux.lock` itself.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use ymux_core::{log_info, log_warn};

const TAG: &str = "CONFIG_LOCK";
const LOCK_FILE: &str = "ymux.lock";
const OWNER_FILE: &str = "ymux.owner.json";

// The lock lives as long as this handle; dropping it would release the lock.
static HELD: OnceLock<File> = OnceLock::new();

/// Who holds the config dir. `exe` is a file name only: a full path would
/// carry the user's home directory into the log (Rule #1 spirit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owner {
    pub pid: u32,
    pub started_at: u64,
    pub exe: String,
    pub version: String,
}

impl Owner {
    fn current() -> Owner {
        Owner {
            pid: std::process::id(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            exe: std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum LockOutcome {
    /// We hold the lock. `stale` = an owner file from a dead holder was replaced.
    Acquired { stale: bool },
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
pub fn acquire(dir: &Path) -> (LockOutcome, Option<File>) {
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
            let stale = read_owner(dir).is_some();
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
pub fn is_pre_rename_build(name: &str) -> bool {
    name.to_lowercase().contains("winmux")
}

/// Take the lock for the life of the process and log one line per outcome.
/// Never blocks boot and never refuses to start.
pub fn hold_for_process(dir: &Path) {
    let (outcome, file) = acquire(dir);
    match &outcome {
        LockOutcome::Acquired { stale: false } => log_info(
            TAG,
            &format!("config dir lock taken pid={} v{}", std::process::id(), env!("CARGO_PKG_VERSION")),
        ),
        LockOutcome::Acquired { stale: true } => log_warn(
            TAG,
            &format!("stale owner record replaced (previous holder gone) pid={}", std::process::id()),
        ),
        LockOutcome::HeldBy(Some(o)) => log_warn(
            TAG,
            &format!(
                "config dir already in use by pid={} exe={} v{}; saves are merged, set YMUX_CONFIG_DIR to give this build its own dir",
                o.pid, o.exe, o.version
            ),
        ),
        LockOutcome::HeldBy(None) => log_warn(
            TAG,
            "config dir already in use by another process (owner unknown); saves are merged, set YMUX_CONFIG_DIR to give this build its own dir",
        ),
        // AI-NOTE: fail-open — a lock we cannot take must not stop the app.
        LockOutcome::Unavailable(why) => log_warn(TAG, &format!("config dir lock unavailable: {why}")),
    }
    if let Some(f) = file {
        // set() only fails if called twice; the first handle keeps the lock.
        let _ = HELD.set(f);
    }
    spawn_pre_rename_scan();
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
        assert_eq!(outcome, LockOutcome::Acquired { stale: false });
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
        let dead = Owner { pid: 1, started_at: 1, exe: "ymux.exe".into(), version: "0.0.0".into() };
        write_owner(dir.path(), &dead).unwrap();
        let (outcome, _file) = acquire(dir.path());
        assert_eq!(outcome, LockOutcome::Acquired { stale: true });
        assert_eq!(read_owner(dir.path()).unwrap().pid, std::process::id());
    }

    // Pins: garbage in the owner file must not wedge startup or count as a holder.
    #[test]
    fn a_malformed_owner_file_is_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OWNER_FILE), "{not json").unwrap();
        assert!(read_owner(dir.path()).is_none());
        let (outcome, _file) = acquire(dir.path());
        assert_eq!(outcome, LockOutcome::Acquired { stale: false });
        assert_eq!(read_owner(dir.path()).unwrap().pid, std::process::id());
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
