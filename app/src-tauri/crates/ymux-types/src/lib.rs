//! Phase 51.A: pure data types extracted from `app/src-tauri/src/lib.rs`.
//!
//! This crate holds the wire-format / persistence-format types that the
//! rest of the ymux backend (and the future split crates for ssh,
//! pty, feed, workspaces, rpc) all reference. By keeping them here:
//!
//! - The `app` crate doesn't grow new copies as features land.
//! - Future MCP server / CLI subcrates can depend on this type set
//!   without pulling in the full Tauri runtime.
//! - ts-rs binding regeneration is isolated to one place.
//!
//! Intentionally no business logic: just structs, enums, serde attrs,
//! and the small helper functions that serde attrs reference by name
//! (`default_true`, `is_true`, `is_terminal_kind`). Default impls that
//! belong to a type also stay with it.
//!
//! Note: `WorkspacesFile`, `CreateInput`, and the runtime types
//! (`AppState`, `Session`, etc.) intentionally stayed in `app` for the
//! 51.A POC — they're either internal-shaped or tightly coupled to
//! tokio/AppState. Later phase splits will pull more out.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Connection ─────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Connection {
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
    },
    Ssh {
        host: String,
        user: String,
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_path: Option<String>,
    },
    /// **DEPRECATED 2026-08-19 — deserialization shim only. Do not create.**
    ///
    /// Phase 80 ran local panes inside a WSL distro wrapped in tmux, purely
    /// to get session persistence. Native Windows panes have that now via
    /// zellij, so WSL earns nothing and the whole path is gone.
    ///
    /// The variant itself stays for exactly one reason: `Connection` is
    /// `#[serde(tag = "type")]` and lives in `workspaces.json`. Removing it
    /// while any file in the wild still contains `"type":"wsl"` makes serde
    /// reject the WHOLE file, and `load_from_disk` then refuses every later
    /// save — the `load FAILED … persists will refuse` failure already seen
    /// on 2026-08-18. So it must still parse; `migrate_wsl_workspaces`
    /// rewrites it to `Local` on load, and the variant can be deleted a
    /// release later, once no file can still carry one.
    Wsl {
        /// None = the machine's default distro.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        distro: Option<String>,
    },
}

// ─── SplitDirection ─────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

// ─── PaneKind ───────────────────────────────────────────────────────

// Phase 8.A: pane kind. Defaults to Terminal so older workspaces.json
// (no `pane_kind` field) deserialize unchanged. `is_terminal_kind`
// lets serde elide the field on terminal panes, keeping legacy
// round-trips byte-identical.
#[derive(Clone, Copy, Serialize, Deserialize, Default, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    /// Phase 24.D: the removed ClaudeChat / ClaudeLog variants
    /// (Phase 22 / 24.B) are aliased to Terminal so older
    /// workspaces.json files load cleanly — those panes become
    /// Terminals via `backfill_terminal_connections` in
    /// `load_from_disk`. Three competing "talk to claude" UIs felt
    /// fragmented; Yossi paused the unified-view goal pending a
    /// future rebuild.
    #[default]
    #[serde(
        alias = "claudechat",
        alias = "claude_chat",
        alias = "ClaudeChat",
        alias = "claudelog",
        alias = "claude_log",
        alias = "ClaudeLog"
    )]
    Terminal,
    /// Phase 53 (rebased): the per-pane Browser surface was folded
    /// into a workspace-level singleton floating window. Kept here
    /// only so older `workspaces.json` files still deserialize; a
    /// load-time migration (`rewrite_browser_filemanager_panes_to_terminal`,
    /// gated by `phase_53_remove_browser_filemanager_panes`) rewrites
    /// any leftover Browser pane to Terminal on first load post-upgrade.
    /// Do not create new panes with this kind.
    #[deprecated(
        note = "Phase 53 (rebased): browser is now a workspace-level singleton window, not a pane type. Existing panes auto-migrate to Terminal on load."
    )]
    Browser,
    /// Phase 15.B: dual-column file manager (local + remote SFTP).
    /// The pane itself carries no remote state — it picks up the
    /// workspace's SSH session at runtime, so a file-manager pane in
    /// an SSH workspace lights up the right column only after a
    /// terminal pane in that workspace has authenticated.
    ///
    /// Phase 53 (rebased): folded into a workspace-level singleton
    /// floating window. See `Browser` above for the same migration
    /// notes.
    #[serde(rename = "filemanager", alias = "file_manager", alias = "FileManager")]
    #[deprecated(
        note = "Phase 53 (rebased): file manager is now a workspace-level singleton window, not a pane type. Existing panes auto-migrate to Terminal on load."
    )]
    FileManager,
    /// Phase 33: in-app help pane. Renders a markdown document
    /// keyed by `help_topic` on the pane node (e.g. "ssh-key-setup").
    /// Carries no remote state — entirely local, no SSH/PTY.
    Help,
    /// Phase 50 (#2.4): live unified-diff view of a workspace's git
    /// repo. The pane node carries an optional `diff_source` that
    /// selects working-vs-index, working-vs-HEAD, or working-vs-<ref>.
    /// A background watcher polls `git diff` every ~800ms and emits a
    /// `diff-pane-updated` event when the output hash changes.
    Diff,
}

