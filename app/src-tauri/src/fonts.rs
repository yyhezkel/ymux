//! Font install — download a curated font, verify it, and register it for
//! the current user.
//!
//! Why this exists: the Settings picker offers families that a clean Windows
//! box does not have (see `settings::list_system_fonts`). Flagging them ⚠️
//! stops the silent "nothing happened", but the user still has to go find a
//! .ttf, so the flag is only half an answer. This is the other half.
//!
//! Per-user, never machine-wide: fonts land in
//! `%LOCALAPPDATA%\Microsoft\Windows\Fonts` and are registered under HKCU,
//! which needs no elevation on Windows 10 1809+. `settings.rs` reads that
//! same HKCU hive, so an install is visible to the picker immediately.
//!
//! Two paths, in order:
//!   1. silent  — download → verify → extract → write → register → broadcast
//!   2. guided  — on any failure of (1), drop the file in Downloads and open
//!                it with the shell so the user gets Windows' own
//!                "Install" button. A locked-down box (GPO, AV) can refuse
//!                the silent path; it must not become a dead end.
//!
//! And the mirror: `font_uninstall` removes what `font_install` put there.
//! Symmetry is the point — we wrote those files, so we owe the user a way
//! to remove them that is not "hand-delete from %LOCALAPPDATA% and HKCU".
//! Only the guided path is one-way, and only because Windows performed
//! that install, not us.

use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::{log_info, log_warn};

// ─── catalog ───────────────────────────────────────────────────────────────
//
// Tags and hashes are PINNED, and verified against the real artifacts (see
// the commit that added this file). A pinned hash means an upstream
// re-release breaks the entry loudly instead of installing something we
// never checked — refreshing the catalog is a deliberate maintenance task,
// not something that drifts silently.
//
// Refresh procedure: for each entry, read the latest upstream tag, download
// the asset, run `sha256sum` + `wc -c` on it, then update url / sha256 /
// bytes together. Confirm a nerd-fonts zip still holds the entries matched
// by `name_prefix` (`unzip -l`). Last checked 2026-10-06: JetBrainsMono
// v2.304 and FiraCode 6.2 current, MesloLGS unchanged, nerd-fonts v3.5.1.
//
// Sizes drove which fonts are here at all. Deliberately excluded:
//   - Cascadia Code   upstream release is 150 MB, and it already ships with
//                     Windows 11 and with Windows Terminal on 10.
//   - JetBrainsMono Nerd Font  125 MB — the patched archives carry every
//                     weight × variant. MesloLGS NF below covers the
//                     "I need prompt glyphs" case at 10 MB.

/// Which entries to pull out of a downloaded archive.
struct ZipFilter {
    /// Directory prefix inside the zip ("" = archive root). Entries outside
    /// it are ignored.
    dir: &'static str,
    /// Required file-name prefix. Keeps sibling families out — the
    /// JetBrains archive also ships `JetBrainsMonoNL-*` (no-ligature), which
    /// is a different family and would pollute the picker.
    name_prefix: &'static str,
}

struct FontAsset {
    url: &'static str,
    sha256: &'static str,
    bytes: u64,
    /// `None` → the download IS a font file, saved under `save_as`.
    /// `Some` → a zip; matching entries are extracted.
    zip: Option<ZipFilter>,
    save_as: &'static str,
}

struct CatalogEntry {
    id: &'static str,
    /// The CSS family name this installs — must match what the picker shows,
    /// so an install flips that row from ⚠️ to ✅.
    family: &'static str,
    description: &'static str,
    homepage: &'static str,
    license: &'static str,
    assets: &'static [FontAsset],
}

