fn main() {
    check_staged_resources();
    emit_build_metadata();
    // Phase 65 (build reliability): re-run the build script — and thus
    // re-embed the frontend via generate_context! — whenever the built
    // `dist/` changes. Without this, a pure-frontend change (no .rs edit)
    // could leave the OLD frontend embedded in the binary: the symptom
    // was build #5 shipping stale JS (the new wheel diagnostics never
    // appeared). Belt-and-suspenders over tauri_build's own watching.
    println!("cargo:rerun-if-changed=../dist");
    let mut attributes = tauri_build::Attributes::new();
    if embed_common_controls_manifest() {
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    if let Err(e) = tauri_build::try_build(attributes) {
        eprintln!("error: tauri_build failed: {e:#}");
        std::process::exit(1);
    }
}

// tauri-build embeds its Common-Controls-v6 manifest into the BIN only, so
// the `cargo test` exe of the lib loads comctl32 v5, which has no
// TaskDialogIndirect, and dies before main with 0xc0000139
// (STATUS_ENTRYPOINT_NOT_FOUND). Embedding the same manifest through the
// linker covers every target — the bin and the test exe alike — and the
// default one is switched off above so the bin does not get two.
// windows-app-manifest.xml is a verbatim copy of tauri-build's default.
fn embed_common_controls_manifest() -> bool {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return false;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    true
}

// Fail fast when the gitignored staged CLI is absent (fresh checkout or
// worktree). Without it tauri_build / include_bytes! die later with an
// opaque "resource path doesn't exist" error. Windows stages
// `ymux-cli.exe`, macOS stages `ymux-cli`; either satisfies the build.
fn check_staged_resources() {
    let resources = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
    let candidates = [resources.join("ymux-cli.exe"), resources.join("ymux-cli")];
    for path in &candidates {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if candidates.iter().any(|p| p.is_file()) {
        return;
    }
    eprintln!(
        "error: staged CLI missing: neither {} nor {} exists.\n\
         These files are gitignored build outputs, so a fresh checkout or worktree lacks them.\n\
         Run `npm run build:linux-cli` from `app/` first, then rebuild.",
        candidates[0].display(),
        candidates[1].display()
    );
    std::process::exit(1);
}

// Phase 8.E: emit `YMUX_GIT_HASH` and `YMUX_BUILD_TIME` so the dev
// introspection RPC can show what's running. Falls back to "unknown" if git
// isn't available (e.g. building from a tarball).
fn emit_build_metadata() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=YMUX_GIT_HASH={hash}");

    // The COMMIT time, not the wall clock. `SystemTime::now()` here changed
    // the emitted value on every build-script run, which forced a full
    // recompile of the consuming crate each time -- and for the CLI it made
    // the staged binary differ byte-for-byte between two builds of identical
    // source, contradicting build-linux-cli.ps1's own reproducibility claim
    // and dirtying the app crate through include_bytes!. Pairs naturally with
    // YMUX_GIT_HASH: both now answer "what source is this?".
    let build_time = std::process::Command::new("git")
        .args(["log", "-1", "--format=%ct"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    println!("cargo:rustc-env=YMUX_BUILD_TIME={build_time}");

    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
