// ymux CLI client.
//
// Transport selection:
// - On Windows, by default the CLI talks to the running ymux app over a per-user
//   named pipe at `\\.\pipe\ymux-<USER>`. Override with the `YMUX_PIPE_PATH` env var.
// - On Linux/Unix (and as a Windows fallback when set), the CLI connects over TCP using
//   the address in `YMUX_SOCKET_ADDR` (e.g. `127.0.0.1:8765`). This is the path used
//   when the binary runs on a remote SSH server tunneled back to a local listener.
// - If a Linux build can't find `YMUX_SOCKET_ADDR`, it errors with exit code 2.

mod hooks;
mod port_watch;
mod session_meta;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Read;
use std::process::ExitCode;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Phase 18.1: append a single-line trace entry to
/// `~/.ymux/hook-debug.log` (remote-side debug file). Used by the
/// claude-hook subcommand to record every invocation + branch
/// decision so user-reported permission-mode / matcher issues can
/// be diagnosed by looking at the log instead of reproducing live.
/// Best-effort: any I/O failure is silently swallowed so the hook
/// never aborts due to logging.
///
/// Rule #1: callers pass **metadata only** (enum-like fields, IDs,
/// error kinds) — never `tool_input`, `command`, `prompt`,
/// `transcript`, or any other content that could originate from a
/// user prompt or an agent tool call. The `msg` string reaches disk
/// verbatim and later becomes an LLM-readable file (`cat`, the
/// desktop's Addon Logs panel), so any markup mimicking system
/// messaging — e.g. `<system-reminder>…</system-reminder>` — that
/// leaks in here becomes a prompt-injection payload for whichever
/// LLM reads the log next.
fn hook_dlog(msg: &str) {
    hook_log(HookLevel::Info, msg);
}

/// Severity for hook log lines. Mirrors `ymux_core::LogLevel` (the CLI
/// stays dependency-light, so it carries its own copy) and renders in the
/// system-wide unified format: `[ts] [LEVEL] [HOOK] msg`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HookLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl HookLevel {
    /// Fixed-width column so merged logs align (`[DEBUG]`, `[INFO ]`, …).
    fn column(self) -> &'static str {
        match self {
            HookLevel::Debug => "DEBUG",
            HookLevel::Info => "INFO ",
            HookLevel::Warn => "WARN ",
            HookLevel::Error => "ERROR",
        }
    }
}

/// Write threshold, resolved once per process (each hook is a fresh
/// process, so "once" is effectively "per hook invocation"):
/// `YMUX_HOOK_VERBOSE` → Debug (back-compat), else the desktop-pushed
/// `~/.ymux/log-level` file, else Info.
fn hook_level_threshold() -> HookLevel {
    static THRESHOLD: std::sync::OnceLock<HookLevel> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        if hook_verbose() {
            return HookLevel::Debug;
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        let Some(home) = home else {
            return HookLevel::Info;
        };
        let path = std::path::PathBuf::from(home).join(".ymux").join("log-level");
        match std::fs::read_to_string(&path) {
            Ok(s) => match s.trim().to_ascii_lowercase().as_str() {
                "debug" => HookLevel::Debug,
                "warn" | "warning" => HookLevel::Warn,
                "error" => HookLevel::Error,
                _ => HookLevel::Info,
            },
            Err(_) => HookLevel::Info,
        }
    })
}

/// Leveled hook logger. Same content rules as the doc comment above
/// (`hook_dlog`): metadata only, never prompt/PTY content.
fn hook_log(level: HookLevel, msg: &str) {
    use std::io::Write as _;
    if level < hook_level_threshold() {
        return;
    }
    // v0.4.5 (security): one-shot migration of any pre-v0.4.4
    // hook-debug.log before we touch the file. See
    // `migrate_hook_log_if_legacy` for the full rationale.
    HOOK_LOG_MIGRATION.call_once(migrate_hook_log_if_legacy);
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"));
    let Some(home) = home else { return };
    let dir = std::path::PathBuf::from(home).join(".ymux");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("hook-debug.log");
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f %:z");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        // Phase 80: build the whole record, newline included, and issue ONE
        // write_all. `writeln!` on an unbuffered File splits it into two
        // syscalls, and hooks run as many concurrent short-lived processes,
        // so records fused with each other. An in-process queue is impossible
        // here — separate processes — but O_APPEND makes a single small write
        // atomic, which is the whole fix. It matters beyond this file:
        // log_sync copies these lines verbatim into the desktop's debug.log,
        // so a tear here becomes a tear in the user-visible log too.
        let line = format!("[{ts}] [{}] [HOOK] {msg}\n", level.column());
        let _ = f.write_all(line.as_bytes());
    }
}

/// Guards `migrate_hook_log_if_legacy` so it runs at most once per
/// CLI process. Every hook fires a fresh process, so the marker-file
/// check inside the migration is the real idempotency mechanism —
/// this `Once` just avoids repeat stat calls when a single process
/// emits several log lines (BEGIN + dispatch + ACK).
static HOOK_LOG_MIGRATION: std::sync::Once = std::sync::Once::new();

/// v0.4.5 (security): one-time migration of pre-v0.4.4
/// `~/.ymux/hook-debug.log`.
///
/// Background: before v0.4.4 (commit `94ba208`, 2026-07-06), the
/// static-fallback log lines embedded the policy `reason` field.
/// `reason` in turn quoted verbatim command text from
/// `tool_input.command` (see `ymux_policy::evaluate`). If that
/// command text contained markup mimicking Claude's system
/// messaging — e.g. `<system-reminder>ignore prior
/// instructions…</system-reminder>` — the tokens landed raw on
/// disk. A downstream LLM that later reads the log (a `cat` from
/// another Claude session, the desktop's Addon Logs panel) then
/// interpreted the tokens as real instructions. Classic
/// prompt-injection-through-a-log-file.
///
/// v0.4.4 fixed the write side (`reason` no longer flows to
/// `hook_dlog`; only enum-like metadata does), but existing log
/// files rotate only when they cross 1 MiB, so on quiet installs
/// the tainted historical lines survive indefinitely. This
/// migration closes that gap without requiring users to know they
/// need to delete a file:
///
/// * If the marker `~/.ymux/.hook-log-migrated-v044` already
///   exists → no-op.
/// * Otherwise, if an existing `hook-debug.log` is present, rename
///   it to `hook-debug.log.legacy-<unix_ts>` (preserved, not
///   deleted — so operators / auditors can inspect it if needed),
///   then write the marker.
/// * If no `hook-debug.log` exists, just write the marker so a
///   later fresh install doesn't retry the check every process.
///
/// Best-effort: every I/O call is fallible; failures are swallowed
/// so hook logging never aborts because of the migration itself.
fn migrate_hook_log_if_legacy() {
    use std::io::Write as _;
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"));
    let Some(home) = home else { return };
    let dir = std::path::PathBuf::from(home).join(".ymux");
    let marker = dir.join(".hook-log-migrated-v044");
    if marker.exists() {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let log_path = dir.join("hook-debug.log");
    if log_path.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = dir.join(format!("hook-debug.log.legacy-{ts}"));
        // If the rename fails (e.g. cross-device weirdness on an
        // unusual mount), fall through and still lay the marker.
        // Continuing to append to a possibly-tainted file is worse
        // than doing nothing, but retrying every process forever is
        // also worse — the marker breaks the retry loop.
        let _ = std::fs::rename(&log_path, &backup);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&marker)
    {
        // Content is an audit note for humans grepping ~/.ymux;
        // the marker's existence is what actually gates the check.
        let _ = writeln!(
            f,
            "ymux CLI hook-debug.log migration (v0.4.5)\n\
             Pre-v0.4.4 log lines could embed raw command text (from the\n\
             static-fallback `reason` field); any prior hook-debug.log was\n\
             renamed to hook-debug.log.legacy-<unix_ts>. Do not delete this\n\
             marker — it prevents the migration from re-running."
        );
    }
}

