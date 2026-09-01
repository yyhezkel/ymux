# Releasing ymux

Cutting a new version is a six-step manual checklist for now. CI is on
the roadmap; until then this is your runbook.

## One-time: the 0.5.0 rename cut

The winmux → YMUX rename (2026-08-18) changed the bundle identifier
from `com.winmux.app` to `com.ymux.app`. That identifier IS the
MSI/NSIS upgrade key, so Windows treats 0.5.0 as a **different
product**: it installs alongside the old winmux instead of replacing
it, and the 0.4.5 updater will not offer it as an in-place upgrade.
Nothing can shim this — plan for it instead:

- Cut the rename release as **0.5.0**, never 0.4.6. A patch-looking
  version number on a side-by-side install is the confusing case.
- The release notes must say, in this order: install YMUX, launch it
  once and confirm your workspaces are there (the first launch renames
  `%APPDATA%\winmux` → `%APPDATA%\ymux`), *then* uninstall the old
  winmux from Programs and Features. Uninstalling first is harmless;
  running both at once is not.
- The step-5 `manifest.json` edit is what actually publishes the
  update. Until it lands, users on 0.4.5 stay on 0.4.5 — which is the
  safe default while the notes are being written.

## 1. Bump the version

Update `version` in:

- `app/src-tauri/Cargo.toml` (workspace `[package]`)
- `app/src-tauri/cli/Cargo.toml`
- `app/src-tauri/mcp/Cargo.toml`
- `app/src-tauri/tauri.conf.json` (`"version"` field)
- `app/package.json`

Commit as `chore: bump to vX.Y.Z`.

## 2. Build the release

```pwsh
cd app
powershell -ExecutionPolicy Bypass -File ./scripts/build-release.ps1
```

This wrapper sets `RUSTFLAGS=--remap-path-prefix=...` so embedded
panic-location strings don't carry the build machine's `$HOME`. RUSTFLAGS is
part of Cargo's fingerprint, so a compilation made without the remap can never
be reused -- there is nothing to delete by hand.

It takes two optional switches:

- `-Bundles "nsis"` -- skip the WiX/MSI leg. `build-windows.yml` passes this on
  `workflow_dispatch` runs; a real tag cut must produce both, so leave it off.
- `-Timings` -- write Cargo's HTML timing report to
  `app/src-tauri/target/cargo-timings/cargo-timing.html`. Use it before
  claiming the build got slower.

The output is:

- `app/src-tauri/target/release/app.exe`
- `app/src-tauri/target/release/bundle/msi/ymux_X.Y.Z_x64_en-US.msi`
- `app/src-tauri/target/release/bundle/nsis/ymux_X.Y.Z_x64-setup.exe`

Verify the scrub:

```pwsh
grep -aoc $env:USERNAME app/src-tauri/target/release/app.exe
# should be 0
```

`build-windows.yml` now runs this check itself ("Assert the developer path
scrub"), so a CI-produced release has already been verified; do it by hand only
for a local cut.

## 3. Tag

```pwsh
git tag -a vX.Y.Z -m "ymux vX.Y.Z — <one-line summary>"
git push origin vX.Y.Z
```

## 4. Publish the GitHub Release

```pwsh
gh release create vX.Y.Z `
  --title "ymux vX.Y.Z" `
  --notes-file release_notes.md `
  app/src-tauri/target/release/bundle/msi/ymux_X.Y.Z_x64_en-US.msi `
  app/src-tauri/target/release/bundle/nsis/ymux_X.Y.Z_x64-setup.exe
```

## 4¼. Attach the macOS dmgs

`build-macos-intel.yml` is NOT tag-triggered — it runs on push to `main` and
`workflow_dispatch`. Grab the dmgs from the most recent green run on `main`
whose commit matches the tag's tree (a docs/manifest-only diff is fine — those
files are not embedded in the app), or dispatch it fresh:

```pwsh
gh run download <run-id> -D mac-dmgs
# artifacts: ymux-macos-{x64,arm64}-dmg/ymux_{x64,arm64}_macos13.dmg
```

