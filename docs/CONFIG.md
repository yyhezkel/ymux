# Configuration

File formats and environment variables. All paths use `dirs::config_dir()` on the
Rust side, which is `%APPDATA%` (= `C:\Users\<user>\AppData\Roaming`) on Windows.

## Files

All under `%APPDATA%\ymux\`:

| File | Written by | Read by |
|---|---|---|
| `workspaces.json` | `lib.rs::save_to_disk` (atomic temp+rename, `fsync`) | `lib.rs::load_from_disk` at `setup()` |
| `notes.json` | `notes.rs::save_notes_to_disk` | `notes.rs::load_notes_from_disk` at `setup()` |
| `settings.json` | `settings.rs::save_to_disk` (Phase 9.A) | `settings.rs::load_from_disk` at `setup()` |
| `known_hosts.json` | `lib.rs::save_known_hosts` after host key match/replace | `lib.rs::load_known_hosts` per SSH connect |
| `debug.log` | `lib.rs::dlog` (append-only, all modules) | humans |

Plus, on each SSH-connected remote:

| File | Path | Written by | Read by |
|---|---|---|---|
| `last.env` | `~/.ymux/run/last.env` (mode 0600) | `tunnel::write_remote_env_file` | `cli/main.rs::load_fallback_env_file` |
| `ymux-linux-x64` | `~/.ymux/bin/` | bootstrap SFTP upload | the CLI binary itself; symlink target |
| `ymux` (symlink) | `~/.ymux/bin/` | bootstrap | the user's PATH if they add it |

## `workspaces.json`

The persistent workspaces file.

### Schema

```ts
type WorkspacesFile = {
  version: number;                       // currently 1
  active_workspace_id: string | null;
  workspaces: Workspace[];
  groups?: WorkspaceGroup[];             // cmux-A A2 sidebar sections
};

type Workspace = {
  id: string;                            // "w_<hex_nanos>"
  name: string;
  color?: string;                        // "#7aa2f7"
  cwd?: string;                          // optional starting cwd for local panes
  // legacy field — folded into layout on load if present
  connection?: Connection;
  layout?: LayoutNode;
  // Sidebar nesting. `parent_id` absent = a root row, and only roots
  // participate in `groups`. Written only by the two create paths
  // (`workspace_pin_project_folder`, `workspace_open_worktree`); there
  // is no re-parent gesture, and `load_from_disk` repairs self-parents,
  // dangling ids and cycles so every consumer may assume a forest.
  //
  // `is_project_root` means "this workspace's cwd is a git repo whose
  // worktrees the sidebar lists underneath it". It is NOT derived: a
  // worktree child also has a parent, and `git worktree list` run from a
  // linked worktree returns the same list including main, so scanning
  // one would duplicate the subtree under itself.
  //
  // A worktree child needs no path field of its own — its `cwd` IS the
  // worktree path, which is how a scan row is matched to a workspace.
  parent_id?: string;
  is_project_root?: boolean;             // elided when false
  is_collapsed?: boolean;                // elided when false
  // Phase 84.A: render this workspace's panes as a tab strip (one pane
  // fills the workspace area, the rest keep running in the background)
  // instead of the split grid. Presentational only — `layout` is never
  // touched by the mode, so flipping to tabs and back restores the exact
  // split tree with its ratios. Tabs are derived from the leaves of
  // `layout` in DFS order; the active tab is the active pane.
  tabs_mode?: boolean;                   // elided when false
};

type Connection =
  | { type: "local"; shell?: string }    // shell path; default = pwsh / powershell / cmd
  | { type: "ssh"; host: string; user: string; port: number; key_path?: string };

type LayoutNode =
  | { kind: "pane"; pane_id: string; connection: Connection }
  | {
      kind: "split";
      split_id: string;
      direction: "horizontal" | "vertical";
      first: LayoutNode;
      second: LayoutNode;
      ratio: number;                     // [0.05, 0.95]
    };
