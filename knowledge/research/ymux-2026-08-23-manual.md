# ymux-2026-08-23-manual — research report

## Question

Can the remote port watcher (`app/src-tauri/cli/src/port_watch.rs`) stop reading the whole `/proc/net/tcp{,6}` table on every 1 s tick and use a raw netlink `sock_diag` INET_DIAG dump filtered to `idiag_states = 1<<TCP_LISTEN`, so that each tick sees only the ~50–90 listening sockets? The questions below cover the kernel ABI, what the tree already provides, the current code and its consumers, the Phase 86.A skip path, fallback, and the per-tick cost.

### Q1 — Does an INET_DIAG dump with `idiag_states = 1<<TCP_LISTEN` return only listening sockets for both IPv4 and IPv6 TCP
### Q2 — Which raw netlink message layout (NETLINK_SOCK_DIAG, SOCK_DIAG_BY_FAMILY, nlmsghdr + inet_diag_req_v2) a listener-only dump needs, and which libc or std APIs the tree already exposes for it
### Q3 — How the current port-watch tick reads `/proc/net/tcp{,6}`, and which fields (port, inode, pid) its downstream consumers use
### Q4 — Whether the Phase 86.A skip-when-unchanged path still applies once the source changes from /proc bytes to netlink replies
### Q5 — Whether the netlink listener set carries the same port, inode and pid the /proc parser produces, so callers stay unchanged
### Q6 — What fallback is needed when netlink sock_diag is unavailable or denied, and whether `/proc` must remain as the fallback
### Q7 — How much per-tick cost the /proc read carries versus the netlink dump, and whether the 2,589-line figure is reproducible

## Findings

All measurements were taken on the pipeline host `vmi1391008.contaboserver.net` (kernel `5.4.0-216-generic`, uid 1005, unprivileged). They used a throwaway Python script outside the repo (`/tmp/nlres/diag.py`, `/tmp/nlres/stab.py`) that builds the same raw netlink request a Rust version would send. Nothing in the repo was changed. The pipeline host is not necessarily "Yossi's server" from the 2026-08-23 measurement (see Q7).

### Q1

**Answer: yes, for both families.** One `SOCK_DIAG_BY_FAMILY` dump per family (`AF_INET`, then `AF_INET6`) with `idiag_states = 1<<10` returned only state-10 (LISTEN) rows. The combined count matched the LISTEN rows in `/proc` exactly.

- `python3 -I /tmp/nlres/diag.py check` →
  `fam AF_INET rows 71 bytes 6836 states [10]`, `fam AF_INET6 rows 16 bytes 1684 states [10]`, and with all states (`0xfff`) `all v4 rows 3307 bytes 300108`.
- `awk 'NR>1 && $4=="0A"' /proc/net/tcp /proc/net/tcp6 | wc -l` → `87`; `ss -ltnH | wc -l` → `87` (`ss` uses this same netlink interface). 71 + 16 = 87.
- sock_diag(7): `idiag_states` "is a bit mask that defines a filter of socket states", and it applies to dumps (`NLM_F_DUMP`) but is ignored for a single-socket query. https://man7.org/linux/man-pages/man7/sock_diag.7.html (retrieved 2026-10-27).
- Note: you need one request per family. `sdiag_family` is a single `u8` (`inet_diag.h` `struct inet_diag_req_v2`). Today's two file reads (`port_watch.rs:170-175`) become two dumps.

Confidence: **high** on a 5.4 kernel. It would go higher with one run on the production server's kernel.

### Q2

**Answer:** the dump needs one `AF_NETLINK`/`SOCK_RAW` socket with protocol `NETLINK_SOCK_DIAG` (4). Send it a 72-byte message: a 16-byte `nlmsghdr` (`nlmsg_type = SOCK_DIAG_BY_FAMILY` = 20, `nlmsg_flags = NLM_F_REQUEST|NLM_F_DUMP` = `0x301`) followed by a 56-byte `inet_diag_req_v2` (`sdiag_family`, `sdiag_protocol = IPPROTO_TCP`, `idiag_ext = 0`, `pad`, `idiag_states = 1<<10`, and a zeroed 48-byte `inet_diag_sockid`). Then `recv` until an `NLMSG_DONE` (3) header arrives; an `NLMSG_ERROR` (2) carries a negative errno. Each reply body is an `inet_diag_msg`: 4×u8, then `inet_diag_sockid` (sport/dport `__be16`, src/dst `__be32[4]`, if, cookie[2]), then `expires`, `rqueue`, `wqueue`, `uid`, `inode` (u32 each).

