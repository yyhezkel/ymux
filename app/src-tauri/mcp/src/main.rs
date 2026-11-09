// Phase 8.F.4 — ymux-mcp: stdio MCP server bridging Claude Code / Cursor /
// any MCP-aware agent into the running ymux app's named-pipe RPC.
//
// Wire: agent ⇄ stdio JSON-RPC ⇄ ymux-mcp ⇄ \\.\pipe\ymux-<user> ⇄ app.
// Each `tools/call` opens a fresh pipe connection — server is stateless per
// call. The app must already be running; if the pipe is unreachable the
// tool result is an MCP error with that message.

use serde_json::{json, Value};
use std::io::{self};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    // Newline-delimited JSON-RPC. Each request → one response line. Server
    // exits when stdin closes.
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                write_line(&mut stdout, &err).await?;
                continue;
            }
        };

        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let params = req.get("params").cloned().unwrap_or(json!({}));

        // Notifications (no id) — handle silently. MCP sends
        // `notifications/initialized` etc. after the handshake.
        if id.is_null() {
            // Nothing to send back. Continue.
            continue;
        }

        let resp = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "ymux",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": { "tools": {} }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions() }
            }),
            "tools/call" => match handle_tool_call(params).await {
                Ok(v) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }]
                    }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": e }],
                        "isError": true
                    }
                }),
            },
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") }
            }),
        };

        write_line(&mut stdout, &resp).await?;
    }
}

async fn write_line(stdout: &mut tokio::io::Stdout, v: &Value) -> io::Result<()> {
    let s = serde_json::to_string(v).unwrap_or_default();
    stdout.write_all(s.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await
}

// ─── Tool dispatch ──────────────────────────────────────────────────────────

async fn handle_tool_call(params: Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("tools/call: missing `name`")?
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let (rpc_method, rpc_params) = match name.as_str() {
        // Discovery
        "list_workspaces" => ("list-workspaces", json!({})),
        "tree" => ("tree", args),
        // B1: full LLM control. Thin wrappers over the matching RPC
        // methods in rpc_server.rs:dispatch. Same JSON-RPC params
        // pass through unchanged.
        "list_panes" => ("ui.tree", args),
        "read_pane" => ("pane.scrollback", args),
        "take_screenshot" => ("pane.screenshot", args),
        "split_pane" => ("action.split", args),
        "connect_workspace" => ("action.connect", args),
        "send_keys" => ("action.send_keys", args),
        // Phase 53.G: the 11 browser_* tools (browser_navigate,
        // browser_url, browser_history, browser_go_back, browser_go_home,
        // browser_eval, browser_click, browser_type, browser_find,
        // browser_snapshot, browser_wait_for) were removed. Their
        // backing `pane.browser.*` RPC methods went away when the
        // per-pane Browser surface was folded into a workspace-level
        // floating Webview (Phase 53.D/E) and the iframe-bus that
        // backed the automation tools no longer exists. For agent-
        // driven browser automation, use `lean-chronoscope-mcp`
        // (yyhezkel/lean-chronoscope-mcp) — headless Chrome in Docker,
        // 56 tools across full/slim/gateway mount modes.
        // Agent affordances
        "notify" => ("notify", args),
        "note_add" => ("note-add", args),
        other => return Err(format!("unknown tool: {other}")),
    };

    rpc_call(rpc_method, rpc_params).await
}

// ─── Local RPC client ───────────────────────────────────────────────────────
// Mirrors the CLI's transport: a named pipe on Windows, a Unix-domain socket
// elsewhere. Stateless per call: open, write request, read one line back,
// close. The MCP server runs locally next to the user's agent; remote-agent
// use would target the CLI through the SSH tunnel.

#[cfg(windows)]
fn default_pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(whoami::username);
    format!(r"\\.\pipe\ymux-{}", user)
}

/// `dirs::config_dir() / "ymux"`, without taking the `dirs` dependency —
/// see the same pair in the CLI. Mirrors `ymux_core::config_dir`'s path
/// resolution (not its migration).
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
/// `ymux_core::pipe_names()` plus the pre-rename name. The app binds every
/// one, so the first that connects is a live endpoint. The second exists
/// because macOS caps `sockaddr_un.sun_path` at 104 bytes.
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
    v.push(
        tmp.join(format!("winmux-{user}.sock"))
            .to_string_lossy()
            .into_owned(),
    );
    v
}

