//! Phase 6.2: bootstrap the ymux Linux binary on a remote SSH server.
//!
//! Best-effort. Called after auth succeeds, before opening the user's shell channel.
//! Detects the remote arch, hashes the existing binary (if any), and uploads via SFTP
//! when the hash doesn't match the manifest. Maintains a `~/.ymux/bin/ymux`
//! symlink to the architecture-specific binary.
//!
//! Phase 51.D: moved out of `app/src-tauri/src/remote_bootstrap.rs`. Per
//! Yossi's choice (option c): no `tauri` dep in this crate. The caller
//! (app) resolves Tauri resource paths and passes the manifest + a
//! resource-loader closure in. `bootstrap()` does all the russh+sftp
//! work without ever touching `AppHandle`.

use std::collections::HashMap;

use russh::client::Handle;
use russh::ChannelMsg;
use serde::Deserialize;

use ymux_core::{log_debug, log_error, log_info, log_warn, shell_quote, SshClient};

const REMOTE_DIR: &str = ".ymux/bin";
/// Phase tmux-conf: the per-arch-independent assets — currently just
/// `ymux-tmux.conf` — live at `~/.ymux/<file>` (sibling of `bin/`).
const REMOTE_BASE_DIR: &str = ".ymux";
const TMUX_CONF_REMOTE: &str = "tmux.conf";
const TMUX_CONF_MANIFEST_KEY: &str = "tmux-conf";
/// Per-request SFTP deadline. russh-sftp's own default is 10s, which is the
/// budget for ONE 255KiB chunk — far too tight on a busy or distant host,
/// where a single late ack killed an otherwise healthy multi-megabyte upload.
const SFTP_REQUEST_TIMEOUT_SECS: u64 = 60;

#[derive(Deserialize, Debug)]
pub struct ManifestEntry {
    pub path: String,
    pub sha256: String,
    #[allow(dead_code)]
    pub size: u64,
    #[allow(dead_code)]
    pub built_at: String,
}

#[derive(Debug)]
pub enum BootstrapStatus {
    AlreadyOk,
    Uploaded {
        bytes: usize,
        #[allow(dead_code)]
        sha256: String,
    },
    UnsupportedArch(String),
    /// We tried to converge the remote CLI onto the binary this desktop
    /// embeds and could not — the upload failed, or the post-upload hash
    /// still didn't match. Distinct from `Err`, which means the bootstrap
    /// couldn't run at all (no $HOME, unreadable manifest, dead channel).
    ///
    /// This case used to collapse into a generic `Err(String)` that the
    /// caller showed for five seconds and then discarded, so a pane would
    /// silently keep talking to a CLI of the wrong version. The desktop
    /// speaks the protocol of the binary it embeds, so a mismatch is a
    /// real functional gap and has to stay visible.
    Skew {
        expected: String,
        actual: String,
        reason: String,
    },
}

/// Caller-provided helper to read a manifest-relative resource as
/// bytes. The caller knows where the resources live (Tauri's resource
/// bundling, dev filesystem layout, etc.); this crate just calls the
/// closure with the manifest entry's `path` field.
pub type ResourceLoader<'a> = &'a (dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync);

/// Parse a Tauri-bundled remote-manifest.json. Strips a UTF-8 BOM if
/// present (PowerShell 5.1 writes one and serde_json refuses to parse).
pub fn parse_manifest(text: &str) -> Result<HashMap<String, ManifestEntry>, String> {
    let stripped = text.trim_start_matches('\u{FEFF}');
    serde_json::from_str(stripped).map_err(|e| format!("parse manifest: {e}"))
}

/// One-line summary of a (possibly multi-line) command for the log. Dumping
/// a whole multi-line snippet (e.g. the PATH rc snippet, which contains
/// `echo "ERROR ..."` lines) made `/doctor`'s recent-errors scan flag those
/// script lines as if they were real errors, and bloated debug.log.
fn cmd_summary(cmd: &str) -> String {
    let first = cmd.lines().next().unwrap_or("").trim();
    let n = cmd.lines().count();
    if n > 1 {
        format!("{first} …(+{} lines)", n - 1)
    } else {
        first.to_string()
    }
}

