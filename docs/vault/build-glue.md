---
vault: build-glue
covers:
  - app/package.json
  - app/vite.config.ts
  - app/scripts/build-linux-cli.ps1
  - app/scripts/build-release.ps1
  - app/scripts/reset-local-setup.ps1
  - app/src-tauri/Cargo.toml
  - app/src-tauri/build.rs
  - app/src-tauri/tauri.conf.json
  - sdk-gen/ci-check.mjs
  - sdk-gen/emit-specs.mjs
  - sdk-gen/gen-typescript.mjs
  - sdk-gen/gen-kotlin.mjs
  - sdk-gen/package.json
  - scripts/vault-check.mjs
---

# Build glue and code generation

How the pieces become a binary, and what is generated rather than written. The CI
workflows themselves are documented in **CLAUDE.md § CI** — this file covers what those
workflows call.

## The one build rule that has bitten twice

**Never `cargo build --release`.** It links cleanly and produces a binary that loads
`devUrl` (`localhost:1420`) at startup — every window is `ERR_CONNECTION_REFUSED` on any
machine without a dev server, and the Rust log gives no hint (boot stops silently after
`rpc server spawned`). `tauri-build` only embeds `frontendDist` when the build runs
through the Tauri CLI.

Use `npm run tauri build -- --no-bundle` from `app/`, or `app/scripts/build-release.ps1`
for a real cut. **To check a binary:** the current `app/dist/assets/index-<hash>.js`
filename must appear inside `app.exe`. (`localhost:1420` appears in both kinds and proves
nothing.) That is Rule #13; Rule #14 is its parent — a build that compiles is not a build
that runs, so launch it.

## npm scripts (`app/package.json`)

| Script | Does |
|---|---|
| `dev` / `start` | vite dev server on 1420 |
| `build:linux-cli` | **must run before any cargo step on a fresh checkout** |
| `build:frontend` | `vite build` — bundles only, does not type-check |
| `build` | the two above, in order |
| `typecheck` | `tsc --noEmit` — this is the real type check |
| `test` | `node --test` over `src/*.test.ts` |
| `tauri` | the Tauri CLI |

`build:frontend` uses esbuild, which strips types without checking them. It catches bad
imports and dead assets; it is not a substitute for `typecheck`.

`test` is plain `node --experimental-strip-types --test` — there is no vitest and no
jsdom, so a module is only testable from `src/*.test.ts` if it has no browser or Tauri
imports (which is why `shortcuts.ts` owns `DEFAULT_SHORTCUTS` rather than importing it
from `settings.ts`). `tsconfig.json` excludes `src/**/*.test.ts`, so `typecheck` does
not cover these files — `test` is the only thing that reads them. Added in Phase 87
along with the matching ci-windows step: nine test files had existed since Phase 80
with nothing anywhere that ran them.

`"engines": { "node": ">=22" }` — declared 2026-08-23 when CI moved off the deprecated
node20 Actions runtime. The runners are on node 24 (`actions/setup-node@v7`), and the
third-party actions in the workflows are pinned by commit SHA rather than tag, with
dependabot watching them.

Notable deps: `@xterm/xterm` 6 with the fit/webgl/clipboard addons, `bidi-js`,
`dompurify`, `markdown-it`, `@tauri-apps/api` ~2.10.

## `app/scripts/build-linux-cli.ps1`

Cross-builds the CLI and stages three files into `src-tauri/resources/`:
`ymux-cli.exe` (gitignored), `ymux-linux-x64`, and `remote-manifest.json`.

- **Staging is by sha256, not mtime.** Everything in `resources/` is pulled into the app
  crate with `include_bytes!`/`include_str!` and Cargo's staleness check is mtime-based,
  so re-copying a byte-identical file used to force a full rebuild of the 9.4k-line lib.
- The musl cross-build links with `rust-lld` (`src-tauri/.cargo/config.toml`), so a
  Windows runner needs no external linker.
