# Mouse-escape leak — root-cause investigation (v0.4.4-beta.2)

**Symptom (Yossi):** sequences like `^[[<65;42;31M` / `^[[<0;58;31m` print as
literal text in a bare SSH shell. These are **SGR 1006** mouse-report events
(`\e[<button;col;row;M|m`) that nothing is consuming, so the shell echoes them.

## What was ruled out

- **Shell/user config:** `grep '1006\|mouse' ~/.bashrc ~/.bash_profile ~/.zshrc
  ~/.inputrc` → empty. Not the shell rc files.
- **TERM:** `xterm-256color` (correct). terminfo is not enabling mouse.
- **xterm.js defaults:** the `Terminal` constructor in
  `app/src/terminalInstance.ts` sets **no** mouse option — no `mouseEvents`,
  no `disableStdin`, no `screenReaderMode`. Addons loaded are `FitAddon`,
  `WebglAddon` (non-auto RTL modes only), `ClipboardAddon` — **none enable
  mouse**. There is no `WebLinksAddon`; links use a custom `registerLinkProvider`.
  **xterm.js never turns mouse tracking on by itself** — it only reports mouse
  when the *remote app* sends a DECSET (`\e[?1000h` / `\e[?1006h` …). Confirmed:
  a repo-wide grep for `1000h|1002h|1003h|1006h|1015h` across `app/src` (TS),
  `app/src-tauri/src` (Rust) and `app/src-tauri/server` (Go) finds **no**
  mouse-*enable* sequence emitted by ymux.

## Root cause — ymux turned tmux mouse ON (until 2026-09-08)

**Historical since Phase 91.B:** the injection below is gone, the conf locks
tmux (mouse off, status off, prefix table = `[` / `d` / `C-b`, PageUp
scrollback) and the attach chains `source-file -q` so running servers adopt
it. Kept as the record of how the leak arose.

`app/src-tauri/src/lib.rs` (`build_tmux_attach_script`), the tmux attach command chain:

```
exec tmux -f $HOME/.ymux/tmux.conf new-session -A -s <name> \; set -g mouse on
```

The bundled `ymux-tmux.conf` ships `mouse off` (Phase 65 bug EE — `mouse on`
in the conf garbled Claude Code's live output), but ymux then **turns mouse
on at attach** via `\; set -g mouse on` — intentionally, so tmux-native **wheel
scrollback** works (DECISIONS 2026-06-22, option **O-3**).

With tmux `mouse on`, tmux enables SGR mouse tracking toward xterm.js. While you
stay inside tmux this is correct — tmux consumes the mouse events (wheel scroll,
pane select). **The leak is the exit path:** when you leave tmux to the bare
login shell in the same pane (`exit` / detach / tmux killed / an unclean drop),
the mouse-tracking DECSET is not always disabled on the way out, so xterm.js
keeps reporting — and the bare shell, which doesn't consume mouse events, prints
them as `\e[<..M` text.

(A second, independent source is any full-screen app the user runs — `vim` with
`mouse=a`, `fzf`, `less`, `htop` — that enables its own mouse tracking and exits
uncleanly. That lives in `~/.vimrc` etc., not the shell rc files Yossi grepped,
so it isn't ruled out. The fix below covers both sources.)

## The escape flow

```
ymux connect ──► PTY ──► tmux new-session … \; set -g mouse on
                                     │
                                     ▼
                        tmux sends \e[?1000h \e[?1006h to xterm.js
                                     │
                          xterm.js mouse reporting = ON
                                     │
              (inside tmux: tmux consumes clicks — fine)
                                     │
        user exits tmux ─► bare shell, mouse DECSET NOT cleared
                                     │
                 click ─► xterm sends \e[<b;c;rM ─► shell prints it
```

## Fix shipped (v0.4.4-beta.2)

Client-side, in `terminalInstance.ts` — recovery that works regardless of the
source (ymux/tmux OR a user app):

- **Reset on connect/attach:** `resetMouseModes()` writes
  `\e[?1000l\e[?1002l\e[?1003l\e[?1006l\e[?1015l\e[?9l` to xterm (the DISPLAY,
  not the PTY) so xterm drops any stale mouse-tracking state. Gated by
  Settings → Terminal → **"Reset mouse state on connect"** (default on).
  Note: for a *tmux* session this clears pre-existing stale state. (Until
  Phase 91.B tmux's own `set -g mouse on` then re-enabled mouse for the live
  session; since then tmux stays off and there is nothing to re-enable.)
- **Reset on pane process exit (extra safety):** in the `pty:exit` handler in
  `App.tsx` we call `resetMouseModes()` unconditionally after the
  `[disconnected]` notice — a full-screen app that enabled SGR/X10 tracking
  and died with the PTY (SSH drop, kill -9, tmux crash) never sent its own
  disable, so xterm otherwise keeps reporting on the leftover display. Not
  gated by the setting — this is a Rule-#1-safe fixed control string, and the
  user can't opt into a leak.
- **Manual reset:** Command Palette **"Reset terminal"** / **Ctrl+Alt+R** →
  `resetTerminal()` = the disable set + `\e[0m`. Recovers a pane that already
  leaked mid-session (e.g. right after exiting tmux), without touching
  scrollback (no RIS).

Rule #1: the disable string is a fixed control sequence — never PTY content.

## Decided 2026-09-08 (Phase 91.B): option 2, plus the lock

Yossi's actual complaint was never the wheel: with tmux tracking the mouse,
LEFT-CLICKS landed on whatever Claude Code was redrawing that instant and
selected / pasted things nobody aimed at. So: `set -g mouse on` deleted, O-3
retired, the conf locked like `ymux-zellij.kdl` (status off, `unbind -a -T
prefix`, keep `[` / `d` / `C-b`), root `PPage` → `copy-mode -eu` on the main
screen (passed through under `#{alternate_on}`), and the attach chains
`source-file -q` so a running server adopts it without kill-server. Full
entry in `docs/DECISIONS.md` (2026-09-08, Phase 91.B). The options as they
stood, for the record:

1. **Keep as-is + rely on the reset** (shipped) — wheel scrollback stays; the
   post-tmux-exit leak is cleared on next connect or via Ctrl+Alt+R.
2. **Drop `set -g mouse on`** — no more ymux-side mouse enable, so the bare
   shell can't inherit it; **cost:** lose tmux-native wheel scrollback (reverts
   O-3; wheel would scroll xterm's own scrollback instead).
3. **Belt-and-suspenders:** keep mouse on inside tmux, but append a mouse-off /
   reset to the shell's exit path so leaving tmux always disables mouse. More
   moving parts; needs care not to fight tmux.

#1 shipped in v0.4.4-beta.2; #2 (with the lock) landed 2026-09-08.

## Deferred

Monitor "Terminal state" diagnostic (live `term.modes.mouseTrackingMode`,
last-N escape bytes — metadata only, Rule #1, "Reset now" button) — a follow-up
observability aid, not part of the fix.