- The kernel structs are in `include/uapi/linux/inet_diag.h` (`inet_diag_sockid`, `inet_diag_req_v2`, `inet_diag_msg`): https://github.com/torvalds/linux/blob/master/include/uapi/linux/inet_diag.h (retrieved 2026-10-27).
- `NETLINK_SOCK_DIAG`, `SOCK_DIAG_BY_FAMILY`, `NLM_F_DUMP` semantics: https://man7.org/linux/man-pages/man7/sock_diag.7.html (retrieved 2026-10-27).
- The layout above is the one that produced the Q1 rows: `/tmp/nlres/diag.py` packs `'=IHHII'` (nlmsghdr) + `'=BBBBI'` + 48 zero bytes, and reads `uid,inode` at body offset 64.

**What the tree exposes:**
- **std: nothing.** `std` has no `AF_NETLINK` socket type. A Rust implementation needs `libc::socket`/`sendto`/`recv` FFI (or a crate).
- **libc is not a direct dependency of the CLI.** `app/src-tauri/cli/Cargo.toml` `[dependencies]` lists clap, serde, serde_json, tokio, whoami, hmac, sha2, chrono and ymux-policy only. The lockfile `ymux` package's deps confirm this (no `libc`). `libc 0.2.186` is in `app/src-tauri/Cargo.lock` only transitively (through tokio, mio and others), so adding `libc = "0.2"` to the CLI adds no new crate to the lock.
- **No existing `libc::` call in the Rust tree.** `grep -rln "libc::" app/src-tauri --include='*.rs'` → no output. No netlink code exists anywhere (the only repo hit for netlink/sock_diag is the deferral at `docs/DECISIONS.md:824`).
- **libc 0.2.186 coverage:** `libc::NETLINK_SOCK_DIAG` (= 4) and `libc::nlmsghdr` exist: https://docs.rs/libc/0.2.186/x86_64-unknown-linux-gnu/libc/constant.NETLINK_SOCK_DIAG.html, https://docs.rs/libc/0.2.186/x86_64-unknown-linux-gnu/libc/struct.nlmsghdr.html (retrieved 2026-10-27). The same URL pattern returns **HTTP 404** for `struct.inet_diag_req_v2.html` and `constant.SOCK_DIAG_BY_FAMILY.html` (retrieved 2026-10-27). So `inet_diag_req_v2`/`inet_diag_msg`/`SOCK_DIAG_BY_FAMILY` would need hand-written `#[repr(C)]` structs and a `const`. This is medium confidence (a 404 is not proof of absence); a `grep` of the libc source would settle it.

Confidence: **high** on the wire layout (it ran). **Medium** on the libc symbol inventory.

### Q3

**Answer:** each tick calls `read_raw()`, which does two `std::fs::read_to_string` calls (`/proc/net/tcp`, `/proc/net/tcp6`) and maps errors to an empty string. The tick compares the pair with the previous one, and only on a change parses LISTEN rows and diffs the sets. **The watcher extracts neither inode nor pid.** `ListenPort` is `{port, family, addr}`, and the downstream consumers read only those plus `workspace_id`.

- `port_watch.rs:170-175`: `read_raw()` reads both files, `unwrap_or_default()`.
- `port_watch.rs:245-275`: the loop runs `read_raw` → `if raw != prev_raw` → `parse_snapshot` → diff → `notify` → `sleep(1s)`.
- `port_watch.rs:89-124`: `parse_proc_net_tcp` uses only `cols[1]` (local `ip:port`) and `cols[3]` (state `0A`). The inode column (`cols[9]`) is never read.
- `port_watch.rs:49-84`: `classify_v4`/`classify_v6` keep loopback/any binds and drop specific-IP binds.
- `port_watch.rs:128-139,141-160`: filters drop port < 1024, port 22, `YMUX_PORTFORWARD_EXCLUDE` and the `YMUX_SOCKET_ADDR` port.
- `port_watch.rs:30-37`: `ListenPort { port: u16, family: &'static str, addr: String }`.
- `port_watch.rs:194-203`: the RPC payload is `workspace_id, addr, port, family`.
- Desktop consumer `app/src-tauri/src/rpc_server.rs:2423-2440` (`"port.opened"`) reads `workspace_id`, `port`, `addr`, `family`. `rpc_server.rs:2499-2507` (`"port.closed"`) reads `workspace_id`, `port`. Neither reads inode or pid.
- Single entry point: `app/src-tauri/cli/src/main.rs:1694` `port_watch::run(workspace).await`.

Confidence: **high**.

### Q4

