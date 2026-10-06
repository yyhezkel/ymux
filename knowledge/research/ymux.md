# ymux — research report

## Question

Yossi wants environment variables in ymux so that two developers working against the same host can each act as themselves: push to git under their own identity, run Claude Code on their own API key, and so on. A "Secrets Vault" was already designed (`docs/COMPETITIVE-SCAN.md` § "★ Secrets Vault — Design מלא"). The question is how to build this on top of that design and today's code. This report answers the five outcome questions in `goal.md`. Every claim was checked against the live tree at `f2f1b5b`.

Two things the goal did not anticipate shape every answer, so they come first:

1. **Per-workspace env vars already ship.** `Workspace.env: Vec<EnvVar>` exists (`app/src-tauri/crates/ymux-types/src/lib.rs:290-293`, `:333`). The create/edit UI already has an editor for it (`app/src/WorkspaceExtrasFields.tsx:42-75`). An agent can also set it via RPC `update-workspace` (`app/src-tauri/src/rpc_server.rs:740-768`). The mechanism *types* `export K='V'` into the pane's shell (`lib.rs:2300-2340`, `format_env_line` at `lib.rs:2055-2075`). The feature is therefore not "add env vars". It is "make the existing env vars safe for secrets, and per-developer".
2. **The Vault itself is an Open decision.** `docs/DECISIONS.md:416-420` ("Secrets Vault: revive or archive") is still Open and waiting on Yossi. The 2026-05-28 entry (`docs/DECISIONS.md:1759-1763`) paused Vault work in favour of an *external* MCP that holds the secrets. Under that split, ymux only provides egress hooks: "SSH env inject, child-process spawn with env". A deeper critique of the design already exists in `docs/SECRETS-VAULT-RESEARCH.md` (on main, 550 lines), and this report cites it rather than re-deriving it.

### Q1 — How can the Secrets Vault design from docs/COMPETITIVE-SCAN.md § "Secrets Vault — Design מלא" integrate with ymux's SSH channel setup (spawn_ssh in backend-remote.md) to inject environment variables without exposing secrets to the LLM transcript → answered with cited code paths and schema mapping
### Q2 — What is the minimal MVP scope for environment variable injection — SSH env vars only (backend-remote.md:866), or should HTTP header/browser form fill/stdin injection (COMPETITIVE-SCAN.md:886-898) be included → answered with effort estimates and use-case prioritization
### Q3 — How should per-developer environment variables be scoped and persisted: per-workspace in workspaces.json, per-connection in settings.json, or in a separate secrets.json with DPAPI encryption (COMPETITIVE-SCAN.md:751-752) → answered with each option's tradeoffs
### Q4 — What approval flow should gate agent access to environment variables — feed.push card (like mobile pairing in DECISIONS.md:87), pre-approved per-workspace, or always-ask → answered with security and UX analysis
### Q5 — Are there existing code paths for environment variable injection in local spawns (spawn_local_pty for macOS tmux / Windows zellij) that can inform the remote implementation → answered with cross-platform consistency analysis

Citation corrections to the goal: `spawn_ssh` is documented in `docs/vault/backend-core.md:187-192`, not `backend-remote.md`. `backend-remote.md` has 123 lines, so "backend-remote.md:866" cannot exist. That line is `docs/COMPETITIVE-SCAN.md:866` ("1. SSH env injection").

## Findings

### Q1

**Answer.** Of the three env channels `spawn_ssh` uses today, only one can carry a secret without leaking it: `channel.set_env`. It is also the one most likely to be dropped silently, because OpenSSH's `AcceptEnv` accepts no variables by default. The design's claim "יש לך כבר 90%" (`COMPETITIVE-SCAN.md:866`) is optimistic. The hook point exists, but on a stock sshd the variable never arrives, and both of today's fallbacks leak. The schema maps cleanly: `EgressPolicy::SshInject { host_pattern, env_name }` (`COMPETITIVE-SCAN.md:793`) holds exactly the data the injection loop needs. Delivery is the hard part.

**The three channels `spawn_ssh` uses today, and what each does with a secret:**

