# Security Policy

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security reports. Instead,
email the maintainer at:

**yyhezkel@gmail.com**

Include:

- A description of the vulnerability and its impact
- Steps to reproduce (proof-of-concept code if available)
- The ymux version (`ymux --version` and the git SHA from
  `ymux dev get-state`)
- Your preferred timeline / disclosure expectations

You should receive an acknowledgement within **7 days**. A more detailed
response, including a remediation plan or assessment that the report
does not apply, will follow within **30 days**.

## Disclosure window

We follow a **90-day coordinated disclosure** policy. After that window,
or once a fix is released (whichever is sooner), reporters are free to
publish.

If a vulnerability is being actively exploited in the wild, we may
accelerate disclosure with the reporter's cooperation.

## What's in scope

- The desktop Tauri app (`ymux.exe`)
- The `ymux` CLI binary (both Windows and the bundled Linux build)
- The `ymux-mcp` MCP server binary
- The JSON-RPC + named-pipe + reverse-SSH-tunnel transport
- The agent-hook contract (Claude Code hook integration)
- Any cryptographic primitive we ship (HMAC-SHA256 handshake, key load
  paths, settings/credentials persistence)

## What's not in scope

- Vulnerabilities in third-party SSH servers we connect to
- Vulnerabilities in the upstream Tauri / Rust / russh / xterm.js
  toolchain — please report those upstream
- Social-engineering attacks on the user
- Issues that require attacker-controlled local file write or
  administrator privileges on the user's machine
- DoS via the user's own typed input

## Hardening notes

- Release binaries are built with `--remap-path-prefix` so that
  `$CARGO_HOME`, `$RUSTUP_HOME`, and `$HOME` are scrubbed from any
  panic-location strings the compiler embeds in `.rodata`.
- The provisioning initial password is held in memory only; it is never
  written to disk.
- The reverse-SSH RPC channel between the remote `ymux` CLI and the
  desktop app authenticates each connection via an HMAC-SHA256
  challenge-response. The shared token is never sent on the wire.
- `known_hosts.json` enforces TOFU on first connect and refuses to
  proceed on host-key mismatch without explicit user override.
- AI agent permission prompts are short-circuited when Claude Code is
  invoked from outside a ymux pane (env-gated by `YMUX_PANE_ID`),
  so hooks installed in `~/.claude/settings.json` cannot be used to
  exfiltrate prompts from unrelated terminals.

## Acknowledgements

Reporters who follow this policy and want public credit will be listed
here once the corresponding advisory is published.
