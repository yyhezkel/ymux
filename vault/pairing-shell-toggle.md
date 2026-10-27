# Browser terminal-access toggle (Phase 113) — anchors
- canonical prose: `docs/vault/backend-remote.md` § `pairing.rs`, `docs/vault/server-go.md` § "Browser-pairing approve validates scopes", `docs/vault/frontend-panes.md` § `MobilePairing.tsx`
## Desktop UI
- checkbox state: `hasShell` = device-list `scopes` string contains `"shell:attach"` (`app/src/MobilePairing.tsx:58`)
- disabled unless active: `disabled={d.status !== "active"}` (`app/src/MobilePairing.tsx:440`)
- grant asks `window.confirm(t("mobile.shell.confirm"))`; untick = no confirm (`app/src/MobilePairing.tsx:270`)
## Desktop backend
- `mobile_pairing_set_shell` GET → `with_shell` → PUT `/api/v2/devices/{id}/scopes`, reply re-checked (`app/src-tauri/src/pairing.rs:254`)
- debug.log line `[PAIRING] device <id> terminal access granted|withdrawn` (`app/src-tauri/src/pairing.rs:272-275`, `crate::log_info("PAIRING", …)`) — device id only, no token
- allow-list: `/api/pairing/*` + `is_device_scopes_path` (`app/src-tauri/src/pairing.rs:33`, `daemon_curl` :65); owner token read remote-side via `$(cat ~/.ymux/server/token)`, never desktop-side
## Daemon
- PUT scopes: owner-only, `auth.NormalizeScopes`, 404 `device not found or not active` for pending/revoked (`app/src-tauri/server/internal/api/mobile.go:186`, :206)
- GET scopes expands "all" → explicit list (`internal/api/mobile.go:156`)
- device list `GET /api/pairing/devices` (admin) → `{devices:[{status, scopes,...}]}` — scopes = stored string (`internal/chat/chat_pairing.go:78`)
- issue (`POST /api/pairing/issue`) → `pending`; redeem (`POST /api/pairing/redeem`, public) → `active`; `DELETE /api/pairing/devices/{id}` → revoked (`internal/chat/chat_pairing.go:32`, :106)
- shell gate: raw routes `Service.gate` → 403 `forbidden: shell:attach not granted` (`internal/term/service.go:225`, :236); huma `term-*` ops → 403 `missing scope: shell:attach` (`internal/api/huma.go:27`, :116)
- `"all"` never implies `shell:attach` (`internal/auth/scopes.go:54` GrantableScopes vs AllScopes)
## Browser boot
- `initBackend`: 403 → `no-shell`, 401 → `login` (`app/src/backend/index.ts:45`); first gated call `GET /api/v2/settings` (`app/src/backend/web.ts:354`, raw gate `internal/term/service.go:171`)
## Live proof
- daemon half: committed blob on a side port ([howto/run-committed-server-blob](howto/run-committed-server-blob.md)); desktop half (checkbox, confirm, debug.log, real browser reload) = human only
- issue default scopes = `"all"` (`app/src-tauri/server/internal/chat/chat_pairing.go:44`); `.sh` smoke scripts sit outside docs vault coverage (`scripts/vault-check.mjs:51` CODE_EXT)
- outcome verdict = desktop half: live-verify rows go `unprovable-by-design` (human at SIGNOFF); the daemon smoke is supporting evidence, never the PASS