async fn ssh_exec(
    handle: &mut Handle<SshClient>,
    cmd: &str,
) -> Result<(String, i32), String> {
    log_debug("BOOT", &format!("bootstrap: exec '{}'", cmd_summary(cmd)));
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open exec channel: {e}"))?;
    chan.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                stderr.extend_from_slice(&data[..])
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    let stdout_str = String::from_utf8_lossy(&stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr).to_string();
    log_debug("BOOT", &format!(
        "bootstrap: exec '{}' exit={} stdout={:?} stderr={:?}",
        cmd_summary(cmd),
        exit_code,
        stdout_str.trim(),
        stderr_str.trim()
    ));
    Ok((stdout_str, exit_code))
}

fn detect_triple(uname_output: &str) -> Option<&'static str> {
    let s = uname_output.trim();
    match s {
        "Linux x86_64" => Some("x86_64-linux"),
        "Linux aarch64" => Some("aarch64-linux"),
        _ => None,
    }
}

async fn upload_via_sftp(
    handle: &mut Handle<SshClient>,
    abs_remote_path: &str,
    bytes: &[u8],
    expected_hash: &str,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    // Phase 39.D: write to a sibling `.tmp` then atomically `mv -f` it onto the
    // final name. Truncating a currently-executing binary in place returns
    // ETXTBSY, which OpenSSH SFTP reports as the generic SSH_FX_FAILURE
    // ("Failure: Failure"); rename(2) instead swaps the directory entry to a
    // fresh inode, so a still-running old binary never blocks the replace.
    //
    // The suffix carries pid + a nanosecond stamp because the name used to be
    // a bare `.tmp`: two panes reconnecting at the same instant (which is what
    // a full network drop produces) opened the SAME remote path and interleaved
    // their 2.4MB streams into it. Matches the `<name>.<pid>.tmp` convention
    // the local atomic writes already use.
    let tmp_path = format!(
        "{abs_remote_path}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    log_debug("BOOT", &format!(
        "remote bootstrap: uploading to {tmp_path} then atomic-rename to {abs_remote_path} (sha256 {expected_hash})"
    ));

    log_debug("BOOT", &format!(
        "bootstrap: opening sftp subsystem for {} ({} bytes)",
        tmp_path,
        bytes.len()
    ));
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| {
            log_error("BOOT", &format!("bootstrap: sftp channel_open failed: {e}"));
            format!("open sftp channel: {e}")
        })?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| {
            log_error("BOOT", &format!("bootstrap: sftp request_subsystem failed: {e}"));
            format!("request sftp: {e}")
        })?;
    let stream = chan.into_stream();
    let sftp = russh_sftp::client::SftpSession::new(stream)
        .await
        .map_err(|e| {
            log_error("BOOT", &format!("bootstrap: SftpSession::new failed: {e}"));
            format!("sftp init: {e}")
        })?;
    // russh-sftp defaults to a 10s deadline PER REQUEST, and each request is
    // one 255KiB chunk written strictly serially (write → await ack → write).
    // On a loaded or distant host 10s is far too tight: a 2.4MB payload is ~10
    // sequential round trips, and one slow ack aborted the whole upload. The
    // leftover we found on the server was 522,240 bytes — exactly two chunks.
    sftp.set_timeout(SFTP_REQUEST_TIMEOUT_SECS).await;
    log_debug("BOOT", "bootstrap: sftp session ready");

    let upload = async {
        let mut file = sftp.create(&tmp_path).await.map_err(|e| {
            log_error("BOOT", &format!("bootstrap: sftp.create {tmp_path} failed: {e}"));
            format!("sftp create {tmp_path}: {e}")
        })?;
        file.write_all(bytes).await.map_err(|e| {
            log_error("BOOT", &format!("bootstrap: sftp write_all failed: {e}"));
            format!("sftp write: {e}")
        })?;
        file.flush().await.ok();
        file.shutdown().await.ok();
        Ok::<(), String>(())
    }
    .await;

    // A failed upload used to return straight out and leave the partial file
    // behind forever — that is where the stale `ymux-linux-x64.tmp` on
    // Yossi's server came from. Remove it on every error path before
    // propagating, so a retry never inherits someone else's carcass.
    if let Err(e) = upload {
        if let Err(rm) = sftp.remove_file(&tmp_path).await {
            log_warn("BOOT", &format!(
                "bootstrap: could not remove partial {tmp_path} after failed upload: {rm}"
            ));
        } else {
            log_debug("BOOT", &format!("bootstrap: removed partial {tmp_path}"));
        }
        let _ = sftp.close().await;
        return Err(e);
    }
    log_debug("BOOT", "bootstrap: sftp temp upload complete");

    let _ = sftp.close().await;

    // Atomic-replace the final path with the freshly-uploaded temp file.
    let mv_cmd = format!(
        "mv -f {} {}",
        shell_quote(&tmp_path),
        shell_quote(abs_remote_path)
    );
    let (_, mv_code) = ssh_exec(handle, &mv_cmd).await?;
    if mv_code != 0 {
        log_error("BOOT", &format!(
            "bootstrap: atomic rename {tmp_path} -> {abs_remote_path} failed (exit {mv_code})"
        ));
        // Same reasoning as the upload error path: the fully-written temp is
        // ours and nothing else will ever collect it.
        let _ = ssh_exec(handle, &format!("rm -f {}", shell_quote(&tmp_path))).await;
        return Err(format!(
            "rename {tmp_path} -> {abs_remote_path}: exit {mv_code}"
        ));
    }
    log_debug("BOOT", "bootstrap: sftp upload complete (atomic rename done)");

    Ok(())
}

