# ymux-2026-10-05-manual — research report

## Question

The BACKLOG entry (filed 2026-10-05) says the Rust `[ymux-brief]` parser gained sticky `goal` / `done` keys in the Phase 105 follow-up, and the Go daemon port did not. It asks for the two keys and the `goal_and_done_keys_parse_and_clip` test to be ported to Go, then for both daemon blobs to be rebaked and the daemon version bumped. This report checks each part of that request against the live tree at `6070488f`.

**Headline:** the entry is mostly **stale**. Phase 117 (`1b7f6f1a`, 2026-10-07, "daemon 2.17.0") ported both keys into the Go parser and the Go `Brief` struct two days after the entry was filed. The committed blobs already contain that code. Two things are still missing. First, the Go *test* was never ported. Second, a doc comment in `brief.rs` still says the Go port lacks the keys. A test-only change does not trip the rebake gate, so neither a rebake nor a version bump is needed.

**Path correction:** the goal cites `server/internal/agent/brief.go`. The live path is `app/src-tauri/server/internal/agent/brief.go` (`git ls-files`). The repo has no top-level `server/`.

### Q1 — Does `server/internal/agent/brief.go` parse the sticky `goal` and `done` keys that `brief.rs` parses
### Q2 — Which fields and tests in `brief_test.go` and `brief.rs` cover `goal_and_done_keys_parse_and_clip` and what each asserts
### Q3 — Does the Go `PaneBrief` lack the `goal` / `done` fields, so a browser-created session's brief drops them
### Q4 — Which committed blobs `app/src-tauri/resources/ymux-server-linux-{x64,arm64}` and which version constant must change for the daemon bump
### Q5 — Does the ci-windows "server changed without rebake" gate require the blob commit in the same change as the Go source

## Findings

### Q1

**Yes, it already does.** The Go `ParseBrief` switch has `case "goal": out.Goal = …` and `case "done", "done-when", "done_when", "done when": out.Done = …` (`app/src-tauri/server/internal/agent/brief.go:191-194`). This matches the Rust arms `"goal" => out.goal` and `"done" | "done-when" | "done_when" | "done when" => out.done` (`app/src-tauri/src/brief.rs:229-230`) alias for alias. `ParsedBrief` carries `Goal, Done *string` (`brief.go:97`). `BriefFromStop` copies both on the non-degraded path (`brief.go:217-218`), as Rust does (`brief.rs:254-255`). On the degraded path both stay nil: the `Brief{…}` literal at `brief.go:230` omits them, and Rust sets them to `None` (`brief.rs:268-269`). Value handling is the same as for every other key in both languages: trim, skip empty, then clip to 200 chars (`brief.go:169-173`, `brief.rs:215-219`).

- When: `git log -S'Phase 117 (F3)' -- …/brief.go` → `1b7f6f1a Phase 117: WEB-DESIGN F3 … (daemon 2.17.0)`, dated 2026-10-07. The Rust keys landed in `c2e18700` on 2026-10-05, the same day the BACKLOG line was written.
- **A stale doc comment remains:** `app/src-tauri/src/brief.rs:22-24` still reads "The Go port (`server/internal/agent/brief.go`) does NOT know these two keys yet — BACKLOG parity entry." That is false since `1b7f6f1a`, and the path is also wrong.

Confidence: **high**. The evidence is the code itself plus the commit that introduced it.

### Q2

**The Rust test exists. The Go test does not.**

`goal_and_done_keys_parse_and_clip` (`app/src-tauri/src/brief.rs:293-310`) asserts:
1. A decorated block (`**goal**: Ship the installer`, `- done when: two clean runs`) parses to `goal = "Ship the installer"` and `done = "two clean runs"`. This exercises markdown stripping on the key and the `done when` alias (`brief.rs:300-301`).
2. A 300-char `next:` is clipped to `FIELD_MAX_CHARS + 1` chars, i.e. 200 plus the ellipsis (`brief.rs:302`).
3. A block holding only `done: green CI` parses `done`, and `goal` stays `None`: "absent stays None (sticky lives in the store)" (`brief.rs:303-305`).
4. `brief_from_stop` on the decorated message carries `goal` through to `PaneBrief` (`brief.rs:306-307`).
5. `brief_from_stop` on a message with no marker (degraded) gives `goal` and `done` both `None` (`brief.rs:308-309`).

`brief_test.go` has 13 tests (`grep -n "func Test" …/brief_test.go`): `TestParsesAFullBlock` (:29), `TestHebrewValuesSurvive` (:44), `TestLastMarkerWinsOverAQuotedOne` (:57), `TestMarkdownDecorationIsStripped` (:71), `TestUnknownKeysAndBadStatusAreTolerated` (:83), `TestStatusAliases` (:94), `TestNoMarkerYieldsNoneAndFullPreText` (:106), `TestDegradedBriefFromPlainMessage` (:115), `TestDegradedBriefWithNoMessageAtAll` (:127), `TestParsedStatusDefaultsToDoneWhenMissing` (:134), `TestLongFieldsAreClipped` (:142), `TestClipCharsIsRuneSafe` (:153) and `TestBriefEntryJSONShape` (:164). None of them references `Goal` or `Done` (`grep -n "goal\|Goal" brief_test.go` → no hits for the keys). The Go goal/done code path is therefore **untested**. The real remaining port is a `TestGoalAndDoneKeysParseAndClip` that mirrors assertions 1–5. The file's existing `eqPtr` helper (used at `brief_test.go:41`) fits it.

