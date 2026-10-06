---
vault: index
---

# The vault

Hand-written explanations of this codebase, meant to be read **instead of** the
source. The tree is ~99k lines; this directory is ~3k. If you are about to open a
`.rs` or `.tsx` file to find out what something does, read the vault file that
covers it first.

`.vault-lock.json` records a sha256 of every covered source file.
`scripts/vault-check.mjs` compares those hashes against the tree and fails CI when
they drift, so a vault file cannot quietly become a lie the way `docs/MODULES.md`
did. Details in `docs/CONTRIBUTING.md` § Updating the vault.

## Routing table

**Rust backend** (`app/src-tauri/`)

| Read this | When you want |
|---|---|
| [backend-core.md](backend-core.md) | `AppState`, the workspace model, `workspaces.json` persistence, `spawn_local_pty` / `spawn_ssh`, the layout tree, zellij/tmux wrappers, `run()` |
| [backend-rpc.md](backend-rpc.md) | the named-pipe / Unix-socket JSON-RPC endpoint, the full method catalog, hook→toast, the MCP bridge |
| [backend-sessions.md](backend-sessions.md) | UTF-8 chunk reassembly, the RTL bidi filter, OSC notifications, remote log sync, reverse-tunnel sticky ports |
| [backend-remote.md](backend-remote.md) | CLI bootstrap onto a server, the bootstrap storm guard, the provisioning wizard, add-ons, mobile pairing |
| [backend-wizards.md](backend-wizards.md) | settings.json, the local install engine, shell/key detection, the update checker |
| [backend-panes.md](backend-panes.md) | Diff / File Manager / Browser panes, git worktrees, the workspaces three-way merge, notes, tickets, skills, STT, fonts, tray |
| [backend-claude.md](backend-claude.md) | session summaries, `/usage` quota, transcript mirroring, local Insights |
| [crates.md](crates.md) | the eight `ymux-*` crates and why each thing lives where it does |

**Frontend** (`app/src/`, SolidJS)

| Read this | When you want |
|---|---|
| [frontend-shell.md](frontend-shell.md) | `App.tsx` and its ~50 signals, the backend event subscriptions, sidebar, layout tree, `PaneView`, the drawer/float/fullscreen panel chrome |
| [frontend-panes.md](frontend-panes.md) | Browser, File Manager, the Monitor and its Analytics / Claude-cost tabs, Ports, Diff, tickets, notifications, the feed, transfers, add-ons |
| [frontend-flows.md](frontend-flows.md) | the setup wizard mode tree, the create/edit modals, SSH form sharing, SettingsModal |
| [frontend-lib.md](frontend-lib.md) | the xterm.js wrapper, the four RTL modules, ts-rs type mirrors, logger, i18n, shortcuts, the Claude price table |

**Everything else**

| Read this | When you want |
|---|---|
| [server-go.md](server-go.md) | the `ymux-server` Go daemon on the remote — its 12 packages, the leaf-`core` rule, the frame contract, and why the two Linux blobs are committed |
| [cli.md](cli.md) | the `ymux` CLI: transport selection, the verb families, hooks, the port watcher, session-meta |
| [build-glue.md](build-glue.md) | how a binary gets built, what is generated vs written, and the guard on each |

## Coverage

95% of the tracked `.rs` / `.ts` / `.tsx` / `.go` / `.mjs` lines are covered by a vault
file. `node scripts/vault-check.mjs` prints the current number.

Code no vault claims now fails the gate. The remainder — generated SDK output,
generated ts-rs bindings, test files, a vite shim, a CSS harness — is listed in the
`unowned:` frontmatter of a vault page, each entry with a reason.

Test files are deliberately unowned — a test edit should not trip the freshness gate,
and where the tests are the specification (the RTL modules) the vault says to read them
rather than paraphrasing.

## What the vault is not

- **Not design docs.** Why the system is shaped this way lives in
  `docs/ARCHITECTURE.md`, `docs/PROTOCOLS.md`, `docs/DECISIONS.md`. The vault
  covers what the code *does*, right now, at these paths.
- **Not exhaustive.** Each file ends with a "read the source when" section naming
  the questions it deliberately does not answer. Trust that list.
- **Not a substitute for `git log`.** History lookups still go through
  `PROGRESS.txt` and `git log --all --grep`, per CLAUDE.md.
