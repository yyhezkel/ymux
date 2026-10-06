//! Phase 6.3: bridge a remote-forwarded TCP channel to the local Named Pipe RPC server.
//! Phase 6.4: replace the plain-token preamble with an HMAC-SHA256 challenge-response
//! handshake so the shared secret never travels in cleartext.
//!
//! Phase 51.C: moved out of `app/src-tauri/src/tunnel.rs` into its own
//! crate. Depends on `ymux-core` for `dlog`, `pipe_name`, and the
//! `SshClient` type alias used in russh `Handle<SshClient>` signatures.

use hmac::{Hmac, Mac};
use rand::RngCore;
use russh::{Channel, ChannelMsg};
use sha2::Sha256;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};

use ymux_core::{log_debug, log_info, log_warn, shell_quote, SshClient};
// Windows dials the single pipe name directly; the unix side walks the
// whole `ymux_core::pipe_names()` candidate list instead (sun_path cap).
#[cfg(windows)]
use ymux_core::pipe_name;

type HmacSha256 = Hmac<Sha256>;

const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

// ─── handshake wire tags (winmux → ymux rename) ──────────────────────
//
// The challenge we emit is `YMUX-CHALLENGE`. Both ends still read *and
// mirror* either dialect, so a pre-rename `WINMUX-RESPONSE` is accepted
// and answered in kind:
//
//   new desktop ↔ new CLI   → ymux, native
//   new desktop ↔ old CLI   → ymux challenge; an old CLI that only
//                             matches `WINMUX-CHALLENGE` needs re-bootstrap
//   old desktop ↔ new CLI   → legacy tag, new CLI mirrors it back
//
// Mirrors `ChallengeTag` in server/internal/hooks/hooks.go — flip both
// together. The accept-both arms stay.
const YMUX_TAG: &str = "YMUX";
const LEGACY_TAG: &str = "WINMUX";
/// Dialect this side *emits* when it opens a handshake.
const CHALLENGE_TAG: &str = YMUX_TAG;

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

/// Perform the server-side half of the HMAC challenge-response.
/// Returns `Ok(())` if the client proved knowledge of the token; on failure, it has
/// already written `YMUX-DENIED ...` to the stream and the caller should drop it.
/// `peer` is the originator (`addr:port`) of the forwarded connection, or a
/// label for non-SSH transports. It is threaded through purely so a rejection
/// names WHO was rejected — every arm below used to be anonymous, which made
/// a repeating handshake failure impossible to attribute from debug.log alone.
async fn perform_handshake<S>(
    bs: &mut BufStream<S>,
    expected_token: &str,
    peer: &str,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // 1) Send challenge.
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    let challenge_line = format!("{CHALLENGE_TAG}-CHALLENGE {}\n", hex_encode(&nonce));
    bs.write_all(challenge_line.as_bytes())
        .await
        .map_err(|e| format!("write challenge: {e}"))?;
    bs.flush().await.map_err(|e| format!("flush: {e}"))?;

    // 2) Read response with a timeout — clients that never respond should be dropped.
    let mut line = String::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        bs.read_line(&mut line),
    )
    .await;
    match read {
        Ok(Ok(0)) => {
            return Err(format!("client closed before sending response (peer {peer})"))
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("read response from {peer}: {e}")),
        Err(_) => return Err(format!("response timed out (peer {peer})")),
    }
    let line = line.trim();
    // Accept either dialect and answer in the one we were spoken to, so a
    // pre-rename CLI never sees a verdict tag it can't parse.
    let (reply_tag, resp_hex) = match line
        .strip_prefix("YMUX-RESPONSE ")
        .map(|x| (YMUX_TAG, x))
        .or_else(|| line.strip_prefix("WINMUX-RESPONSE ").map(|x| (LEGACY_TAG, x)))
    {
        Some(x) => x,
        None => {
            let _ = bs
                .write_all(format!("{CHALLENGE_TAG}-DENIED bad-format\n").as_bytes())
                .await;
            let _ = bs.flush().await;
            return Err(format!("bad response framing from {peer}: {:?}", line));
        }
    };
    let resp = hex_decode(resp_hex)?;

    // 3) Verify HMAC in constant time (`Hmac::verify_slice`).
    let mut mac = HmacSha256::new_from_slice(expected_token.as_bytes())
        .map_err(|e| format!("hmac key: {e}"))?;
    mac.update(&nonce);
    if mac.verify_slice(&resp).is_err() {
        let _ = bs
            .write_all(format!("{reply_tag}-DENIED bad-mac\n").as_bytes())
            .await;
        let _ = bs.flush().await;
        return Err(format!("hmac verify failed (peer {peer})"));
    }

    // 4) Tell the client we're good.
    bs.write_all(format!("{reply_tag}-OK\n").as_bytes())
        .await
        .map_err(|e| format!("write OK: {e}"))?;
    bs.flush().await.map_err(|e| format!("flush OK: {e}"))?;
    Ok(())
}