- `remote-manifest.json` gets a fresh `built_at` every run. **That churn is cosmetic**
  (Rule #12) — discard it unless the embedded sha256 actually moved.
- macOS has no equivalent: `build-macos-intel.yml` stages a native `ymux-cli` itself,
  because this script is PowerShell-only.

**A `.ps1` in this repo must keep non-ASCII out of code lines.** Windows PowerShell 5.1
reads BOM-less UTF-8 as ANSI, so an em-dash inside a string literal decodes to a smart
quote that the tokenizer accepts as a string delimiter — the file then fails to parse
with a cascade of errors pointing at innocent lines. Em-dashes in `#` comments are fine;
inside `"..."` they are a build-breaker. `ci-windows.yml` parse-checks these scripts in
seconds, under `shell: powershell` (5.1) specifically because that is the shell whose
encoding behaviour causes the bug.

## `app/src-tauri/Cargo.toml` and `build.rs`

`tauri = "=2.10.3"` with `features = ["unstable", "tray-icon", "image-png", "devtools"]`,
**pinned**. The unstable feature gates `Window::add_child`, which the per-workspace
browser webviews depend on. Bumping tauri means verifying `add_child`'s signature and the
multi-webview shape still compile — push the bump and let CI type-check it (Rule #17),
then smoke-test the workspace Browser.

**`devtools` is the dangerous one.** It is the only thing that makes wry call
`setInspectable(true)` on macOS, i.e. the only way Safari's Develop menu can attach to the
workspace Browser webview in a release build (Phase 82.E). But `tauri-runtime-wry` reads
the setting as `devtools.unwrap_or(true)`, so **enabling the feature makes every webview
inspectable by default** — including `main`, which renders live PTY output, i.e. Rule #1.
The explicit `.devtools(false)` calls on the main-window and popout builders in `lib.rs`
are what keep that from happening. They are not optional, and they are not cleanup. See
`backend-core.md` § Gotchas and `backend-panes.md` § Panes.

The feature list is now `["unstable", "tray-icon", "image-png", "devtools"]`. `devtools`
(Phase 82.E) is the only thing that makes wry call `setInspectable(true)` on macOS, i.e.
the only way any inspector can attach to the workspace Browser webview in a release
build. **It is dangerous on its own:** tauri-runtime-wry reads it as
`devtools.unwrap_or(true)`, so turning it on makes every webview inspectable by default
— including `main`, which renders live PTY output (Rule #1). The explicit
`.devtools(false)` on the main and popout window builders in `lib.rs` and the
`.devtools(true)` in `workspace_browser.rs` are one unit with this line; a window added
later inherits `true` unless it opts out.

`build.rs` runs `tauri_build`, which is what embeds `frontendDist` — the whole reason
Rule #13 exists.

The workspace also contains the eight `crates/ymux-*` members, the `cli` and `mcp`
binaries.

## `sdk-gen/` — the SDK drift-guard

`ci-check.mjs` regenerates every spec and SDK from the current server source and fails if
anything moved versus what is committed:

1. `emit-specs.mjs` — runs `go run ./cmd/ymux-server openapi` and writes
   `sdk-gen/specs/{openapi,asyncapi,frames.schema}.json`
2. `gen-typescript.mjs` → `sdk/typescript`
3. `gen-kotlin.mjs` → `sdk/kotlin`
4. diff against the tree

A red check means someone changed a handler or a frame without running `npm run gen`.
Not running this is exactly how ~670 lines of already-stale generated output rode into
the rename commit unnoticed (FOLLOWUPS P2, 2026-08-18) — the same "server changed,
derived artifact didn't" family as the server-rebake gate and, now, the vault gate.

## `scripts/vault-check.mjs` — this vault's own gate

Two checks over `docs/vault/*.md`:

- **hash** — sha256 of every file a vault `covers:`, against `docs/vault/.vault-lock.json`
- **diff** (`--diff-base <sha>`) — if a covered file moved in this change, the owning
  vault file must have moved too

```bash
node scripts/vault-check.mjs
```

`--write` re-stamps the lock after you update the prose. `[vault-skip]` in the PR title
or head commit subject skips the diff half with a `::notice::`. No dependencies, so it
runs on a fresh checkout. See `docs/CONTRIBUTING.md` § Updating the vault.

## The three generated things

Nothing regenerates these automatically. Each has its own guard:

| Artifact | Generated by | Guarded by |
|---|---|---|
| `app/src/bindings/*.ts` | ts-rs, via `cargo test` in `src-tauri` | `tsc` fails on drift |
| `sdk/{typescript,kotlin}` + `sdk-gen/specs` | `npm run gen` in `sdk-gen` | `sdk-gen/ci-check.mjs` |
| `resources/ymux-server-linux-{x64,arm64}` | CI's Go cross-build | the rebake gate in `ci-windows.yml` |
| `docs/vault/.vault-lock.json` | `vault-check.mjs --write` | the vault gate |

## Invariants

- **Rule #13 / #14** — build through the Tauri CLI, then launch it.
- **Rule #17** — builds and tests run on CI only. `vault-check.mjs`, `tsc`, and
  `ci-check.mjs` are the exceptions worth running locally: they are seconds, not minutes,
  and need no toolchain.
- `build:linux-cli` before cargo, always.
- Don't commit `release_notes.md`; don't touch `backup-phase23-*/` or the repo-root
  `.bat`/`.ps1` helpers (Rule #11).

## Read the source when

You need the exact bundler config, an NSIS installer hook, or the codegen templates.
Release process: `docs/RELEASING.md`. Build details: `docs/BUILD.md` and
`docs/CONTRIBUTING.md` → "Building a runnable exe".