const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "jetbrains-mono",
        family: "JetBrains Mono",
        description: "Ligatures, tall x-height. The family the picker offers by default but Windows never ships.",
        homepage: "https://github.com/JetBrains/JetBrainsMono",
        license: "SIL Open Font License 1.1",
        assets: &[FontAsset {
            url: "https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip",
            sha256: "6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf",
            bytes: 5_622_857,
            zip: Some(ZipFilter {
                dir: "fonts/ttf/",
                name_prefix: "JetBrainsMono-",
            }),
            save_as: "JetBrainsMono.zip",
        }],
    },
    CatalogEntry {
        id: "fira-code",
        family: "Fira Code",
        description: "The most-requested programming ligature font.",
        homepage: "https://github.com/tonsky/FiraCode",
        license: "SIL Open Font License 1.1",
        assets: &[FontAsset {
            url: "https://github.com/tonsky/FiraCode/releases/download/6.2/Fira_Code_v6.2.zip",
            sha256: "0949915ba8eb24d89fd93d10a7ff623f42830d7c5ffc3ecbf960e4ecad3e3e79",
            bytes: 2_462_987,
            zip: Some(ZipFilter {
                dir: "ttf/",
                name_prefix: "FiraCode-",
            }),
            save_as: "FiraCode.zip",
        }],
    },
    CatalogEntry {
        id: "meslolgs-nf",
        family: "MesloLGS NF",
        description: "Nerd Font with the glyphs Powerlevel10k / starship prompts draw. Four faces, no archive.",
        homepage: "https://github.com/romkatv/powerlevel10k#manual-font-installation",
        license: "Apache License 2.0",
        assets: &[
            FontAsset {
                url: "https://raw.githubusercontent.com/romkatv/powerlevel10k-media/v2.3.3/MesloLGS%20NF%20Regular.ttf",
                sha256: "d97946186e97f8d7c0139e8983abf40a1d2d086924f2c5dbf1c29bd8f2c6e57d",
                bytes: 2_594_368,
                zip: None,
                save_as: "MesloLGS NF Regular.ttf",
            },
            FontAsset {
                url: "https://raw.githubusercontent.com/romkatv/powerlevel10k-media/v2.3.3/MesloLGS%20NF%20Bold.ttf",
                sha256: "b6c0199cf7c7483c8343ea020658925e6de0aeb318b89908152fcb4d19226003",
                bytes: 2_603_868,
                zip: None,
                save_as: "MesloLGS NF Bold.ttf",
            },
            FontAsset {
                url: "https://raw.githubusercontent.com/romkatv/powerlevel10k-media/v2.3.3/MesloLGS%20NF%20Italic.ttf",
                sha256: "6f357bcbe2597704e157a915625928bca38364a89c22a4ac36e7a116dcd392ef",
                bytes: 2_553_260,
                zip: None,
                save_as: "MesloLGS NF Italic.ttf",
            },
            FontAsset {
                url: "https://raw.githubusercontent.com/romkatv/powerlevel10k-media/v2.3.3/MesloLGS%20NF%20Bold%20Italic.ttf",
                sha256: "56b4131adecec052c4b324efb818dd326d586dbc316fc68f98f1cae2eb8d1220",
                bytes: 2_561_984,
                zip: None,
                save_as: "MesloLGS NF Bold Italic.ttf",
            },
        ],
    },
    CatalogEntry {
        id: "firacode-nf",
        // The family is what the font DECLARES in its name table
        // ("FiraCode Nerd Font Mono"), which is not the file-name spelling
        // ("FiraCodeNerdFontMono-Regular.ttf"). Getting this wrong leaves a
        // permanently-⚠️ row that installing can never clear — the picker
        // reads the declared name back out of the registry.
        family: "FiraCode Nerd Font Mono",
        description: "Fira Code patched with Nerd Font glyphs, fixed-width variant. 27 MB download.",
        homepage: "https://github.com/ryanoasis/nerd-fonts",
        license: "SIL Open Font License 1.1",
        assets: &[FontAsset {
            url: "https://github.com/ryanoasis/nerd-fonts/releases/download/v3.5.1/FiraCode.zip",
            sha256: "239395baf60c89b2eaf4862b6b09db0ef95605cd3e8eef51c00345822a81a665",
            bytes: 28_602_426,
            zip: Some(ZipFilter {
                dir: "",
                name_prefix: "FiraCodeNerdFontMono-",
            }),
            save_as: "FiraCodeNerdFont.zip",
        }],
    },
];

/// Families this module can install. `settings::list_system_fonts` seeds the
/// picker with these so a missing one is still visible (flagged ⚠️) and
/// therefore still reachable by the Install button — a font that only
/// appears once installed can never be discovered.
pub(crate) fn installable_families() -> impl Iterator<Item = &'static str> {
    CATALOG.iter().map(|e| e.family)
}

// ─── public shapes ─────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct FontCatalogItem {
    pub id: String,
    pub family: String,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub download_bytes: u64,
    /// At least one face of this entry is present in the per-user font
    /// directory right now. Drives the Remove button.
    ///
    /// Derived from the directory rather than from a record of what we
    /// installed, deliberately: a manifest would only know about installs
    /// done AFTER it shipped, and every font already on a user's machine
    /// — the ones this followup was filed about — would stay unremovable.
    pub installed: bool,
}

/// `Debug` so `an_unknown_id_is_refused_before_anything_is_deleted` can call
/// `unwrap_err()` — `Result::unwrap_err` has to be able to print the Ok side.
#[derive(Debug, Serialize)]
pub(crate) struct FontUninstallResult {
    /// File names deleted from the per-user font directory.
    pub removed: Vec<String>,
    /// Registry values dropped (Windows only; empty elsewhere, where a
    /// font is found by directory scan and has nothing to unregister).
    pub unregistered: Vec<String>,
    /// Faces found but NOT removed, each with its reason. The expected
    /// case on Windows is a font file held open by a running application,
    /// which is why this is a reported outcome and not an error: the rest
    /// of the family still came out.
    pub failed: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct FontInstallResult {
    /// Faces actually written and registered.
    pub installed: Vec<String>,
    /// True when the silent path failed and we fell back to opening the file
    /// for the user — the font is NOT installed yet in that case.
    pub guided: bool,
    /// Set when `guided` is true: what we handed to the shell.
    pub guided_path: Option<String>,
    /// Human-readable reason the silent path was abandoned.
    pub fallback_reason: Option<String>,
}

// ─── commands ──────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn font_catalog() -> Result<Vec<FontCatalogItem>, String> {
    // One directory read for the whole catalog, not one per entry.
    // A missing or unreadable font dir is not an error here: it means
    // nothing is installed, which is a perfectly good answer.
    let present = installed_face_files().unwrap_or_default();
    Ok(CATALOG
        .iter()
        .map(|e| FontCatalogItem {
            id: e.id.to_string(),
            family: e.family.to_string(),
            description: e.description.to_string(),
            homepage: e.homepage.to_string(),
            license: e.license.to_string(),
            download_bytes: e.assets.iter().map(|a| a.bytes).sum(),
            installed: present.iter().any(|f| entry_owns_file(e, f)),
        })
        .collect())
}

