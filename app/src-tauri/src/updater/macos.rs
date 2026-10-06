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
    swap_with(mnt, bundle, parent, |s, d| run(DITTO, &[s, d]), |a, b| std::fs::rename(a, b))
}

/// `swap_from_mount` with the copy and rename steps injected (test seam).
fn swap_with(
    mnt: &Path,
    bundle: &Path,
    parent: &Path,
    copy: impl Fn(&Path, &Path) -> Result<(), String>,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
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

    copy(&src, &staging)?;
    rename(bundle, &old).map_err(|e| format!("swap failed: {e}"))?;
    if let Err(e) = rename(&staging, bundle) {
        let restored = rename(&old, bundle);
        remove_any(&staging);
        return Err(match restored {
            Ok(()) => format!("swap failed: {e}"),
            Err(r) => format!("swap failed: {e}; restoring old app also failed: {r}"),
        });
    }
    Ok(())
}

/// Writable precheck: create + drop a probe dir in the parent.
fn ensure_writable(parent: &Path) -> Result<(), String> {
    let probe = parent.join(".ymux-update-probe");
    remove_any(&probe);
    std::fs::create_dir(&probe)
        .map_err(|_| format!("cannot write {} — download the .dmg manually", parent.display()))?;
    let _ = std::fs::remove_dir(&probe);
    Ok(())
}

/// Compare the dmg's sha256 with `expected`; a mismatch deletes the dmg.
fn verify_dmg_sha(dmg: &Path, expected: Option<&str>) -> Result<(), String> {
    match expected {
        Some(expected) => {
            let actual = sha256_file(dmg)?;
            if !actual.eq_ignore_ascii_case(expected.trim()) {
                let _ = std::fs::remove_file(dmg);
                return Err(format!(
                    "downloaded dmg failed integrity check — expected {expected}, got {actual}"
                ));
            }
            log_info("UPDATER", &format!("updater: dmg sha256 verified ({actual})"));
        }
        None => log_warn("UPDATER", "updater: no dmg sha256 given — installing unverified"),
    }
    Ok(())
}

fn hdiutil_attach(mnt: &Path, dmg: &Path) -> Result<(), String> {
    let attach = Command::new(HDIUTIL)
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
        .arg(mnt)
        .arg(dmg)
        .output()
        .map_err(|e| format!("{HDIUTIL}: {e}"));
    match attach {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "hdiutil attach failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(e),
    }
}

