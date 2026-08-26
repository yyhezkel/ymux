//! Phase 36 (#2.2): listening-port watcher.
//!
//! Runs on the remote Linux box (this binary cross-compiles to
//! `ymux-linux-x64`). Every 1s it reads `/proc/net/tcp` +
//! `/proc/net/tcp6`; when either file changed since the last tick it
//! extracts sockets in the LISTEN state and diffs the set against the
//! previous tick. New ports send a `port.opened` RPC notification to the
//! desktop, vanished ports `port.closed`. On the desktop `port.opened` is
//! **detection only**: `rpc_server.rs` records the port and emits a
//! `port-detected` event for the UI; the forward itself is opened by the
//! user (or a rule) from there — the watcher never opens anything.
//!
//! Lifetime (Phase 86): the watcher is an SSH exec child with no PTY, so
//! nothing signals it when its channel dies. Two exits cover that: a
//! stdin watchdog (sshd closes stdin when the channel goes — EOF → exit 0)
//! and a connect-failure counter on the pinned tunnel (3 in a row → exit
//! 0, never the last.env fallback — that would attach a stale watcher to
//! a newer tunnel).
//!
//! Only loopback (127.0.0.1 / ::1) and bind-any (0.0.0.0 / ::) listeners
//! are considered — those are what dev servers use. Specific-LAN-IP
//! binds are ignored. Ports below 1024, SSH (22), and anything in
//! `YMUX_PORTFORWARD_EXCLUDE` are filtered out.

use std::collections::HashSet;

use serde_json::json;

/// A forwardable listening socket, normalized for diffing + display.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListenPort {
    pub port: u16,
    /// "v4" or "v6".
    pub family: &'static str,
    /// Human-readable bind address ("127.0.0.1", "0.0.0.0", "::1", "::").
    pub addr: String,
}

/// How the bind address classifies. Only Loopback / Any are forwarded.
#[derive(Debug, PartialEq, Eq)]
enum AddrClass {
    Loopback,
    Any,
    Other,
}

/// Classify an IPv4 `/proc/net/tcp` local-address hex (8 chars, the u32
/// in little-endian memory order). 127.x.x.x → Loopback, 0.0.0.0 → Any.
fn classify_v4(hex: &str) -> (AddrClass, String) {
    if hex.len() != 8 {
        return (AddrClass::Other, hex.to_string());
    }
    // Bytes are little-endian: chars [0..2] = octet 0 (LSB) … [6..8] = octet 3 (MSB).
    let b = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    let o0 = b(0);
    let o1 = b(2);
    let o2 = b(4);
    let o3 = b(6);
    // Network order is o3.o2.o1.o0.
    let display = format!("{}.{}.{}.{}", o3, o2, o1, o0);
    if o3 == 0 && o2 == 0 && o1 == 0 && o0 == 0 {
        (AddrClass::Any, "0.0.0.0".to_string())
    } else if o3 == 127 {
        (AddrClass::Loopback, display)
    } else {
        (AddrClass::Other, display)
    }
}

/// Classify an IPv6 `/proc/net/tcp6` local-address hex (32 chars).
fn classify_v6(hex: &str) -> (AddrClass, String) {
    if hex.len() != 32 {
        return (AddrClass::Other, hex.to_string());
    }
    let upper = hex.to_ascii_uppercase();
    if upper.chars().all(|c| c == '0') {
        (AddrClass::Any, "::".to_string())
    } else if upper == "00000000000000000000000001000000" {
        // ::1 — last 32-bit word is 0x00000001 in little-endian = "01000000".
        (AddrClass::Loopback, "::1".to_string())
    } else {
        (AddrClass::Other, hex.to_string())
    }
}