/// Helper for `LayoutNode::Pane.pane_kind` serde elision. Lives here
/// (not in `app`) because serde resolves the function name in the
/// local scope of the type using the attr.
pub fn is_terminal_kind(k: &PaneKind) -> bool {
    matches!(k, PaneKind::Terminal)
}

// ─── DiffSource ─────────────────────────────────────────────────────

// Phase 50: which diff a Diff pane shows. Default = Working (git diff
// with no ref → working tree vs index). `#[serde(tag = "kind")]` so
// the JSON shape mirrors PaneKind's pattern and is easy to switch on
// in TypeScript.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    Working,
    Head,
    Ref { git_ref: String },
}

impl Default for DiffSource {
    fn default() -> Self {
        DiffSource::Working
    }
}

// ─── BrowserState ───────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct BrowserState {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<String>,
    // Phase 8.B: when true (default) and the pane lives in an SSH workspace,
    // navigate-resolve rewrites localhost:N / 127.0.0.1:N to a forwarded local
    // listener. The address bar still shows the original URL.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub forward_localhost: bool,
    // Phase 8.C fix: URL the iframe most recently fired `load` for. Lets
    // `browser-wait` return immediately when the page is already loaded
    // instead of timing out waiting for the next load event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_loaded_url: Option<String>,
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            url: String::new(),
            home_url: None,
            history: Vec::new(),
            forward_localhost: true,
            last_loaded_url: None,
        }
    }
}

pub fn default_true() -> bool {
    true
}
pub fn is_true(b: &bool) -> bool {
    *b
}

pub const BROWSER_HISTORY_MAX: usize = 50;

// ─── LayoutNode ─────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LayoutNode {
    Pane {
        pane_id: String,
        // Phase 8.A: terminal vs browser. Default = Terminal so legacy JSON works.
        #[serde(default, skip_serializing_if = "is_terminal_kind")]
        pane_kind: PaneKind,
        // Phase 8.A: connection is required for terminal panes, absent for browser.
        // Legacy JSON always has it set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<Connection>,
        // Phase 8.A: browser pane state (url, home_url, history). None for terminal panes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser: Option<BrowserState>,
        // Phase 24.D: removed chat / claudelog fields with the
        // ClaudeChat (Phase 22) and ClaudeLog (Phase 24.B) panes.
        // Any leftover objects under those keys in an existing
        // workspaces.json are silently dropped by serde (no
        // `deny_unknown_fields`) and the converted Terminal pane
        // gets a fresh connection via `backfill_terminal_connections`
        // in load_from_disk.
        // Phase 7.A: optional human-readable annotations on each leaf. Both fields
        // serialize-skip when None so existing workspaces.json files round-trip
        // unchanged until the user edits one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        // Phase 81: Claude-derived title, set automatically from the stop
        // hook's transcript summary. Display fallback only — the manual
        // `title` above always wins when present. Kept separate so a hook
        // update never clobbers something the user typed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotation: Option<String>,
        // Phase 31: per-pane identity. None = inherit from the parent
        // workspace's identity (set in Phase 30). The workspace value
        // is what shows in the sidebar/accent strip; the pane value
        // takes precedence in the pane header + window title when set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emoji: Option<String>,
        // Phase 33: which help topic this pane shows (only meaningful
        // for `pane_kind == Help`). Same Option pattern as `browser` —
        // present for Help panes, absent for everything else.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        help_topic: Option<String>,
        // Phase 50: which diff the Diff pane shows. Present for Diff
        // panes, absent for everything else. None on a Diff pane is
        // treated as DiffSource::Working by the backend watcher.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_source: Option<DiffSource>,
        // Phase 52 (BiDi 33B): opt-in PTY-stream bidi filter. None or
        // Some(false) = passthrough; Some(true) = inject FSI/PDI around
        // Latin runs near RTL context. Persists across reloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        smart_bidi: Option<bool>,
    },
    Split {
        split_id: String,
        direction: SplitDirection,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
        ratio: f32,
    },
}

