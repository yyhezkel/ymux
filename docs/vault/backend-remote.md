---
vault: backend-remote
covers:
  - app/src-tauri/src/remote_bootstrap.rs
  - app/src-tauri/src/bootstrap_guard.rs
  - app/src-tauri/src/provisioning.rs
  - app/src-tauri/src/addons.rs
  - app/src-tauri/src/pairing.rs
---

# Getting ymux onto a remote host

Everything the desktop does *to* a server: put the CLI there, provision a fresh box,
install add-ons, expose it to a phone. ~3,400 lines, and the interesting parts are the
coordination, not the commands.

## `remote_bootstrap.rs` (83) — a shim, deliberately

The connect-time deploy of `ymux-linux-{x64,arm64}` to `~/.ymux/bin/`. The actual russh
+ SFTP logic moved to the **`ymux-bootstrap` crate** (Phase 51.D); this module only does
the Tauri-specific bit — resolving the manifest and the bundled binary — and re-exports
`BootstrapStatus` and `PATH_RC_SNIPPET` so existing `crate::remote_bootstrap::*`
callsites still resolve.

The payloads are **embedded with `include_bytes!`, not resolved via
`BaseDirectory::Resource`.** Resource lookup works for an installed bundle (where
`resources/` sits next to the exe) but not for the standalone debug exe Yossi runs by
copying just `app.exe` into a folder — there the read failed with "os error 3" as a
silent WARN. Same reason `resources/ymux-server-linux-*` are committed blobs.

<!-- Phase 91.F: the pane literal `provisioning.rs` builds also sets
`diff_cwd: None` (a new `LayoutNode::Pane` field — see crates.md); mechanical,
no behavioural change here. -->

## `bootstrap_guard.rs` (237) — why connects stopped storming

`spawn_ssh` calls `bootstrap` **once per pane**, and panes reconnect together — a
network drop returns every pane at once. With a CLI/manifest hash mismatch that meant N
concurrent 2.4 MB SFTP uploads to one host, retried on every subsequent connect, forever.
Under load they all timed out, which killed the transport before `tmux new-session` was
ever sent, which triggered another reconnect. That loop is what left Yossi unable to
reach his server at all.

Two mechanisms, and **explicitly not a third**:

- A **per-host async lock** — one bootstrap per host at a time; the others reuse its
  outcome.
- A **short negative cache** keyed by `(host, wanted sha256)` — a failed upload is not
  re-attempted for a few minutes.
- **No retry/backoff**, because there was never a retry loop to back off: the storm was
  one attempt per connect, and connects are driven by the user and the reconnect driver.
  Adding backoff would have addressed a mechanism that does not exist.

It also records whether the remote CLI actually matches the embedded binary. Lives on
`AppState.bootstrap_guard`.

## `provisioning.rs` (1,822) — the server wizard

Takes a fresh box beyond "create a workspace": inspect the remote (OS, package manager,
disk), then apply a profile of steps — update, install basics, create user, deploy SSH
key, harden sshd, install language runtimes, install Claude Code, run `ymux setup-hooks`.

- Progress streams to the frontend as `provisioning:progress` events, which is what
  makes the wizard's live log feel native.
- **One russh `client::Handle` per run**, reused across every step's exec channel.
- **Failures do not abort the run.** Each step ends `pending | running | done | failed`;
  the wizard offers retry/skip per step and a checkpoint is saved after every state
  change, so a second pass resumes.
- Profiles persist in `%APPDATA%\ymux\provisioning-profiles.json`; original credentials
  in `provisioning-secrets.json`.

**This is the file where Rule #3 (argv arrays, never string concatenation) is enforced
most often.** It and `local_setup.rs` are the two paths that build remote commands from
user input.

## `addons.rs` (1,030) — the add-on manager

Detect / install / uninstall / update ymux add-ons over a workspace's existing SSH
session. The manifest schema and the built-in registry live in the **`ymux-addons`
crate**; this module is the desktop side that runs the actions and exposes the `addon_*`
Tauri commands behind the Settings → Add-ons table and the wizards.

Built-ins dispatch to the **remote shell / remote CLI**, not back into the Rust
bootstrap, so the connect-time bootstrap stays the single owner of the CLI + tmux.conf
upload — those appear here as detect-only / "managed on connect". Hooks are fully
manageable through the remote `ymux setup-hooks`; `insights` ships its own
`ymux insights install` subcommand.

`insights_fetch` lives here and is the **remote-vs-local routing decision** for the
Insights panel — the frontend does not make it. Shared helpers `exec`, `exec_stdin`,
`pick_handle`, `remote_home` are used by `pairing.rs` too.

`src/addons.rs` is also where `include_bytes!` pulls in
`resources/ymux-server-linux-{x64,arm64}`. **Those blobs are committed and rebaked by
hand** — a Go change that skips the rebake ships the old server to every remote, which
is why `ci-windows.yml` has a gate for exactly that.

## `pairing.rs` (231) — mobile

Drives the `nginx-proxy` add-on install (domain + Cloudflare token) and the daemon's
`/api/pairing/*` endpoints, curled over the workspace SSH session the same way
`insights_fetch` is. Returns JSON strings the Mobile tab parses — no ts-rs bindings.

**Rule #2:** the Cloudflare token is `Zeroize`d desktop-side once the install returns.
It persists remote-side only in `/etc/ymux/cloudflare.ini` (mode 600, root) because
certbot's auto-renew needs it. The domain marker lives at
`~/.ymux/server/mobile-domain` — note `server`, not `insights`; Phase 77 renamed that
directory and migrates it in place on first 2.0 boot.

## Invariants

- **Rule #3** — argv arrays. Anything interpolated into a POSIX script goes through
  `ymux_core::shell_quote`.
- **Rule #2** — no passphrase or sudo password in plaintext at rest; DPAPI when
  persistence is genuinely needed.
- Bootstrap is idempotent and hash-gated: compare the remote binary's sha256 against
  the manifest before uploading anything.

## Read the source when

You need a provisioning step's exact command, the add-on manifest schema (that is in
`ymux-addons` — see `crates.md`), or the pairing endpoint shapes. Manual server build
commands are in `docs/ymux-server/README.md` § Build.
