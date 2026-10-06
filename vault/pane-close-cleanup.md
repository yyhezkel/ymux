# Per-pane state cleanup — where a retired pane id is dropped

- Pane retire points (lib.rs): `fn workspace_close_pane` (single pane, via `close_pane_in`) · `fn teardown_workspace_runtime` (every pane of each deleted workspace, called from `fn workspace_delete`)
- `workspace_close_pane` drops: diff watcher (`diff_pane::stop_watcher`) · `core.pane_sessions` binding · the session unless an SSH consumer pane remains
- `teardown_workspace_runtime` drops: workspace browser webview/popout · bootstrap guard · tunnel registry · port detection · session owners · per-pane `pane_sessions` + session · forwards · notes
- `AppState.agent_runs`: removed on hook `session-end` (rpc_server.rs session-end arm, emits Unknown with `seq + 1`); ticket ymux-app-src-tauri-src-32 adds pane close + workspace delete (`clear_pane_agent_run`)
- `AppState.briefs`: NOT dropped on pane close (only flagged `session_ended`); in-memory, keyed by pane id
- Hooks cannot re-insert a closed pane: `fn resolve_hook_pane` returns only ids found in a layout (or recovered via tmux session name to a live pane)
- Frontend mirror: `app/src/App.tsx` `pane:agent-run` listener deletes on `state === "unknown"`; seq guard drops events with `seq <= prev.seq`
- Canonical prose: `docs/vault/backend-core.md` (PaneAgentState bullet), `docs/vault/backend-rpc.md` (hook → agent state)
