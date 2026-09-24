//! 2026-09-23: webview↔backend traffic meter.
//!
//! A 0.5.0 build on a 2017 MacBook Pro sent ~63 `invoke`s a second for over an
//! hour, and the macOS unified log could say only that — WebKit records a
//! custom-scheme load per call but never which command it carried. Tracking it
//! down meant reading the code for every possible caller. This counts every
//! command at the one choke point (`invoke_handler`), plus the hottest
//! Rust→JS emits, and reports the rate to `debug.log` once a minute — but only
//! when there is something to report, so an idle app writes nothing.
//!
//! Metadata only (Rule #1): command and event NAMES and counts, never an
//! argument or a payload.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ymux_core::{log_info, log_warn};

const WINDOW: Duration = Duration::from_secs(60);
/// Stay silent below this many calls per window — an idle app makes a
/// handful of invokes a minute and that is not news.
const REPORT_AT: u64 = 120;
/// At or above this sustained rate the line is a WARN: something is looping.
const WARN_PER_SEC: u64 = 10;
const TOP_N: usize = 8;

struct Window {
    start: Instant,
    total: u64,
    counts: BTreeMap<String, u64>,
}

impl Window {
    fn new(start: Instant) -> Self {
        Self { start, total: 0, counts: BTreeMap::new() }
    }
}

static METER: Mutex<Option<Window>> = Mutex::new(None);

/// Count one invoke (command name) or one emit (`emit:<event>`).
pub(crate) fn record(name: &str) {
    let finished = {
        let Ok(mut guard) = METER.lock() else { return };
        let now = Instant::now();
        let w = guard.get_or_insert_with(|| Window::new(now));
        w.total += 1;
        match w.counts.get_mut(name) {
            Some(c) => *c += 1,
            None => {
                w.counts.insert(name.to_string(), 1);
            }
        }
        if now.duration_since(w.start) < WINDOW {
            return;
        }
        std::mem::replace(w, Window::new(now))
    };
    report(finished);
}

fn report(w: Window) {
    if w.total < REPORT_AT {
        return;
    }
    let secs = w.start.elapsed().as_secs().max(1);
    let per_sec = w.total / secs;
    let mut top: Vec<(String, u64)> = w.counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let list = top
        .iter()
        .take(TOP_N)
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let msg = format!("{} calls in {secs}s (~{per_sec}/s) — top: {list}", w.total);
    if per_sec >= WARN_PER_SEC {
        log_warn("IPC", &msg);
    } else {
        log_info("IPC", &msg);
    }
}

/// Wrap the generated command dispatcher so every invoke is counted.
pub(crate) fn metered<F>(
    handler: F,
) -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static
where
    F: Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static,
{
    move |invoke| {
        record(invoke.message.command());
        handler(invoke)
    }
}