async fn rpc_call(method: &str, params: Value) -> Result<Value, String> {
    #[cfg(windows)]
    {
        // `WINMUX_PIPE_PATH` fallback: the rename bridge. A pre-rename
        // desktop, or an MCP host config written before 0.5.0, only sets
        // the old spelling. Drop once every deployed desktop is ≥0.5.0.
        let name = std::env::var("YMUX_PIPE_PATH")
            .or_else(|_| std::env::var("WINMUX_PIPE_PATH"))
            .unwrap_or_else(|_| default_pipe_name());
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .map_err(|e| {
                format!(
                    "connect to {name}: {e} (is the ymux app running?)"
                )
            })?;
        rpc_via(pipe, method, params).await
    }
    // Unix: same candidate walk the CLI does. This used to hard-error, which
    // made the MCP tool unusable on macOS even though the app was listening.
    #[cfg(not(windows))]
    {
        let explicit = std::env::var("YMUX_PIPE_PATH")
            .or_else(|_| std::env::var("WINMUX_PIPE_PATH"))
            .ok()
            .filter(|s| !s.is_empty());
        let names = match &explicit {
            Some(p) => vec![p.clone()],
            None => default_socket_paths(),
        };
        let mut errors: Vec<String> = Vec::new();
        for name in &names {
            match tokio::net::UnixStream::connect(name).await {
                Ok(sock) => return rpc_via(sock, method, params).await,
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(format!(
            "connect to the local ymux socket failed ({}) — is the ymux app running?",
            errors.join("; ")
        ))
    }
}

async fn rpc_via<S>(stream: S, method: &str, params: Value) -> Result<Value, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (r, mut w) = tokio::io::split(stream);
    let mut reader = BufReader::new(r);
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let mut req_line = serde_json::to_string(&req).map_err(|e| format!("encode req: {e}"))?;
    req_line.push('\n');
    w.write_all(req_line.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    w.flush().await.ok();

    let mut buf = String::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        reader.read_line(&mut buf),
    )
    .await
    .map_err(|_| "ymux RPC read timed out (60s)".to_string())?
    .map_err(|e| format!("read: {e}"))?;
    if r == 0 {
        return Err("ymux closed pipe before response".into());
    }
    let resp: Value = serde_json::from_str(buf.trim()).map_err(|e| format!("parse resp: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("ymux RPC error: {}", err));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

// ─── Tool definitions ──────────────────────────────────────────────────────

fn obj(props: &[(&str, Value)], required: &[&str]) -> Value {
    let mut p = serde_json::Map::new();
    for (k, v) in props {
        p.insert((*k).to_string(), v.clone());
    }
    json!({
        "type": "object",
        "properties": p,
        "required": required,
    })
}

fn s(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}
// Phase 53.G: `i` (integer) and `b` (boolean) helpers were only used
// by the deleted browser_* tool schemas. The remaining tools
// (list_workspaces, tree, notify, note_add) only need string fields.
// Keep the helpers around as plain doc-only sketches if you need them
// for a new tool, or delete the comment and resurrect them when
// adding back.

fn tool_definitions() -> Vec<Value> {
    vec![
        // ── Discovery ────────────────────────────────────────────────────
        json!({
            "name": "list_workspaces",
            "description": "List ymux workspaces with their layouts. Returns the full WorkspacesFile JSON so the agent can find pane_ids.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "tree",
            "description": "Inspect a workspace's pane layout. Pass workspace_id to target a specific one; otherwise returns the active workspace's tree.",
            "inputSchema": obj(
                &[("workspace_id", s("workspace id (omit for the active workspace)"))],
                &[]
            )
        }),

        // ── B1: full LLM control ─────────────────────────────────────────
        json!({
            "name": "list_panes",
            "description": "Structured view of every workspace + its panes with kind/title/active markers. Higher-level than list_workspaces — designed for an agent that's about to act on a specific pane.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "read_pane",
            "description": "Read recent text from a pane, through its tmux or zellij session. Returns an error for a pane that has no multiplexer session.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("pane id (see list_panes)")),
                    ("lines", s("number of trailing lines (default 200, maximum 10000)"))
                ],
                &["pane_id"]
            )
        }),
        json!({
            "name": "take_screenshot",
            "description": "Capture the pane as a PNG. Note: terminal panes render on the frontend canvas, so the current backend returns an error pointing at read_pane as the text-only alternative.",
            "inputSchema": obj(
                &[("pane_id", s("pane id (see list_panes)"))],
                &["pane_id"]
            )
        }),
        json!({
            "name": "split_pane",
            "description": "Split a pane in the workspace tree. The new pane is a Terminal that inherits the workspace's connection.",
            "inputSchema": obj(
                &[
                    ("workspace_id", s("workspace id")),
                    ("pane_id", s("the parent pane (the one being split)")),
                    ("direction", s("\"horizontal\" (default) | \"vertical\""))
                ],
                &["workspace_id", "pane_id"]
            )
        }),
        json!({
            "name": "connect_workspace",
            "description": "Activate a workspace's UI tab. For SSH workspaces the frontend's active-workspace effect runs ensure_connected.",
            "inputSchema": obj(
                &[("workspace_id", s("workspace id"))],
                &["workspace_id"]
            )
        }),
        json!({
            "name": "send_keys",
            "description": "Send keystrokes to a pane. Same key translation as send-key (e.g. \"Enter\", \"Ctrl+C\", or literal text).",
            "inputSchema": obj(
                &[
                    ("pane_id", s("pane id")),
                    ("key", s("key name or literal text to send"))
                ],
                &["pane_id", "key"]
            )
        }),

        // ── Browser tools removed (Phase 53.G) ───────────────────
        // The 11 browser_* tools were thin wrappers over
        // pane.browser.* RPC methods that no longer exist. The
        // per-pane Browser surface moved to a workspace-level
        // floating Webview in Phase 53.D/E, and the iframe-bus
        // that backed the automation tools is gone. For agent-
        // driven browser automation, use lean-chronoscope-mcp
        // (yyhezkel/lean-chronoscope-mcp): headless Chrome in
        // Docker, 56 tools across full/slim/gateway mount modes.

        // ── Agent affordances ────────────────────────────────────────────
        json!({
            "name": "notify",
            "description": "Show a desktop toast in ymux. Use to ping the user when a long task finishes or a decision is needed.",
            "inputSchema": obj(
                &[
                    ("title", s("toast title")),
                    ("body", s("toast body")),
                    ("workspace_id", s("optional workspace id this notification belongs to"))
                ],
                &["title", "body"]
            )
        }),
        json!({
            "name": "note_add",
            "description": "Append a quick-capture note to ymux's notes. Tag is free-form; suggestions: idea, bug, todo. workspace_id / pane_id auto-attach context.",
            "inputSchema": obj(
                &[
                    ("text", s("note body")),
                    ("tag", s("optional tag (idea, bug, todo, …)")),
                    ("workspace_id", s("optional workspace id")),
                    ("pane_id", s("optional pane id"))
                ],
                &["text"]
            )
        }),
    ]
}
