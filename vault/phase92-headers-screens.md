# Phase 92 — headers are not screens (anchor map)

- Prose: `docs/vault/backend-core.md` § "Headers vs screens (Phase 92)"; `docs/vault/frontend-shell.md` § "Only a screen is ever active" + § "Headers + cards"
- `is_header` → `app/src-tauri/src/lib.rs:6500`; TS mirror `isHeader` → `app/src/wsTree.ts:22`
- `first_screen_of` lib.rs:6586 · `screen_or_self` lib.rs:6597 · `create_root_with_screen` lib.rs:6637
- `migrate_headers_to_screens` lib.rs:6670; log line `migrate: header ws=<id> → its panes now live on screen ws=<id>` (tag WORKSPACE) lib.rs:6702
- `active_after_delete` lib.rs:6714 — sibling screen → any screen under root → any screen → none; never a header
- `workspace_new_screen` lib.rs:6736 · `workspace_pin_project_folder` lib.rs:6303 · `workspace_open_worktree` lib.rs:6392 · `workspace_delete` lib.rs:8273
- `create_root_with_screen` callers: `workspace_create` lib.rs:6044 · RPC `new-workspace` rpc_server.rs:684 · provisioning `finalize_workspace` Branch 2 (connect-existing / provision-new, `existing_workspace_id: None`) — pre-fix built a bare root WITH layout, no `shell` (ticket ymux-joining-existing-workspace-does)
- Wizard return contract: hands back the ROOT id; `onOpenWorkspace` → `handleSetActive` resolves the first screen (`app/src/App.tsx:5337`)
- RPC `select-workspace` → `screen_or_self` → `app/src-tauri/src/rpc_server.rs:667`; also `action.connect` rpc_server.rs:996
- CLI verb → `app/src-tauri/cli/src/main.rs:1698` `Cmd::SelectWorkspace` → `rpc_call("select-workspace", {id})`
- Sidebar: header click → `onSetCollapsed` `app/src/Sidebar.tsx:936`; `+` → `onNewScreen` Sidebar.tsx:1024
- App: `headerChain` `app/src/App.tsx:1049` · `allPaneAgentRows` App.tsx:1273 · `openWorktree` App.tsx:1988 · `newScreen` App.tsx:3042
- Tests: `mod header_screen_tests` lib.rs:13807 (CI-only, Rule #17)
- Line cites verified 2026-10-09 at 6f3ad47b (lib.rs + create_root_with_screen callers only; other cites from 2026-10-06 e2715ee1); re-grep the symbol when a cite misses
