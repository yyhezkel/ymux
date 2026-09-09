//! Phase 51.B: core infrastructure shared across the ymux backend crates.
//!
//! This crate holds the cross-cutting concerns the rest of the codebase
//! (current `app`, plus future `ymux-ssh`/`-tunnel`/`-pty`/`-rpc`) all
//! reach for: the user-visible debug log, shell-quote helper, and the
//! pure layout-tree walkers that have no state and no I/O.
//!
//! 51.B is being landed in incremental sub-commits (51.B1, 51.B2, …)
//! rather than as one ~5,000-LOC move, so intermediate states stay
//! green and the build is never left broken between commits.
//!
//! Things explicitly NOT in this crate (yet): SshClient + russh
//! handler impl, Session/SshSession types, ForwardEntry, AppState +
//! CoreState. Those land in subsequent 51.B sub-commits.

// beta.3 (netfree): shared HTTP retry helper for the updater path.
// See http.rs — GET-only, jittered exponential backoff on transport errors.
pub mod http;
mod log_writer;
pub use log_writer::flush_log;

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use portable_pty::{ChildKiller, MasterPty};
use russh::client;
use russh::Channel;
use russh_keys::HashAlg;
use serde::{Deserialize, Serialize};
use ymux_types::{Connection, LayoutNode, PaneKind};

// ─── config dir + debug.log ──────────────────────────────────────────

/// Resolve the per-user ymux config directory.
///
/// `YMUX_CONFIG_DIR` env var wins if set (used by tests + isolated
/// debug builds, see `ymux-debug-test\run-ymux-debug.bat`); the
/// pre-rename `WINMUX_CONFIG_DIR` is still honoured as a fallback so
/// existing debug harnesses keep working.
/// Otherwise: `dirs::config_dir() / "ymux"` (≈ `%APPDATA%\ymux\`).
/// The directory is created on demand.
///
/// **Rename migration (winmux → ymux).** If `%APPDATA%\ymux` does not
/// exist yet but the pre-rename `%APPDATA%\winmux` does, the old
/// directory is renamed onto the new name on first access, carrying
/// workspaces.json / settings.json / keys / machine-id across the
/// rebrand instead of silently starting from a blank profile. The
/// rename is best-effort: if it fails (old directory still held by a
/// running pre-rename build) we fall through and create a fresh
/// `ymux` directory rather than failing the whole app's boot.
pub fn config_dir() -> Result<PathBuf, String> {
    if let Ok(custom) =
        std::env::var("YMUX_CONFIG_DIR").or_else(|_| std::env::var("WINMUX_CONFIG_DIR"))
    {
        let p = PathBuf::from(custom);
        std::fs::create_dir_all(&p).map_err(|e| format!("create {:?}: {e}", p))?;
        return Ok(p);
    }
    let base = dirs::config_dir().ok_or_else(|| "no config dir available".to_string())?;
    let dir = base.join("ymux");
    // `Once`, not a plain `if !dir.exists()`: the log_* calls below route
    // back through `config_dir()` to find debug.log. Without the latch a
    // failing rename would re-enter, fail again, log again — unbounded
    // recursion. One attempt per process is also exactly the semantics we
    // want: a migration that failed won't half-apply on a later call.
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    MIGRATED.call_once(|| match migrate_legacy_config_dir(&base) {
        Ok(true) => log_info("CORE", "config dir migrated winmux -> ymux"),
        Ok(false) => {}
        Err(e) => log_warn(
            "CORE",
            &format!("config dir migration failed ({e}); continuing with a fresh profile"),
        ),
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    Ok(dir)
}

/// The winmux → ymux config-dir move, split out of `config_dir` so it can be
/// tested against a temp `base` instead of the real `%APPDATA%`.
///
/// Returns `Ok(true)` when it moved something, `Ok(false)` when there was
/// nothing to do — which covers both "never had a winmux install" and
/// "already migrated". Only ever acts when the new name is absent, so it can
/// never clobber a live ymux profile with an older winmux one.
fn migrate_legacy_config_dir(base: &std::path::Path) -> Result<bool, String> {
    let dir = base.join("ymux");
    let legacy = base.join("winmux");
    if dir.exists() || !legacy.is_dir() {
        return Ok(false);
    }
    std::fs::rename(&legacy, &dir)
        .map_err(|e| format!("rename {legacy:?} -> {dir:?}: {e}"))?;
    Ok(true)
}

/// Documented alias for `config_dir` so external module callsites
/// have a stable name. (Historical: this used to be private in lib.rs
/// with `config_dir_pub` as the cross-module surface; keeping the
/// alias means every existing callsite continues to resolve.)
pub fn config_dir_pub() -> Result<PathBuf, String> {
    config_dir()
}

/// Size cap for `debug.log` before it rotates to `debug.log.1`. Bounds the
/// on-disk footprint to ~2× this (current + one rotation) so a chatty session
/// can't balloon the log — the v0.3.1 pipe-leak produced ~936k lines.
pub const DEBUG_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Severity for the unified user-visible log. Repr matches the atomic
/// threshold below; ordering is Debug < Info < Warn < Error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl LogLevel {
    /// Parse a persisted/user-supplied level. Unknown values fall back to
    /// Info so a corrupt settings value can never silence errors.
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" => LogLevel::Debug,
            "warn" | "warning" => LogLevel::Warn,
            "error" => LogLevel::Error,
            _ => LogLevel::Info,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }

    /// Fixed-width column for the log line (`[DEBUG]`, `[INFO ]`, …) so the
    /// component tag lines up vertically when scanning the file.
    fn column(self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => LogLevel::Debug,
            2 => LogLevel::Warn,
            3 => LogLevel::Error,
            _ => LogLevel::Info,
        }
    }
}