pub async fn bridge_to_pipe(
    channel: Channel<russh::client::Msg>,
    expected_token: &str,
    peer: &str,
) -> Result<(), String> {
    bridge_stream_to_pipe(channel.into_stream(), expected_token, peer).await
}

/// Phase 80: the transport-agnostic core of `bridge_to_pipe` — same HMAC
/// handshake + pipe bridge over ANY duplex stream. The WSL RPC bridge
/// feeds it plain TcpStreams (WSL2 Linux can't reach Windows named
/// pipes); the SSH reverse-tunnel path feeds it russh channel streams.
pub async fn bridge_stream_to_pipe<S>(
    stream: S,
    expected_token: &str,
    peer: &str,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut bs = BufStream::new(stream);

    if let Err(e) = perform_handshake(&mut bs, expected_token, peer).await {
        log_warn("TUNNEL", &format!("tunnel: handshake REJECTED — {e}"));
        return Err(e);
    }
    log_debug("TUNNEL", &format!("tunnel: handshake OK (peer {peer})"));

    // Open a fresh client connection to the local pipe server.
    // Phase 39.A: on ERROR_PIPE_NOT_AVAILABLE (231) — all server
    // instances momentarily busy — retry with bounded exponential
    // backoff instead of failing the bridge. After the rpc_server cap
    // lift + parallel-accept fixes this path should be effectively
    // unreachable, but a remote agent that races a hair ahead of the
    // server no longer turns a transient busy into a hard error +
    // log spam. Per-attempt waits are silent (tracing::debug only);
    // a genuine give-up surfaces via spawn_bridge's dlog.
    #[cfg(windows)]
    let pipe_name = pipe_name();
    #[cfg(windows)]
    let pipe = {
    let mut backoff_ms = 25u64;
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(231) && backoff_ms <= 800 => {
                tracing::debug!(
                    "tunnel: pipe busy (231), retrying in {}ms",
                    backoff_ms
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms *= 2;
            }
            Err(e) => return Err(format!("open pipe {}: {}", pipe_name, e)),
        }
    }
    };
    // Unix: the rpc_server listens on a Unix domain socket; the kernel
    // queues concurrent connects in the listener backlog, so no
    // busy-retry loop is needed. It may have fallen back to the second
    // candidate path (see ymux_core::pipe_names), so try them in the
    // same order the server does — otherwise the two ends split and every
    // RPC dies with ENOENT on a path nobody is listening on.
    #[cfg(not(windows))]
    let pipe = {
        let mut connected = None;
        let mut errors: Vec<String> = Vec::new();
        for name in ymux_core::pipe_names() {
            match tokio::net::UnixStream::connect(&name).await {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        match connected {
            Some(s) => s,
            None => return Err(format!("open socket: {}", errors.join("; "))),
        }
    };

    log_debug("TUNNEL", "tunnel: bridging channel <-> pipe");
    bridge_copy(bs, pipe).await;
    Ok(())
}

/// v0.3.1 (pipe-leak belt-and-suspenders): copy each direction and finish as
/// soon as EITHER side reaches EOF, shutting down the peer's write so the other
/// end unblocks immediately. `copy_bidirectional` waits for BOTH directions to
/// close, which deadlocked here: the russh channel stream never surfaced the
/// remote CLI's close, so the pipe stayed open and its rpc_server instance
/// leaked (after 254, ERROR_PIPE_BUSY wedged every connection). Half-closing on
/// first-EOF frees the pipe instance the moment the one-shot RPC reply is done
/// — independent of the handler-side one-shot fix.
async fn bridge_copy<A, B>(a: A, b: B)
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut ar, mut aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);
    let a2b = async {
        let n = tokio::io::copy(&mut ar, &mut bw).await;
        let _ = bw.shutdown().await; // EOF → unblocks the peer's read
        n
    };
    let b2a = async {
        let n = tokio::io::copy(&mut br, &mut aw).await;
        let _ = aw.shutdown().await;
        n
    };
    tokio::select! {
        r = a2b => log_debug("TUNNEL", &format!("tunnel: bridge done (a→b: {r:?})")),
        r = b2a => log_debug("TUNNEL", &format!("tunnel: bridge done (b→a: {r:?})")),
    }
}

