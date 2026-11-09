<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: none -->

**Do it in part: build one handle-based SSH exec core and migrate to it. Drop `exec(workspace_id, ...)`, Local/WSL dispatch and the path-translation move.** Findings come from reading code; nothing was run.

**1. Inventory: 20 one-shot loops, not 6** (PTY and watcher channels excluded)

| Behaviour | Sites |
|---|---|
| Stops at EOF, exit code stays 0 | `provisioning.rs:1269`, `file_manager.rs:1527`, `lib.rs:4157,4348,4439`, `ymux-bootstrap/src/lib.rs:102`, `ymux-tunnel/src/lib.rs:379` |
| No exit code; timeout returns `Ok(partial)` | `addons.rs:65,308`, `claude_log.rs:125`, `claude_summary.rs:60`, `updater.rs:518`, `lib.rs:10536,11599`, `rpc_server.rs:2226` |
| Output and code ignored | `file_manager.rs:641`, `lib.rs:5705` |
| Status-aware | `worktrees.rs:297`, `lib.rs:9205,12119` |

- **Pickers:** 8 workspace-exact (`claude_log.rs:99`, `claude_summary.rs:49`, `file_manager.rs:548`, `updater.rs:507`, `rpc_server.rs:2207`, `lib.rs:5820,10773,11562`), one machine (`lib.rs:6522`), one host (`worktrees.rs:371`).
- **SFTP opens:** 5.
- **Needed differences:** per-call timeout (none for zip, `file_manager.rs:1523`; 800 ms for kill), stdin for secrets (`addons.rs:308`), stderr mode.
- **Accidental differences:**
  - no timeout at `provisioning.rs:1269`, `file_manager.rs:641`, `lib.rs:4157,4348,4439`
  - `&mut Handle`
  - `sh -c` only at `worktrees.rs:286`, so fish can break `lib.rs:11599`
  - 60 s SFTP timeout missing at `file_manager.rs:563` and `claude_log.rs:107`
  - no output cap
  - per-chunk UTF-8 decoding at `lib.rs:12133`
  - no explicit close at `provisioning.rs:1269`, `file_manager.rs:1527`
- **`ChannelMsg::ExitSignal`:** no copy handles it, so a killed process reports 0, even in `worktrees.rs`. Close without status also reports 0 there.

**2. Bugs**

The EOF race was seen live once and fixed only in `worktrees.rs` (`PROGRESS.txt:2324`). The others are inferred from identical code:
- A failed provisioning step emits "done" (`provisioning.rs:1114`).
- `preflight_sudo` passes when `sudo -n` failed (`:1311`).
- A failed `mv -f` reports a saved ticket (`tickets.rs:586`).
- A failed pubkey install still persists key auth (`lib.rs:4181`).
- Inverse case: a successful tmux rename errors (`lib.rs:9227`).

Silent timeouts and pickers:
- The hooks update has 6 s and no exit check, and `App.tsx:807` shows success anyway.
- `addons::remote_home` becomes `""` on timeout, or polluted by stderr, giving wrong detect paths.
- `file_manager` fails with "no active SSH session" where addons succeeds through a sibling session (`lib.rs:6534`).

WSL:
- `is_local_workspace` (`addons.rs:53`) returns false for WSL, then fails with "no active SSH session". This is effectively dead, because load rewrites every WSL connection to Local (`lib.rs:1223`).
- No UI creates one, though `CreateInput` (`lib.rs:796`) and `workspace_update` don't refuse a raw payload.
- BACKLOG's stated payoff is gone, and CLAUDE.md's "still load and run" is stale.
- Carry zero WSL in the new layer. Delete the arms and `wsl_unc_path` with the variant (`ymux-types/src/lib.rs:48`). I did not audit `local_setup.rs`.

**3. Smallest API**, in `ymux-core` (it owns `SshClient`, and both crates depend on it):

```rust
exec(&Handle<SshClient>, cmd: &str, ExecOpts { timeout, stdin, stderr: Merge|Drop|Separate, max_bytes })
  -> Result<ExecOutcome { exit: Code|Signal|Unknown, stdout, stderr, timed_out }, String>
```

- It takes a handle because `provisioning.rs:1201` connects before any workspace exists.
- After ExitStatus, wait only a short grace for EOF, so daemons holding stdout don't burn the timeout.
- `exec(workspace_id, script)` doesn't fit Local: Windows has no `sh`, and `worktrees.rs:288` already refuses. Keep `run_git_raw` as the only dispatcher.

Options:

| Option | Trade-off |
|---|---|
| Patch EOF in place | Smallest, but copies drift again |
| Handle core | Fixes the bug class; **chosen** |
| Full workspace_fs | Largest; its Local and WSL arms have little real use |

Migration, one PR each:
1. Core, drain-logic tests, move `worktrees.rs` onto it.
2. `file_manager` (fixes tickets).
3. `provisioning`.
4. `lib.rs:4157,4348,4439`, bootstrap, tunnel.
5. `addons` (`Merge`, keep partial output; `log_sync.rs:218` tolerates it).
6. `updater` plus the toast, `claude_log`, `claude_summary`.
7. Remaining `lib.rs` sites, `rpc_server`.
8. Pickers to `ssh_handle_for_machine`, one `open_sftp`.

PRs 1-4 fix live risk; 5-8 are mostly cleanup.

**4. Rules**
- **#3:** the helper takes a finished string, since SSH exec is a shell string by protocol. Callers use `shell_quote`; replace the hand escaping at `lib.rs:4155,4345`.
- **#4:** `?` and `map_err` only.
- **#1:** log exit code and byte counts only, because `sessions_overview.rs:193` sends `capture-pane` output through exec. Do not inherit bootstrap's stdout debug log.

**Files:** new code in `ymux-core`, the 11 modules above, `App.tsx`, and vault pages `crates`, `backend-panes`, `backend-remote`, `backend-core`, `backend-claude`, `backend-rpc`, `backend-wizards`.

**Size:** about +150 lines, -400 lines, 30-120 per PR.

**Invalidated if:**
- something starts creating `Connection::Wsl`
- several features need one shared Local script runner
- live tests show the grace window breaks installs
- a caller depends on `Ok(partial)` and can't read `timed_out`