// ─── EnvVar ─────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

// ─── Workspace ──────────────────────────────────────────────────────

#[derive(Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    // Phase 30: per-workspace emoji glyph. Free-form (up to 16 UTF-8 bytes),
    // typically a single grapheme cluster. Renders as a prefix on the
    // sidebar tab and in the OS window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    // Phase 23.D: canonical workspace-level connection. Populated on
    // create AND on load (back-filled from the first Terminal pane's
    // connection if absent). Pane-level `connection` is kept as an
    // optional override for back-compat, but everything that needs
    // to spawn a session (pane_connect / split fallback / FE dropdown)
    // falls through to this field when the pane has no own value.
    // Reason: enforces "SSH workspace never produces a local shell"
    // and lets non-Terminal panes (FileManager / Browser / ClaudeChat)
    // reconnect to the workspace's intended target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<Connection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutNode>,
    // Phase 7.C: per-workspace shell automation. Sent into the spawned shell after a
    // small delay (so the shell has finished printing its banner). `env` is exported
    // first, then `setup_command` runs. `teardown_command` is sent right before
    // disconnect with a brief grace period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,
    // Phase 36 (#2.2) → 39: when true, ymux auto-opens SSH
    // local-forwards for listening ports the remote watcher reports.
    // Phase 39: default flipped to FALSE — users opt in per-workspace
    // (Settings / Ports window). Avoids a flood of forwards on connect
    // and the YMUX-CHALLENGE foot-gun. `#[serde(default)]` → bool
    // false, so older workspaces.json without the field load as OFF.
    #[serde(default)]
    pub auto_port_forward: bool,
    // Phase 49-C: unix-seconds timestamp of the last user activation.
    // Used by the optional auto-destroy sweep at startup. Updated in
    // workspace_set_active. `#[serde(default)]` so existing
    // workspaces.json files load with 0 (treated as "unknown / very
    // old" by the sweep — never deletes on the first run, only after
    // the workspace has been activated at least once this session).
    #[serde(default)]
    pub last_active_at: u64,
    // Phase 49-B: if Some, this workspace is anchored to a git worktree
    // path created by `workspace_create_worktree`. The UI shows a 🌿
    // chip on the workspace tab. The path is what `cd`s into when a
    // pane spawns; it sits alongside (not inside) the original repo so
    // it never collides with the user's working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_worktree: Option<PathBuf>,
    // Phase 78: when true, this workspace uses a DIFFERENT Claude account
    // than the others, so its subscription-usage % must be fetched from
    // this workspace specifically rather than reusing the global (single-
    // account) value. Default false → all workspaces share one account and
    // one fetch. `#[serde(default)]` so older workspaces.json load cleanly.
    #[serde(default)]
    pub claude_separate_account: bool,
    // cmux-A A2: optional workspace group id. Missing/None = ungrouped
    // (renders in the top "Ungrouped" sidebar section). `skip_serializing_if`
    // keeps the field elided so a pre-A2 workspaces.json round-trips
    // byte-identical after touching a workspace that isn't in a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    // beta.3 (ws-dragdrop): 0-based sort key WITHIN a group_id scope
    // (Ungrouped = None scope). Absent → the workspace hasn't been
    // touched by the reorder path yet; the sidebar falls back to
    // insertion order. On first successful `workspace_reorder` the
    // backend fills in consecutive 0..N-1 within each scope, so a
    // pre-beta.3 workspaces.json round-trips byte-identical until the
    // user actually drags something.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    // Sidebar nesting: the workspace this one hangs under. None = a root
    // row, which is what participates in groups. Written only by the three
    // create paths (pin a project folder, open a worktree, open a session
    // from the active-sessions overview); there is no
    // re-parent gesture. `load_from_disk` repairs self-parents, dangling
    // ids and cycles, so every consumer may assume a forest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    // This workspace's `cwd` is a git repo whose worktrees the sidebar
    // lists underneath it. One bit, written only by the pin command.
    //
    // Deliberately NOT derived. "Has a parent" is wrong — worktree
    // children have parents too, and `git worktree list` run from a
    // linked worktree returns the same list including main, so scanning
    // one would duplicate the whole subtree under itself. "Has children"
    // is wrong — a freshly pinned repo has none, which is exactly when
    // the list must appear. "cwd is a repo" is a git round-trip over SSH
    // per row, per render, against the rule that scans are lazy.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_project_root: bool,
    // Persisted collapse state of this workspace's subtree.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_collapsed: bool,
    // Phase 84.A: render this workspace's panes as a tab strip — one pane
    // fills the workspace area, the rest keep running in the background —
    // instead of the split grid.
    //
    // The `layout` tree is UNTOUCHED by the mode. Tabs are just
    // `collect_panes(layout)` in DFS order, so flipping to tabs and back
    // restores the exact split layout with its ratios and nesting. That is
    // the whole reason this is a flag and not a `LayoutNode::Tabs` variant:
    // a third variant would make every `match` on LayoutNode non-exhaustive
    // (~44 sites in the app crate alone), it would be lossy in both
    // directions, and an older ymux reading a `Tabs` node out of
    // workspaces.json could not render it at all.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tabs_mode: bool,