/// Random alphanumeric token for the per-connection tunnel.
pub fn generate_token() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// The body of `~/.ymux/run/last.env`. Pure, so the shape is testable
/// without an SSH session.
///
/// YMUX_* lines only; the legacy WINMUX_* spelling is no longer written
/// (the CLI still reads it as a fallback).
///
/// `pane_id: None` omits the pane lines entirely, so the caller's
/// read-modify-write can preserve whatever the file already had. Writing an
/// EMPTY pane id would be much worse than omitting it: the remote hook reads
/// the variable's absence as "ymux is not in this session" and stops gating
/// tool calls, i.e. every permission card silently disappears.
pub fn render_env_file(socket_addr: &str, token: &str, pane_id: Option<&str>) -> String {
    let mut body = format!(
        "YMUX_SOCKET_ADDR={socket_addr}\nYMUX_TUNNEL_TOKEN={token}\n"
    );
    if let Some(p) = pane_id {
        body.push_str(&format!("YMUX_PANE_ID={p}\n"));
    }
    body
}

/// Write the env file `~/.ymux/run/last.env` on the remote so the CLI can pick up
/// `YMUX_SOCKET_ADDR` and `YMUX_TUNNEL_TOKEN` even if sshd's `AcceptEnv` rejects
/// the per-channel `set_env` requests.
///
/// Phase 80: `pane_id` is optional, because the headless connect path now
/// writes this file too and has no pane. When it is `None` the existing
/// `YMUX_PANE_ID` is read back off the remote and carried over, so a
/// workspace-level refresh can never blank out a pane's identity.
///
/// Also Phase 80: tmp + `mv -f` instead of writing the live file in place.
/// A hook can be reading `last.env` at the moment we rewrite it, and the old
/// heredoc left a window where it would see a truncated file.
///
/// Takes `&Handle` rather than `&mut` — only `channel_open_session` is used,
/// and that takes `&self`. That is what lets the caller hold an `Arc<Handle>`.
pub async fn write_remote_env_file(
    handle: &russh::client::Handle<SshClient>,
    home: &str,
    socket_addr: &str,
    token: &str,
    pane_id: Option<&str>,
) -> Result<(), String> {
    let env_dir = format!("{}/.ymux/run", home);
    let env_file = format!("{}/last.env", env_dir);
    let body = render_env_file(socket_addr, token, pane_id);

    // Rule #3: a fixed script literal, every interpolated value quoted. The
    // values are ours (a 127.0.0.1:port, a generated alphanumeric token, an
    // internally-generated pane id), but quoting them is the rule, not a
    // judgement call about this particular call site.
    // The pane lines are appended by `printf` rather than folded into the
    // shell variable: inside double quotes a `\n` is a literal backslash-n,
    // so string-concatenating them would write the whole file on one line.
    let carry_pane = if pane_id.is_none() { "1" } else { "0" };
    let script = format!(
        "set -e\n\
         d={dir}\n\
         f=\"$d/last.env\"\n\
         mkdir -p \"$d\"\n\
         pid=''\n\
         if [ {carry_pane} = 1 ]; then\n\
         pid=$(sed -n 's/^YMUX_PANE_ID=//p' \"$f\" 2>/dev/null | tail -n1)\n\
         fi\n\
         printf '%s' {body} > \"$f.tmp\"\n\
         if [ -n \"$pid\" ]; then\n\
         printf 'YMUX_PANE_ID=%s\\n' \"$pid\" >> \"$f.tmp\"\n\
         fi\n\
         chmod 0600 \"$f.tmp\"\n\
         mv -f \"$f.tmp\" \"$f\"\n",
        dir = shell_quote(&env_dir),
        body = shell_quote(&body),
        carry_pane = carry_pane,
    );
    exec_simple(handle, &script).await?;
    log_info(
        "TUNNEL",
        &format!("tunnel: wrote {} ({} bytes)", env_file, body.len()),
    );
    Ok(())
}

async fn exec_simple(
    handle: &russh::client::Handle<SshClient>,
    cmd: &str,
) -> Result<(), String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open exec channel: {e}"))?;
    chan.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    if exit_code != 0 {
        return Err(format!("exec '{}' exit {}", cmd, exit_code));
    }
    Ok(())
}