/// Download + install the catalog entry `id` for the current user.
///
/// Runs on a blocking thread: `ureq` is synchronous and these are multi-MB
/// downloads, so holding the async runtime would freeze unrelated commands.
#[tauri::command]
pub(crate) async fn font_install(id: String) -> Result<FontInstallResult, String> {
    tauri::async_runtime::spawn_blocking(move || install_blocking(&id))
        .await
        .map_err(|e| format!("font install task failed: {e}"))?
}

/// Remove the faces of catalog entry `id` from the per-user font
/// directory, and drop their HKCU registrations.
///
/// Blocking for the same reason `font_install` is: file deletion and a
/// registry walk are synchronous, and a font held open by another app
/// makes the delete slow as well as fallible.
#[tauri::command]
pub(crate) async fn font_uninstall(id: String) -> Result<FontUninstallResult, String> {
    tauri::async_runtime::spawn_blocking(move || uninstall_blocking(&id))
        .await
        .map_err(|e| format!("font uninstall task failed: {e}"))?
}

fn uninstall_blocking(id: &str) -> Result<FontUninstallResult, String> {
    // Same as install: look the id up rather than trusting it. Here it
    // matters more — an unchecked id would turn this into "delete an
    // arbitrary file from the font directory".
    let entry = CATALOG
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("unknown font id: {id}"))?;

    let dir = user_font_dir()?;
    let names: Vec<String> = installed_face_files()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| entry_owns_file(entry, f))
        .collect();

    log_info(
        "FONTS",
        &format!("uninstall: {} ({} face(s) found)", entry.id, names.len()),
    );

    // Delete FIRST, unregister only what actually went. The other order
    // would leave a font that is still on disk and still loadable with no
    // registry entry — present but invisible to the picker, which is a
    // worse state than either end of the operation.
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    let mut gone_paths = Vec::new();
    for name in names {
        let target = dir.join(&name);
        match std::fs::remove_file(&target) {
            Ok(()) => {
                gone_paths.push(target);
                removed.push(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Raced with something else removing it. Still unregister.
                gone_paths.push(target);
                removed.push(name);
            }
            Err(e) => {
                // The everyday case: an app has the file open. Report it
                // per-face instead of failing the whole call — the rest of
                // the family did come out.
                log_warn("FONTS", &format!("uninstall: {name} not removed: {e}"));
                failed.push(format!("{name}: {e}"));
            }
        }
    }

    let unregistered = unregister_fonts(&gone_paths);
    if !removed.is_empty() {
        notify_font_change();
    }
    log_info(
        "FONTS",
        &format!(
            "uninstall: {} done ({} removed, {} registry, {} failed)",
            entry.id,
            removed.len(),
            unregistered.len(),
            failed.len()
        ),
    );
    Ok(FontUninstallResult {
        removed,
        unregistered,
        failed,
    })
}

// ─── which files belong to which entry ─────────────────────────────────────

/// A font file by extension. Both are what the catalog ships and what
/// `validate_font_magic` accepts.
fn has_font_extension(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".ttf") || lower.ends_with(".otf")
}

/// Does a bare file name in the per-user font dir belong to this entry?
///
/// Mirrors what `install_faces` wrote, from the same catalog data, which
/// is why no install-time record is needed:
///   - a zip asset produced every entry matching `ZipFilter::name_prefix`,
///     so the prefix identifies them again;
///   - a bare asset produced exactly `save_as`.
///
/// The prefixes are specific enough not to collide across the catalog —
/// `FiraCodeNerdFontMono-` is not matched by `FiraCode-`, since the file
/// names differ from the fourth character. `entry_prefixes_do_not_collide`
/// pins that, because a future entry could break it silently.
///
/// A user's own hand-installed copy of the same family WOULD match. That
/// is accepted rather than worked around: it is the same family they just
/// asked to remove, it lives in the per-user directory either way, and the
/// alternative — a manifest — cannot see anything installed before it
/// existed, which is precisely the case this feature was filed for.
fn entry_owns_file(entry: &CatalogEntry, file_name: &str) -> bool {
    entry.assets.iter().any(|asset| match &asset.zip {
        Some(filter) => has_font_extension(file_name) && file_name.starts_with(filter.name_prefix),
        None => file_name.eq_ignore_ascii_case(asset.save_as),
    })
}

/// Bare file names of every font sitting in the per-user font directory.
/// `Err` only for a directory that exists and cannot be read; a missing
/// one yields an empty list, since "no font dir" and "no fonts" are the
/// same answer to every caller here.
fn installed_face_files() -> Result<Vec<String>, String> {
    let dir = user_font_dir()?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    Ok(read
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| has_font_extension(n))
        .collect())
}

