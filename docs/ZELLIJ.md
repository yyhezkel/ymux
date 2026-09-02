# Zellij — the command surface ymux talks to

**Source of truth: `zellij 0.44.3` on Windows (`%LOCALAPPDATA%\Zellij\zellij.exe`),
dumped from `--help` on 2026-08-20.** Not from zellij.dev — the website documents a
newer build and already lists verbs this binary does not have (`focus-last-pane`,
`set-pane-frame-style`, `toggle-no-ui-fullscreen`). If a verb is not in the tables
below, our zellij does not have it. Regenerate with the script at the bottom before
trusting this after a zellij upgrade.

Companion to `docs/PROTOCOLS.md` (wire formats) and `docs/CLI.md` (ymux's own CLI).

---

## 1. What ymux sends today

Everything is in one block in [lib.rs:2149](../app/src-tauri/src/lib.rs#L2149).
Two kinds of caller, and the difference is the whole safety story:

| what | how it reaches zellij | site |
|---|---|---|
| `zellij attach -c <name>` | **typed as a string into the user's shell** — must parse in both cmd.exe and PowerShell | [`build_zellij_attach_command`](../app/src-tauri/src/lib.rs#L2374) |
| `list-sessions -n` | child process, argv array | [`zellij_args_list`](../app/src-tauri/src/lib.rs#L2247) |
| `delete-session -f <name>` | child process, argv array | [`zellij_args_delete_force`](../app/src-tauri/src/lib.rs#L2257) |
| `-s <name> action write-chars <chars>` | child process, argv array | [`zellij_args_write_chars`](../app/src-tauri/src/lib.rs#L2276) |
| `-s <name> action dump-screen` | child process, argv array, stdout captured (Phase 90 active-sessions overview) | [`zellij_args_dump_screen`](../app/src-tauri/src/lib.rs) |

argv only, never a shell — Rule #3. New verbs go through
[`zellij_try`](../app/src-tauri/src/lib.rs#L2325), which reports `Ok` /
`Failed { code, stderr }` / `Missing`. The last one matters: a missing binary used
to be the same `false` as a failed verb, which is how a Kill on a machine with no
zellij installed destroyed nothing and reported success.
[`zellij_run`](../app/src-tauri/src/lib.rs#L2366) is a bool wrapper over it for
callers that only need "did it land".

Binary resolution: [`zellij_exe`](../app/src-tauri/src/lib.rs#L2192) prefers
`%LOCALAPPDATA%\Zellij\zellij.exe` and falls back to PATH, because the winget MSI's
PATH edit only reaches processes started *after* the install.

**Deliberately not sent** (keep this list current when you add a verb):

- `kill-session <name>` — it stops a running session but keeps the serialized copy,
  so the session comes back marked EXITED and `attach` still resurrects it. ymux sent
  it until 2026-08-20, falling through to `delete-session` only when the kill
  *failed*, so that one click never destroyed a live session. The intent was a
  fail-safe two-step; the second step was unreachable, because the first click also
  drops the pane from `pane_sessions` and the button is gated on `isConnected` /
  `isTmux()`. See §6.
- `attach -b` (create detached) — returns 0 and creates nothing without a tty, so
  there is no way to tell success from silence. Panes create by attaching.
- `action rename-session` — the session name is derived from the pane id so a cold
  start can find it again. Pane labels live app-side. Phase 90's active-sessions
  overview renames tmux sessions for real and shows its Rename button disabled on
  zellij for exactly this reason — do not "fix" that by sending the verb.
- `kill-all-sessions` / `delete-all-sessions` — nothing in the UI means "every
  session on this machine", including ones ymux never created.

---

## 2. Root CLI

```
zellij [OPTIONS] [SUBCOMMAND]
```

Root options — **these come before the subcommand**. That positioning is not
cosmetic: `zellij -s foo action write-chars x` targets session `foo`, while
`zellij action write-chars x -s foo` is a parse error.

| option | env var | meaning |
|---|---|---|
| `-c, --config <FILE>` | `ZELLIJ_CONFIG_FILE` | config file path — **this is how ymux injects its config**, see §5 |
| `--config-dir <DIR>` | `ZELLIJ_CONFIG_DIR` | config directory |
| `--data-dir <DIR>` | | where zellij looks for plugins |
| `-d, --debug` | | extra debug output |
| `-l, --layout <NAME\|PATH>` | | layout; inside a session adds tabs, outside starts a session |
| `--layout-string <KDL>` | | same, raw KDL instead of a file |
| `-n, --new-session-with-layout <L>` | | always a new session, even from inside one |
| `-s, --session <NAME>` | | name of the session — **also the targeting flag for `action`** |
| `--max-panes <N>` | | opening more closes old ones |

Subcommands (aliases in brackets):

| subcommand | alias | what it does |
|---|---|---|
| `action` | `ac` | send an action to a session — §4 |
| `attach` | `a` | attach to a session — §3 |
| `list-sessions` | `ls` | list active + resurrectable sessions — §3 |
| `kill-session` | `k` | stop a running session (serialized copy survives) |
| `delete-session` | `d` | discard the serialized copy |
| `kill-all-sessions` | `ka` | — |
| `delete-all-sessions` | `da` | — |
| `run` | `r` | run a command in a new pane; prints `terminal_<id>` |
| `edit` | `e` | open a file in `$EDITOR`; prints `terminal_<id>` |
| `plugin` | `p` | load a plugin; prints `plugin_<id>` |
| `pipe` | | send data to plugins, launching them if needed |
| `list-aliases` | `la` | list plugin aliases |
| `options` | | override behaviour flags at launch — §5 |
| `setup` | | dump config / check setup — §5 |
| `convert-config` / `convert-layout` / `convert-theme` | | migrate old YAML to KDL |
| `subscribe` | | subscribe to pane render updates (viewport + scrollback) |
| `watch` | `w` | attach read-only |
| `web` | | run the web server that serves sessions in a browser |

---

## 3. Session verbs (the ones ymux lives on)

### `attach`

```
zellij attach [OPTIONS] [SESSION_NAME]
```

| flag | meaning |
|---|---|
| `-c, --create` | create if missing — **attach-or-create**, same as tmux `new-session -A -s` |
| `-b, --create-background` | create detached. Unusable headless: exits 0 having created nothing |
| `-f, --force-run-commands` | when resurrecting a dead session, rerun its commands immediately |
| `--forget` | delete the saved session before connecting |
| `--index <N>` | attach by position in the creation-ordered list |
| `-t, --token`, `-r, --remember`, `--ca-cert`, `--insecure` | remote/web-server auth only |

`attach -c` also **resurrects an EXITED session** — that is the reattach-after-reboot
path, and the one thing zellij does that tmux cannot.

### `list-sessions`

```
zellij list-sessions [-n|--no-formatting] [-r|--reverse] [-s|--short]
```

Use `-n`. Upstream documents it as the parsing form; `-s` drops the age and the
EXITED marker. Real 0.44.3 output on Windows:

```text
spike [Created 12m 30s ago]
old-one [Created 3h 4m 1s ago] (EXITED - attach to resurrect)
current-one [Created 5s ago] (current)
```

Parsed by [`parse_zellij_sessions`](../app/src-tauri/src/lib.rs#L2258). No window
count in this output — ymux reports `1` as an honest floor. `(current)` marks the
session the *calling client* is inside; ymux always asks from outside, so it is
never us, and is treated as "someone is attached".

### `kill-session` / `delete-session`

`kill-session <name>` stops a running session — the serialized copy survives, so it
reappears in the list as `(EXITED - attach to resurrect)`.
`delete-session [-f|--force] <name>` discards that copy; `-f` kills it first if it is
still running.

**ymux sends `delete-session -f` and nothing else.** One verb covers both the live and
the already-exited case, so "Kill session" means the session is gone rather than
resurrectable. Captured from 0.44.3 on 2026-08-20, and what the outcome classifier in
`kill_pane_session_inner` keys on:

```text
$ zellij delete-session -f a-name-that-does-not-exist
Session: "a-name-that-does-not-exist" not found.      # on STDERR
$ echo $?
2
```

That case is reported as `already_gone`, not `failed` — there was nothing to destroy,
which is what the caller wanted. A live delete cannot be exercised headlessly
(`attach -b` exits 0 without creating anything), so it is a live-checklist item.

---

## 4. `zellij action` — the full verb list (0.44.3)

**Targeting.** `action` needs to know which session. Either it inherits `$ZELLIJ`
(running inside one) or you pass the **root** option: `zellij -s <session> action …`.
Within a session, most verbs default to the focused pane / active tab, and take
`--pane-id <terminal_N|plugin_N|N>` or `--tab-id <N>` to aim elsewhere. A bare
integer pane id means `terminal_<N>`.

**Verbs that return data on stdout** — `list-panes`, `list-tabs`, `list-clients`,
`current-tab-info`, `query-tab-names`, `dump-layout`, `dump-screen`,
`are-floating-panes-visible` (also exits 1 for false). The first four take `--json`,
which is the form to parse from Rust. Pane-creating verbs (`new-pane`, `run`,
`edit`, `plugin`, `launch-plugin`) print the new pane id.

| action | args | flags |
|---|---|---|
| `are-floating-panes-visible` | — | `--tab-id` |
| `change-floating-pane-coordinates` | — | `--borderless`, `--height`, `--pane-id`, `--pinned`, `--width`, `--x`, `--y` |
| `clear` | — | `--pane-id` |
| `close-pane` | — | `--pane-id` |
| `close-tab` | — | `--tab-id` |
| `close-tab-by-id` | `<ID>` | — |
| `current-tab-info` | — | `--json` |
| `detach` | — | — |
| `dump-layout` | — | — |
| `dump-screen` | — | `--ansi`, `--full`, `--pane-id`, `--path` |
| `edit` | `<FILE>` | `--borderless`, `--close-replaced-pane`, `--cwd`, `--direction`, `--floating`, `--height`, `--in-place`, `--line-number`, `--near-current-pane`, `--pinned`, `--tab-id`, `--width`, `--x`, `--y` |
| `edit-scrollback` | — | `--ansi`, `--pane-id` |
| `focus-next-pane` | — | — |
| `focus-pane-id` | `<PANE_ID>` | — |
| `focus-previous-pane` | — | — |
| `go-to-next-tab` | — | — |
| `go-to-previous-tab` | — | — |
| `go-to-tab` | `<INDEX>` | — |
| `go-to-tab-by-id` | `<ID>` | — |
| `go-to-tab-name` | `<NAME>` | `--create` |
| `half-page-scroll-down` | — | `--pane-id` |
| `half-page-scroll-up` | — | `--pane-id` |
| `hide-floating-panes` | — | `--tab-id` |
| `launch-or-focus-plugin` | `<URL>` | `--close-replaced-pane`, `--configuration`, `--floating`, `--in-place`, `--move-to-focused-tab`, `--skip-plugin-cache`, `--tab-id` |
| `launch-plugin` | `<URL>` | `--close-replaced-pane`, `--configuration`, `--floating`, `--in-place`, `--skip-plugin-cache`, `--tab-id` |
| `list-clients` | — | — |
| `list-panes` | — | `--all`, `--command`, `--geometry`, `--json`, `--state`, `--tab` |
| `list-tabs` | — | `--all`, `--dimensions`, `--json`, `--layout`, `--panes`, `--state` |
| `move-focus` | `<DIRECTION>` | — |
| `move-focus-or-tab` | `<DIRECTION>` | — |
| `move-pane` | `<DIRECTION>` | `--pane-id` |
| `move-pane-backwards` | — | `--pane-id` |
| `move-tab` | `<DIRECTION>` | `--tab-id` |
| `new-pane` | `<COMMAND>...` | `--block-until-exit`, `--block-until-exit-failure`, `--block-until-exit-success`, `--blocking`, `--borderless`, `--close-on-exit`, `--close-replaced-pane`, `--configuration`, `--cwd`, `--direction`, `--floating`, `--height`, `--in-place`, `--name`, `--near-current-pane`, `--pinned`, `--plugin`, `--skip-plugin-cache`, `--stacked`, `--start-suspended`, `--tab-id`, `--width`, `--x`, `--y` |
| `new-tab` | `<INITIAL_COMMAND>...` | `--block-until-exit`, `--block-until-exit-failure`, `--block-until-exit-success`, `--close-on-exit`, `--cwd`, `--initial-plugin`, `--layout`, `--layout-dir`, `--layout-string`, `--name`, `--start-suspended` |
| `next-swap-layout` | — | `--tab-id` |
| `override-layout` | `<LAYOUT>` | `--apply-only-to-active-tab`, `--layout-dir`, `--layout-string`, `--retain-existing-plugin-panes`, `--retain-existing-terminal-panes` |
| `page-scroll-down` | — | `--pane-id` |
| `page-scroll-up` | — | `--pane-id` |
| `paste` | `<CHARS>` | `--pane-id` |
| `pipe` | `<PAYLOAD>` | `--args`, `--floating-plugin`, `--force-launch-plugin`, `--in-place-plugin`, `--name`, `--plugin`, `--plugin-configuration`, `--plugin-cwd`, `--plugin-title`, `--skip-plugin-cache` |
| `previous-swap-layout` | — | `--tab-id` |
| `query-tab-names` | — | — |
| `rename-pane` | `<NAME>` | `--pane-id` |
| `rename-session` | `<NAME>` | — |
| `rename-tab` | `<NAME>` | `--tab-id` |
| `rename-tab-by-id` | `<ID> <NAME>` | — |
| `resize` | `<RESIZE> <DIRECTION>` | `--pane-id` |
| `save-session` | — | — |
| `scroll-down` | — | `--pane-id` |
| `scroll-to-bottom` | — | `--pane-id` |
| `scroll-to-top` | — | `--pane-id` |
| `scroll-up` | — | `--pane-id` |
| `send-keys` | `<KEYS>...` | `--pane-id` |
| `set-dark-theme` | — | — |
| `set-light-theme` | — | — |
| `set-pane-borderless` | — | `--borderless`, `--pane-id` |
| `set-pane-color` | — | `--bg`, `--fg`, `--pane-id`, `--reset` |
| `show-floating-panes` | — | `--tab-id` |
| `stack-panes` | `<PANE_IDS>...` | — |
| `start-or-reload-plugin` | `<URL>` | `--configuration` |
| `switch-mode` | `<INPUT_MODE>` | — |
| `switch-session` | `<NAME>` | `--cwd`, `--layout`, `--layout-dir`, `--layout-string`, `--pane-id`, `--tab-position` |
| `toggle-active-sync-tab` | — | `--tab-id` |
| `toggle-floating-panes` | — | `--tab-id` |
| `toggle-fullscreen` | — | `--pane-id` |
| `toggle-pane-borderless` | — | `--pane-id` |
| `toggle-pane-embed-or-floating` | — | `--pane-id` |
| `toggle-pane-frames` | — | — |
| `toggle-pane-pinned` | — | `--pane-id` |
| `toggle-theme` | — | — |
| `undo-rename-pane` | — | `--pane-id` |
| `undo-rename-tab` | — | `--tab-id` |
| `write` | `<BYTES>...` | `--pane-id` |
| `write-chars` | `<CHARS>` | `--pane-id` |

### Verbs worth knowing about for ymux

- **`write-chars <CHARS>`** — what the connect wizard uses. Writes into the shell
  *inside* zellij, not the shell zellij was launched from. Takes `--pane-id`.
- **`write <BYTES>...`** — raw bytes (space-separated decimals), for control chars.
- **`send-keys <KEYS>...`** — named keys: `"Ctrl a"`, `"F1"`, `"Alt Shift b"`.
- **`paste <CHARS>`** — bracketed-paste mode, so a TUI sees it as a paste, not typing.
- **`dump-screen [--full] [--ansi] [--path F]`** — viewport, optionally with
  scrollback and ANSI. A read path that doesn't need the PTY stream.
- **`list-panes --json --all`** — machine-readable pane inventory (state, geometry,
  running command, tab). The honest way to get a window/pane count, which
  `list-sessions` does not give.
- **`new-pane`/`run`** — `--block-until-exit-success` and friends make the CLI call
  wait for the command, which turns a pane into a job runner.
- **`switch-mode <locked|pane|tab|resize|move|search|session>`** — relevant if we
  ever want zellij's own keybinds out of the way; `locked` is the "hands off" mode.
- **`toggle-pane-frames`** — the runtime equivalent of our `pane_frames false`.
  Not used: a config file has no race with startup, a runtime toggle does.

---

## 5. Configuration — what ymux sets and how

### How the config reaches zellij

`spawn_local_pty` sets **two** env vars on the pane's shell, both resolved by
[`resolve_zellij_config`](../app/src-tauri/src/lib.rs#L1905):

| var | points at | why |
|---|---|---|
| `ZELLIJ_CONFIG_FILE` | `resources/ymux-zellij.kdl` | the settings |
| `ZELLIJ_CONFIG_DIR` | `resources/` | makes `default_layout "ymux"` resolve |

Env vars rather than flags in the typed line, because the path contains spaces and
would need two dialects of quoting. Both `--config` and `--config-dir` document their
env var in `--help`, so this is the supported form, not a trick — and there is no flag
for the layout at all, since `zellij attach` takes no `--layout` and the root `-l`
conflicts with `attach`. The config file is the only channel.

**Why `ZELLIJ_CONFIG_DIR` and not `layout_dir`.** `default_layout` names a layout
*inside* the layout dir, which defaults to a `layouts/` subdirectory of the config dir.
Setting `layout_dir` explicitly would mean writing an absolute install path into a
bundled file that ships identical bytes to every machine. Captured from
`zellij setup --check` on 0.44.3, 2026-08-20:

```text
[CONFIG DIR]:  …\app\src-tauri\resources
[CONFIG FILE]: Well defined.
[LAYOUT DIR]:  …\app\src-tauri\resources\layouts
[DATA DIR]:    C:\Users\…\AppData\Roaming\Zellij\data
[PLUGIN DIR]:  C:\Users\…\AppData\Roaming\Zellij\data\plugins
```

Note what did **not** move: DATA DIR and PLUGIN DIR. It also becomes the base for
`theme_dir`, which is harmless — ymux ships no themes and sets none.

**Resolution is all-or-nothing.** `pick_zellij_resources` accepts a directory only when
it holds both `ymux-zellij.kdl` and `layouts/ymux.kdl`. Shipping the config alone would
promise a `default_layout` the layout dir cannot keep, and zellij is not loud about an
unknown layout name. Missing either one falls back silently to the user's own config —
a supported state, logged as a WARN.

**This replaces the user's config, it does not merge with it.** Every key is one more
thing that can disagree with what the user configured for their own zellij, so the bar
is "the invariant breaks without it".

### What ymux sets, and why each one

The invariant: **1 ymux pane == 1 zellij session == 1 tab == exactly 1 zellij pane,
with zero zellij chrome.** Zellij is a persistence layer underneath a ymux pane, not a
multiplexer the user drives.

| key | reason |
|---|---|
| `pane_frames false` | a ymux pane draws its own header; the frame's vertical bars at both edges also made every row look like a table row to the per-row direction pass in `app/src/textDirection.ts` (the mirrored-English screenshot, 2026-08-19) |
| `default_layout "ymux"` | selects `layouts/ymux.kdl`, a bare `pane` — see below |
| `keybinds clear-defaults=true { }` | zero keybinds; the empty body is required by the parser |
| `default_mode "locked"` | second lock, so an un-clearable future binding still doesn't fire |
| `mouse_mode true` | zellij owns the wheel — it holds the only scrollback the pane has |
| `show_release_notes false` / `show_startup_tips false` | either can add a pane ymux never asked for |

**`pane_frames false` is not what removes "the frame".** It removes the border *around*
a pane. The two rows above and below are **plugin panes** in zellij's default layout:

```text
layout {
    pane size=1 borderless=true { plugin location="tab-bar" }
    pane
    pane size=1 borderless=true { plugin location="status-bar" }
}
```

That is why the frame survived `pane_frames false` for a day. `layouts/ymux.kdl` is
`layout { pane }` — no plugin rows, which gives every pane back two rows.

**`mouse_mode` was argued both ways on 2026-08-20; `true` is the settled answer.** It
shipped `false` first, on the theory that xterm.js owns the wheel and the selection. Half
right — xterm.js does own selection, but its wheel scrolls its NORMAL buffer while zellij
holds the alt screen. Combined with cleared keybinds (no scroll mode), that left a pane
with **no way to scroll back at all**. `true` is also what the tmux side already does:
ymux appends `set -g mouse on` to the tmux attach for exactly this (decision O-3,
`docs/MOUSE-DEBUG.md`).

What `true` costs, and why each cost is covered:

- zellij captures drag-select, so a drag copies via `copy_on_select` (default `true`) and
  OSC 52. With keybinds cleared **that is the only way to copy inside the pane**, so
  leaving `copy_on_select` alone is load-bearing.
- a native xterm.js selection still works with **Shift** held.
- mouse-tracking escapes leaking into a bare shell after an unclean exit are already
  handled transport-agnostically by `resetMouseModes()` on connect and on `pty:exit`.

**Copy comes back as visual-order Hebrew in a Claude pane**, and is un-reversed on the way
to the clipboard by `app/src/copyBidi.ts`. Owning the mouse was never the blocker for
that — `ClipboardAddon` has always routed OSC 52 through ymux's own provider.

**Relied-on defaults, not restated in the file:** `session_serialization true` (the
whole persistence feature) and `on_force_close "detach"` (already the default, and a
signal handler with no signal to deliver on Windows).

**Which keys need a restart.** `default_layout` and `mouse_mode` are marked
"(Requires restart)" — they are read when the session is *created*, which is the moment
ymux creates it. `pane_frames` and `keybinds` are client-side and apply on the next
attach. `default_mode` is not marked at all; it applies per client attach. The practical
consequence: **a session created before this shipped adopts the change only partly** —
keybinds and frames yes, layout and mouse no — so an old session comes back with bars
until it is killed once.

### Config file shape

KDL. Top-level scalar keys plus these blocks:

| block | what it holds |
|---|---|
| `keybinds { <mode> { bind "key" { Action "arg"; } } }` | per-mode bindings. Modes in the shipped defaults: `normal locked resize pane move tab scroll search entersearch renametab renamepane session tmux`, plus `shared_except "<mode>" …` / `shared_among` blocks for bindings that span modes. `keybinds clear-defaults=true` drops every built-in |
| `plugins { <alias> location="zellij:name" { … } }` | plugin aliases (tab-bar, status-bar, strider, session-manager, filepicker, configuration, plugin-manager, about) |
| `load_plugins { "zellij:link" }` | background plugins loaded at session start |
| `themes { <name> { … } }` | theme definitions (or drop files in `theme_dir`) |
| `ui { pane_frames { rounded_corners true; hide_session_name false } }` | frame styling |
| `env { VAR "value" }` | env vars for every pane |
| `web_client { font "monospace" }` | browser-client rendering |

Every scalar key below is also a `zellij options --<kebab-case>` flag and a
`zellij --config-…`-time override, so **the CLI flag name is the config key with
dashes swapped for underscores.** Full list for 0.44.3:

**Appearance / UI** — `theme`, `theme_dark`, `theme_light` (the last two need each
other; if either is missing, static `theme` wins), `theme_dir`, `pane_frames`,
`simplified_ui`, `styled_underlines`, `visual_bell`, `osc8_hyperlinks`,
`show_startup_tips`, `show_release_notes`, `auto_layout`, `stacked_resize`.

**Shell / startup** — `default_shell`, `default_cwd`, `default_mode`,
`default_layout`, `layout_dir`, `session_name`, `attach_to_session`,
`on_force_close` (`detach` | `quit`).

**Mouse / clipboard** — `mouse_mode`, `advanced_mouse_actions`,
`mouse_hover_effects`, `mouse_click_through`, `focus_follows_mouse`,
`copy_on_select`, `copy_clipboard` (`system` | `primary`), `copy_command`,
`scrollback_editor`.

**Persistence / resurrection** — `session_serialization`, `serialize_pane_viewport`,
`scrollback_lines_to_serialize`, `serialization_interval`,
`disable_session_metadata`, `post_command_discovery_hook`, `scroll_buffer_size`.

**Multi-client / web** — `mirror_session`, `web_server`, `web_sharing`
(`on` | `off` | `disabled`), `client_async_worker_tasks`,
`support_kitty_keyboard_protocol`.

Some are marked *(Requires restart)* in the dumped default config — notably
`on_force_close`, `session_serialization`, `serialize_pane_viewport`. Changing those
mid-session does nothing; the session has to be killed and recreated.

### Considered and rejected for `ymux-zellij.kdl`

- `scroll_buffer_size` — zellij owns the buffer behind its alt screen; sizing it
  changes nothing ymux can reach.
- `copy_on_select` — **relied on at its default `true`, deliberately not restated.** With
  keybinds cleared it is the only way to copy inside the pane. Setting it explicitly would
  invite someone to flip it; a comment on `mouse_mode` says why it must not move.
- `simplified_ui`, `auto_layout`, `stacked_resize` — chrome only.
- `on_force_close "detach"` — already the default, and it handles SIGTERM/SIGHUP; on
  Windows the PTY kill has no signal to deliver, so restating it buys noise.
- `mouse_mode` **was** on this list and has been withdrawn twice over — first because the
  rejection was about forcing it *on* when the default was already on, then because the
  value we actually want is `true`. See above.

### Three ways to set the same thing, and which to use

1. **`ymux-zellij.kdl`** (via `ZELLIJ_CONFIG_FILE`) — persistent, applies before the
   first frame, no race. **Default choice.**
2. **`zellij options --foo bar` at launch** — one session, set at start. Would mean
   putting flags in the line typed into the user's shell; more quoting, no benefit.
3. **`zellij -s <session> action …`** — runtime, after the fact. Use only for things
   that are genuinely dynamic (writing chars, querying panes), never for settings —
   a runtime toggle races the first frame, a config file does not.

---

## 6. Gotchas already paid for

- **Root options go before the subcommand.** `-s <session>` is a root option; a
  rename attempt that put it after `action` just listed sessions instead.
- **`attach -b` is useless headless** — exits 0, creates nothing.
- **No `exec` on Windows**, so the typed `zellij attach -c` leaves the shell as
  parent. When zellij exits the user lands in a working shell, not a dead pane.
- **No existence guard around the typed command, on purpose.** cmd and PowerShell
  each print their own "not recognized" and leave a usable shell — a better fallback
  than a cross-shell check would give.
- **Session names must stay `[A-Za-z0-9_-]`** (`sanitize_session_name`), which is
  what makes the typed attach line safe to build with `format!`. Asserted by
  `zellij_attach_command_is_a_single_safe_line`.
- **EXITED sessions are real sessions.** They survive a reboot and are resurrectable;
  the picker offers them with `attached: false`.
- **The website is ahead of the binary.** Check `--help` before adding a verb.
- **A safety valve nobody can reach is not a safety valve.** The `kill-session` →
  `delete-session` two-step was designed so one click could never destroy a live
  session and a failure between the verbs left something resurrectable. Both true, and
  both irrelevant: the first click removed the pane from `pane_sessions`, so a second
  one returned early having sent no verb, and the Kill button is gated on
  `isConnected` / `isTmux()` — false by then, so it was not even rendered.
  `zellij_args_delete` had exactly one call site and nothing could reach it. The
  result was a red, danger-classed button labelled "no resume" that left a
  resurrectable session behind. Replaced 2026-08-20 by one forcing verb plus an
  honest outcome (`KillSessionOutcome`) the UI can report.
- **`no_session` is an answer, not an error.** `pane_kill_session` returns
  `Result<KillSessionOutcome, String>`; `Err` is reserved for "could not even try".
  A pane with nothing bound to it reports `no_session` and counts as gone.

---

## 7. Regenerating this doc

```bash
ZJ="$LOCALAPPDATA/Zellij/zellij.exe"; OUT=./zj-help
mkdir -p "$OUT"
"$ZJ" --version && "$ZJ" --help > "$OUT/root.txt"
"$ZJ" action --help > "$OUT/action.txt"
"$ZJ" options --help > "$OUT/options.txt"
"$ZJ" setup --dump-config > "$OUT/default-config.kdl"
for s in $(sed -n 's/^    \([a-z][a-z-]*\)$/\1/p' "$OUT/action.txt" | grep -v '^help$'); do
  echo "===== $s ====="; "$ZJ" action "$s" --help
done > "$OUT/action-all.txt"
```

`zellij setup` flags:

| flag | what it gives you |
|---|---|
| `--dump-config` | the full default config, every option documented in a comment |
| `--check` | resolved config / data / plugin dirs — how to confirm `ZELLIJ_CONFIG_FILE` took |
| `--clean` | ignore the user config entirely, run shipped defaults (bisecting a config bug) |
| `--dump-layout <NAME>` / `--dump-swap-layout <NAME>` | a built-in layout as KDL |
| `--dump-plugins [DIR]` | the built-in plugin wasm files |
| `--generate-completion <SHELL>` / `--generate-auto-start <SHELL>` | shell integration |

`setup --dump-config` also emits every default keybind and the commented-out
documentation for each option — the fastest way to see a key's default without
guessing. `setup --check` prints the resolved config/data/plugin dirs, which is how
to confirm `ZELLIJ_CONFIG_FILE` actually took.