**Answer: the mechanism carries over, but it is mostly dead today, and it fires more often with netlink.** On this host the `/proc` byte comparison saw a change on **29 of 29** one-second ticks. Most of the table is ESTABLISHED/TIME_WAIT rows whose timer and queue columns move all the time, so the Phase 86.A skip (`port_watch.rs:250`) almost never saves the parse here. The same comparison over the raw LISTEN-only netlink replies changed on **2 of 29** ticks. The listener bodies still carry volatile fields (`idiag_rqueue` = pending connections for a listener, per sock_diag(7); also `expires`/`timer`), so a raw-bytes comparison is not a pure "set changed" signal. It is cheap (~8.5 KB) and stays correct, because the parse-and-diff after it is what decides events.

- `python3 -I /tmp/nlres/stab.py` → `29 1s-ticks: netlink raw bytes changed 2 | proc bytes changed 29`.
- `awk 'NR>1{print $4}' /proc/net/tcp | sort | uniq -c` → `2233 06` (TIME_WAIT), `1044 01` (ESTABLISHED), `71 0A`, `21 03`. LISTEN is about 2% of the rows.
- `port_watch.rs:162-168,247-250`: `RawSnapshot` stores whole bodies, compared with `!=`. Test `identical_raw_snapshot_skips_reparse` at `port_watch.rs:345-354`.
- sock_diag(7) `idiag_rqueue`: "For listening sockets: the number of pending connections." https://man7.org/linux/man-pages/man7/sock_diag.7.html (retrieved 2026-10-27).

Either way the skip stays correct; the 1 s tick and the skip logic are out of scope and need no change. If an implementer wants the skip to stay meaningful, they could compare the parsed `HashSet<ListenPort>` (about 87 entries) instead of raw bytes. That is cheaper than the current `/proc` string compare.

Confidence: **high** for this host. Medium for other hosts, where the share of TIME_WAIT rows differs.

### Q5

**Answer: yes for everything the callers use, and the inode/pid premise does not apply.** Each `inet_diag_msg` carries `idiag_family`, `id.idiag_sport` (big-endian), `id.idiag_src` (the bind address) and `idiag_state`. Those map onto `ListenPort { port, family, addr }` and the loopback/any classification (`port_watch.rs:30-84`). It also carries `idiag_inode` and `idiag_uid`, which the `/proc` parser never extracts. Neither path provides a **pid**: `/proc/net/tcp` has no pid column (header at `port_watch.rs:283-284`), and `inet_diag_msg` has no pid field (`inet_diag.h`). Mapping inode to pid would need a `/proc/<pid>/fd` walk on both paths. So callers stay unchanged: same three fields, same RPC payload (`port_watch.rs:194-203`), same desktop reads (`rpc_server.rs:2423-2440, 2499-2507`).

- Measured: every netlink row had a non-zero inode (`inode0 0` in the `diag.py check` output), and the rows decoded to `(family, state, sport, inet_ntop(src), uid, inode)`.
- One difference to handle: the netlink address is raw network-order bytes, not `/proc`'s little-endian hex. So `classify_v4`/`classify_v6`, which take hex strings (`port_watch.rs:49-84`), would need byte-based twins or a conversion. Their output strings (`"127.0.0.1"`, `"0.0.0.0"`, `"::1"`, `"::"`) must stay identical, or the desktop dedup on `(port, addr, family)` sees new ports.

Confidence: **high**.

### Q6

**Answer: keep `/proc` as the fallback.** `sock_diag` for TCP is not guaranteed. On this host it is a loadable module: `/boot/config-5.4.0-216-generic` → `CONFIG_INET_DIAG=m`, `CONFIG_INET_TCP_DIAG=m`, with `tcp_diag` and `inet_diag` loaded at the time of checking (`/proc/modules`). A kernel that has them off, a module-load restriction, a seccomp profile that blocks `AF_NETLINK`, or a non-Linux-kernel sandbox all make `socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG)` or the dump fail. The watcher then needs a path that still works, and `/proc/net/tcp{,6}` is the one that works today (`port_watch.rs:170-175`). Permissions are **not** the issue: the dump worked unprivileged (uid 1005), and `ss -ltn` relies on this too. A sound shape:

