---
vault: backend-wizards
covers:
  - app/src-tauri/src/connect_wizard.rs
  - app/src-tauri/src/local_setup.rs
  - app/src-tauri/src/local_wizard.rs
  - app/src-tauri/src/settings.rs
  - app/src-tauri/src/updater.rs
---

# Setup wizards, settings, updates

The local-machine half of the app: what it detects on this box, what it installs here,
what it remembers, and how it learns there is a new version. ~7,500 lines.

## `settings.rs` (2,897) — `%APPDATA%\ymux\settings.json`

Theme, fonts, terminal, hooks, notifications, updates, Claude, logs. Same discipline as
`workspaces.json`: **atomic write + load-poison gate**. Every mutation emits
`settings:changed` to the frontend, which is why a `ymux settings set` from the CLI
re-themes the running app with no reload.

**`settings_save` REPLACES the whole document, so any field the UI doesn't round-trip
is a wipe waiting to happen.** That is not hypothetical: a client holding a copy from
before `terminal.rtl` existed sent it back absent and erased the block, costing most of a
day of RTL testing against settings that were not in force. Two fields are therefore
carried over from the stored copy rather than taken from the client — `terminal.rtl` (the
UI can only ever ADD it, so `None` cannot mean "delete"), and `floating_windows`, which is
**Rust-owned**: its only writer is `set_browser_popout`, driven by the Browser pop-out
window opening and closing (Phase 85.C). No UI surface edits `floating_windows`, so a
value arriving from a client can only be a stale echo. If you add a field the backend
writes on its own, add it to that carry list.

`HookType` is the canonical enum of Claude Code hook types, serialized in the settings
file, and it is what `rpc_server`'s `hook_toast_enabled` / `hook_toast_should_sound`
consult per hook. `list_system_fonts` reads the HKCU font hive — the same hive
`fonts.rs` installs into, so a font install shows up in the picker immediately.

Presets (`settings.preset`, `settings.get-presets`) are exposed over RPC as well as
Tauri.

**`BriefOptions`** (BRIEF) — the Briefing-card trigger group
(`entry_card_on_return` / `entry_card_on_idle`, both default-false opt-ins, plus
`absence_minutes`=30 / `idle_minutes`=15), container-level `#[serde(default)]`,
hung off `Settings.brief`. Mirrored in `app/src/settings.ts` (`BriefSettings`)
and round-tripped by SettingsModal's General tab — the settings_save
whole-document replace makes that mirror mandatory, per the `terminal.rtl`
incident.

**`Shortcuts` is the one struct with container-level `#[serde(default)]`.** It carries
30 accelerator fields (28 as of Phase 87 — it was 8, the rest were hardcoded in the
frontend — plus BRIEF's `toggle_queue` Ctrl+Shift+Q and `show_briefing`
Ctrl+Alt+Q), and per-field
`#[serde(default = "...")]` would have meant twenty
near-identical helper fns. The container attribute makes `impl Default for Shortcuts`
the single source of truth instead, so a `settings.json` written by an older build
simply lacks the new keys and picks them up. Add a field there and to the `Default`
impl, nothing else. The frontend parses these strings; Rust never validates them.

### `RtlProfiles` — the one schema here with a real invariant

`TerminalSettings.rtl` is a `RtlProfiles { local, remote }`, each an `RtlProfile`
carrying `rtl_mode`, `auto_direction`, `mirror_arrows_rtl`, `tui_owns_bidi` and
`direction_policy`. The split is not cosmetic and not a local-vs-remote *geography*
test: it is keyed on `ConnCaps.posixExec`, so **WSL counts as remote**. It exists
because the two classes were measured to need OPPOSITE modes — a Windows ConPTY
pane hands over Hebrew already in VISUAL order, while SSH to Linux delivers
LOGICAL order — so one global setting could only ever satisfy one of them. Yossi's
report was "ההגדרה הזו עובדת או למקומי או למרוחק".

Three things here are load-bearing:

- **`rtl: Option<RtlProfiles>` is an `Option` on purpose.** Absent is the migration
  signal that `migrate_rtl_profiles` reads to lift the four deprecated flat fields
  onto the two profiles. `rtl_mode` is deliberately **not** carried over: a single
  pre-split value cannot be right for both sides.
- **`direction_policy` defaults to `any_rtl` when absent**, so an upgrade cannot
  silently move an existing pane onto the newer `tui_dominance` rule. The vote
  first shipped keyed on whether Claude held the pane, and because the OSC title
  propagates over SSH it fired on remote panes and broke a working path. The test
  `remote_direction_policy_is_the_pre_2026_08_19_rule` pins the separation.
- **`rtl_mode` is a `String`, not an enum** (the `sidebar_mode` pattern), so adding
  a mode costs nothing here and an unknown value from an older or newer
  settings.json degrades in the frontend rather than failing the whole
  deserialise. `force_rtl` (2026-08-23) was added that way; `default_rtl_mode`
  stays `auto_per_line` and neither profile default moved. What the modes *do*
  lives in `docs/vault/frontend-lib.md`.

## `local_setup.rs` (2,534) — the local install engine

