//! One-shot SSH exec over an existing handle: the only copy of the drain loop.
//!
//! The copies this replaces stopped at the first of EOF / ExitStatus with the
//! exit code initialised to 0. OpenSSH normally sends EOF first, so a failed
//! remote command read as success. Here an exit is only ever what the server
//! said: `ExitState::Unknown` is never reported as 0.
//!
//! Takes a finished command string because SSH exec is a shell string by
//! protocol (Rule #3): callers quote every value with `shell_quote`.
//!
//! Logs the exit and byte counts only. Output can be PTY content
//! (`capture-pane` goes through here) and a command can carry a token, so
//! neither is ever logged (Rules #1, #8).

use std::time::{Duration, Instant};

use russh::client::{Handle, Msg};
use russh::{Channel, ChannelMsg, Sig};

use crate::{log_debug, log_warn, SshClient};

/// Bound on opening the channel and sending the request and stdin. A live
/// connection does this in a few round trips, so this only fires on a dead one.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
/// After the exit is known, the wait for EOF ends when no data arrives for
/// this long. A daemon started by the command can hold stdout open for ever,
/// and it must not burn the caller's timeout.
const GRACE_IDLE: Duration = Duration::from_millis(500);
/// Hard limit on the wait after the exit is known. sshd can send the exit
/// before it has flushed the output, so data that still flows extends the
/// idle window, up to here.
const GRACE_MAX: Duration = Duration::from_secs(5);
/// Default output cap for each stream. Far above any real caller: it only
/// stops a runaway remote command from exhausting desktop memory.
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;

/// What to do with the remote stderr.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StderrMode {
    /// Append to `stdout` in arrival order.
    #[default]
    Merge,
    /// Discard.
    Drop,
    /// Collect in `stderr`.
    Separate,
}

pub struct ExecOpts<'a> {
    /// Limit on the wait for the command, counted after the request is sent.
    /// `None` waits for as long as the connection lives (zip, apt-get).
    pub timeout: Option<Duration>,
    /// Sent to the remote stdin, then EOF. For small payloads only (secrets).
    /// When `None`, no EOF is sent.
    pub stdin: Option<&'a [u8]>,
    pub stderr: StderrMode,
    /// Cap for each stream. Going over it is an error, never a silent cut.
    pub max_bytes: usize,
}

impl Default for ExecOpts<'_> {
    fn default() -> Self {
        Self {
            timeout: None,
            stdin: None,
            stderr: StderrMode::default(),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl ExecOpts<'_> {
    pub fn timeout_secs(secs: u64) -> Self {
        Self {
            timeout: Some(Duration::from_secs(secs)),
            ..Self::default()
        }
    }
}

/// How the remote command ended, as far as the server told us.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitState {
    Code(i32),
    /// Killed by this signal. There is no exit code.
    Signal(String),
    /// The channel ended, or the timeout hit, before any exit message.
    Unknown,
}

impl std::fmt::Display for ExitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitState::Code(c) => write!(f, "code {c}"),
            ExitState::Signal(s) => write!(f, "signal {s}"),
            ExitState::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutcome {
    pub exit: ExitState,
    pub stdout: String,
    pub stderr: String,
    /// The timeout hit before any exit message. Output is what arrived so far.
    pub timed_out: bool,
}

impl ExecOutcome {
    /// The numeric exit code, for callers with no use for anything less
    /// certain. A signal, a timeout or a missing status is an error here,
    /// never a code the caller could mistake for a result.
    pub fn require_code(&self) -> Result<i32, String> {
        match &self.exit {
            ExitState::Code(c) => Ok(*c),
            ExitState::Signal(s) => Err(format!("remote command killed by signal {s}")),
            ExitState::Unknown if self.timed_out => Err("remote command timed out".to_string()),
            ExitState::Unknown => Err("remote command ended without an exit status".to_string()),
        }
    }
}

