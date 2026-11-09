<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: ymux-2026-08-23-manual-3, ymux-2026-08-23-manual-4 -->

**Result:** collapse 4 of the 5 formatters onto `fmtBytes` and keep the File Manager's compact one. Enable Dependabot monthly, grouped, `lockfile-only`; manifest-editing PRs fail CI.

## 1. Byte formatters

Only these five exist. Go, Rust and `backend/web` have none; `claudePricing.ts:170` formats tokens.

| n | insightsFmt:6 | transferStore:171 | FileEditor:346 | FileManager:465 | Settings:80 |
|---|---|---|---|---|---|
| 0 | `0` | `0 B` | `0 B` | `0B` | `1 KB` |
| 512 | `512 B` | `512 B` | `512 B` | `512B` | `1 KB` |
| 1023 | `1023 B` | `1023 B` | `1023 B` | `1023B` | `1 KB` |
| 1024 | `1.0 KB` | `1.0 KB` | `1.0 KB` | `1.0K` | `1 KB` |
| 1536 | `1.5 KB` | `1.5 KB` | `1.5 KB` | `1.5K` | `2 KB` |
| 1048576 | `1.0 MB` | `1.0 MB` | `1.0 MB` | `1.0M` | `1.0 MB` |
| 1.5e9 | `1.4 GB` | `1.4 GB` | `1430.5 MB` | `1.4G` | `1430.5 MB` |

- **Bugs:** unitless `0` (`insightsFmt:7`, also `0/s`); no GB tier in Editor and Settings; false comment at `transferStore.ts:170`.
- **Intentional:** File Manager's compact form (60px column, `App.css:3109`). Settings' KB floor is deliberate but goes once unified.
- **Tests:** none direct. `insightsReport.test.ts:106,124-125,130` pins non-zero output only.

**Options:** full collapse clips or changes the File Manager column. `fmtBytes(n, compact?)` adds an API for one caller. Renaming the module adds 4 importer edits for no behaviour change.

**Recommendation:**
- Change zero to `0 B`.
- `transferStore` re-exports `fmtBytes`; Editor and Settings import it.
- Leave File Manager alone; no rename; add `insightsFmt.test.ts`.
- Touches 4 source files, 1 test, 3 vault pages plus lock; about 25 lines.

**Invalidated if:** Yossi wants one format everywhere, or always-one-decimal (`50.0 MB` becomes `50 MB`).

## 2. Dependabot

- **npm:** 15 of 20 direct deps outdated (5 out of range). 11 advisories (2 critical, 7 high), all `fixAvailable: true`. Direct: dompurify, markdown-it, vite.
- **cargo:** 34 of 39 direct outdated, 15 semver-major among those locked. 31 locked crates have OSV hits.
- **Dependabot can't fix:** `tauri` GHSA-7gmj-67g7-phm9 (fixed 2.11.1, blocked by the pin at `Cargo.toml:52`; needs a manual bump), `russh` 0.49 (23 advisories), `zip` 2 (fixed 9.x).
- **Lock sync:** package-lock root is 0.4.5, package.json 0.5.1. `npm ci` tolerates it. `Cargo.lock` lacks `rusqlite` (`Cargo.toml:82`); CI logs `Adding rusqlite v0.32.1`, and no workflow uses `--locked`.
- **Vault gate:** manifests are covered (`build-glue.md:4,10`, `cli.md:9`, `backend-rpc.md:8`), lockfiles aren't. The hash check (`vault-check.mjs:187-197`) still runs with `[vault-skip]`, and a bot branch can't re-stamp.
- **Other gates:** staged CLI, Windows and macOS gates run normally. rustc 1.95.0 may reject bumps needing newer. gomod is excluded because `ci-windows.yml` demands a blob rebake.
- **Musl blob:** already diverged; CI staged `fa9d0c…`, committed `4dbbc9…`. Drift only raises a notice, so lock bumps surface at release rebake.
- **Repo state:** alerts and automated fixes are off.

**Options:**
- Security-only: misses in-range drift.
- `auto`, grouped: edits manifests, red.
- Monthly `lockfile-only`: about 2 green PRs a month. **Chosen.**

```yaml
  - package-ecosystem: npm
    directory: /app
    schedule: {interval: monthly}
    versioning-strategy: lockfile-only
    open-pull-requests-limit: 2
    ignore: [{dependency-name: "@tauri-apps/*"}]
    groups:
      npm: {applies-to: version-updates, patterns: ["*"]}
      npm-security: {applies-to: security-updates, patterns: ["*"]}
  - package-ecosystem: cargo
    directory: /app/src-tauri
    schedule: {interval: monthly}
    versioning-strategy: lockfile-only
    open-pull-requests-limit: 2
    ignore: [{dependency-name: "tauri*"}]
    groups:
      cargo: {applies-to: version-updates, patterns: ["*"]}
      cargo-security: {applies-to: security-updates, patterns: ["*"]}
```

`lockfile-only` already freezes `=2.10.3`, `~2.10.0` and `~5.6.2`. The ignores cover the `^2` Tauri siblings.

**First:** enable alerts and security updates (the only route for npm transitives); run `npm install --package-lock-only` and `cargo update -w` (untested). Later add `--locked`. Touches `dependabot.yml` plus two lockfiles; about 25 lines.

**Invalidated if:** a security PR edits a manifest (docs say it honours the strategy; unobserved), or checks become required.

## Prior reports

Manual-3 differs: it collapses File Manager, renames the module, keeps `0`. Manual-4 picked `auto`-style Option A and missed the hash gate, so its PRs would fail; alerts were unchecked. Lock-drift findings agree.