/// v0.4.4: deep-debug gate for the hook logger. The default hook path now
/// writes only concise one-line summaries (REQ/ACK/auto-allow/passive) to
/// keep hook-debug.log readable — a normal 4-minute Claude pipeline used to
/// spew ~4 lines PER tool call (BEGIN + branch + dispatch + rpc-ok). Set
/// `YMUX_HOOK_VERBOSE=1` (or true/yes) in the pane's environment to restore
/// the full per-branch trace for diagnosing matcher / permission-mode issues.
fn hook_verbose() -> bool {
    matches!(
        std::env::var("YMUX_HOOK_VERBOSE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Debug-level trace line: lands only when the threshold is Debug (via
/// `YMUX_HOOK_VERBOSE` or a desktop-pushed `~/.ymux/log-level` of
/// "debug"). Interesting/rare events (denials, timeouts, static fallbacks,
/// RPC errors) use `hook_log(Warn/Error, ..)` so they always land.
/// Like `hook_dlog`, callers pass metadata only — never PTY / prompt content.
fn hook_vlog(msg: &str) {
    hook_log(HookLevel::Debug, msg);
}

/// Phase 66 (66.D.1): the CLI's own copy of the permission policy, used
/// ONLY when the desktop is unreachable. The historical failure mode was
/// that hook calls never reached the desktop, the blocking request timed
/// out, and the timeout was conservatively denied — so Claude could not
/// run a single tool (not even a read). With this fallback the agent keeps
/// moving on safe defaults even with no desktop: Block → deny; Auto →
/// allow; Gate → allow (there's nobody to approve a card here — the
/// desktop path is what actually gates). Returns (permissionDecision,
/// reason).
fn static_fallback_decision(payload: &Value) -> (&'static str, String) {
    let tool_name = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let bash_cmd = payload
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(|c| c.as_str());
    let verdict = ymux_policy::evaluate(tool_name, bash_cmd);
    match verdict.decision {
        ymux_policy::Decision::Block => ("deny", verdict.reason),
        ymux_policy::Decision::Gate => (
            "allow",
            format!(
                "{} [static fallback: desktop unreachable, allowing]",
                verdict.reason
            ),
        ),
        ymux_policy::Decision::Auto => ("allow", verdict.reason),
    }
}

/// Phase 66 (66.D.2): print the Claude Code v2.1+ PreToolUse hook output
/// (exit 0 + this JSON is the in-band signaling).
fn print_pre_tool_use(decision: &str, reason: Option<&str>) {
    let mut hso = json!({
        "hookEventName": "PreToolUse",
        "permissionDecision": decision,
    });
    if let Some(r) = reason {
        hso["permissionDecisionReason"] = json!(r);
    }
    let out = json!({ "hookSpecificOutput": hso });
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
}

/// Phase 66 (66.D.2): feed.push with a small retry on *connection* failure.
/// Only connection errors retry — a returned `timeout` verdict means we DID
/// reach the desktop, so it's surfaced as-is (retrying would double the
/// user's wait).
async fn feed_push_with_retry(params: &Value, max_attempts: u32) -> Result<Value, String> {
    let mut last = String::from("no attempts");
    for attempt in 0..max_attempts {
        match rpc_call("feed.push", params.clone()).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                hook_log(
                    HookLevel::Warn,
                    &format!("claude-hook feed.push attempt={attempt} err={e}"),
                );
                last = e;
                if attempt + 1 < max_attempts {
                    let ms = if attempt == 0 { 200 } else { 500 };
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                }
            }
        }
    }
    Err(last)
}

/// Read the local ymux-server API token (`~/.ymux/server/token`). Used to
/// authenticate the B-path hook forward. Best-effort — None if absent.
fn read_server_token() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(".ymux/server/token");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// B path (Phase 77 push): fire-and-forget POST of a desktop-origin hook to the
/// LOCAL ymux-server (127.0.0.1:7879) so paired phones get a push. The desktop
/// stays the decision authority in this stage. Best-effort over a raw TCP HTTP
/// request (no extra deps); ANY failure is swallowed so the desktop/A path is
/// never affected. The token is never logged (Rule #8).
async fn forward_hook_to_server(
    req_id: &str,
    pane_id: &str,
    tool_name: &str,
    title: &str,
    timeout_secs: u64,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(token) = read_server_token() else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = json!({
        "req_id": req_id,
        "workspace_id": "",
        "pane_id": pane_id,
        "tool_name": tool_name,
        "title": title,
        "timeout_at": now + timeout_secs,
    })
    .to_string();
    let req = format!(
        "POST /api/v2/hooks/forward HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let dur = std::time::Duration::from_secs(3);
    let Ok(Ok(mut s)) =
        tokio::time::timeout(dur, tokio::net::TcpStream::connect("127.0.0.1:7879")).await
    else {
        return;
    };
    if s.write_all(req.as_bytes()).await.is_err() {
        return;
    }
    let _ = s.flush().await;
    // Read + discard the response so the server finishes processing the request.
    let mut buf = [0u8; 256];
    let _ = tokio::time::timeout(dur, s.read(&mut buf)).await;
}

#[derive(Parser, Debug)]
#[command(
    name = "ymux",
    version,
    about = "ymux CLI client (talks to a running ymux app via named pipe or TCP)"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,

    /// Print raw RPC response (single-line JSON) instead of pretty.
    #[arg(long, global = true)]
    raw: bool,
    /// Suppress normal output on success (errors still printed).
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    ListWorkspaces,
    SelectWorkspace {
        #[arg(long)]
        id: String,
    },
    NewWorkspace {
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "local")]
        r#type: String,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long)]
        key_path: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        color: Option<String>,
        /// Phase 7.C: command run after the shell prompt is ready.
        #[arg(long)]
        setup: Option<String>,
        /// Phase 7.C: command sent right before disconnect.
        #[arg(long)]
        teardown: Option<String>,
        /// Phase 7.C: env var (KEY=VALUE). Repeat for multiple.
        #[arg(long = "env")]
        env: Vec<String>,
    },

    /// Update an existing workspace's editable fields. Phase 7.C.
    UpdateWorkspace {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        setup: Option<String>,
        #[arg(long)]
        teardown: Option<String>,
        /// Repeat for each var. Passing --env without any clears all.
        #[arg(long = "env")]
        env: Vec<String>,
        /// Force-replace env even if no --env flags given (default behavior is
        /// "leave env alone if no --env flag was passed").
        #[arg(long)]
        clear_env: bool,
    },
    DeleteWorkspace {
        #[arg(long)]
        id: String,
    },
    Send {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        data: String,
    },
    SendKey {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        key: String,
    },
    Notify {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        workspace_id: Option<String>,
    },
    Tree {
        #[arg(long)]
        workspace_id: Option<String>,
    },
    SetStatus {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        text: String,
    },

    /// Set a persistent title on a pane (Phase 7.A). Pass an empty string to clear.
    SetPaneTitle {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        title: String,
    },

    /// Set a persistent free-text annotation on a pane (Phase 7.A). Empty clears.
    SetPaneAnnotation {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        annotation: String,
    },

    /// Phase 8.A: split a pane (terminal or browser).
    Split {
        /// Pane id of the existing pane to split off.
        #[arg(long)]
        pane: String,
        /// `right`/`horizontal` (default) or `down`/`vertical`.
        #[arg(long, default_value = "right")]
        direction: String,
        /// `terminal` (default) inherits the pane's connection; `browser` opens an iframe.
        #[arg(long, default_value = "terminal")]
        kind: String,
        /// Initial URL when --kind=browser. Defaults to about:blank.
        #[arg(long)]
        url: Option<String>,
    },

    /// Phase 8.A: navigate a browser pane to a new URL (history is pushed).
    BrowserNavigate {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        url: String,
    },

    /// Phase 8.A: pop the browser pane's history once.
    BrowserGoBack {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.A: reset the browser pane to its home URL.
    BrowserGoHome {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.B: resolve a URL through the workspace's port-forward map.
    /// Opens a forward if needed; prints the rewritten URL. Useful for agents
    /// that want to share a URL with the user that actually works on Windows.
    BrowserResolveUrl {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        url: String,
    },

    /// Phase 8.C: read the persisted current URL of a browser pane.
    BrowserUrl {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.C: read the navigation history of a browser pane.
    BrowserHistory {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.C: block until the iframe fires onload (default 10s). Returns
    /// the loaded URL on success.
    BrowserWait {
        #[arg(long)]
        pane: String,
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.1 (replaces the original 8.C eval): evaluate JS inside the
    /// iframe via the postMessage bridge and return the result. Works on
    /// cross-origin pages — the bridge runs in every frame at document
    /// creation time.
    BrowserEval {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        expr: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.C: capture a screenshot of the pane (html2canvas). With --output,
    /// writes a PNG to disk; otherwise prints the data URL. Cross-origin iframe
    /// content renders as blanks under html2canvas.
    BrowserScreenshot {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        output: Option<String>,
        #[arg(long, default_value_t = 15_000)]
        timeout_ms: u64,
    },

    /// Phase 8.E: developer / introspection tools. See `ymux dev --help`.
    Dev {
        #[command(subcommand)]
        op: DevOp,
    },

    /// Phase 8.F.1: click a CSS-selector match in a browser pane's iframe.
    /// Works on cross-origin pages (the bridge script runs in every frame).
    BrowserClick {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        selector: String,
        /// "left" (default) or "right"
        #[arg(long, default_value = "left")]
        button: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.2: semantic element search inside the iframe. Filters AND
    /// together — at least one must be specified. Returns matching elements
    /// with synthesized stable selectors usable for browser-click / type.
    BrowserFind {
        #[arg(long)]
        pane: String,
        /// ARIA role: button, link, textbox, checkbox, heading, listitem, ...
        #[arg(long)]
        role: Option<String>,
        /// Visible text content (case-insensitive contains)
        #[arg(long)]
        text: Option<String>,
        /// Accessible label (`aria-label` or `<label for>`)
        #[arg(long)]
        label: Option<String>,
        /// `placeholder` attribute (case-insensitive contains)
        #[arg(long)]
        placeholder: Option<String>,
        /// `alt` attribute on images
        #[arg(long)]
        alt: Option<String>,
        /// `title` attribute
        #[arg(long)]
        title: Option<String>,
        /// `data-testid` attribute (exact match)
        #[arg(long)]
        testid: Option<String>,
        /// Raw CSS selector to narrow the search pool before filters run.
        #[arg(long)]
        selector: Option<String>,
        /// Skip elements with display:none / visibility:hidden / zero area.
        #[arg(long)]
        visible_only: bool,
        /// Return only the first match.
        #[arg(long)]
        first: bool,
        /// Cap on number of matches returned.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.3a: poll the iframe until the criteria are met or timeout.
    /// At least one criterion must be specified; multiple AND together.
    /// Default state is `visible` — the matched element must also be visible.
    BrowserWaitFor {
        #[arg(long)]
        pane: String,
        /// CSS selector to find.
        #[arg(long)]
        selector: Option<String>,
        /// Visible text content (deepest-match, same as browser-find).
        #[arg(long)]
        text: Option<String>,
        /// ARIA role.
        #[arg(long)]
        role: Option<String>,
        /// Accessible label (`aria-label` or `<label for>`).
        #[arg(long)]
        label: Option<String>,
        /// `data-testid` attribute (exact).
        #[arg(long)]
        testid: Option<String>,
        /// Substring the iframe's `window.location.href` must contain.
        #[arg(long)]
        url_contains: Option<String>,
        /// `visible` (default) | `attached` | `hidden` | `detached`. Detached
        /// inverts: succeeds when NO element matches.
        #[arg(long, default_value = "visible")]
        state: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.2: simplified accessibility-flavored DOM tree of the iframe.
    /// JSON by default; --text renders as a YAML-like outline.
    BrowserSnapshot {
        #[arg(long)]
        pane: String,
        #[arg(long, default_value_t = 50)]
        max_depth: usize,
        /// Strip non-essential attributes (level/url/src/alt/name) — keeps
        /// only role + text + children.
        #[arg(long)]
        text_only: bool,
        /// Render the tree as an indented YAML-ish outline instead of JSON.
        #[arg(long)]
        text: bool,
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.1: type text into an input/textarea matched by CSS selector.
    BrowserType {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        selector: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        clear_first: bool,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Stub for Claude Code agent hooks: reads JSON from stdin, fires a notify.
    ClaudeHook {
        subcommand: String,
    },

    /// Quick-capture notes (Phase 7.B). See `ymux note --help` for subcommands.
    Note {
        #[command(subcommand)]
        op: NoteOp,
    },

    /// Register agent hooks (e.g. Claude Code's hooks.json) so AI agents pipe
    /// permission requests + lifecycle events through ymux. Idempotent and additive.
    SetupHooks {
        /// Which agent's config to install. `claude` (default) or `all`.
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Print what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Replace any existing ymux hook entries even if already registered.
        #[arg(long)]
        force: bool,
        /// Phase 18: where to load the hook spec from. `github` (default) pulls
        /// from raw.githubusercontent.com/yyhezkel/ymux/main/hooks/<agent>.json
        /// with a `~/.ymux/cache/hooks/<agent>.json` fallback; `bundled`
        /// uses the hardcoded spec compiled into this CLI; `url=<custom>`
        /// fetches from an arbitrary HTTPS URL.
        #[arg(long, default_value = "github")]
        source: String,
        /// Optional version pin. Currently informational — emitted into
        /// settings.json's `ymux_hooks_version` field so the desktop's
        /// outdated-check can compare to manifest.
        #[arg(long, default_value = "latest")]
        hooks_version: String,
        /// Phase 18.1: which PreToolUse matcher to install. `restrictive`
        /// (default) keeps whatever the loaded spec uses — currently
        /// `Bash|Write|Edit|MultiEdit|NotebookEdit|Task`; `all` overrides
        /// it to `.*` so every tool routes through ymux's card;
        /// `custom` is a no-op — caller is hand-managing the matcher.
        #[arg(long, default_value = "restrictive")]
        matcher_mode: String,
    },

    /// Phase 9.A: read or modify persisted app settings.
    /// See `ymux settings --help` for subcommands.
    Settings {
        #[command(subcommand)]
        op: SettingsOp,
    },

    /// Phase 11.A: disconnect a pane. For persistent panes, --kill also
    /// destroys the multiplexer session (otherwise it just detaches).
    PaneDisconnect {
        #[arg(long)]
        pane: String,
        /// Also destroy the pane's multiplexer session — tmux over SSH/WSL,
        /// zellij on a native Windows pane — INCLUDING zellij's saved copy.
        /// No resume, no resurrect.
        #[arg(long)]
        kill: bool,
    },

    /// Phase 11.A: destroy the multiplexer session bound to a pane, and its
    /// saved copy. No resume, no resurrect.
    /// Convenience alias for `pane-disconnect --pane <id> --kill`.
    PaneKillSession {
        #[arg(long)]
        pane: String,
    },

    /// Phase 11.A: list every pane currently bound to a tmux persistent
    /// session, as `{ pane_id: tmux_session_name, ... }`.
    PanePersistenceList,

    /// Phase 12.B: list recent Claude Code sessions on the workspace's host.
    /// For SSH workspaces the live SSH handle is reused; for local workspaces
    /// the local ~/.claude/projects tree is walked. Returns JSON.
    ClaudeSessionsList {
        #[arg(long)]
        workspace: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },

    /// Phase 36 (#2.2): long-running listening-port watcher. Scans
    /// /proc/net/tcp(6) every 500ms and reports new/closed LISTEN ports
    /// to the ymux app, which opens/closes SSH local-forwards.
    /// Launched automatically by the app on SSH connect — not meant to
    /// be run by hand. Runs until killed (the exec channel dies with
    /// the workspace's SSH session).
    PortWatch {
        /// Workspace id this watcher reports for.
        #[arg(long)]
        workspace: String,
    },
    /// Phase 48-C: print a JSON diagnostic snapshot of the running
    /// ymux app (version, workspaces, PTY count, RPC handlers served,
    /// bundled Linux CLI sha256, recent errors). Calls the `doctor`
    /// RPC method — requires the desktop app to be running.
    Doctor,

    /// Multi-machine sync: read/update `~/.ymux/session-meta.json`,
    /// the server-side tmux-session → Claude-session/label/origin map.
    /// Purely local file ops — no RPC, works without the desktop app.
    SessionMeta {
        #[command(subcommand)]
        op: SessionMetaOp,
    },
}

#[derive(Subcommand, Debug)]
enum SessionMetaOp {
    /// Create/update one session's entry.
    Set {
        /// tmux session name (the map key).
        #[arg(long)]
        session: String,
        /// Creating machine's id.
        #[arg(long)]
        origin: Option<String>,
        /// Only write --origin when the entry has none yet (creator wins;
        /// a later attach from another machine never steals origin).
        #[arg(long)]
        origin_if_absent: bool,
        /// Manual label as hex-encoded UTF-8 (sidesteps SSH-exec quoting
        /// for Hebrew/RTL text).
        #[arg(long)]
        label_hex: Option<String>,
        /// Remove the manual label.
        #[arg(long)]
        clear_label: bool,
    },
    /// Drop entries whose tmux session no longer exists.
    Prune,
    /// Print the whole map as JSON (debugging).
    Get,
}

#[derive(Subcommand, Debug)]
enum DevOp {
    /// Phase 9.B: poll the manifest URL and report current/latest versions.
    CheckUpdates {
        /// Pretty-print JSON (default is compact).
        #[arg(long)]
        pretty: bool,
    },
    /// Snapshot of app state: version, git hash, workspaces summary, active
    /// sessions, tunnel state, feed/notes counts, debug.log tail, console tail.
    GetState {
        /// Pretty-print JSON (default for `ymux dev` is compact).
        #[arg(long)]
        pretty: bool,
        /// Human-readable summary instead of JSON.
        #[arg(long)]
        text: bool,
    },
    /// Last N captured frontend console events (errors + warns).
    ConsoleTail {
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Last N lines of `<appdata>/ymux/debug.log`.
    DebugLogTail {
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Capture a bug report (state snapshot + description) under
    /// `<appdata>/ymux/bug-reports/bug-<unix>.json`. Reads description from
    /// stdin (terminate with empty line + Ctrl-Z+Enter on Windows / Ctrl-D on
    /// Unix) when --description is omitted.
    ReportBug {
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        repro_steps: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SettingsOp {
    /// Print current settings as JSON (pretty by default).
    Show {
        /// One-line compact JSON instead of pretty.
        #[arg(long)]
        json: bool,
    },
    /// Set a single dotted-path field, e.g. `--key theme.preset --value dracula`
    /// or `--key terminal.scrollback_lines --value 8000`.
    Set {
        #[arg(long)]
        key: String,
        #[arg(long)]
        value: String,
    },
    /// Apply a built-in theme preset (tokyo-night | dracula | solarized-dark |
    /// nord | solarized-light).
    Preset {
        name: String,
    },
    /// List available theme presets.
    Presets {
        #[arg(long)]
        json: bool,
    },
    /// Write the current settings to a file.
    Export {
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Replace settings with the contents of a file (full overwrite).
    Import {
        #[arg(long)]
        input: std::path::PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum NoteOp {
    /// Add a new note. Tag is optional — try `idea`, `bug`, `todo`, or your own.
    Add {
        /// Free-text body. Wrap in quotes if it contains spaces.
        text: String,
        #[arg(long)]
        tag: Option<String>,
        /// Workspace id to associate with the note (auto-detected from --pane if not set).
        #[arg(long)]
        workspace: Option<String>,
        /// Pane id to associate with the note (defaults to $YMUX_PANE_ID env if set).
        #[arg(long)]
        pane: Option<String>,
    },
    /// List notes. Defaults to open notes only; pass --done or --all to include resolved.
    List {
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        done: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Mark a note as done.
    Done {
        id: String,
    },
    /// Delete a note.
    Delete {
        id: String,
    },
    /// Update text/tag/status of a note.
    Update {
        id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// "open" or "done".
        #[arg(long)]
        status: Option<String>,
    },
}

#[cfg(windows)]
fn default_pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\ymux-{}", user)
}

/// `dirs::config_dir() / "ymux"`, without taking the `dirs` dependency.
///
/// Mirrors `ymux_core::config_dir`'s path resolution (not its migration):
/// the explicit override wins, then the platform config root. Duplicated
/// rather than shared for the same reason `default_pipe_name` is — this
/// binary cross-compiles to musl for the remote and must not pull
/// ymux-core's russh tree in behind it.
#[cfg(not(windows))]
fn ymux_config_dir() -> Option<std::path::PathBuf> {
    if let Some(custom) = std::env::var_os("YMUX_CONFIG_DIR")
        .or_else(|| std::env::var_os("WINMUX_CONFIG_DIR"))
        .filter(|v| !v.is_empty())
    {
        return Some(std::path::PathBuf::from(custom));
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library").join("Application Support").join("ymux"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(x) => Some(std::path::PathBuf::from(x).join("ymux")),
            None => Some(home.join(".config").join("ymux")),
        }
    }
}

/// Unix-domain socket paths to try, in order — the client half of
/// `ymux_core::pipe_names()` plus the pre-rename name.
///
/// The app binds every one of these, so the first that connects is a live
/// endpoint. The second exists because macOS caps `sockaddr_un.sun_path`
/// at 104 bytes and a long `$TMPDIR` can push the primary name over it;
/// the config dir is short and stable. Keep this list in step with
/// ymux-core — if the two ends disagree, every local hook fails with
/// ENOENT on a path nobody is listening on.
#[cfg(not(windows))]
fn default_socket_paths() -> Vec<String> {
    let user = whoami::username();
    let tmp = std::env::temp_dir();
    let mut v = vec![tmp
        .join(format!("ymux-{user}.sock"))
        .to_string_lossy()
        .into_owned()];
    if let Some(cfg) = ymux_config_dir() {
        v.push(cfg.join("rpc.sock").to_string_lossy().into_owned());
    }
    // winmux -> ymux rename: an app still running from before the rename
    // only serves the old socket name and has no way to learn otherwise.
    v.push(
        tmp.join(format!("winmux-{user}.sock"))
            .to_string_lossy()
            .into_owned(),
    );
    v
}

/// Pre-rename pipe name, tried only as a fallback — see the connect site
/// in `rpc_call`. Drop once 0.5.0 is the floor.
#[cfg(windows)]
fn default_pipe_name_legacy() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\winmux-{}", user)
}

/// Parse `KEY=VALUE` lines. Blank lines and `#` comments are skipped; a
/// repeated key takes its LAST value, matching how the file is appended to.
fn parse_env_file(content: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

/// Read and parse `$HOME/.ymux/run/last.env`, or None if it isn't there.
fn read_env_file() -> Option<std::collections::HashMap<String, String>> {
    // Phase 80: USERPROFILE fallback for native-Windows runs.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = std::path::Path::new(&home).join(".ymux/run/last.env");
    let content = std::fs::read_to_string(&path).ok()?;
    Some(parse_env_file(&content))
}

/// The endpoint the file names, but ONLY if it differs from `current`.
///
/// Phase 80. The env we inherited can be older than the process that set it:
/// `claude` gets `YMUX_SOCKET_ADDR` at spawn and its hooks inherit it, but a
/// reconnect can move the tunnel to a different port and nothing can rewrite
/// the environment of a process that is already running. `load_fallback_env_file`
/// cannot help — it returns early precisely because the variable IS set. So
/// on a failed connect we come back here and read the file directly.
///
/// Returns None when the file agrees with what we already tried, so a genuine
/// "the desktop is gone" stays one clean error rather than two.
fn refreshed_endpoint(current: &str) -> Option<(String, String)> {
    refreshed_endpoint_from(&read_env_file()?, current)
}

/// The decision half of `refreshed_endpoint`, split out so it is testable
/// without a filesystem or an environment.
fn refreshed_endpoint_from(
    map: &std::collections::HashMap<String, String>,
    current: &str,
) -> Option<(String, String)> {
    // Either spelling: an already-provisioned remote may still be writing
    // the pre-rename names until its next bootstrap.
    let addr = map
        .get("YMUX_SOCKET_ADDR")
        .or_else(|| map.get("WINMUX_SOCKET_ADDR"))?;
    if addr == current {
        return None;
    }
    let token = map
        .get("YMUX_TUNNEL_TOKEN")
        .or_else(|| map.get("WINMUX_TUNNEL_TOKEN"))?;
    Some((addr.clone(), token.clone()))
}

/// Load env vars from `$HOME/.ymux/run/last.env` if the relevant ones aren't already set.
/// Phase 6.3: written by the Windows app for each SSH workspace as a fallback for
/// sshd configurations that strip per-channel env vars.
fn load_fallback_env_file() {
    if std::env::var("YMUX_SOCKET_ADDR").is_ok() {
        return;
    }
    let map = match read_env_file() {
        Some(m) => m,
        None => return,
    };
    for (k, v) in map {
        if std::env::var_os(&k).is_none() {
            std::env::set_var(&k, &v);
        }
    }
}

// Phase 8.F.2: render an accessibility snapshot as a YAML-ish outline.
fn render_snapshot_text(node: &Value, depth: usize, out: &mut String) {
    if node.is_null() {
        out.push_str("(empty tree)\n");
        return;
    }
    let pad = "  ".repeat(depth);
    let role = node.get("role").and_then(|v| v.as_str()).unwrap_or("?");
    let text = node.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let level = node.get("level").and_then(|v| v.as_u64());
    let url = node.get("url").and_then(|v| v.as_str());
    let mut head = format!("{}- {}", pad, role);
    if let Some(l) = level {
        head.push_str(&format!("[{}]", l));
    }
    if !text.is_empty() {
        head.push_str(&format!(": \"{}\"", text.replace('\n', " ").replace('\r', "")));
    } else if let Some(name) = node.get("name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            head.push_str(&format!(": \"{}\"", name));
        }
    }
    if let Some(u) = url {
        head.push_str(&format!(" → {}", u));
    }
    head.push('\n');
    out.push_str(&head);
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for c in children {
            render_snapshot_text(c, depth + 1, out);
        }
    }
}

// Phase 8.E: render `ymux dev get-state` as a short human summary instead
// of dumping the full JSON. Used when --text is passed.
fn render_dev_state_text(v: &Value) -> String {
    let mut out = String::new();
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("?").to_string();
    out.push_str(&format!("ymux {} ({})\n", s("version"), s("git_hash")));
    out.push_str(&format!("appdata: {}\n", s("appdata_dir")));
    if let Some(ws) = v.get("workspaces") {
        out.push_str(&format!(
            "workspaces: {} (active: {})\n",
            ws.get("count").and_then(|x| x.as_u64()).unwrap_or(0),
            ws.get("active_id")
                .and_then(|x| x.as_str())
                .unwrap_or("none")
        ));
    }
    if let Some(sessions) = v.get("sessions").and_then(|x| x.as_array()) {
        out.push_str(&format!("active sessions: {}\n", sessions.len()));
        for s in sessions {
            out.push_str(&format!(
                "  pane={} kind={} conn={}\n",
                s.get("pane_id").and_then(|x| x.as_str()).unwrap_or("?"),
                s.get("kind").and_then(|x| x.as_str()).unwrap_or("?"),
                s.get("connection_type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
            ));
        }
    }
    if let Some(forwards) = v
        .get("tunnels")
        .and_then(|t| t.get("browser_forwards"))
        .and_then(|x| x.as_array())
    {
        out.push_str(&format!("port forwards: {}\n", forwards.len()));
        for f in forwards {
            out.push_str(&format!(
                "  ws={} remote={} -> local={}\n",
                f.get("workspace_id").and_then(|x| x.as_str()).unwrap_or("?"),
                f.get("remote_port").and_then(|x| x.as_u64()).unwrap_or(0),
                f.get("local_port").and_then(|x| x.as_u64()).unwrap_or(0),
            ));
        }
    }
    if let Some(notes) = v.get("notes") {
        out.push_str(&format!(
            "notes: {} open / {} done\n",
            notes.get("open").and_then(|x| x.as_u64()).unwrap_or(0),
            notes.get("done").and_then(|x| x.as_u64()).unwrap_or(0),
        ));
    }
    if let Some(feed) = v.get("feed") {
        out.push_str(&format!(
            "feed: {} open / {} done\n",
            feed.get("open").and_then(|x| x.as_u64()).unwrap_or(0),
            feed.get("done").and_then(|x| x.as_u64()).unwrap_or(0),
        ));
    }
    if let Some(log) = v.get("log_tail").and_then(|x| x.as_array()) {
        out.push_str(&format!("debug.log: {} tail lines\n", log.len()));
    }
    if let Some(c) = v.get("console_tail").and_then(|x| x.as_array()) {
        out.push_str(&format!("console: {} captured events\n", c.len()));
    }
    out
}

async fn rpc_call(method: &str, params: Value) -> Result<Value, String> {
    rpc_call_with(method, params, true).await
}

/// Phase 86: `rpc_call` without the last.env retry. For the port watcher,
/// whose tunnel is fixed for its whole life: a connect failure there means
/// *its* tunnel is gone, and following last.env would attach it to a
/// newer tunnel while a fresh watcher is already being spawned for it.
async fn rpc_call_pinned(method: &str, params: Value) -> Result<Value, String> {
    rpc_call_with(method, params, false).await
}

async fn rpc_call_with(
    method: &str,
    params: Value,
    refresh_on_stale: bool,
) -> Result<Value, String> {
    load_fallback_env_file();

    // Prefer TCP if YMUX_SOCKET_ADDR is set (works on any OS, including remote tunnels).
    if let Ok(addr) = std::env::var("YMUX_SOCKET_ADDR") {
        match tokio::net::TcpStream::connect(&addr).await {
            Ok(stream) => {
                let token = std::env::var("YMUX_TUNNEL_TOKEN").ok();
                return rpc_via(stream, method, params, token.as_deref()).await;
            }
            Err(e) if !refresh_on_stale => return Err(format!("connect tcp {addr}: {e}")),
            Err(e) => {
                // Phase 80: the address we inherited may be stale. `claude`
                // is long-lived and its hooks are its children, so both keep
                // whatever port was current when claude started — a later
                // reconnect cannot reach them. Re-read last.env (which the
                // desktop rewrites on every allocation) and try once more.
                //
                // Deliberately no `set_var`: this is a multi-threaded tokio
                // process, where setting environment variables is unsound.
                // The refreshed pair is passed down as values instead.
                //
                // YMUX_PANE_ID is NOT refreshed — it was already consumed by
                // the hook's "is ymux in this session" gate, and pane ids are
                // stable across reconnects anyway.
                match refreshed_endpoint(&addr) {
                    Some((fresh_addr, fresh_token)) => {
                        let stream = tokio::net::TcpStream::connect(&fresh_addr)
                            .await
                            .map_err(|e2| {
                                format!(
                                    "connect tcp {fresh_addr}: {e2} (stale {addr} also failed: {e})"
                                )
                            })?;
                        hook_log(
                            HookLevel::Info,
                            &format!(
                                "rpc: {addr} was stale, reconnected on {fresh_addr} from last.env"
                            ),
                        );
                        return rpc_via(stream, method, params, Some(&fresh_token)).await;
                    }
                    None => return Err(format!("connect tcp {addr}: {e}")),
                }
            }
        }
    }

    // Otherwise on Windows, use a named pipe.
    #[cfg(windows)]
    {
        let explicit = std::env::var("YMUX_PIPE_PATH").ok();
        let name = explicit.clone().unwrap_or_else(default_pipe_name);
        let open = |n: &str| tokio::net::windows::named_pipe::ClientOptions::new().open(n);
        let pipe = match open(&name) {
            Ok(p) => p,
            // winmux → ymux rename: an app still running from before the
            // rename only serves the old pipe name. Only worth trying when
            // we picked the default — an explicit YMUX_PIPE_PATH is the
            // caller telling us exactly where to go.
            Err(e) if explicit.is_none() => {
                let legacy = default_pipe_name_legacy();
                open(&legacy).map_err(|_| {
                    format!("connect to {name}: {e} (is the ymux app running?)")
                })?
            }
            Err(e) => {
                return Err(format!(
                    "connect to {name}: {e} (is the ymux app running?)"
                ))
            }
        };
        return rpc_via(pipe, method, params, None).await;
    }

    // Otherwise on unix, dial the local Unix-domain socket. Without this the
    // CLI only worked when something had already exported YMUX_SOCKET_ADDR,
    // so every local hook and statusline call on macOS exited 2 — while the
    // app had been listening on this socket the whole time.
    #[cfg(not(windows))]
    {
        let explicit = std::env::var("YMUX_PIPE_PATH")
            .ok()
            .filter(|s| !s.is_empty());
        // An explicit path is the caller telling us exactly where to go;
        // don't second-guess it by walking the defaults.
        let names = match &explicit {
            Some(p) => vec![p.clone()],
            None => default_socket_paths(),
        };
        let mut errors: Vec<String> = Vec::new();
        for name in &names {
            match tokio::net::UnixStream::connect(name).await {
                Ok(sock) => return rpc_via(sock, method, params, None).await,
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(format!(
            "connect to the local ymux socket failed ({}) — is the ymux app running? \
             (or set YMUX_SOCKET_ADDR=host:port for a remote)",
            errors.join("; ")
        ))
    }
}

/// Phase 6.4: HMAC-SHA256 challenge-response. The token never travels on the wire;
/// only the random nonce (sent by the server) and the HMAC of it (sent by the client).
async fn perform_handshake<R, W>(
    reader: &mut tokio::io::BufReader<R>,
    writer: &mut W,
    token: &str,
) -> Result<(), String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    // Read challenge.
    let mut line = String::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reader.read_line(&mut line),
    )
    .await;
    match r {
        Ok(Ok(0)) => return Err("server closed before challenge".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("read challenge: {e}")),
        Err(_) => return Err("challenge read timed out".into()),
    }
    let trimmed = line.trim();
    // winmux → ymux rename: read either dialect and mirror it for the rest
    // of the exchange, so this binary works against both a pre-rename
    // desktop (which only speaks WINMUX-*) and a post-flip one. See the
    // wire-tag note in `ymux-tunnel`.
    let (tag, nonce_hex) = trimmed
        .strip_prefix("YMUX-CHALLENGE ")
        .map(|x| ("YMUX", x))
        .or_else(|| trimmed.strip_prefix("WINMUX-CHALLENGE ").map(|x| ("WINMUX", x)))
        .ok_or_else(|| format!("expected YMUX-CHALLENGE, got {:?}", trimmed))?;
    let nonce = hex_decode(nonce_hex)?;

    // Compute HMAC and respond.
    let mut mac = HmacSha256::new_from_slice(token.as_bytes())
        .map_err(|e| format!("hmac key: {e}"))?;
    mac.update(&nonce);
    let response = mac.finalize().into_bytes();
    let resp_line = format!("{tag}-RESPONSE {}\n", hex_encode(&response));
    writer
        .write_all(resp_line.as_bytes())
        .await
        .map_err(|e| format!("write response: {e}"))?;
    writer.flush().await.ok();

    // Read OK / DENIED.
    let mut ok = String::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reader.read_line(&mut ok),
    )
    .await;
    match r {
        Ok(Ok(0)) => return Err("server closed before verdict".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("read verdict: {e}")),
        Err(_) => return Err("verdict timed out".into()),
    }
    let verdict = ok.trim();
    // Accept the verdict in either dialect regardless of which one we sent:
    // a mixed-version server may answer on the tag it prefers rather than
    // the one it was addressed in.
    if verdict == "YMUX-OK" || verdict == "WINMUX-OK" {
        Ok(())
    } else if let Some(reason) = verdict
        .strip_prefix("YMUX-DENIED")
        .or_else(|| verdict.strip_prefix("WINMUX-DENIED"))
    {
        Err(format!("auth denied:{}", reason))
    } else {
        Err(format!("unexpected handshake verdict: {:?}", verdict))
    }
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(hex_digit(x >> 4));
        s.push(hex_digit(x & 0xf));
    }
    s
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '?',
    }
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(format!("odd-length hex ({})", bytes.len()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("bad hex char: {:?}", c as char)),
    }
}

async fn rpc_via<S>(
    stream: S,
    method: &str,
    params: Value,
    token: Option<&str>,
) -> Result<Value, String>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf = BufReader::new(reader);

    // Phase 6.4: TCP transport authenticates via HMAC challenge-response. Token never
    // appears on the wire. Pipe transport (Windows) skips this — the pipe's per-user
    // ACL is the auth.
    if let Some(t) = token {
        perform_handshake(&mut buf, &mut writer, t).await?;
    }

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let line = format!("{}\n", req);
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.flush().await.ok();
    let mut response = String::new();
    buf.read_line(&mut response)
        .await
        .map_err(|e| e.to_string())?;
    let resp: Value = serde_json::from_str(response.trim())
        .map_err(|e| format!("bad response: {} ({})", e, response))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("rpc error: {}", err));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

fn derive_hook_title(subcommand: &str, payload: &Value) -> String {
    match subcommand {
        "tool-permission" | "pre-tool-use" => {
            if let Some(cmd) = payload.get("command").and_then(|v| v.as_str()) {
                format!("Run `{}` ?", cmd)
            } else if let Some(tool) = payload.get("tool").and_then(|v| v.as_str()) {
                format!("Allow `{}` ?", tool)
            } else if let Some(t) = payload.get("title").and_then(|v| v.as_str()) {
                t.to_string()
            } else {
                format!("agent: {}", subcommand)
            }
        }
        "session-start" => "Claude session started".to_string(),
        "session-stop" | "session-end" => "Claude session ended".to_string(),
        "session-idle" => "Claude is idle".to_string(),
        "session-active" => "Claude is active".to_string(),
        "notification" => payload
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "Claude notification".to_string()),
        "prompt-submit" => "Prompt submitted".to_string(),
        other => format!("agent: {}", other),
    }
}

fn derive_hook_summary(_subcommand: &str, payload: &Value) -> String {
    if let Some(s) = payload.get("summary").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = payload.get("description").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = payload.get("body").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(reason) = payload.get("reason").and_then(|v| v.as_str()) {
        return reason.to_string();
    }
    let s = serde_json::to_string(payload).unwrap_or_default();
    if s.len() > 280 {
        format!("{}…", truncate_at_char_boundary(&s, 280))
    } else {
        s
    }
}

/// Truncate at or before `max_bytes`, never inside a multi-byte UTF-8
/// character. The naive `&s[..max_bytes]` panics for Hebrew / Arabic / CJK
/// (and emoji) when `max_bytes` lands in the middle of a code-point —
/// observed in the wild when Claude Code sent a Stop hook with a Hebrew
/// `last_assistant_message`.
pub(crate) fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Parse `KEY=VALUE` repeats into the JSON shape the backend expects.
/// Errors out if any entry has no `=`.
fn parse_env_flags(flags: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(flags.len());
    for f in flags {
        let (k, v) = f
            .split_once('=')
            .ok_or_else(|| format!("--env argument {:?} is missing '='", f))?;
        if k.is_empty() {
            return Err(format!("--env argument {:?} has empty key", f));
        }
        out.push(json!({ "key": k, "value": v }));
    }
    Ok(out)
}

fn build_connection(
    type_: &str,
    shell: Option<String>,
    host: Option<String>,
    user: Option<String>,
    port: u16,
    key_path: Option<String>,
) -> Result<Value, String> {
    match type_ {
        "local" => Ok(json!({ "type": "local", "shell": shell })),
        "ssh" => {
            let host = host.ok_or("ssh requires --host")?;
            let user = user.ok_or("ssh requires --user")?;
            Ok(json!({
                "type": "ssh",
                "host": host,
                "user": user,
                "port": port,
                "key_path": key_path,
            }))
        }
        other => Err(format!("unknown type: {} (expected local|ssh)", other)),
    }
}

// Phase 8.F.2 fix: Windows debug builds give the main thread a 1 MB stack.
// Clap's derive macro for our 30+ subcommands (especially `BrowserFind` with
// 13 Option fields) generates a lot of format-string state that — combined
// with tokio's runtime + serde — overflows that 1 MB during arg parsing on
// some invocations. Spawn the real work on a worker thread with an 8 MB
// stack and join.
/// winmux → ymux rename bridge. For every `WINMUX_*` variable in the
/// environment, set the matching `YMUX_*` name when it isn't already
/// set.
///
/// The desktop writes both spellings, but a pane can outlive the app
/// that opened it: a long-lived tmux session carries whatever
/// `set-environment -g` put there when it was first created, and a
/// pre-rename desktop only ever set `WINMUX_*`. Promoting once here
/// means every read site downstream (`YMUX_SOCKET_ADDR`,
/// `YMUX_TUNNEL_TOKEN`, `YMUX_PANE_ID`, `YMUX_PIPE_PATH`,
/// `YMUX_HOOK_VERBOSE`, `YMUX_PORTFORWARD_EXCLUDE`) works unchanged,
/// instead of thirteen fallbacks that have to stay in sync.
///
/// Runs before any thread is spawned — `set_var` is only sound
/// single-threaded. Drop this once every deployed desktop is ≥0.5.0.
fn adopt_legacy_env() {
    let legacy: Vec<(String, std::ffi::OsString)> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let name = k.to_str()?;
            let suffix = name.strip_prefix("WINMUX_")?;
            Some((format!("YMUX_{suffix}"), v))
        })
        .collect();
    for (new_name, value) in legacy {
        if std::env::var_os(&new_name).is_none() {
            std::env::set_var(&new_name, &value);
        }
    }
}

/// winmux → ymux rename: fold a pre-rename `~/.winmux` into `~/.ymux`.
///
/// Sibling of the desktop bootstrap's `migrate_legacy_remote_dir`, for
/// the hosts the bootstrap never touches — WSL local setup, and remotes
/// where the daemon spawns `claude` children directly. Whichever runs
/// first wins; both leave the same marker so the other becomes a no-op.
///
/// Copy-then-mark rather than rename: both directories can already
/// exist, and anything already written under the new name stays
/// authoritative. Best-effort — this is a convenience, not a
/// precondition, so every failure is swallowed rather than blocking a
/// hook the user is waiting on.
fn migrate_legacy_home_dir() {
    let home = match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(h) => std::path::PathBuf::from(h),
        None => return,
    };
    let legacy = home.join(".winmux");
    if !legacy.is_dir() || legacy.join(".migrated-to-ymux").exists() {
        return;
    }
    let target = home.join(".ymux");
    if std::fs::create_dir_all(&target).is_err() {
        return;
    }
    copy_tree_no_clobber(&legacy, &target);
    let _ = std::fs::write(legacy.join(".migrated-to-ymux"), b"");
}

/// Recursive copy that never overwrites an existing destination entry.
fn copy_tree_no_clobber(from: &std::path::Path, to: &std::path::Path) {
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                if std::fs::create_dir_all(&dst).is_ok() {
                    copy_tree_no_clobber(&src, &dst);
                }
            }
            Ok(_) => {
                if !dst.exists() {
                    let _ = std::fs::copy(&src, &dst);
                }
            }
            Err(_) => {}
        }
    }
}

fn main() -> ExitCode {
    adopt_legacy_env();
    migrate_legacy_home_dir();
    match std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(real_main)
        .and_then(|h| h.join().map_err(|_| std::io::Error::other("worker panicked")))
    {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: worker thread spawn/join failed: {e}");
            ExitCode::from(2)
        }
    }
}

#[tokio::main]
async fn real_main() -> ExitCode {
    let cli = Cli::parse();
    // Phase 36: the port watcher loops forever and never produces a
    // single RPC result, so it's handled before the request/response
    // dispatch below. `run` diverges (-> !); it only returns control
    // if the process is signalled.
    if let Cmd::PortWatch { workspace } = &cli.command {
        load_fallback_env_file();
        port_watch::run(workspace).await;
    }
    let result: Result<Value, String> = match &cli.command {
        Cmd::ListWorkspaces => rpc_call("list-workspaces", json!({})).await,
        Cmd::SelectWorkspace { id } => rpc_call("select-workspace", json!({ "id": id })).await,
        Cmd::NewWorkspace {
            name,
            r#type,
            shell,
            host,
            user,
            port,
            key_path,
            cwd,
            color,
            setup,
            teardown,
            env,
        } => {
            let conn = match build_connection(
                r#type,
                shell.clone(),
                host.clone(),
                user.clone(),
                *port,
                key_path.clone(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(2);
                }
            };
            let env_pairs = match parse_env_flags(env) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(2);
                }
            };
            rpc_call(
                "new-workspace",
                json!({
                    "name": name,
                    "connection": conn,
                    "cwd": cwd,
                    "color": color,
                    "setup_command": setup,
                    "teardown_command": teardown,
                    "env": env_pairs,
                }),
            )
            .await
        }
        Cmd::UpdateWorkspace {
            id,
            name,
            color,
            cwd,
            setup,
            teardown,
            env,
            clear_env,
        } => {
            let mut params = json!({ "workspace_id": id });
            if let Some(v) = name {
                params["name"] = json!(v);
            }
            if let Some(v) = color {
                params["color"] = json!(v);
            }
            if let Some(v) = cwd {
                params["cwd"] = json!(v);
            }
            if let Some(v) = setup {
                params["setup_command"] = json!(v);
            }
            if let Some(v) = teardown {
                params["teardown_command"] = json!(v);
            }
            // env: only send if user passed --env or --clear-env.
            if !env.is_empty() || *clear_env {
                let env_pairs = match parse_env_flags(env) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: {}", e);
                        return ExitCode::from(2);
                    }
                };
                params["env"] = json!(env_pairs);
            }
            rpc_call("update-workspace", params).await
        }
        Cmd::DeleteWorkspace { id } => rpc_call("delete-workspace", json!({ "id": id })).await,
        Cmd::Send { pane, data } => {
            rpc_call("send", json!({ "pane_id": pane, "data": data })).await
        }
        Cmd::SendKey { pane, key } => {
            rpc_call("send-key", json!({ "pane_id": pane, "key": key })).await
        }
        Cmd::Notify {
            title,
            body,
            workspace_id,
        } => {
            rpc_call(
                "notify",
                json!({
                    "title": title,
                    "body": body,
                    "workspace_id": workspace_id,
                }),
            )
            .await
        }
        Cmd::Tree { workspace_id } => {
            rpc_call("tree", json!({ "workspace_id": workspace_id })).await
        }
        Cmd::SetStatus { pane, text } => {
            rpc_call("set-status", json!({ "pane_id": pane, "text": text })).await
        }
        Cmd::SetPaneTitle { pane, title } => {
            rpc_call(
                "set-pane-title",
                json!({ "pane_id": pane, "title": title }),
            )
            .await
        }
        Cmd::SetPaneAnnotation { pane, annotation } => {
            rpc_call(
                "set-pane-annotation",
                json!({ "pane_id": pane, "annotation": annotation }),
            )
            .await
        }
        Cmd::Split {
            pane,
            direction,
            kind,
            url,
        } => {
            rpc_call(
                "split",
                json!({
                    "pane_id": pane,
                    "direction": direction,
                    "kind": kind,
                    "url": url,
                }),
            )
            .await
        }
        Cmd::BrowserNavigate { pane, url } => {
            rpc_call(
                "pane.browser.navigate",
                json!({ "pane_id": pane, "url": url }),
            )
            .await
        }
        Cmd::BrowserGoBack { pane } => {
            rpc_call("pane.browser.go-back", json!({ "pane_id": pane })).await
        }
        Cmd::BrowserGoHome { pane } => {
            rpc_call("pane.browser.go-home", json!({ "pane_id": pane })).await
        }
        Cmd::BrowserResolveUrl { pane, url } => {
            rpc_call(
                "pane.browser.resolve-url",
                json!({ "pane_id": pane, "url": url }),
            )
            .await
        }
        Cmd::BrowserUrl { pane } => rpc_call("pane.browser.url", json!({ "pane_id": pane })).await,
        Cmd::BrowserHistory { pane } => {
            rpc_call("pane.browser.history", json!({ "pane_id": pane })).await
        }
        Cmd::BrowserWait { pane, timeout_ms } => {
            rpc_call(
                "pane.browser.wait",
                json!({ "pane_id": pane, "timeout_ms": timeout_ms }),
            )
            .await
        }
        Cmd::BrowserEval {
            pane,
            expr,
            timeout_ms,
        } => {
            // Phase 8.F.1: route to the new iframe bridge instead of the
            // old `pane.browser.eval` (which used the html2canvas-era
            // frontend listener and just returned a "cross-origin needs
            // 8.D" error). Strict upgrade: the bridge actually returns the
            // value, on cross-origin pages too.
            rpc_call(
                "pane.browser.iframe.eval",
                json!({
                    "pane_id": pane,
                    "expression": expr,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::BrowserScreenshot {
            pane,
            output,
            timeout_ms,
        } => {
            rpc_call(
                "pane.browser.screenshot",
                json!({
                    "pane_id": pane,
                    "output_path": output,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::BrowserFind {
            pane,
            role,
            text,
            label,
            placeholder,
            alt,
            title,
            testid,
            selector,
            visible_only,
            first,
            limit,
            timeout_ms,
        } => {
            // Build the params map directly so unspecified fields stay absent
            // (the bridge skips empty filters, but snake/camel case matters).
            let mut p = serde_json::Map::new();
            p.insert("pane_id".into(), json!(pane));
            p.insert("timeout_ms".into(), json!(timeout_ms));
            if let Some(v) = role { p.insert("role".into(), json!(v)); }
            if let Some(v) = text { p.insert("text".into(), json!(v)); }
            if let Some(v) = label { p.insert("label".into(), json!(v)); }
            if let Some(v) = placeholder { p.insert("placeholder".into(), json!(v)); }
            if let Some(v) = alt { p.insert("alt".into(), json!(v)); }
            if let Some(v) = title { p.insert("title".into(), json!(v)); }
            if let Some(v) = testid { p.insert("testid".into(), json!(v)); }
            if let Some(v) = selector { p.insert("selector".into(), json!(v)); }
            if *visible_only { p.insert("visibleOnly".into(), json!(true)); }
            if *first { p.insert("first".into(), json!(true)); }
            if let Some(v) = limit { p.insert("limit".into(), json!(v)); }
            rpc_call("pane.browser.iframe.find", Value::Object(p)).await
        }
        Cmd::BrowserWaitFor {
            pane,
            selector,
            text,
            role,
            label,
            testid,
            url_contains,
            state,
            timeout_ms,
        } => {
            let mut p = serde_json::Map::new();
            p.insert("pane_id".into(), json!(pane));
            p.insert("timeout_ms".into(), json!(timeout_ms));
            p.insert("state".into(), json!(state));
            if let Some(v) = selector { p.insert("selector".into(), json!(v)); }
            if let Some(v) = text { p.insert("text".into(), json!(v)); }
            if let Some(v) = role { p.insert("role".into(), json!(v)); }
            if let Some(v) = label { p.insert("label".into(), json!(v)); }
            if let Some(v) = testid { p.insert("testid".into(), json!(v)); }
            if let Some(v) = url_contains { p.insert("urlContains".into(), json!(v)); }
            rpc_call("pane.browser.iframe.wait-for", Value::Object(p)).await
        }
        Cmd::BrowserSnapshot {
            pane,
            max_depth,
            text_only,
            text,
            timeout_ms,
        } => {
            match rpc_call(
                "pane.browser.iframe.snapshot",
                json!({
                    "pane_id": pane,
                    "maxDepth": max_depth,
                    "textOnly": text_only,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
            {
                Ok(res) => {
                    if *text {
                        let tree = res.get("tree").cloned().unwrap_or(Value::Null);
                        let mut out = String::new();
                        render_snapshot_text(&tree, 0, &mut out);
                        print!("{}", out);
                        std::process::exit(0);
                    }
                    Ok(res)
                }
                Err(e) => Err(e),
            }
        }
        Cmd::BrowserClick {
            pane,
            selector,
            button,
            timeout_ms,
        } => {
            rpc_call(
                "pane.browser.iframe.click",
                json!({
                    "pane_id": pane,
                    "selector": selector,
                    "button": button,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::BrowserType {
            pane,
            selector,
            text,
            clear_first,
            timeout_ms,
        } => {
            rpc_call(
                "pane.browser.iframe.type",
                json!({
                    "pane_id": pane,
                    "selector": selector,
                    "text": text,
                    "clear_first": clear_first,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::Dev { op } => match op {
            DevOp::CheckUpdates { pretty } => {
                let v = rpc_call("updates.check", json!({})).await;
                match v {
                    Ok(val) => {
                        let s = if *pretty {
                            serde_json::to_string_pretty(&val).unwrap_or_default()
                        } else {
                            serde_json::to_string(&val).unwrap_or_default()
                        };
                        println!("{s}");
                        std::process::exit(0);
                    }
                    Err(e) => Err(e),
                }
            }
            DevOp::GetState { pretty: _, text } => {
                match rpc_call("dev.get-state", json!({})).await {
                    Ok(v) => {
                        if *text {
                            // Bypass the JSON-pretty pipeline and print directly.
                            print!("{}", render_dev_state_text(&v));
                            std::process::exit(0);
                        }
                        Ok(v)
                    }
                    Err(e) => Err(e),
                }
            }
            DevOp::ConsoleTail { limit } => {
                rpc_call("dev.console-tail", json!({ "limit": limit })).await
            }
            DevOp::DebugLogTail { limit } => {
                rpc_call("dev.debug-log-tail", json!({ "limit": limit })).await
            }
            DevOp::ReportBug {
                description,
                repro_steps,
            } => {
                use std::io::Read;
                let desc = match description {
                    Some(d) => Ok(d.clone()),
                    None => {
                        eprintln!("Describe the issue (Ctrl-Z then Enter to finish on Windows, Ctrl-D on Unix):");
                        let mut s = String::new();
                        std::io::stdin()
                            .read_to_string(&mut s)
                            .map_err(|e| format!("read stdin: {e}"))
                            .map(|_| s.trim().to_string())
                    }
                };
                match desc {
                    Ok(d) if !d.is_empty() => {
                        rpc_call(
                            "dev.report-bug",
                            json!({
                                "description": d,
                                "repro_steps": repro_steps,
                            }),
                        )
                        .await
                    }
                    Ok(_) => Err("description is required".into()),
                    Err(e) => Err(e),
                }
            }
        },
        Cmd::Note { op } => match op {
            NoteOp::Add {
                text,
                tag,
                workspace,
                pane,
            } => {
                let pane_eff = pane
                    .clone()
                    .or_else(|| std::env::var("YMUX_PANE_ID").ok())
                    .filter(|s| !s.is_empty());
                rpc_call(
                    "note-add",
                    json!({
                        "text": text,
                        "tag": tag,
                        "workspace_id": workspace,
                        "pane_id": pane_eff,
                    }),
                )
                .await
            }
            NoteOp::List {
                tag,
                done,
                all,
                workspace,
                limit,
                json: as_json,
            } => {
                let status = if *all {
                    None
                } else if *done {
                    Some("done".to_string())
                } else {
                    Some("open".to_string())
                };
                let result = rpc_call(
                    "note-list",
                    json!({
                        "tag": tag,
                        "status": status,
                        "workspace_id": workspace,
                        "limit": limit,
                    }),
                )
                .await;
                match result {
                    Ok(v) => {
                        if *as_json || cli.raw {
                            let s = if cli.raw {
                                serde_json::to_string(&v).unwrap_or_default()
                            } else {
                                serde_json::to_string_pretty(&v).unwrap_or_default()
                            };
                            println!("{}", s);
                            return ExitCode::SUCCESS;
                        }
                        let arr = v.as_array().cloned().unwrap_or_default();
                        if arr.is_empty() {
                            println!("(no notes)");
                            return ExitCode::SUCCESS;
                        }
                        for n in arr {
                            let id = n.get("id").and_then(|x| x.as_str()).unwrap_or("?");
                            let st =
                                n.get("status").and_then(|x| x.as_str()).unwrap_or("open");
                            let tg = n.get("tag").and_then(|x| x.as_str()).unwrap_or("-");
                            let upd = n
                                .get("updated_at")
                                .and_then(|x| x.as_str())
                                .unwrap_or("");
                            let txt = n.get("text").and_then(|x| x.as_str()).unwrap_or("");
                            let mark = if st == "done" { "✓" } else { " " };
                            let snippet = if txt.len() > 80 {
                                format!("{}…", truncate_at_char_boundary(txt, 80))
                            } else {
                                txt.to_string()
                            };
                            println!(
                                "{}  {:<8}  [{}]  {}  {}",
                                mark, tg, &id[..id.len().min(20)], upd, snippet
                            );
                        }
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        return ExitCode::from(2);
                    }
                }
            }
            NoteOp::Done { id } => rpc_call("note-done", json!({ "id": id })).await,
            NoteOp::Delete { id } => rpc_call("note-delete", json!({ "id": id })).await,
            NoteOp::Update {
                id,
                text,
                tag,
                status,
            } => {
                let mut params = json!({ "id": id });
                if let Some(t) = text {
                    params["text"] = json!(t);
                }
                if let Some(tg) = tag {
                    params["tag"] = json!(tg);
                }
                if let Some(s) = status {
                    params["status"] = json!(s);
                }
                rpc_call("note-update", params).await
            }
        },
        Cmd::SessionMeta { op } => {
            // Local file ops only — no tunnel, no desktop. Called over a
            // bare SSH exec by the desktop app (origin/label writes) or by
            // hand for debugging.
            match op {
                SessionMetaOp::Set { session, origin, origin_if_absent, label_hex, clear_label } => {
                    let session = session.trim();
                    if session.is_empty() {
                        eprintln!("error: --session is empty");
                        return ExitCode::from(2);
                    }
                    let mut meta = session_meta::load_meta();
                    let entry = meta.sessions.entry(session.to_string()).or_default();
                    if let Some(o) = origin {
                        if !*origin_if_absent || entry.origin.is_none() {
                            entry.origin = Some(o.clone());
                        }
                    }
                    if *clear_label {
                        entry.label = None;
                    } else if let Some(hex) = label_hex {
                        match session_meta::decode_hex_utf8(hex) {
                            Ok(label) if label.trim().is_empty() => entry.label = None,
                            Ok(label) => entry.label = Some(label),
                            Err(e) => {
                                eprintln!("error: bad --label-hex: {e}");
                                return ExitCode::from(2);
                            }
                        }
                    }
                    entry.updated_at = Some(session_meta::now_rfc3339());
                    session_meta::prune(&mut meta);
                    if let Err(e) = session_meta::save_meta_atomic(&meta) {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                    return ExitCode::SUCCESS;
                }
                SessionMetaOp::Prune => {
                    let mut meta = session_meta::load_meta();
                    if session_meta::prune(&mut meta) {
                        if let Err(e) = session_meta::save_meta_atomic(&meta) {
                            eprintln!("error: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                    return ExitCode::SUCCESS;
                }
                SessionMetaOp::Get => {
                    let meta = session_meta::load_meta();
                    match serde_json::to_string_pretty(&meta) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("error: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                    return ExitCode::SUCCESS;
                }
            }
        }
        Cmd::ClaudeHook { subcommand } => {
            // Phase 66 (wiring fix): load the fallback env file FIRST, before
            // the env-gate / permission-mode checks read YMUX_PANE_ID.
            // sshd usually rejects `set_env` for YMUX_* (AcceptEnv only
            // lists LANG/LC_*), so on a plain (non-tmux) shell those vars
            // never reach the process env — only `~/.ymux/run/last.env`
            // has them. Previously this file was loaded lazily inside
            // rpc_call (AFTER the env-gate), so the env-gate saw PANE_ID
            // unset → it treated a real ymux session as "not ymux"
            // (old CLI: ask/block; new CLI: allow-but-never-gate, no cards).
            // Loading it here makes the env-gate, permission-mode shortcut,
            // and the RPC dial all see the real pane id + socket + token.
            load_fallback_env_file();
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            let payload: Value = if buf.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(buf.trim()).unwrap_or_else(|_| json!({ "raw": buf.trim() }))
            };

            // Phase 18.1: comprehensive diagnostic log of every hook
            // invocation. The file lives at `~/.ymux/hook-debug.log`
            // (server-side, where this CLI runs). Used to debug
            // permission_mode mismatches, matcher coverage gaps, and
            // post-action card surprises. Best-effort — silent on any
            // I/O error so the hook never fails due to logging.
            //
            // The payload Claude Code sends varies by event:
            //   PreToolUse: { tool_name, tool_input, permission_mode,
            //                 session_id, transcript_path, cwd, … }
            //   Notification: { message, … }
            //   SessionStart/End: { session_id, … }
            //   Stop: { session_id, stop_hook_active, … }
            // We dump the keys + selected values rather than the whole
            // body so secrets / large prompts don't leak to disk.
            let pane_id_log = std::env::var("YMUX_PANE_ID")
                .unwrap_or_else(|_| "(unset)".into());
            let tool_name_log = payload
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("(absent)");
            let perm_mode_log = payload
                .get("permission_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("(absent)");
            let session_id_log = payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("(absent)");
            let payload_keys: Vec<&str> = payload
                .as_object()
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            hook_vlog(&format!(
                "claude-hook BEGIN subcommand={subcommand} \
                 pane_id={pane_id_log} tool_name={tool_name_log} \
                 permission_mode={perm_mode_log} session_id={session_id_log} \
                 payload_keys={payload_keys:?}"
            ));

            // Phase setup-hooks-fix v4: env-gate. The hooks are written to
            // ~/.claude/settings.json which is global — they fire for EVERY
            // Claude Code invocation on this machine, not just the ones
            // launched inside a ymux pane. Unrelated terminals (plain
            // pwsh, VS Code's own terminal, an external WSL session) would
            // otherwise dial ymux on every tool call. We tag ymux-spawned
            // shells with YMUX_PANE_ID — if it's missing, Claude Code is
            // not running under us and ymux has no business in the loop.
            // Phase 66.D.x: ALLOW (not "ask"). Returning "ask" made Claude
            // Code pop its built-in "Do you want to proceed?" prompt on every
            // tool call in non-ymux terminals — pure noise, and the original
            // reason the hooks were hated. If ymux isn't involved there's no
            // policy to apply, so get out of the way and let Claude run.
            if std::env::var("YMUX_PANE_ID").is_err() {
                hook_vlog(&format!(
                    "claude-hook BRANCH=env-gate (YMUX_PANE_ID unset) \
                     decision=allow-or-noop subcommand={subcommand}"
                ));
                match subcommand.as_str() {
                    "pre-tool-use" => {
                        let out = json!({
                            "hookSpecificOutput": {
                                "hookEventName": "PreToolUse",
                                "permissionDecision": "allow",
                                "permissionDecisionReason": "ymux not in this session — not gating"
                            }
                        });
                        println!("{}", serde_json::to_string(&out).unwrap_or_default());
                    }
                    _ => {
                        // notification / session-start / session-end / stop /
                        // legacy tool-permission — silent ack.
                    }
                }
                return ExitCode::SUCCESS;
            }

            // Phase setup-hooks-fix v3.5: honor Claude Code's `permission_mode`.
            // When the user has flipped to `acceptEdits` / `bypassPermissions`
            // (Shift+Tab in the agent, or starting `claude --dangerously-skip-permissions`),
            // Claude Code does not actually wait on our hook decision before
            // proceeding — but it still INVOKES us. If we then synchronously
            // ring the user's ymux for a card they have to manually
            // dismiss, that's pure noise. Short-circuit with allow + reason
            // so the user sees nothing and Claude Code is happy.
            if subcommand == "pre-tool-use" {
                let permission_mode = payload
                    .get("permission_mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                if matches!(
                    permission_mode,
                    "acceptEdits" | "bypassPermissions" | "skip"
                ) {
                    // v0.4.4: expected path (agent runs with its own
                    // permission mode) — one concise line, not two.
                    hook_dlog(&format!(
                        "auto-allow tool={tool_name_log} mode={permission_mode} session={session_id_log}"
                    ));
                    let out = json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "allow",
                            "permissionDecisionReason": format!(
                                "ymux: agent has permission_mode={permission_mode}, deferring to it"
                            )
                        }
                    });
                    println!("{}", serde_json::to_string(&out).unwrap_or_default());
                    return ExitCode::SUCCESS;
                } else {
                    hook_vlog(&format!(
                        "claude-hook BRANCH=permission_mode-passthrough \
                         permission_mode={permission_mode} tool_name={tool_name_log} \
                         (will dispatch to ymux card)"
                    ));
                }
            }

            // v0.4.4: drop pure-noise passive hooks entirely. SessionStart and
            // Notification are observability-only — they filled the feed and
            // hook-debug.log without anyone acting on them (Notification in
            // particular: the agent is driven by the main session, not by these
            // alerts). Silent-ack with NO feed.push and NO log line. The
            // meaningful lifecycle signals (Stop = "your turn", SessionEnd =
            // "session closed") still dispatch below. errors/timeouts are on
            // the pre-tool-use path and are unaffected.
            if matches!(subcommand.as_str(), "session-start") {
                return ExitCode::SUCCESS;
            }

            // ── Phase 84.B: Notification, back — but as pane STATE only ──
            //
            // This reverses half of the v0.4.4 decision above, and the
            // distinction matters: Notification was noise as a FEED ITEM
            // (a card nobody acted on, per event, forever). It is the only
            // signal there is for "the agent is blocked on the human",
            // which is the red light on a pane tab.
            //
            // It gets its own branch and returns before the shared dispatch
            // below, for two reasons. First, derive_hook_summary falls back
            // to serialising the whole payload, and `notification_text` is
            // agent- or user-authored prose — Rule #1 territory, and it
            // would land in the FeedStore and possibly a toast. So the push
            // carries the TYPE and nothing else. Second, everything after
            // this point exists to build a feed item, which is precisely
            // what a notification must not become.
            if subcommand == "notification" {
                let ntype = payload
                    .get("notification_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Types that mean "blocked on the human". Kept in sync with
                // NEEDS_INPUT_NOTIFICATIONS / RESUMED_NOTIFICATIONS in the
                // desktop's lib.rs — the desktop is the authority on what
                // each one means; this list only decides whether it is worth
                // a socket round-trip at all.
                const RELEVANT: &[&str] = &[
                    "permission_prompt",
                    "idle_prompt",
                    "agent_needs_input",
                    "elicitation_dialog",
                    "elicitation_url_dialog",
                    "elicitation_complete",
                    "elicitation_response",
                ];
                if !RELEVANT.contains(&ntype) {
                    // auth_success, or a type Claude Code added after this
                    // shipped. Exit before any network call — an unknown
                    // type is not an error, it is simply not our business.
                    hook_vlog(&format!(
                        "claude-hook notification type={} — not a state signal, ignored",
                        if ntype.is_empty() { "(absent)" } else { ntype }
                    ));
                    return ExitCode::SUCCESS;
                }
                let request_id = format!(
                    "req_{:x}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                let pane_id = std::env::var("YMUX_PANE_ID").ok();
                // Rule #1: the type is a fixed enum value from Claude Code.
                // notification_text never appears here or in the push.
                hook_dlog(&format!(
                    "notification type={ntype} pane={} req_id={request_id}",
                    pane_id.as_deref().unwrap_or("(none)")
                ));
                let mut push = json!({
                    "request_id": request_id,
                    "kind": "passive",
                    "subkind": "notification",
                    "pane_id": pane_id,
                    "title": format!("claude notification: {ntype}"),
                    "summary": "",
                    "payload": { "notification_type": ntype },
                    // Nothing blocks on this; keep the desktop's thread for
                    // as short a time as the clamp allows.
                    "wait_timeout_seconds": 5,
                });
                // The pane id above rides a stale-prone env var (Phase
                // 81.G). Send the multiplexer session name so the desktop
                // can recover the real pane — otherwise the light lands on
                // whichever pane connected last.
                if let Some(s) = session_meta::resolve_session_name() {
                    push["tmux_session"] = json!(s);
                }
                let _ = rpc_call("feed.push", push).await;
                return ExitCode::SUCCESS;
            }

            // Multi-machine sync: on every `stop` (fires each turn) record
            // this pane's tmux-session → Claude-session mapping + title in
            // the server-side `~/.ymux/session-meta.json`; `session-end`
            // prunes dead sessions. `user-prompt-submit` seeds the stable
            // `auto_name` from the session's FIRST prompt (see
            // session_meta::handle_hook) and returns it as the display
            // title from then on. Best-effort — a failure never blocks
            // the hook. Runs only for ymux panes (env-gate above).
            // Rule #1: log the error KIND only, never the title text.
            let (meta_claude_title, meta_tmux_session) =
                if matches!(subcommand.as_str(), "stop" | "session-end" | "user-prompt-submit") {
                    match session_meta::handle_hook(subcommand, &payload) {
                        Ok(v) => v,
                        Err(kind) => {
                            hook_vlog(&format!(
                                "claude-hook session-meta skipped kind={kind} \
                                 subcommand={subcommand}"
                            ));
                            (None, None)
                        }
                    }
                } else {
                    (None, None)
                };

            let blocking = matches!(subcommand.as_str(), "tool-permission" | "pre-tool-use");
            let kind = if blocking { "permission_request" } else { "passive" };

            let request_id = format!(
                "req_{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let pane_id = std::env::var("YMUX_PANE_ID").ok();
            let title = derive_hook_title(subcommand, &payload);
            let summary = derive_hook_summary(subcommand, &payload);

            // Honor `wait_timeout_seconds` from the agent's payload if present and sane.
            // Default 120, clamped to [1, 600] to bound server-side resource holds.
            let timeout_secs = payload
                .get("wait_timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(120)
                .clamp(1, 600);
            // Phase setup-hooks-fix v2: keep stderr clean. Claude Code's UI
            // surfaces stderr and our diagnostic line was cluttering the chat.
            // The same data is already captured server-side via the feed.push
            // RPC and shows up in `ymux dev debug-log-tail`.

            // v0.4.4: concise always-on summary — one line per request.
            // Blocking permission requests get a REQ line (paired with the
            // ACK line at rpc-ok); passive lifecycle hooks (stop / session-*
            // / notification) get a single line and nothing more.
            if blocking {
                hook_dlog(&format!(
                    "REQ {subcommand} tool={tool_name_log} pane={pane_id_log} \
                     req_id={request_id} mode={perm_mode_log}"
                ));
            } else {
                hook_dlog(&format!(
                    "{subcommand} pane={pane_id_log} session={session_id_log} \
                     req_id={request_id}"
                ));
            }
            hook_vlog(&format!(
                "claude-hook BRANCH=dispatch kind={kind} blocking={blocking} \
                 subcommand={subcommand} request_id={request_id} \
                 timeout_secs={timeout_secs}"
            ));

            let mut push_params = json!({
                "request_id": request_id,
                "kind": kind,
                "subkind": subcommand,
                "pane_id": pane_id,
                "title": title,
                "summary": summary,
                "payload": payload,
                "wait_timeout_seconds": timeout_secs,
            });
            // Multi-machine sync: piggyback the Claude session title on the
            // stop push so the desktop can set the pane's auto_title without
            // another roundtrip. Old desktops ignore unknown params.
            if let Some(t) = &meta_claude_title {
                push_params["claude_title"] = json!(t);
            }
            if let Some(s) = &meta_tmux_session {
                push_params["tmux_session"] = json!(s);
            }

            // Phase 86: no pre-flight ping. A connect failure surfaces from
            // feed_push_with_retry below and lands in the static-fallback
            // arm of the match — one code path, one connection attempt set.

            // B path (Phase 77 push): also forward a pre-tool-use hook to the
            // LOCAL ymux-server so paired phones get a push. Fire-and-forget —
            // the desktop stays the decision authority; a failure never affects
            // the blocking desktop call below (A path untouched).
            if subcommand == "pre-tool-use" {
                let rid = request_id.clone();
                let pid = pane_id.clone().unwrap_or_default();
                let ttl = title.clone();
                let tool = payload
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let to = timeout_secs;
                tokio::spawn(async move {
                    forward_hook_to_server(&rid, &pid, &tool, &ttl, to).await;
                });
            }

            // Phase 66 (66.D.2): retry the push on connection failure (not
            // on a timeout verdict — that means we reached the desktop).
            let result = if subcommand == "pre-tool-use" {
                feed_push_with_retry(&push_params, 2).await
            } else {
                rpc_call("feed.push", push_params).await
            };

            match result {
                Ok(v) => {
                    let decision = v
                        .get("decision")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown");
                    // v0.4.4: concise ACK line for blocking requests (pairs
                    // with the REQ line). Passive hooks already logged their
                    // single line at dispatch — no ACK for them. The decision
                    // (allow/deny/timeout) is captured here; the full branch
                    // trace is verbose-only.
                    if blocking {
                        hook_dlog(&format!("ACK {decision} req_id={request_id}"));
                    }
                    hook_vlog(&format!(
                        "claude-hook BRANCH=rpc-ok decision={decision} \
                         subcommand={subcommand} request_id={request_id}"
                    ));
                    // Phase setup-hooks-fix v2: emit Claude Code v2.1+ hook
                    // output format. Per https://docs.claude.com/en/docs/claude-code/hooks
                    //   { "hookSpecificOutput": { "hookEventName": ..., "permissionDecision": "allow"|"deny"|"ask", "permissionDecisionReason"? } }
                    // exit 0 + the JSON ABOVE is the in-band signaling — exit
                    // codes 1/2/3 are NOT how decisions are expressed. The
                    // legacy `tool-permission` subcommand keeps the old shape
                    // so any pre-existing custom flow doesn't break.
                    match subcommand.as_str() {
                        "pre-tool-use" => {
                            let (perm, reason) = match decision {
                                "allow" | "passive" => ("allow", None),
                                "deny" => ("deny", Some("User denied via ymux".to_string())),
                                "timeout" => (
                                    "deny",
                                    Some(
                                        "ymux permission request timed out — denying conservatively"
                                            .to_string(),
                                    ),
                                ),
                                other => (
                                    "ask",
                                    Some(format!("ymux returned unknown decision: {other}")),
                                ),
                            };
                            let mut hso = json!({
                                "hookEventName": "PreToolUse",
                                "permissionDecision": perm,
                            });
                            if let Some(r) = reason {
                                hso["permissionDecisionReason"] = json!(r);
                            }
                            let out = json!({ "hookSpecificOutput": hso });
                            println!("{}", serde_json::to_string(&out).unwrap_or_default());
                            return ExitCode::SUCCESS;
                        }
                        "notification" | "session-start" | "session-end" | "stop"
                        | "user-prompt-submit" | "post-tool-use" | "subagent-stop"
                        | "pre-compact" => {
                            // Passive lifecycle hooks: silent ack. exit 0, no
                            // stdout — Claude Code does not need a structured
                            // response for these.
                            return ExitCode::SUCCESS;
                        }
                        _ => {
                            // Legacy `tool-permission` or unknown subcommands:
                            // print the raw RPC payload and use the historical
                            // exit-code-based signaling so anything that scrapes
                            // the JSON or branches on $? keeps working.
                            if !cli.quiet {
                                let s = if cli.raw {
                                    serde_json::to_string(&v).unwrap_or_default()
                                } else {
                                    serde_json::to_string_pretty(&v).unwrap_or_default()
                                };
                                println!("{}", s);
                            }
                            return match decision {
                                "allow" | "passive" => ExitCode::SUCCESS,
                                "deny" => ExitCode::from(1),
                                "timeout" => ExitCode::from(2),
                                _ => ExitCode::from(3),
                            };
                        }
                    }
                }
                Err(e) => {
                    // Phase setup-hooks fix v3.5 — graceful pipe failure.
                    // If we can't talk to ymux (pipe not running, EOF
                    // mid-frame, timeout) we don't want to fail the entire
                    // hook. For pre-tool-use we emit `permissionDecision:
                    // "ask"` so Claude Code falls back to its built-in UI
                    // (better than a hard error). Lifecycle hooks stay
                    // silent (they don't need a response anyway). Legacy
                    // subcommands keep the old exit-code shape.
                    hook_log(
                        HookLevel::Warn,
                        &format!(
                            "claude-hook BRANCH=rpc-error-fallback error={e} \
                             subcommand={subcommand} request_id={request_id} \
                             (Claude Code's built-in UI will be shown)"
                        ),
                    );
                    eprintln!(
                        "ymux claude-hook: pipe error: {} (using static fallback policy)",
                        e
                    );
                    match subcommand.as_str() {
                        "pre-tool-use" => {
                            // Phase 66 (66.D.1): desktop unreachable even
                            // after the connect retry — decide with the
                            // CLI's static policy so Claude keeps moving on
                            // safe defaults rather than denying everything.
                            let (decision, reason) = static_fallback_decision(&payload);
                            // Rule #1: never log `reason` (can embed command
                            // text). Metadata only — the error kind is safe.
                            hook_log(
                                HookLevel::Warn,
                                &format!(
                                    "static-fallback decision={decision} req_id={request_id} \
                                     error={e}"
                                ),
                            );
                            print_pre_tool_use(decision, Some(&reason));
                            return ExitCode::SUCCESS;
                        }
                        "notification" | "session-start" | "session-end" | "stop"
                        | "user-prompt-submit" | "post-tool-use" | "subagent-stop"
                        | "pre-compact" => {
                            return ExitCode::SUCCESS;
                        }
                        _ => {
                            return ExitCode::from(3);
                        }
                    }
                }
            }
        }
        Cmd::SetupHooks {
            agent,
            dry_run,
            force,
            source,
            hooks_version,
            matcher_mode,
        } => {
            // hooks_version is informational for now; we surface it
            // in the log so a user explicitly pinning to a version
            // sees their request acknowledged. The actual fetch
            // pulls `main`'s copy from raw.githubusercontent — pinning
            // to a tag URL would happen via `--source url=…` instead.
            if hooks_version != "latest" {
                eprintln!(
                    "setup-hooks: --hooks-version={hooks_version} requested; \
                     note that the github source always tracks `main` for now"
                );
            }
            if !matches!(matcher_mode.as_str(), "restrictive" | "all" | "custom") {
                eprintln!(
                    "error: unknown --matcher-mode {:?} (use 'restrictive', 'all', or 'custom')",
                    matcher_mode
                );
                return ExitCode::from(2);
            }
            let mut adapters: Vec<Box<dyn hooks::HookAdapter>> = Vec::new();
            match agent.as_str() {
                "claude" => adapters.push(Box::new(hooks::Claude)),
                "all" => {
                    adapters.push(Box::new(hooks::Claude));
                    adapters.push(Box::new(hooks::Stub { label: "Codex" }));
                    adapters.push(Box::new(hooks::Stub { label: "Cursor" }));
                    adapters.push(Box::new(hooks::Stub { label: "OpenCode" }));
                    adapters.push(Box::new(hooks::Stub { label: "Gemini CLI" }));
                    adapters.push(Box::new(hooks::Stub { label: "Copilot CLI" }));
                }
                other => {
                    eprintln!("error: unknown --agent {:?} (use 'claude' or 'all')", other);
                    return ExitCode::from(2);
                }
            }
            hooks::run_all(&adapters, *dry_run, *force, source, matcher_mode);
            return ExitCode::SUCCESS;
        }
        Cmd::PaneDisconnect { pane, kill } => {
            if *kill {
                rpc_call("pane.kill-session", json!({ "pane_id": pane })).await
            } else {
                rpc_call("pane.disconnect", json!({ "pane_id": pane })).await
            }
        }
        Cmd::PaneKillSession { pane } => {
            rpc_call("pane.kill-session", json!({ "pane_id": pane })).await
        }
        Cmd::PanePersistenceList => rpc_call("pane.persistence.list", json!({})).await,
        Cmd::ClaudeSessionsList { workspace, limit } => {
            rpc_call(
                "claude.sessions.list",
                json!({ "workspace_id": workspace, "limit": limit }),
            )
            .await
        }
        Cmd::Settings { op } => match op {
            SettingsOp::Show { json } => {
                let v = rpc_call("settings.load", json!({})).await;
                match v {
                    Ok(val) => {
                        let s = if *json {
                            serde_json::to_string(&val).unwrap_or_default()
                        } else {
                            serde_json::to_string_pretty(&val).unwrap_or_default()
                        };
                        println!("{s}");
                        std::process::exit(0);
                    }
                    Err(e) => Err(e),
                }
            }
            SettingsOp::Set { key, value } => {
                rpc_call(
                    "settings.set",
                    json!({ "key": key, "value": value }),
                )
                .await
            }
            SettingsOp::Preset { name } => {
                rpc_call("settings.preset", json!({ "preset": name })).await
            }
            SettingsOp::Presets { json: as_json } => {
                let v = rpc_call("settings.get-presets", json!({})).await;
                match v {
                    Ok(val) => {
                        let s = if *as_json {
                            serde_json::to_string(&val).unwrap_or_default()
                        } else {
                            serde_json::to_string_pretty(&val).unwrap_or_default()
                        };
                        println!("{s}");
                        std::process::exit(0);
                    }
                    Err(e) => Err(e),
                }
            }
            SettingsOp::Export { output } => {
                let v = rpc_call("settings.load", json!({})).await;
                match v {
                    Ok(val) => {
                        let s = serde_json::to_string_pretty(&val).unwrap_or_default();
                        match std::fs::write(output, s) {
                            Ok(()) => {
                                eprintln!("settings exported to {:?}", output);
                                std::process::exit(0);
                            }
                            Err(e) => Err(format!("write {:?}: {e}", output)),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            SettingsOp::Import { input } => {
                let text = match std::fs::read_to_string(input) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("error: read {:?}: {e}", input);
                        return ExitCode::from(2);
                    }
                };
                let parsed: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: parse {:?}: {e}", input);
                        return ExitCode::from(2);
                    }
                };
                rpc_call("settings.save", json!({ "settings": parsed })).await
            }
        },
        // Phase 36: handled before this match (port_watch::run diverges).
        Cmd::PortWatch { .. } => unreachable!("PortWatch handled before dispatch"),
        // Phase 48-C: thin RPC wrapper. The backend builds the snapshot.
        Cmd::Doctor => rpc_call("doctor", json!({})).await,
    };

    match result {
        Ok(v) => {
            if cli.quiet {
                ExitCode::SUCCESS
            } else {
                let s = if cli.raw {
                    serde_json::to_string(&v).unwrap_or_default()
                } else {
                    serde_json::to_string_pretty(&v).unwrap_or_default()
                };
                println!("{}", s);
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod env_file_tests {
    use super::{parse_env_file, refreshed_endpoint_from};
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_env_file_skips_comments_and_blank_lines() {
        let m = parse_env_file("# a comment\n\nYMUX_SOCKET_ADDR=127.0.0.1:1\n   \nX=y\n");
        assert_eq!(m.len(), 2, "got {m:?}");
        assert_eq!(m.get("YMUX_SOCKET_ADDR").map(String::as_str), Some("127.0.0.1:1"));
    }

    #[test]
    fn parse_env_file_keeps_a_value_containing_equals_signs() {
        // Tokens are alphanumeric today, but splitting on the LAST `=` would
        // silently corrupt any value that ever contains one.
        let m = parse_env_file("K=a=b=c\n");
        assert_eq!(m.get("K").map(String::as_str), Some("a=b=c"));
    }

    #[test]
    fn parse_env_file_takes_the_last_value_for_a_repeated_key() {
        // The remote file is written by append in one branch of the
        // read-modify-write, so a duplicate key means "the newer one wins".
        let m = parse_env_file("YMUX_PANE_ID=old\nYMUX_PANE_ID=new\n");
        assert_eq!(m.get("YMUX_PANE_ID").map(String::as_str), Some("new"));
    }

    #[test]
    fn refreshed_endpoint_is_none_when_the_file_matches_the_current_address() {
        let m = map(&[
            ("YMUX_SOCKET_ADDR", "127.0.0.1:34173"),
            ("YMUX_TUNNEL_TOKEN", "t"),
        ]);
        // The desktop really is unreachable — report that once, cleanly,
        // rather than dialing the same dead port twice.
        assert!(refreshed_endpoint_from(&m, "127.0.0.1:34173").is_none());
    }

    #[test]
    fn refreshed_endpoint_returns_the_file_pair_when_the_address_changed() {
        let m = map(&[
            ("YMUX_SOCKET_ADDR", "127.0.0.1:44495"),
            ("YMUX_TUNNEL_TOKEN", "fresh"),
        ]);
        // The exact case from the log: a hook inherited 34173 from a claude
        // started an hour earlier, while the tunnel had moved to 44495.
        let (addr, token) =
            refreshed_endpoint_from(&m, "127.0.0.1:34173").expect("a fresher endpoint");
        assert_eq!(addr, "127.0.0.1:44495");
        assert_eq!(token, "fresh", "the token must come from the same file");
    }

    #[test]
    fn refreshed_endpoint_accepts_the_legacy_winmux_spelling() {
        let m = map(&[
            ("WINMUX_SOCKET_ADDR", "127.0.0.1:44495"),
            ("WINMUX_TUNNEL_TOKEN", "fresh"),
        ]);
        let (addr, _) = refreshed_endpoint_from(&m, "127.0.0.1:1").expect("legacy names count");
        assert_eq!(addr, "127.0.0.1:44495");
    }

    #[test]
    fn refreshed_endpoint_is_none_without_a_token() {
        // A half-written file is not a reason to attempt an unauthenticated
        // handshake; the server would reject it anyway.
        let m = map(&[("YMUX_SOCKET_ADDR", "127.0.0.1:44495")]);
        assert!(refreshed_endpoint_from(&m, "127.0.0.1:1").is_none());
    }
}
