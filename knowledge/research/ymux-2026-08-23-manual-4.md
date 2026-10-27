# ymux-2026-08-23-manual-4 — research report

## Question
The request asks to extend `.github/dependabot.yml` from `github-actions` only to the `npm` (app/) and `cargo` (app/src-tauri/) ecosystems without setting off a flood of unreviewed PRs. It also asks whether `app/package-lock.json` carries a stale root `version`. This report covers the investigation only. No file was changed, and no `npm`/`cargo` command was run (goal "Out of scope"). Registry state was read over plain HTTPS GETs on 2026-10-27.

### Q1 — Which ecosystems does `.github/dependabot.yml` cover today, and what would adding `npm` (app/) and `cargo` (app/src-tauri/) entries change
### Q2 — How many open dependency updates would the `npm` and `cargo` ecosystems raise against the current `app/package-lock.json` and `Cargo.lock`
### Q3 — Does the root `version` in `app/package-lock.json` differ from `app/package.json`, and which tooling or release step writes it
### Q4 — What staging controls (schedule, open-pull-requests-limit, grouping, ignore rules) keep the first wave reviewable, given the SHA-pinned actions and the rule that builds run only on CI

## Findings

### Q1
**Answer.** Today the file has one `updates` entry, `github-actions` at `/`. It runs monthly, and a single group `actions` with `patterns: ["*"]` batches every action bump into one PR. A comment in the file says npm and cargo are deliberately left out. Adding the two entries would cover the `app/` npm tree and the whole Rust workspace. Each entry would open PRs that change manifests and lockfiles, and those PRs trigger the full CI matrix.

**Evidence.**
- `.github/dependabot.yml:4-12`: `version: 2` defines one entry, `package-ecosystem: github-actions`, `directory: "/"`, `interval: monthly`, `groups.actions.patterns: ["*"]`.
- `.github/dependabot.yml:21-23`: "npm and cargo are deliberately NOT enabled here yet … Tracked in BACKLOG.md." The ledger line is `BACKLOG.md:84`.
- Several manifests exist (`git ls-files` output). An `npm` entry at `/app` covers only `app/package.json` + `app/package-lock.json`. The `sdk-gen/` and `sdk/typescript/` npm trees and the Go module `app/src-tauri/server/go.mod` stay uncovered unless they get their own entries or are listed under `directories:`.
- A `cargo` entry at `/app/src-tauri` covers the whole workspace: 11 members (`app/src-tauri/Cargo.toml:1-2`) share one `app/src-tauri/Cargo.lock`.
- CI impact: dependabot PRs target `main` and touch lockfiles, which are not on the `paths-ignore` list (`.github/workflows/ci-windows.yml:28-40`). Each PR therefore runs ci-windows, the three parallel jobs described in CLAUDE.md § CI. It also runs `build-macos-intel.yml`, which has a `pull_request` trigger at `:63`. Once a cargo PR merges, `warm-rust-cache.yml` also runs, because it triggers on push to main touching `app/src-tauri/Cargo.lock`/`Cargo.toml` (`.github/workflows/warm-rust-cache.yml:22-28`).
- By default, version updates keep "All dependencies explicitly defined in a manifest" up to date, i.e. direct dependencies only (docs.github.com options reference, retrieved 2026-10-27).

**Confidence.** High. Raising it further would take a dry run of the config on a fork.

### Q2
**Answer.** Of 20 direct npm dependencies, **15** have a newer release. **10** of those fit the current semver range in `app/package.json`; **5** fall outside it (a major bump or a pinned range). Of 39 direct crates.io dependencies across the cargo workspace, **34** have a newer release. **17** fit their requirement and **17** fall outside it. Version updates cover direct dependencies only (Q1), so without grouping that is up to 49 candidate PRs (15 + 34). The default open-PR limit of 5 per ecosystem (Q4) means only the first 5 of each would open at once.

**Evidence (npm).** Locked versions are from `app/package-lock.json`, and latest versions are from `https://registry.npmjs.org/-/package/<name>/dist-tags` (retrieved 2026-10-27). Direct dependencies are listed in `app/package.json` (15 `dependencies` + 5 `devDependencies`).

