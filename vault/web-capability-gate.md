# Web-mode command gating — `backend.can(cap)`

- Symptom shape: component calls a desktop command; `WebBackend.call` has no `handlers` entry → rejects `not available in the browser: <cmd>` (`app/src/backend/web.ts#not available in the browser: ${cmd}`), warned once
- Rule (`docs/vault/frontend-lib.md` § "backend.can(cap)"): reaching an unhandled name = a missing `can()` gate → hide/skip the entry point, never add a fake handler
- Capability list: `ALL_CAPABILITIES` in `app/src/backend/types.ts#export const ALL_CAPABILITIES`; `Capability` type derives from it
- Desktop: `TauriBackend.caps = new Set(ALL_CAPABILITIES)` (`app/src/backend/tauri.ts#new Set(ALL_CAPABILITIES)`) → a new cap is ON on desktop with no tauri.ts edit
- Browser: `WebBackend.caps` = explicit short set (`app/src/backend/web.ts#readonly caps: ReadonlySet<Capability>`) → a new cap is OFF in the browser with no web.ts edit
- Recipe: add `"<cap>"` to `ALL_CAPABILITIES` + `backend.can("<cap>")` at the call site; update `docs/vault/frontend-lib.md` "Gated today" list + `node scripts/vault-check.mjs --write`
- Ported instead of gated: Context Rail `session_context_list` (`app/src/ContextRail.tsx#const refetch = async`) — Phase 117 (1b7f6f1a, 2026-10-07) added `WebBackend` handler `session_context_list: (a) => api("GET", ...)` → daemon `GET /api/v2/context/sessions` (`app/src-tauri/server/internal/term/service.go#GET /api/v2/context/sessions`); FOLLOWUPS entry of 2026-10-06 predates it
- Before gating: `grep -n '<cmd>:' app/src/backend/web.ts` — a handler may already exist; a stale deployed bundle/daemon shows the old error
- Test limits: `web.ts` imports extensionless paths → not loadable by `node --experimental-strip-types --test`; gate proof = tsc on CI + grep-structural checks