<<<<<<< HEAD
    // Phase 90.B: the multiplexer session this workspace was opened FOR by
    // the active-sessions overview ("Open" = a row of its own in the tree).
    // Written only by `workspace_open_session`, renamed by
    // `tmux_rename_session`. It is what makes Open idempotent (a second
    // Open activates this row instead of creating another), what the
    // sidebar draws the terminal glyph from, and the fallback that lets the
    // row's first pane re-attach to its session after a restart on a machine
    // whose localStorage never saw it. Elided when absent, so every existing
    // workspaces.json round-trips byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
=======
    // BRIEF: the user's one-line session goal ("what I want to do here"),
    // shown on the Briefing card and editable inline there. User content,
    // so it lives in workspaces.json (survives machine changes, rides the
    // three-way merge) rather than localStorage. Elided when unset so an
    // untouched file round-trips byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
>>>>>>> origin/main
}

/// `skip_serializing_if` for bools. Plain `#[serde(default)]` still
/// *writes* `false`, which is why `auto_port_forward` and
/// `claude_separate_account` appear in every persisted file. Those are
/// grandfathered; a new always-emitted key would add bytes to an
/// untouched workspaces.json and break the byte-identical round-trip.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

// ─── cmux-A A2: WorkspaceGroup ──────────────────────────────────────
//
// Sidebar collapsible section. Grouping is presentational only — the
// group carries a name, an accent color (small dot in the header), and
// the collapsed/open state. Assignment is a one-to-many relation via
// `Workspace.group_id` (a workspace belongs to at most one group).

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct WorkspaceGroup {
    pub id: String,
    pub name: String,
    /// One of the eight cmux-A palette colors (frontend-picked hex).
    /// Not validated on the backend — the picker constrains it.
    #[serde(default)]
    pub color: String,
    /// Persisted collapsed/open state. `#[serde(default)]` so an older
    /// workspaces.json (before A2) that grows a group entry via a
    /// hand-edit still loads.
    #[serde(default)]
    pub is_collapsed: bool,
    /// beta.3 (ws-dragdrop): 0-based sort key across groups. Absent →
    /// hasn't been ordered by the reorder path yet; the sidebar falls
    /// back to array position. The backend fills in consecutive 0..N-1
    /// on the first `workspace_group_reorder` call so a pre-beta.3
    /// workspaces.json round-trips byte-identical until the user
    /// actually drags a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
}