| package | range | locked | latest | kind |
|---|---|---|---|---|
| @tauri-apps/api | ~2.10.0 | 2.10.1 | 2.12.4 | out of range (minor; ~ pin) |
| @tauri-apps/plugin-dialog | ^2 | 2.7.1 | 2.8.3 | in range |
| @tauri-apps/plugin-opener | ^2 | 2.5.3 | 2.7.2 | in range |
| bidi-js | ^1.0.3 | 1.0.3 | 1.1.0 | in range |
| dompurify | ^3.4.11 | 3.4.11 | 3.4.17 | in range |
| lucide-solid | ^1.23.0 | 1.23.0 | 1.62.0 | in range |
| markdown-it | ^14.2.0 | 14.2.0 | 15.0.3 | major |
| marked | ^18.0.4 | 18.0.4 | 18.1.2 | in range |
| qrcode-generator | ^1.4.4 | 1.4.4 | 2.0.4 | major |
| solid-js | ^1.9.3 | 1.9.12 | 1.9.17 | in range |
| @tauri-apps/cli (dev) | ^2 | 2.10.1 | 2.12.4 | in range |
| @types/markdown-it (dev) | ^14.1.2 | 14.1.2 | 14.2.0 | in range |
| typescript (dev) | ~5.6.2 | 5.6.3 | 7.0.2 | major (~ pin) |
| vite (dev) | ^6.0.3 | 6.4.2 | 8.3.5 | major |
| vite-plugin-solid (dev) | ^2.11.0 | 2.11.12 | 2.11.14 | in range |

These 5 are already at latest: `@xterm/xterm` 6.0.0, `@xterm/addon-fit` 0.11.0, `@xterm/addon-webgl` 0.19.0, `@xterm/addon-clipboard` 0.2.0 and `html2canvas` 1.4.1. The lockfile holds 166 packages (`python3 json.load(app/package-lock.json)['packages']` → 166 entries besides the root). Transitive packages are not proposed unless `allow: dependency-type: all` is set.

**Evidence (cargo).** Direct dependencies come from every workspace `Cargo.toml` (root, `cli/`, `mcp/`, `crates/*`), parsed with a regex over `[*dependencies]` sections; path-only entries are excluded. Locked versions are from `app/src-tauri/Cargo.lock`, which has 737 `[[package]]` entries, 726 of them registry-sourced and 0 git-sourced (`grep -c`). Latest versions are `max_stable_version` from `https://crates.io/api/v1/crates/<name>` (retrieved 2026-10-27).

- **In requirement (17):** anyhow 1.0.102→1.0.104, arboard 3.6.1→3.7.0, async-trait 0.1.89→0.1.92, bytes 1.11.1→1.12.1, chrono 0.4.44→0.4.45, clap 4.6.1→4.6.7, futures-util 0.3.32→0.3.34, notify-rust 4.16.1→4.18.2, open 5.3.4→5.4.5, serde 1.0.228→1.0.229, serde_json 1.0.149→1.0.154, tauri-build 2.5.6→2.7.4, tauri-plugin-dialog 2.7.1→2.8.3, tauri-plugin-opener 2.5.3→2.7.2, tokio 1.52.1→1.54.1, tokio-util 0.7.18→0.7.20, zeroize 1.8.2→1.9.1.
- **Outside requirement (17):** base64 0.22→0.23, bollard 0.17→0.21, dirs 5→7, hmac 0.12→0.13, portable-pty 0.8→0.9, rand 0.8→0.10, rusqlite 0.32→0.41, russh 0.49→0.65, russh-sftp 2→3, sha2 0.10→0.11, sysinfo 0.32→0.39, **tauri =2.10.3→2.12.5**, ts-rs 8→12, ureq 2→3, whoami 1→2, winreg 0.55→0.56, zip 2→9.
- **Already at latest (5):** russh-keys 0.49.2, tempfile 3.27.0, tracing 0.1.44, tracing-subscriber 0.3.23, windows-sys 0.61.2.