/// Drop the HKCU values pointing at `paths`.
///
/// Matched by VALUE DATA rather than by rebuilding the value name.
/// `register_font` names the value from the font's internal name table
/// with a fallback to the file stem, so rebuilding it would have to
/// reproduce that decision from a file that has just been deleted. The
/// path is the thing we actually know.
#[cfg(target_os = "windows")]
fn unregister_fonts(paths: &[PathBuf]) -> Vec<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    if paths.is_empty() {
        return Vec::new();
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = match hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows NT\CurrentVersion\Fonts",
        KEY_READ | KEY_SET_VALUE,
    ) {
        Ok(k) => k,
        Err(e) => {
            // No key means nothing was ever registered per-user. The files
            // are already gone, so this is not a failure.
            log_warn("FONTS", &format!("uninstall: per-user font key unavailable: {e}"));
            return Vec::new();
        }
    };

    let wanted: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase())
        .collect();
    // Collect before deleting: mutating the key while enumerating it is
    // undefined for the iterator.
    let doomed: Vec<String> = key
        .enum_values()
        .filter_map(|r| r.ok())
        .filter_map(|(name, _)| {
            let data: String = key.get_value(&name).ok()?;
            wanted.contains(&data.to_lowercase()).then_some(name)
        })
        .collect();

    let mut dropped = Vec::new();
    for name in doomed {
        match key.delete_value(&name) {
            Ok(()) => dropped.push(name),
            Err(e) => log_warn(
                "FONTS",
                &format!("uninstall: cannot drop registry value {name}: {e}"),
            ),
        }
    }
    dropped
}

/// No-op off Windows, mirroring `register_font`: CoreText and fontconfig
/// scan the directory, so removing the file IS the uninstall.
#[cfg(not(target_os = "windows"))]
fn unregister_fonts(_paths: &[PathBuf]) -> Vec<String> {
    Vec::new()
}

fn install_blocking(id: &str) -> Result<FontInstallResult, String> {
    // Look the id up rather than trusting it — the frontend only ever sends
    // ids we handed it, and anything else stops here.
    let entry = CATALOG
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("unknown font id: {id}"))?;

    log_info(
        "FONTS",
        &format!("install: {} ({} assets)", entry.id, entry.assets.len()),
    );

    // Phase 1 — fetch and verify everything BEFORE touching the font
    // directory, so a mid-way failure can't leave a half-installed family.
    let mut faces: Vec<(String, Vec<u8>)> = Vec::new();
    for asset in entry.assets {
        let bytes = download_verified(asset)?;
        match &asset.zip {
            Some(filter) => faces.extend(extract_fonts(&bytes, filter)?),
            None => {
                validate_font_magic(&bytes, asset.save_as)?;
                faces.push((asset.save_as.to_string(), bytes));
            }
        }
    }
    if faces.is_empty() {
        return Err(format!(
            "{}: download contained no font files — the catalog filter is stale",
            entry.id
        ));
    }

    // Phase 2 — install. Any failure here falls back to guided rather than
    // leaving the user stuck.
    match install_faces(&faces) {
        Ok(installed) => {
            log_info(
                "FONTS",
                &format!("install: {} ok, {} faces", entry.id, installed.len()),
            );
            Ok(FontInstallResult {
                installed,
                guided: false,
                guided_path: None,
                fallback_reason: None,
            })
        }
        Err(reason) => {
            log_warn(
                "FONTS",
                &format!("install: {} silent path failed ({reason}) — falling back to guided", entry.id),
            );
            let path = hand_off_to_shell(&faces).map_err(|e| {
                format!("silent install failed ({reason}); guided fallback also failed ({e})")
            })?;
            Ok(FontInstallResult {
                installed: Vec::new(),
                guided: true,
                guided_path: Some(path.to_string_lossy().to_string()),
                fallback_reason: Some(reason),
            })
        }
    }
}

// ─── download + verify ─────────────────────────────────────────────────────

fn download_verified(asset: &FontAsset) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let resp = ymux_core::http::get_with_retry(|| ureq::get(asset.url))
        .map_err(|e| format!("download failed: {e}"))?;

    // Cap the read at a generous multiple of the expected size: a redirect to
    // something unexpected must not stream into memory unbounded.
    let cap = asset.bytes.saturating_mul(2).max(1 << 20);
    let mut buf = Vec::with_capacity(asset.bytes as usize);
    resp.into_reader()
        .take(cap)
        .read_to_end(&mut buf)
        .map_err(|e| format!("download read failed: {e}"))?;

    if buf.len() as u64 != asset.bytes {
        return Err(format!(
            "size mismatch: expected {} bytes, got {}",
            asset.bytes,
            buf.len()
        ));
    }
    let got = hex_lower(&Sha256::digest(&buf));
    if got != asset.sha256 {
        // Deliberately loud and fatal. A hash mismatch means the bytes are
        // not what this catalog was built against — never install them.
        return Err(format!(
            "checksum mismatch for {}: expected {}, got {got}",
            asset.save_as, asset.sha256
        ));
    }
    Ok(buf)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        // Writing to a String cannot fail; the Result is discarded rather
        // than unwrapped (Rule #4).
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Font files start with a recognisable sfnt tag. Checking it stops us from
/// registering a README or an HTML error page as a font.
fn validate_font_magic(bytes: &[u8], name: &str) -> Result<(), String> {
    let head = bytes.get(..4).ok_or_else(|| format!("{name}: too short to be a font"))?;
    let ok = matches!(
        head,
        [0x00, 0x01, 0x00, 0x00] // TrueType outlines
            | [0x74, 0x72, 0x75, 0x65] // 'true'
            | [0x4F, 0x54, 0x54, 0x4F] // 'OTTO' — CFF outlines
            | [0x74, 0x74, 0x63, 0x66] // 'ttcf' — collection
    );
    if ok {
        Ok(())
    } else {
        Err(format!("{name}: not a font file (bad sfnt tag)"))
    }
}