fn signal_name(sig: &Sig) -> String {
    match sig {
        Sig::Custom(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// The drain state machine. No I/O and no clock of its own, so every ordering
/// of the channel messages is testable.
struct Drain {
    stderr_mode: StderrMode,
    max_bytes: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: ExitState,
    exit_seen_at: Option<Instant>,
    last_data_at: Option<Instant>,
    got_eof: bool,
    over_cap: bool,
}

impl Drain {
    fn new(stderr_mode: StderrMode, max_bytes: usize) -> Self {
        Self {
            stderr_mode,
            max_bytes,
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit: ExitState::Unknown,
            exit_seen_at: None,
            last_data_at: None,
            got_eof: false,
            over_cap: false,
        }
    }

    /// Take one message. Returns true when the drain is finished.
    fn feed(&mut self, msg: Option<ChannelMsg>, now: Instant) -> bool {
        match msg {
            // A refused exec request gets no further message from the server.
            None | Some(ChannelMsg::Close) | Some(ChannelMsg::Failure) => true,
            Some(ChannelMsg::Eof) => {
                self.got_eof = true;
                self.exit_seen_at.is_some()
            }
            Some(ChannelMsg::Data { ref data }) => self.collect(false, data, now),
            Some(ChannelMsg::ExtendedData { ref data, .. }) => self.collect(true, data, now),
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                self.set_exit(ExitState::Code(exit_status as i32), now)
            }
            Some(ChannelMsg::ExitSignal { signal_name: ref sig, .. }) => {
                self.set_exit(ExitState::Signal(signal_name(sig)), now)
            }
            Some(_) => false,
        }
    }

    fn set_exit(&mut self, exit: ExitState, now: Instant) -> bool {
        self.exit = exit;
        self.exit_seen_at.get_or_insert(now);
        self.got_eof
    }

    fn collect(&mut self, is_stderr: bool, data: &[u8], now: Instant) -> bool {
        self.last_data_at = Some(now);
        let buf = match (is_stderr, self.stderr_mode) {
            (true, StderrMode::Drop) => return false,
            (true, StderrMode::Separate) => &mut self.stderr,
            (true, StderrMode::Merge) | (false, _) => &mut self.stdout,
        };
        if buf.len() + data.len() > self.max_bytes {
            self.over_cap = true;
            return true;
        }
        buf.extend_from_slice(data);
        false
    }

    /// When the grace after the exit ends. `None` until the exit is known.
    fn grace_end(&self) -> Option<Instant> {
        let exit_at = self.exit_seen_at?;
        let last = self.last_data_at.map_or(exit_at, |d| d.max(exit_at));
        Some((last + GRACE_IDLE).min(exit_at + GRACE_MAX))
    }

    /// When the wait must stop, given the caller's deadline.
    fn wake_at(&self, deadline: Option<Instant>) -> Option<Instant> {
        match (self.grace_end(), deadline) {
            (Some(g), Some(d)) => Some(g.min(d)),
            (g, d) => g.or(d),
        }
    }

    /// The outcome. `woke` is true when the wait ended at `wake_at`, not on a
    /// channel message. That is a timeout only if the exit is still unknown.
    fn finish(self, woke: bool) -> ExecOutcome {
        ExecOutcome {
            timed_out: woke && self.exit_seen_at.is_none(),
            exit: self.exit,
            // Decoded once, here: a chunk can end inside a UTF-8 sequence.
            stdout: String::from_utf8_lossy(&self.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
        }
    }
}

async fn start(
    handle: &Handle<SshClient>,
    cmd: &str,
    stdin: Option<&[u8]>,
) -> Result<Channel<Msg>, String> {
    let ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    let sent = async {
        ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
        if let Some(bytes) = stdin {
            ch.data(bytes).await.map_err(|e| format!("stdin: {e}"))?;
            ch.eof().await.map_err(|e| format!("stdin eof: {e}"))?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = sent {
        let _ = ch.close().await;
        return Err(e);
    }
    Ok(ch)
}

/// Run `cmd` on a fresh exec channel and wait for it to end.
///
/// `Err` means the channel could not be set up or the output went over the
/// cap. Everything the command itself did, a failure or a timeout included,
/// is in the `Ok` outcome.
pub async fn exec(
    handle: &Handle<SshClient>,
    cmd: &str,
    opts: ExecOpts<'_>,
) -> Result<ExecOutcome, String> {
    let mut ch = tokio::time::timeout(SETUP_TIMEOUT, start(handle, cmd, opts.stdin))
        .await
        .map_err(|_| "exec: timed out starting the remote command".to_string())??;

    let deadline = opts.timeout.map(|t| Instant::now() + t);
    let mut drain = Drain::new(opts.stderr, opts.max_bytes);
    let mut woke = false;
    loop {
        let msg = match drain.wake_at(deadline) {
            Some(at) => match tokio::time::timeout_at(at.into(), ch.wait()).await {
                Ok(m) => m,
                Err(_) => {
                    woke = true;
                    break;
                }
            },
            None => ch.wait().await,
        };
        if drain.feed(msg, Instant::now()) {
            break;
        }
    }
    let _ = ch.close().await;

    if drain.over_cap {
        log_warn("SSH", &format!("exec: output over the {} byte cap", drain.max_bytes));
        return Err(format!("remote output over the {} byte cap", drain.max_bytes));
    }
    let outcome = drain.finish(woke);
    let summary = format!(
        "exec: exit={} stdout={}B stderr={}B timed_out={}",
        outcome.exit,
        outcome.stdout.len(),
        outcome.stderr.len(),
        outcome.timed_out
    );
    if outcome.timed_out {
        log_warn("SSH", &summary);
    } else {
        log_debug("SSH", &summary);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::CryptoVec;

    fn data(s: &str) -> Option<ChannelMsg> {
        Some(ChannelMsg::Data { data: CryptoVec::from_slice(s.as_bytes()) })
    }
    fn ext(s: &str) -> Option<ChannelMsg> {
        Some(ChannelMsg::ExtendedData { data: CryptoVec::from_slice(s.as_bytes()), ext: 1 })
    }
    fn status(c: u32) -> Option<ChannelMsg> {
        Some(ChannelMsg::ExitStatus { exit_status: c })
    }
    fn eof() -> Option<ChannelMsg> {
        Some(ChannelMsg::Eof)
    }
    fn close() -> Option<ChannelMsg> {
        Some(ChannelMsg::Close)
    }
    fn signal(sig: Sig) -> Option<ChannelMsg> {
        Some(ChannelMsg::ExitSignal {
            signal_name: sig,
            core_dumped: false,
            error_message: String::new(),
            lang_tag: String::new(),
        })
    }

    /// Feed messages at one instant. Returns the drain and whether it finished.
    fn run(mode: StderrMode, msgs: Vec<Option<ChannelMsg>>) -> (Drain, bool) {
        let mut d = Drain::new(mode, DEFAULT_MAX_BYTES);
        let now = Instant::now();
        let mut done = false;
        for m in msgs {
            done = d.feed(m, now);
            if done {
                break;
            }
        }
        (d, done)
    }

    #[test]
    fn eof_before_status_keeps_the_failure() {
        // The P1: OpenSSH's usual order, which every old copy read as 0.
        let (d, done) = run(StderrMode::Merge, vec![data("x"), eof(), status(3)]);
        assert!(done);
        let o = d.finish(false);
        assert_eq!(o.exit, ExitState::Code(3));
        assert_eq!(o.require_code(), Ok(3));
        assert_eq!(o.stdout, "x");
    }

    #[test]
    fn status_before_eof_keeps_later_output() {
        let (d, done) = run(StderrMode::Merge, vec![status(0), data("tail"), eof()]);
        assert!(done);
        let o = d.finish(false);
        assert_eq!(o.exit, ExitState::Code(0));
        assert_eq!(o.stdout, "tail");
    }

    #[test]
    fn eof_alone_keeps_waiting_for_the_status() {
        // A command can close stdout and go on running.
        let (d, done) = run(StderrMode::Merge, vec![eof()]);
        assert!(!done);
        assert_eq!(d.grace_end(), None);
    }

    #[test]
    fn close_without_status_is_unknown_not_zero() {
        let (d, done) = run(StderrMode::Merge, vec![data("x"), eof(), close()]);
        assert!(done);
        let o = d.finish(false);
        assert_eq!(o.exit, ExitState::Unknown);
        assert!(!o.timed_out);
        assert!(o.require_code().is_err());
    }

    #[test]
    fn channel_gone_is_unknown() {
        let (d, done) = run(StderrMode::Merge, vec![None]);
        assert!(done);
        assert_eq!(d.finish(false).exit, ExitState::Unknown);
    }

    #[test]
    fn refused_request_ends_the_drain_as_unknown() {
        let (d, done) = run(StderrMode::Merge, vec![Some(ChannelMsg::Failure)]);
        assert!(done);
        assert_eq!(d.finish(false).exit, ExitState::Unknown);
    }

    #[test]
    fn signal_is_reported_and_is_never_a_code() {
        let (d, done) = run(StderrMode::Merge, vec![eof(), signal(Sig::KILL)]);
        assert!(done);
        let o = d.finish(false);
        assert_eq!(o.exit, ExitState::Signal("KILL".to_string()));
        assert!(o.require_code().unwrap_err().contains("KILL"));
    }

    #[test]
    fn custom_signal_keeps_its_bare_name() {
        let (d, _) = run(StderrMode::Merge, vec![signal(Sig::Custom("WINCH".to_string()))]);
        assert_eq!(d.finish(false).exit, ExitState::Signal("WINCH".to_string()));
    }

    #[test]
    fn exit_without_eof_gets_only_the_grace() {
        // A daemon holds stdout: EOF never comes.
        let start = Instant::now();
        let mut d = Drain::new(StderrMode::Merge, DEFAULT_MAX_BYTES);
        assert!(!d.feed(status(0), start));
        assert_eq!(d.grace_end(), Some(start + GRACE_IDLE));
        let far = start + Duration::from_secs(60);
        assert_eq!(d.wake_at(Some(far)), Some(start + GRACE_IDLE));
        // The wait ended at the grace: the command finished, no timeout.
        let o = d.finish(true);
        assert!(!o.timed_out);
        assert_eq!(o.exit, ExitState::Code(0));
    }

    #[test]
    fn data_after_exit_extends_the_grace_up_to_the_hard_limit() {
        let start = Instant::now();
        let mut d = Drain::new(StderrMode::Merge, DEFAULT_MAX_BYTES);
        d.feed(status(0), start);
        let later = start + Duration::from_millis(300);
        assert!(!d.feed(data("a"), later));
        assert_eq!(d.grace_end(), Some(later + GRACE_IDLE));
        let much_later = start + GRACE_MAX;
        d.feed(data("b"), much_later);
        assert_eq!(d.grace_end(), Some(start + GRACE_MAX));
    }

    #[test]
    fn early_deadline_beats_the_grace() {
        let start = Instant::now();
        let mut d = Drain::new(StderrMode::Merge, DEFAULT_MAX_BYTES);
        d.feed(status(0), start);
        let deadline = start + Duration::from_millis(100);
        assert_eq!(d.wake_at(Some(deadline)), Some(deadline));
    }

    #[test]
    fn deadline_before_any_exit_is_a_timeout_with_partial_output() {
        let (d, done) = run(StderrMode::Merge, vec![data("partial")]);
        assert!(!done);
        let o = d.finish(true);
        assert!(o.timed_out);
        assert_eq!(o.exit, ExitState::Unknown);
        assert_eq!(o.stdout, "partial");
        assert!(o.require_code().unwrap_err().contains("timed out"));
    }

    #[test]
    fn no_timeout_and_no_exit_means_no_wake() {
        let d = Drain::new(StderrMode::Merge, DEFAULT_MAX_BYTES);
        assert_eq!(d.wake_at(None), None);
    }

    #[test]
    fn stderr_modes() {
        let msgs = || vec![data("o1"), ext("e1"), data("o2"), status(0), eof()];

        let o = run(StderrMode::Merge, msgs()).0.finish(false);
        assert_eq!((o.stdout.as_str(), o.stderr.as_str()), ("o1e1o2", ""));

        let o = run(StderrMode::Drop, msgs()).0.finish(false);
        assert_eq!((o.stdout.as_str(), o.stderr.as_str()), ("o1o2", ""));

        let o = run(StderrMode::Separate, msgs()).0.finish(false);
        assert_eq!((o.stdout.as_str(), o.stderr.as_str()), ("o1o2", "e1"));
    }

    #[test]
    fn utf8_split_across_chunks_survives() {
        let bytes = "é".as_bytes();
        let first = Some(ChannelMsg::Data { data: CryptoVec::from_slice(&bytes[..1]) });
        let second = Some(ChannelMsg::Data { data: CryptoVec::from_slice(&bytes[1..]) });
        let o = run(StderrMode::Merge, vec![first, second]).0.finish(false);
        assert_eq!(o.stdout, "é");
    }

    #[test]
    fn output_over_the_cap_stops_the_drain() {
        let mut d = Drain::new(StderrMode::Merge, 3);
        let now = Instant::now();
        assert!(!d.feed(data("abc"), now));
        assert!(d.feed(data("d"), now));
        assert!(d.over_cap);
    }

    #[test]
    fn dropped_stderr_does_not_count_against_the_cap() {
        let mut d = Drain::new(StderrMode::Drop, 1);
        assert!(!d.feed(ext("too long"), Instant::now()));
        assert!(!d.over_cap);
    }

    #[test]
    fn timeout_secs_keeps_the_other_defaults() {
        let o = ExecOpts::timeout_secs(8);
        assert_eq!(o.timeout, Some(Duration::from_secs(8)));
        assert_eq!(o.stderr, StderrMode::Merge);
        assert_eq!(o.max_bytes, DEFAULT_MAX_BYTES);
        assert!(o.stdin.is_none());
    }
}