/// Used as a small helper inside the russh `Handler`: spawn a bridge task. Exists so
/// the trait method body stays tiny.
pub fn spawn_bridge(
    channel: Channel<russh::client::Msg>,
    token: std::sync::Arc<String>,
    peer: String,
) {
    tokio::spawn(async move {
        if let Err(e) = bridge_to_pipe(channel, &token, &peer).await {
            tracing::warn!("tunnel bridge: {e}");
            log_warn("TUNNEL", &format!("tunnel: bridge error: {e}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{bridge_copy, render_env_file};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn env_file_carries_only_ymux_spellings() {
        let body = render_env_file("127.0.0.1:44495", "tok123", Some("p_1_0"));
        // The legacy spelling is retired from the writer; a regression that
        // re-adds it would show up as a WINMUX_ line below.
        for expect in [
            "YMUX_SOCKET_ADDR=127.0.0.1:44495",
            "YMUX_TUNNEL_TOKEN=tok123",
            "YMUX_PANE_ID=p_1_0",
        ] {
            assert!(
                body.lines().any(|l| l == expect),
                "missing line {expect:?} in:\n{body}"
            );
        }
        assert!(!body.contains("WINMUX_"), "legacy env leaked:\n{body}");
    }

    #[test]
    fn env_file_omits_the_pane_id_line_when_the_caller_has_none() {
        let body = render_env_file("127.0.0.1:1", "t", None);
        // Omitted, never written empty: the remote hook reads a MISSING
        // YMUX_PANE_ID as "ymux is not in this session" and stops gating, so
        // an empty value would silently disable every permission card. The
        // headless path relies on this and carries the old id over instead.
        assert!(!body.contains("PANE_ID"), "got:\n{body}");
        assert!(body.contains("YMUX_SOCKET_ADDR=127.0.0.1:1"));
    }

    #[test]
    fn env_file_terminates_every_line_including_the_last() {
        let with_pane = render_env_file("a", "b", Some("p"));
        let without = render_env_file("a", "b", None);
        // The remote reads this with `sed -n 's/^YMUX_PANE_ID=//p'`, which
        // needs real line breaks — an unterminated tail would fuse into
        // whatever the read-modify-write appends next.
        assert!(with_pane.ends_with('\n'), "got: {with_pane:?}");
        assert!(without.ends_with('\n'), "got: {without:?}");
        assert_eq!(with_pane.lines().count(), 3);
        assert_eq!(without.lines().count(), 2);
    }

    // v0.3.1 pipe-leak fix: when the rpc_server handler closes after sending
    // its one-shot reply, the bridge must release PROMPTLY (not hang waiting
    // for the channel side to also EOF, as copy_bidirectional did). Models the
    // bridge with two in-memory duplex pipes: `cli` <-> bridge <-> `server`.
    #[tokio::test]
    async fn bridge_releases_when_server_closes_after_one_reply() {
        let (mut cli, bridge_a) = tokio::io::duplex(1024);
        let (bridge_b, mut server) = tokio::io::duplex(1024);
        let bridge = tokio::spawn(async move { bridge_copy(bridge_a, bridge_b).await });

        // CLI sends one request.
        cli.write_all(b"REQ\n").await.unwrap();

        // Server (rpc_server handler) reads it, replies once, then CLOSES.
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"REQ\n");
        server.write_all(b"RESP\n").await.unwrap();
        drop(server); // one-shot handler returns → stream dropped

        // CLI receives the reply, then EOF (bridge shut its write down).
        let mut out = Vec::new();
        cli.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"RESP\n");

        // The bridge task must finish promptly — the whole point of the fix.
        tokio::time::timeout(std::time::Duration::from_secs(2), bridge)
            .await
            .expect("bridge_copy must release after the server closed")
            .unwrap();
    }

    // The mirror case: when the CLI side closes first (channel EOF), the bridge
    // must shut the pipe write down so the rpc_server handler's read sees EOF.
    #[tokio::test]
    async fn bridge_releases_when_client_closes_first() {
        let (cli, bridge_a) = tokio::io::duplex(1024);
        let (bridge_b, mut server) = tokio::io::duplex(1024);
        let bridge = tokio::spawn(async move { bridge_copy(bridge_a, bridge_b).await });

        drop(cli); // remote CLI hung up

        // The server side must observe EOF (read returns 0), not hang.
        let mut buf = [0u8; 16];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), server.read(&mut buf))
            .await
            .expect("server read must not hang after client close")
            .unwrap();
        assert_eq!(n, 0, "server should see EOF");
        tokio::time::timeout(std::time::Duration::from_secs(2), bridge)
            .await
            .expect("bridge must release after client close")
            .unwrap();
    }
}
