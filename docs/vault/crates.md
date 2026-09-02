---
vault: crates
covers:
  - app/src-tauri/crates/ymux-addons/src/lib.rs
  - app/src-tauri/crates/ymux-bootstrap/src/lib.rs
  - app/src-tauri/crates/ymux-core/src/lib.rs
  - app/src-tauri/crates/ymux-core/src/http.rs
  - app/src-tauri/crates/ymux-core/src/log_writer.rs
  - app/src-tauri/crates/ymux-policy/src/lib.rs
  - app/src-tauri/crates/ymux-protocol/src/lib.rs
  - app/src-tauri/crates/ymux-ssh/src/lib.rs
  - app/src-tauri/crates/ymux-tunnel/src/lib.rs
  - app/src-tauri/crates/ymux-types/src/lib.rs
---

# The eight `ymux-*` crates

5,900 lines carved out of `lib.rs` across phases 51.A–51.H and 66–68. **The organising
rule is `AppState` coupling:** anything that takes `&AppState + &AppHandle` and stitches
helpers together is orchestration and stays in `app`; anything that takes plain
arguments and returns a result moved out here. That is why `connect_and_authenticate`
is still in `lib.rs` while the auth primitives it calls are in `ymux-ssh`.

Dependency direction: `app` → everything. `ymux-tunnel` → `ymux-core`. `ymux-core` →
`ymux-types`. The pure-data crates (`types`, `policy`, `protocol`, `addons`) depend on
nothing but serde, so a CLI or MCP subcrate can link them without the Tauri runtime.

## `ymux-core` (2,207 across 3 files) — the cross-cutting one

The only crate `app` cannot function without. Three files:

