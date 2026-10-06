# macOS signing + notarisation
- config: `app/src-tauri/tauri.macos.conf.json:14-19` — `bundle.macOS`: `hardenedRuntime: true`, `entitlements: Entitlements.plist`, `signingIdentity: "-"` (ad-hoc default)
- entitlements: `app/src-tauri/Entitlements.plist`
- CI Developer-ID path: `.github/workflows/build-macos-intel.yml` — `notarytool submit` (:367 app zip, :485 dmg), `stapler staple` (:373, :488), `spctl -a -vv` Gatekeeper gate (:440; ad-hoc reject = warning, Developer-ID reject = error :446-448)
- identity override via `APPLE_SIGNING_IDENTITY` secret; config stays `"-"` so fork/ad-hoc builds still sign
- blocker: Apple Developer ID membership declined — `docs/DECISIONS.md` § "2026-08-23 — Mac builds stay ad-hoc signed" (option B; developer-id path never executed); live Gatekeeper acceptance never proven
- rule: no local builds (CLAUDE.md Rule #17) — proof only from a CI run with secrets set