| channel | code | reaches the shell? | leak surface |
|---|---|---|---|
| `channel.set_env` before `request_pty` | `lib.rs:4451-4462` (`YMUX_*`), `lib.rs:4464-4476` (hyperlink vars) | only if sshd's `AcceptEnv` lists the name. OpenSSH default: "not to accept any environment variables" | none on the wire beyond SSH. Value lives only in that shell's environ |
| `~/.ymux/run/last.env` file | `lib.rs:5158-5179` → `tunnel::write_remote_env_file` | yes, read by the CLI | **secret at rest on the remote disk** (`SECRETS-VAULT-RESEARCH.md:351` already flags this) |
| `tmux set-environment -g` | `build_tmux_attach_script`, `lib.rs:3404-3440` | yes, for new tmux windows | **server-global, last-connector-wins** (`DECISIONS.md:876`, `:930`). With two developers on one Unix user and one tmux server, B's token overwrites A's. That is exactly the case Yossi described |
| typed `export K='V'` (workspace `env`) | `schedule_setup_injection`, `lib.rs:2300-2340`, called at `lib.rs:9211-9218` | yes | **the value is typed into the PTY**: it is echoed to the terminal, sits in scrollback, is readable by agents through `read_pane` / `pane.scrollback` (`docs/vault/backend-rpc.md` § MCP bridge), and lands in shell history |

There is a further trap with tmux. The typed exports fire at 500 ms into the *outer* login shell, and `tmux new-session -A` is typed at 900 ms (`lib.rs:4806-4809`). tmux copies the outer environment into its global environment only when the *server starts*. After that, only the `update-environment` list (default `DISPLAY`-family) is copied on new-session/attach (tmux(1), NetBSD man page). As a result, a workspace env var reaches the persistent pane on the first pane that starts the tmux server. It silently does not reach panes created on a host whose tmux server is already running. This is inferred from the man page plus the timing in code and has not been tested live (see Open questions).

**How the secret reaches the LLM today, independent of injection.** RPC `list-workspaces` serializes the entire `WorkspacesFile` (`rpc_server.rs:667-670`). It is exposed both as MCP tool `list_workspaces` (`mcp/src/main.rs:121`) and as CLI `ymux list-workspaces` (`cli/src/main.rs:1783`). The CLI reaches the desktop from any remote pane through the reverse tunnel. Any value in `Workspace.env` is therefore one tool call away from the transcript. A secret store must keep values out of `Workspace`, or redact them in this serializer.

**Recommended integration (schema → code):**

- `Secret { id, kind, name, egress: SshInject { host_pattern, env_name }, principals, approval }` (`COMPETITIVE-SCAN.md:755-819`). The metadata lives outside `workspaces.json`, and the workspace references secrets by `SecretId` only (see Q3).
- In `spawn_ssh`, right after `channel_open_session` (`lib.rs:4446-4449`) and next to the existing `YMUX_*` `set_env` calls, add a `collect_for_ssh_channel(host, workspace, developer)` step and `set_env` each approved value. This is the design's own snippet (`COMPETITIVE-SCAN.md:869-882`).
- **Never** route a vault value through `last.env`, `tmux set-environment -g`, or `schedule_setup_injection`. `SECRETS-VAULT-RESEARCH.md:351` reaches the same conclusion for the env-file.
- When `set_env` is refused, the honest fallbacks are both "use, don't see" paths:
  - **(a) Stdin/fd delivery to a CLI helper.** Over a separate SSH *exec* channel, the desktop writes the value into a per-pane `0600` file under `$XDG_RUNTIME_DIR` (tmpfs), or pipes it to `ymux secret get` on demand. The tool consumes it through a credential helper (`git credential`, `gh auth`, Claude Code `apiKeyHelper`).
  - **(b) A per-session `tmux set-environment -t <session>`** (not `-g`). This removes last-connector-wins for tmux panes but still puts the raw value in the session env.
- For git specifically, non-secret identity (`GIT_AUTHOR_NAME/EMAIL`, `GIT_COMMITTER_*`) can use the plain path, since leaking a name and email is harmless. The *push credential* should never be a raw env var when avoidable. Options: SSH agent forwarding (no secret on the remote at all; not implemented today, since grep finds no agent-forward request in `app/src-tauri`), or `GIT_CONFIG_COUNT/KEY/VALUE` pointing `credential.helper` at the ymux CLI (git-config docs). Claude Code accepts `ANTHROPIC_API_KEY`, which overrides a logged-in subscription (code.claude.com env-vars). That is a raw env var, so it inherits the `echo $X` exposure that `COMPETITIVE-SCAN.md:885` already accepts as a trade-off.

**Confidence:** high for the code paths and leak surfaces. Medium for the tmux env-propagation gap (inferred from the man page, not run). It goes to high with one live test on a host with a running tmux server.