// ─── Phase 59: serde back-compat tests ──────────────────────────────
//
// Workspaces.json is the user's persisted state — any serde regression
// in these types would silently corrupt it on the next save. Each test
// pins a specific shape; if a future refactor breaks the wire format
// (renaming a field, dropping an alias, flipping a default), the test
// fails BEFORE a release ships.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Connection ──────────────────────────────────────────────────

    #[test]
    fn connection_local_round_trip_elides_none_shell() {
        let c = Connection::Local { shell: None };
        let v = serde_json::to_value(&c).unwrap();
        // `type: "local"` + no `shell` field because skip_serializing_if.
        assert_eq!(v, json!({ "type": "local" }));
        // Inverse direction must also work.
        let back: Connection = serde_json::from_value(v).unwrap();
        assert!(matches!(back, Connection::Local { shell: None }));
    }

    #[test]
    fn connection_ssh_round_trip_preserves_all_fields() {
        let c = Connection::Ssh {
            host: "h.example.com".into(),
            user: "yossi".into(),
            port: 22,
            key_path: Some("/keys/id_ed25519".into()),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "ssh",
                "host": "h.example.com",
                "user": "yossi",
                "port": 22,
                "key_path": "/keys/id_ed25519",
            })
        );
        // Round-trip identity.
        let back: Connection = serde_json::from_value(v).unwrap();
        match back {
            Connection::Ssh {
                host,
                user,
                port,
                key_path,
            } => {
                assert_eq!(host, "h.example.com");
                assert_eq!(user, "yossi");
                assert_eq!(port, 22);
                assert_eq!(key_path.as_deref(), Some("/keys/id_ed25519"));
            }
            _ => panic!("expected Ssh, got something else"),
        }
    }

    #[test]
    fn connection_wsl_round_trip_elides_none_distro() {
        // Phase 80: default-distro WSL workspace serializes to just the
        // tag, mirroring Local's elided shell.
        let c = Connection::Wsl { distro: None };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v, json!({ "type": "wsl" }));
        let back: Connection = serde_json::from_value(v).unwrap();
        assert!(matches!(back, Connection::Wsl { distro: None }));
    }

    #[test]
    fn connection_wsl_round_trip_preserves_distro() {
        let c = Connection::Wsl {
            distro: Some("Ubuntu".into()),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v, json!({ "type": "wsl", "distro": "Ubuntu" }));
        let back: Connection = serde_json::from_value(v).unwrap();
        match back {
            Connection::Wsl { distro } => assert_eq!(distro.as_deref(), Some("Ubuntu")),
            _ => panic!("expected Wsl"),
        }
    }

    #[test]
    fn connection_ssh_without_key_path_round_trip() {
        // Pre-key SSH workspaces (password auth, no key on disk).
        // key_path omitted on serialize via skip_serializing_if; on
        // deserialize, serde(default) fills None.
        let raw = json!({
            "type": "ssh",
            "host": "h",
            "user": "u",
            "port": 22,
        });
        let c: Connection = serde_json::from_value(raw).unwrap();
        match c {
            Connection::Ssh { key_path, .. } => assert_eq!(key_path, None),
            _ => panic!("expected Ssh"),
        }
    }

    // ── PaneKind ────────────────────────────────────────────────────

    #[test]
    fn pane_kind_lowercase_wire_format() {
        // The TS bindings tooling consumes the same JSON; if any of
        // these renames break, the frontend's discriminated union
        // silently mismatches.
        assert_eq!(
            serde_json::to_value(PaneKind::Terminal).unwrap(),
            json!("terminal")
        );
        #[allow(deprecated)]
        {
            assert_eq!(
                serde_json::to_value(PaneKind::Browser).unwrap(),
                json!("browser")
            );
            assert_eq!(
                serde_json::to_value(PaneKind::FileManager).unwrap(),
                // Phase 53 rename: serialized form is "filemanager",
                // NOT "file_manager".
                json!("filemanager")
            );
        }
        assert_eq!(serde_json::to_value(PaneKind::Help).unwrap(), json!("help"));
        assert_eq!(serde_json::to_value(PaneKind::Diff).unwrap(), json!("diff"));
    }

    #[test]
    #[allow(deprecated)]
    fn pane_kind_legacy_claude_aliases_load_as_terminal() {
        // Phase 24.D: the ClaudeChat / ClaudeLog pane kinds were
        // removed but their JSON tags still appear in older
        // workspaces.json files. They MUST deserialize to Terminal so
        // the file loads without error.
        for tag in [
            "claudechat",
            "claude_chat",
            "ClaudeChat",
            "claudelog",
            "claude_log",
            "ClaudeLog",
        ] {
            let k: PaneKind = serde_json::from_value(json!(tag)).unwrap();
            assert!(
                matches!(k, PaneKind::Terminal),
                "{tag} should deserialize to Terminal, got {k:?}",
            );
        }
    }

    #[test]
    #[allow(deprecated)]
    fn pane_kind_filemanager_legacy_aliases() {
        // Phase 53 (rebased): we renamed `file_manager` → `filemanager`
        // in the serialized form but kept both forms (plus the
        // PascalCase "FileManager") as aliases for back-compat.
        for tag in ["filemanager", "file_manager", "FileManager"] {
            let k: PaneKind = serde_json::from_value(json!(tag)).unwrap();
            assert!(
                matches!(k, PaneKind::FileManager),
                "{tag} should deserialize to FileManager, got {k:?}",
            );
        }
    }

    // ── DiffSource (Phase 50) ───────────────────────────────────────

    #[test]
    fn diff_source_round_trips_all_variants() {
        assert_eq!(
            serde_json::to_value(DiffSource::Working).unwrap(),
            json!({ "kind": "working" })
        );
        assert_eq!(
            serde_json::to_value(DiffSource::Head).unwrap(),
            json!({ "kind": "head" })
        );
        assert_eq!(
            serde_json::to_value(DiffSource::Ref {
                git_ref: "main".into()
            })
            .unwrap(),
            json!({ "kind": "ref", "git_ref": "main" })
        );
        // Round-trip the trickier Ref variant.
        let back: DiffSource =
            serde_json::from_value(json!({ "kind": "ref", "git_ref": "main" })).unwrap();
        assert_eq!(
            back,
            DiffSource::Ref {
                git_ref: "main".into()
            }
        );
    }

    // ── LayoutNode ──────────────────────────────────────────────────

    fn term_pane(id: &str, conn: Option<Connection>) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.into(),
            pane_kind: PaneKind::Terminal,
            connection: conn,
            browser: None,
            title: None,
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }
    }

    #[test]
    fn pane_round_trip_terminal_elides_pane_kind() {
        // is_terminal_kind elides pane_kind when serializing a
        // Terminal pane, so legacy workspaces.json files (pre-Phase
        // 8.A) round-trip byte-identical.
        let p = term_pane("p1", Some(Connection::Local { shell: None }));
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["kind"], "pane");
        assert_eq!(v["pane_id"], "p1");
        // pane_kind MUST be absent from the JSON.
        assert!(v.get("pane_kind").is_none());
        // browser / title / annotation / color / emoji / help_topic /
        // diff_source / smart_bidi all elided too.
        for f in [
            "browser",
            "title",
            "auto_title",
            "annotation",
            "color",
            "emoji",
            "help_topic",
            "diff_source",
            "smart_bidi",
        ] {
            assert!(v.get(f).is_none(), "field {f} should be elided");
        }
    }

    #[test]
    fn pane_round_trip_diff_pane_keeps_diff_source() {
        let p = LayoutNode::Pane {
            pane_id: "pd".into(),
            pane_kind: PaneKind::Diff,
            connection: None,
            browser: None,
            title: None,
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: Some(DiffSource::Head),
            smart_bidi: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["pane_kind"], "diff");
        assert_eq!(v["diff_source"], json!({ "kind": "head" }));
    }

    #[test]
    fn pane_legacy_no_pane_kind_field_defaults_to_terminal() {
        // Pre-Phase 8.A workspaces.json: pane node has no `pane_kind`.
        let raw = json!({
            "kind": "pane",
            "pane_id": "old1",
            "connection": { "type": "local" },
        });
        let n: LayoutNode = serde_json::from_value(raw).unwrap();
        match n {
            LayoutNode::Pane {
                pane_id, pane_kind, ..
            } => {
                assert_eq!(pane_id, "old1");
                assert!(matches!(pane_kind, PaneKind::Terminal));
            }
            _ => panic!("expected Pane"),
        }
    }

    #[test]
    fn layout_deep_split_round_trips_and_preserves_ratio() {
        // Build: Split-V[ Split-H[ pane(p1) | pane(p2) ] | pane(p3) ]
        let inner = LayoutNode::Split {
            split_id: "s_inner".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(term_pane("p1", None)),
            second: Box::new(term_pane("p2", None)),
            ratio: 0.3,
        };
        let outer = LayoutNode::Split {
            split_id: "s_outer".into(),
            direction: SplitDirection::Vertical,
            first: Box::new(inner),
            second: Box::new(term_pane("p3", None)),
            ratio: 0.6,
        };
        let v = serde_json::to_value(&outer).unwrap();
        let back: LayoutNode = serde_json::from_value(v).unwrap();
        // Round-trip preserves the exact structure.
        match back {
            LayoutNode::Split {
                split_id,
                direction,
                first,
                second,
                ratio,
            } => {
                assert_eq!(split_id, "s_outer");
                assert!(matches!(direction, SplitDirection::Vertical));
                assert!((ratio - 0.6).abs() < 1e-6);
                match *first {
                    LayoutNode::Split {
                        ref split_id,
                        ratio,
                        ..
                    } => {
                        assert_eq!(split_id, "s_inner");
                        assert!((ratio - 0.3).abs() < 1e-6);
                    }
                    _ => panic!("inner should be Split"),
                }
                match *second {
                    LayoutNode::Pane { ref pane_id, .. } => assert_eq!(pane_id, "p3"),
                    _ => panic!("second should be Pane"),
                }
            }
            _ => panic!("outer should be Split"),
        }
    }

    // ── Workspace ───────────────────────────────────────────────────

    #[test]
    fn workspace_minimal_legacy_load() {
        // The smallest workspaces.json entry that should still load —
        // simulates an early-phase install where most fields were
        // absent. Each #[serde(default)] fills in.
        let raw = json!({
            "id": "w1",
            "name": "legacy",
        });
        let w: Workspace = serde_json::from_value(raw).unwrap();
        assert_eq!(w.id, "w1");
        assert_eq!(w.name, "legacy");
        assert!(w.color.is_none());
        assert!(w.emoji.is_none());
        assert!(w.cwd.is_none());
        assert!(w.connection.is_none());
        assert!(w.layout.is_none());
        assert!(w.setup_command.is_none());
        assert!(w.teardown_command.is_none());
        assert!(w.env.is_empty());
        assert!(!w.auto_port_forward);
        assert_eq!(w.last_active_at, 0);
        assert!(w.git_worktree.is_none());
        assert!(w.parent_id.is_none());
        assert!(!w.is_project_root);
        assert!(!w.is_collapsed);
        assert!(!w.tabs_mode);
    }

    #[test]
    fn workspace_without_tree_fields_round_trips_byte_identical() {
        // A workspaces.json written before the tree existed must come
        // back out unchanged, so merely opening the app never rewrites
        // the user's file with a pile of null/false keys. This is why
        // the two new bools carry skip_serializing_if — #[serde(default)]
        // alone would write `false` into every workspace on disk.
        // Exactly what the serializer emits for an untouched workspace:
        // `env` is skipped when empty, while `auto_port_forward`,
        // `claude_separate_account` and `last_active_at` are always
        // written — they predate the skip_serializing_if convention and
        // are grandfathered. The three tree keys must NOT join them, and
        // neither must Phase 84.A's `tabs_mode`.
        let raw = json!({
            "id": "w1",
            "name": "legacy",
            "auto_port_forward": false,
            "last_active_at": 0,
            "claude_separate_account": false,
        });
        let w: Workspace = serde_json::from_value(raw.clone()).unwrap();
        let back = serde_json::to_value(&w).unwrap();
<<<<<<< HEAD
        for key in ["parent_id", "is_project_root", "is_collapsed", "tabs_mode", "tmux_session"] {
=======
        for key in ["parent_id", "is_project_root", "is_collapsed", "tabs_mode", "intent"] {
>>>>>>> origin/main
            assert!(back.get(key).is_none(), "{key} must be elided, got {back}");
        }
        assert_eq!(raw, back, "an untouched workspace must round-trip byte-identical");
    }

    #[test]
    fn workspace_ignores_the_abandoned_inline_project_folders_key() {
        // The first cut of this feature hung `project_folders` off the
        // WORKSPACE. That shape shipped on a branch and never reached a
        // release, but a file carrying it must still load rather than
        // hard-error — serde ignores unknown keys, and this test pins
        // that so nobody "helpfully" adds deny_unknown_fields later.
        let raw = json!({
            "id": "w1",
            "name": "srv",
            "project_folders": [
                { "id": "pf_1", "name": "ymux", "path": "/home/y/src/ymux" },
            ],
            "project_folder_id": "pf_1",
            "worktree_path": "/home/y/src/ymux-feature-x",
        });
        let w: Workspace = serde_json::from_value(raw).unwrap();
        assert_eq!(w.id, "w1");
        assert!(w.parent_id.is_none());
        let back = serde_json::to_value(&w).unwrap();
        for key in ["project_folders", "project_folder_id", "worktree_path"] {
            assert!(back.get(key).is_none(), "{key} must not be re-emitted");
        }
    }

    #[test]
    fn workspace_tree_binding_round_trips() {
        // A worktree child: parented, and its cwd IS the worktree path —
        // there is no second field to disagree with it.
        let raw = json!({
            "id": "w2",
            "name": "feature/x",
            "cwd": "/home/y/src/ymux-feature-x",
            "parent_id": "w1",
        });
        let w: Workspace = serde_json::from_value(raw).unwrap();
        assert_eq!(w.parent_id.as_deref(), Some("w1"));
        assert_eq!(w.cwd.as_deref(), Some("/home/y/src/ymux-feature-x"));
        let back = serde_json::to_value(&w).unwrap();
        assert_eq!(back["parent_id"], "w1");
        assert!(back.get("is_project_root").is_none());
    }

    #[test]
    fn project_root_flags_round_trip() {
        let raw = json!({
            "id": "w1",
            "name": "ymux",
            "cwd": "/home/y/src/ymux",
            "parent_id": "w0",
            "is_project_root": true,
            "is_collapsed": true,
        });
        let w: Workspace = serde_json::from_value(raw).unwrap();
        assert!(w.is_project_root);
        assert!(w.is_collapsed);
        let back = serde_json::to_value(&w).unwrap();
        assert_eq!(back["is_project_root"], true);
        assert_eq!(back["is_collapsed"], true);
        assert_eq!(back["parent_id"], "w0");
        assert_eq!(back["cwd"], "/home/y/src/ymux");
    }

    #[test]
    fn workspace_round_trip_with_full_layout() {
        let conn = Connection::Ssh {
            host: "h".into(),
            user: "u".into(),
            port: 2222,
            key_path: None,
        };
        let layout = LayoutNode::Split {
            split_id: "s1".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(term_pane("p1", Some(conn.clone()))),
            second: Box::new(term_pane("p2", Some(conn.clone()))),
            ratio: 0.5,
        };
        let w = Workspace {
            id: "wfull".into(),
            name: "Production".into(),
            color: Some("#7aa2f7".into()),
            emoji: Some("🚀".into()),
            cwd: Some("/home/u".into()),
            connection: Some(conn),
            layout: Some(layout),
            setup_command: Some("tmux source ~/.tmux.conf".into()),
            env: vec![EnvVar {
                key: "FOO".into(),
                value: "bar".into(),
            }],
            auto_port_forward: true,
            last_active_at: 1_700_000_000,
            ..Default::default()
        };
        let v = serde_json::to_value(&w).unwrap();
        // Spot-check the wire format.
        assert_eq!(v["id"], "wfull");
        assert_eq!(v["auto_port_forward"], true);
        assert_eq!(v["last_active_at"], 1_700_000_000u64);
        // teardown_command + git_worktree elided.
        assert!(v.get("teardown_command").is_none());
        assert!(v.get("git_worktree").is_none());
        // env serialized as array of {key, value}.
        assert_eq!(v["env"][0]["key"], "FOO");
        // Round-trip identity.
        let back: Workspace = serde_json::from_value(v).unwrap();
        assert_eq!(back.id, "wfull");
        assert_eq!(back.name, "Production");
        assert_eq!(back.color.as_deref(), Some("#7aa2f7"));
        assert!(back.auto_port_forward);
        assert_eq!(back.env.len(), 1);
    }

    // ── SplitDirection ──────────────────────────────────────────────

    #[test]
    fn split_direction_lowercase_wire_format() {
        assert_eq!(
            serde_json::to_value(SplitDirection::Horizontal).unwrap(),
            json!("horizontal")
        );
        assert_eq!(
            serde_json::to_value(SplitDirection::Vertical).unwrap(),
            json!("vertical")
        );
    }

    // ── BrowserState ────────────────────────────────────────────────

    #[test]
    fn browser_state_defaults_round_trip() {
        let bs = BrowserState::default();
        let v = serde_json::to_value(&bs).unwrap();
        // forward_localhost is the only field that should serialize
        // (default = true; is_true predicate keeps the field out when
        // already true — so the wire format is EMPTY beyond `url`).
        assert_eq!(v["url"], "");
        // home_url, history, last_loaded_url all elided.
        assert!(v.get("home_url").is_none());
        assert!(v.get("history").is_none());
        assert!(v.get("last_loaded_url").is_none());
        // forward_localhost: true → skip_serializing_if(is_true)
        // means it should be elided.
        assert!(v.get("forward_localhost").is_none());
    }

    #[test]
    fn browser_state_forward_localhost_false_round_trips() {
        let bs = BrowserState {
            forward_localhost: false,
            ..BrowserState::default()
        };
        let v = serde_json::to_value(&bs).unwrap();
        assert_eq!(v["forward_localhost"], false);
    }
}