/// Mount `dmg` at `mnt`, run `swap`, detach. attach/swap/detach injected (test seam).
fn mount_swap_detach(
    mnt: &Path,
    dmg: &Path,
    attach: impl Fn(&Path, &Path) -> Result<(), String>,
    swap: impl Fn(&Path) -> Result<(), String>,
    detach: impl Fn(&Path),
) -> Result<(), String> {
    // Stale mount from a crashed earlier run: detach, then reuse the path.
    if mnt.exists() {
        detach(mnt);
        remove_any(mnt);
    }
    std::fs::create_dir(mnt).map_err(|e| format!("create {}: {e}", mnt.display()))?;

    if let Err(e) = attach(mnt, dmg) {
        let _ = std::fs::remove_dir(mnt);
        return Err(e);
    }

    // Every path after attach goes through this single detach.
    let swapped = swap(mnt);
    detach(mnt);
    swapped
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

    ensure_writable(&parent)?;

    let tmp = std::env::temp_dir();
    let dmg = tmp.join(format!("ymux-update-{label}.dmg"));
    log_info("UPDATER", &format!("updater: downloading dmg {url}"));
    http_download_to_file(&url, &dmg).await?;

    verify_dmg_sha(&dmg, expected_sha.as_deref())?;

    let mnt = tmp.join(format!("ymux-update-mnt-{label}"));
    mount_swap_detach(
        &mnt,
        &dmg,
        hdiutil_attach,
        |m| swap_from_mount(m, &bundle, &parent),
        detach,
    )?;

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

    use std::cell::Cell;
    use std::fs;
    use std::io;

    /// Fake ditto: recursive-free copy of a dir's single file.
    fn fake_copy(s: &Path, d: &Path) -> Result<(), String> {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
        fs::copy(s.join("id"), d.join("id")).map(|_| ()).map_err(|e| e.to_string())
    }

    /// mnt with `n` .app dirs, parent holding a bundle tagged "old".
    fn fixture(n: usize) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        let mnt = t.path().join("mnt");
        let parent = t.path().join("parent");
        let bundle = parent.join("YMUX.app");
        fs::create_dir_all(&mnt).expect("mnt");
        fs::create_dir_all(&bundle).expect("bundle");
        fs::write(bundle.join("id"), "old").expect("old id");
        for i in 0..n {
            let a = mnt.join(format!("N{i}.app"));
            fs::create_dir_all(&a).expect("app");
            fs::write(a.join("id"), "new").expect("new id");
        }
        (t, mnt, bundle, parent)
    }

    /// Rename that fails on the `fail_on`-th call (1-based), else real.
    fn failing_rename(fail_on: &[u32]) -> impl Fn(&Path, &Path) -> io::Result<()> + '_ {
        let n = Cell::new(0u32);
        move |a, b| {
            n.set(n.get() + 1);
            if fail_on.contains(&n.get()) {
                Err(io::Error::other(format!("boom{}", n.get())))
            } else {
                fs::rename(a, b)
            }
        }
    }

    #[test]
    fn swap_success_replaces_bundle_and_keeps_old() {
        // happy path: new content in place, old kept for the caller's cleanup
        let (_t, mnt, bundle, parent) = fixture(1);
        swap_with(&mnt, &bundle, &parent, fake_copy, |a, b| fs::rename(a, b)).expect("swap");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "new");
        assert_eq!(fs::read_to_string(parent.join(OLD_NAME).join("id")).expect("old"), "old");
        assert!(!parent.join(STAGING_NAME).exists());
    }

    #[test]
    fn swap_clears_stale_staging_and_old() {
        // leftovers from a crashed run must not break copy/rename
        let (_t, mnt, bundle, parent) = fixture(1);
        for n in [STAGING_NAME, OLD_NAME] {
            fs::create_dir_all(parent.join(n)).expect("stale");
            fs::write(parent.join(n).join("junk"), "x").expect("junk");
        }
        swap_with(&mnt, &bundle, &parent, fake_copy, |a, b| fs::rename(a, b)).expect("swap");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "new");
        assert!(!parent.join(OLD_NAME).join("junk").exists());
    }

    #[test]
    fn swap_errors_when_no_app() {
        // empty dmg must fail before touching the installed bundle
        let (_t, mnt, bundle, parent) = fixture(0);
        let e = swap_with(&mnt, &bundle, &parent, fake_copy, |a, b| fs::rename(a, b)).unwrap_err();
        assert_eq!(e, "no .app in dmg");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "old");
    }

    #[test]
    fn swap_errors_when_several_apps() {
        // ambiguous dmg must not pick one at random
        let (_t, mnt, bundle, parent) = fixture(2);
        let e = swap_with(&mnt, &bundle, &parent, fake_copy, |a, b| fs::rename(a, b)).unwrap_err();
        assert_eq!(e, "no .app in dmg: expected exactly one, found several");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "old");
    }

    #[test]
    fn swap_copy_failure_leaves_bundle() {
        // a failed ditto passes its error through with the bundle untouched
        let (_t, mnt, bundle, parent) = fixture(1);
        let e = swap_with(&mnt, &bundle, &parent, |_, _| Err("ditto failed".into()), |a, b| {
            fs::rename(a, b)
        })
        .unwrap_err();
        assert_eq!(e, "ditto failed");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "old");
    }

    #[test]
    fn swap_first_rename_failure_leaves_bundle() {
        // bundle->old failing must leave the installed app in place
        let (_t, mnt, bundle, parent) = fixture(1);
        let e = swap_with(&mnt, &bundle, &parent, fake_copy, failing_rename(&[1])).unwrap_err();
        assert_eq!(e, "swap failed: boom1");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "old");
    }

    #[test]
    fn swap_second_rename_failure_restores_old() {
        // staging->bundle failing must put the original back and drop staging
        let (_t, mnt, bundle, parent) = fixture(1);
        let e = swap_with(&mnt, &bundle, &parent, fake_copy, failing_rename(&[2])).unwrap_err();
        assert_eq!(e, "swap failed: boom2");
        assert_eq!(fs::read_to_string(bundle.join("id")).expect("id"), "old");
        assert!(!parent.join(STAGING_NAME).exists());
    }

    #[test]
    fn swap_restore_failure_reports_both() {
        // both errors must surface so the user knows the app may be missing
        let (_t, mnt, bundle, parent) = fixture(1);
        let e = swap_with(&mnt, &bundle, &parent, fake_copy, failing_rename(&[2, 3])).unwrap_err();
        assert_eq!(e, "swap failed: boom2; restoring old app also failed: boom3");
    }

    #[test]
    fn ensure_writable_ok_leaves_no_probe() {
        // probe must be cleaned up or the next run's create_dir would see it
        let t = tempfile::tempdir().expect("tempdir");
        ensure_writable(t.path()).expect("writable");
        assert!(!t.path().join(".ymux-update-probe").exists());
    }

    #[test]
    fn ensure_writable_errors_on_readonly_dir() {
        // read-only /Applications must give the manual-download hint, not a swap failure
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().expect("tempdir");
        let ro = t.path().join("ro");
        fs::create_dir(&ro).expect("ro");
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).expect("chmod");
        let r = ensure_writable(&ro);
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).expect("reset");
        // root ignores mode bits; only assert the message when the probe was refused
        if let Err(e) = r {
            assert_eq!(e, format!("cannot write {} — download the .dmg manually", ro.display()));
        }
    }

    fn dmg_with(t: &tempfile::TempDir) -> (PathBuf, String) {
        let dmg = t.path().join("u.dmg");
        fs::write(&dmg, "payload").expect("dmg");
        let sha = sha256_file(&dmg).expect("sha");
        (dmg, sha)
    }

    #[test]
    fn verify_sha_match_keeps_dmg() {
        // matching hash (any case, padded) must not delete the download
        let t = tempfile::tempdir().expect("tempdir");
        let (dmg, sha) = dmg_with(&t);
        verify_dmg_sha(&dmg, Some(&format!(" {} ", sha.to_uppercase()))).expect("match");
        assert!(dmg.exists());
        verify_dmg_sha(&dmg, None).expect("none is ok");
        assert!(dmg.exists());
    }

    #[test]
    fn verify_sha_mismatch_deletes_dmg() {
        // a tampered dmg must be removed and reported with both hashes
        let t = tempfile::tempdir().expect("tempdir");
        let (dmg, sha) = dmg_with(&t);
        let e = verify_dmg_sha(&dmg, Some("deadbeef")).unwrap_err();
        assert_eq!(
            e,
            format!("downloaded dmg failed integrity check — expected deadbeef, got {sha}")
        );
        assert!(!dmg.exists());
    }

    #[test]
    fn mount_attach_failure_removes_mnt() {
        // failed attach: no swap, no detach, mount dir cleaned up
        let t = tempfile::tempdir().expect("tempdir");
        let mnt = t.path().join("mnt");
        let (swaps, detaches) = (Cell::new(0), Cell::new(0));
        let e = mount_swap_detach(
            &mnt,
            &t.path().join("u.dmg"),
            |_, _| Err("attach boom".into()),
            |_| {
                swaps.set(swaps.get() + 1);
                Ok(())
            },
            |_| detaches.set(detaches.get() + 1),
        )
        .unwrap_err();
        assert_eq!(e, "attach boom");
        assert!(!mnt.exists());
        assert_eq!((swaps.get(), detaches.get()), (0, 0));
    }

    #[test]
    fn mount_swap_failure_still_detaches() {
        // a failed swap must not leave the dmg mounted
        let t = tempfile::tempdir().expect("tempdir");
        let mnt = t.path().join("mnt");
        let detaches = Cell::new(0);
        let e = mount_swap_detach(
            &mnt,
            &t.path().join("u.dmg"),
            |_, _| Ok(()),
            |_| Err("swap boom".into()),
            |_| detaches.set(detaches.get() + 1),
        )
        .unwrap_err();
        assert_eq!(e, "swap boom");
        assert_eq!(detaches.get(), 1);
    }

    #[test]
    fn mount_success_detaches_once() {
        // happy path: swap sees the mount, exactly one detach
        let t = tempfile::tempdir().expect("tempdir");
        let mnt = t.path().join("mnt");
        let detaches = Cell::new(0);
        mount_swap_detach(
            &mnt,
            &t.path().join("u.dmg"),
            |_, _| Ok(()),
            |m| if m.is_dir() { Ok(()) } else { Err("no mnt".into()) },
            |_| detaches.set(detaches.get() + 1),
        )
        .expect("ok");
        assert_eq!(detaches.get(), 1);
    }

    #[test]
    fn mount_stale_mnt_detached_and_recreated() {
        // leftover mount dir from a crash: detach it, wipe it, start fresh
        let t = tempfile::tempdir().expect("tempdir");
        let mnt = t.path().join("mnt");
        fs::create_dir(&mnt).expect("mnt");
        fs::write(mnt.join("stale"), "x").expect("stale");
        let detaches = Cell::new(0);
        mount_swap_detach(
            &mnt,
            &t.path().join("u.dmg"),
            |_, _| Ok(()),
            |m| if m.join("stale").exists() { Err("stale survived".into()) } else { Ok(()) },
            |_| detaches.set(detaches.get() + 1),
        )
        .expect("ok");
        assert_eq!(detaches.get(), 2);
    }
}
