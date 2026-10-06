//! macOS in-app update: mount the .dmg, swap the running .app, relaunch.
//! Only compiled on macOS (`build-macos-intel.yml` is the sole compiler).

use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::AppHandle;

use super::{http_download_to_file, sha256_file};
use crate::{log_info, log_warn};

const HDIUTIL: &str = "/usr/bin/hdiutil";
const DITTO: &str = "/usr/bin/ditto";
const XATTR: &str = "/usr/bin/xattr";
const STAGING_NAME: &str = ".ymux-update-staging.app";
const OLD_NAME: &str = ".ymux-update-old.app";

/// Waits for the parent pid ($1) to exit, then opens the bundle ($2).
const WAIT_THEN_OPEN: &str =
    r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$2""#;

/// `.../Foo.app/Contents/MacOS/foo` → `.../Foo.app`. None when the exe
/// is not inside an `.app` bundle (dev build, bare binary).
fn bundle_root_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

fn run(prog: &str, args: &[&Path]) -> Result<(), String> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{prog} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn remove_any(p: &Path) {
    if p.is_dir() {
        let _ = std::fs::remove_dir_all(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}

fn detach(mnt: &Path) {
    let r = Command::new(HDIUTIL)
        .arg("detach")
        .arg(mnt)
        .arg("-quiet")
        .output();
    match r {
        Ok(o) if o.status.success() => {}
        Ok(o) => log_warn("UPDATER", &format!("hdiutil detach {}: {}", mnt.display(), o.status)),
        Err(e) => log_warn("UPDATER", &format!("hdiutil detach {}: {e}", mnt.display())),
    }
    let _ = std::fs::remove_dir(mnt);
}

/// Copy the single .app in `mnt` next to `bundle` and swap it in.
/// On failure of the second rename the old bundle is put back.
fn swap_from_mount(mnt: &Path, bundle: &Path, parent: &Path) -> Result<(), String> {
    let mut apps = std::fs::read_dir(mnt)
        .map_err(|e| format!("read {}: {e}", mnt.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "app"));
    let src = apps.next().ok_or_else(|| "no .app in dmg".to_string())?;
    if apps.next().is_some() {
        return Err("no .app in dmg: expected exactly one, found several".into());
    }

    let staging = parent.join(STAGING_NAME);
    let old = parent.join(OLD_NAME);
    remove_any(&staging);
    remove_any(&old);

    run(DITTO, &[&src, &staging])?;
    std::fs::rename(bundle, &old).map_err(|e| format!("swap failed: {e}"))?;
    if let Err(e) = std::fs::rename(&staging, bundle) {
        let restored = std::fs::rename(&old, bundle);
        remove_any(&staging);
        return Err(match restored {
            Ok(()) => format!("swap failed: {e}"),
            Err(r) => format!("swap failed: {e}; restoring old app also failed: {r}"),
        });
    }
    Ok(())
}

pub(super) async fn install_dmg_and_relaunch(
    app: AppHandle,
    url: String,
    expected_sha: Option<String>,
    label: String,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let bundle = bundle_root_of(&exe).ok_or_else(|| "not running from an .app bundle".to_string())?;
    let parent = bundle
        .parent()
        .ok_or_else(|| "not running from an .app bundle".to_string())?
        .to_path_buf();

    // Writable precheck: create + drop a probe dir in the parent.
    let probe = parent.join(".ymux-update-probe");
    remove_any(&probe);
    std::fs::create_dir(&probe)
        .map_err(|_| format!("cannot write {} — download the .dmg manually", parent.display()))?;
    let _ = std::fs::remove_dir(&probe);

    let tmp = std::env::temp_dir();
    let dmg = tmp.join(format!("ymux-update-{label}.dmg"));
    log_info("UPDATER", &format!("updater: downloading dmg {url}"));
    http_download_to_file(&url, &dmg).await?;

    match expected_sha {
        Some(expected) => {
            let actual = sha256_file(&dmg)?;
            if !actual.eq_ignore_ascii_case(expected.trim()) {
                let _ = std::fs::remove_file(&dmg);
                return Err(format!(
                    "downloaded dmg failed integrity check — expected {expected}, got {actual}"
                ));
            }
            log_info("UPDATER", &format!("updater: dmg sha256 verified ({actual})"));
        }
        None => log_warn("UPDATER", "updater: no dmg sha256 given — installing unverified"),
    }

    // Stale mount from a crashed earlier run: detach, then reuse the path.
    let mnt = tmp.join(format!("ymux-update-mnt-{label}"));
    if mnt.exists() {
        detach(&mnt);
        remove_any(&mnt);
    }
    std::fs::create_dir(&mnt).map_err(|e| format!("create {}: {e}", mnt.display()))?;

    let attach = Command::new(HDIUTIL)
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
        .arg(&mnt)
        .arg(&dmg)
        .output()
        .map_err(|e| format!("{HDIUTIL}: {e}"));
    let attach = match attach {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "hdiutil attach failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(e),
    };
    if let Err(e) = attach {
        let _ = std::fs::remove_dir(&mnt);
        return Err(e);
    }

    // Every path after attach goes through this single detach.
    let swapped = swap_from_mount(&mnt, &bundle, &parent);
    detach(&mnt);
    swapped?;

    // AI-NOTE: best-effort — a quarantined copy only costs a Gatekeeper prompt.
    if let Err(e) = run(XATTR, &[Path::new("-dr"), Path::new("com.apple.quarantine"), &bundle]) {
        log_warn("UPDATER", &format!("updater: quarantine strip: {e}"));
    }
    remove_any(&parent.join(OLD_NAME));
    let _ = std::fs::remove_file(&dmg);

    Command::new("/bin/sh")
        .arg("-c")
        .arg(WAIT_THEN_OPEN)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(&bundle)
        .spawn()
        .map_err(|e| format!("spawn relaunch: {e}"))?;

    log_info("UPDATER", "updater: dmg swapped, exiting for relaunch");
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    ymux_core::flush_log();
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_root_standard_layout() {
        // pins the normal installed layout; breaking it means no update on real installs
        let exe = Path::new("/Applications/YMUX.app/Contents/MacOS/app");
        assert_eq!(bundle_root_of(exe), Some(PathBuf::from("/Applications/YMUX.app")));
    }

    #[test]
    fn bundle_root_with_spaces() {
        // paths with spaces must survive (argv-only handling downstream)
        let exe = Path::new("/Users/a b/Apps/My YMUX.app/Contents/MacOS/app");
        assert_eq!(bundle_root_of(exe), Some(PathBuf::from("/Users/a b/Apps/My YMUX.app")));
    }

    #[test]
    fn bundle_root_none_outside_bundle() {
        // dev builds must get the "not running from an .app bundle" error, not a wrong swap
        assert_eq!(bundle_root_of(Path::new("/usr/local/bin/ymux")), None);
        assert_eq!(bundle_root_of(Path::new("")), None);
    }
}