/// Global write threshold for `log_at`. Default Info; Settings → Logs flips
/// it to Debug. Relaxed ordering is fine — a racy line during a level change
/// is harmless.
static GLOBAL_LOG_LEVEL: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub fn set_log_level(level: LogLevel) {
    GLOBAL_LOG_LEVEL.store(level as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn log_level() -> LogLevel {
    LogLevel::from_u8(GLOBAL_LOG_LEVEL.load(std::sync::atomic::Ordering::Relaxed))
}

/// Hand one already-formatted line to the log writer.
///
/// Phase 80: this used to `stat`, open, write, write and close on the
/// caller's thread, with no lock. `writeln!` on an unbuffered `File` emits
/// the payload and the newline as TWO syscalls, and `O_APPEND` makes each
/// atomic but not the pair — so concurrent callers fused each other's
/// records (~120 such lines in one day of Yossi's log), and the
/// stat-then-rename rotation could lose a whole 5 MB generation.
///
/// Now the line is terminated here and enqueued; `log_writer` owns the file
/// and issues exactly one `write_all` per batch. Errors are still swallowed
/// — logging must never crash the caller.
fn write_line(line: &str) {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    log_writer::submit(log_writer::Msg::Block(buf));
}

/// The `[ts]` field, shared by `log_at` and the writer's dropped-lines
/// marker so both read identically in the file.
pub(crate) fn log_timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f %:z")
        .to_string()
}

/// Append a line verbatim (no timestamp/level/tag prefix), still honoring
/// rotation. For the remote-log sync, whose pulled lines already carry the
/// unified prefix from their origin host. Rule 1 applies to the writers on
/// the remote side: pulled content must be log metadata, never PTY content.
pub fn append_raw_line(line: &str) {
    write_line(line);
}

/// Append many already-formatted lines as ONE write.
///
/// Phase 80: the remote-log sync pulls up to 256 KiB × 3 files × N hosts per
/// cycle, and submitting them one at a time is what turned a routine sync
/// into thousands of open/stat/write/write/close sequences — the single
/// biggest source of interleaving in the old logger, and of rotation races,
/// since each one also re-ran the size check.
pub fn append_raw_lines<I, S>(lines: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut buf = String::new();
    for l in lines {
        buf.push_str(l.as_ref());
        buf.push('\n');
    }
    if !buf.is_empty() {
        log_writer::submit(log_writer::Msg::Block(buf));
    }
}

/// The unified user-visible log line: `[ts] [LEVEL] [TAG] msg` where ts is
/// local time with UTC offset (`2026-07-15 14:32:05.123 +03:00`) so lines
/// merged from other machines still correlate. Lines below the global
/// threshold are dropped. Rule 1: never log PTY input/output content — only
/// metadata (pane IDs, byte counts, error kinds). See CLAUDE.md Rule 9 for
/// the dlog-vs-tracing audience distinction.
pub fn log_at(level: LogLevel, tag: &str, msg: &str) {
    if level < log_level() {
        return;
    }
    // Stamped at call time, not write time: the file must say when the
    // event happened. Two threads can therefore stamp and then race into the
    // queue, so file order and timestamp order can differ by microseconds —
    // as they already could before, since formatting preceded `open()` and
    // that latency dwarfs a channel send.
    let ts = log_timestamp();
    write_line(&format!(
        "[{ts}] [{}] [{}] {msg}",
        level.column(),
        tag.to_uppercase()
    ));
}

pub fn log_debug(tag: &str, msg: &str) {
    log_at(LogLevel::Debug, tag, msg);
}

pub fn log_info(tag: &str, msg: &str) {
    log_at(LogLevel::Info, tag, msg);
}

pub fn log_warn(tag: &str, msg: &str) {
    log_at(LogLevel::Warn, tag, msg);
}

pub fn log_error(tag: &str, msg: &str) {
    log_at(LogLevel::Error, tag, msg);
}

/// Legacy shim: untagged info-level line. Prefer `log_*` with a component
/// tag; kept so out-of-tree callers keep compiling.
pub fn dlog(msg: &str) {
    log_at(LogLevel::Info, "APP", msg);
}

/// Legacy shim: tagged info-level line. Prefer the leveled `log_*` family.
pub fn dlog_tag(subsystem: &str, msg: &str) {
    log_at(LogLevel::Info, subsystem, msg);
}

/// Phase 75: prune debug logs so they can't accumulate. Deletes the rotated
/// `debug.log.1` once it's older than `retention_days`, and if the primary
/// `debug.log` itself hasn't been touched within the window (app unused for a
/// while), clears it for a fresh start. `retention_days == 0` disables pruning
/// (keep forever). Called once at startup. Best-effort — never fails the boot.
///
/// Phase 80: runs on the writer thread, so it can no longer truncate the file
/// out from under an in-flight write.
pub fn prune_logs(retention_days: u32) {
    if retention_days == 0 {
        return;
    }
    log_writer::submit(log_writer::Msg::Prune(retention_days));
}

/// Phase 75: clear the debug log now (Settings → Logs "Clear" button).
/// Truncates `debug.log` and removes the rotated `debug.log.1`.
///
/// Phase 80: ordered against queued lines. Previously an in-flight write from
/// another thread could land AFTER the clear and resurrect content the user
/// had just wiped.
pub fn clear_debug_log() -> Result<(), String> {
    log_writer::clear_now()
}

// ─── shell escape ────────────────────────────────────────────────────

/// Minimal POSIX single-quote escape. Wraps the value in single quotes
/// and rewrites any internal single-quote as `'\''`. Safe for
/// /bin/sh-style. Per Absolute Rule #3, used wherever we must inject
/// caller-supplied strings into a remote shell command.
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

// ─── pure LayoutNode walkers ─────────────────────────────────────────

/// Flat-collect every pane id under the given subtree (depth-first).
pub fn collect_panes(node: &LayoutNode, out: &mut Vec<String>) {
    match node {
        LayoutNode::Pane { pane_id, .. } => out.push(pane_id.clone()),
        LayoutNode::Split { first, second, .. } => {
            collect_panes(first, out);
            collect_panes(second, out);
        }
    }
}

/// Phase 8.E: visit every leaf pane and report its kind to the callback.
/// Used by the `dev.get-state` summary builder.
pub fn collect_panes_with_kind(node: &LayoutNode, f: &mut dyn FnMut(PaneKind)) {
    match node {
        LayoutNode::Pane { pane_kind, .. } => f(*pane_kind),
        LayoutNode::Split { first, second, .. } => {
            collect_panes_with_kind(first, f);
            collect_panes_with_kind(second, f);
        }
    }
}

/// First Terminal-pane connection found in DFS order, if any.
/// Module-private in lib.rs; here we keep it `pub` for cross-crate use
/// but consumers should prefer `first_terminal_connection_pub` which
/// is the documented surface.
pub fn first_terminal_connection(node: &LayoutNode) -> Option<Connection> {
    match node {
        LayoutNode::Pane {
            pane_kind,
            connection,
            ..
        } if matches!(pane_kind, PaneKind::Terminal) => connection.clone(),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            first_terminal_connection(first).or_else(|| first_terminal_connection(second))
        }
    }
}

