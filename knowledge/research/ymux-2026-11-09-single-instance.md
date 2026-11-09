<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: none -->

**Recommendation: make `config_lock` a real per-config-dir gate. Do not adopt the plugin. First fix a merge bug that defeats the "saves are merged" guarantee.** Everything below is static reading; nothing was run.

**1. Second instance today (identical boot on both OSes)**
It WARNs and boots fully (`config_lock.rs:130-140`). No plugin is present; the comment at `rpc_server.rs:189` is false. Pre-rename builds no longer share the default dir, because the migration moves it (`ymux-core/lib.rs:116`). The real risk is ymux-named builds up to v0.5.0, or anything before v0.5.1-beta.3, which take no lock.

Collisions:
- **`workspaces.json`, the merge bug:** A and B load T0. B adds X. A saves: X is kept, but A's base becomes the merged text (`lib.rs:1073`). A saves again: disk equals base, so the fast path (`workspaces_merge.rs:203`) writes A's in-memory state and X vanishes with no log line. In-memory state is never reloaded (only load is `lib.rs:12881`). The same path strips unknown same-schema fields, so the original fear survives. The tests do one save per instance (`lib.rs:16445`).
- **`settings.json`:** whole-document last-write-wins, no merge (`settings.rs:1742-1754`).
- **`secret-env.json`:** whole-document dump with a fixed tmp name (`secret_env.rs:71`). Lost values exist nowhere else. `known_hosts.json` has the same fixed tmp name (`ymux-core/lib.rs:518`), so concurrent saves can rename the other's partial file.
- **RPC on Windows:** B adds instances to the same pipe (`rpc_server.rs:93`), so a hook can reach the instance that doesn't own its pane.
- **RPC on macOS:** B unlinks A's live socket (`rpc_server.rs:190`). A stays deaf until restart.
- **`YMUX_CONFIG_DIR` doesn't isolate RPC:** the name is per-user (`ymux-core/lib.rs:788,801`).
- **WebView2:** with persistence off, B runs `remove_dir_all` on the live profile (`lib.rs:12809`), before the lock is taken.

**2. Log rotation: closed in code, unverified live; BACKLOG.md:118 is stale** (see FOLLOWUPS-ARCHIVE, closed 2026-10-06).
- Rotate closes its handle, locks `debug.log.lock`, re-stats, renames (`log_writer.rs:348-364`). A second rotator finds a fresh file and skips.
- FILE_SHARE_DELETE lets the rename succeed while B holds the file. B's next batch sees a length mismatch and reopens (`:304-308`).
- Residual: one batch lands in `debug.log.1` if the rename falls between check and write. That is misplacement, not loss.
- If the rename fails (AV holding `.1`), truncating loses up to one cap (`:369`).

**3. Plugin (2.5.5, Rust 1.90)**
- Windows: named mutex `<identifier>-sim`, args via WM_COPYDATA, then `exit(0)`.
- macOS: `/tmp/<identifier>_si.sock`; connect-refused unlinks and rebinds.
- Deep links are an optional feature; ymux has none.
- Keyed by identifier, not binary or config dir. It would kill a dev build even with `YMUX_CONFIG_DIR` set. With `semver`, different versions coexist on one dir, which is the dangerous case. It can't stop builds that lack it.

Legitimate multi-instance: dev beside release (`CONTRIBUTING.md:208`), portable exe (`RELEASE-0.5.0-CHECKLIST.md:394`). Popouts are in-process (`lib.rs:12694`).

**4. Stale lock: not real.** The kernel frees LockFileEx/flock at death, and `Acquired{stale}` already handles the leftover record. BACKLOG's "needs no cooperation" is also wrong: an advisory lock binds only builds that take it. Remaining edge: a live holder hung or hidden in the tray (`tray.rs:16`), which is why focus matters.

**5. Options**

| Option | Trade-off |
|---|---|
| Diagnostics only | Silent loss continues |
| Refuse with dialog | Safe, but confusing with a tray-hidden holder |
| Focus holder | Best UX; needs one RPC method (none exists) |
| Plugin | Wrong key; breaks dev workflow |
| Per-build dir | Fragments user data on every upgrade |
| Log per pid | Solves a closed problem |

**Do, in one PR:**
1. Line 1073: store `ours`, not merged `text`, as base. Add a three-save regression test. Cost: merge runs and logs on every later save.
2. Per-pid tmp names in the two stores.
3. Move `acquire` to the top of `run()`, before the WebView2 wipe. On `HeldBy`: call new `app.focus` RPC and exit; if that fails, dialog naming pid and version, then exit. `Unavailable` stays fail-open.
4. When `YMUX_CONFIG_DIR` is set, suffix the pipe/socket with a dir hash and export `YMUX_PIPE_PATH` to local panes. Defaults stay unchanged.

**Files:** `lib.rs`, `config_lock.rs`, `rpc_server.rs`, `secret_env.rs`, `ymux-core/lib.rs`, `vault/backend-core.md`, `vault/crates.md`, `CONTRIBUTING.md`, `BACKLOG.md`, `FOLLOWUPS.md`.
**Size:** about 100-150 production lines plus tests.

**Test live:**
- Windows and macOS: second launch focuses from tray; kill -9 then relaunch starts clean.
- Hung holder.
- Dev build with its own dir keeps hooks on each side.
- NSIS update and macOS relaunch don't hit the gate.
- Pre-lock v0.5.0 beside a new build: three saves each.

**Invalidated if:**
- Yossi deliberately runs two instances on one dir.
- The updater overlaps the old process.
- A holder's lock survives in a child process (Rust handles shouldn't inherit; confirm live).
- Pre-lock builds stay in routine use; the gate can't see them, so only fix 1 protects you.
