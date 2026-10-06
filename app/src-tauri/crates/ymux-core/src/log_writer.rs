//! The single owner of `%APPDATA%\ymux\debug.log`.
//!
//! # What went wrong
//!
//! Every log line used to do its own `stat` + `open` + write + `close`, from
//! whatever thread happened to call it, with no lock anywhere. Two defects
//! fell out of that:
//!
//!   * **Torn lines.** `writeln!` on an unbuffered `File` goes through
//!     `core::fmt::write`, which issues the payload and the trailing `"\n"`
//!     as TWO separate `write()` syscalls. `O_APPEND` makes each one atomic
//!     but not the pair, so a concurrent writer could land between them:
//!     ~120 lines in one day of Yossi's log are two records fused with no
//!     newline, each followed by a stray blank line. That is not cosmetic —
//!     it means the file cannot be counted or parsed, which is exactly what
//!     you need it for when diagnosing something.
//!   * **Rotation races.** `stat` then `rename` is a TOCTOU: two threads
//!     could both decide to rotate, and the second `rename` clobbered the
//!     first's `debug.log.1`, losing up to 5 MB. Rust opens files on Windows
//!     with `FILE_SHARE_DELETE`, so a rename also succeeds while another
//!     thread holds the file open — and that thread then keeps writing into
//!     the rotated file.
//!   * **Two processes.** The same race across two `ymux.exe` instances
//!     sharing one config dir: both stat, both rename, and the second
//!     clobbers the first's `debug.log.1`; the loser's open handle also keeps
//!     appending into the rotated file.
//!
//! The worst amplifier was `log_sync`, which pulls up to 256 KiB × 3 files ×
//! N hosts per cycle and used to submit them one line at a time.
//!
//! # The design
//!
//! One dedicated OS thread owns the file handle. Callers `try_send` a
//! pre-encoded, newline-terminated block onto a bounded channel and walk
//! away; the writer drains, coalesces a batch into one buffer, and issues a
//! single `write_all`. Rotation, prune and clear all run on that thread, so
//! they are ordered against the lines around them and cannot race.
//!
//! **Across processes.** Within a process the thread orders everything; between
//! processes `rotate` takes an exclusive `File::lock` on `debug.log.lock`
//! around stat + rename only, and re-stats `debug.log` under it, so the second
//! process to arrive finds a fresh file and does not rename. Each batch
//! compares the handle's length with the path's and reopens on a mismatch,
//! which is how a process notices another one rotated under it. Two stats per
//! batch, none per line. If the lock cannot be taken it rotates unlocked.
//!
//! **Why a thread and not a `Mutex<File>`.** A mutex plus one `write_all`
//! would fix the tearing, so it is worth being honest that this goes
//! further. It buys four things a mutex cannot: I/O happens outside any lock
//! (`docs/CONTRIBUTING.md` requires that, and a mutex would block PTY reader
//! threads on disk latency); `log_sync`'s burst coalesces into a handful of
//! syscalls; `config_dir()` leaves the caller's thread, which removes a
//! latent re-entrancy hazard (it logs from inside its own `Once`); and with
//! no mutex there is no poisoning to handle, so Rule #4 holds by
//! construction rather than by careful `match` arms.
//!
//! **Why `std::sync::mpsc` and not tokio.** `write_line` is called from raw
//! `std::thread::spawn` PTY readers, from the panic hook, from
//! `ymux-bootstrap`, and from `cargo test` — none of which have a runtime. A
//! tokio sender works off-runtime but the draining task would not.
//!
//! **Bounded, never blocking.** A flood bug once produced 15,477 lines in
//! minutes; an unbounded queue behind a stalled disk is unbounded memory.
//! Blocking is not an option either — a blocked PTY reader stalls the user's
//! terminal, and blocking in the panic hook would hang the crash path. So a
//! full queue drops, and says so in one line rather than silently.

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::OnceLock;

use crate::{config_dir, DEBUG_LOG_MAX_BYTES};

/// Lines in flight before `submit` starts dropping. ~1.6 MB at 200 B/line.
const QUEUE_CAP: usize = 8192;
/// Ceiling on how many messages one drain absorbs before writing.
const BATCH_MAX_MSGS: usize = 4096;
/// Ceiling on one `write_all`.
const BATCH_MAX_BYTES: usize = 1 << 20;
/// How long `flush_log` / `clear_debug_log` wait for the writer to answer.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Lines dropped because the queue was full, not yet reported.
static DROPPED: AtomicU64 = AtomicU64::new(0);