**Anomaly that affects the first cargo PR.** `rusqlite = { version = "0.32", features = ["bundled"] }` is declared at `app/src-tauri/Cargo.toml:82`, added by `f1cda228` (2026-10-06, which touched only `Cargo.toml`, per `git show --stat`). `grep -n rusqlite app/src-tauri/Cargo.lock` returns nothing, and the last commit to the lockfile is `1be9dc5a` (v0.5.1 bump), so the committed lockfile is out of sync with the manifest. CI does not catch this because it runs `cargo test --workspace` without `--locked` (`.github/workflows/ci-windows.yml:279`, `build-macos-intel.yml:206`). The first dependabot cargo PR resolves against the manifest, so it will most likely also add `rusqlite`/`libsqlite3-sys` and their dependencies to `Cargo.lock`. That makes its diff larger than the one bump it names. (This is inferred: dependabot regenerates the lockfile; it was not observed.)

**Confidence.** High for the counts as of 2026-10-27. Medium for the exact PR count, which depends on dependabot's per-ecosystem logic (e.g. whether `@tauri-apps/api` gets a range increase) and on cooldown (Q4). A fork dry run would raise it.

### Q3
**Answer.** Yes, and the mismatch is now bigger than the request says. `app/package-lock.json` has root `version` **0.4.5** in two places (top-level and `packages[""]`), while `app/package.json` is at **0.5.1**, not 0.5.0. No release step writes that field. `docs/RELEASING.md` § 1 lists the files to bump, and `package-lock.json` is not among them. `npm ci`, which every CI workflow runs, never rewrites the lockfile. So the field moves only when someone runs `npm install` locally or edits it by hand. The last time that happened was the manual fix `b431f86c`.

**Evidence.**
- `app/package-lock.json:3` and `:9`: `"version": "0.4.5"`. `app/package.json:3`: `"version": "0.5.1"`. These are the same on `origin/main` @ `6070488f` (`git show origin/main:…`).
- `docs/RELEASING.md:26-36`: the bump list is the three `Cargo.toml` files, `tauri.conf.json` and `app/package.json`; `package-lock.json` is absent.
- `git show --stat 1be9dc5a` ("chore: bump to v0.5.1") touched `app/package.json` and `app/src-tauri/Cargo.lock` but not `app/package-lock.json`. `Cargo.lock` follows the bump because cargo rewrites workspace-member versions on any build. npm has no equivalent.
- `git log -- app/package-lock.json`: the last commit is `b431f86c` (2026-07-28), "chore: sync package-lock.json to the v0.4.5 bump … Left behind by the version bump the same way Cargo.lock was (339dd09). No dependency changes — only the "version" fields." So this drift has happened before, and it was fixed by hand.
- CI uses `npm ci` (`.github/workflows/ci-windows.yml:162`, `build-windows.yml:77`, `build-macos-intel.yml:167`, `warm-rust-cache.yml:79`). With lockfileVersion 3, `npm ci` does not fail on a root-version mismatch: CI has stayed green with the drift since `f2f1b278` (0.5.0, 2026-08-19).
- Side effect: the first npm dependabot PR regenerates the lockfile. It will most likely also rewrite the root `version` to 0.5.1 (inferred, not observed), so the mismatch could disappear inside an unrelated bump PR unless it is fixed first.

**Confidence.** High for the mismatch and for the release-checklist gap. Medium for whether dependabot rewrites the root field.

