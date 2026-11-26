# Installing AI coding agents

ymux integrates with four AI coding agents. They're all
optional — pick whichever ones you want. The **Provisioning Wizard**
(sidebar → ☁ Provision server) can install any subset of them on a
fresh remote Linux box, but you can also install them manually anywhere
using the commands below.

| Agent | Vendor | Install medium | License model |
|---|---|---|---|
| [Claude Code](https://docs.claude.com/en/docs/claude-code) | Anthropic | `curl` installer + winget + npm | Subscription / API key |
| [Grok Build](https://docs.x.ai/build) | xAI | `curl` / PowerShell installer | grok.com sign-in or API key |
| [Codex CLI](https://github.com/openai/codex) | OpenAI | npm | Plus / Pro plan or API key |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | Google | npm | Free tier + paid plans |

## Claude Code

### Remote Linux (what the Provisioning Wizard runs)

```bash
curl -fsSL https://claude.ai/install.sh | bash
```

Self-contained installer — no npm prerequisite. Drops a static binary
launcher and updates your shell's PATH on next login.

### Windows desktop

Three options:

```pwsh
# Option 1 — winget (recommended)
winget install Anthropic.ClaudeCode

# Option 2 — PowerShell installer
irm https://claude.ai/install.ps1 | iex

# Option 3 — manual download
# https://docs.claude.com/en/docs/claude-code → Install
```

### macOS

```bash
curl -fsSL https://claude.ai/install.sh | bash
```

(Same installer as Linux.)

### Authentication

After install, run `claude` in any terminal. On a desktop OS this opens
a browser to console.anthropic.com. On a headless server (or when the
browser auto-open fails), `claude` prints a code + URL that you open
on a phone or another machine. The credentials persist in
`~/.claude/credentials.json`.

Alternative for CI / headless: set `ANTHROPIC_API_KEY=sk-ant-...`. Get
a key from [console.anthropic.com/api-keys](https://console.anthropic.com/settings/keys).

## Grok Build

### Remote Linux (what the Provisioning Wizard runs)

```bash
curl -fsSL https://x.ai/cli/install.sh | bash
```

Self-contained installer. No Node.js. The binary lands at
`~/.grok/bin/grok`, and the script adds that directory to the shell
rc when it can. The `all-agents` profile runs this. The `default` and
`hardened` profiles stay Claude-only.

### Windows desktop

```pwsh
irm https://x.ai/cli/install.ps1 | iex
```

The binary lands at `%USERPROFILE%\.grok\bin\grok.exe`. A WinGet
package also exists (`xAI.GrokBuild`); ymux's wizard uses the script
above, and a WinGet install updates with `winget upgrade`, not
`grok update`.

### macOS

```bash
curl -fsSL https://x.ai/cli/install.sh | bash
```

Same installer as Linux.

### Authentication

After install, run `grok` in any terminal. On a desktop OS this opens
a browser to grok.com. Credentials persist in `~/.grok/auth.json`.

Headless / CI: set `XAI_API_KEY`. The wizard cannot sign you in.

### Hooks

`ymux setup-hooks --agent grok` writes `~/.grok/hooks/ymux.json`. The
commands inside are the existing `ymux claude-hook` handler: Grok's
hook stdin carries Claude's field names, so there is no second parser.
`--agent claude` is still the default. `--agent all` registers Claude,
Grok, and the still-empty Codex / Gemini stubs. Connect-time bootstrap
runs the claude and grok adapters (`--source bundled`) and ignores a
machine where that agent is not installed.

Permission cards, the pane traffic light, and Context Rail injection
work for a Grok session inside a ymux pane. Shell calls are judged
with the same Block / Gate rules as Claude's Bash tool. The connect
wizard can resume a Grok session from `~/.grok/sessions` (`grok --resume`
plus a session id). The cost tab counts Grok's cumulative
`turn_completed` tokens and does not price an unknown model. The
daemon transcript route reads `updates.jsonl` when there is no Claude
file for that id.

After the hooks file lands (including when it was already current),
and only when the run is not a dry run, setup-hooks merges two keys
that are missing: `[terminal] alt_screen = "always"` into
`~/.grok/pager.toml`, and `[ui] compact_mode = true` into
`~/.grok/config.toml`. An existing value stays, including `never` and
`false`. A missing file is created with only that section. The write
is atomic. A hooks error skips the merge. The same path runs on
remote bootstrap (tmux) and on the local wizard, including Windows
where the multiplexer is zellij. It does not create `~/.grok` when
grok is not installed, and it does not set
`[scrollback.display] rtl_bidi`.

Which agents the connect wizard offers is a manual mark, not a probe
of what is installed. Settings → Coding agents: Claude defaults on,
Grok defaults off, and a default applies on a server with one agent.
A machine-header toggle, `multi_agent`, shows every agent checked in
Settings; off, the wizard offers only the default. `grok`,
`grok -c`, and `grok --resume <id>` launch as a shell command
(`mode: cmd`), not as Claude mode, so the Hebrew TUI signal stays off.
The resume id has to be one shell token.

## Codex CLI

### Install (any platform with Node.js)

```bash
npm install -g @openai/codex
```

Codex doesn't ship a self-contained installer — Node.js must already be
on PATH. The Provisioning Wizard's "Install Node.js LTS" step covers
this; for manual installs use [nvm](https://github.com/nvm-sh/nvm),
[fnm](https://github.com/Schniz/fnm), or the official Node.js installer.

### Authentication

Two paths:

```bash
# Desktop (has a browser)
codex login

# Headless (server / SSH session)
codex login --device-auth
```

`--device-auth` prints a short code; open the printed URL on any
device, paste the code, and the server picks up the token. Credentials
persist in `~/.codex/`.

Alternative for CI: set `OPENAI_API_KEY=sk-...`. Get a key from
[platform.openai.com/api-keys](https://platform.openai.com/api-keys).
(Note: Codex's authenticated mode bills against your ChatGPT Plus/Pro
subscription; the API-key path bills against your OpenAI API account.)

## Gemini CLI

### Install (any platform with Node.js)

```bash
npm install -g @google/gemini-cli@latest
```

Same Node.js prerequisite as Codex.

### Authentication

Two paths:

```bash
# Free tier — sign in with Google
gemini

# Headless or API-key mode
export GEMINI_API_KEY=AIza...
gemini
```

`gemini` (no args) walks you through OAuth — opens a browser on
desktop, falls back to a device-code flow on headless servers.
Credentials persist in `~/.gemini/`.

For API-key mode get one from [aistudio.google.com/apikey](https://aistudio.google.com/apikey).

## Using the Provisioning Wizard

The wizard's **Configure** step lists every agent above as a checkbox
under "AI coding agents". Three things to know:

1. **Pick any subset.** The `default` profile checks only Claude Code.
   `all-agents` enables Claude, Grok, Codex and Gemini. A profile
   already saved on disk is not rewritten when this list grows; pick
   Grok on that profile by hand, or delete the profile so the built-in
   one is seeded again.
2. **Codex and Gemini both need Node.js.** Grok Build does not. Make
   sure "Install Node.js LTS" is also checked if you want Codex or
   Gemini. The step will fail fast with a clear error if Node isn't on
   PATH.
3. **Authentication is manual.** None of the wizard steps can sign you
   in (that needs a token only you should hold). The execute-step log
   prints what to run on the remote after the wizard finishes (`claude`,
   `grok`, or the agent's login command).

## Combining agents

The four agents can coexist on the same machine without conflict.
They install into distinct locations (`~/.claude/`, `~/.grok/`,
`~/.codex/`, `~/.gemini/`), expose distinct CLI binaries (`claude`,
`grok`, `codex`, `gemini`), and have separate auth state. You can use
one in one ymux pane and another in a sibling pane.

The **agent permission hook** integration ships for Claude Code and
Grok Build. Codex and Gemini still have placeholder specs
(`hooks/codex.json`, `hooks/gemini.json`) with no events. As those
CLIs add a public hook contract, `setup-hooks --agent <name>` grows
the adapter.

## Updating

| Agent | Update command |
|---|---|
| Claude Code | `claude update` (in-place self-update) |
| Grok Build | `grok update` (a WinGet install: `winget upgrade --id xAI.GrokBuild -e`) |
| Codex CLI | `npm update -g @openai/codex` |
| Gemini CLI | `npm update -g @google/gemini-cli` |

## Uninstalling

| Agent | Uninstall |
|---|---|
| Claude Code | `claude uninstall` or remove `~/.claude/` + the PATH bumps |
| Grok Build | remove `~/.grok/bin` (and the PATH line the installer added) and `~/.grok/` if you also want the config. ymux's add-on uninstall only strips `~/.grok/hooks/ymux.json`; it does not delete Grok |
| Codex CLI | `npm uninstall -g @openai/codex` + remove `~/.codex/` |
| Gemini CLI | `npm uninstall -g @google/gemini-cli` + remove `~/.gemini/` |