static TX: OnceLock<Option<SyncSender<Msg>>> = OnceLock::new();

pub(crate) enum Msg {
    /// Pre-encoded and newline-terminated. One line or many.
    Block(String),
    /// Drain everything queued ahead of this, then acknowledge.
    Flush(SyncSender<()>),
    /// Truncate `debug.log`, remove `debug.log.1`, then acknowledge.
    Clear(SyncSender<Result<(), String>>),
    /// Retention sweep. Fire and forget.
    Prune(u32),
}

/// Hand a message to the writer. Returns false if it was dropped.
pub(crate) fn submit(msg: Msg) -> bool {
    let tx = match TX.get_or_init(spawn_writer) {
        Some(tx) => tx,
        None => return direct_fallback(msg),
    };
    match tx.try_send(msg) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            false
        }
        // The writer thread died. Degrade to a synchronous write rather than
        // losing logging entirely — still one `write_all`, so still untorn.
        Err(TrySendError::Disconnected(m)) => direct_fallback(m),
    }
}

/// Block until everything submitted before this call is on disk.
///
/// Bounded, so a wedged writer can never hang app exit. There is no buffering
/// across drains, so this only has to drain the channel — no `sync_data`,
/// which would mean an fsync per batch; the page cache survives process
/// death, and only a machine crash loses it.
pub fn flush_log() {
    let (ack_tx, ack_rx) = sync_channel(1);
    if submit(Msg::Flush(ack_tx)) {
        let _ = ack_rx.recv_timeout(ACK_TIMEOUT);
    }
}

pub(crate) fn clear_now() -> Result<(), String> {
    let (ack_tx, ack_rx) = sync_channel(1);
    if !submit(Msg::Clear(ack_tx)) {
        return Err("log writer unavailable".into());
    }
    match ack_rx.recv_timeout(ACK_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err("log writer did not acknowledge the clear".into()),
    }
}

fn spawn_writer() -> Option<SyncSender<Msg>> {
    let (tx, rx) = sync_channel(QUEUE_CAP);
    // Does no logging and touches no config dir, so it cannot re-enter the
    // `OnceLock` initializer that is calling it.
    std::thread::Builder::new()
        .name("ymux-log-writer".into())
        .spawn(move || LogWriter::new().run(rx))
        .ok()
        .map(|_| tx)
}

/// Last resort when there is no writer thread: open, one `write_all`, close.
fn direct_fallback(msg: Msg) -> bool {
    let text = match msg {
        Msg::Block(b) => b,
        // Nothing is queued if there is no queue, so an ack is immediate and
        // a prune has nothing to order against.
        Msg::Flush(ack) => {
            let _ = ack.send(());
            return true;
        }
        Msg::Clear(ack) => {
            let _ = ack.send(clear_files());
            return true;
        }
        Msg::Prune(days) => {
            prune_files(days);
            return true;
        }
    };
    let dir = match config_dir() {
        Ok(d) => d,
        Err(_) => return false,
    };
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("debug.log"))
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .is_ok()
}

// ─── the owner ───────────────────────────────────────────────────────────

struct LogWriter {
    dir: Option<std::path::PathBuf>,
    file: Option<std::fs::File>,
    /// Bytes in the open `debug.log`, tracked incrementally so there is no
    /// `stat` per line.
    size: u64,
    max_bytes: u64,
    /// Reused across drains — one allocation for the process lifetime.
    scratch: String,
}