```

### Migration: v2/v3 project folders

A `project_folders` array at the file root (with `project_folder_id` /
`worktree_path` on the workspaces that belonged to it) is the pre-tree
shape. `load_from_disk` converts it once: each folder becomes a child
workspace (`is_project_root`, `cwd` = the repo path) under the first root
workspace on the same host, and its worktree workspaces re-parent under
that, keeping their directory as `cwd`. A folder whose host matches no
existing workspace is kept as a root rather than discarded.

This runs before anything else touches the tree, and the file is
persisted only after the new shape is in memory — `save_to_disk` rewrites
the whole file from the struct rather than merging, so an unread legacy
key is gone on the first write.

### Migration

Workspaces written before Phase 4 had a top-level `connection` field and no
`layout`. On load, `lib.rs::load_from_disk` wraps each such workspace's
connection into a single `pane` node with a freshly-generated `pane_id` and
saves the migrated file back. **Since Phase 92 this applies to screens only**:
a header — a root workspace (`parent_id` absent) or a pinned project folder
(`is_project_root`) — has no `layout` by design and is left alone.

### Migration: headers → screens (Phase 92)

Before Phase 92 the machine row and a pinned folder each carried their own
`layout`. On the first load after the upgrade, `migrate_headers_to_screens`
moves every such layout onto a new child workspace named `shell`, inserted
right after its header in the file, with the header's `connection`, `cwd`,
`setup_command` / `teardown_command` / `env` / `auto_port_forward` /
`claude_separate_account` copied and `tabs_mode` moved. Pane ids are
unchanged, so per-pane restore hints keep binding. If `active_workspace_id`
pointed at the header it now points at the shell. Idempotent: a header
without a layout is skipped, and the pre-Phase-4 backfill above no longer
touches headers.

### Example

```json
{
  "version": 1,
  "active_workspace_id": "w_18abf123abc",
  "workspaces": [
    {
      "id": "w_18abf123abc",
      "name": "Local PowerShell",
      "color": "#7aa2f7",
      "layout": {
        "kind": "pane",
        "pane_id": "p_18abf124def_0",
        "connection": { "type": "local" }
      }
    },
    {
      "id": "w_18abf999999",
      "name": "runner1",
      "color": "#7aa2f7",
      "layout": {
        "kind": "split",
        "split_id": "sp_18abff_0",
        "direction": "horizontal",
        "ratio": 0.5,
        "first": {
          "kind": "pane",
          "pane_id": "p_aaa",
          "connection": {
            "type": "ssh",
            "host": "203.0.113.5",
            "user": "runner",
            "port": 22,
            "key_path": "C:\\Users\\me\\.ssh\\runner_key"
          }
        },
        "second": {
          "kind": "pane",
          "pane_id": "p_bbb",
          "connection": {
            "type": "ssh",
            "host": "203.0.113.5",
            "user": "runner",
            "port": 22
          }
        }
      }
    }
  ]
}
```

### Safety

If the file fails to parse on startup, `LoadState` is set to `Failed` and
`persist()` refuses to write thereafter — so a corrupted file is **not** silently
clobbered with empty state. The user has to fix the file and restart.

## `known_hosts.json`

TOFU host-key store (Phase 6.4 — written by `lib.rs::SshClient::check_server_key`).

### Schema

```ts
type KnownHostsFile = {
  hosts: { [hostPort: string]: KnownHost };  // key = "host:port", e.g. "1.2.3.4:22"
};

