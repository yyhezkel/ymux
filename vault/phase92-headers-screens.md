# Phase 92 — headers are not screens (anchor map)

- Prose: `docs/vault/backend-core.md` § "Headers vs screens (Phase 92)"; `docs/vault/frontend-shell.md` § "Only a screen is ever active" + § "Headers + cards"
- `is_header` → `app/src-tauri/src/lib.rs:6415`; TS mirror `isHeader` → `app/src/wsTree.ts:22`
- `first_screen_of` lib.rs:6501 · `screen_or_self` lib.rs:6512 · `create_root_with_screen` lib.rs:6552
- `migrate_headers_to_screens` lib.rs:6585; log line `migrate: header ws=<id> → its panes now live on screen ws=<id>` (tag WORKSPACE) lib.rs:6617
- `active_after_delete` lib.rs:6629 — sibling screen → any screen under root → any screen → none; never a header
- `workspace_new_screen` lib.rs:6651 · `workspace_pin_project_folder` lib.rs:6218 · `workspace_open_worktree` lib.rs:6307 · `workspace_delete` lib.rs:8188
- RPC `select-workspace` → `screen_or_self` → `app/src-tauri/src/rpc_server.rs:667`; also `action.connect` rpc_server.rs:996
- CLI verb → `app/src-tauri/cli/src/main.rs:1698` `Cmd::SelectWorkspace` → `rpc_call("select-workspace", {id})`
- Sidebar: header click → `onSetCollapsed` `app/src/Sidebar.tsx:936`; `+` → `onNewScreen` Sidebar.tsx:1024
- App: `headerChain` `app/src/App.tsx:1049` · `allPaneAgentRows` App.tsx:1273 · `openWorktree` App.tsx:1988 · `newScreen` App.tsx:3042
- Tests: `mod header_screen_tests` lib.rs:13675 (CI-only, Rule #17)
- Line cites verified 2026-10-06 at e2715ee1; re-grep the symbol when a cite misses
