//! Phase 35 (#1.2): OSC notification detection.
//!
//! A stateful, streaming parser that watches a PTY byte stream for the
//! desktop-notification OSC escape sequences emitted by iTerm2 / Kitty /
//! rxvt-unicode and by any tool that knows them (a `cargo build` wrapper,
//! a pytest plugin, a custom script doing `printf '\e]9;done\a'`):
//!
//!   - OSC 9   — `ESC ] 9 ; <message> <terminator>`        → body only
//!   - OSC 99  — `ESC ] 99 ; <message> <terminator>`       → body only
//!   - OSC 777 — `ESC ] 777 ; notify ; <title> ; <body> <terminator>`
//!
//! Both terminators are accepted: BEL (`0x07`) and ST (`ESC \`, i.e.
//! `0x1B 0x5C`).
//!
//! The parser is OBSERVE-ONLY. It never mutates or strips the stream —
//! the caller passes the original bytes through to xterm.js unchanged
//! and uses the returned notifications as a side channel. This makes it
//! a universal complement to the agent-specific hooks: any process that
//! can print an escape sequence gets a ymux feed item for free.
//!
//! A 4 KB cap on the in-progress message guards against a runaway /
//! adversarial stream that opens an OSC and never terminates it.

const MAX_MSG: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OscKind {
    Osc9,
    Osc99,
    Osc777,
}

impl OscKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            OscKind::Osc9 => "osc9",
            OscKind::Osc99 => "osc99",
            OscKind::Osc777 => "osc777",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OscNotification {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) kind: OscKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Scanning for `ESC`.
    Idle,
    /// Saw `ESC`; waiting to see if it's `ESC ]` (OSC introducer).
    AfterEsc,
    /// Inside an OSC; collecting payload bytes until a terminator.
    InOsc,
}

pub(crate) struct OscNotifyParser {
    state: State,
    buf: Vec<u8>,
    // Inside InOsc we saw an ESC and are waiting on the next byte to
    // decide whether it's the `\` of an ST terminator.
    esc_pending: bool,
}

impl OscNotifyParser {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Idle,
            buf: Vec::new(),
            esc_pending: false,
        }
    }

    fn reset(&mut self) {
        self.state = State::Idle;
        self.buf.clear();
        self.esc_pending = false;
    }

    /// Feed a chunk of PTY bytes. Returns any notifications that
    /// completed within (or spanning into) this chunk. The chunk is
    /// not modified — the caller still forwards the original bytes.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<OscNotification> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                State::Idle => {
                    if b == 0x1B {
                        self.state = State::AfterEsc;
                    }
                }
                State::AfterEsc => {
                    if b == 0x5D {
                        // `ESC ]` — OSC introducer.
                        self.state = State::InOsc;
                        self.buf.clear();
                        self.esc_pending = false;
                    } else if b == 0x1B {
                        // Consecutive ESC — stay armed.
                    } else {
                        // Some other escape sequence (CSI etc.) — ignore.
                        self.state = State::Idle;
                    }
                }
                State::InOsc => {
                    if self.esc_pending {
                        self.esc_pending = false;
                        if b == 0x5C {
                            // `ESC \` — ST terminator.
                            if let Some(n) = self.parse_payload() {
                                out.push(n);
                            }
                            self.reset();
                        } else if b == 0x1B {
                            // ESC ESC — keep waiting on the next byte.
                            self.esc_pending = true;
                        } else {
                            // ESC followed by something that isn't `\`
                            // — a new escape sequence began mid-OSC.
                            // Abort this OSC; re-arm if this byte is ESC
                            // (handled above) else drop to Idle.
                            self.reset();
                        }
                    } else if b == 0x07 {
                        // BEL terminator.
                        if let Some(n) = self.parse_payload() {
                            out.push(n);
                        }
                        self.reset();
                    } else if b == 0x1B {
                        // Possible start of an ST terminator.
                        self.esc_pending = true;
                    } else {
                        self.buf.push(b);
                        if self.buf.len() > MAX_MSG {
                            // Runaway OSC — abort and resync.
                            self.reset();
                        }
                    }
                }
            }
        }
        out
    }

    fn parse_payload(&self) -> Option<OscNotification> {
        // buf holds the bytes between `ESC ]` and the terminator,
        // e.g. b"9;message" or b"777;notify;title;body".
        let s = String::from_utf8_lossy(&self.buf);
        let (ps, rest) = s.split_once(';')?;
        match ps {
            // ConEmu / Windows Terminal overload OSC 9 with numbered
            // sub-commands — `9;4;<state>;<pct>` is the taskbar progress bar,
            // which Claude Code re-emits many times a second while it works.
            // Reading those as notifications flooded the frontend with one
            // event (and one pane-pulse, one Notification Center row, one
            // dock-badge invoke) per frame; on a 2017 MacBook the resulting
            // repaint load hung the Intel GPU. A sub-command is a bare number
            // first field; a real notification body is text.
            "9" if is_conemu_subcommand(rest) => None,
            "9" => Some(OscNotification {
                title: String::new(),
                body: rest.to_string(),
                kind: OscKind::Osc9,
            }),
            "99" => Some(OscNotification {
                title: String::new(),
                body: rest.to_string(),
                kind: OscKind::Osc99,
            }),
            "777" => {
                // rest = "notify;<title>;<body>" — title/body may
                // themselves be empty; body may contain `;`.
                let mut parts = rest.splitn(3, ';');
                let sub = parts.next()?;
                if sub != "notify" {
                    return None;
                }
                let title = parts.next().unwrap_or("").to_string();
                let body = parts.next().unwrap_or("").to_string();
                Some(OscNotification {
                    title,
                    body,
                    kind: OscKind::Osc777,
                })
            }
            _ => None,
        }
    }
}

