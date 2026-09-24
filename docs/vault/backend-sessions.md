---
vault: backend-sessions
covers:
  - app/src-tauri/src/pty_decode.rs
  - app/src-tauri/src/bidi_filter.rs
  - app/src-tauri/src/osc_notify.rs
  - app/src-tauri/src/log_sync.rs
  - app/src-tauri/src/tunnel_registry.rs
---

# The PTY byte stream and what rides on it

Five modules that sit on the raw bytes flowing between a shell and the terminal, plus
the bookkeeping that keeps the reverse tunnel pointing at the right port. All five are
consequences of real bugs; each header comment names the incident.

## `pty_decode.rs` (266) — incremental UTF-8 reassembly

PTY output arrives in arbitrary-length chunks, so one Hebrew letter, emoji, or
box-drawing character is routinely split across two reads. This decoder **holds an
incomplete trailing sequence back** until the next chunk completes it.

The whole design turns on `Utf8Error::error_len()`:

- `None` — input ended mid-sequence, the tail may still become valid. **Hold it.**
- `Some(n)` — those `n` bytes are invalid no matter what follows (a bare `0x80`
  continuation byte, `cat` on a binary, a legacy app writing CP1255). **Emit U+FFFD,
  skip, continue.** Without this branch the decoder waits forever for bytes that can
  never complete.

`from_utf8_lossy` per chunk is the shortcut this module exists to *not* take — it turns
every split Hebrew letter into U+FFFD. That was the mojibake bug fixed in `1bcb584`.

## `bidi_filter.rs` (502) — opt-in RTL isolate wrapping

Wraps Latin runs in Unicode bidi isolates (FSI `U+2068` / PDI `U+2069`) when they appear
near Hebrew/Arabic, so `דוח-DEV.txt` renders in its logical position. **Default off**,
toggled per pane (`pane_set_smart_bidi`); state lives in `AppState.bidi_filters`, lazily
created on the first chunk per pane.

The non-goals are the specification:

- ANSI/CSI/OSC/DCS escapes pass through **verbatim** — a `U+2068` inside `\x1b[31m`
  breaks color.
- Cursor positioning (`\x1b[H`, `\x1b[<n>;<m>H`) preserved — bidi marks shift columns.
- Box-drawing (U+2500–257F, U+2580–259F) preserved, and a segment where box-drawing
  dominates is skipped whole — Claude Code's Ink TUI draws its borders with these.

Byte-level state machine, because a single escape sequence can split across chunks.

## `osc_notify.rs` (260) — desktop notifications from the stream

A streaming parser for the OSC notification sequences that iTerm2/Kitty/rxvt speak, and
that any script can emit with `printf '\e]9;done\a'`:

| Sequence | Shape | Yields |
|---|---|---|
| OSC 9 | `ESC ] 9 ; <message>` | body only — **except** ConEmu sub-commands, see below |
| OSC 99 | `ESC ] 99 ; <message>` | body only |
| OSC 777 | `ESC ] 777 ; notify ; <title> ; <body>` | title + body |

Both terminators accepted: BEL (`0x07`) and ST (`ESC \`).

**Observe-only** — it never mutates or strips the stream. The caller passes the original
bytes to xterm.js unchanged and treats the notifications as a side channel. That is what
makes it the universal complement to agent-specific hooks: any process that can print an
escape sequence gets a feed item for free. A 4 KB cap on the in-progress message guards
against a stream that opens an OSC and never closes it.

**`OSC 9;<1-2 digits>[;…]` is not a notification** (`is_conemu_subcommand`). ConEmu and
Windows Terminal overload OSC 9 with numbered commands, and `9;4;<state>;<pct>` is the
taskbar progress bar Claude Code re-emits many times a second while it works. Until
2026-09-23 each one became an `osc-notification` event → a Notification Center row, a
dock-badge invoke and a pulsing pane ring, and the repaint load hung a 2017 MacBook's
Intel GPU. A body that merely *starts* with a digit (`3 tests failed`) is still text.

## `tunnel_registry.rs` (339) — sticky ports and the connect lock

**Read the header comment before touching this.** The failure it fixes: a pane's hooks
reach the desktop through a reverse tunnel, and the address is handed to the remote three
ways — `set_env` on the shell channel, `tmux set-environment -g`, and
`~/.ymux/run/last.env`. **None of the three reaches a process that is already running.**
So a reconnect that asked the kernel for a fresh port left a long-lived `claude` dialing
the old one; every hook failed with `Connection refused`, and Claude Code silently fell
back to its own permission UI. From the user's side the pane just stopped talking to
ymux. In Yossi's 2026-08-18 log a hook was dialing a port that had been replaced 73
minutes earlier.

Three mechanisms:

1. **Sticky ports** — a workspace remembers the port it was granted and re-requests it
   on reconnect, so the baked-in address stays valid.
2. **The connect lock** — per-workspace, stops `workspace_ensure_connected` and
   `spawn_ssh` racing.
3. **One triple** `(port, token, owning session)` — replacing two maps that were read
   independently. The old port set was sampled with `.iter().next()` and only ever grew;
   the token map was cleared nowhere at all, so a watcher could get one connection's port
   with another's token and fail as `-DENIED bad-mac`.

Lives on `AppState` (not `CoreState`) because it also owns the connect lock.

## `log_sync.rs` (417) — one log file for the whole fleet

1. **Level push** — writes Settings → Logs level into `~/.ymux/log-level` on remote
   hosts. The Go server watches the file on a 30s ticker (no restart) and the CLI hooks
   read it once per process, so the fleet converges on the desktop's setting.
2. **Log pull** — every `SYNC_INTERVAL_SECS`, fetch the *new* bytes of the three remote
   log files (server / hooks / install) over the existing SSH sessions and append them
   verbatim to the local `debug.log`, so a non-technical user reads one file. Remote
   files stay in place as backup. Per-host byte cursors persist in `log-sync.json`,
   atomic tmp+rename.

**Rule #1 boundary:** pulled lines land in an LLM-readable local file. Every remote
writer already enforces metadata-only content; this module copies lines, never
interprets or executes them, and must not relax that.

## Invariants

- Nothing here logs stream **content** — byte counts and pane ids only (Rule #1).
- `tunnel_registry`'s token is Rule #8 material: never logged.
- The bidi filter and OSC parser both see the same bytes `pty_decode` produced. Order
  matters — decode first, then observe, then filter.

## Read the source when

You need the exact state-machine transitions in `bidi_filter`, the OSC terminator
edge cases, or `tunnel_registry`'s lock acquisition order. All three are short enough
to read whole, and all three have unit tests at the bottom of the file.
