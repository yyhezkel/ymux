//! 2026-09-24: coalesce `pty:data` before it crosses into the webview.
//!
//! Every Tauri emit is an `evaluate_script` into the page, and on macOS each
//! one also flips WebKit's process-throttle state several times. The reader
//! threads used to emit once per `read()` — per 8 KB, or per SSH channel
//! message — so a busy pane cost 10–14 evals a second on the 0.5.1 test Mac,
//! and a flood like `yes` far more. xterm.js already folds everything that
//! lands inside one animation frame into a single write (terminalInstance.ts
//! `flushPending`), so smaller emits bought nothing on the far side.
//!
//! One flusher thread owns every session's pending text:
//! - after a quiet spell the first chunk goes out AT ONCE (leading edge), so
//!   a keystroke's echo is not delayed;
//! - anything arriving within `INTERVAL` of the last flush is buffered and
//!   sent when the interval closes — at most ~30 emits/s per busy session;
//! - a session holding `MAX_PENDING` bytes flushes early, bounding memory;
//! - nothing pending = the thread is blocked in `recv()`: zero emits idle.
//!
//! `pty:exit` goes through the same thread and flushes that session first, so
//! the last bytes can never arrive after the exit that closes the pane.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::{ipc_meter, log_warn, PtyDataEvent, PtyExitEvent};

const INTERVAL: Duration = Duration::from_millis(33);
const MAX_PENDING: usize = 1 << 20;

enum Msg {
    Data { session_id: String, text: String },
    Exit { session_id: String, reason: Option<String> },
}

static TX: OnceLock<Option<Sender<Msg>>> = OnceLock::new();

fn sender(app: &AppHandle) -> Option<&'static Sender<Msg>> {
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let app = app.clone();
        match std::thread::Builder::new()
            .name("pty-emit".into())
            .spawn(move || run(app, rx))
        {
            Ok(_) => Some(tx),
            Err(e) => {
                log_warn("PTY", &format!("pty-emit thread failed to start ({e}); emitting unbatched"));
                None
            }
        }
    })
    .as_ref()
}

/// Queue decoded terminal text for `session_id`.
pub(crate) fn data(app: &AppHandle, session_id: &str, text: String) {
    let msg = Msg::Data { session_id: session_id.to_string(), text };
    match sender(app) {
        Some(tx) => {
            if let Err(mpsc::SendError(msg)) = tx.send(msg) {
                emit_now(app, msg);
            }
        }
        None => emit_now(app, msg),
    }
}

/// Flush `session_id`'s pending text, then emit `pty:exit`.
pub(crate) fn exit(app: &AppHandle, session_id: &str, reason: Option<String>) {
    let msg = Msg::Exit { session_id: session_id.to_string(), reason };
    match sender(app) {
        Some(tx) => {
            if let Err(mpsc::SendError(msg)) = tx.send(msg) {
                emit_now(app, msg);
            }
        }
        None => emit_now(app, msg),
    }
}

fn emit_now(app: &AppHandle, msg: Msg) {
    match msg {
        Msg::Data { session_id, text } => emit_data_event(app, session_id, text),
        Msg::Exit { session_id, reason } => {
            let _ = app.emit("pty:exit", PtyExitEvent { session_id, reason });
        }
    }
}

fn emit_data_event(app: &AppHandle, session_id: String, data: String) {
    ipc_meter::record("emit:pty:data");
    let _ = app.emit("pty:data", PtyDataEvent { session_id, data });
}

/// Pending text per session, in first-arrival order. A Vec, not a map: there
/// are a handful of panes, and order keeps two panes' flushes deterministic.
struct Pending(Vec<(String, String)>);

impl Pending {
    fn push(&mut self, session_id: String, text: String) -> usize {
        if let Some((_, buf)) = self.0.iter_mut().find(|(s, _)| *s == session_id) {
            buf.push_str(&text);
            return buf.len();
        }
        let len = text.len();
        self.0.push((session_id, text));
        len
    }

    fn take(&mut self, session_id: &str) -> Option<String> {
        let i = self.0.iter().position(|(s, _)| s == session_id)?;
        Some(self.0.remove(i).1)
    }

    fn flush_all(&mut self, app: &AppHandle) {
        for (session_id, text) in self.0.drain(..) {
            emit_data_event(app, session_id, text);
        }
    }
}

fn run(app: AppHandle, rx: mpsc::Receiver<Msg>) {
    let mut pending = Pending(Vec::new());
    // Start "long ago" so the very first chunk takes the leading edge.
    let mut last_flush = Instant::now().checked_sub(INTERVAL).unwrap_or_else(Instant::now);
    loop {
        let msg = if pending.0.is_empty() {
            match rx.recv() {
                Ok(m) => m,
                Err(_) => return,
            }
        } else {
            let deadline = last_flush + INTERVAL;
            let now = Instant::now();
            if now >= deadline {
                pending.flush_all(&app);
                last_flush = Instant::now();
                continue;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    pending.flush_all(&app);
                    last_flush = Instant::now();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    pending.flush_all(&app);
                    return;
                }
            }
        };
        match msg {
            Msg::Data { session_id, text } => {
                let quiet = pending.0.is_empty() && last_flush.elapsed() >= INTERVAL;
                if quiet {
                    emit_data_event(&app, session_id, text);
                    last_flush = Instant::now();
                } else if pending.push(session_id.clone(), text) >= MAX_PENDING {
                    if let Some(buf) = pending.take(&session_id) {
                        emit_data_event(&app, session_id, buf);
                    }
                }
            }
            Msg::Exit { session_id, reason } => {
                if let Some(buf) = pending.take(&session_id) {
                    emit_data_event(&app, session_id.clone(), buf);
                }
                let _ = app.emit("pty:exit", PtyExitEvent { session_id, reason });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Pending;

    #[test]
    fn pending_appends_per_session_in_arrival_order() {
        let mut p = Pending(Vec::new());
        assert_eq!(p.push("a".into(), "12".into()), 2);
        assert_eq!(p.push("b".into(), "x".into()), 1);
        assert_eq!(p.push("a".into(), "345".into()), 5);
        assert_eq!(p.0[0], ("a".to_string(), "12345".to_string()));
        assert_eq!(p.take("a").as_deref(), Some("12345"));
        assert_eq!(p.take("a"), None);
        assert_eq!(p.0.len(), 1);
    }
}