/// Documented alias for `first_terminal_connection` so external module
/// callsites have a stable name. (Phase 23.D introduced the
/// `_pub` suffix when this used to be private; keeping the alias means
/// every existing callsite continues to resolve.)
pub fn first_terminal_connection_pub(node: &LayoutNode) -> Option<Connection> {
    first_terminal_connection(node)
}

/// Phase 23.D: fix-up loop run at load_from_disk time.
/// Walks the workspace's layout tree and, for every Terminal pane with
/// no `connection`, fills it from the workspace-level fallback (the
/// first sibling Terminal pane's connection, or `Local{shell:None}`).
/// Returns the patched node plus a bool telling the caller whether the
/// tree was actually mutated (so persistence can be marked dirty).
pub fn backfill_terminal_connections(
    node: LayoutNode,
    workspace_conn: &Option<Connection>,
) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            browser,
            title,
            auto_title,
            annotation,
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
            diff_cwd,
        } => {
            let needs_fix =
                matches!(pane_kind, PaneKind::Terminal) && connection.is_none();
            let new_conn = if needs_fix {
                Some(
                    workspace_conn
                        .clone()
                        .unwrap_or(Connection::Local { shell: None }),
                )
            } else {
                connection
            };
            (
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection: new_conn,
                    browser,
                    title,
                    auto_title,
                    annotation,
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                    diff_cwd,
                },
                needs_fix,
            )
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            let (new_first, c1) = backfill_terminal_connections(*first, workspace_conn);
            let (new_second, c2) = backfill_terminal_connections(*second, workspace_conn);
            (
                LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(new_first),
                    second: Box::new(new_second),
                    ratio,
                },
                c1 || c2,
            )
        }
    }
}

// ─── Known-hosts (TOFU) ──────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct KnownHost {
    #[serde(rename = "type")]
    pub key_type: String,
    pub fingerprint: String,
    pub first_seen: String,
    pub last_seen: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct KnownHostsFile {
    #[serde(default)]
    pub hosts: HashMap<String, KnownHost>,
}

pub fn known_hosts_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("known_hosts.json"))
}

pub fn load_known_hosts() -> KnownHostsFile {
    if let Ok(p) = known_hosts_path() {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(f) = serde_json::from_str::<KnownHostsFile>(&text) {
                return f;
            }
        }
    }
    KnownHostsFile::default()
}

pub fn save_known_hosts(file: &KnownHostsFile) -> Result<(), String> {
    let path = known_hosts_path()?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

pub fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Default, Clone, Debug)]
pub struct HostCheckOutcome {
    pub fingerprint: String,
    pub key_type: String,
    pub matched: bool,
    pub is_unknown: bool,
    pub mismatch_old: Option<String>,
}

// ─── SshClient (russh Handler) ───────────────────────────────────────

/// Callback type for forwarded-tcpip bridge. The app crate (or a
/// future `ymux-tunnel`) supplies a closure; `ymux-core` calls it
/// when the SSH server forwards a connection back to us. Phase 51.B2
/// option β: this is how we break the SshClient → tunnel circular dep
/// without folding tunnel into core.
/// The third arg is the connection's originator (`addr:port`, as reported by
/// the SSH server in the forwarded-tcpip channel open). It is logging-only,
/// and it exists because a handshake that fails is otherwise anonymous —
/// "client closed before sending response" told us nothing about WHO closed.
pub type BridgeSpawner = Arc<dyn Fn(Channel<client::Msg>, Arc<String>, String) + Send + Sync>;

pub struct SshClient {
    pub target: String,
    pub accept_unknown: bool,
    pub result: Arc<Mutex<HostCheckOutcome>>,
    /// If set, the handler accepts forwarded-tcpip channels and bridges
    /// them via `bridge_spawner` after validating this token on the
    /// first line.
    pub tunnel_token: Option<Arc<String>>,
    /// Phase 51.B2: caller-injected spawner so this crate avoids a
    /// dep on ymux-tunnel. Forwarded channels are dropped if either
    /// `tunnel_token` or `bridge_spawner` is None.
    pub bridge_spawner: Option<BridgeSpawner>,
}

impl SshClient {
    /// Construct a tolerant client for one-shot operations (the connect
    /// wizard test, provisioning steps). Accepts any server key,
    /// doesn't touch known_hosts, no tunnel token / spawner. The
    /// host-check outcome is captured but never persisted.
    pub fn new_anonymous(target: String) -> Self {
        Self {
            target,
            accept_unknown: true,
            result: Arc::new(Mutex::new(HostCheckOutcome {
                fingerprint: String::new(),
                key_type: String::new(),
                matched: true,
                is_unknown: false,
                mismatch_old: None,
            })),
            tunnel_token: None,
            bridge_spawner: None,
        }
    }
}