type KnownHost = {
  type: string;            // ssh-key algorithm name, e.g. "ssh-ed25519"
  fingerprint: string;     // "SHA256:..." (server pubkey fingerprint)
  first_seen: string;      // RFC 3339 UTC, e.g. "2026-04-30T13:04:40Z"
  last_seen: string;       // RFC 3339 UTC
};
```

### Behavior

- First connect to a `host:port` — if `accept_unknown_host=true` was passed
  (= the user clicked Trust on the dialog), record the entry and proceed.
  Otherwise the connection is rejected with `UNKNOWN_HOST:<target>:<keytype>:<fingerprint>`.
- Subsequent connects with matching fingerprint — silent, just bumps `last_seen`.
- Mismatched fingerprint — connection rejected with
  `HOST_KEY_MISMATCH:<target>:<keytype>:<old_fingerprint>:<new_fingerprint>` unless
  the user explicitly clicked Replace, which sets `accept_unknown_host=true` and
  causes the new fingerprint to overwrite.

## `remote-manifest.json`

Bundled resource describing the cross-compiled CLI binaries available for upload.

### Schema

```ts
type RemoteManifest = {
  [triple: string]: {                    // e.g. "x86_64-linux"
    path: string;                        // relative to resources/, e.g. "ymux-linux-x64"
    sha256: string;                      // lowercase hex
    size: number;                        // bytes
    built_at: string;                    // ISO 8601 UTC
  };
};
```

Currently only `x86_64-linux` is shipped. `aarch64-linux` is reserved.

### Encoding

UTF-8 **without** BOM. The writer (`scripts/build-linux-cli.ps1`) uses
`[System.IO.File]::WriteAllText($path, $json, [System.Text.UTF8Encoding]::new($false))`
because Windows PowerShell 5.1's `Set-Content -Encoding utf8` adds a BOM and
`serde_json::from_str` rejects it with `"expected value at line 1 column 1"`.
The reader (`remote_bootstrap::read_manifest`) also strips a leading `\u{FEFF}`
defensively, so a future regression in the writer doesn't silently break
bootstrap again.

## `last.env` (remote)

Plain `KEY=value` per line, one variable per line, written via SSH heredoc to
`~/.ymux/run/last.env` with mode 0600. Loaded by the Linux CLI's
`load_fallback_env_file` if `YMUX_SOCKET_ADDR` isn't already set.

```
YMUX_SOCKET_ADDR=127.0.0.1:23456
YMUX_TUNNEL_TOKEN=A1B2C3...32 alphanum chars total
YMUX_PANE_ID=p_18abc_1
```

## `settings.json`  *(Phase 9.A)*

Persistent app preferences — theme, fonts, terminal behavior, hooks, notifications, and the update-checker. Loaded at `setup()`, defaults written on first run, atomically saved on every change. Mutations emit `settings:changed` so the frontend re-applies the theme live.

### Schema

```ts
type Settings = {
  version: 1;
  theme: {
    preset: "tokyo-night" | "dracula" | "solarized-dark" | "nord" | "solarized-light" | "custom";
    accent: string;          // "#7aa2f7"
    background: string;
    surface: string;
    border: string;
    text_primary: string;
    text_secondary: string;
    success: string;
    warning: string;
    error: string;
    ansi: {                  // 16 xterm colors used by xterm.js
      black: string; red: string; green: string; yellow: string;
      blue: string; magenta: string; cyan: string; white: string;
      bright_black: string; bright_red: string; bright_green: string; bright_yellow: string;
      bright_blue: string; bright_magenta: string; bright_cyan: string; bright_white: string;
    };
  };
  font: {
    ui_family: string;       // "system-ui"
    ui_size_pt: number;
    terminal_family: string; // "Cascadia Mono"
    terminal_size_pt: number;
  };
  terminal: {
    cursor_style: "block" | "bar" | "underline";
    scrollback_lines: number;
    bidi_enabled: boolean;
    allow_proposed_api: boolean;
  };
  hooks: {
    enabled: boolean;
    agents: string[];        // ["claude"]
    policy_preset: "paranoid" | "default" | "relaxed" | "auto";
  };
  notifications: {
    toast_enabled: boolean;
    sound_enabled: boolean;
  };
  updates: {
    check_on_startup: boolean;
    auto_download: boolean;       // currently always false (no signing keys yet)
    manifest_url?: string;        // default: raw.githubusercontent.com/yyhezkel/ymux/main/manifest.json
    last_check_iso?: string;
    last_seen_version?: string;
  };
};
```

### Theme presets

Built-in presets are returned by `settings.get-presets` (RPC) or `ymux settings presets` (CLI). Selecting one overwrites all theme fields; manual color edits flip `theme.preset` to `"custom"`.

- `tokyo-night` (default)
- `dracula`
- `solarized-dark`
- `nord`
- `solarized-light`

### Live theme apply

The frontend reads `settings.theme` on startup and writes the colors as CSS custom properties on `<html>` (`--w-bg`, `--w-accent`, etc.) — `App.css` references all colors through these vars, so a theme change re-tints the entire UI without reload. Subscribed to the `settings:changed` event so updates from the CLI reflect live.

### Update manifest

`settings.updates.manifest_url` (default `https://raw.githubusercontent.com/yyhezkel/ymux/main/manifest.json`) is polled by `updater.rs` on startup (after a 3-second grace period). The manifest is a static JSON file in the repo root — see [docs/RELEASING.md](RELEASING.md) for the full release flow and the manifest schema. The file lives on `main`, so it's globally cached + served fast by GitHub's CDN, with no API rate limits.

## Environment variables

### Read by the CLI

| Var | Default | Effect |
|---|---|---|
| `YMUX_SOCKET_ADDR` | unset | If set (anywhere), CLI uses TCP transport with this `host:port`. Required on Linux. |
| `YMUX_TUNNEL_TOKEN` | unset | If TCP transport is selected, used as the HMAC key for the challenge-response handshake. Required on Linux. |
| `YMUX_PIPE_PATH` | `\\.\pipe\ymux-<USER>` | Override the default named-pipe path (Windows only). |
| `YMUX_PANE_ID` | unset | Stamped on `feed.push` so the agent feed card knows which pane it belongs to. |
| `HOME` | OS-provided | Used to find `~/.ymux/run/last.env` for the fallback env load. |
| `USERNAME` | OS-provided | Used in the default Windows pipe name if `YMUX_PIPE_PATH` is unset. |

### Read by the Windows app

| Var | Default | Effect |
|---|---|---|
| `USERNAME` | OS-provided | Builds the default pipe name. |
| `USERPROFILE` / `HOME` | OS-provided | Used by `try_authenticate` to find default `~/.ssh/id_*` keys. |

### Written into the remote shell

These are set by `set_env` (best-effort) on the SSH shell channel **and** mirrored
into `~/.ymux/run/last.env` so the CLI works either way:

- `YMUX_SOCKET_ADDR=127.0.0.1:<remote_port>` (the port `tcpip_forward` returned)
- `YMUX_TUNNEL_TOKEN=<32-char alphanumeric>`
- `YMUX_PANE_ID=<the workspace's pane>`

Common sshd setups filter unknown env vars via `AcceptEnv`. The file fallback
exists precisely for that case.
