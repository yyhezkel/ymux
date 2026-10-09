# session-meta keys (writer vs reader)

- File: `~/.ymux/session-meta.json`, CLI writer `app/src-tauri/cli/src/session_meta.rs`; canonical prose `docs/vault/cli.md` § `session_meta.rs`
- Writer key (pre-fix): `resolve_session_name` (`session_meta.rs:225`) → tmux `#S` when `$TMUX`, else `ymux-<YMUX_PANE_ID>` (`sanitize_tmux_session_name`, `:213`)
- Reader key (zellij, desktop): `list_zellij_sessions` (`app/src-tauri/src/lib.rs:10096`) → `apply_session_meta` (`lib.rs:3666`) joins by zellij session name
- Zellij session name for a pane: `session_name_for_pane` (`lib.rs:2511`) = explicit → sanitized title (`sanitize_tmux_session_name_for_title`, `lib.rs:2450`) → `ymux-<pane>`; attach via `build_zellij_attach_command` (`lib.rs:3578`)
- ⇒ titled zellij pane: writer `ymux-<pane>` ≠ reader `<title>` (the bug); plan-ymux-session-meta-prune-marks keys the writer by `$ZELLIJ_SESSION_NAME`
- Prune: `prune_with` (`session_meta.rs:164`) judges keys vs `tmux ls` only; `prune` skips when tmux unspawnable (`:139`) → with tmux installed, zellij keys get ended/dropped (fix: `mux: "zellij"` owner marker → skipped)
- Desktop pane state stores the zellij name as `tmux_session` (`lib.rs:3238`) → `resolve_hook_pane` (`lib.rs:2046`) maps a hook's session name back to a pane
- Hook callers of `resolve_session_name`: `app/src-tauri/cli/src/main.rs:2439`, `:2551`, `:2594`
