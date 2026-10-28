# Browser hello hydration — daemon snapshot → App agent rows

- Daemon hello: `app/src-tauri/server/internal/term/events.go:138` `func (r *HookRegistry) hello`; lines 151-152 add a run event + `BriefEntry` for EVERY non-released registered session, touched by a hook or not
- Zero-value entry: `BriefEntry` (`internal/agent/brief.go:83`) → `{brief: null, last_prompt: null, prompt_ms: null, session_ended: false, seq: 0}`
- `seq` semantics: bumped on every hook mutation (`internal/term/hookdispatch.go:271,291,302`); `pane:brief` published only when it moved (`hookdispatch.go:126`) → `seq 0` = never written; live events always `seq >= 1`
- Browser answer: `app/src/backend/web.ts:1123-1124` `pane_agent_states` / `pane_briefs` read `this.hello` (first hello; boot only — App calls them once, `app/src/App.tsx:3965,3984`)
- Desktop parity: `app/src-tauri/src/lib.rs:2902` `fn pane_briefs` clones `AppState.briefs` = hook-written entries only
- Card path: `allPaneAgentRows` (App.tsx:1281) → `inQueue` (`app/src/queueModel.ts:106`, `light !== null || brief !== null`) → `queueStatus` fallthrough `"done"` (queueModel.ts:52) → `workspaceCardInfo` (App.tsx:2958) text `sidebar.card.status.done` ("הסוכן סיים את התור")
- Consequence pre-fix: any plain shell session alive at page load hydrates a non-null empty brief → card reads "agent done" (ticket ymux-app-src-app-tsx-3)
- Fix decision (ticket ymux-app-src-app-tsx-3): filter `seq > 0` at the browser boundary (`app/src/backend/web/hello.ts` `touchedEntries`), not in the Go hello
  - alternative A: skip zero entries in `events.go` hello → rejected: server change forces committed-blob rebake from CI artifact (CLAUDE.md "Rebaking the server")
  - alternative B: tighten `inQueue` in `queueModel.ts` → rejected: downstream guard shared with desktop; boundary fix restores desktop shape instead
- Canonical prose: `docs/vault/frontend-lib.md` § "The browser arm" (Events bullet); `docs/vault/server-go.md` § term/feed.go + events.go
