# Phase 84 — tabs mode + Claude traffic light: anchors for a live smoke

- Live status: NOT VERIFIED LIVE (FOLLOWUPS.md entry "NOT VERIFIED LIVE: all of Phase 84"); headless Linux pipeline cannot run it → runbook + human at SIGNOFF
- Canonical prose: `docs/vault/frontend-shell.md` (PaneTabs, App signals), `docs/vault/backend-core.md` (PaneAgentState), `docs/vault/backend-rpc.md` (hook → agent state)

## Tabs mode (frontend)
- Strip: `app/src/PaneTabs.tsx` `export function PaneTabs` — owns no state; strip stays LTR, label `dir="auto"` (`.pane-tab-label`) → Hebrew: order fixed, labels mirror
- Drag swap: tab carries `data-pane-id` → reuses pane drag (`paneDrag.ts`) → backend `fn workspace_swap_panes` (lib.rs)
- Mode flag: `App.tsx` `const tabsMode` reads `activeWs()?.tabs_mode`; `const setTabsMode` → RPC `workspace_set_tabs_mode` (lib.rs `fn workspace_set_tabs_mode`; layout tree untouched → split ratios survive)
- Persistence: `tabs_mode` field on the workspace in `workspaces.json`
- Ctrl+Tab: shortcut ids `tab_next`/`tab_prev`, `when: tabsMode` → `const focusAdjacentPane`
- Ctrl+1..9: hard-coded block "Phase 84.A" in App.tsx keydown, 9 = last tab
- `+`: `const newTab` (returns new pane id, focuses it)
- Close: `const closeTab` — picks `next[min(idx, len-1)]` after close → right neighbour, last tab → left

## Traffic light
- Colour: `app/src/paneAgentState.ts` `export function trafficLight` — permission card → red first; running → green; done → yellow; needs-input → red; unknown/idle/stale (>6 h `STALE_AFTER_MS`) → none
- Render: `app/src/AgentLight.tsx` `export function AgentLight` (aria/tooltip via `trafficLightKey`)
- State machine: lib.rs `fn apply_hook` (AgentRunState); `idle_prompt` Notification promotes done → needs-input (test `idle_prompt_promotes_done_to_needs_input`)
- Push/reload: lib.rs `fn emit_agent_run_event` (event `pane:agent-run`); `fn pane_agent_states` (F5 re-hydrate)
- Pane close: see [pane-close-cleanup](pane-close-cleanup.md)
- Hook spec: `hooks/claude-code.json` keys `Notification`, `Stop`, `UserPromptSubmit`
- Install: `ymux setup-hooks --agent claude --source bundled` → `app/src-tauri/cli/src/hooks.rs` `"bundled" => Ok(bundled_spec_for(agent_id))`
- Rule #1 log line: `app/src-tauri/cli/src/main.rs` `hook_dlog(... "notification type={ntype} pane={} req_id=...")` — `notification_text` never logged
- Log read: CLI `ymux dev debug-log-tail` → RPC `"dev.debug-log-tail"` (rpc_server.rs)
