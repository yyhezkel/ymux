# Device scopes on huma ops — where each op is gated
- Enforcement: `app/src-tauri/server/internal/api/huma.go` `opScopes` map + `bearerMiddleware` (`huma.go:96-122`); op absent from map → any valid token passes; owner/shared token bypasses scopes; device lacking scope → 403 `missing scope: <s>`; scope check runs only when `s.deps.Chat != nil` (`huma.go:113`) — no Chat → no device tokens accepted either (`tokenOK`)
- Scope set: `app/src-tauri/server/internal/auth/scopes.go:18-34`; fail-open `AllScopes` `scopes.go:41-47` excludes `shell:attach`; `ParseScopes` "", "all", malformed → `AllScopes`
- Op ids: files `internal/files/huma.go:74,96,118,144,166,208,218,228,238,249` (list, read, upload, download, delete, mkdir, rename, copy, archive, unzip); logs `internal/logs/huma.go:49,61,87` (list, read, stream); term `internal/term/huma.go:84-181`; mobile `internal/api/mobile.go:138-347`
- Raw stdlib routes gate separately: insights `api/api.go:178` (`insights:read`, GET only); daemon log `/api/v2/logs/daemon` = `internal/insights/service.go:60` → insights auth
- Scope-test helpers: `internal/api/term_test.go` `pairDevice` (request → approve → redeem), `termDo`; set grants via owner `PUT /api/v2/devices/{id}/scopes`
- `NormalizeScopes` drops unknown names (incl. "all" inside a list) → `["all","shell:attach"]` stores `["shell:attach"]` only; desktop `app/src-tauri/src/pairing.rs` `with_shell` expands to the full list first

## Decision (ticket ymux-server-internal-api-huma, 2026-10-27)
- Logs ops → `insights:read`; context: no logs scope exists, new scope names out of scope
- Alternatives: `files:read` (logs are files on disk) — rejected: logs are observability, and `/api/v2/logs/daemon` already needs `insights:read` (`docs/vault/frontend-lib.md:173`); new `logs:read` — rejected: out of scope, would not be in already-narrowed devices' grants
- Files: list/read/download → `files:read`; upload/delete/mkdir/rename/copy/archive/unzip → `files:write`
