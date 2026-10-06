# Contributing

How to navigate the codebase and add features without flying blind. This is a
personal project so there's no PR review process — these are just the
conventions to keep things consistent.

## Where to start reading

1. [`docs/vault/INDEX.md`](./vault/INDEX.md) — **the vault**: ~3k lines of prose
   explaining ~90k lines of code, so you can answer "what does this do / where does X
   live" without opening the source. Start here, every time.
2. [`docs/ARCHITECTURE.md`](./ARCHITECTURE.md) — the big picture.
3. [`docs/PROTOCOLS.md`](./PROTOCOLS.md) — the wire formats. Useful when adding RPC
   methods or wire-level features.
4. The source. `lib.rs` is large but well-sectioned with `// ─── Section ─────` headers,
   and each vault page names the questions it deliberately does not answer.

(`docs/MODULES.md` used to be step 2. It is now a pointer to the vault — see that file
for why.)

## Building a runnable exe

**Never build the app with plain `cargo build --release`.** It compiles and
links without complaint, and the binary it produces is broken: `tauri-build`
only embeds `frontendDist` when the build runs through the Tauri CLI, which
injects the env that selects production mode. Without it the exe stays in dev
mode and loads `devUrl` (`http://localhost:1420`) at startup — so on a machine
with no dev server running, every window is a WebView2
`ERR_CONNECTION_REFUSED` page. Nothing in the Rust log hints at it; boot
proceeds normally up to `rpc server spawned` and then simply stops, because no
frontend ever loads to call in.

Use one of:

```bash
npm run tauri build -- --no-bundle            # just the exe, from app/
powershell -ExecutionPolicy Bypass -File ./scripts/build-release.ps1   # full release, see RELEASING.md
```

To check a binary is a real production build, look for the current
`app/dist/assets/index-<hash>.js` filename inside it:

```bash
grep -c -a -F "$(ls app/dist/assets/*.js | xargs -n1 basename)" app/src-tauri/target/release/app.exe
```

A dev-mode binary scores 0. Note that `localhost:1420` appears in *both* kinds
— the whole config, `devUrl` included, is embedded either way — so its presence
proves nothing on its own.

And per CLAUDE.md: a build that compiles is not a build that runs. Launch it and
confirm the UI actually comes up before calling it verified.

### Building from a worktree

A fresh worktree has no `target/`, so the first build there compiles all ~740
lockfile packages -- 12-15 minutes. Point it at the main checkout's cache
instead (a junction, not `CARGO_TARGET_DIR`, because every script and workflow
here hardcodes `app/src-tauri/target/...`):

```
mklink /J "<worktree>pp\src-tauri	arget" "<main checkout>pp\src-tauri	arget"
```

Junctions are resolved below the Win32 file API, so Cargo sees a plain
directory. Registry dependencies share fingerprints verbatim; workspace crates
get a different `-C metadata` per worktree path and coexist rather than
thrashing. Two worktrees building the same profile at once will serialize on
Cargo's build lock -- correct, just slower.

**One footgun:** `git clean -xdf` inside the worktree follows the junction and
deletes the shared target directory. So does `rm -rf` of the worktree itself.

## Recipes

### Add a new RPC method

The dispatcher is in `app/src-tauri/src/rpc_server.rs::dispatch`. It's a flat
`match` on the `method` string. Add an arm:

```rust
"my-new-method" => {
    let arg = params.get("arg").and_then(|v| v.as_str()).ok_or("missing arg")?;
    // … do work, can read state.workspaces / state.feed / etc.
    Ok(json!({ "ok": true }))
}
```

If your method needs to mutate workspace state, follow the existing pattern:
mutate in a small `{ … }` block holding the lock briefly, then `persist(state)?`
outside the lock to avoid holding it across I/O.

If your method needs to emit a UI event, use `app.emit("event:name", &payload)`
and add a corresponding `listen()` in `App.tsx`.

Then expose it on the CLI side:

1. Add a `Cmd::MyNewMethod { ... }` variant in `cli/src/main.rs`.
2. Add the corresponding `Cmd::MyNewMethod { ... } => rpc_call("my-new-method", json!({...})).await`
   arm in `main()`.