#[async_trait::async_trait]
impl client::Handler for SshClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh_keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        let key_type = server_public_key.algorithm().as_str().to_string();
        let mut known = load_known_hosts();
        let mut outcome = HostCheckOutcome {
            fingerprint: fp.clone(),
            key_type: key_type.clone(),
            matched: false,
            is_unknown: false,
            mismatch_old: None,
        };
        let now = iso_now();
        let existing = known.hosts.get(&self.target).cloned();
        let accept = match existing {
            Some(entry) if entry.fingerprint == fp => {
                outcome.matched = true;
                if let Some(h) = known.hosts.get_mut(&self.target) {
                    h.last_seen = now;
                    let _ = save_known_hosts(&known);
                }
                true
            }
            Some(entry) => {
                outcome.mismatch_old = Some(entry.fingerprint);
                if self.accept_unknown {
                    // User explicitly said "replace" — overwrite the known_hosts entry.
                    known.hosts.insert(
                        self.target.clone(),
                        KnownHost {
                            key_type,
                            fingerprint: fp,
                            first_seen: now.clone(),
                            last_seen: now,
                        },
                    );
                    let _ = save_known_hosts(&known);
                    true
                } else {
                    false
                }
            }
            None => {
                outcome.is_unknown = true;
                if self.accept_unknown {
                    known.hosts.insert(
                        self.target.clone(),
                        KnownHost {
                            key_type,
                            fingerprint: fp,
                            first_seen: now.clone(),
                            last_seen: now,
                        },
                    );
                    let _ = save_known_hosts(&known);
                    true
                } else {
                    false
                }
            }
        };
        *self.result.lock().unwrap() = outcome;
        Ok(accept)
    }

    /// Phase 6.3: when the server forwards a connection back to us (via reverse
    /// tunnel `tcpip-forward`), bridge it to the local Named Pipe RPC server.
    /// Phase 51.B2: the actual bridge spawn is delegated to a caller-injected
    /// closure so ymux-core stays decoupled from the tunnel impl.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        // The originator is the remote-side peer that dialled the forwarded
        // port. russh hands it to us and we used to drop it on the floor,
        // which left every handshake failure in the tunnel unattributable.
        let peer = format!("{originator_address}:{originator_port}");
        match (self.tunnel_token.clone(), self.bridge_spawner.clone()) {
            (Some(token), Some(spawn)) => spawn(channel, token, peer),
            _ => tracing::warn!(
                "forwarded-tcpip channel from {peer} arrived but no tunnel_token/bridge_spawner set; dropping"
            ),
        }
        Ok(())
    }
}

// ─── Session types ───────────────────────────────────────────────────

/// Either a local PTY-backed session or an SSH-backed one. AppState's
/// `sessions` map (Phase 51.B5 will pull this into CoreState) is
/// keyed by session id; pane operations look up the matching variant
/// and dispatch.
pub enum Session {
    Local(LocalSession),
    Ssh(SshSession),
}

pub struct LocalSession {
    pub writer: Box<dyn Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
    /// Phase 80: WSL panes wrap their shell in `tmux new-session -A`
    /// exactly like persistent SSH panes; the name enables
    /// pane_persistence_get/list badges + pane_kill_session. Plain
    /// local panes (cmd/pwsh) always carry None — tmux can't host them.
    pub tmux_session: Option<String>,
    /// Phase 80: the explicit distro a WSL pane runs in. None on plain
    /// local panes AND on default-distro WSL panes — kill-session only
    /// fires when `tmux_session` is Some, and omitting `-d` targets the
    /// default distro, so the two None cases never conflict.
    pub wsl_distro: Option<String>,
}

pub struct SshSession {
    /// Phase 41: `None` for a headless session — one established by
    /// `workspace_ensure_connected` to back the tmux picker / file manager
    /// with no PTY behind it. Pane-backed sessions always carry `Some`.
    pub tx: Option<tokio::sync::mpsc::UnboundedSender<SshCmd>>,
    /// Phase 8.B: shared russh client handle. The I/O task and any port-forward
    /// accept loop both hold an Arc; russh's Handle methods take &self, so
    /// concurrent users send commands through the underlying mpsc sender.
    pub handle: Arc<client::Handle<SshClient>>,
    /// Phase 8.B: workspace this session belongs to, so port-forward bookkeeping
    /// can clean up when the workspace is deleted or all SSH sessions exit.
    pub workspace_id: String,
    /// Phase 11.A: when this session was started with `persistent=true` we wrap
    /// the shell in a tmux attach-or-create. Storing the name lets us send
    /// `tmux kill-session -t NAME` via a separate exec channel on demand.
    pub tmux_session: Option<String>,
    /// Phase 23.C: connection metadata so we can rehydrate a `Connection`
    /// value from a live session — used by `live_ssh_connection_for_workspace`
    /// when the user adds a new terminal pane to an SSH workspace whose
    /// connection details no longer live in any pane (e.g. all terminals
    /// closed but a FileManager pane kept the SSH handle alive).
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key_path: Option<String>,
    /// beta.3 (netfree, Track 1b): set by the io-loop when the SSH transport
    /// drops so a background reconnect task can announce itself to the UI
    /// and reject a second cascading drop-emit if the same session flaps
    /// twice in quick succession. Cleared by the `ssh_cancel_reconnect`
    /// Tauri command or when the reconnect flow completes / gives up.
    /// Arc<AtomicBool> so multiple `_for_task` clones share one flag.
    pub reconnecting: Arc<AtomicBool>,
}