1. Try netlink on the first tick. On a `socket`/`sendto` error or an `NLMSG_ERROR` reply, fall back to `/proc` **for the life of the process** (decide once, and don't retry on every tick).
2. Log only the metadata of the switch (errno), per CLAUDE.md Rule #1/#9 and the existing `hook_log` start/exit lines (`port_watch.rs:220-223, 237, 267`).
3. A dump that returns zero rows is not an error, because a box can have no listeners.

The current `/proc` path also fails silently (`unwrap_or_default()` at `port_watch.rs:172-173` reads a missing file as empty, which means "no listeners"). The fallback design should not make that worse.

Confidence: **medium**. Which real environments lack `sock_diag` (gVisor, locked-down containers) is unverified; see Open questions.

### Q7

**Answer:** on this host the `/proc` pair is 3,323 lines / ~500 KB, and reading it costs **~14–25 ms wall and ~13–15 ms kernel CPU per tick**. The two LISTEN-only netlink dumps return 87 rows / ~8.5 KB in **~0.26–0.32 ms wall** and ≤0.05 ms kernel CPU. That is about **50–90× cheaper**. The 2,589-line figure is a historical reading from Yossi's server. It **cannot be reproduced from here**: the number moves with live connection counts. This host shows the same order of magnitude (3,323).

- `wc -l /proc/net/tcp /proc/net/tcp6` → `3306` + `17` = `3323`. `wc -c` → `495900` + `2977` bytes.
- `python3 -I /tmp/nlres/diag.py bench` (N=200, two runs):
  - run 1: `proc read us/tick 14321 bytes 508327 | netlink listen-dump us/tick 260`, `sys-cpu us/tick proc 13450 netlink 0`
  - run 2: `proc read us/tick 25260 bytes 538177 | netlink listen-dump us/tick 322`, `sys-cpu us/tick proc 15050 netlink 50`
  - `os.times()` resolution is 10 ms over 400 dumps, so the netlink system CPU is below what this harness can resolve.
- At one tick per second, ~13–15 ms of kernel time is ~1.3–1.5% of a core **per watcher** on this host, before the Rust parse. Phase 86.C cut the count to one watcher per host (`docs/vault/cli.md` § `port_watch.rs`, "One per host").
- Source of 2,589: `PROGRESS.txt:6022` ("/proc/net/tcp+tcp6 = 2,589 lines parsed every 500ms by each", Phase 86, 2026-08-23, 18 watchers at ~4.7% CPU each, `PROGRESS.txt:6020-6021`). Repeated in `BACKLOG.md:22` and `docs/vault/cli.md` § `port_watch.rs`. Those per-process CPU figures were measured at a 500 ms tick, with the user-space parse, before Phase 86.A.

Confidence: **high** for the relative cost on this host. **Low** for the production server's absolute numbers until someone measures there.

## Recommendation

Implement the netlink LISTEN dump, with `/proc` kept as a once-decided fallback. The kernel cost of a tick drops from ~13–15 ms of system CPU (a ~500 KB table render) to well under 1 ms (87 rows). The Phase 86.A raw-bytes skip does not recover this, because the `/proc` bytes changed on 29 of 29 ticks here (Q4). The change is local to `port_watch.rs`:

- Replace `read_raw` (`port_watch.rs:170-175`) with two `SOCK_DIAG_BY_FAMILY` dumps.
- Add byte-based twins of `classify_v4`/`classify_v6` (`port_watch.rs:49-84`) that emit the identical address strings.
- Leave `ListenPort`, `should_forward`, `notify` and the desktop consumers (`rpc_server.rs:2423-2507`) untouched (Q3, Q5).

On dependencies, add `libc = "0.2"` to `app/src-tauri/cli/Cargo.toml`. It is already in the lockfile transitively (0.2.186), so there is no new crate. Hand-write the `inet_diag_req_v2` / `inet_diag_msg` `#[repr(C)]` structs and `SOCK_DIAG_BY_FAMILY = 20` (Q2). Don't pull in a netlink crate for one 72-byte request. The unsafe FFI must follow CLAUDE.md Rule #4 (no `unwrap`/`expect`). Unit tests can feed canned reply buffers to the decoder, the same way the current tests feed canned `/proc` text (`port_watch.rs:279-354`).

Trade-off accepted: about 100 lines of `unsafe` FFI and a second code path (the `/proc` fallback) to keep tested, in exchange for a ~50–90× cheaper tick.

What would change this: if Yossi's server now has far fewer connections than this host (so `/proc` is small), or if a target environment turns out to deny `NETLINK_SOCK_DIAG` widely (Q6), the gain may not justify the second path. Running `wc -l /proc/net/tcp*` and the `diag.py bench` command on that server would decide it.

## Open questions

- **Q7 (part):** the absolute per-tick cost on Yossi's production server (the 2,589-line host) was not measured. To answer it, run `wc -l /proc/net/tcp /proc/net/tcp6` and the netlink/`/proc` bench on that server.
- **Q6 (part):** which deployment targets actually lack or deny `NETLINK_SOCK_DIAG` (gVisor sandboxes, seccomp-restricted containers, kernels built without `CONFIG_INET_DIAG`) is unverified; a web search returned nothing gVisor-specific. To answer it, run `ss -ltn` (it fails the same way) on each class of remote ymux supports, or check the gVisor netlink compatibility docs.
- **Q2 (part):** whether libc 0.2.186 truly lacks `inet_diag_req_v2`/`inet_diag_msg`/`SOCK_DIAG_BY_FAMILY` rests on docs.rs 404s, not the source. To answer it, run `grep -rn "inet_diag\|SOCK_DIAG_BY_FAMILY"` over the libc 0.2.186 crate source.

## Sources

- `app/src-tauri/cli/src/port_watch.rs:30-37` — `ListenPort {port, family, addr}`, no inode/pid
- `app/src-tauri/cli/src/port_watch.rs:49-84` — hex-based v4/v6 bind classification
- `app/src-tauri/cli/src/port_watch.rs:89-124` — `/proc` parser uses cols[1] + cols[3] only
- `app/src-tauri/cli/src/port_watch.rs:128-160` — port filters, exclude env
- `app/src-tauri/cli/src/port_watch.rs:162-175` — `RawSnapshot`, `read_raw` with `unwrap_or_default`
- `app/src-tauri/cli/src/port_watch.rs:194-211` — RPC payload `workspace_id, addr, port, family`
- `app/src-tauri/cli/src/port_watch.rs:245-275` — tick loop, Phase 86.A raw compare at :250
- `app/src-tauri/cli/src/port_watch.rs:345-354` — skip test
- `app/src-tauri/cli/src/main.rs:1694` — sole entry point
- `app/src-tauri/src/rpc_server.rs:2423-2440, 2499-2507` — desktop reads workspace_id/port/addr/family only
- `app/src-tauri/cli/Cargo.toml` `[dependencies]` — no libc
- `app/src-tauri/Cargo.lock` — `ymux` package deps have no libc; `libc 0.2.186` present transitively
- `docs/DECISIONS.md:824` — netlink sock_diag deferred in Phase 86
- `docs/vault/cli.md` § `port_watch.rs` — vault description, 2,589-line origin, one-per-host
- `PROGRESS.txt:6020-6022` — 2,589 lines, 500 ms tick, ~4.7% CPU per watcher (2026-08-23)
- `BACKLOG.md:22` — request text
- `python3 -I /tmp/nlres/diag.py check` → v4 71 rows / v6 16 rows, states `[10]` only, inodes non-zero; all-states v4 3307 rows
- `awk 'NR>1 && $4=="0A"' /proc/net/tcp /proc/net/tcp6 | wc -l` → 87; `ss -ltnH | wc -l` → 87
- `python3 -I /tmp/nlres/diag.py bench` (×2) → /proc 14.3–25.3 ms wall, 13.5–15.1 ms sys per tick; netlink 0.26–0.32 ms wall, ≤0.05 ms sys
- `python3 -I /tmp/nlres/stab.py` → raw bytes changed over 29 ticks: netlink 2, /proc 29
- `wc -l` / `wc -c /proc/net/tcp /proc/net/tcp6` → 3306+17 lines, 495900+2977 bytes
- `awk … | uniq -c` on /proc/net/tcp states → 2233 TIME_WAIT, 1044 ESTABLISHED, 71 LISTEN, 21 SYN_RECV
- `grep CONFIG_INET_DIAG /boot/config-5.4.0-216-generic` → `=m` (both); `/proc/modules` → tcp_diag, inet_diag loaded
- `grep -rln "libc::" app/src-tauri --include='*.rs'` → none
- https://man7.org/linux/man-pages/man7/sock_diag.7.html — retrieved 2026-10-27 — request/reply structs, `idiag_states` dump filter, `idiag_rqueue` for listeners, NETLINK_SOCK_DIAG since 3.3
- https://github.com/torvalds/linux/blob/master/include/uapi/linux/inet_diag.h — retrieved 2026-10-27 — exact `inet_diag_sockid` / `inet_diag_req_v2` / `inet_diag_msg` field types
- https://docs.rs/libc/0.2.186/x86_64-unknown-linux-gnu/libc/constant.NETLINK_SOCK_DIAG.html — retrieved 2026-10-27 — `NETLINK_SOCK_DIAG = 4` present
- https://docs.rs/libc/0.2.186/x86_64-unknown-linux-gnu/libc/struct.nlmsghdr.html — retrieved 2026-10-27 — `nlmsghdr` present
- https://docs.rs/libc/0.2.186/x86_64-unknown-linux-gnu/libc/struct.inet_diag_req_v2.html and `.../constant.SOCK_DIAG_BY_FAMILY.html` — retrieved 2026-10-27 — HTTP 404