3. Update [`docs/CLI.md`](./CLI.md) and [`docs/PROTOCOLS.md`](./PROTOCOLS.md).

### Add a new agent hook subcommand

The mapping from subcommand → `kind` lives in `cli/src/main.rs::Cmd::ClaudeHook`:

```rust
let blocking = matches!(subcommand.as_str(), "tool-permission" | "pre-tool-use");
let kind = if blocking { "permission_request" } else { "passive" };
```

Add your name to the `matches!` list to make it blocking, or just leave it as
`passive`. The title heuristic is in `derive_hook_title` — if your subcommand
needs special title formatting, add a `match` arm there too.

The server side handles all kinds uniformly via `feed.push` — no changes needed
unless you're introducing a new `kind` (currently `permission_request` and
`passive`; new kinds would need backend dispatch logic in
`rpc_server.rs::dispatch::"feed.push"` to know whether to block).

### Add a new pane type (e.g. browser pane)

The current `LayoutNode::Pane` carries a `Connection` that's either Local or SSH.
A WebView2 pane would be a third option:

1. Extend the `Connection` enum in `lib.rs` (and the matching TS type in `types.ts`):
   ```rust
   #[serde(tag = "type", rename_all = "lowercase")]
   pub(crate) enum Connection {
       Local { … },
       Ssh { … },
       Webview { url: String, … },
   }
   ```
2. Update `pane_connect` in `lib.rs` — for `Connection::Webview` you don't open a
   PTY. Instead, return a fake "session_id" the frontend can recognize.
3. In `PaneView.tsx`, branch on the connection type — render an `<iframe>` or a
   Tauri webview child instead of the xterm slot.
4. Decide what `pty_write` / `pty_resize` mean for webview panes (probably no-ops).
5. Update the create-workspace modal to offer the third option.

This is the planned Phase 6.6+ work; nobody has done it yet.

### Add a new persisted file

If you need a new on-disk config file beyond `workspaces.json` and
`known_hosts.json`:

1. Put it under `config_dir()?.join("<your-file>.json")`. Don't reach for
   `Tauri::path::BaseDirectory::*` — we deliberately don't use those because of
   the sandbox-redirection issue (see [BUILD.md](./BUILD.md#common-gotchas)).
2. Use the `save_to_disk` pattern: write to a `.tmp` file, fsync, rename. Don't
   write directly to the target path.
3. Add a `LoadState`-style poison flag if mutations could clobber existing data
   on parse error.
4. Document the schema in [`docs/CONFIG.md`](./CONFIG.md).

## Type synchronization

The frontend data-model types (`Workspace`, `LayoutNode`, `Connection`,
`PaneKind`, `FeedItem`, `Settings`, …) are generated from the Rust structs
by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). To regenerate after
changing a Rust struct: `cd app/src-tauri && cargo test`. The bindings
land in `app/src/bindings/` and are re-exported from `app/src/types.ts`.
Don't hand-edit `app/src/bindings/*.ts`. Note: ts-rs renders `Option<T>`
as `T | null`, and `app/src/settings.ts` keeps a richer hand-tuned mirror
(literal unions for the UI) rather than the generated `Settings`.

## Updating the vault

`docs/vault/*.md` explain the code so it can be read instead of the source. Each page
declares in its frontmatter which files it `covers:`, and `docs/vault/.vault-lock.json`
holds a sha256 of each of them.

When you change covered code, update the page that covers it **in the same commit**:

```bash
node scripts/vault-check.mjs
```

That names every page whose code has moved. Fix the prose, then re-stamp and commit both:

```bash
node scripts/vault-check.mjs --write
```

`ci-windows.yml` runs the same script on every PR, and additionally requires that the
owning `.md` appears in the diff — so re-stamping without touching the prose does not
pass. If a change genuinely does not affect the explanation (a typo fix, a rename the
page never named), put `[vault-skip]` in the PR title; it is logged as a `::notice::` on
the run.

### Citing a line

Cite a line as `[symbol@path/to/file.rs:123](relative/link/to/file.rs)`: the symbol the
line is about, `@`, the path suffix and line, then a link that resolves from the page.
`vault-check` fails if `symbol` is not a whole word within 3 lines of that line, and
prints the corrected cite (`symbol@file:<n>`) — copy it in. A bare `file:line` is
rejected, because with no symbol there is nothing to check; this runs even under
`[vault-skip]`.

Adding coverage is just adding a path to a `covers:` list — the script needs no changes.
A file may be covered by exactly one page. Every tracked code file (`.rs`/`.ts`/`.tsx`/`.go`/`.mjs`)
must be covered or listed in a page's `unowned:` frontmatter (block list, globs allowed, a `#`
reason on each entry) — an unclaimed file fails the gate. Tests and generated output go in `unowned:`.
Keep a page around 200 lines: if it needs more, the area wants splitting.

