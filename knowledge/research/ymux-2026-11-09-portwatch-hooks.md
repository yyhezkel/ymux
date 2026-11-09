<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: ymux-2026-08-23-manual, ymux-2026-08-23-manual-2 -->

**Q1: don't build netlink, change the tick from 1 s to 3 s. Q2: don't pool. Add timing and raise the `session-start` budget.** Nothing below ran in a live build, and no repo files changed.

## Q1: port-watch

**Facts**
- The 1 s sleep is at `port_watch.rs:275`.
- The parse skip at `:250` never fired: bytes were identical on 0 of 59 ticks, because timer columns change. The vault's "does nothing" (`cli.md:111`) is false in practice.
- `/proc` cost is mostly a fixed walk of the established hash (`tcp_ehash_entries=262144`), not rows. tcp costs 7.1 ms at 421 rows, tcp6 6.0 ms at 101, so about 5.5 ms fixed per file plus about 3.7 µs/row. That predicts about 21 ms for 2,589 rows at this hash size.

**Method:** `getrusage` over 7×1000 reads, plus the live watcher's `/proc/PID/stat` for 61 s (`utime` 5 ticks, `stime` 102). Scripts are in `/tmp/claude-1000/-home-runner-winmuxMac-winmux/60b24d38-ff63-4067-ae09-04ba42090f73/scratchpad/`.

| Option | ms/read | % of one core |
|---|---|---|
| `/proc`, 1 s (528 rows) | 13.4 (live about 17.5) | 1.3–1.75 |
| `/proc`, 3 s | same | 0.45–0.58 |
| `/proc`, 5 s | same | 0.27–0.35 |
| Netlink, 1 s (77 rows) | 1.2–1.3 | 0.12 |

The saving is real but small: about 1.6% of a core per host, and one watcher per host since `lib.rs:5547`.

**Netlink traps**
- A `/proc` fallback is mandatory: `CONFIG_INET_DIAG=m` here, and module-less containers or gVisor can refuse it.
- The network namespace matches `/proc`, an unprivileged dump works (run as uid 1000), and musl is unaffected.
- `socket2` is already in `Cargo.lock`, so it can avoid `unsafe`. It still needs about 100 lines, byte-based address classifiers and a permanent second code path.

**Alternatives rejected:** pausing when no pane is attached needs a desktop signal and misses background agents. Adaptive ticking has no cheap predictor of a new listener.

**Recommendation:** tick 3 s. Events are detection-only (`rpc_server.rs`, `port-detected`), so the cost is at most 2 s later detection. Touches `port_watch.rs:4,275` and `docs/vault/cli.md:110`, about 3 lines.

**Invalidated by:** a measured watcher above 3% of a core at 3 s, or a UI need for sub-second detection.

## Q2: hooks

Claude awaits every hook: the spec has no `async` flag (`hooks/claude-code.json`).

| Event | RPCs | Blocks on |
|---|---|---|
| PreToolUse, default mode | 1, plus local POST `:7879` | Human, up to 600 s if Gate (`main.rs:2731`) |
| PreToolUse, bypass/acceptEdits | 0 (`:2400-2419`) | Nothing |
| SessionStart | 1 `context.inject` (`:2457`) | 300 ms budget, fail-open |
| UserPromptSubmit, Stop, SessionEnd | 1 `feed.push` (`:2734`) | Reply |
| Notification | 0 or 1 (`:2524`) | Reply |
| StopFailure | 1 (`:2610`) | Reply |

**Round trips:** 3 per call: channel open plus challenge, HMAC verdict, request (`main.rs:1396-1470`, `ymux-tunnel/lib.rs:92-135`). Pooling removes 2.

**Latency is already recorded.** REQ→ACK in `~/.ymux/hook-debug.log`, machine-decided pairs under 2 s (n=271): p50 368 ms, p90 405 ms. SSH RTT from `ss -ti` is 124–177 ms, which fits 3 RTT. Process exec is about 5 ms. The sample is one day and 2 panes.

**Share of agent time:** 2.0% of burst wall time overall (4.1% at the median gap, 14% at p10, gaps from REQ spacing). Pooling would recover about two-thirds of that, roughly 1.3%.

**Bug found:** the 300 ms `context.inject` budget (`main.rs:1248`) is below 3 RTT at this latency. Only 16% of measured pushes finished under 300 ms, so injection likely fails open most of the time. This is inferred: this box has no ymux hooks registered and no `session-start` log lines.

| Option | Saves | Size | Risk |
|---|---|---|---|
| Timing fields + budget | Nothing directly | About 10 lines | None |
| Send request with HMAC response | 1 RTT | About 5 lines, CLI | Older desktops unverified; `bridge_copy` keeps read-ahead (`:246`) |
| Pool via `ymux-server` | 2 RTT | Hundreds of lines, Go and Rust | Reconnects, stale ports |

**Recommendation:** no pool. Add `connect/handshake/reply ms` to the ACK and passive lines, log the `session-start` outcome at Info, and raise the default budget to 1000 ms. Touches `main.rs` and `docs/vault/cli.md`.

**Invalidated by:** hooks above 10% of burst wall time across several real remotes.

## Compared with the prior reports

- **manual (netlink):** differs. It recommended netlink with `unsafe` `libc`, citing 50–90×. I measured about 11× here, and the absolute gain doesn't pay for a second path. It attributes cost to table size; here the fixed hash walk dominates. Its 4.7% figure was at a 500 ms tick.
- **manual-2 (pooling):** same "no pool", different basis. It says no end-to-end latency exists; the REQ/ACK timestamps provide it. Its own trigger (setup above 100 ms) looks met, at about 250 ms, but 2% of wall time still doesn't justify pooling. It did not flag the 300 ms budget as breached.