### Q2

**Answer.** MVP = **two egresses**: SSH env injection (`set_env` only, with an explicit "not delivered" signal when `AcceptEnv` drops it) plus a **local child-process shim** (`ymux exec --with-secret`), which also covers HTTP header injection. Browser form fill and stdin are deferred. This matches the existing research recommendation (`SECRETS-VAULT-RESEARCH.md:12-23` point 5; `:373-446`).

**Effort.** The original estimate is ~5 days without browser fill and ~10 with it (`COMPETITIVE-SCAN.md:990-1000`). The revised MVP is 6–7 days once peer authentication, output scrubbing and the audit hash-chain are added as non-negotiable (`SECRETS-VAULT-RESEARCH.md:455-464`). Today's code adds about 1–2 days that neither estimate includes:
- (i) redact or remove env values from `list-workspaces` (`rpc_server.rs:667-670`);
- (ii) a native in-process DPAPI *unprotect*, since none exists (`provisioning.rs:255-276` shells out to PowerShell, protect-only; FOLLOWUPS P2 "`save_workspace_secret` is write-only");
- (iii) a macOS at-rest store, since `dpapi_protect` returns `Err` off Windows (`provisioning.rs:293-295`) and Keychain was deliberately declined (`DECISIONS.md:851`).

This is an inferred estimate, not measured.

**Prioritizing by Yossi's use cases:**

| use case | needs | best egress |
|---|---|---|
| git commit identity per developer | non-secret env | plain env is enough (no vault needed) |
| git push credential per developer | secret | SSH agent forwarding or credential-helper via shim. Raw `GH_TOKEN` via `set_env` as the fallback |
| Claude API key per developer | secret | `set_env ANTHROPIC_API_KEY`, or the `apiKeyHelper` → shim pattern (`SECRETS-VAULT-RESEARCH.md:12-23` point 3 cites Claude Code `apiKeyHelper` as the broker pattern) |
| HTTP header to an API | secret | shim (`--with-secret` injects the header; `SECRETS-VAULT-RESEARCH.md:438`) |
| browser login | secret | defer. WebView2 has no isolated world, so the design's premise is wrong (`SECRETS-VAULT-RESEARCH.md:16`) |
| `ssh-add` passphrase | secret | defer (niche; `SECRETS-VAULT-RESEARCH.md:437`) |

**Confidence:** high on scope (two independent sources agree). Medium on the day counts.

### Q3

**Answer.** Use a separate store: metadata in `secrets.json`, values encrypted per value. Scope each value by **(developer, host-pattern)**. A workspace holds only `SecretId` references. Neither `workspaces.json` nor `settings.json` should hold a secret value.

| option | pros | cons (evidence) |
|---|---|---|
| **per-workspace in `workspaces.json`** (what `Workspace.env` does today) | already built: schema `ymux-types/src/lib.rs:333`, UI `WorkspaceExtrasFields.tsx`, RPC `rpc_server.rs:740-768`, injection `lib.rs:2300` | plaintext at rest, against the spirit of Absolute Rule #2. Returned whole by `list-workspaces` to every agent (`rpc_server.rs:667-670`, `mcp/src/main.rs:121`). Cloned onto every new screen (`docs/vault/backend-core.md:372`). Writable by any agent via `update-workspace` (`rpc_server.rs:768`). A workspace is per-machine, not per-developer |
| **per-connection in `settings.json`** | one place per host | same plaintext problem. `settings.load` / `settings.save` are RPC methods agents can call (`rpc_server.rs:2053-2078`). A connection is still not a developer |
| **`secrets.json` + per-value DPAPI** (`COMPETITIVE-SCAN.md:751-752`) | values encrypted. Metadata separated, so `list` can show "GH_TOKEN for github.com" without the value. Matches the broker model | DPAPI is per-OS-user, not per-process, so any same-user process can decrypt (`SECRETS-VAULT-RESEARCH.md:14`). Metadata is tamperable, so policy needs an HMAC (`:333-337`). No unprotect exists today. No macOS store (`provisioning.rs:293-295`). The existing `std::fs::write` in `save_workspace_secret` (`provisioning.rs:305-320`) is not atomic, contrary to Rule #7, so a new store must use tmp+rename |