## Running two builds side by side (YMUX_CONFIG_DIR)

A stable build and a dev build share `%APPDATA%\ymux` (`~/Library/Application Support/ymux`
on macOS) unless told otherwise. Newer builds cope: every save is a three-way merge
(`workspaces_merge.rs`) behind a schema gate, and the second instance logs a `[CONFIG_LOCK]`
WARN naming the holding pid. An **already-shipped older build** (any `winmux-*.exe`) has none
of that and rewrites `workspaces.json` without the fields it does not know, so isolate it:

```
set YMUX_CONFIG_DIR=C:\temp\ymux-dev      (cmd)
$env:YMUX_CONFIG_DIR = "C:\temp\ymux-dev"  (PowerShell)
```

Set it before launching the second build; it then gets its own workspaces, settings and
`debug.log`. The lock never blocks startup, and a crash leaves no lock to clean (the OS frees
it; the leftover `ymux.owner.json` is replaced and logged as `stale`). If `debug.log` shows
`pre-rename build running`, that is the case this section is for.

## Logging conventions

We have two logging mechanisms:

| Use | When |
|---|---|
| `dlog("msg")` | Operational events that we'd want to see post-mortem from a user's machine: load/save, bootstrap steps, tunnel handshake outcomes, every SSH exec command. Writes to `%APPDATA%\ymux\debug.log` unconditionally. |
| `tracing::info!` / `tracing::warn!` / `tracing::debug!` | Engineer-facing log output. Visible only when running via `cargo run` / `tauri dev` (the standalone exe runs with the `windows_subsystem = "windows"` flag and has no console). |

When in doubt: use `dlog` for things you'd want to see if a user reports a bug.
The Phase 6.2 BOM bug was diagnosed in 5 minutes once the bootstrap was
fully `dlog`-instrumented; before that, we were guessing.

## Style

- **Don't write comments that restate the code.** Save comments for *why*: a
  hidden constraint, a workaround for a specific upstream issue (with a link or
  bug number), or the reason a non-obvious choice was made. Examples in the
  current source:
  - `// TODO Phase X: TOFU + known_hosts` (intent for future)
  - `// The pageant-0.0.1 crate calls .unwrap() on a Windows error when…` (why
    we wrap in `catch_unwind`)
  - `// Phase 6.4: TCP transport authenticates via HMAC challenge-response.
    Token never appears on the wire.` (security-critical why)
- **Section headers** in long files (`// ─── Section name ───`) keep `lib.rs` navigable.
- **Error strings** as `Result<_, String>` for Tauri commands (so they serialize cleanly
  to the frontend). Internal Rust APIs can use `anyhow::Error` if richer contexts help.
- **Newline-delimited JSON** is the wire format choice on every transport. Don't
  switch a transport to length-prefixed framing without updating both ends and
  the docs.

## Scrollback reflow is fundamentally limited

A common UX complaint is "I resized the pane and old lines didn't rewrap." Phase
55-C audited both sides and concluded: this is structural, not a config we can
fix outright. Documenting so future contributors don't chase it.

