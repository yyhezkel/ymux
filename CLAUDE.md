# CLAUDE.md

This file is read at the start of every Claude session working on ymux. Keep it small. Deep references live in `docs/`.

## Where to start

- **`docs/vault/INDEX.md` — the vault. Read the page that covers an area BEFORE opening its source.** ~3k lines of prose standing in for ~90k lines of code, covering 96% of the tree. It is enforced: `scripts/vault-check.mjs` hashes every covered file and ci-windows fails when the prose and the code drift. When you change covered code, update the covering page in the same commit and run `node scripts/vault-check.mjs --write` — `[vault-skip]` in the PR title is the escape hatch. Details: `docs/CONTRIBUTING.md` § Updating the vault.
- `docs/ARCHITECTURE.md` — system map
- `docs/CONTRIBUTING.md` — recipes, style, commit conventions
- `docs/RELEASING.md` — version cut process
- `docs/DECISIONS.md` — **READ FIRST**: open threads + decisions log
- `docs/ZELLIJ.md` — zellij's CLI + config surface as our 0.44.3 binary reports it; read before adding a verb, don't guess from zellij.dev
- `docs/BRIEF.md` — the agent-brief wire format (`[ymux-brief]`), the Queue panel and the Briefing card; read before touching the brief parser or its surfaces
- `docs/COMPETITIVE-SCAN.md` — survey of the 8 GitHub projects named `winmux`, ideas inventory, Secrets Vault design (pre-rename doc, kept verbatim: it is about *other people's* repos and is what motivated the move to YMUX)
- `docs/IDEAS-RANKING.md` — decision table for the ideas inventory (MUST / SHOULD / COULD)

## Session workflow (memory arch)

- **`git fetch` FIRST, every session — and again before every merge/push.** ymux is worked on from several machines and servers at once (Yossi's box, collaborators, cloud sessions), so `origin/main` moves while you work. Start with `git fetch origin && git status`; if `main` is behind, pull before touching anything. Re-fetch before any merge or push — a plan built on a stale tree is worse than no plan. This is not theoretical: on 2026-08-09 a collaborator's macOS port landed 5 commits on `origin/main` mid-session and was only noticed by accident at the end. Never assume the tree you started with is current.
- **PROGRESS.txt** — append after every significant change (timestamp, task, files, result). NEVER overwrite. Too big → rename `PROGRESS_OLD_<date>.txt`, start fresh.
- **FOLLOWUPS.md / BACKLOG.md** — read both at session start. Open P0/P1 in FOLLOWUPS → surface before new work. Out-of-scope bug found in passing → one line to FOLLOWUPS (P0-P3, file:line, repro). Out-of-scope idea / mock-stub debt → BACKLOG. Never silently leave broken state. **FOLLOWUPS.md holds OPEN items only** — when one closes, move it to `FOLLOWUPS-ARCHIVE.md` with its full text plus a note saying how it closed; do not delete it and do not leave it in place as `[x]`. That archive is NOT read at session start, so nothing still needing action may be parked there. **Cite an entry by its text, never by `FOLLOWUPS.md:NN`** — the line numbers shift every time an item is added or archived, and two entries had already rotted into pointing at the wrong lines.
- **Past-work lookup order** — before re-investigating: 1) `PROGRESS.txt` + `PROGRESS_OLD_*` 2) `FOLLOWUPS-ARCHIVE.md` (a closed entry keeps the root cause, which is often not the one first written down) 3) `git log --all --oneline --grep=<keyword>` 4) memory search 5) `docs/*.md` + this file.
- **"Verified" = real run, not compile.** Build/type-check pass = syntax only. Say "compiles, untested" until run live.
- **Sync this file with code.** New port/service/endpoint/schema/deploy step → update the matching doc in the same commit. Same for the vault page covering the code you touched — CI checks that one for you (Rule #18).

## Decisions & open threads

When an idea or design question comes up:

1. If it's resolved in the same message, do it — no log entry needed.
2. If a decision is made but action is deferred, log it under **Decided** in `docs/DECISIONS.md` with the outcome and a deferral note.
3. If it stays open (user hasn't decided, blocked on input, flagged for later), log it under **Open** in `docs/DECISIONS.md` with options and current state.

When starting a new session, scan the **Open** section. Don't let threads die silently — if something's been pending a while, surface it.

## Pinned deps

- `tauri = "=2.10.3"` with `features = ["unstable"]` (app/src-tauri/Cargo.toml). The unstable feature gates `Window::add_child`, which Phase 53 uses to mount per-workspace browser webviews inside the main window. Bumping tauri requires verifying `Window::add_child`'s signature hasn't changed and the multi-webview shape still compiles. Push the bump and let CI type-check it (Rule #17 — no local `cargo check`), then smoke-test the workspace Browser window (sidebar 🌐 → open / hide via a modal / navigate / close).

## Off-limits paths

- `backup-phase23-*` folders — never touch
- Repo-root `.bat` / `.ps1` helper scripts the user maintains — never touch
- `release_notes.md` — do not commit
- `remote-manifest.json` timestamp churn — discard unless the SHA actually changed
- Linux CLI binary rebakes itself on release builds (CARGO_PKG_VERSION) — expected, commit as part of the release

## Release safety

- Never push a half-done release. If a step fails for a real reason, stop and report.
- Build through the Tauri CLI, never plain `cargo build --release` — see Rule #13.
- `app.exe` running on the user's machine causes `os error 32` during NSIS bundler cleanup — cosmetic; the binary + bundles produced fine. A running `app.exe` also blocks the link step outright (`failed to remove file … Access is denied`); ask Yossi to close it rather than retrying.
- v0.2.3+: updater uses native `ureq` + `rustls` (no more PowerShell).

## Platforms

Windows and macOS (Intel, 13+) are both supported desktop targets; the real
work happens on remote Linux. macOS lives on `main` behind
`cfg(target_os = ...)` — there is no separate branch. It had one until
Phase 82 (2026-08-20), and it drifted 65 commits behind in two months,
which is the reason there isn't one now. When you add a local-machine
feature (shell spawn, a wizard step, a path join, a file location),
write the non-Windows arm in the same commit — `build-macos-intel.yml` is
the only thing that will tell you, and only if it is wired to run.

Local-pane **persistence is the one place the two genuinely differ**, and
it is deliberate: Windows wraps a native pane in zellij, macOS in tmux.
`spawn_local_pty` takes a single `persist_session: Option<String>` and
picks the backend with `cfg`; `pane_kill_session` mirrors it with
`KillTarget::Zellij` / `KillTarget::LocalUnix`. The setup wizard shows a
persistence group only on macOS — on Windows zellij is an ordinary tool
row, so a group would offer the same install twice. WSL left that wizard
on 2026-08-19: existing `Connection::Wsl` workspaces still load and run,
nothing creates new ones.

Known not to work on macOS: in-app update, code signing / notarization.
See FOLLOWUPS.

## The winmux → YMUX rename (2026-08-18)

The app was `winmux` until 0.4.5. **A `winmux` you find in the code is almost
certainly load-bearing, not a leftover** — every one is a compat shim for an
existing install or an already-provisioned remote, and each is commented as
such. Do not "finish the rename" by deleting them; the removal is scheduled
(FOLLOWUPS P1, one release after 0.5.0) and has to happen as a set.

- **Still emits the legacy wire tag.** The handshake sends `WINMUX-CHALLENGE`
  on purpose: a pre-rename remote CLI does a literal prefix match. Both ends
  read *and mirror* either dialect (`CHALLENGE_TAG` in `crates/ymux-tunnel`,
  `challengeTag` in `server/internal/chat/chat_hookrpc.go`) — flip both together.
- **Migrations that run once, on upgrade:** `%APPDATA%\winmux` → `ymux`
  (`ymux-core::config_dir`), `~/.winmux` → `~/.ymux` (bootstrap + CLI), and
  the daemon's data dir. These stay long after the rest go.
- **`"winmux"` does not contain `"ymux"`.** Two substring checks broke on
  exactly that and were fixed; if you add another, match both spellings.
- History is deliberately NOT renamed: `PROGRESS.txt`, the Decided log in
  `docs/DECISIONS.md`, and `docs/COMPETITIVE-SCAN.md` (that one is about
  *other people's* repos named winmux — renaming it would make it a lie).

## CI (GitHub Actions)

- `ci-windows.yml` — cargo test + tsc + vite + the full Go server gate on
  every push/PR to `main`, as **three parallel jobs** (~3.5 min wall-clock
  warm, was ~6 serial): `frontend` (parse-check, vault gate, tsc, `npm
  test`, vite build), `rust` (stage CLI + `cargo test`, windows-latest,
  `shared-key: windows-dev`), and `go` — which runs on **ubuntu-latest** on
  purpose: `go vet` + `go test` there exercise linux, the platform the
  server actually ships on, **plus the linux/amd64 + linux/arm64
  cross-build**, because those two binaries are the only server artifact
  that ever ships (`include_bytes!` in `src/addons.rs`) and nothing else in
  CI compiles them. The `go` job also runs `sdk-gen/ci-check.mjs`
  (SDK/OpenAPI drift) and fails the build if `server/**` changed without
  rebaking `resources/ymux-server-linux-{x64,arm64}` — see "Rebaking the
  server" below. Bookkeeping-only diffs (`PROGRESS*`, `FOLLOWUPS*`,
  `BACKLOG.md`, `README*`, `.claude/**`) skip CI via `paths-ignore`;
  `docs/**` deliberately still triggers it, for the vault gate.
- `build-windows.yml` — installers + exe on `workflow_dispatch` or a `v*` tag; enforces Rule #13 by asserting the asset hash is embedded, and Rule #2's spirit by asserting the `$USERNAME` scrub landed. A tag gets MSI **and** NSIS (the published `manifest.json` advertises both); a `workflow_dispatch` gets NSIS only. Publishing stays manual (`docs/RELEASING.md`).
- `warm-rust-cache.yml` — **the reason release builds are ~7 min and not ~15.** GitHub scopes cache *reads* to the current branch plus the default branch, and `build-windows.yml` never runs on `main`, so a build dispatched from a fresh branch used to compile all ~740 lockfile packages from scratch (measured: 813s cold vs 420s warm, same workflow, same week). This job keeps that release-profile cache alive on `main` under `shared-key: windows-release`, on Cargo.lock changes and twice weekly. If release builds suddenly go slow again, check this workflow first.
- `build-macos-intel.yml` — the macOS build (x64 + arm64 matrix), on `workflow_dispatch` or push/PR to `main`. Since Phase 82 it runs the same gates ci-windows does (cargo test + tsc + frontend build), because it is the **only** job that compiles the `cfg(target_os = "macos")` / `cfg(not(windows))` arms at all. It also stages a native `resources/ymux-cli` — the mac counterpart of `build:linux-cli`, which is PowerShell-only. **On a PR it runs those gates ONLY** (~2-4 min): the `.app` bundle, embedded-frontend assert, dmg, signing and notarisation run on push to `main` and `workflow_dispatch` — so a bundle-stage break surfaces on the merge run, not the PR.
- Steps that shell out to Windows PowerShell need `shell: cmd`. The default `run:` shell is pwsh, which rewrites `PSModulePath` for its children, so the 5.1 instance `build:linux-cli` spawns loses `Get-FileHash`.
- `npm run build:linux-cli` must run before any cargo step on a fresh checkout — it stages the gitignored `ymux-cli.exe` the Tauri build script requires. It stages by **sha256, not mtime**: everything in `resources/` is pulled into the app crate with `include_bytes!`/`include_str!`, and Cargo's staleness check is mtime-based, so re-copying a byte-identical file used to force a full rebuild of the 9.4k-line lib.
- **A `.ps1` in this repo must keep non-ASCII out of code lines.** Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI, so an em-dash inside a string literal decodes to a smart quote that the tokenizer accepts as a string delimiter — the file then fails to parse with a cascade of errors pointing at innocent lines. Em-dashes in `#` comments are fine (already all over `build-linux-cli.ps1`); inside `"..."` they are a build-breaker.
- **Action pins are deliberate, and `.github/dependabot.yml` watches them.** All four workflows run node24-runtime majors (`checkout@v7`, `setup-node@v7`, `setup-go@v7`, `upload-artifact@v7`, `cache@v6`) on `node-version: 24` — Node 20 hit EOL 2026-04-30 and the old majors were being force-migrated by the runner. `actions/*` stay on floating majors; the two third-party actions are SHA-pinned. **`dtolnay/rust-toolchain` is pinned to the SHA of the `stable` BRANCH**, and its only tag (`v1`) is `master`, where `toolchain` is a required input with no default — a dependabot bump onto it switches branches silently, so every call site passes `toolchain: stable` explicitly and a bump PR touching it needs the SHA checked by hand. Every workflow is `permissions: contents: read`; nothing here writes to the repo.

### Rebaking the server

`app/src-tauri/resources/ymux-server-linux-{x64,arm64}` are **committed
blobs**, not build output. The desktop `include_bytes!`s them, so a Go change
that skips the rebake is green in every job and ships the OLD server to every
remote. `ci-windows.yml` now fails on exactly that. To rebake without a local
Go toolchain (Rule #17): download the `ymux-server-linux` artifact from the
ci-windows run, drop both files into `app/src-tauri/resources/`, and commit
them in the same change as the Go source. Manual build commands live in
`docs/ymux-server/README.md` § Build.

## Communication

- User: Yossi (`yyhezkel@gmail.com`). Prefers Hebrew, terse, action-oriented replies.
- Phase numbering: stable in commit history. Sub-numbers (`23.J`) for follow-ups. No reuse.
- Commit format per `docs/CONTRIBUTING.md`.

## Absolute Rules — Do Not Violate

1. **Never log PTY input or output content.** Only metadata (pane ID, byte counts, error kinds). User shell content is private.
2. **Never store SSH passphrases or sudo passwords in plaintext at rest.** Use DPAPI (`CryptProtectData`) when persistence is necessary; otherwise keep in memory only.
3. **Never build shell commands by string concatenation.** Use `Command::new(...).arg(...)` arrays. The agent and provisioning paths are the only places this is enforced repeatedly — don't drift from it.
4. **No `unwrap()` or `expect()` in non-test Rust** outside the `main()` boot path. Use `?` or `.map_err(...)` and surface a clean error.
5. **No `any` in TypeScript.** Use `unknown` and narrow, or define a type. Tauri command return types are always explicit.
6. **All Tauri commands return `Result<_, String>`.** The frontend handles the error; don't `panic!`.
7. **Workspace persistence is atomic.** Write to `<file>.tmp` then `rename` to the target. Never partial writes to `workspaces.json` / `settings.json`.
8. **Never expose the tunnel HMAC token to logs.** Treat it like a password.
9. **The unified logger (`ymux_core::log_debug/info/warn/error(tag, msg)`) is user-visible** (lands in `%APPDATA%\ymux\debug.log`, format `[ts] [LEVEL] [TAG] msg`; threshold from Settings → Logs). Rust uses `log_*` with a component tag; frontend uses `createLogger(tag)` from `app/src/logger.ts` (never raw `console.*`); Go server uses `internal/logging.New("SRV:X")`; CLI hooks use `hook_log(level, msg)`. `dlog()`/`dlog_tag()` are legacy info-level shims — don't add new callers. `tracing::*` stays engineer-only (dev builds). Pick by audience.
10. **Don't push a half-done release.** If any step in RELEASING.md fails for a real reason (not the `os error 32` NSIS cleanup false-alarm), stop and report.
11. **Don't touch `backup-phase23-*/` or repo-root `.bat` / `.ps1` helper scripts.** Don't commit `release_notes.md`.
12. **`remote-manifest.json` timestamp churn is cosmetic.** Discard unless the embedded SHA actually changed.
13. **Never build the app with plain `cargo build --release`.** It links cleanly and produces a binary that loads `devUrl` (`localhost:1420`) at startup — every window is an `ERR_CONNECTION_REFUSED` page on any machine without a dev server, and the Rust log gives no hint (boot stops silently after `rpc server spawned`). `tauri-build` only embeds `frontendDist` when the build runs through the Tauri CLI. Use `npm run tauri build -- --no-bundle` from `app/`, or `app/scripts/build-release.ps1` for a real cut. To check a binary: the current `app/dist/assets/index-<hash>.js` filename must appear inside `app.exe` (`localhost:1420` appears in both kinds and proves nothing). Details in `docs/CONTRIBUTING.md` → "Building a runnable exe".
14. **A build that compiles is not a build that runs.** Launch it and confirm the UI comes up before saying "built" or "verified" — this is Rule #13's parent, and the reason that one shipped twice.
15. **Never push without fetching first, and never force-push a shared branch.** Several machines push to this repo. `git fetch origin` immediately before any merge or push; if `origin/main` moved, integrate and re-run the checks before pushing. Rewriting published history costs a collaborator their work.
16. **One session, one worktree — never two sessions in the same working copy.** Dispatched code tasks use `isolation: "worktree"`. The main checkout stays on `main` and clean, for reading and integration only. Commit before the session ends and remove the worktree by hand: git only auto-cleans worktrees that are *unchanged*, which is why 13 of them accumulated by 2026-08-03. Weekly: `git worktree prune` + delete branches with `ahead=0`. Two sessions sharing one tree is how commits land on the wrong branch.
17. **Builds and tests run on CI only — never locally.** No `cargo build`, `cargo check`, `cargo test`, `npm run build`, `vite build`, `tsc`, or `npm run tauri build` on a dev box or in an agent session. Push the branch and let GitHub Actions do it (see the CI section above); read the result with `gh run watch` / `gh run view <id> --log-failed`. Local runs are banned because they occupy the machine for many minutes, they cannot reproduce the real matrix (a Linux box type-checks none of the `cfg(windows)` / `cfg(target_os = "macos")` code that actually ships), and a fresh checkout can't build at all without the gitignored `ymux-cli.exe` (or `ymux-cli` on mac). This overrides any "verify before you commit" instinct: the verification step IS the CI run. Rule #14 still holds — a green CI run is "compiles, untested" until it runs live on a real machine.
18. **The vault is part of the change, not a follow-up.** `docs/vault/*.md` is what an agent reads instead of ~90k lines of source, so a page that has gone stale is worse than no page — whoever trusts it plans against a fiction and never finds out. That is not hypothetical: `docs/MODULES.md` was the same idea without a gate and ended up claiming `lib.rs` was ~1760 lines when it had reached 12,475. Before you merge anything from a branch or a separate worktree, run `node scripts/vault-check.mjs` — it hashes every covered source file against `docs/vault/.vault-lock.json` and names what drifted. Fix the prose, re-stamp with `--write`, commit both together. ci-windows enforces it on every PR and additionally requires the owning `.md` to appear in the diff, so re-stamping alone does not pass. `[vault-skip]` in the PR title is the deliberate, logged escape hatch for a change that genuinely does not affect any explanation.
19. **Before EVERY push: update the vault for the whole worktree-vs-`main` diff — not just your last commit.** ci-windows runs `vault-check` over the full diff a PR carries, so a worktree that has drifted from `main` across several commits fails the gate even when the final commit touched nothing covered. The sweep is: `git fetch origin` → `git diff --name-only origin/main...HEAD` → for every covered file in that list, update the page in `docs/vault/` that covers it → `node scripts/vault-check.mjs --write` → commit the prose **and** `.vault-lock.json` together → push. Skipping this is the single most common way this repo's CI goes red. `[vault-skip]` in the PR title stays the only exception, and only when nothing in the diff changes an explanation.