**The "two developers" axis.** Nothing in today's model represents *who* is at the keyboard. If both developers use separate desktops (the likely case), each desktop's `secrets.json` is already per-developer through DPAPI's per-user binding. The real conflict is on the *shared remote*, through `tmux set-environment -g` (`DECISIONS.md:876`, `:930`) and the shared `last.env`. The fix there is delivery (Q1: `set_env` / per-session), not storage. If both developers share one desktop OS account, a "profile" selector is required. That is a product decision, listed in Open questions.

**Non-secret env** (git name/email, `EDITOR`) can stay in `Workspace.env`. The store split should be "secret vs not", not "env vs not".

**Confidence:** high on the trade-offs. Medium on which deployment shape (separate desktops vs shared) Yossi means.

### Q4

**Answer.** Use a **feed card, decided only by the human**, with policy `FirstUseWorkspace` as the default and `AlwaysAsk` for write-capable credentials. `PreApproved` should be allowed only for values the user marks low-risk. That is the design's own enum (`COMPETITIVE-SCAN.md:810-819`). Today's feed machinery has one hard blocker that must be fixed before any of this: **an agent can approve its own card.**

**Why the feed is the right rail.** `feed.push` with `blocking: true` parks the caller on a oneshot until `decide_feed` fires (`docs/vault/backend-rpc.md:114-118`). Mobile/browser pairing chose this same rail precisely because it "already toasts, already works with every panel closed, and is the same winner-takes-all machinery the agent hook gates use" (`DECISIONS.md:85-92`, decided as option B at `DECISIONS.md:69`). The research doc agrees: reuse `feed.push`, show the full intent, add number-matching for write operations, and rate-limit to prevent flooding (`SECRETS-VAULT-RESEARCH.md:356-359`).

**Security gaps in the current feed (code-observed):**
- `feed.decide` is an ordinary RPC method that takes only `request_id` + `decision` and has no caller check (`rpc_server.rs:2298-2311`). The local pipe accepts any same-user process, and remote panes reach it via the tunnel. An agent that can `feed.push` a secret request can also `feed.decide` it. The research doc names the same class of issue as T-S1/T-E1 (principal is claimed, not derived; `SECRETS-VAULT-RESEARCH.md:339-348`). **Fix before shipping:** secret-request cards must be decidable only from the Tauri UI command, not from the RPC `feed.decide` arm.
- Blocking is hard-wired to `kind == "permission_request"` (`rpc_server.rs:1665`). A `secret_request` kind needs its own arm.
- Pane identity comes from `YMUX_PANE_ID` in the caller's env. That value has already proven stale and untrustworthy (`DECISIONS.md:876-877`), so principal checks keyed on pane id are weak.

**UX trade-offs.** `AlwaysAsk` invites MFA fatigue (`SECRETS-VAULT-RESEARCH.md` §5.4). `PreApproved` per workspace means no prompt at all, which is fine for injection *at spawn time*, because then the human's click on "connect" is the approval. The cleanest fit for Yossi's case: env injected at spawn (`set_env`) is approved by the act of connecting and needs no card. The card is for *agent-initiated* requests (`secret.request` / shim) only.

**Confidence:** high on the code gaps (read directly). Medium on the UX recommendation (no user testing).

### Q5

**Answer.** Yes, and they show what to avoid as much as what to copy. Local panes have two env channels: the process environment set *before* spawn, and the typed exports *after* spawn. Only the first is safe for secrets, and it is exactly what the remote side lacks unless `AcceptEnv` allows it.

- **Spawn-time env (safe).** `spawn_local_pty` (`lib.rs:2767`) sets `FORCE_HYPERLINK*`, `COLORTERM`, `TERM`, `LANG`, `YMUX_PANE_ID`/`WINMUX_PANE_ID` with `cmd.env(...)` (`lib.rs:2824-2851`), plus `ZELLIJ_CONFIG_FILE/DIR` (`lib.rs:2871-2872`). The WSL path does the same (`lib.rs:3521-3526`). This is the local equivalent of `channel.set_env`, and it is where a local vault egress belongs. Values never touch the PTY.
- **Typed exports (unsafe for secrets).** `schedule_setup_injection` runs identically for Local and SSH sessions (`lib.rs:2330-2338`). It shapes the line per shell: PowerShell `$env:K = '…'`, cmd `set K=…`, POSIX `export K='…'` (`lib.rs:2055-2075`). It is cross-platform today, and it echoes the value into the terminal.
- **Persistence wrappers.** Windows types `zellij attach` at 900 ms, after the 500 ms exports (`lib.rs:2960-2965`). macOS types the tmux attach script at 900 ms with an empty socket address, so **no** `YMUX_*` exports reach local mac tmux, and the "local RPC bridge for hooks is not wired for mac panes yet" (`lib.rs:2997-3006`). Both persist through a multiplexer server that outlives the pane. The tmux server-env caveat from Q1 therefore applies to local macOS too. For zellij the equivalent behaviour was not verified.
- **Go daemon** (browser-created Claude sessions on the remote): `spawnEnv` builds `os.Environ()` plus the `YMUX_*` trio (`server/internal/chat/chat_session.go:288-305`). It is a third spawn site with no per-user env. If browser users are a "second developer", they inherit the daemon owner's environment.