The detect-and-install engine behind "local → new", **and** the home of the shared
hidden-console process helpers that every `wsl.exe` / `winget` / `npm` invocation in the
app goes through. Nothing in `src/` set `CREATE_NO_WINDOW` before this module existed;
spawning `wsl.exe` from a GUI app without it flashes a console window.

Mirrors `provisioning.rs` **on purpose** — same `StepProgress` payload so the wizard
reuses the step-card UI verbatim, same keep-going-on-failure semantics, events
`local-setup:progress` / `local-setup:complete`.

Rule #3 throughout: argv arrays only, and anything interpolated into a POSIX script
inside a distro goes through `ymux_core::shell_quote` — no exceptions.

**Platform split lives here.** On macOS the same commands and events drive the mac
wizard: the `winget` slot describes Homebrew, and the WSL chain is replaced by the tmux
persistence chain (`InstallTmuxLocal` → `DeployTmuxConfLocal`). The wizard shows a
persistence group **only on macOS** — on Windows zellij is an ordinary tool row, so a
group would offer the same install twice.

## `local_wizard.rs` (439) — the two small local affordances

1. `detect_local_shells()` — what is actually installed, labelled. Windows: PowerShell 7,
   Windows PowerShell, cmd, Git Bash, WSL. macOS/Linux: the `$SHELL` login shell first,
   then zsh/bash/fish/sh. The point is picking by label instead of typing a binary path.
2. `recent_paths` — a small JSON store of recently-used cwds, seeded with built-in
   defaults (`$USERPROFILE`, `~/Documents`, `~/source`) plus the user's own history by
   recency. Deduped, capped at 20. Lives on `AppState.recent_paths`.

## `connect_wizard.rs` (787) — the SSH form, made usable

Four affordances that turn a raw 4-field form into something an OpenSSH user drives in
seconds:

1. **Import from `~/.ssh/config`** — parse Host blocks and auto-fill
   host/user/port/key_path/proxy_command.
2. **Auto-detect keys** under `~/.ssh/` — anything starting `id_`, ending `.pem`, or
   whose first line matches an OpenSSH private-key header. Surfaces filename, mtime, and
   fingerprint when the public half parses.
3. **Check/fix Windows permissions** via `icacls` — sshd-style "too open" private keys,
   with one-click remediation.
4. **Test connect** — opens a real russh session and runs the same auth ladder the app
   uses, so a green test means the workspace will connect.

## `updater.rs` (1,003) — check only

Fetches a remote `manifest.json`, compares versions, emits `update:available`. **No
download or install** on the check path — that needs signing keys.

The manifest URL is `settings.updates.manifest_url`, switchable without recompiling, and
a failed fetch is **silent and never blocks startup**.

Fetch is native `ureq` + rustls in-process since v0.2.3. Before that it shelled out to
PowerShell, which broke on machines where `powershell.exe` is intercepted by AV/EDR or
running in Constrained Language Mode — the parser-error output (the script source echoed
back) surfaced as the user-facing error message.

## Invariants

- **Rule #7** — `settings.json` is written tmp + fsync + rename, like every other
  config file, and refuses to write when its load poisoned.
- **Rule #3** — every wizard is a command builder; argv arrays only.
- **Rule #2** — a key passphrase entered in the connect wizard stays in memory.
- macOS arms are written in the **same commit** as the Windows arm
  (CLAUDE.md § Platforms). `build-macos-intel.yml` is the only job that compiles them.

## Gotchas

- WSL left the wizard on 2026-08-19. Existing `Connection::Wsl` workspaces still load
  and run (and `lib.rs::migrate_wsl_workspaces` rewrites them to `Local` on load), but
  nothing creates new ones. `wsl_exec` survives because `worktrees.rs` still dispatches
  on it.
- `CREATE_NO_WINDOW` is not optional — a missing flag is a console flash, not an error,
  so it fails review rather than CI.
- `hidden_cmd` sets `kill_on_drop(true)`: tokio does NOT kill a child when a timeout
  drops the `output()` future, and an orphaned `install.ps1` kept
  `~\.claude\downloads\claude-<ver>.exe` locked, so every retry of InstallClaudeCode
  failed with "being used by another process". Same reasoning as `claude_usage.rs`.
- `resolve_claude_binary` (Phase 87) also tries `claude.cmd` on Windows: `which` only
  appends PATHEXT to the name it is handed, so `which("claude.exe")` cannot see an
  npm-global install, which is a `.cmd` shim. `probe_tool` had always known both
  spellings; the one-shot `claude -p` callers (`claude_usage`, `sessions_overview`)
  resolve through here and did not. A `.cmd` is spawned via `cmd.exe /c` by std with
  its own escaping, which is why those callers keep their prompt free of `"` and `%`.
- InstallClaudeCode (both arms) succeeds even when the installer exits non-zero, **if**
  a working `claude` binary is already on disk — the downloads dir is shared with
  claude's own auto-updater, so a locked file can fail the installer on a machine
  where the goal is already met.

## Read the source when

You need the exact settings schema (or its serde field names — the frontend mirrors
them), a specific install step's command line, or the `~/.ssh/config` parser's handling
of a directive. The user-facing config surface is documented in `docs/CONFIG.md`.
