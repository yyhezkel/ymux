<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: ymux-2026-10-05-manual, ymux-2026-10-05-manual-2 -->

**Both BACKLOG entries are stale since Phase 117 (`1b7f6f1a`). Only `context.inject` is still missing, and the drift needs shared golden fixtures.** I only read the code; nothing was run.

## Q1: goal/done
- **Facts:** Go parses and carries both keys (`brief.go:75-76,191-194,217-218`). Both blobs contain `goal,omitempty`, and `git diff 1b7f6f1a..872dbf84 -- internal/agent` is empty.
- **Browser impact:** none. The keys reach the rail through `context.go:200-207` and `ContextRail.tsx:304`. No frontend code reads `brief.goal`.
- **Left over:** a stale comment at `brief.rs:22-24`. Go has no alias or clip test; `context_test.go:33` covers plain keys only.
- **Options:** close outright, test plus comment, or rebake.
- **Recommendation:** test plus comment, with no rebake or bump (`_test.go` is exempt, `ci-windows.yml:400`). About 30 lines plus vault re-stamp.
- **Invalidated if:** something starts reading `brief.goal === null`.

## Q2: Context Rail
- **Ported:** store, log, persistence, `context:changed`, list route (`context.go`, `web.ts:895`).
- **Missing:** injection only. `hookdispatch.go:78-79` returns unknown-method and the CLI fails open (`main.rs:2464-2466`). `session_context_get` has no caller, so it is worth nothing.
- **Value:**
  - `compact`/`resume`: the agent recovers goal, done-when, now/next, 5 deltas and the original prompt (≤1536 bytes). Resume is a real browser path (`claudesessions.go`).
  - `startup`: workspace intent plus sibling sessions.
  - Also removes one daemon Warn per session start.
- **Limit:** stores are separate, so a desktop-created session resumed in the browser gets nothing.
- **Options:** defer, port always-on, or port with the off switch.
- **Recommendation:** port with the off switch, reading only `brief.inject_context` from the opaque settings (`settings.go:7-9`).
- **Touches:** `term/context.go`, `hookdispatch.go`, tests, both blobs, `core.go:91`, `ymux-addons/src/lib.rs:146`, `server-go.md`, `CONTEXT.md:158`. About 130 Go lines plus tests.
- **Version bump is mandatory:** `addons.rs:767-769` compares version strings.
- **Verify first:** whether `HookRegistry` can reach the intent store (`webws.go:52`).
- **Invalidated if:** browser Claude use is rare; then defer.

**Side finding:** `872dbf84` rebaked at 2.19.0 without a bump, so remotes already on 2.19.0 are not offered that usage fix.

## Q3: measured drift
These match: state table, notification lists, parser rules, humanize copy, key table.

| Behavior | Rust | Go | Impact |
|---|---|---|---|
| Permission policy | Auto/Block/Gate, custom patterns (`rpc_server.rs:1687-1757`) | `none`/`gate`; `none` allows everything (`hookdispatch.go:173-176`) | **High:** Block rules absent in browser |
| `context.inject` | yes | no | Q2 |
| Gate-card fallback text | CLI text | humanized (`feed.go:179-198`) | Low; Go ahead |
| Alerts | per-event toasts | `notify` push (`hookdispatch.go:164`) | Platform |
| Card language | settings | both, per client | By design |
| Empty goal/done JSON | `null` | omitted (`brief.go:75-76`) | None today |
| Empty `claude_title` | `task:""` | nil (`hookdispatch.go:282-287`) | Cosmetic |
| Session-end, no prior state | seq 0, no brief event | seq+1, brief event | None |
| Empty prompt | no context touch | touches (`hookdispatch.go:146-155`) | Negligible |
| Context files | prunes `.tmp`, strips BOM, pretty | none, compact | Negligible |
| `120.0` duration | rejected | accepted (`humanize.go:110`) | Negligible |

**Options**
- **Port-per-feature:** the status quo. It has no cross-check, and its "change both" comments already went stale (`brief.rs:22-24`).
- **Desktop consumes the daemon's copy:** impossible. Only Linux binaries are embedded (`addons.rs:34-35`), and local panes have no daemon.
- **Golden fixtures:** Go has a JSON-shape test and humanize goldens; Rust has neither, which is why `omitempty` versus `null` stays invisible.

**Recommendation:** fixtures first, then inject.
- Put them in `server/internal/agent/testdata/*.json`, read by `go test` and by Rust via `include_str!`.
- Cover parser, JSON shape, state transitions, humanize, keys, then `build_injection`.
- JSON and `_test.go` need no rebake. About 250 lines.
- Fix `omitempty` in the inject rebake.

**Limits**
- `state_since` is not comparable (`lib.rs:406` stamps now; `state.go:130` takes it as a parameter).
- `feed.push` arms stay uncovered, because they sit inline with `AppHandle`. The policy gap therefore needs its own decision: is `none` meant to skip Block?

**Invalidated if:** the desktop stops handling hooks, or the daemon moves off Go.

## Prior reports
Conclusions match on Q1 and Q2: stale, test plus comment, narrow to `context.inject`, `omitempty` harmless. Differences:
- I fold the Go test into fixtures rather than adding a one-off.
- I make the off-switch call (honor it) that `manual-2` left open.
- They did not cover Q3; the fixture plan and the policy gap are new.