/// winmux → ymux rename: fold a pre-rename `~/.winmux` into `~/.ymux`.
///
/// The remote state directory holds the deployed CLI, `tmux.conf`, the
/// hook cache, `log-level`, `run/last.env` and `session-meta.json`. Left
/// alone, the rename would strand all of it and every remote would look
/// like a first-ever connect.
///
/// Runs before the hash comparison so the existing binary is found under
/// its new path and an unchanged CLI isn't needlessly re-uploaded. Moves
/// the contents rather than the directory itself, so a host where the
/// bootstrap has already created `~/.ymux` still gets the old files.
/// Best-effort: a failure here only costs a re-provision, so it is
/// logged and stepped over rather than aborting the connect.
async fn migrate_legacy_remote_dir(handle: &mut Handle<SshClient>, home: &str) {
    let legacy = format!("{home}/.winmux");
    let target = format!("{home}/{REMOTE_BASE_DIR}");
    // `cp -a` + `rm -rf` rather than `mv`: the two directories can both
    // exist (partial earlier migration), and `mv A B` would then nest A
    // *inside* B instead of merging. The `-n` keeps anything already
    // written under the new name authoritative.
    // The `rm` afterwards drops the pre-rename binaries the copy dragged
    // along: they are dead weight next to the `ymux-linux-*` the upload
    // below installs, and `pkill -f winmux-linux-x64` elsewhere would
    // still match them.
    let script = format!(
        "if [ -d {legacy} ] && [ ! -e {legacy}/.migrated-to-ymux ]; then \
           mkdir -p {target} && \
           cp -an {legacy}/. {target}/ 2>/dev/null; \
           rm -f {target}/bin/winmux {target}/bin/winmux-linux-* 2>/dev/null; \
           : > {legacy}/.migrated-to-ymux && \
           echo migrated; \
         fi",
        legacy = shell_quote(&legacy),
        target = shell_quote(&target),
    );
    match ssh_exec(handle, &script).await {
        Ok((out, 0)) if out.trim() == "migrated" => log_info(
            "BOOT",
            &format!("bootstrap: migrated remote {legacy} -> {target} (winmux -> ymux rename)"),
        ),
        Ok(_) => {}
        Err(e) => log_warn(
            "BOOT",
            &format!("bootstrap: remote {legacy} -> {target} migration failed ({e}); continuing"),
        ),
    }
}