impl SshSession {
    /// Phase 41: forward a command to the PTY task. Headless sessions have
    /// no PTY (`tx == None`), so this is a no-op for them. Pane operations
    /// only ever look sessions up by pane id, so in practice this only
    /// reaches `Some` senders — the `None` arm is the safety net.
    pub fn try_send(&self, cmd: SshCmd) -> Result<(), String> {
        match &self.tx {
            Some(tx) => tx.send(cmd).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}

#[derive(Debug)]
pub enum SshCmd {
    Data(Vec<u8>),
    Resize(u32, u32),
    Kill,
}

// ─── Forward bookkeeping ─────────────────────────────────────────────

/// Phase 8.B: SSH local port forwards (browser pane → remote dev server).
/// Key = (workspace_id, remote_port). Value carries the local listener port
/// and a oneshot to cancel the accept loop on cleanup.
pub struct ForwardEntry {
    pub local_port: u16,
    pub cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

pub type ForwardMap = Arc<Mutex<HashMap<(String, u16), ForwardEntry>>>;
pub type SessionMap = Arc<Mutex<HashMap<String, Session>>>;
pub type PaneSessionMap = Arc<Mutex<HashMap<String, String>>>;

// ─── pipe_name ───────────────────────────────────────────────────────

/// Phase 51.C: the per-user Windows Named Pipe path that the RPC
/// server binds to and the remote tunnel bridges into. Lives in
/// ymux-core so both ymux-tunnel (bridge_to_pipe) and the future
/// ymux-rpc (server bind) can reach it without depending on each
/// other.
#[cfg(windows)]
pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\ymux-{}", user)
}

/// Pre-rename endpoint name, kept alive alongside `pipe_name` for one
/// release.
///
/// The Windows installer leaves a `winmux-cli.exe` from an earlier
/// install on PATH, and MCP host configs point at whatever binary they
/// were set up with. Those dial `\\.\pipe\winmux-<user>` and have no way
/// to learn otherwise, so the app answers on both names rather than
/// letting every pre-rename integration fail at connect.
///
/// FOLLOWUPS P1: drop this and its listener once 0.5.0 is the floor.
#[cfg(windows)]
pub fn pipe_name_legacy() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\winmux-{}", user)
}

/// Unix counterpart of `pipe_name_legacy`.
#[cfg(not(windows))]
pub fn pipe_name_legacy() -> String {
    let user = whoami::username();
    std::env::temp_dir()
        .join(format!("winmux-{user}.sock"))
        .to_string_lossy()
        .into_owned()
}

/// Unix equivalent: a per-user Unix domain socket. `temp_dir()` honors
/// TMPDIR, which on macOS is a per-user private directory — so the
/// socket gets the same user-isolation the per-user pipe name gives
/// on Windows.
#[cfg(not(windows))]
pub fn pipe_name() -> String {
    let user = whoami::username();
    std::env::temp_dir()
        .join(format!("ymux-{user}.sock"))
        .to_string_lossy()
        .into_owned()
}

/// Second socket path to try when `pipe_name()` can't be bound.
///
/// `sockaddr_un.sun_path` is capped at 104 bytes on macOS (108 on Linux),
/// and `$TMPDIR` on macOS is a generated `/var/folders/<xx>/<hash>/T/` path
/// — normally ~66 bytes with the file name, so there's headroom, but no
/// guarantee and no way to notice when it runs out. A custom TMPDIR, a long
/// user name, or a sandboxed container can all push it over, and the only
/// symptom is that nothing that travels over the RPC socket ever arrives
/// (detected ports, CLI hooks) while SSH panes keep working, because those
/// use russh channels directly.
///
/// The config dir is short and stable (`~/Library/Application Support/ymux`
/// on macOS), so it makes a good second try. Both the server (rpc_server) and
/// the client (ymux-tunnel) must walk this list in the same order.
#[cfg(not(windows))]
pub fn pipe_name_fallback() -> Option<String> {
    let primary = pipe_name();
    let alt = config_dir().ok()?.join("rpc.sock");
    let alt = alt.to_string_lossy().into_owned();
    if alt == primary {
        None
    } else {
        Some(alt)
    }
}

/// The socket paths to try, in order. Windows has exactly one name (the
/// pipe namespace has no length problem), so this is a single-element list
/// there and callers stay platform-agnostic.
///
/// Deliberately excludes `pipe_name_legacy()`: this list is "where THIS
/// build's endpoint lives", walked by both the server and ymux-tunnel,
/// which always ship together. The legacy name is a one-release compat
/// shim for *foreign* pre-rename callers, so only the server binds it.
pub fn pipe_names() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![pipe_name()]
    }
    #[cfg(not(windows))]
    {
        let mut v = vec![pipe_name()];
        v.extend(pipe_name_fallback());
        v
    }
}

// ─── CoreState ───────────────────────────────────────────────────────

/// Phase 51.B4: the russh/PTY/forward runtime state that every future
/// split crate (tunnel, bootstrap, ssh, pty, rpc) will need, factored
/// out of AppState so ymux-core owns it instead of app. The outer
/// `AppState` (in `app/lib.rs`) holds a `core: CoreState` plus the
/// fields that depend on tauri / notes / settings / dev modules.
///
/// All fields are `Arc<Mutex<…>>` and `Clone`able, so cloning
/// CoreState (e.g. for spawning a tokio task that holds its own
/// reference) only clones the Arcs, not the data behind them.
#[derive(Default, Clone)]
pub struct CoreState {
    pub sessions: SessionMap,
    pub pane_sessions: PaneSessionMap,
    pub forwards: ForwardMap,
    pub port_watchers: Arc<Mutex<std::collections::HashSet<String>>>,
    // Phase 80: `internal_reverse_tunnel_remote_ports` and
    // `workspace_tunnel_tokens` used to live here. They were read
    // independently — the port set was sampled with `.iter().next()` and only
    // ever grew, the token map was cleared nowhere at all — so a watcher
    // could be handed one connection's port with another's token and get
    // `-DENIED bad-mac`. Both are now one triple in
    // `app::tunnel_registry::TunnelRegistry`, which lives on `AppState`
    // because it also owns the per-workspace connect lock.
    pub detected_ports:
        Arc<Mutex<HashMap<String, HashMap<u16, (String, String)>>>>,
    pub port_watcher_tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    pub diff_pane_watchers: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

// ─── Phase 59: pure-function unit tests ──────────────────────────────
//
// Targets the layout walkers + shell_quote + pipe_name. These are
// hot-path helpers — a regression here would break SSH command
// injection of caller-supplied strings, mis-fill pane connections
// on load, or send the remote tunnel to the wrong named-pipe path.

#[cfg(test)]
mod tests {
    use super::*;
    use ymux_types::{Connection, LayoutNode, PaneKind, SplitDirection};