impl LogWriter {
    fn new() -> Self {
        LogWriter {
            dir: None,
            file: None,
            size: 0,
            max_bytes: DEBUG_LOG_MAX_BYTES,
            scratch: String::new(),
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        while let Ok(first) = rx.recv() {
            self.scratch.clear();
            let mut acks: Vec<SyncSender<()>> = Vec::new();
            self.absorb(first, &mut acks);
            for m in rx.try_iter().take(BATCH_MAX_MSGS) {
                self.absorb(m, &mut acks);
                if self.scratch.len() >= BATCH_MAX_BYTES {
                    break;
                }
            }
            self.flush_scratch();
            // Only after the bytes are out, or a flush would return early.
            for a in acks {
                let _ = a.send(());
            }
        }
    }

    /// Fold one message into the pending batch, acting on it immediately if
    /// it is not a plain block.
    fn absorb(&mut self, msg: Msg, acks: &mut Vec<SyncSender<()>>) {
        match msg {
            Msg::Block(b) => self.scratch.push_str(&b),
            Msg::Flush(ack) => acks.push(ack),
            Msg::Clear(ack) => {
                // Order matters: everything queued BEFORE the clear has to
                // land before it, or a line from another thread would
                // resurrect content the user just cleared.
                self.flush_scratch();
                self.file = None;
                self.size = 0;
                let _ = ack.send(clear_files());
            }
            Msg::Prune(days) => {
                self.flush_scratch();
                self.file = None;
                prune_files(days);
                self.size = 0;
            }
        }
    }

    fn flush_scratch(&mut self) {
        if self.scratch.is_empty() {
            return;
        }
        // Report anything the queue had to drop, once, ahead of the batch.
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        let payload = if dropped > 0 {
            let ts = crate::log_timestamp();
            format!(
                "[{ts}] [WARN ] [LOG] {dropped} lines dropped — writer queue full\n{}",
                self.scratch
            )
        } else {
            std::mem::take(&mut self.scratch)
        };
        self.write_batch(payload.as_bytes());
        self.scratch.clear();
    }

    /// Resolve the config dir once per batch, not per line. Cheap, keeps a
    /// `YMUX_CONFIG_DIR` change observable, and copes with the
    /// winmux → ymux migration landing mid-run.
    fn resolve_dir(&mut self) -> bool {
        let dir = match config_dir() {
            Ok(d) => d,
            Err(_) => return false,
        };
        if self.dir.as_deref() != Some(dir.as_path()) {
            self.file = None;
            self.dir = Some(dir);
            self.size = 0;
        }
        true
    }

    fn write_batch(&mut self, bytes: &[u8]) {
        if self.resolve_dir() {
            self.write_to_current(bytes);
        }
    }

    /// Append one batch to `<dir>/debug.log`, first making sure the handle
    /// still points at that path. Two stats per batch, never per line.
    ///
    /// A second process can rotate under us; our handle then names
    /// `debug.log.1` and would keep appending there. The handle's length
    /// differs from the path's (or the path is gone) exactly in that case.
    fn write_to_current(&mut self, bytes: &[u8]) {
        let path = match &self.dir {
            Some(d) => d.join("debug.log"),
            None => return,
        };
        if let Some(f) = &self.file {
            let held = f.metadata().map(|m| m.len()).ok();
            let on_disk = std::fs::metadata(&path).map(|m| m.len()).ok();
            if held.is_none() || held != on_disk {
                self.file = None;
            }
        }
        if self.file.is_none() {
            match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => {
                    self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
                    self.file = Some(f);
                }
                Err(_) => return,
            }
        }
        let ok = match self.file.as_mut() {
            Some(f) => f.write_all(bytes).is_ok(),
            None => false,
        };
        if !ok {
            // Drop the handle so the next batch reopens; a transient failure
            // (AV lock, a full disk that frees up) then recovers by itself.
            self.file = None;
            return;
        }
        // The real length, not `+= len`: another process may have appended.
        self.size = match self.file.as_ref().map(|f| f.metadata()) {
            Some(Ok(m)) => m.len(),
            _ => self.size + bytes.len() as u64,
        };
        if self.size > self.max_bytes {
            self.rotate();
        }
    }

    fn rotate(&mut self) {
        let dir = match &self.dir {
            Some(d) => d.clone(),
            None => return,
        };
        // CLOSE FIRST. Rust opens files on Windows with FILE_SHARE_DELETE, so
        // the rename below succeeds even with our handle live — and we would
        // then keep appending into debug.log.1. This line is the fix, not
        // tidiness.
        self.file = None;
        // Held only across the stat + rename. Another process that also saw
        // the cap exceeded waits here, then re-stats and finds a fresh file.
        // AI-NOTE: fail-open — if the lock file cannot be opened or locked,
        // rotate unlocked (the pre-lock behaviour) rather than let the log
        // grow without bound; the writer thread must not log or panic here.
        let _lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("debug.log.lock"))
            .ok()
            .and_then(|f| f.lock().ok().map(|_| f));
        let primary = dir.join("debug.log");
        let real = std::fs::metadata(&primary).map(|m| m.len()).unwrap_or(0);
        if real > self.max_bytes
            && std::fs::rename(&primary, dir.join("debug.log.1")).is_err()
        {
            // Something else is holding debug.log.1 (an AV scanner, a second
            // instance). Truncate in place rather than grow without bound.
            // One attempt, no retry loop.
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(&primary);
        }
        self.size = 0;
    }
}

// ─── file operations shared with the fallback path ───────────────────────