**Consistency recommendation.** One egress abstraction, `inject(SecretRef) → (name, value)`, with three sinks: `cmd.env` (local, all OSes), `channel.set_env` (SSH), and `cmd.Env` (Go daemon). Each sink reports "delivered / refused", and none falls back to the typed-export path. Today's `schedule_setup_injection` stays for non-secret env only.

**Confidence:** high (all read directly from code).

## Options

1. **A. Harden the existing `Workspace.env` (smallest).** Add a `secret: bool` per `EnvVar`. Store secret values DPAPI-wrapped outside `workspaces.json`, redact them in `list-workspaces`, and deliver secrets via `cmd.env` / `set_env` only, never typed. Cost ≈ 2–3 days (inferred). Risk: no per-agent approval, no audit. Every process in the pane sees the value. Solves Yossi's stated need (per-developer git and Claude key) on separate desktops.
2. **B. Vault MVP per `SECRETS-VAULT-RESEARCH.md` §7.** Two egresses, cap protocol, feed cards, audit chain. Cost 6–7 days plus the 1–2 days of gaps in Q2. Risk: conflicts with the 2026-05-28 decision that secrets live in an external MCP (`DECISIONS.md:1759-1763`).
3. **C. Egress hooks only (per 2026-05-28).** ymux exposes `spawn-with-env` / `set_env` hooks plus a UI-only `feed.decide`, and the external MCP owns the store. Cost depends on the external MCP, whose status is unknown (`DECISIONS.md:416-420`).

## Recommendation

Do **Option A first, shaped so it becomes Option B's SSH and local egress later**. Yossi's request (each developer pushes and runs Claude as themselves) is a *spawn-time* need, and approval at spawn time is the human's own click on "connect". The agent-facing capability protocol is not needed to satisfy it. Concretely:
- mark `EnvVar` values as secret;
- keep them out of `workspaces.json` and out of `list-workspaces` (`rpc_server.rs:667-670`);
- deliver via `cmd.env` (`lib.rs:2824` area) and `channel.set_env` (`lib.rs:4451` area) only;
- surface "sshd refused `GH_TOKEN` (AcceptEnv)" to the user instead of falling back;
- prefer SSH agent forwarding over a raw push token for git.

In parallel, two independent fixes are worth making whatever is decided: make `feed.decide` UI-only for any security card (`rpc_server.rs:2298`), and redact `env` from `list-workspaces`. Both are leaks with or without a vault.

The trade-off being accepted: raw values in the remote process environment (`echo $X` exposure, already accepted at `COMPETITIVE-SCAN.md:885`), with no audit trail. The open Vault decision (`DECISIONS.md:416`) must be closed before Option B. Two things would change this recommendation: Yossi confirming the external MCP is alive (→ C), or the two developers sharing one desktop account (→ add profiles before anything else).

## Open questions

- **Deployment shape (Q3):** do the two developers use separate ymux desktops against a shared host, or one desktop or OS account? This decides whether a "profile" concept is needed. Answer: ask Yossi.
- **Vault revive/archive (`DECISIONS.md:416-420`):** still Open. Is the external secrets MCP alive? Answer: Yossi.
- **tmux env propagation (Q1/Q5):** inferred, not observed. Does a workspace `env` export reach a new tmux session when the tmux server is already running? Test: on a host with a running tmux server, set workspace env `FOO=1`, open a new persistent pane, run `echo $FOO`.
- **zellij env inheritance (Q5):** whether `zellij attach` to a running session sees exports typed in the outer shell was not verified. Test as above on Windows.
- **Tunnel method allowlist:** grep found no per-method filter on tunnel-borne RPC. If one exists elsewhere, the `feed.decide` and `list-workspaces` exposure from remote panes is narrower. Answer: read `crates/ymux-tunnel` bridge code.
- **russh agent forwarding:** not checked whether the pinned russh exposes `agent_forward` on a channel. Answer: check the russh version in `Cargo.lock` and its `Channel` API.
- **Effort figures** are estimates carried from the two design docs plus inference, not measured.