Rename to `ymux_X.Y.Z_x64.dmg` / `ymux_X.Y.Z_aarch64.dmg` and
`gh release upload vX.Y.Z <both>`. The release notes' macOS section must carry
the `xattr -dr com.apple.quarantine` step — the bundles are ad-hoc signed and
Gatekeeper blocks the first launch without it.

In step 5, also fill the `dmg_x64_*` / `dmg_aarch64_*` manifest fields (url,
sha256, size — same `gh release view` digest workflow). The desktop updater
does not consume them yet (in-app update is Windows-only; the macOS banner
links to the release page), but the manifest is the record a future macOS
self-update will read.

## 4½. Bump hook specs (only when hooks changed)

If this release changes any of `hooks/*.json` (added a Claude Code
event, switched a matcher, renamed a subcommand…):

1. Bump `ymux_hooks_version` in the affected `hooks/<agent>.json`
   file (semver: bump major if events were removed/renamed, minor for
   additive changes, patch for matcher tweaks).
2. Bump the matching `BUNDLED_CLAUDE_VERSION` constant in
   `app/src-tauri/cli/src/hooks.rs` so the bundled fallback stays in
   sync (and matches what `setup-hooks --source bundled` writes).
3. In `manifest.json`, bump the matching `hooks.<agent>.version`
   field so the desktop's outdated-check picks up the new version
   on the next SSH connect.

The desktop's `check_remote_hooks` (in `updater.rs`) compares each
remote's `~/.claude/settings.json::ymux_meta.hooks_version` against
manifest's `hooks.claude-code.version`. When a server is on an older
version AND the user hasn't dismissed that version (Settings → Claude
→ Hook updates), a banner fires.

## 5. Update `manifest.json`

The updater (`updater.rs`) polls
`https://raw.githubusercontent.com/yyhezkel/ymux/main/manifest.json`
on startup and surfaces a banner when a newer version is available.
**This file must be updated for every release** — otherwise existing
installs won't know there's an update.

Workflow:

1. Get the SHA256s and sizes of the assets you just uploaded:

   ```pwsh
   gh release view vX.Y.Z --json assets
   ```

   Look for the `digest` (format `sha256:abcdef…`) and `size` fields.

2. Edit `manifest.json` at the repo root:

   ```json
   {
     "version": "X.Y.Z",
     "released_at": "<ISO8601 UTC timestamp>",
     "notes_url": "https://github.com/yyhezkel/ymux/releases/tag/vX.Y.Z",
     "msi_url": "https://github.com/yyhezkel/ymux/releases/download/vX.Y.Z/ymux_X.Y.Z_x64_en-US.msi",
     "msi_sha256": "<from gh release view>",
     "msi_size": <bytes>,
     "nsis_url": "https://github.com/yyhezkel/ymux/releases/download/vX.Y.Z/ymux_X.Y.Z_x64-setup.exe",
     "nsis_sha256": "<from gh release view>",
     "nsis_size": <bytes>,
     "min_supported_version": "<oldest version that should be told to upgrade>"
   }
   ```

3. Commit + push to `main`. `raw.githubusercontent.com` picks up changes
   within ~1 minute.

## 6. Verify the update banner

On a previous-version install:

1. Wait for the 3-second startup grace period; the updater task fires
   after that.
2. Look for the floating banner at the bottom centre: `ymux X.Y.Z is
   available — current X.Y.(Z-1)`. "Release notes" link should open
   the new tag's page.
3. Alternatively: Settings → Updates → "Check now" force-runs the
   poll without waiting.

If the banner doesn't appear:

- `ymux dev check-updates --pretty` from a terminal shows the parsed
  manifest + the version comparison result + the last-check ISO.
- Check `%APPDATA%\ymux\debug.log` for any `updater: fetch … failed`
  lines — typically DNS, certificate, or proxy issues.

## Caveats

- **Code-signing**: the MSI / NSIS bundles are not signed yet.
  SmartScreen will warn on first launch. Adding signing to the release
  flow is a future task — when it lands, the manifest schema may grow
  a `signature` field.
- **Auto-install**: only the *notification* part of update flow is
  implemented. Users still download the MSI manually. Real
  auto-install would need signing keys + a verified-download path.
- **Old versions**: bumping `min_supported_version` doesn't *force*
  an upgrade — it's just a hint the future updater can use to refuse
  to load workspace files written by versions newer than itself.