Note: `TestBriefEntryJSONShape` (`brief_test.go:170`) pins a degraded brief's JSON *without* `goal` / `done` keys. That is consistent with the `omitempty` tags (see Q3).

Confidence: **high**.

### Q3

**No. The Go struct has the fields, so a browser session's brief does not drop them.** In the Go port, `PaneBrief` is named `Brief` (`brief.go:63-66`). It declares `Goal *string \`json:"goal,omitempty"\`` and `Done *string \`json:"done,omitempty"\`` (`brief.go:75-76`). The sticky value also reaches the daemon's per-session context store: `term/context.go:200-205` keeps the last non-empty `b.Goal` / `b.Done`, the Go counterpart of `context_store.rs`.

There is one wire-shape difference, and its impact is minor. Rust `PaneBrief` serialises `goal` / `done` as explicit `null` when absent: it uses plain `Serialize` with no skip attribute (`brief.rs:85-108`), and the ts-rs binding types them as `goal: string | null` (`app/src/bindings/PaneBrief.ts`). Go *omits* the keys when nil because of `omitempty`. A browser client therefore sees `undefined` rather than `null`. No frontend code was found that reads `brief.goal` / `brief.done` directly. The only `goal` reader is `contextModel.ts:75` (`s.goal?.trim()`), which reads the context-store snapshot, not the brief, and is nullish-safe anyway (grep of `goal` in `app/src` excluding bindings and tests). So the difference causes no observed bug. It is a parity nit to note, not to fix in this ticket.

Confidence: **high** for the fields. **Medium** for "no consumer breaks on undefined": this rests on a grep, and a browser-only bundle path that reads `brief.goal` would raise it if one existed.

### Q4

**None need to change for this ticket.** If a Go *source* change were made, these are the bump surfaces:

- Blobs: `app/src-tauri/resources/ymux-server-linux-x64` and `…-arm64` (`git ls-files app/src-tauri/resources | grep server`). They are embedded via `include_bytes!` at `app/src-tauri/src/addons.rs:34-35`.
- Version constant: `const Version = "2.19.0"` at `app/src-tauri/server/internal/core/core.go:91`. Its comment (`core.go:88-90`) requires `ymux-addons`' `INSIGHTS_VERSION` to stay equal: `pub const INSIGHTS_VERSION: &str = "2.19.0"` at `app/src-tauri/crates/ymux-addons/src/lib.rs:146`. A bump changes both. Phase 117 did exactly that (`git show 1b7f6f1a -- …/ymux-addons/src/lib.rs` → `2.16.0` → `2.17.0`).

The current blobs already contain the goal/done parser. `git merge-base --is-ancestor 1b7f6f1a 74e935bc` → ancestor, where `74e935bc chore: rebake server blobs after merging main (2.19.0)` is the latest blob commit. `grep -c -a done_when` on each blob → `x64: 1`, `arm64: 1`. That string literal comes only from the alias switch in `brief.go:193`. The rebake and the version bump in the BACKLOG entry were satisfied by Phase 117 and the 2.18.0/2.19.0 rebakes that followed it.

Confidence: **high** for the surfaces. **Medium-high** that the blobs contain the parser: the `done_when` string grep plus commit ancestry is strong evidence, but not a byte-for-byte rebuild.

### Q5

**Only for shipping Go source, and test files are exempt.** The gate is `.github/workflows/ci-windows.yml:380-411`, "Assert the embedded server binaries were rebaked". It diffs `base..HEAD` (`:399`), where base is the PR base sha or the push's `before` sha (`:385-389`). It fails when any `app/src-tauri/server/**.{go,mod,sum}` changed **excluding `_test.go`** (`:400`) and neither blob changed (`:401-406`). The comment at `:378` says why: "Test files are excluded: they do not reach the binary."

Two consequences:
- The gate works on the whole push or PR diff, not per commit. Source and blobs must land in the same push/PR. They do not have to share one commit, but CLAUDE.md § Rebaking the server asks for "the same change".
- The remaining work (adding a Go test only) does **not** trip the gate. No rebake, no version bump.

A separate gate does apply. Go `*_test.go` files are vault-covered (`vault/moc.md` Test facts: `docs/vault/server-go.md` globs `internal/agent/*.go`, tests included). Editing `brief_test.go` therefore needs `docs/vault/server-go.md` touched in the same diff, plus `node scripts/vault-check.mjs --write`. Fixing the stale `brief.rs:22-24` comment changes a covered file (`docs/vault/.vault-lock.json:47`, owned by `docs/vault/backend-rpc.md:5`), so that page must be touched too.

