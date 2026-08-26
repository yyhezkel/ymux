# Backlog

Out-of-scope feature ideas, intentional mock/stub code, deferred refactors. The `plan-to-backlog.sh` hook auto-appends bullets from approved plans' `## Out of scope` sections.

Format:

```
- [ ] <YYYY-MM-DD> | from-plan:<plan-name> | <one-line idea>
- [ ] <YYYY-MM-DD> | manual                 | <one-line idea>
```

## Open

- [ ] 2026-08-23 | manual                 | **P2** - port-watch: replace the /proc/net/tcp{,6} read (2,589 lines on Yossi's server, every tick) with a netlink `sock_diag` INET_DIAG query filtered to `idiag_states = 1<<TCP_LISTEN` - returns only listeners (~50 rows). Phase 86.A fixed the user-space half (skip parse when bytes unchanged, 1s tick); the kernel still renders the whole table each read. Raw netlink, no crate in the tree yet. `app/src-tauri/cli/src/port_watch.rs`.
- [ ] 2026-08-23 | manual                 | **P3** - hooks: every Claude hook is its own process and pays TCP + forwarded SSH channel + HMAC per call (cli/src/main.rs `rpc_call`). Pooling needs a resident agent on the remote (the Go daemon could proxy: hook -> unix socket -> one long-lived tunnel connection). Only worth it if per-call latency shows up; Phase 86.E already cut pre-tool-use from 3 connections to 1.
- [ ] 2026-08-23 | manual                 | **P3** - FIVE byte-formatters, one job. `fmtBytes` now lives in `app/src/insightsFmt.ts` (extracted from InsightsWindow so the Analytics tab could share it), but `FileEditor.tsx:346` and `FileManagerPane.tsx:448` each carry a private `fmtSize`, and `SettingsModal.tsx:74` a private `formatBytes`. They are not identical - the rounding and the unit table differ - so "the same size" reads differently depending on which panel you are looking at. Collapse them onto `insightsFmt.fmtBytes` (and rename the module to something less Monitor-specific when doing it). Mechanical, touches four files, wants its own commit rather than riding along with a feature.

- [x] 2026-08-03 | manual | **P2** — `typescript` is not a project dependency; there is no reproducible type-check — **DONE 2026-08-19.** `typescript@~5.6.2` is in `app/package.json` devDependencies and `npm run typecheck` now exists. `ci-windows.yml` had been papering over this with `npm i --no-save typescript@5`, which also meant CI type-checked against a different minor than developers ran locally; that line is gone.

  Found during the Phase 2 rebase verification. `app/node_modules` was
  completely empty (0 entries) in the main checkout, so no frontend
  verification had been possible at all until `npm ci` was run. Worse:
  even after a clean install, `typescript` is absent from
  `app/package.json` — there is no `tsc` binary and no `typecheck`
  script, despite `app/tsconfig.json` setting `"strict": true` and
  `"noEmit": true`.

  The Phase 2 type-checks passed, but only against a compiler installed
  outside the lockfile — that result is not reproducible on another
  machine or in CI.

  `vite build` is not a substitute: esbuild strips types without
  checking them, so type errors ship silently today.

  Fix: `npm i -D typescript@<pin>` and add `"typecheck": "tsc --noEmit"`
  to `app/package.json` scripts. Left undone deliberately — it changes
  package.json + package-lock.json, which is out of scope for a
  worktree-cleanup pass.

- [ ] 2026-08-03 | manual | Command palette polish — cherry-pick `5413ef9` from `design-pass-01` when the palette outgrows ~23 commands

  Parked, not dropped. `design-pass-01` is 5 commits; 4 already landed on `main`
  under different SHAs (`7fbc7fe` docs/SVG, `b3c2965` logical properties,
  `cfd50e8` welcome screen, `56bd57d` --wmx-* tokens). `5413ef9` is the only
  real delta and it is pure UX polish, deferred on purpose:

  - fzy-style fuzzy scorer + `<mark>` highlighting + score ranking (replaces
    the `includes()` substring match at `app/src/CommandPalette.tsx:38`)
  - category grouping with sticky headers, derived from the dotted command-id
    prefix (`pane.*`, `ssh.*`) — no churn on the command definitions
  - per-category icons + right-aligned keybinding hints
  - Recent section (localStorage, last 5) when the query is empty
  - footer with nav/run hints + live result count
  - i18n: `cmd.cat.*` + palette hints/count across he/en/ar/ru

  Why deferred: the palette holds ~23 commands. Substring match is adequate at
  that size; a fuzzy scorer earns its keep at 100+. The one genuine bug the
  commit fixed (references to `--w-text-primary` / `--w-text-secondary`, which
  were never defined) is already gone from `main` — the `--wmx-*` token system
  superseded it, 0 references remain in `App.css`.

  Trigger to revisit: the command count roughly quadruples, or discoverability
  complaints come in. The categories + keybinding hints (~40 of the 246 lines)
  are the parts worth having early — cheap to rewrite fresh on `main` if only
  those are wanted.

  Cost when picked up: `CommandPalette.tsx` and the 4 i18n files apply cleanly
  (`main` has not touched them since the fork). The 109 lines in `App.css` will
  conflict — `main` grew that file by ~2355 lines, including the whole new
  design-token system. Resolve by hand.

  Keep branch `design-pass-01` alive as the archive (like
  `browser-dev-mode-tickets`). Do not merge it wholesale — 4/5 of it is already
  in `main` and a full rebase would replay landed work.

- [ ] 2026-08-23 | manual | Extend `.github/dependabot.yml` to the `npm` and `cargo` ecosystems — enabled for `github-actions` only so far, because turning on the other two opens a wave of PRs against dependency trees nobody has reviewed. `app/package-lock.json` also carries a stale root `version` (0.4.5 vs package.json 0.5.0) worth fixing in the same pass.

## workspace_fs — one exec/filesystem layer instead of six copies

Found while putting tickets on the remote (2026-08-12). There is no
abstraction for "do this in the workspace's filesystem". The same russh
exec loop is hand-rolled **six** times — `addons.rs:71`, `claude_log.rs:113`
(whose own comment admits it mirrors two others), `claude_summary.rs:60`,
`provisioning.rs:1329`, `updater.rs:448`, `file_manager.rs:1464` — and the
SSH handle picker **four** times (`addons.rs:38`, `claude_log.rs:87`,
`claude_summary.rs:49`, `file_manager.rs:525`). `open_sftp` exists twice.

Shape if picked up: a module owning (a) the handle/transport pickers,
(b) one `exec(workspace_id, script) -> (code, stdout)` dispatching
Local -> `Command`, Wsl -> `local_setup::wsl_exec`, Ssh -> the russh loop,
and (c) the path translation that currently lives only in `tickets.rs`
(`wsl_unc_path` / `wsl_linux_from_unc`).

Deliberately NOT done with the tickets work: Yossi's instruction was to
add tickets without touching what already works, and this touches a lot
of working code. `tickets.rs::resolve` is the reference implementation of
the dispatch if someone wants a starting point.

Payoff is real though — it is what would fix the two FOLLOWUPS above
(diff_pane on remote workspaces, addons misclassifying WSL) rather than
patching each one separately.

### Single-instance lock on the config dir (2026-08-23)

The other half of the FOLLOWUPS P1 "two builds share %APPDATA%\ymux and the older
one silently strips newer fields". That entry offered two fixes: (b) a schema
version that refuses to write over a newer file, and (a) refusing to START when
another ymux already holds the config dir. (b) shipped 2026-08-23 and is in
FOLLOWUPS-ARCHIVE.

(a) is still open, and it is the only one of the two that can stop a build that
is ALREADY on disk. A 0.4.x binary never reads the schema version and never
will; nothing added to this tree can reach it. A lock file with the holder's
pid, checked at startup, is the one mechanism that does not require the other
side to cooperate.

Not done now because it is a behaviour change that can lock a user out of their
own app: a stale lock after a crash or a kill must be recoverable without
hand-deleting a file, and that recovery UX is the actual work — the lock itself
is twenty lines. `tauri-plugin-single-instance` is the obvious candidate and is
already noted in a separate P2 about two ymux.exe racing on log rotation, so
these two should be looked at together rather than solved twice.

Also worth deciding at the same time: `settings.rs` has the same atomic-write +
load-poison shape as workspaces.json and got no schema gate, deliberately — the
loss mode there is smaller. If a lock lands, it covers both and the question
goes away.

### `workspace_browser_resize` is dead code (2026-08-23)

`app/src-tauri/src/workspace_browser.rs` still exports `workspace_browser_resize`, and it
has zero call sites in `app/src/`. Every geometry change rides the `workspace_browser_show`
fast path instead, which does the same `set_position` + `set_size` and then `.show()`.

Left in place during Phase 85 rather than deleted mid-change. Either delete it and drop it
from the `generate_handler!` list in `lib.rs`, or give it the one job show can't do —
reposition WITHOUT un-hiding — which is the only reason it would earn its keep.

## Done

### Scrollback inside a locked zellij pane (2026-08-20 — closed same day)

Filed when the full lock shipped with `mouse_mode false`: no keybinds meant no scroll
mode, the mouse was withheld, and xterm.js's own wheel scrolls a normal buffer that sits
behind zellij's alt screen — so a zellij pane could not scroll back at all.

Closed by giving zellij the wheel (`mouse_mode true`) rather than by building the
`dump-screen --full` viewer this entry proposed. That viewer is not needed: the wheel is
the affordance, and it is the same bet the tmux side already took (`set -g mouse on`,
decision O-3 in docs/MOUSE-DEBUG.md).

(Also: this entry was originally appended below the `## Done` heading by mistake, so it
read as done on the day it was filed. It is done now, which is a coincidence.)