**`lib.rs`**
- **The user-visible logger** — `log_debug/info/warn/error(tag, msg)`, `log_at`,
  `set_log_level`/`log_level`, `prune_logs(retention_days)`, `clear_debug_log`.
  `DEBUG_LOG_MAX_BYTES = 5 MB`. `dlog`/`dlog_tag` are legacy info-level shims — **do not
  add callers** (Rule #9).
- `config_dir()` — and this is where the `%APPDATA%\winmux` → `ymux` migration runs,
  once, on upgrade.
- `shell_quote(s)` — the only sanctioned way to put a value into a POSIX script
  (Rule #3).
- Pure layout walkers — `collect_panes`, `collect_panes_with_kind`,
  `first_terminal_connection`, `backfill_terminal_connections`.
- **Known-hosts / TOFU** — `KnownHost`, `KnownHostsFile`, `load_known_hosts`,
  `save_known_hosts`, `HostCheckOutcome`.
- `SshClient` — the russh `client::Handler` type used in every `Handle<SshClient>`
  signature, plus the `BridgeSpawner` callback the forwarded-tcpip handoff uses.
- **Session types** — `Session::{Local,Ssh}`, `LocalSession`, `SshSession`, `SshCmd`,
  and the map aliases `SessionMap`, `PaneSessionMap`, `ForwardMap`.
- `CoreState` — the 7 russh/session/forwards/watcher fields `AppState` wraps.
- **`pipe_name()` / `pipe_names()` / `pipe_name_legacy()`** — the RPC endpoint paths,
  shared with `ymux-tunnel` so both ends resolve identically. The Unix side returns a
  *list* because macOS caps `sun_path` at 104 bytes.

**`log_writer.rs` (554)** — the queued writer behind the logger, and the reason
`flush_log()` is public: a panic on its way to an abort loses queued lines otherwise.

**`http.rs` (236)** — shared HTTP retry helper, added for the updater path on
restricted networks.

## `ymux-types` (1,019) — pure persistence/wire types

`Connection` (`local | ssh`, plus the retired `wsl` variant that still deserializes),
`LayoutNode` (`pane | split`), `PaneKind`, `SplitDirection`, `DiffSource`,
`BrowserState`, `EnvVar`, `Workspace`, `WorkspaceGroup`.

**`Workspace.tmux_session: Option<String>`** (Phase 87.B) marks a row the active-sessions
overview opened FOR one multiplexer session. Written only by `workspace_open_session`,
renamed by `tmux_rename_session`, elided when absent so old files round-trip byte-identical
(the round-trip test lists it). It is what makes Open idempotent, what the sidebar draws
the terminal glyph from, and the fallback that lets the row's first pane re-attach after a
restart on a machine whose localStorage never saw it. `parent_id`'s comment now names three
create paths, not two.

**`Workspace.tabs_mode: bool`** is worth reading the comment on. It renders the
workspace's panes as a tab strip instead of a split grid — and it is a **flag, not a
`LayoutNode::Tabs` variant**, deliberately. The `layout` tree is untouched by the mode:
tabs are just `collect_panes(layout)` in DFS order, so flipping to tabs and back restores
the exact splits with their ratios and nesting. A third `LayoutNode` variant would make
every `match` non-exhaustive (~44 sites in the app crate alone), would be lossy in both
directions, and an older ymux reading a `Tabs` node out of `workspaces.json` could not
render it at all. It is also one of the keys excluded from the "grandfathered" list in
the migration test — check that list before adding another.

Deliberately **no business logic** — structs, enums, serde attrs, and the small helpers
serde references by name (`default_true`, `is_true`, `is_terminal_kind`). ts-rs binding
regeneration is isolated here, which is the point: the frontend's `src/bindings/` comes
out of this crate.

## `ymux-ssh` (385) — pure auth primitives only

~270 LOC of actual surface: `AuthMethod` (which method succeeded),
`key_load_needs_passphrase` (an error-message classifier), and `pkwh`/`pkwh_pub` (an
RSA-aware `PrivateKey` wrapper). Functions that take a `Handle<SshClient>` plus
credentials and return a result — nothing that needs `AppState`.

## `ymux-tunnel` (527) — the reverse tunnel

Bridges a remote-forwarded TCP channel to the local RPC endpoint. The preamble is an
**HMAC-SHA256 challenge/response**, not a plain token, so the shared secret never travels
in cleartext.

**`CHALLENGE_TAG` still emits the legacy `WINMUX-CHALLENGE`** on purpose — a pre-rename
remote CLI does a literal prefix match. Both ends read *and mirror* either dialect; the
Go counterpart is `challengeTag` in `server/internal/chat/chat_hookrpc.go`. **Flip both
together or not at all.** Rule #8: the token never reaches a log.

## `ymux-policy` (542) — the 3-state permission engine

Phase 18's PreToolUse integration routed **every** matched tool call to a blocking
approval card. In Claude Code's `default` permission mode that stalled the agent on each
Bash/Write/Edit until the user clicked — and if the user was away it timed out and was
conservatively denied, so Claude could make no progress. That foot-gun is why the feature
was shelved, and this crate is what unshelved it.

`evaluate(tool_name, bash_command)` → `Verdict { decision, … }`:

- **`Auto`** — allow immediately, no card. The common case: ordinary edits, normal shell
  commands.
- **`Gate`** — surface the approval card and wait. Elevated-but-legitimate: sudo,
  recursive deletes.
- **`Deny`** — refuse outright.

`split_chained_command` exists because `a && b; c` must be judged per command, not as one
string. `rpc_server::push_policy_audit` records the outcomes.

## `ymux-protocol` (327) — mobile/web wire types

Shared between `ymux serve` and its clients (Android first, web later). Pure data.

- **WebSocket.** Control messages are JSON **text** frames (`ServerMsg` / `ClientMsg`);
  high-rate PTY bytes are **binary** frames (see `frame`), to keep base64/JSON overhead
  off the hot path.
- Default port **7878**.
- Each in-flight hook approval carries a `req_id`, so either the desktop or the phone can
  resolve it — whoever answers first wins, and the other side receives
  `ServerMsg::HookResolved`.

## `ymux-addons` (311) — add-on manifest schema + registry

An add-on is anything ymux installs on a remote: the CLI binary, the tmux config, the
Claude hooks, the `ymux-insights` daemon. Before Phase 68 each had a bespoke installer;
this gives them **one shape** so the desktop manager can install/update/remove/detect
uniformly. Pure data — no IO. The SSH side and the `Builtin` routine dispatch live in
`app/src/addons.rs`.

## `ymux-bootstrap` (616) — remote CLI deploy

Detect the remote arch, hash any existing binary, upload via SFTP when it does not match
the manifest, maintain the `~/.ymux/bin/ymux` symlink. Best-effort, called after auth
succeeds and before the user's shell channel opens.

**No `tauri` dependency, by explicit decision.** The caller resolves resource paths and
passes in the manifest plus a resource-loader closure; `bootstrap()` does all the russh +
SFTP work without ever touching an `AppHandle`.

It also runs the zombie `port-watch` reap from a prior session. Until Phase 86.B the
pattern was `[w]inmux-linux-x64$` — the pre-rename binary name, anchored at end of
line — which never matched the real cmdline `…/ymux port-watch --workspace <id>`, so
the reap had been a no-op since the rename. Now `/([w]inmux|[y]mux) port-watch `.

## Invariants

- **Rule #4** — no `unwrap`/`expect` outside tests anywhere in these crates. They have no
  `main()` to be the exception.
- **Rule #9** — `ymux-core`'s `log_*` is the user-visible logger. `tracing::*` is
  engineer-only and dev-build-only. Pick by audience.
- A crate that grows a `tauri` dependency has stopped being one of these crates. That is
  the line the split was drawn on.

## Read the source when

You need an exact type's serde field names (the frontend mirrors them via ts-rs), the
policy rule tables, or the handshake byte sequence. Wire formats are documented in
`docs/PROTOCOLS.md`.