/// `rest` is everything after `9;`. ConEmu sub-commands are `1`..`12`
/// optionally followed by `;args` (`4;1;50`, `9;<cwd>`, `12`).
fn is_conemu_subcommand(rest: &str) -> bool {
    let first = rest.split(';').next().unwrap_or("");
    !first.is_empty() && first.len() <= 2 && first.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_clean_one_chunk() {
        let mut p = OscNotifyParser::new();
        let n = p.feed(b"\x1b]9;Build complete\x07");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, OscKind::Osc9);
        assert_eq!(n[0].title, "");
        assert_eq!(n[0].body, "Build complete");
    }

    #[test]
    fn osc9_split_across_two_chunks() {
        let mut p = OscNotifyParser::new();
        let first = p.feed(b"\x1b]9;Hel");
        assert!(first.is_empty());
        let second = p.feed(b"lo world\x07");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].body, "Hello world");
    }

    #[test]
    fn osc99_with_st_terminator() {
        let mut p = OscNotifyParser::new();
        let n = p.feed(b"\x1b]99;Tests passed\x1b\\");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, OscKind::Osc99);
        assert_eq!(n[0].body, "Tests passed");
    }

    #[test]
    fn osc777_title_and_body() {
        let mut p = OscNotifyParser::new();
        let n = p.feed(b"\x1b]777;notify;Deploy;Server is up\x07");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, OscKind::Osc777);
        assert_eq!(n[0].title, "Deploy");
        assert_eq!(n[0].body, "Server is up");
    }

    #[test]
    fn leading_bytes_passed_through_and_osc_still_detected() {
        let mut p = OscNotifyParser::new();
        // Ordinary shell output precedes the OSC. The parser observes
        // only; the detection still fires on the trailing sequence.
        let n = p.feed(b"user@host:~$ make\r\n\x1b]9;done\x07");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].body, "done");
    }

    #[test]
    fn garbage_between_two_oscs() {
        let mut p = OscNotifyParser::new();
        let n = p.feed(b"\x1b]9;first\x07some unrelated text\x1b]9;second\x07");
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].body, "first");
        assert_eq!(n[1].body, "second");
    }

    #[test]
    fn runaway_osc_is_aborted() {
        let mut p = OscNotifyParser::new();
        let mut huge = Vec::from(&b"\x1b]9;"[..]);
        huge.extend(std::iter::repeat(b'x').take(MAX_MSG + 100));
        let n = p.feed(&huge);
        assert!(n.is_empty());
        // Parser resynced — a fresh clean OSC after the runaway works.
        let n2 = p.feed(b"\x1b]9;ok\x07");
        assert_eq!(n2.len(), 1);
        assert_eq!(n2[0].body, "ok");
    }

    #[test]
    fn conemu_subcommands_are_not_notifications() {
        let mut p = OscNotifyParser::new();
        // Claude Code's progress bar, set / indeterminate / clear.
        assert!(p.feed(b"\x1b]9;4;1;50\x07").is_empty());
        assert!(p.feed(b"\x1b]9;4;3\x1b\\").is_empty());
        assert!(p.feed(b"\x1b]9;4;0;0\x07").is_empty());
        assert!(p.feed(b"\x1b]9;12\x07").is_empty());
        // Text that merely starts with a digit is still a notification.
        let n = p.feed(b"\x1b]9;3 tests failed\x07");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].body, "3 tests failed");
    }
}