- **xterm.js side.** `Terminal#resize(cols, rows)` rewrites the live buffer
  using the new column width, but **scrollback rows that were already pushed
  out** aren't recomputed — they were rasterised at their original width when
  pushed. xterm.js has no "rewrap scrollback" knob: keeping the rasterised text
  is what makes scrollback cheap. `convertEol` controls LF→CRLF translation on
  the **input** stream and has nothing to do with reflow — leave it `false` so
  CRLF-emitting PTYs (every modern terminal, including ConPTY) don't get
  double-newlined.
- **tmux side.** Without `aggressive-resize on`, tmux uses the LARGEST attached
  client's geometry when reflowing — which leaves narrow trailing columns black
  on the new smaller ymux pane. Phase 55-C flipped that on in
  `app/src-tauri/resources/ymux-tmux.conf`. Alt-screen apps (vim, htop) now
  see their full terminal area shrink on every resize, which is what we want
  for ymux's split-heavy workflow.
- **What we DO trigger** on resize: `fitAndResize()` on every pane after a
  layout edit, maximize toggle, or distribute-evenly. That re-fits + re-emits
  pty_resize so future output uses the new column width — only the scrollback
  that was already there stays at its old width.

If a user reports "old lines look weird after resize," that's expected. The
fix is to clear scrollback (Ctrl+L in most shells, `tmux clear-history` in
tmux) and re-emit the content at the new width.

## Commit conventions

One commit per logical change. Subject line follows
`Phase <N.M>[: short summary]` for phase work, `Phase <N.M> fix: <subject>` for
follow-up fixes. For non-phase work, anything descriptive in the imperative
mood:

```
docs: comprehensive documentation — architecture, modules, protocols, …
chore: bump russh to 0.50 (when released)
```

The body should explain *why* a change was made when it isn't obvious; the
diff explains *what*.

## Postmortem-style fix notes (commit body + release notes)

When a commit fixes a real-world bug — anything where a user hit broken
behavior — write the body in postmortem shape. Same for the bullet
that lands in a GitHub release. Four parts:

- **SYMPTOM:** what the user saw. Specific. Quote the error string if
  there was one.
- **DISCOVERY:** how you found the cause. The grep, the dlog line, the
  reproduction that pinpointed it.
- **ROOT CAUSE:** the actual reason. Not "X failed" but *why* X failed
  in this scenario when it works elsewhere.
- **FIX:** what changed and why that specifically addresses the root
  cause. If a workaround would've been simpler, say why you didn't.

Example (modeled on Phase 39.D):

> **SYMPTOM:** `sftp create …ymux-linux-x64: Failure: Failure` on every
> reconnect after the Phase 39 CLI rebuild changed the bundled binary's
> hash. **DISCOVERY:** OpenSSH server logs showed `ETXTBSY` returned to
> the SFTP layer; matched against the leftover `port-watch` process
> from the pre-39.C pipe crash still running the old binary.
> **ROOT CAUSE:** Linux returns `ETXTBSY` when truncating a currently-
> executing binary; the SFTP server maps it to the generic
> `SSH_FX_FAILURE` and renders it as the unhelpful "Failure" string.
> **FIX:** Upload to `<name>.tmp`, then shell `mv -f` (rename atomically
> swaps the dir-entry to a new inode — the running process keeps its
> own inode). Also `pkill -f ymux-linux-x64` first to free orphaned
> watchers; belt-and-suspenders.

A future-you reading the log a year from now should understand the
incident in ~30 seconds without leaving the commit. The discipline is
the point — it costs ~5 extra minutes per bug-fix commit and pays back
the next time something similar bites.

For features and refactors, the regular "why + what" body is fine. The
postmortem shape only applies to fixes.

## Phase numbering

Phases are stable in the commit history. Reading `git log --oneline | grep '^...... Phase'`
gives you a chronological view of what was added when:

- 1–5: in the **initial commit** (`4a6d402`). These were built before git was
  set up; the commit message lists them.
- 6.x: split into separate commits, one per phase + fix follow-ups where needed.
  See `git log` for the current state.

If you start a new phase that's a logical follow-up to 6.5, call it 6.6. If
you're starting a new pillar, call it 7.x. Don't reuse numbers.