    // ── helpers ────────────────────────────────────────────────────

    fn pane(id: &str, kind: PaneKind, conn: Option<Connection>) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.into(),
            pane_kind: kind,
            connection: conn,
            browser: None,
            title: None,
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
            diff_cwd: None,
        }
    }

    fn split(id: &str, dir: SplitDirection, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            split_id: id.into(),
            direction: dir,
            first: Box::new(first),
            second: Box::new(second),
            ratio: 0.5,
        }
    }

    // ── shell_quote (Absolute Rule #3 helper) ──────────────────────

    #[test]
    fn shell_quote_empty_string() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn shell_quote_simple_alphanumeric() {
        assert_eq!(shell_quote("hello"), "'hello'");
        assert_eq!(shell_quote("foo123"), "'foo123'");
    }

    #[test]
    fn shell_quote_path_with_slashes_unchanged() {
        // Slashes don't need escaping inside single quotes.
        assert_eq!(
            shell_quote("/home/yossi/.ssh/id_ed25519"),
            "'/home/yossi/.ssh/id_ed25519'"
        );
    }

    #[test]
    fn shell_quote_embedded_single_quote_uses_close_quote_escape() {
        // The classic POSIX trick: close the quote, insert a backslash-
        // quote, reopen. The end result is the literal four chars '\''.
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn shell_quote_multiple_single_quotes() {
        assert_eq!(shell_quote("'a''b'"), r#"''\''a'\'''\''b'\'''"#);
    }

    #[test]
    fn shell_quote_dangerous_metachars_safe() {
        // Inside single quotes, $/`/!/;/&/|/space/newline/backslash are
        // ALL literal. The threat model here is command injection on
        // the remote shell; verifying the escape leaves them inert.
        assert_eq!(
            shell_quote("$(rm -rf /); echo pwn"),
            "'$(rm -rf /); echo pwn'"
        );
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
        assert_eq!(shell_quote("a`b`c"), "'a`b`c'");
    }

    // ── collect_panes / collect_panes_with_kind ────────────────────

    #[test]
    fn collect_panes_single_leaf() {
        let n = pane("p1", PaneKind::Terminal, None);
        let mut out = Vec::new();
        collect_panes(&n, &mut out);
        assert_eq!(out, vec!["p1".to_string()]);
    }

    #[test]
    fn collect_panes_dfs_order() {
        // Tree:    s_outer
        //         /        \
        //    s_inner        p3
        //    /     \
        //   p1     p2
        // DFS-first should produce [p1, p2, p3].
        let tree = split(
            "s_outer",
            SplitDirection::Vertical,
            split(
                "s_inner",
                SplitDirection::Horizontal,
                pane("p1", PaneKind::Terminal, None),
                pane("p2", PaneKind::Terminal, None),
            ),
            pane("p3", PaneKind::Terminal, None),
        );
        let mut out = Vec::new();
        collect_panes(&tree, &mut out);
        assert_eq!(out, vec!["p1", "p2", "p3"]);
    }

    #[test]
    fn collect_panes_with_kind_visits_every_leaf() {
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("a", PaneKind::Terminal, None),
            split(
                "s2",
                SplitDirection::Vertical,
                pane("b", PaneKind::Diff, None),
                pane("c", PaneKind::Help, None),
            ),
        );
        let mut kinds: Vec<PaneKind> = Vec::new();
        collect_panes_with_kind(&tree, &mut |k| kinds.push(k));
        assert_eq!(kinds, vec![PaneKind::Terminal, PaneKind::Diff, PaneKind::Help]);
    }

    // ── first_terminal_connection ──────────────────────────────────

    #[test]
    fn first_terminal_connection_none_when_no_terminal_panes() {
        let tree = pane("h", PaneKind::Help, None);
        assert!(first_terminal_connection(&tree).is_none());
    }

    #[test]
    fn first_terminal_connection_skips_non_terminal_panes() {
        // Non-terminal pane in DFS-first slot must be skipped; the
        // search continues into the second subtree to find a real
        // Terminal pane's connection.
        let ssh = Connection::Ssh {
            host: "h".into(),
            user: "u".into(),
            port: 22,
            key_path: None,
        };
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("help", PaneKind::Help, None),
            pane("term", PaneKind::Terminal, Some(ssh.clone())),
        );
        let found = first_terminal_connection(&tree).expect("should find SSH");
        // Pattern-match — Connection has no Debug.
        match found {
            Connection::Ssh { host, .. } => assert_eq!(host, "h"),
            _ => panic!("expected SSH"),
        }
    }

    #[test]
    fn first_terminal_connection_skips_orphan_and_finds_real_connection() {
        // A Terminal pane with no connection returns None from the
        // Pane arm; the Split arm's `or_else` falls through to the
        // second subtree. So the walker effectively finds the first
        // Terminal pane that ACTUALLY has a connection in DFS order.
        // (Phase 23.D documented this as the "second tier" of the
        // four-tier fallback chain for split_pane_in.)
        let ssh = Connection::Ssh {
            host: "h2".into(),
            user: "u".into(),
            port: 22,
            key_path: None,
        };
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("orphan", PaneKind::Terminal, None),
            pane("realssh", PaneKind::Terminal, Some(ssh)),
        );
        let found = first_terminal_connection(&tree).expect("should find SSH on right");
        match found {
            Connection::Ssh { host, .. } => assert_eq!(host, "h2"),
            _ => panic!("expected SSH from the second subtree"),
        }
    }

    #[test]
    fn first_terminal_connection_returns_none_when_all_terminals_orphaned() {
        // No connection anywhere → walker returns None (and the
        // caller falls back to Connection::Local{shell:None} via
        // split_pane_in's tier-4 default).
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("orphan1", PaneKind::Terminal, None),
            pane("orphan2", PaneKind::Terminal, None),
        );
        assert!(first_terminal_connection(&tree).is_none());
    }

    // ── backfill_terminal_connections ──────────────────────────────

    #[test]
    fn backfill_does_nothing_when_no_terminal_panes_lack_connection() {
        let conn = Connection::Local { shell: None };
        let tree = pane("p1", PaneKind::Terminal, Some(conn));
        let (new_tree, changed) =
            backfill_terminal_connections(tree, &Some(Connection::Local { shell: None }));
        assert!(!changed, "no missing connection → no backfill");
        // pane_id preserved.
        match new_tree {
            LayoutNode::Pane { pane_id, .. } => assert_eq!(pane_id, "p1"),
            _ => panic!("should still be Pane"),
        }
    }

    #[test]
    fn backfill_fills_missing_terminal_pane_from_workspace_conn() {
        // Phase 23.D scenario: a Terminal pane whose connection field
        // is None must inherit the workspace-level fallback.
        let ws_conn = Connection::Ssh {
            host: "ws-host".into(),
            user: "ws-user".into(),
            port: 22,
            key_path: None,
        };
        let tree = pane("p1", PaneKind::Terminal, None);
        let (new_tree, changed) = backfill_terminal_connections(tree, &Some(ws_conn));
        assert!(changed, "missing connection should be backfilled");
        match new_tree {
            LayoutNode::Pane {
                connection: Some(Connection::Ssh { host, .. }),
                ..
            } => assert_eq!(host, "ws-host"),
            _ => panic!("connection should be filled with workspace SSH"),
        }
    }

    #[test]
    fn backfill_falls_back_to_local_when_no_workspace_conn() {
        // No workspace_conn → backfill uses Local{shell:None} so a
        // Terminal pane never ends up unconnectable.
        let tree = pane("p1", PaneKind::Terminal, None);
        let (new_tree, changed) = backfill_terminal_connections(tree, &None);
        assert!(changed);
        match new_tree {
            LayoutNode::Pane {
                connection: Some(Connection::Local { shell }),
                ..
            } => assert!(shell.is_none()),
            _ => panic!("should be Local fallback"),
        }
    }

    #[test]
    fn backfill_recurses_into_splits_changed_flag_or() {
        // changed == c1 || c2 — if only the inner subtree needed a fix
        // the bool should still propagate.
        let ws_conn = Connection::Local { shell: None };
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane(
                "good",
                PaneKind::Terminal,
                Some(Connection::Local { shell: None }),
            ),
            pane("orphan", PaneKind::Terminal, None),
        );
        let (_new_tree, changed) = backfill_terminal_connections(tree, &Some(ws_conn));
        assert!(changed, "orphan side needed backfill → changed=true");
    }

    #[test]
    fn backfill_leaves_non_terminal_panes_alone() {
        // A Help pane with no connection is correct — only Terminal
        // panes get the fix-up.
        let tree = pane("h", PaneKind::Help, None);
        let (new_tree, changed) = backfill_terminal_connections(
            tree,
            &Some(Connection::Local { shell: None }),
        );
        assert!(!changed);
        match new_tree {
            LayoutNode::Pane {
                connection,
                pane_kind,
                ..
            } => {
                assert!(matches!(pane_kind, PaneKind::Help));
                assert!(connection.is_none());
            }
            _ => panic!("should still be Pane"),
        }
    }

    // ── pipe_name ──────────────────────────────────────────────────

    #[cfg(windows)]
    #[test]
    fn pipe_name_prefixes_correctly() {
        let name = pipe_name();
        assert!(
            name.starts_with(r"\\.\pipe\ymux-"),
            "expected Windows pipe prefix, got {name}"
        );
        // Whatever USERNAME / whoami returns, it shouldn't be empty.
        assert!(name.len() > r"\\.\pipe\ymux-".len());
    }

    // ── iso_now ────────────────────────────────────────────────────

    #[test]
    fn iso_now_has_z_suffix_and_seconds_precision() {
        let s = iso_now();
        // RFC 3339 with SecondsFormat::Secs + use_z=true: e.g.
        // "2026-06-09T05:14:00Z".
        assert!(s.ends_with('Z'), "expected Z suffix, got {s}");
        // No fractional seconds (use Secs precision).
        assert!(!s.contains('.'), "no fractional seconds expected, got {s}");
        assert_eq!(s.len(), 20, "expected 20-char RFC 3339, got {s}");
    }

    // ── unified logger ─────────────────────────────────────────────

    #[test]
    fn log_level_parse_round_trip_and_fallback() {
        for l in [
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ] {
            assert_eq!(LogLevel::from_str(l.as_str()), l);
        }
        assert_eq!(LogLevel::from_str("WARNING"), LogLevel::Warn);
        assert_eq!(LogLevel::from_str(" Debug "), LogLevel::Debug);
        // Unknown / corrupt values must never silence errors → Info.
        assert_eq!(LogLevel::from_str("verbose"), LogLevel::Info);
        assert_eq!(LogLevel::from_str(""), LogLevel::Info);
    }

    #[test]
    fn log_level_ordering() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    // ── winmux → ymux config-dir migration ─────────────────────────
    //
    // The highest-stakes shim of the rename: get it wrong and a user's
    // workspaces.json / settings.json / keys / machine-id are stranded
    // under a directory nothing reads any more. Driven through
    // `migrate_legacy_config_dir` against a temp base rather than
    // `config_dir()`, which resolves the real %APPDATA%.

    fn migration_base(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("ymux-core-migrate-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migration_carries_a_pre_rename_profile_over() {
        let base = migration_base("carry");
        std::fs::create_dir_all(base.join("winmux")).unwrap();
        std::fs::write(base.join("winmux").join("workspaces.json"), b"{\"v\":1}").unwrap();

        assert_eq!(migrate_legacy_config_dir(&base), Ok(true));
        assert!(!base.join("winmux").exists(), "legacy dir should be gone, not copied");
        assert_eq!(
            std::fs::read(base.join("ymux").join("workspaces.json")).unwrap(),
            b"{\"v\":1}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn migration_never_clobbers_a_live_ymux_profile() {
        // Both directories present — the ymux one is authoritative and an
        // older winmux install must not overwrite it. This is the case that
        // would lose real data if the check were `legacy.is_dir()` alone.
        let base = migration_base("clobber");
        std::fs::create_dir_all(base.join("winmux")).unwrap();
        std::fs::write(base.join("winmux").join("workspaces.json"), b"old").unwrap();
        std::fs::create_dir_all(base.join("ymux")).unwrap();
        std::fs::write(base.join("ymux").join("workspaces.json"), b"new").unwrap();

        assert_eq!(migrate_legacy_config_dir(&base), Ok(false));
        assert_eq!(std::fs::read(base.join("ymux").join("workspaces.json")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn migration_is_a_no_op_on_a_fresh_install() {
        let base = migration_base("fresh");
        assert_eq!(migrate_legacy_config_dir(&base), Ok(false));
        assert!(!base.join("ymux").exists(), "must not create the dir itself");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_pipe_name_differs_from_the_current_one() {
        // The compat listener is pointless if both resolve to one endpoint.
        assert_ne!(pipe_name(), pipe_name_legacy());
        assert!(pipe_name_legacy().contains("winmux"));
    }

    // The fs-backed assertions share one test: YMUX_CONFIG_DIR and the
    // global level are process-wide, so splitting them would race under
    // the parallel test runner.
    //
    // Phase 80: writes are asynchronous now, so every read-back needs a
    // `flush_log()` first. Without it this test is a coin flip.
    #[test]
    fn log_at_format_threshold_and_raw_append() {
        let dir = std::env::temp_dir().join(format!("ymux-core-logtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("YMUX_CONFIG_DIR", &dir);

        set_log_level(LogLevel::Info);
        log_debug("ssh", "below threshold, dropped");
        log_info("ssh", "hello info");
        log_error("Tunnel", "boom");
        append_raw_line("[2026-07-15 09:00:00.000 +00:00] [INFO ] [SRV:CHAT] remote line");

        flush_log();
        let text = std::fs::read_to_string(dir.join("debug.log")).expect("debug.log written");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "debug line must be filtered out: {text}");
        // `[YYYY-MM-DD HH:MM:SS.mmm +HH:MM] [LEVEL] [TAG] msg` — check shape,
        // level column width, and tag uppercasing.
        assert!(
            lines[0].contains("] [INFO ] [SSH] hello info"),
            "unexpected line: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("] [ERROR] [TUNNEL] boom"),
            "unexpected line: {}",
            lines[1]
        );
        assert!(lines[0].starts_with('['), "timestamp prefix: {}", lines[0]);
        // Raw append is verbatim — no double prefix.
        assert_eq!(
            lines[2],
            "[2026-07-15 09:00:00.000 +00:00] [INFO ] [SRV:CHAT] remote line"
        );

        // Debug threshold lets debug through; legacy shims stay info-level.
        set_log_level(LogLevel::Debug);
        log_debug("fm", "now visible");
        dlog("legacy untagged");
        dlog_tag("boot", "legacy tagged");
        flush_log();
        let text = std::fs::read_to_string(dir.join("debug.log")).expect("debug.log written");
        assert!(text.contains("] [DEBUG] [FM] now visible"), "{text}");
        assert!(text.contains("] [INFO ] [APP] legacy untagged"), "{text}");
        assert!(text.contains("] [INFO ] [BOOT] legacy tagged"), "{text}");

        // ── Phase 80: the regression test for the torn lines ──
        //
        // 16 threads x 500 lines. Before the write-queue this produced
        // records fused with no newline between them, each followed by a
        // stray blank line, because `writeln!` on an unbuffered File issues
        // the payload and the "\n" as two separate syscalls.
        let before = text.lines().count();
        let mut hands = Vec::new();
        for t in 0..16 {
            hands.push(std::thread::spawn(move || {
                for i in 0..500 {
                    log_info("RACE", &format!("t{t:02}-i{i:03}"));
                }
            }));
        }
        for h in hands {
            h.join().expect("writer threads must not panic");
        }
        flush_log();
        let text = std::fs::read_to_string(dir.join("debug.log")).expect("debug.log written");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            before + 16 * 500,
            "every line must be present exactly once"
        );
        assert!(
            lines.iter().all(|l| l.starts_with('[')),
            "a line starting mid-record means two writes interleaved"
        );
        assert!(
            !text.contains("\n\n"),
            "the stray blank line is the other half of a torn record"
        );
        for t in 0..16 {
            for i in 0..500 {
                let want = format!("t{t:02}-i{i:03}");
                assert!(
                    lines.iter().any(|l| l.ends_with(&want)),
                    "lost line {want}"
                );
            }
        }

        // Restore defaults for any test that runs after us.
        set_log_level(LogLevel::Info);
        std::env::remove_var("YMUX_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