/// Parse a `/proc/net/tcp` or `/proc/net/tcp6` file body. `is_v6`
/// selects address decoding. Returns only forwardable (loopback / any)
/// LISTEN-state sockets.
pub fn parse_proc_net_tcp(content: &str, is_v6: bool) -> Vec<ListenPort> {
    let mut out = Vec::new();
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        // cols[1] = local_address "HEXIP:HEXPORT", cols[3] = st (state).
        if cols.len() < 4 {
            continue;
        }
        // LISTEN is state 0x0A.
        if !cols[3].eq_ignore_ascii_case("0A") {
            continue;
        }
        let (ip_hex, port_hex) = match cols[1].split_once(':') {
            Some(p) => p,
            None => continue,
        };
        let port = match u16::from_str_radix(port_hex, 16) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let (class, addr) = if is_v6 {
            classify_v6(ip_hex)
        } else {
            classify_v4(ip_hex)
        };
        if class == AddrClass::Other {
            continue;
        }
        out.push(ListenPort {
            port,
            family: if is_v6 { "v6" } else { "v4" },
            addr,
        });
    }
    out
}

/// Apply the port-level filters: skip privileged ports (<1024), SSH
/// (22), and anything in the comma-separated exclude list.
pub fn should_forward(port: u16, exclude: &HashSet<u16>) -> bool {
    if port < 1024 {
        return false;
    }
    if port == 22 {
        return false;
    }
    if exclude.contains(&port) {
        return false;
    }
    true
}

fn parse_exclude_env() -> HashSet<u16> {
    let mut set: HashSet<u16> = std::env::var("YMUX_PORTFORWARD_EXCLUDE")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|p| p.trim().parse::<u16>().ok())
                .collect()
        })
        .unwrap_or_default();
    // Phase 39: never report ymux's own reverse-tunnel port. The
    // backend filters it too (defence in depth), but excluding it here
    // avoids even sending the port.opened RPC. YMUX_SOCKET_ADDR is
    // "127.0.0.1:<remote_port>".
    if let Ok(addr) = std::env::var("YMUX_SOCKET_ADDR") {
        if let Some(port) = addr.rsplit(':').next().and_then(|p| p.trim().parse::<u16>().ok()) {
            set.insert(port);
        }
    }
    set
}

/// Raw `/proc/net/tcp{,6}` bodies from one tick. Kept verbatim so the next
/// tick can skip parsing when nothing moved (the common case, 1/s forever).
#[derive(Default, PartialEq, Eq)]
struct RawSnapshot {
    v4: String,
    v6: String,
}

fn read_raw() -> RawSnapshot {
    RawSnapshot {
        v4: std::fs::read_to_string("/proc/net/tcp").unwrap_or_default(),
        v6: std::fs::read_to_string("/proc/net/tcp6").unwrap_or_default(),
    }
}

fn parse_snapshot(raw: &RawSnapshot, exclude: &HashSet<u16>) -> HashSet<ListenPort> {
    let mut set = HashSet::new();
    for (content, is_v6) in [(&raw.v4, false), (&raw.v6, true)] {
        for lp in parse_proc_net_tcp(content, is_v6) {
            if should_forward(lp.port, exclude) {
                set.insert(lp);
            }
        }
    }
    set
}

/// Consecutive tunnel connect failures before the watcher gives up.
const MAX_CONNECT_FAILURES: u32 = 3;

/// Send one port event on the pinned tunnel. Returns false only when the
/// tunnel could not be reached (a transport error, not an RPC error).
async fn notify(method: &str, workspace_id: &str, lp: &ListenPort) -> bool {
    match crate::rpc_call_pinned(
        method,
        json!({
            "workspace_id": workspace_id,
            "addr": lp.addr,
            "port": lp.port,
            "family": lp.family,
        }),
    )
    .await
    {
        Ok(_) => true,
        // Both transports (`rpc_call_with`) prefix a dial failure with
        // "connect"; anything else reached the desktop and got an answer.
        Err(e) => !e.starts_with("connect "),
    }
}