### Q4
**Answer.** Keep the shape that already works for actions: a monthly schedule plus groups. Add semver-aware grouping, and set `ignore` rules on the pins that are deliberate in this repo. Concretely, per new entry:
1. **schedule** `interval: monthly`, matching the existing entry (`.github/dependabot.yml:8-9`). Each PR costs a full CI run (Q1), and builds may run only on CI (CLAUDE.md Rule #17), so fewer and larger PRs cost less than many small ones.
2. **groups** split by `update-types`: one group for `minor` + `patch` (one PR per ecosystem per month) and nothing grouped for `major`. Majors then arrive one per PR and are reviewed individually. In npm, a `dependency-type: development` group can carry the 5 dev deps separately.
3. **ignore** with `update-types: ["version-update:semver-major"]`, at least for the first wave. The 17 cargo and 4 npm majors (Q2) are the "unreviewed tree" the comment worries about. Lift the ignore per dependency later.
4. **Tauri pin.** `tauri` is pinned `=2.10.3` (`app/src-tauri/Cargo.toml:52`), and CLAUDE.md:40 requires a manual `Window::add_child` verification and a live Browser-window smoke test for any bump. Ignore `tauri` in cargo and `@tauri-apps/api` / `@tauri-apps/cli` in npm, or group them together in their own group, so a JS-only Tauri bump never lands without the matching crate bump. The `@tauri-apps/plugin-*` npm packages and `tauri-plugin-*` crates are a paired set in the same way.
5. **typescript** is pinned `~5.6.2` on purpose: "checks the same minor developers use" (`.github/workflows/ci-windows.yml:164-165`). Ignore its majors.
6. **open-pull-requests-limit.** The default is 5 per entry. With grouping on, 2–3 is enough and keeps the review queue bounded.
7. **cooldown.** The default is 3 days. A larger `semver-major-days` holds back fresh majors (docs, retrieved 2026-10-27).
8. Fix the lockfile drift first, outside dependabot: Q3's root `version` and Q2's missing `rusqlite` lock entries. Otherwise the first grouped PR mixes that cleanup with the dependency bumps.

The SHA-pinned actions are not affected. They stay in the existing `github-actions` entry, and the `dtolnay/rust-toolchain` warning (`.github/dependabot.yml:13-19`) still applies to that entry only. `rust-toolchain.toml` (rustc 1.95.0) is not managed by the `cargo` ecosystem's version updates.

**Evidence.**
- `.github/dependabot.yml:8-12` shows the existing monthly + group pattern.
- From the GitHub Docs options reference (retrieved 2026-10-27): the open-pull-requests-limit default is 5 ("If five pull requests with version updates are open, no further pull requests are raised"); `groups` support `patterns`, `exclude-patterns`, `dependency-type` (development/production), `update-types` (minor/patch/major) and `applies-to`; `ignore.update-types` accepts `version-update:semver-major`; `cooldown` has `default-days` (3 when unset) and `semver-major-days`; `versioning-strategy` supports npm and cargo, and `auto` uses `increase` for apps.
- Pins: `app/src-tauri/Cargo.toml:52`, `CLAUDE.md:40`, `.github/workflows/ci-windows.yml:164-165`.

**Confidence.** Medium-high. The config semantics come from the docs, and the repo-specific pins are observed. A first-cycle trial on a fork, or watching the first month's PRs, would raise it.

## Options
- **A — Grouped minor/patch, majors ignored, monthly, limit 2–3 per ecosystem.** About 2 PRs a month (one per ecosystem) and a few CI runs. The risk is that majors stay stale until someone lifts the ignore one at a time. Evidence: Q2 counts, Q4 docs.
- **B — Grouped minor/patch + ungrouped majors, limit 5.** Up to 5 + 4 major PRs at once for the 21 majors. That is more CI time and more review, but every major is visible right away.
- **C — `versioning-strategy: lockfile-only` first.** Only in-range lockfile bumps (10 npm / 17 cargo candidates) and no manifest edits. This is the lowest-risk start, but it never surfaces the out-of-range Tauri or TypeScript pins.

## Recommendation
Go with **Option A** for the first wave, after a separate one-commit cleanup: sync the `app/package-lock.json` root `version` to `app/package.json` (0.5.1) and regenerate `Cargo.lock` so it includes `rusqlite` (`app/src-tauri/Cargo.toml:82`). Both steps generate lockfiles, so under Rule #17 they need a decision on who runs them; the earlier manual-sync precedent is `b431f86c`. Then add `npm` at `/app` and `cargo` at `/app/src-tauri` to `.github/dependabot.yml`, each with:
- `interval: monthly`;
- one group for `update-types: [minor, patch]`;
- `ignore` of `version-update:semver-major` everywhere;
- `ignore` (or a dedicated paired group) for `tauri`, `@tauri-apps/api` and `@tauri-apps/cli`;
- `open-pull-requests-limit: 3`.

Also add `package-lock.json` to the `docs/RELEASING.md` § 1 bump list (`docs/RELEASING.md:28-34`) so the root version stops drifting.

The trade-off is that majors such as `vite` 6→8, `ureq` 2→3 and `russh` 0.49→0.65 stay invisible until the ignores are lifted one at a time. If the first grouped PRs prove routinely green on CI, switch to Option B.

## Open questions
- Whether GitHub Dependabot security alerts or security updates are already enabled on `yyhezkel/ymux`, and whether dependabot has opened PRs before. `gh` is not installed on this host (`timeout 20 gh api … → No such file or directory`). Answer with `gh api repos/yyhezkel/ymux/vulnerability-alerts` and `gh pr list --author app/dependabot --state all`.
- Whether the first npm dependabot PR rewrites the root `version` field in `app/package-lock.json`, and whether the first cargo PR adds the missing `rusqlite` lock entries. Both are inferred. Answer with a dry run on a fork or by watching the first PR.
- Whether `sdk-gen/`, `sdk/typescript/` (npm) and `app/src-tauri/server/go.mod` (gomod) should also be covered. This is out of the goal's stated scope; the human would decide.
- Exact PR count under the default `auto` strategy for the `~`-pinned `@tauri-apps/api` and `typescript`. Answer with the first dependabot run.

## Sources
- `.github/dependabot.yml:4-23` — current single github-actions entry, monthly, grouped; npm/cargo deliberately off
- `BACKLOG.md:84` — the ledger line for this ticket
- `app/package.json:3` — version 0.5.1; direct deps list
- `app/package-lock.json:3,9` — root version 0.4.5 (×2)
- `app/src-tauri/Cargo.toml:1-2` — 11-member workspace
- `app/src-tauri/Cargo.toml:52` — `tauri = "=2.10.3"` pin
- `app/src-tauri/Cargo.toml:82` — rusqlite declared
- `CLAUDE.md:40` — tauri bump verification requirement
- `docs/RELEASING.md:26-36` — bump list without package-lock.json
- `.github/workflows/ci-windows.yml:28-40,162,164-165,279` — PR trigger + paths-ignore, `npm ci`, typescript pin rationale, `cargo test --workspace` without `--locked`
- `.github/workflows/build-macos-intel.yml:63,167,206` — PR trigger, `npm ci`, `cargo test --workspace`
- `.github/workflows/warm-rust-cache.yml:22-28,79` — triggers on Cargo.lock/Cargo.toml push
- `git show --stat 1be9dc5a` → v0.5.1 bump touched Cargo.lock but not package-lock.json
- `git show b431f86c` → earlier manual "sync package-lock.json to the v0.4.5 bump"; references 339dd096 (same for Cargo.lock)
- `git show --stat f1cda228` → rusqlite added to Cargo.toml only (2026-10-06)
- `grep -n rusqlite app/src-tauri/Cargo.lock` → no match
- `grep -c '^\[\[package\]\]' app/src-tauri/Cargo.lock` → 737; `grep -c 'source = "registry'` → 726; git-sourced → 0
- `git ls-files | grep -E '(package(-lock)?\.json|Cargo\.(toml|lock)|go\.mod)$'` → 19 manifests incl. sdk-gen, sdk/typescript, server/go.mod
- https://registry.npmjs.org/-/package/<name>/dist-tags — retrieved 2026-10-27 — latest for the 20 direct npm deps (table in Q2)
- https://crates.io/api/v1/crates/<name> — retrieved 2026-10-27 — `max_stable_version` for the 39 direct cargo deps (list in Q2)
- https://docs.github.com/en/code-security/dependabot/working-with-dependabot/dependabot-options-reference — retrieved 2026-10-27 — open-pull-requests-limit default 5, groups/update-types, ignore semver-major, cooldown default 3 days, versioning-strategy values and `auto` behavior