// ─── zip extraction ────────────────────────────────────────────────────────

fn extract_fonts(archive: &[u8], filter: &ZipFilter) -> Result<Vec<(String, Vec<u8>)>, String> {
    use std::io::{Cursor, Read};

    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|e| format!("archive unreadable: {e}"))?;
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();

    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|e| format!("archive entry {i} unreadable: {e}"))?;
        if !f.is_file() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..` traversal — the
        // zip-slip guard. We additionally keep only the final component, so
        // nothing can be written outside the font directory even if a future
        // filter widens.
        let Some(path) = f.enclosed_name() else {
            continue;
        };
        let full = path.to_string_lossy().replace('\\', "/");
        if !full.starts_with(filter.dir) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(filter.name_prefix) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
            continue;
        }
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| format!("{name}: extract failed: {e}"))?;
        validate_font_magic(&buf, name)?;
        out.push((name.to_string(), buf));
    }
    Ok(out)
}

// ─── the sfnt `name` table ─────────────────────────────────────────────────

/// Read the full font name (name ID 4, e.g. "JetBrains Mono ExtraBold") out
/// of a font file.
///
/// This is not cosmetic. Windows keys the per-user registry entry on this
/// name, and `settings::list_system_fonts` builds the picker by reading
/// those very names back — so deriving it from the file name instead would
/// list the font as "JetBrainsMono-Regular" and the ✅ next to
/// "JetBrains Mono" would never light up.
pub(crate) fn full_font_name(data: &[u8]) -> Option<String> {
    let be16 = |off: usize| -> Option<u16> {
        Some(u16::from_be_bytes(data.get(off..off + 2)?.try_into().ok()?))
    };
    let be32 = |off: usize| -> Option<u32> {
        Some(u32::from_be_bytes(data.get(off..off + 4)?.try_into().ok()?))
    };

    // A collection ('ttcf') points at its first font's table directory.
    let base = if data.get(..4) == Some(&[0x74, 0x74, 0x63, 0x66]) {
        be32(12)? as usize
    } else {
        0
    };

    let num_tables = be16(base + 4)? as usize;
    let mut name_table: Option<(usize, usize)> = None;
    for i in 0..num_tables {
        let rec = base + 12 + i * 16;
        if data.get(rec..rec + 4)? == b"name" {
            name_table = Some((be32(rec + 8)? as usize, be32(rec + 12)? as usize));
            break;
        }
    }
    let (tbl, tbl_len) = name_table?;
    let count = be16(tbl + 2)? as usize;
    let storage = tbl + be16(tbl + 4)? as usize;

    // Prefer the Windows/Unicode/en-US record; fall back to any nameID 4.
    let mut fallback: Option<String> = None;
    for i in 0..count {
        let rec = tbl + 6 + i * 12;
        if rec + 12 > tbl + tbl_len {
            break;
        }
        let platform = be16(rec)?;
        let encoding = be16(rec + 2)?;
        let language = be16(rec + 4)?;
        let name_id = be16(rec + 6)?;
        if name_id != 4 {
            continue;
        }
        let len = be16(rec + 8)? as usize;
        let off = storage + be16(rec + 10)? as usize;
        let raw = data.get(off..off + len)?;

        if platform == 3 {
            // Windows platform strings are UTF-16BE.
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            let s = String::from_utf16(&units).ok()?;
            if encoding == 1 && language == 0x0409 {
                return Some(s);
            }
            fallback.get_or_insert(s);
        } else if platform == 1 && fallback.is_none() {
            // Macintosh platform: single-byte, ASCII in practice.
            fallback = Some(raw.iter().map(|&b| b as char).collect());
        }
    }
    fallback
}

// ─── install ───────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn user_font_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| "cannot locate %LOCALAPPDATA%".to_string())?;
    Ok(base.join("Microsoft").join("Windows").join("Fonts"))
}

/// macOS: `~/Library/Fonts`. The per-user font directory CoreText scans on
/// its own — which is why `register_font` below is a no-op here rather than
/// an error.
///
/// This used to be the Windows arm unconditionally, so a mac install wrote
/// the faces into `~/Library/Application Support/Microsoft/Windows/Fonts`
/// — a directory nothing reads — and then failed at the registration step,
/// leaving the orphans behind.
#[cfg(target_os = "macos")]
fn user_font_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "cannot locate $HOME".to_string())?;
    Ok(home.join("Library").join("Fonts"))
}

/// Linux/other unix: the XDG per-user font dir, picked up by fontconfig.
#[cfg(all(unix, not(target_os = "macos")))]
fn user_font_dir() -> Result<PathBuf, String> {
    let base = dirs::data_dir().ok_or_else(|| "cannot locate $XDG_DATA_HOME".to_string())?;
    Ok(base.join("fonts"))
}

/// Write each face into the per-user font directory and register it.
/// Returns the registered face names.
fn install_faces(faces: &[(String, Vec<u8>)]) -> Result<Vec<String>, String> {
    let dir = user_font_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let mut installed = Vec::new();
    for (file_name, bytes) in faces {
        // Defence in depth: never let a name from an archive steer the path.
        let safe = Path::new(file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("unsafe font file name: {file_name}"))?;
        let target = dir.join(safe);

        // Write via a temp file + rename so a crash can't leave a truncated
        // font behind a valid-looking registry entry.
        let tmp = dir.join(format!("{safe}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &target) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("install {}: {e}", target.display()));
        }

        let face = full_font_name(bytes).unwrap_or_else(|| {
            // Losing the real name only costs a cosmetic registry label.
            log_warn("FONTS", &format!("no name table in {safe}; using file stem"));
            safe.trim_end_matches(".ttf").trim_end_matches(".otf").to_string()
        });
        register_font(&face, &target)?;
        installed.push(face);
    }

    notify_font_change();
    Ok(installed)
}

/// Add the HKCU registry entry Windows uses to find per-user fonts.
///
/// Note the value DATA differs from the machine-wide hive: HKLM stores a
/// bare file name (resolved against %WINDIR%\Fonts), HKCU stores the full
/// path.
#[cfg(target_os = "windows")]
fn register_font(face: &str, path: &Path) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows NT\CurrentVersion\Fonts",
            KEY_SET_VALUE,
        )
        .or_else(|_| {
            hkcu.create_subkey(r"Software\Microsoft\Windows NT\CurrentVersion\Fonts")
                .map(|(k, _)| k)
        })
        .map_err(|e| format!("cannot open the per-user font key: {e}"))?;

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("ttf")
        .to_ascii_lowercase();
    let tag = if ext == "otf" { "(OpenType)" } else { "(TrueType)" };
    let value_name = format!("{face} {tag}");

    key.set_value(&value_name, &path.to_string_lossy().to_string())
        .map_err(|e| format!("cannot register {value_name}: {e}"))?;
    Ok(())
}

/// No-op off Windows: registration IS the Windows-specific half. Dropping a
/// face into `~/Library/Fonts` (CoreText) or `$XDG_DATA_HOME/fonts`
/// (fontconfig) is all it takes for the system to find it.
///
/// Returning `Err` here used to abort `install_faces` *after* it had already
/// written the file, so the install both failed and littered.
#[cfg(not(target_os = "windows"))]
fn register_font(_face: &str, _path: &Path) -> Result<(), String> {
    Ok(())
}

/// Make the new font usable without a reboot: load it into this session and
/// tell every top-level window the font set changed.
#[cfg(target_os = "windows")]
fn notify_font_change() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_FONTCHANGE,
    };

    // SAFETY: a parameterless broadcast of a documented notification. The
    // timeout keeps a hung top-level window in another process from
    // blocking us; the result is advisory, so it is intentionally ignored.
    unsafe {
        let mut result: usize = 0;
        SendMessageTimeoutW(
            HWND_BROADCAST as HWND,
            WM_FONTCHANGE,
            0 as WPARAM,
            0 as LPARAM,
            SMTO_ABORTIFHUNG,
            1000,
            &mut result,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn notify_font_change() {}

// ─── guided fallback ───────────────────────────────────────────────────────

/// Drop the first face into Downloads and open it, so the user gets the
/// Windows font preview with its own "Install" button.
fn hand_off_to_shell(faces: &[(String, Vec<u8>)]) -> Result<PathBuf, String> {
    let (name, bytes) = faces
        .first()
        .ok_or_else(|| "nothing to hand off".to_string())?;
    let dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "cannot locate the Downloads folder".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let safe = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("unsafe font file name: {name}"))?;
    let target = dir.join(safe);
    std::fs::write(&target, bytes).map_err(|e| format!("write {}: {e}", target.display()))?;

    open::that_detached(&target)
        .map_err(|e| format!("cannot open {}: {e}", target.display()))?;
    log_info("FONTS", &format!("guided install: opened {safe} from Downloads"));
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── uninstall: which files belong to which entry ──────────────────────
    //
    // The whole mechanism rests on `entry_owns_file` being exactly as wide as
    // what `install_faces` wrote - no wider. Too narrow leaves orphans the
    // user still cannot remove; too wide deletes somebody else's font.

    #[test]
    fn entry_owns_exactly_what_it_would_have_installed() {
        let jb = CATALOG.iter().find(|e| e.id == "jetbrains-mono").unwrap();
        assert!(entry_owns_file(jb, "JetBrainsMono-Regular.ttf"));
        assert!(entry_owns_file(jb, "JetBrainsMono-BoldItalic.ttf"));
        // The no-ligature sibling is a DIFFERENT family that the ZipFilter
        // deliberately excludes at install time, so uninstall must not claim
        // it either.
        assert!(!entry_owns_file(jb, "JetBrainsMonoNL-Regular.ttf"));
        // Not a font file.
        assert!(!entry_owns_file(jb, "JetBrainsMono-Regular.txt"));
        assert!(!entry_owns_file(jb, "JetBrainsMono.zip"));

        // A bare-asset entry matches its exact file names and nothing else.
        let meslo = CATALOG.iter().find(|e| e.id == "meslolgs-nf").unwrap();
        assert!(entry_owns_file(meslo, "MesloLGS NF Regular.ttf"));
        assert!(entry_owns_file(meslo, "MesloLGS NF Bold Italic.ttf"));
        assert!(!entry_owns_file(meslo, "MesloLGS NF Light.ttf"));
        // Windows file names are case-insensitive; a face installed by an
        // older build or by the user's own hand may differ in case.
        assert!(entry_owns_file(meslo, "meslolgs nf regular.TTF"));
    }

    #[test]
    fn entry_prefixes_do_not_collide() {
        // `FiraCode-` and `FiraCodeNerdFontMono-` are different families that
        // share a stem. If a future catalog entry ever makes one prefix a
        // prefix of another, uninstalling one would silently take the other -
        // so this is checked over the whole catalog, not just today's pair.
        for a in CATALOG {
            for b in CATALOG {
                if a.id == b.id {
                    continue;
                }
                for asset in a.assets {
                    let Some(filter) = &asset.zip else { continue };
                    let sample = format!("{}Regular.ttf", filter.name_prefix);
                    assert!(
                        !entry_owns_file(b, &sample),
                        "{} would claim {}'s file {sample}",
                        b.id,
                        a.id
                    );
                }
            }
        }
    }

    #[test]
    fn every_bare_asset_is_a_font_file_we_can_find_again() {
        // A bare asset is matched by exact name, so `save_as` has to BE the
        // installed file name - if one ever gets a .zip extension by mistake,
        // install writes it and uninstall can never see it.
        for e in CATALOG {
            for asset in e.assets {
                if asset.zip.is_none() {
                    assert!(
                        has_font_extension(asset.save_as),
                        "{}: bare asset {} is not a font file",
                        e.id,
                        asset.save_as
                    );
                }
            }
        }
    }

    #[test]
    fn an_unknown_id_is_refused_before_anything_is_deleted() {
        // The id is what selects files for deletion, so an unchecked one
        // would make this "delete arbitrary things from the font directory".
        let err = uninstall_blocking("../../etc/passwd").unwrap_err();
        assert!(err.contains("unknown font id"), "got: {err}");
        let err = uninstall_blocking("").unwrap_err();
        assert!(err.contains("unknown font id"), "got: {err}");
    }

    #[test]
    fn catalog_ids_are_unique_and_wired() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate catalog id");
        for e in CATALOG {
            assert!(!e.assets.is_empty(), "{} has no assets", e.id);
            assert!(!e.family.is_empty());
            for a in e.assets {
                assert_eq!(a.sha256.len(), 64, "{}: sha256 must be hex-64", e.id);
                assert!(
                    a.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                    "{}: sha256 not hex",
                    e.id
                );
                assert!(a.bytes > 0, "{}: size must be pinned too", e.id);
                assert!(
                    a.url.starts_with("https://"),
                    "{}: fonts must come over TLS",
                    e.id
                );
            }
        }
    }

    /// Every catalog family must be something the picker can actually show,
    /// otherwise installing it would leave the ⚠️ in place.
    #[test]
    fn catalog_families_look_monospace_to_the_picker() {
        for e in CATALOG {
            assert!(
                crate::settings::looks_monospace(e.family),
                "{} ({}) would not appear in the terminal font list",
                e.id,
                e.family
            );
        }
    }

    /// The face names each catalog entry ACTUALLY registers, read out of the
    /// shipped font files' `name` tables. Recorded here so the round trip
    /// install → registry → picker is pinned by a test instead of by hope.
    ///
    /// This is the check that catches the failure mode a "does the family
    /// look monospace" test cannot: a family string that is well-formed and
    /// plausible but not what the font declares. `FiraCodeNerdFontMono`
    /// passed every other assertion while being unmatchable forever.
    const REGISTERED_FACES: &[(&str, &[&str])] = &[
        (
            "JetBrains Mono",
            &["JetBrains Mono Regular", "JetBrains Mono ExtraBold Italic"],
        ),
        ("Fira Code", &["Fira Code Regular", "Fira Code Retina"]),
        ("MesloLGS NF", &["MesloLGS NF Regular", "MesloLGS NF Bold Italic"]),
        (
            "FiraCode Nerd Font Mono",
            &[
                "FiraCode Nerd Font Mono Reg",
                "FiraCode Nerd Font Mono Med",
                "FiraCode Nerd Font Mono Ret",
                "FiraCode Nerd Font Mono SemBd",
                "FiraCode Nerd Font Mono Bold",
            ],
        ),
    ];

    #[test]
    fn installed_faces_resolve_back_to_their_catalog_family() {
        for (family, faces) in REGISTERED_FACES {
            for face in *faces {
                assert!(
                    crate::settings::family_is_installed(family, &[face.to_string()]),
                    "installing {face:?} must mark {family:?} as installed"
                );
            }
        }
        // Every catalog family must be covered above, so adding an entry
        // without recording its real face names fails here rather than
        // shipping an un-clearable ⚠️.
        for e in CATALOG {
            assert!(
                REGISTERED_FACES.iter().any(|(f, _)| *f == e.family),
                "{}: no recorded face names for family {:?}",
                e.id,
                e.family
            );
        }
    }

    #[test]
    fn rejects_non_font_bytes() {
        assert!(validate_font_magic(b"<!DOCTYPE html><html>", "x.ttf").is_err());
        assert!(validate_font_magic(b"", "x.ttf").is_err());
        assert!(validate_font_magic(&[0x00, 0x01, 0x00, 0x00, 0x00], "x.ttf").is_ok());
        assert!(validate_font_magic(b"OTTO____", "x.otf").is_ok());
    }

    #[test]
    fn hex_lower_pads_single_digits() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
    }

    /// Minimal hand-built sfnt with one `name` record, to prove the parser
    /// finds the Windows/en-US full name rather than guessing from a path.
    #[test]
    fn reads_full_name_from_name_table() {
        let text: Vec<u8> = "Test Mono Bold"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        let mut name_tbl = Vec::new();
        name_tbl.extend_from_slice(&0u16.to_be_bytes()); // format
        name_tbl.extend_from_slice(&1u16.to_be_bytes()); // count
        name_tbl.extend_from_slice(&18u16.to_be_bytes()); // storage offset
        name_tbl.extend_from_slice(&3u16.to_be_bytes()); // platform: Windows
        name_tbl.extend_from_slice(&1u16.to_be_bytes()); // encoding: UCS-2
        name_tbl.extend_from_slice(&0x0409u16.to_be_bytes()); // language: en-US
        name_tbl.extend_from_slice(&4u16.to_be_bytes()); // nameID: full name
        name_tbl.extend_from_slice(&(text.len() as u16).to_be_bytes());
        name_tbl.extend_from_slice(&0u16.to_be_bytes()); // string offset
        name_tbl.extend_from_slice(&text);

        let tbl_off: u32 = 12 + 16;
        let mut font = Vec::new();
        font.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // sfnt version
        font.extend_from_slice(&1u16.to_be_bytes()); // numTables
        font.extend_from_slice(&[0; 6]); // searchRange/entrySelector/rangeShift
        font.extend_from_slice(b"name");
        font.extend_from_slice(&0u32.to_be_bytes()); // checksum
        font.extend_from_slice(&tbl_off.to_be_bytes());
        font.extend_from_slice(&(name_tbl.len() as u32).to_be_bytes());
        font.extend_from_slice(&name_tbl);

        assert_eq!(full_font_name(&font).as_deref(), Some("Test Mono Bold"));
    }

    #[test]
    fn malformed_font_does_not_panic() {
        // Truncated / garbage input must return None, never index out of
        // bounds — this parser runs on bytes fetched from the network.
        assert_eq!(full_font_name(&[]), None);
        assert_eq!(full_font_name(&[0x00, 0x01, 0x00, 0x00]), None);
        assert_eq!(full_font_name(&[0xff; 64]), None);
        let mut truncated = vec![0x00, 0x01, 0x00, 0x00];
        truncated.extend_from_slice(&9u16.to_be_bytes());
        assert_eq!(full_font_name(&truncated), None);
    }
}

#[cfg(test)]
mod live_install_tests {
    //! Ignored by default: these reach the network and write to the user's
    //! font directory. Run explicitly with
    //!   cargo test --lib live_install -- --ignored --nocapture
    use super::*;

    #[test]
    #[ignore]
    fn installs_nerd_font_for_real() {
        let fam = "FiraCode Nerd Font Mono";
        let before = crate::settings::list_system_fonts().expect("picker");
        println!("BEFORE: {:?}", before.mono.iter().find(|e| e.name == fam).map(|e| e.installed));
        let r = install_blocking("firacode-nf").expect("install");
        assert!(!r.guided);
        println!("faces ({}): {:?}", r.installed.len(), r.installed);
        let after = crate::settings::list_system_fonts().expect("picker");
        let hit = after.mono.iter().find(|e| e.name == fam).map(|e| e.installed);
        println!("AFTER: {:?}", hit);
        assert_eq!(hit, Some(true), "the abbreviated Nerd Font faces must resolve to the family");
    }

    #[test]
    #[ignore]
    fn installs_jetbrains_mono_for_real() {
        let before = crate::settings::list_system_fonts().expect("picker");
        println!("BEFORE: {:?}", before.mono.iter().find(|e| e.name == "JetBrains Mono").map(|e| e.installed));
        let r = install_blocking("jetbrains-mono").expect("install");
        assert!(!r.guided);
        println!("faces ({}): {:?}", r.installed.len(), r.installed);
        assert!(!r.installed.iter().any(|f| f.contains("NL")), "the no-ligature sibling family must be filtered out");
        let after = crate::settings::list_system_fonts().expect("picker");
        println!("AFTER: {:?}", after.mono.iter().find(|e| e.name == "JetBrains Mono").map(|e| e.installed));
        assert_eq!(after.mono.iter().find(|e| e.name == "JetBrains Mono").map(|e| e.installed), Some(true));
    }

    #[test]
    #[ignore]
    fn installs_fira_code_for_real() {
        let before = crate::settings::list_system_fonts().expect("picker");
        let was = before
            .mono
            .iter()
            .find(|e| e.name == "Fira Code")
            .map(|e| e.installed);
        println!("BEFORE: Fira Code in picker = {was:?}");

        let r = install_blocking("fira-code").expect("install must succeed");
        println!("guided={} faces={:?}", r.guided, r.installed);
        assert!(!r.guided, "silent path should work on this machine");
        assert!(!r.installed.is_empty());

        let dir = user_font_dir().expect("font dir");
        for face in &r.installed {
            println!("  registered: {face}");
        }
        let on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("read font dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("FiraCode"))
            .collect();
        println!("ON DISK: {on_disk:?}");
        assert!(!on_disk.is_empty(), "files must be written");

        let after = crate::settings::list_system_fonts().expect("picker");
        let now = after
            .mono
            .iter()
            .find(|e| e.name == "Fira Code")
            .map(|e| e.installed);
        println!("AFTER: Fira Code in picker = {now:?}");
        assert_eq!(now, Some(true), "the picker must now show it installed");
    }
}