## Sources

- `app/src-tauri/crates/ymux-types/src/lib.rs:290-293`, `:333`, `:363` — `EnvVar`, `Workspace.env`, `claude_separate_account`
- `app/src/WorkspaceExtrasFields.tsx:42-75` — existing env editor UI
- `app/src-tauri/src/lib.rs:2055-2075` — `format_env_line` per shell
- `app/src-tauri/src/lib.rs:2300-2340` — `schedule_setup_injection` types exports into Local and SSH sessions
- `app/src-tauri/src/lib.rs:9211-9218`, `:8954` — call site, `ws.env` source
- `app/src-tauri/src/lib.rs:2767`, `:2824-2851`, `:2871-2872`, `:3521-3526` — local spawn-time `cmd.env`
- `app/src-tauri/src/lib.rs:2960-2965`, `:2997-3006` — zellij/tmux local attach timing; mac has no `YMUX_*` exports
- `app/src-tauri/src/lib.rs:3404-3440` — `tmux set-environment -g`
- `app/src-tauri/src/lib.rs:4236`, `:4446-4476` — `spawn_ssh`, `channel.set_env`
- `app/src-tauri/src/lib.rs:4806-4809` — tmux attach at 900 ms after 500 ms exports
- `app/src-tauri/src/lib.rs:5158-5179` — `write_remote_env_file` fallback
- `app/src-tauri/src/rpc_server.rs:667-670` — `list-workspaces` returns the whole file
- `app/src-tauri/src/rpc_server.rs:740-768` — `update-workspace` sets env
- `app/src-tauri/src/rpc_server.rs:1665` — blocking only for `permission_request`
- `app/src-tauri/src/rpc_server.rs:2298-2311` — `feed.decide` without caller check
- `app/src-tauri/mcp/src/main.rs:121`; `app/src-tauri/cli/src/main.rs:1783` — agent access to `list-workspaces`
- `app/src-tauri/src/provisioning.rs:255-276`, `:293-295`, `:305-320` — PowerShell DPAPI protect-only, no mac store, non-atomic write
- `app/src-tauri/server/internal/chat/chat_session.go:288-305` — Go `spawnEnv`
- `docs/COMPETITIVE-SCAN.md:743-1000` — Vault design (storage 751-752, schema 755-819, SSH 866-885, browser 886-894, HTTP 895-896, stdin 897-898, feed 899-931, estimates 990-1000)
- `docs/SECRETS-VAULT-RESEARCH.md:12-23`, `:259-287`, `:331-371`, `:373-466` — prior research: TL;DR, cap shape, critique, MVP
- `docs/DECISIONS.md:28-92` — pairing via blocking `feed.push` (B2)
- `docs/DECISIONS.md:416-420` — Vault revive/archive, Open
- `docs/DECISIONS.md:851` — no Keychain
- `docs/DECISIONS.md:876-877`, `:930` — `set-environment -g` last-connector-wins; stale `YMUX_PANE_ID`
- `docs/DECISIONS.md:1524`, `:1759-1763` — Vault deferred to external MCP
- `docs/vault/backend-core.md:187-192`, `:372`; `docs/vault/backend-rpc.md:114-118`, `:206-230` — orientation
- `FOLLOWUPS.md` entry "`save_workspace_secret` is write-only" — no DPAPI reader
- `grep -rn 'agent_forward|request_agent_forward|auth-agent' app` → no matches (no agent forwarding)
- https://man.openbsd.org/sshd_config — retrieved 2026-10-06 — AcceptEnv: "The default is not to accept any environment variables."
- https://man.netbsd.org/NetBSD-10.0/tmux.1 — retrieved 2026-10-06 (via search excerpt) — global env copied when server starts; `update-environment` copied on new session/attach
- https://git-scm.com/docs/git-config — retrieved 2026-10-06 — `GIT_CONFIG_COUNT/KEY_<n>/VALUE_<n>` add runtime config
- https://code.claude.com/docs/en/env-vars — retrieved 2026-10-06 — `ANTHROPIC_API_KEY` overrides subscription; `CLAUDE_CONFIG_DIR` ignored in settings `env`