Confidence: **high**.

## Recommendation

**Narrow the ticket to a test-and-comment change, with no rebake and no version bump.** Concretely:
1. Add `TestGoalAndDoneKeysParseAndClip` to `app/src-tauri/server/internal/agent/brief_test.go`, mirroring the five assertions of `brief.rs:293-310` (decorated keys plus the `done when` alias, the 200+1 clip on `next`, the done-only block leaving `Goal` nil, `BriefFromStop` carrying `Goal`, and the degraded brief leaving both nil).
2. Fix the stale doc comment at `app/src-tauri/src/brief.rs:22-24`. It should say the Go port parses both keys (`app/src-tauri/server/internal/agent/brief.go`) and drop the "BACKLOG parity entry" clause.
3. Touch `docs/vault/server-go.md` and `docs/vault/backend-rpc.md` in the same diff and re-stamp with `node scripts/vault-check.mjs --write`.

Trade-off accepted: the `omitempty`-vs-`null` JSON difference (Q3) stays as it is. No consumer is affected, and changing it would alter shipping Go source, which *would* force a rebake and a version bump. I would change my mind if a browser-side reader turns up that tests `brief.goal === null`.

## Open questions

- Whether the browser bundle has any reader of `brief.goal` / `brief.done` that distinguishes `null` from `undefined`. A grep of `app/src` found none (Q3). A live browser session against the daemon, inspecting a `pane:brief` event, would settle it.
- A byte-level confirmation that the committed blobs were built from a tree containing `1b7f6f1a`. Commit ancestry plus the `done_when` string grep is strong but indirect. Running the committed blob on a side port (`vault/howto/run-committed-server-blob.md`) and feeding it a brief with `goal:` would prove it.

## Sources

- `app/src-tauri/server/internal/agent/brief.go:73-76` — `Goal` / `Done` fields with `omitempty` on the Go `Brief`
- `app/src-tauri/server/internal/agent/brief.go:97` — `ParsedBrief` has `Goal, Done`
- `app/src-tauri/server/internal/agent/brief.go:191-194` — goal/done parse arms with aliases
- `app/src-tauri/server/internal/agent/brief.go:217-218,230` — `BriefFromStop` carries them; degraded path omits
- `app/src-tauri/server/internal/agent/brief_test.go:29-170` — 13 tests, none on goal/done; `:170` JSON shape pin
- `app/src-tauri/src/brief.rs:22-24` — stale "Go port does NOT know these two keys" comment
- `app/src-tauri/src/brief.rs:97-101,140-141,229-230,254-255,268-269` — Rust fields, parse arms, stop builder
- `app/src-tauri/src/brief.rs:293-310` — `goal_and_done_keys_parse_and_clip`
- `app/src/bindings/PaneBrief.ts` — `goal: string | null`, `done: string | null`
- `app/src/contextModel.ts:75` — only `goal` reader, on the context snapshot, nullish-safe
- `app/src-tauri/server/internal/term/context.go:62,200-205` — daemon context store keeps the sticky goal/done
- `app/src-tauri/server/internal/core/core.go:88-91` — `Version = "2.19.0"` and the equality rule
- `app/src-tauri/crates/ymux-addons/src/lib.rs:140-146` — `INSIGHTS_VERSION = "2.19.0"`
- `app/src-tauri/src/addons.rs:34-35` — blobs embedded via `include_bytes!`
- `.github/workflows/ci-windows.yml:365-411` — rebake staleness gate; `_test.go` excluded at `:400`
- `docs/vault/.vault-lock.json:47`, `docs/vault/backend-rpc.md:5` — `brief.rs` is vault-covered
- `vault/moc.md` Test facts — Go tests are covered by `docs/vault/server-go.md`
- `docs/BRIEF.md:28-32` — sticky goal/done wire spec
- `git log -S'Phase 117 (F3)' -- app/src-tauri/server/internal/agent/brief.go` → `1b7f6f1a` (2026-10-07, daemon 2.17.0)
- `git log -S'goal_and_done_keys_parse_and_clip' -- app/src-tauri/src/brief.rs` → `c2e18700` (2026-10-05)
- `git show 1b7f6f1a -- app/src-tauri/crates/ymux-addons/src/lib.rs` → `INSIGHTS_VERSION` 2.16.0 → 2.17.0
- `git log --oneline -3 -- app/src-tauri/resources/ymux-server-linux-x64` → `74e935bc` rebake (2.19.0) latest
- `git merge-base --is-ancestor 1b7f6f1a 74e935bc` → ancestor
- `grep -c -a done_when app/src-tauri/resources/ymux-server-linux-{x64,arm64}` → 1 / 1
- No web sources used: the repo answered every question.
