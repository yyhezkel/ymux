<!-- Research 2026-11-09, unanchored on earlier decisions. Static reading unless the report says measured: not verified live. Reconsiders: ymux-2026-09-01-plan, ymux-2026-08-03-manual -->

**Q1: Don't ship the skill. Inject the brief instructions through the existing SessionStart hook, and keep the CLAUDE.md snippet as the fallback.**

Facts:
- The brief is required on every final answer (`docs/BRIEF.md:11`). A skill body loads only when invoked. Installation is also manual, per workspace, and needs a live SSH session (`skills.rs:145-154`).
- The carrier already exists: `context.inject` (`rpc_server.rs:1288`) → `build_injection` (`context_store.rs:576`) → `additionalContext` (`cli/src/main.rs:2483`). The text is desktop-side, so changing it needs no CLI change.
- Remotes get the hook automatically: bootstrap runs `setup-hooks --source bundled` (`ymux-bootstrap/src/lib.rs:501`).
- Gate: the committed `resources/ymux-linux-x64` (0.5.0) has 0 `context.inject` hits. Release builds re-stage it (`build-windows.yml:79`), so a tagged build should carry it, but this is unverified on a real remote.
- Gaps: `clear` and an empty `startup` return `""` (`context_store.rs:572-574`). The 1536-byte cap (`:488`) would crowd out deltas, since the snippet is 672 bytes.
- Briefs are effectively Claude-only: codex/gemini shims send no message (`brief.rs:239`).

| Option | Reach | Cost |
|---|---|---|
| Skill | Fires unreliably | Bundling, seeding, manual install |
| CLAUDE.md snippet | Reliable, but pasted per project and machine | Already built; lowest adoption |
| `setup-hooks` writes `~/.claude/CLAUDE.md` | Survives everything, no cap | Edits a user-owned file, leaks outside ymux, needs a CLI change |
| SessionStart injection | Automatic on every connected remote | Fails open at 300 ms (`main.rs:2455`) |

`setup-hooks` is the delivery mechanism for the injection, not a rival to it.

Recommendation: in `injection_for_hook`, append a fixed protocol block outside the cap, on all four sources including `clear`. Add a separate `brief.inject_protocol` toggle, because this changes visible agent output.

Files: `context_store.rs`, `settings.rs`, `SettingsModal.tsx`, `settings.ts`, generated bindings, 4 i18n files, `docs/BRIEF.md:63-64`, vault `backend-rpc.md`. Move `BRIEF_SNIPPET` text to one shared source if practical. Size: about 60-80 lines plus tests.

Invalidated if:
- The `degraded=` rate (`rpc_server.rs:1558`, debug level) stays high. Then escalate to a per-turn `UserPromptSubmit` reminder.
- Real tunnels regularly exceed 300 ms. Then raise the budget or use the managed CLAUDE.md block.
- `last.env` (`main.rs:2301`) leaks the block into non-ymux sessions on the same box. I did not test this.

**Q2: Build two things: token search and live keybinding hints. Drop the rest.**

Facts:
- `5413ef9` and `design-pass-01` are gone (`git cat-file` fails; no matching ref). There is nothing to cherry-pick, and the BACKLOG's "keep branch alive" is false.
- 29 commands, one array (`App.tsx:1192-1224`).
- Search is a substring match on the localized label only (`CommandPalette.tsx:38`).

Hit rate, from a script over mechanically derived queries (no telemetry exists):

| Query | Today | Token-AND on localized + EN label + id |
|---|---|---|
| Single last word, EN UI | 29/29 | 29/29 |
| Reversed order ("close pane" style) | 0/29 | 29/29 |
| Label read aloud (colon blocks it) | 3/29 | 29/29 |
| Single last word, English query, Hebrew UI | 2/29 | 29/29 |

Verdict on each piece:
- **Fuzzy + highlight: no.** Token-AND closes every measured gap; typos are the only remainder.
- **Category grouping: no.** Labels already carry `Pane:` / `SSH:` prefixes.
- **Recent: no.** No evidence of need at 29 commands.
- **Footer: no.** Decoration.
- **Keybinding hints: yes.** 12 of 29 commands map to a shortcut, read from `shortcutTable()` (`App.tsx:885`). This also fixes a bug: two labels hardcode "(Ctrl+Enter)" and "(Ctrl+Alt+=)", which go stale because shortcuts are rebindable.

Files: `CommandPalette.tsx` (pure exported matcher with a test), `App.tsx` (id→shortcut map), `App.css`, 4 i18n files (strip the two accelerators), `i18n/index.ts` (expose EN lookup), vault `frontend-shell.md:591`. Size: about 40-60 lines.

Invalidated if: the palette passes about 100 commands, or real usage shows typo-driven misses. Then fuzzy earns its place.

**Compared with prior research**

- Q1 differs. The 09-01 plan scoped how to ship the skill and never tested whether a skill fits. Its snippet choice rested on seeding being new plumbing, and SessionStart injection (Phase 105.C) came later.
- Q2 agrees on the facts: 29 commands, commit gone, rewrite fresh. It differs on scope: the BACKLOG's cheap part was categories + hints; I drop categories and add token search, which the 08-03 report never measured.

Nothing in the repo changed; the only file created is the hit-rate script, `/tmp/claude-1000/-home-runner-winmuxMac-winmux/60b24d38-ff63-4067-ae09-04ba42090f73/scratchpad/hit.mjs`.