fn clear_files() -> Result<(), String> {
    let dir = config_dir()?;
    std::fs::write(dir.join("debug.log"), b"").map_err(|e| format!("clear debug.log: {e}"))?;
    let _ = std::fs::remove_file(dir.join("debug.log.1"));
    Ok(())
}

/// Body of `prune_logs`, unchanged except that it runs on the writer thread
/// so it can no longer race a write.
fn prune_files(retention_days: u32) {
    if retention_days == 0 {
        return;
    }
    let dir = match config_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let cutoff = match std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(retention_days) * 86_400))
    {
        Some(c) => c,
        None => return,
    };
    let stale = |p: &std::path::Path| -> bool {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|mt| mt < cutoff)
            .unwrap_or(false)
    };
    // Rotated file older than the window → delete outright.
    let rotated = dir.join("debug.log.1");
    if rotated.exists() && stale(&rotated) {
        let _ = std::fs::remove_file(&rotated);
    }
    // Primary log untouched for > retention (a stale session) → truncate fresh.
    let primary = dir.join("debug.log");
    if primary.exists() && stale(&primary) {
        let _ = std::fs::write(&primary, b"");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// A writer bound to its own directory, so these run in parallel without
    /// touching the process-global config dir or the real debug.log.
    impl LogWriter {
        fn for_dir(dir: &std::path::Path, max_bytes: u64) -> Self {
            LogWriter {
                dir: Some(dir.to_path_buf()),
                file: None,
                size: 0,
                max_bytes,
                scratch: String::new(),
            }
        }

        /// `write_batch` re-resolves the process config dir; tests pin the
        /// directory instead and call the same per-batch path.
        fn write_pinned(&mut self, bytes: &[u8]) {
            self.write_to_current(bytes);
        }
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ymux-logwriter-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    fn read(p: &std::path::Path) -> String {
        let mut s = String::new();
        if let Ok(mut f) = std::fs::File::open(p) {
            let _ = f.read_to_string(&mut s);
        }
        s
    }

    #[test]
    fn a_batch_of_blocks_becomes_one_newline_terminated_buffer() {
        let mut w = LogWriter::new();
        let mut acks = Vec::new();
        w.absorb(Msg::Block("a\n".into()), &mut acks);
        w.absorb(Msg::Block("b\n".into()), &mut acks);
        w.absorb(Msg::Block("c\n".into()), &mut acks);
        // The whole point: one buffer, one write_all. The old code issued the
        // payload and the "\n" as two syscalls per line, which is how two
        // records ended up fused with a stray blank line after them.
        assert_eq!(w.scratch, "a\nb\nc\n");
    }

    #[test]
    fn rotation_moves_the_old_log_aside_without_losing_a_line() {
        let dir = tmpdir("rotate-keeps-all");
        // 200 lines of exactly 10 bytes = 2000; a 1500-byte cap rotates ONCE,
        // at line 151. Sized deliberately: only one rotated generation is
        // kept, so a cap that rotated repeatedly would drop earlier lines by
        // design and this test would be asserting the wrong thing.
        let mut w = LogWriter::for_dir(&dir, 1500);
        for i in 0..200 {
            w.write_pinned(format!("line-{i:04}\n").as_bytes());
        }
        drop(w);
        assert!(
            dir.join("debug.log.1").exists(),
            "the cap must have triggered exactly one rotation"
        );
        let combined = read(&dir.join("debug.log.1")) + &read(&dir.join("debug.log"));
        for i in 0..200 {
            let want = format!("line-{i:04}");
            assert_eq!(
                combined.lines().filter(|l| *l == want).count(),
                1,
                "{want} must appear exactly once across both files"
            );
        }
    }

    #[test]
    fn a_second_rotation_replaces_the_previous_rotated_file() {
        let dir = tmpdir("rotate-twice");
        let mut w = LogWriter::for_dir(&dir, 32);
        w.write_pinned(b"generation-one-padding-padding-padding\n");
        w.write_pinned(b"generation-two-padding-padding-padding\n");
        w.write_pinned(b"generation-three\n");
        drop(w);
        // Two threads both deciding to rotate used to clobber each other's
        // .1; single-threaded, the second generation simply replaces it.
        assert!(read(&dir.join("debug.log.1")).contains("generation-two"));
        assert!(read(&dir.join("debug.log")).contains("generation-three"));
    }

    #[test]
    fn rotation_closes_the_handle_before_renaming() {
        let dir = tmpdir("rotate-closes-handle");
        let mut w = LogWriter::for_dir(&dir, 16);
        w.write_pinned(b"before-rotation-padding-padding\n");
        w.write_pinned(b"after-rotation\n");
        drop(w);
        let after = read(&dir.join("debug.log"));
        let rotated = read(&dir.join("debug.log.1"));
        // Windows renames succeed with the handle open (FILE_SHARE_DELETE),
        // and the writer would then keep appending into the rotated file.
        assert!(
            after.contains("after-rotation"),
            "post-rotation writes belong in debug.log, got: {after:?}"
        );
        assert!(
            !rotated.contains("after-rotation"),
            "nothing written after the rename may land in debug.log.1"
        );
    }

    #[test]
    fn a_full_queue_is_reported_as_one_counted_line_and_then_forgotten() {
        let dir = tmpdir("drop-marker");
        let mut w = LogWriter::for_dir(&dir, u64::MAX);
        DROPPED.store(7, Ordering::Relaxed);
        w.scratch.push_str("real-line\n");
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        let payload = format!(
            "[ts] [WARN ] [LOG] {dropped} lines dropped — writer queue full\n{}",
            w.scratch
        );
        w.write_pinned(payload.as_bytes());
        drop(w);
        let out = read(&dir.join("debug.log"));
        assert!(out.contains("7 lines dropped"), "got: {out}");
        assert!(out.contains("real-line"));
        // Swapped, not read: the next batch must not re-report the same loss.
        assert_eq!(DROPPED.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_changed_directory_reopens_against_the_new_path() {
        let first = tmpdir("dir-change-a");
        let second = tmpdir("dir-change-b");
        let mut w = LogWriter::for_dir(&first, u64::MAX);
        w.write_pinned(b"in-first\n");
        w.dir = Some(second.clone());
        w.file = None;
        w.size = 0;
        w.write_pinned(b"in-second\n");
        drop(w);
        assert!(read(&first.join("debug.log")).contains("in-first"));
        let b = read(&second.join("debug.log"));
        assert!(b.contains("in-second"));
        assert!(!b.contains("in-first"), "the old file must not follow us");
    }

    #[test]
    fn two_writers_rotating_never_clobber_the_rotated_file() {
        let dir = tmpdir("two-writers-clobber");
        let mut a = LogWriter::for_dir(&dir, 64);
        let mut b = LogWriter::for_dir(&dir, 64);
        // Pins the cross-process case: B's stale handle must not keep writing
        // into the file A rotated aside, nor rename A's fresh log over it.
        b.write_pinned(format!("B1-{}\n", "b".repeat(36)).as_bytes());
        a.write_pinned(format!("A1-{}\n", "a".repeat(66)).as_bytes());
        a.write_pinned(b"A2\n");
        b.write_pinned(format!("B2-{}\n", "b".repeat(26)).as_bytes());
        drop((a, b));
        let rotated = read(&dir.join("debug.log.1"));
        let current = read(&dir.join("debug.log"));
        assert!(rotated.contains("A1-") && rotated.contains("B1-"), "got: {rotated:?}");
        assert!(current.contains("A2") && current.contains("B2-"), "got: {current:?}");
        let all = rotated + &current;
        for tag in ["B1-", "A1-", "A2", "B2-"] {
            assert_eq!(all.lines().filter(|l| l.starts_with(tag)).count(), 1, "{tag}");
        }
    }

    #[test]
    fn two_writers_interleave_and_every_line_survives_rotation() {
        let dir = tmpdir("two-writers-interleave");
        // 200 lines x 7 bytes = 1400; a 1000-byte cap rotates the shared file
        // exactly once (only one rotated generation is kept).
        let mut a = LogWriter::for_dir(&dir, 1000);
        let mut b = LogWriter::for_dir(&dir, 1000);
        for i in 0..100 {
            a.write_pinned(format!("a-{i:04}\n").as_bytes());
            b.write_pinned(format!("b-{i:04}\n").as_bytes());
        }
        drop((a, b));
        assert!(dir.join("debug.log.1").exists(), "the cap must rotate once");
        let all = read(&dir.join("debug.log.1")) + &read(&dir.join("debug.log"));
        for i in 0..100 {
            for p in ["a", "b"] {
                let want = format!("{p}-{i:04}");
                assert_eq!(all.lines().filter(|l| *l == want).count(), 1, "{want}");
            }
        }
        // No torn or fused line.
        assert!(all.lines().all(|l| l.len() == 6 && (l.starts_with("a-") || l.starts_with("b-"))));
    }
}