pub async fn bootstrap(
    handle: &mut Handle<SshClient>,
    manifest: HashMap<String, ManifestEntry>,
    resource_loader: ResourceLoader<'_>,
    force: bool,
    auto_install_hooks: bool,
) -> Result<BootstrapStatus, String> {
    log_debug("BOOT", &format!(
        "bootstrap: starting (force={force} auto_install_hooks={auto_install_hooks})"
    ));

    // Identify remote.
    let (uname, code) = ssh_exec(handle, "uname -s -m").await?;
    if code != 0 {
        return Err(format!("uname failed: exit {code}"));
    }
    let triple = match detect_triple(&uname) {
        Some(t) => t,
        None => {
            log_warn("BOOT", &format!("bootstrap: unsupported arch '{}'", uname.trim()));
            return Ok(BootstrapStatus::UnsupportedArch(uname.trim().to_string()));
        }
    };
    log_debug("BOOT", &format!("bootstrap: triple = {}", triple));

    // Resolve manifest entry for this triple.
    let entry = manifest
        .get(triple)
        .ok_or_else(|| format!("no manifest entry for {triple}"))?;
    log_debug("BOOT", &format!(
        "bootstrap: manifest entry path={} sha256={}",
        entry.path, entry.sha256
    ));

    // Get remote $HOME so SFTP gets an absolute path.
    let (home_out, _) = ssh_exec(handle, "echo $HOME").await?;
    let home = home_out.trim();
    if home.is_empty() {
        return Err("empty $HOME on remote".into());
    }
    migrate_legacy_remote_dir(handle, home).await;

    let remote_dir_abs = format!("{}/{}", home, REMOTE_DIR);
    let remote_bin_abs = format!("{}/{}", remote_dir_abs, entry.path);
    let remote_symlink_abs = format!("{}/ymux", remote_dir_abs);
    log_debug("BOOT", &format!(
        "bootstrap: remote paths — dir={} bin={} symlink={}",
        remote_dir_abs, remote_bin_abs, remote_symlink_abs
    ));

    // Compare existing hash unless forced.
    if !force {
        let (sum_out, _) = ssh_exec(
            handle,
            &format!("sha256sum {remote_bin_abs} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await?;
        let remote_hash = sum_out.trim().to_lowercase();
        if remote_hash == entry.sha256.to_lowercase() {
            log_debug("BOOT", "bootstrap: hash matches existing — skipping upload");
            // Ensure symlink anyway.
            let _ = ssh_exec(
                handle,
                &format!("ln -sf {remote_bin_abs} {remote_symlink_abs}"),
            )
            .await;
            // Even when the binary is up to date, re-check the rc file
            // — the user may have wiped their shell config since the
            // last bootstrap, or this is a fresh machine that has the
            // binary cached but no PATH entry. Idempotent.
            ensure_path_in_rc(handle).await;
            if auto_install_hooks {
                ensure_hooks_installed(handle, &remote_symlink_abs).await;
            }
            return Ok(BootstrapStatus::AlreadyOk);
        }
        log_debug("BOOT", &format!(
            "bootstrap: hash mismatch — remote='{}' expected='{}' — will upload",
            remote_hash, entry.sha256
        ));
    }

    // Make dir, upload, chmod, symlink.
    ssh_exec(handle, &format!("mkdir -p {remote_dir_abs}")).await?;

    // Phase 39.D: reap any zombie port-watch from a prior session that may
    // still hold the binary's inode (e.g. orphaned by the pre-39.C pipe
    // crash). Non-fatal — pkill exits 1 when nothing matches, which is the
    // normal case; the trailing `true` keeps the channel exit clean.
    // Phase 86: the pattern used to be `[w]inmux-linux-x64$`, the pre-rename
    // binary name — it never matched the real cmdline
    // (`$HOME/.ymux/bin/ymux port-watch --workspace <id>`), which is how 15
    // orphans piled up on one server. It now matches the `<dir>/ymux
    // port-watch ` (and legacy `winmux`) invocation: the leading `/` plus the
    // verb keep it off tmux/claude/anything that merely mentions the path,
    // and the `[w]`/`[y]` bracket trick (same as the port-watch reaper)
    // stops the `sh -c` running this very command from matching itself.
    // Narrow beats broad — this runs on the user's server.
    let _ = ssh_exec(
        handle,
        "pkill -f '/([w]inmux|[y]mux) port-watch ' 2>/dev/null; sleep 0.1; true",
    )
    .await;

    let bytes = resource_loader(&entry.path)?;
    // An upload that fails is NOT a bootstrap that couldn't run — the remote
    // is simply still on a different binary. Report that as Skew so the
    // caller can surface it and gate the CLI-dependent features, instead of
    // `?`-ing out into an error string that got shown for five seconds.
    if let Err(e) = upload_via_sftp(handle, &remote_bin_abs, &bytes, &entry.sha256).await {
        log_warn("BOOT", &format!("bootstrap: upload failed, remote stays on its own binary: {e}"));
        let (cur, _) = ssh_exec(
            handle,
            &format!("sha256sum {remote_bin_abs} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await
        .unwrap_or_default();
        return Ok(BootstrapStatus::Skew {
            expected: entry.sha256.clone(),
            actual: cur.trim().to_lowercase(),
            reason: e,
        });
    }
    ssh_exec(handle, &format!("chmod 0755 {remote_bin_abs}")).await?;
    ssh_exec(
        handle,
        &format!("ln -sf {remote_bin_abs} {remote_symlink_abs}"),
    )
    .await?;

    // Verify post-upload.
    let (verify_out, _) = ssh_exec(
        handle,
        &format!("sha256sum {remote_bin_abs} | awk '{{print $1}}'"),
    )
    .await?;
    let after_hash = verify_out.trim().to_lowercase();
    if after_hash != entry.sha256.to_lowercase() {
        log_error("BOOT", &format!(
            "bootstrap: FAILED post-upload hash mismatch: got {} expected {}",
            after_hash, entry.sha256
        ));
        return Ok(BootstrapStatus::Skew {
            expected: entry.sha256.clone(),
            actual: after_hash,
            reason: "post-upload hash mismatch".into(),
        });
    }
    log_info("BOOT", "bootstrap: COMPLETE — upload verified");

    // Phase 18: add `~/.ymux/bin` to the user's shell rc file so a
    // fresh non-ymux SSH session also gets `ymux` on PATH.
    ensure_path_in_rc(handle).await;

    // Phase tmux-conf: drop the bundled scrollback-friendly tmux
    // config at `~/.ymux/tmux.conf`. Whether tmux actually loads
    // it is decided per-pane at launch time (Settings →
    // `terminal.use_ymux_tmux_config`); we always upload so the
    // toggle works without re-bootstrapping.
    ensure_tmux_conf(handle, resource_loader, home, &manifest, force).await;

    // Phase 66 (66.B): install the Claude Code permission hooks now that
    // the CLI is on the remote. Best-effort, idempotent, no-op without
    // Claude Code installed.
    if auto_install_hooks {
        ensure_hooks_installed(handle, &remote_symlink_abs).await;
    }

    Ok(BootstrapStatus::Uploaded {
        bytes: bytes.len(),
        sha256: entry.sha256.clone(),
    })
}

/// Phase 66 (66.B): best-effort `ymux setup-hooks` on the remote so a
/// freshly-bootstrapped server starts piping Claude Code permission
/// requests back to the desktop without the user running setup-hooks by
/// hand. Idempotent (the CLI skips events already present) and `--source
/// bundled` so it never depends on network access at connect time. No-op
/// when `~/.claude` is absent — the adapter reports "not detected". We do
/// NOT pass `--force`, so a user's hand-edited matcher is never clobbered.
async fn ensure_hooks_installed(handle: &mut Handle<SshClient>, symlink_abs: &str) {
    let cmd = format!("{symlink_abs} setup-hooks --agent claude --source bundled 2>&1 || true");
    match ssh_exec(handle, &cmd).await {
        Ok((out, code)) => {
            // Log only the final status line (e.g. "Done. …") — never the
            // full output, which can include remote paths.
            let last = out.trim().lines().last().unwrap_or("").trim();
            log_debug("BOOT", &format!(
                "bootstrap: setup-hooks exit={code} status={:?}",
                last
            ));
        }
        Err(e) => log_warn("BOOT", &format!("bootstrap: setup-hooks exec failed: {e}")),
    }
}

/// Phase tmux-conf: upload `ymux-tmux.conf` to `~/.ymux/tmux.conf`
/// if absent / hash drift / `force`. Best-effort — never fails the
/// bootstrap.
async fn ensure_tmux_conf(
    handle: &mut Handle<SshClient>,
    resource_loader: ResourceLoader<'_>,
    home: &str,
    manifest: &HashMap<String, ManifestEntry>,
    force: bool,
) {
    let entry = match manifest.get(TMUX_CONF_MANIFEST_KEY) {
        Some(e) => e,
        None => {
            log_warn("BOOT", "bootstrap: tmux-conf entry missing from manifest — skipping upload");
            return;
        }
    };
    let remote_base = format!("{}/{}", home, REMOTE_BASE_DIR);
    let remote_conf = format!("{}/{}", remote_base, TMUX_CONF_REMOTE);

    if !force {
        let (sum_out, _) = match ssh_exec(
            handle,
            &format!("sha256sum {remote_conf} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                log_warn("BOOT", &format!("bootstrap: tmux-conf hash check failed: {e}"));
                return;
            }
        };
        if sum_out.trim().to_lowercase() == entry.sha256.to_lowercase() {
            log_debug("BOOT", "bootstrap: tmux-conf hash matches — skipping upload");
            return;
        }
    }

    if let Err(e) = ssh_exec(handle, &format!("mkdir -p {remote_base}")).await {
        log_warn("BOOT", &format!("bootstrap: mkdir for tmux-conf failed: {e}"));
        return;
    }
    let bytes = match resource_loader(&entry.path) {
        Ok(b) => b,
        Err(e) => {
            log_warn("BOOT", &format!("bootstrap: read tmux-conf bundle failed: {e}"));
            return;
        }
    };
    if let Err(e) = upload_via_sftp(handle, &remote_conf, &bytes, &entry.sha256).await {
        log_warn("BOOT", &format!("bootstrap: upload tmux-conf failed: {e}"));
        return;
    }
    let _ = ssh_exec(handle, &format!("chmod 0644 {remote_conf}")).await;
    // Phase 65 (bug EE): the round-4 `tmux source-file` auto-apply was
    // removed. The conf now ships `mouse off`, and mouse-on is set
    // per-session via the new-session command chain (`\; set -g mouse on`,
    // see pane connect). Re-sourcing the conf into a running server would
    // reset `mouse` back to off globally and fight that injection on
    // every new pane. New sessions still pick up the conf via `-f`.
    log_info("BOOT", &format!(
        "bootstrap: tmux-conf uploaded ({} bytes)",
        bytes.len()
    ));
}

/// Shell snippet that idempotently appends `~/.ymux/bin` to the
/// user's shell rc file. Shared between the bootstrap auto-fire
/// (best-effort, silent) and the Provisioning Wizard's
/// `AddYmuxToPath` step (visible, ✓-in-the-log).
pub const PATH_RC_SNIPPET: &str = r#"
set -e
SH="$(basename "${SHELL:-/bin/bash}")"
case "$SH" in
  zsh)  RC="$HOME/.zshrc";    LINE='export PATH="$HOME/.ymux/bin:$PATH"' ;;
  fish) RC="$HOME/.config/fish/config.fish"; LINE='set -gx PATH $HOME/.ymux/bin $PATH' ;;
  *)    RC="$HOME/.bashrc";   LINE='export PATH="$HOME/.ymux/bin:$PATH"' ;;
esac
mkdir -p "$(dirname "$RC")" 2>/dev/null || true
touch "$RC" 2>/dev/null || { echo "ERROR cannot touch $RC"; exit 0; }
if grep -q 'ymux/bin' "$RC" 2>/dev/null; then
  echo "EXISTS $RC"
else
  printf '\n# Added by ymux — keep `ymux` on PATH\n%s\n' "$LINE" >> "$RC" || {
    echo "ERROR cannot write to $RC"; exit 0;
  }
  echo "ADDED $RC"
fi
"#;

async fn ensure_path_in_rc(handle: &mut Handle<SshClient>) {
    let result = ssh_exec(handle, PATH_RC_SNIPPET).await;
    match result {
        Ok((out, _exit)) => {
            let line = out.trim();
            if line.starts_with("ADDED ") {
                log_info("BOOT", &format!(
                    "bootstrap: added PATH entry to {}",
                    line.trim_start_matches("ADDED ").trim()
                ));
            } else if line.starts_with("EXISTS ") {
                log_debug("BOOT", &format!(
                    "bootstrap: PATH already configured in {}",
                    line.trim_start_matches("EXISTS ").trim()
                ));
            } else {
                log_debug("BOOT", &format!("bootstrap: ensure_path_in_rc: {line}"));
            }
        }
        Err(e) => log_warn("BOOT", &format!("bootstrap: ensure_path_in_rc failed: {e}")),
    }
}