/// The watch loop. Diverges: it only leaves via `std::process::exit` —
/// stdin EOF (the SSH channel died) or three consecutive failures to
/// connect to the tunnel it was born with. Each RPC opens a fresh tunnel
/// connection — chatty but simple — and `port.opened` is idempotent on
/// the desktop (a repeat just re-records the same port), so two watchers
/// from two panes are harmless.
pub async fn run(workspace_id: &str) -> ! {
    crate::hook_log(
        crate::HookLevel::Info,
        &format!("port-watch start workspace={workspace_id}"),
    );
    // Stdin watchdog: this is an SSH exec child without a PTY, so the
    // only thing that tells us the channel is gone is sshd closing stdin.
    // A plain thread, not tokio::io::stdin: that needs the `io-std`
    // feature, and a blocking read on a dedicated thread costs nothing.
    std::thread::spawn(|| {
        let mut sink = [0u8; 256];
        let mut stdin = std::io::stdin().lock();
        loop {
            match std::io::Read::read(&mut stdin, &mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        crate::hook_log(crate::HookLevel::Info, "port-watch exit: stdin eof");
        std::process::exit(0);
    });

    let exclude = parse_exclude_env();
    let mut prev_raw = RawSnapshot::default();
    let mut prev: HashSet<ListenPort> = HashSet::new();
    let mut connect_failures: u32 = 0;
    loop {
        let raw = read_raw();
        // Byte-identical /proc files → nothing to parse or diff. The first
        // tick always differs from the empty default, so everything already
        // listening at connect time is reported.
        if raw != prev_raw {
            let cur = parse_snapshot(&raw, &exclude);
            let events: Vec<(&str, &ListenPort)> = cur
                .difference(&prev)
                .map(|lp| ("port.opened", lp))
                .chain(prev.difference(&cur).map(|lp| ("port.closed", lp)))
                .collect();
            if !events.is_empty() {
                let mut reached = true;
                for (method, lp) in &events {
                    reached &= notify(method, workspace_id, lp).await;
                }
                if reached {
                    connect_failures = 0;
                } else {
                    connect_failures += 1;
                    if connect_failures >= MAX_CONNECT_FAILURES {
                        crate::hook_log(crate::HookLevel::Info, "port-watch exit: tunnel gone");
                        std::process::exit(0);
                    }
                }
            }
            prev = cur;
            prev_raw = raw;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    #[test]
    fn parses_normal_v4_listen() {
        // 0100007F = 127.0.0.1, 0BB8 = 3000, st 0A = LISTEN.
        let body = format!(
            "{HEADER}\n   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 12345 1"
        );
        let ports = parse_proc_net_tcp(&body, false);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 3000);
        assert_eq!(ports[0].family, "v4");
        assert_eq!(ports[0].addr, "127.0.0.1");
    }

    #[test]
    fn parses_v6_loopback_listen() {
        // ::1 = 00000000000000000000000001000000, 1F90 = 8080.
        let body = format!(
            "{HEADER}\n   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 99 1"
        );
        let ports = parse_proc_net_tcp(&body, true);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 8080);
        assert_eq!(ports[0].family, "v6");
        assert_eq!(ports[0].addr, "::1");
    }

    #[test]
    fn skips_established_sockets() {
        // st 01 = ESTABLISHED — must be skipped.
        let body = format!(
            "{HEADER}\n   0: 0100007F:0BB8 0100007F:C001 01 00000000:00000000 00:00000000 00000000  1000 0 12345 1"
        );
        let ports = parse_proc_net_tcp(&body, false);
        assert!(ports.is_empty());
    }

    #[test]
    fn parses_bind_any_listen() {
        // 00000000 = 0.0.0.0 (bind-any), 1F90 = 8080, st 0A.
        let body = format!(
            "{HEADER}\n   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 777 1"
        );
        let ports = parse_proc_net_tcp(&body, false);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 8080);
        assert_eq!(ports[0].addr, "0.0.0.0");
    }

    #[test]
    fn filters_privileged_and_excluded() {
        let mut ex = HashSet::new();
        ex.insert(9000u16);
        assert!(!should_forward(80, &ex)); // privileged
        assert!(!should_forward(22, &ex)); // ssh
        assert!(!should_forward(9000, &ex)); // excluded
        assert!(should_forward(3000, &ex)); // ok
    }

    #[test]
    fn identical_raw_snapshot_skips_reparse() {
        // Phase 86: the loop parses only when the raw bodies changed.
        let a = RawSnapshot { v4: "x".into(), v6: "y".into() };
        let b = RawSnapshot { v4: "x".into(), v6: "y".into() };
        let c = RawSnapshot { v4: "x".into(), v6: "z".into() };
        assert!(a == b);
        assert!(a != c);
        // The first tick always differs from the empty default.
        assert!(a != RawSnapshot::default());
    }
}
