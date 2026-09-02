// Phase 24.D: claude_chat module deleted with the ClaudeChat pane.
mod addons;
mod bidi_filter;
mod bootstrap_guard;
// Phase 53 (rebased): browser_pane.rs renamed to workspace_browser.rs;
// per-pane commands swapped for workspace-keyed commands.
mod workspace_browser;
mod claude_log;
mod claude_summary;
mod claude_usage;
mod connect_wizard;
mod dev;
mod diff_pane;
mod file_manager;
mod fonts;
// beta.3-lh-insights: native local Insights (sysinfo + bollard) for the
// Monitor panel on Local workspaces. Commands mirror the remote daemon's
// JSON shape so `insights_fetch` can route local vs. SSH transparently.
mod claude_usage_local;
mod insights_local;
mod local_setup;
mod local_wizard;
mod log_sync;
mod notes;
mod osc_notify;
mod pairing;
mod provisioning;
mod pty_decode;
mod remote_bootstrap;
mod rpc_server;
mod sessions_overview;
mod settings;
mod skills;
mod stt;
mod tickets;
mod tray;
mod tunnel_registry;
mod updater;
mod worktrees;
mod workspaces_merge;
// Phase 51.C: `mod tunnel` moved to its own crate ymux-tunnel.
// Existing crate::tunnel::* callsites still resolve via this alias.
use ymux_tunnel as tunnel;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use russh::client;
use russh::ChannelMsg;
// Phase 51.H: russh-keys imports removed (now used only inside ymux-ssh).

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
static PANE_COUNTER: AtomicU64 = AtomicU64::new(0);
static SPLIT_COUNTER: AtomicU64 = AtomicU64::new(0);

// Phase 51.B3: Session/LocalSession/SshSession/SshCmd + SessionMap
// moved to ymux-core. Re-exported below so existing crate::Session,
// crate::SshSession, crate::SshCmd references resolve unchanged.
pub(crate) use ymux_core::{LocalSession, Session, SessionMap, SshCmd, SshSession};
// PaneSessionMap moved to ymux-core (51.B4).
type WorkspacesState = Arc<Mutex<WorkspacesFile>>;

// Phase 51.B3: ForwardEntry + ForwardMap moved to ymux-core.
// Phase 51.B4: PaneSessionMap + CoreState live in ymux-core too.
pub(crate) use ymux_core::{CoreState, ForwardEntry, ForwardMap, PaneSessionMap};


/// Tri-state for whether persistence is safe:
/// - `Loaded`: load_from_disk succeeded (file present or absent doesn't matter — state reflects truth).
/// - `Failed`: load_from_disk hit a real error (read or parse). Persisting would clobber data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadState {
    Loaded,
    Failed,
}

#[derive(Clone, Serialize)]
pub(crate) struct NotificationItem {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) timestamp_ms: u128,
    // Unshipped-fivefer (#1): coarse category for the Notification Center
    // filter — "agent" (hooks/Claude), "notification" (OSC/generic),
    // "error", "build", "mention".
    pub(crate) kind: String,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub(crate) enum FeedItemState {
    Pending,
    Allowed,
    Denied,
    Timedout,
    Passive,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct FeedItem {
    pub(crate) request_id: String,
    pub(crate) kind: String,
    pub(crate) subkind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_id: Option<String>,
    pub(crate) title: String,
    pub(crate) summary: String,
    // serde_json::Value has no fixed shape; surface it as `unknown` on
    // the TS side (caller narrows) rather than ts-rs's default `any`.
    #[ts(type = "unknown")]
    pub(crate) payload: serde_json::Value,
    pub(crate) state: FeedItemState,
    #[ts(type = "number")]
    pub(crate) created_ms: u128,
    pub(crate) blocking: bool,
}

#[derive(Default)]
pub(crate) struct FeedStore {
    pub(crate) items: std::collections::VecDeque<FeedItem>,
    pub(crate) pending: HashMap<String, tokio::sync::oneshot::Sender<String>>,
}

#[allow(dead_code)] // used as documentation; rpc_server has its own copy
const FEED_MAX_ITEMS: usize = 50;

/// Phase 51.B4: the 9 russh/session/forwards/tunnel runtime fields
/// previously inline here moved into `ymux_core::CoreState`. The
/// outer AppState now wraps it and adds the tauri/notes/settings/
/// dev/feed/browser/claude/console/iframe fields that the
/// application shell needs. Callsites access russh state through
/// `state.core.<field>` (e.g. `state.core.sessions.lock()`).
#[derive(Default, Clone)]
pub(crate) struct AppState {
    pub(crate) core: CoreState,
    pub(crate) workspaces: WorkspacesState,
    pub(crate) load_state: Arc<Mutex<Option<LoadState>>>,
    pub(crate) notifications: Arc<Mutex<Vec<NotificationItem>>>,
    pub(crate) pane_status: Arc<Mutex<HashMap<String, String>>>,
    /// issue #4 (ymux-tools Ticker): per-pane current-turn timing, keyed by
    /// pane_id. turn-start = UserPromptSubmit hook, turn-end = Stop hook.
    /// In-memory and session-scoped — the rolling average is a within-session
    /// signal, meaningless after a restart, so it's never persisted.
    pub(crate) agent_runs: Arc<Mutex<HashMap<String, AgentRunState>>>,
    pub(crate) feed: Arc<Mutex<FeedStore>>,
    pub(crate) notes: Arc<Mutex<notes::NotesFile>>,
    // Phase 9.A: persistent app settings (theme, fonts, terminal, hooks, etc.)
    pub(crate) settings: Arc<Mutex<settings::Settings>>,
    // Phase 12.C: small history of recently-used cwds for local PTY workspaces.
    pub(crate) recent_paths: Arc<Mutex<local_wizard::RecentPathsFile>>,
    // Phase 8.E: ring buffer of frontend console.error/warn captures.
    pub(crate) console_buffer: dev::ConsoleBuffer,
    /// Phase 22.B-fix: cached absolute path to the `claude` binary,
    /// keyed by `<workspace_id>:<scope>` where scope is "ssh" or
    /// "local". Detection runs on first chat-send and the result
    /// sticks for the rest of the session — saves a roundtrip per
    /// message and survives the non-interactive-shell PATH gotcha
    /// (SSH execs do NOT source ~/.bashrc, so a `claude` only on
    /// the user's interactive PATH is otherwise invisible).
    pub(crate) claude_paths: Arc<Mutex<HashMap<String, String>>>,
    /// Phase 52 (BiDi 33B): per-pane PTY-stream bidi filter state. The
    /// filter type lives in `app` (not ymux-core) since it's a
    /// feature concern, not core russh/sessions. Lazy-created on
    /// first chunk per pane; toggled via `pane_set_smart_bidi`.
    pub(crate) bidi_filters: bidi_filter::BidiFilterMap,
    /// Phase 53 (rebased): per-workspace child Webview for the
    /// floating Browser window. At most one Webview per workspace
    /// keyed by `workspace_id`. Lives only at runtime — never
    /// persisted to workspaces.json. `workspace_delete` also calls
    /// `workspace_browser::cleanup_workspace_sessions` to remove the
    /// matching `browser-sessions/<workspace_id>/` directory.
    pub(crate) workspace_browsers: workspace_browser::WorkspaceBrowserMap,
    /// Phase 62.A (item D): serializes native Browser Webview creation.
    /// WebView2's `add_child` intermittently returns 0x8007139F
    /// (ERROR_INVALID_STATE) when two creations race, or when a
    /// just-closed webview hasn't fully released its WebView2
    /// environment. Held across the (retrying) slow path in
    /// `workspace_browser_show` so at most one creation runs at a time.
    pub(crate) browser_create_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes the connect-time CLI bootstrap per host, suppresses a
    /// just-failed upload for a few minutes, and records whether the remote
    /// CLI actually matches the binary we embed. See `bootstrap_guard`.
    pub(crate) bootstrap_guard: bootstrap_guard::BootstrapGuard,
    /// Per-workspace reverse-tunnel bookkeeping: the connect lock that stops
    /// `workspace_ensure_connected` and `spawn_ssh` racing, the sticky port
    /// that keeps an already-running `claude` reachable across a reconnect,
    /// and the port/token/owning-session triple that replaced two maps read
    /// independently. See `tunnel_registry`.
    pub(crate) tunnel_registry: tunnel_registry::TunnelRegistry,
    /// Phase 86: one remote `ymux port-watch` per HOST, not per workspace.
    /// Keyed by `bootstrap_guard::host_key(user, host, port)`. The `owner`
    /// is the workspace whose session actually spawned the process (its
    /// slot lives in `core.port_watchers` / `core.port_watcher_tasks` as
    /// before); `subscribers` (owner included) are fanned out to by
    /// `port.opened` / `port.closed`. LOCK ORDER: always taken ALONE — never
    /// while holding any `core.*` lock, and nothing is taken under it.
    pub(crate) port_watcher_hosts: PortWatcherHosts,
}

/// issue #4: per-pane agent turn timing for the ymux-tools chrome Ticker.
/// `turn_started_at` is `Some` while a turn is in flight (set on the
/// UserPromptSubmit hook, cleared on Stop). `sum_ms`/`count` accumulate the
/// durations of completed non-trivial turns for the rolling average. Turns
/// shorter than `AGENT_RUN_MIN_TURN_MS` (text-only) are excluded from the mean.
/// Phase 84.B: the effective state of the agent holding a pane — what the
/// traffic light in the pane header and the tab strip renders.
///
/// Derived here rather than in the frontend for three reasons. The
/// frontend can infer running/done from the turn timing above, but it
/// cannot infer `NeedsInput` from the Notification hook: that hook is
/// deliberately kept off the feed (it was removed entirely in v0.4.4 for
/// being noise there), so nothing about it reaches the UI unless a state
/// machine on this side interprets it. Second, the open thread in
/// docs/DECISIONS.md asks for *effective* state rather than raw hook
/// arrival order, which needs a single writer and a monotonic sequence.
/// Third, a transition table here is unit-testable; a pile of derived
/// signals is not.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaneAgentState {
    /// No hook has ever arrived for this pane. Renders nothing — a pane
    /// running a plain shell must not sprout a status light.
    #[default]
    Unknown,
    /// The agent is working on a turn.
    Running,
    /// The agent finished its turn. Your move.
    ///
    /// This does NOT expire on a timer. "Claude finished and you haven't
    /// dealt with it" is a lasting fact, and the whole point of the light
    /// is glancing at a strip of tabs to see who is done. The decay that
    /// does exist is semantic, not cosmetic: Claude Code fires
    /// Notification/idle_prompt once it has been waiting on you, which
    /// promotes Done → NeedsInput on its own.
    Done,
    /// The agent is blocked on you — a permission prompt, an elicitation
    /// dialog, or it has gone idle waiting for a reply.
    NeedsInput,
}

impl PaneAgentState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PaneAgentState::Unknown => "unknown",
            PaneAgentState::Running => "running",
            PaneAgentState::Done => "done",
            PaneAgentState::NeedsInput => "needs-input",
        }
    }
}

/// Notification types that mean "the agent is blocked on the human".
/// Names come from Claude Code's documented `notification_type` matcher
/// values. Anything not listed — `auth_success`, or a type added upstream
/// after this shipped — produces NO transition at all, so an unknown
/// value can never strand a pane in the wrong colour.
const NEEDS_INPUT_NOTIFICATIONS: &[&str] = &[
    "permission_prompt",
    "idle_prompt",
    "agent_needs_input",
    "elicitation_dialog",
    "elicitation_url_dialog",
];

/// Notification types that mean the human answered and work resumed.
const RESUMED_NOTIFICATIONS: &[&str] = &["elicitation_complete", "elicitation_response"];

#[derive(Default, Clone)]
pub(crate) struct AgentRunState {
    pub(crate) turn_started_at: Option<std::time::SystemTime>,
    pub(crate) sum_ms: u128,
    pub(crate) count: u32,
    /// Phase 84.B: effective agent state, plus when it last actually
    /// changed and a per-pane monotonic counter.
    pub(crate) state: PaneAgentState,
    pub(crate) state_since: Option<std::time::SystemTime>,
    // u32, not u64: ts-rs maps u64 to `bigint` while the Tauri event
    // carries a plain JSON number, and the frontend compares the two.
    // Four billion hooks on a single pane is not a scenario.
    pub(crate) seq: u32,
}

impl AgentRunState {
    /// Rolling average of completed turns, or None if none counted yet.
    pub(crate) fn avg_ms(&self) -> Option<u128> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum_ms / self.count as u128)
        }
    }

    /// Current turn's start as epoch-ms, or None if no turn is in flight.
    pub(crate) fn started_at_ms(&self) -> Option<u128> {
        self.turn_started_at
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
    }

    /// Fold a completed turn's duration into the rolling average — but only
    /// if it ran long enough to be meaningful; text-only turns (< the min)
    /// would drag the mean toward zero, so they're excluded.
    pub(crate) fn record_turn(&mut self, dur_ms: u128) {
        if dur_ms >= AGENT_RUN_MIN_TURN_MS {
            self.sum_ms = self.sum_ms.saturating_add(dur_ms);
            self.count = self.count.saturating_add(1);
        }
    }

    /// `state_since` as epoch-ms, for the frontend's staleness check.
    pub(crate) fn state_since_ms(&self) -> Option<u128> {
        self.state_since
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
    }

    /// Phase 84.B: fold one hook into the effective agent state.
    ///
    /// Returns true when the state actually changed, so the caller can
    /// skip an emit for a no-op. `seq` bumps on every applied transition
    /// (including a no-op one, so the frontend's ordering guard still
    /// advances), while `state_since` is stamped only on a real change —
    /// otherwise a long turn full of tool calls would keep resetting its
    /// own clock and "running for three minutes" would never be true.
    ///
    /// Note `pre-tool-use` is a bonus source of Running, not a reliable
    /// one: the CLI short-circuits that hook in acceptEdits/bypass modes
    /// and never dials the desktop at all. `user-prompt-submit` is the
    /// dependable turn boundary — that is why it exists.
    pub(crate) fn apply_hook(
        &mut self,
        subkind: &str,
        notification_type: Option<&str>,
    ) -> bool {
        let next = match subkind {
            "user-prompt-submit" | "pre-tool-use" => PaneAgentState::Running,
            "stop" => PaneAgentState::Done,
            "notification" => match notification_type {
                Some(t) if NEEDS_INPUT_NOTIFICATIONS.contains(&t) => {
                    PaneAgentState::NeedsInput
                }
                Some(t) if RESUMED_NOTIFICATIONS.contains(&t) => PaneAgentState::Running,
                // Unmapped or absent: no opinion. Leaving the state alone
                // is the honest answer, and it means a notification type
                // added upstream can never freeze a pane on a stale colour.
                _ => return false,
            },
            _ => return false,
        };
        self.seq = self.seq.saturating_add(1);
        if self.state == next {
            return false;
        }
        self.state = next;
        self.state_since = Some(std::time::SystemTime::now());
        true
    }
}

#[cfg(test)]
mod agent_run_tests {
    use super::AgentRunState;

    #[test]
    fn average_excludes_short_turns_and_means_the_rest() {
        let mut r = AgentRunState::default();
        assert_eq!(r.avg_ms(), None, "no turns yet");

        r.record_turn(1_000); // < 2s → text-only, excluded
        assert_eq!(r.count, 0);
        assert_eq!(r.avg_ms(), None);

        r.record_turn(40_000);
        r.record_turn(20_000);
        assert_eq!(r.count, 2);
        assert_eq!(r.avg_ms(), Some(30_000));
    }

    #[test]
    fn started_at_ms_none_when_idle() {
        let mut r = AgentRunState::default();
        assert_eq!(r.started_at_ms(), None);
        r.turn_started_at = Some(std::time::SystemTime::UNIX_EPOCH);
        assert_eq!(r.started_at_ms(), Some(0));
    }

    // ── Phase 84.B: the traffic-light transition table ────────────────

    use super::PaneAgentState;

    #[test]
    fn a_pane_with_no_hooks_has_no_state() {
        // Unknown renders nothing. A plain shell pane must not sprout a
        // status light just because the workspace is in tabs mode.
        assert_eq!(AgentRunState::default().state, PaneAgentState::Unknown);
    }

    #[test]
    fn the_turn_cycle_walks_running_then_done() {
        let mut r = AgentRunState::default();
        assert!(r.apply_hook("user-prompt-submit", None));
        assert_eq!(r.state, PaneAgentState::Running);
        assert!(r.apply_hook("stop", None));
        assert_eq!(r.state, PaneAgentState::Done);
    }

    #[test]
    fn idle_prompt_promotes_done_to_needs_input() {
        // This is the decay for yellow, and it is semantic rather than a
        // timer: Claude Code tells us it is waiting, we do not guess.
        let mut r = AgentRunState::default();
        r.apply_hook("stop", None);
        assert!(r.apply_hook("notification", Some("idle_prompt")));
        assert_eq!(r.state, PaneAgentState::NeedsInput);
    }

    #[test]
    fn every_documented_blocking_notification_means_needs_input() {
        for t in [
            "permission_prompt",
            "idle_prompt",
            "agent_needs_input",
            "elicitation_dialog",
            "elicitation_url_dialog",
        ] {
            let mut r = AgentRunState::default();
            r.apply_hook("user-prompt-submit", None);
            assert!(r.apply_hook("notification", Some(t)), "{t} must transition");
            assert_eq!(r.state, PaneAgentState::NeedsInput, "{t}");
        }
    }

    #[test]
    fn answering_an_elicitation_resumes_running() {
        let mut r = AgentRunState::default();
        r.apply_hook("notification", Some("elicitation_dialog"));
        assert!(r.apply_hook("notification", Some("elicitation_response")));
        assert_eq!(r.state, PaneAgentState::Running);
    }

    #[test]
    fn an_unmapped_notification_changes_nothing() {
        // The load-bearing case: a notification type added upstream after
        // this shipped must leave the light alone rather than freeze it.
        let mut r = AgentRunState::default();
        r.apply_hook("stop", None);
        let seq_before = r.seq;
        for t in [Some("auth_success"), Some("something_invented_in_2027"), None] {
            assert!(!r.apply_hook("notification", t));
        }
        assert_eq!(r.state, PaneAgentState::Done, "state must be untouched");
        assert_eq!(r.seq, seq_before, "a no-op must not bump seq either");
    }

    #[test]
    fn a_stop_arriving_after_a_notification_still_wins() {
        // Hooks are separate processes racing over a socket; ordering is
        // not guaranteed. Last-writer-wins is the documented behaviour —
        // `seq` is what lets the frontend drop a genuinely stale event.
        let mut r = AgentRunState::default();
        r.apply_hook("notification", Some("permission_prompt"));
        r.apply_hook("stop", None);
        assert_eq!(r.state, PaneAgentState::Done);
        assert_eq!(r.seq, 2);
    }

    #[test]
    fn a_long_turn_does_not_keep_resetting_its_own_clock() {
        // state_since must survive repeated pre-tool-use inside one turn,
        // or "running for three minutes" is never true and the staleness
        // cutoff can never fire.
        let mut r = AgentRunState::default();
        assert!(r.apply_hook("user-prompt-submit", None));
        let first = r.state_since;
        assert!(first.is_some());
        for _ in 0..5 {
            assert!(!r.apply_hook("pre-tool-use", None), "already Running");
        }
        assert_eq!(r.state_since, first, "state_since must not move");
        assert_eq!(r.seq, 6, "but every applied hook still advances seq");
    }

    #[test]
    fn unrelated_subkinds_are_ignored() {
        let mut r = AgentRunState::default();
        r.apply_hook("user-prompt-submit", None);
        assert!(!r.apply_hook("session-start", None));
        assert!(!r.apply_hook("post-tool-use", None));
        assert_eq!(r.state, PaneAgentState::Running);
    }
}

/// Minimum turn duration folded into the rolling average (mirrors the
/// statusline hook's gate in ymux-tools/statuslines/hooks/turn-state.js).
pub(crate) const AGENT_RUN_MIN_TURN_MS: u128 = 2000;

pub(crate) static NOTIF_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize)]
struct PtyDataEvent {
    session_id: String,
    data: String,
}

#[derive(Clone, Serialize)]
struct PtyExitEvent {
    session_id: String,
    reason: Option<String>,
}

// ─── Workspace data model ────────────────────────────────────────────────────
//
// Phase 51.A: the data types previously defined inline here moved to
// the `ymux-types` crate (app/src-tauri/crates/ymux-types/) so
// future split crates (ssh, pty, feed, rpc) can reference them without
// pulling in tauri. Re-exported below so all existing
// `crate::Connection` / `crate::Workspace` / etc. paths continue to
// resolve unchanged. ts-rs bindings are generated by the sub-crate's
// own derive — `cargo test` still regenerates `app/src/bindings/*.ts`
// since the export_to path resolves to the same on-disk location.
pub(crate) use ymux_types::{
    BrowserState, Connection, DiffSource, EnvVar, LayoutNode, PaneKind,
    SplitDirection, Workspace, WorkspaceGroup,
};

#[derive(Clone, Serialize, Deserialize, Default)]
struct WorkspacesFile {
    #[serde(default = "default_version", serialize_with = "serialize_schema_version")]
    version: u32,
    #[serde(default)]
    active_workspace_id: Option<String>,
    #[serde(default)]
    workspaces: Vec<Workspace>,
    // cmux-A A2: sidebar collapsible groups. `#[serde(default)]` so a
    // pre-A2 workspaces.json (no `groups` key) loads with an empty
    // vec; the file only grows a `groups` array once the user creates
    // one, keeping the persisted file backwards-compatible.
    #[serde(default)]
    groups: Vec<WorkspaceGroup>,
}

/// The workspaces.json schema this binary writes.
///
/// **Bump this whenever a field is added that an older build would drop on
/// its next save** — that is the entire trigger, and the only one.
///
/// v1 -> v2 (2026-08-23): the field existed since forever and nothing ever
/// read it. It is load-bearing from here on, for the failure recorded as
/// FOLLOWUPS P1 of 2026-08-18: a stale build sharing `%APPDATA%\\ymux`
/// predated `parent_id` / `is_project_root`, serde dropped the keys it did
/// not know, and `save_to_disk` rewrote the WHOLE document from the struct,
/// so every save by the old app erased the workspace nesting. The symptom
/// was maximally misleading: pinned folders reappeared as top-level
/// workspaces, worktrees stopped listing, and NOTHING appeared in the new
/// build's log, because the new build had not done anything.
///
/// **Know what this does and does not buy**, so nobody reads more safety
/// into it than is here:
///   - It stops THIS build clobbering a file written by a FUTURE one. That
///     is the dangerous direction — losing user data to an older binary —
///     and it is a hard refusal.
///   - It cannot stop an ALREADY-SHIPPED 0.4.x build, which never reads
///     this field and will happily write v1 back over a v2 file. Nothing
///     added here can reach a binary that is already on disk. What covers
///     that case is `workspaces_merge`, which re-reads and merges rather
///     than dumping, and treats a field that vanished on the other side as
///     an absence rather than a deletion.
///   - What it adds for that case is a LOG LINE. The original incident
///     cost hours precisely because it was silent; a downgrade of the
///     on-disk version now says so.
pub(crate) const WORKSPACES_SCHEMA_VERSION: u32 = 2;

/// A `version` key that is absent entirely means a pre-versioning file.
fn default_version() -> u32 {
    1
}

/// Stamp the CURRENT schema version on every write, whatever the loaded
/// struct happens to be carrying.
///
/// A `serialize_with` rather than assigning the field somewhere, because
/// the invariant is "what we WRITE is always current" and serialization is
/// the one place that cannot be bypassed. An assignment would have to be
/// repeated at every construction site and would drift the first time one
/// is added.
fn serialize_schema_version<S>(_current: &u32, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    ser.serialize_u32(WORKSPACES_SCHEMA_VERSION)
}

/// The `version` of a workspaces.json document without deserializing the
/// rest of it.
///
/// Deliberately tolerant: the guard it feeds must not turn an unparseable
/// or version-less file into a refusal to save. `None` means "no usable
/// version here", which every caller treats as "carry on" — the existing
/// load-poison gate already handles a genuinely broken file, and a BOM is
/// stripped first because Windows tooling adds them routinely.
/// What `save_to_disk` should do about the version it found on disk.
///
/// Extracted from the save path for the same reason
/// `resolve_effective_session_name` was: the interesting cases need two
/// binaries sharing a config dir, which no test can arrange, and a policy
/// buried in an I/O function is a policy nobody checks.
#[derive(Debug, PartialEq, Eq)]
enum SchemaGate {
    /// Normal: write it.
    Write,
    /// A NEWER build owns the file. Refusing is the whole point.
    Refuse,
    /// An OLDER build rewrote the file since we last wrote it. Write
    /// anyway — the three-way merge is the repair — but say so, because
    /// silence is what made this expensive the first time.
    WarnDowngrade,
}

/// `on_disk` / `last_written` are `None` when the file is absent, empty,
/// unparseable or version-less. All of those mean "carry on": a refusal
/// hangs off this, and refusing on a malformed file would lock the user
/// out of saving entirely.
fn schema_gate(on_disk: Option<u32>, last_written: Option<u32>) -> SchemaGate {
    let Some(disk) = on_disk else {
        return SchemaGate::Write;
    };
    if disk > WORKSPACES_SCHEMA_VERSION {
        return SchemaGate::Refuse;
    }
    match last_written {
        Some(base) if base > disk => SchemaGate::WarnDowngrade,
        _ => SchemaGate::Write,
    }
}

fn schema_version_of(text: &str) -> Option<u32> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("version")?
        .as_u64()
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
}

#[derive(Deserialize)]
pub(crate) struct CreateInput {
    pub(crate) name: String,
    pub(crate) connection: Connection,
    #[serde(default)]
    pub(crate) color: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) setup_command: Option<String>,
    #[serde(default)]
    pub(crate) teardown_command: Option<String>,
    #[serde(default)]
    pub(crate) env: Option<Vec<EnvVar>>,
}

// ─── ID helpers ──────────────────────────────────────────────────────────────

fn next_session_id() -> String {
    format!("s{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn new_pane_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = PANE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("p_{:x}_{:x}", t, n)
}

pub(crate) fn new_split_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = SPLIT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("sp_{:x}_{:x}", t, n)
}

pub(crate) fn new_workspace_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("w_{:x}", t)
}

// ─── Persistence ─────────────────────────────────────────────────────────────

// Phase 51.B1: config_dir + dlog + shell_quote + pure layout walkers
// moved to ymux-core. Re-exported below so every existing
// `crate::dlog` / `crate::shell_quote` / `crate::collect_panes` /
// `crate::first_terminal_connection_pub` / `crate::backfill_terminal_connections`
// callsite resolves unchanged.
pub(crate) use ymux_core::{
    backfill_terminal_connections, clear_debug_log, collect_panes, collect_panes_with_kind,
    config_dir, config_dir_pub, first_terminal_connection, first_terminal_connection_pub,
    log_debug, log_error, log_info, log_warn, prune_logs, shell_quote,
};

/// Phase 38: absolute path to the debug log, for the Settings → Logs
/// UI ("Open folder" / "Copy path"). Single source of truth — matches
/// exactly what `dlog` writes to.
#[tauri::command]
fn log_dir_path() -> Result<String, String> {
    Ok(config_dir()?.join("debug.log").to_string_lossy().to_string())
}

/// Phase 39: last `n` lines of debug.log for the Logs tab viewer. Only
/// the tail end of the file is read (seek from EOF, ~256 KB window) so
/// a multi-MB log doesn't get slurped whole on every 5s refresh.
#[tauri::command]
fn read_log_tail(n: usize) -> Result<String, String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = config_dir()?.join("debug.log");
    if !path.exists() {
        return Ok(String::new());
    }
    let mut f = std::fs::File::open(&path).map_err(|e| format!("open log: {e}"))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    // Read at most the last 256 KB — comfortably more than 200 lines.
    const WINDOW: u64 = 256 * 1024;
    let start = len.saturating_sub(WINDOW);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("read log: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    // If we started mid-file, drop the first (likely partial) line.
    let text = if start > 0 {
        text.splitn(2, '\n').nth(1).unwrap_or("")
    } else {
        &text
    };
    let lines: Vec<&str> = text.lines().collect();
    let tail = if lines.len() > n {
        &lines[lines.len() - n..]
    } else {
        &lines[..]
    };
    Ok(tail.join("\n"))
}

/// Phase 75: clear the debug log now (Settings → Logs "Clear" button).
/// Truncates debug.log and removes the rotated debug.log.1.
#[tauri::command]
fn clear_debug_log_cmd() -> Result<(), String> {
    clear_debug_log()?;
    log_info("LOGS", "debug.log cleared by user");
    Ok(())
}

fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("workspaces.json"))
}

/// Phase 81: stable per-install machine id — the `origin` value written
/// into the server-side session-meta map so the picker can tell which
/// machine created each tmux session. Lives in its own file
/// (%APPDATA%/ymux/machine-id), NOT settings.json, so "Reset all
/// settings" never changes this machine's identity. Generated once as
/// `<sanitized COMPUTERNAME>-<4 hex>`; read back forever after.
pub(crate) fn machine_id() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let path = match config_dir() {
                Ok(d) => d.join("machine-id"),
                Err(_) => return "ymux-unknown".to_string(),
            };
            if let Ok(existing) = std::fs::read_to_string(&path) {
                let existing = existing.trim().to_string();
                if !existing.is_empty() {
                    return existing;
                }
            }
            let host = std::env::var("COMPUTERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "ymux".to_string());
            let host: String = host
                .trim()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let id = format!("{}-{:04x}", host, (nanos & 0xffff) as u16);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // Atomic (Rule #7): tmp + rename, like every other config write.
            let tmp = path.with_file_name(format!("machine-id.{}.tmp", std::process::id()));
            if std::fs::write(&tmp, &id).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
            id
        })
        .clone()
}

/// Phase 81: hex-encoded UTF-8 for `session-meta set --label-hex` — the
/// label crosses an SSH exec as plain hex so Hebrew/RTL text never meets
/// shell quoting.
pub(crate) fn hex_utf8(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// The file content as this process last read or wrote it.
///
/// `save_to_disk` used to dump the whole in-memory struct, which is
/// last-write-wins across the entire document — fine while one app owns
/// the file, and wrong the moment two do. Two do routinely: a stable
/// winmux and a dev build share %APPDATA%\winmux unless somebody
/// remembers WINMUX_CONFIG_DIR, and the older of the two silently drops
/// every field its structs do not know.
///
/// So this is the "base" of a three-way merge. When the file on disk no
/// longer matches it, another writer got there first and we apply only
/// our own delta on top of theirs instead of flattening their work.
static LAST_KNOWN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(crate) fn remember_file_text(text: &str) {
    if let Ok(mut g) = LAST_KNOWN.lock() {
        *g = Some(text.to_string());
    }
}

fn save_to_disk(file: &WorkspacesFile) -> Result<(), String> {
    use std::io::Write as _;

    if file.workspaces.is_empty() && file.active_workspace_id.is_none() {
        log_warn("WORKSPACE", &format!(
            "save_to_disk: writing empty state (workspaces=0). version={}",
            file.version
        ));
    }

    let path = config_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("workspaces.{}.tmp", std::process::id()));
    let mut text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;

    // Re-read before writing. The fast path — nobody else touched the
    // file — is the overwhelmingly common one and costs a single read.
    // The decision itself lives in `workspaces_merge::reconcile` so it is
    // testable without a GUI: an idle app never saves, so the interesting
    // path cannot be reached by launching one and waiting.
    let base_text = LAST_KNOWN.lock().ok().and_then(|g| g.clone());
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();

    // The schema gate, in both directions. See WORKSPACES_SCHEMA_VERSION for
    // what this does and does not cover.
    let disk_version = schema_version_of(&on_disk);
    match schema_gate(disk_version, base_text.as_deref().and_then(schema_version_of)) {
        SchemaGate::Write => {}
        SchemaGate::Refuse => {
            // A NEWER ymux owns this file and knows fields this build would
            // drop. Merging cannot save it — we would have to understand the
            // keys to merge them — so do not write at all.
            let msg = format!(
                "save_to_disk: REFUSING \u{2014} workspaces.json on disk is schema v{} \
                 and this build writes v{}. A newer ymux owns this config dir; \
                 close it, or give this build its own with YMUX_CONFIG_DIR.",
                disk_version.unwrap_or_default(),
                WORKSPACES_SCHEMA_VERSION
            );
            log_error("WORKSPACE", &msg);
            return Err(msg);
        }
        SchemaGate::WarnDowngrade => {
            log_warn(
                "WORKSPACE",
                &format!(
                    "save_to_disk: workspaces.json was rewritten by an OLDER build \
                     (on disk v{}, we last wrote v{}). Fields that build does not \
                     know may have been dropped; the three-way merge below \
                     restores what it can.",
                    disk_version.unwrap_or_default(),
                    WORKSPACES_SCHEMA_VERSION
                ),
            );
        }
    }

    let (reconciled, notes) =
        workspaces_merge::reconcile(&text, base_text.as_deref(), &on_disk);
    for n in &notes {
        log_warn("WORKSPACE", &format!("save_to_disk: {n}"));
    }
    text = reconciled;

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync tmp: {e}"))?;
    }

    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    remember_file_text(&text);
    // The tree shape goes in the line, not just the count. Two pinned
    // folders lost `parent_id` and `is_project_root` with nothing in the
    // log to say when or why — every writer mutates in place, serde
    // round-trips clean, and the load-time repair logs each fix. Whatever
    // did it, the next occurrence is now bracketed by the save before it.
    let roots = file.workspaces.iter().filter(|w| w.parent_id.is_none()).count();
    let repos = file.workspaces.iter().filter(|w| w.is_project_root).count();
    log_debug("WORKSPACE", &format!(
        "save_to_disk: wrote {} bytes ({} workspaces: {} root / {} nested / {} repo) → {:?}",
        text.len(),
        file.workspaces.len(),
        roots,
        file.workspaces.len() - roots,
        repos,
        path
    ));
    Ok(())
}

/// 2026-08-19: rewrite every `Connection::Wsl` to `Connection::Local`.
///
/// WSL existed here only to give local panes tmux persistence; zellij gives
/// native Windows panes the same thing, so the distro is dead weight. The
/// workspace keeps its name, cwd, layout, panes and everything else — only
/// the transport changes, and the panes come back as native Windows shells.
///
/// Walks pane connections too, not just the workspace's: a pane can carry
/// its own `Connection` that overrides the workspace's (see `paneCaps` and
/// `find_pane_connection`), so migrating only the workspace would leave a
/// `wsl` pane inside a `local` workspace with nothing able to spawn it.
///
/// Returns how many connections were rewritten, so the caller persists.
fn migrate_wsl_workspaces(file: &mut WorkspacesFile) -> usize {
    fn migrate_layout(node: &mut LayoutNode) -> usize {
        match node {
            LayoutNode::Pane { connection, .. } => {
                if matches!(connection, Some(Connection::Wsl { .. })) {
                    *connection = Some(Connection::Local { shell: None });
                    1
                } else {
                    0
                }
            }
            LayoutNode::Split { first, second, .. } => {
                migrate_layout(first) + migrate_layout(second)
            }
        }
    }

    let mut n = 0;
    for ws in file.workspaces.iter_mut() {
        if matches!(ws.connection, Some(Connection::Wsl { .. })) {
            ws.connection = Some(Connection::Local { shell: None });
            n += 1;
        }
        if let Some(layout) = ws.layout.as_mut() {
            n += migrate_layout(layout);
        }
    }
    if n > 0 {
        log_info(
            "WORKSPACE",
            &format!("migrated {n} WSL connection(s) to local — WSL support was removed"),
        );
    }
    n
}

fn load_from_disk() -> Result<WorkspacesFile, String> {
    let path = config_path()?;
    log_debug("WORKSPACE", &format!("load_from_disk: path={:?} exists={}", path, path.exists()));
    if !path.exists() {
        log_info("WORKSPACE", "load_from_disk: file absent → fresh empty state (LoadState=Loaded)");
        return Ok(WorkspacesFile {
            version: 1,
            ..Default::default()
        });
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read {:?}: {e}", path))?;
    // Tolerate a UTF-8 BOM. serde_json rejects one with "expected value
    // at line 1 column 1", which sets load_state = Failed and disables
    // persistence entirely — the user is locked out of their own file and
    // the message says nothing about a byte-order mark. Windows puts them
    // there routinely: PowerShell 5.1's `Set-Content -Encoding utf8` and
    // `Out-File` both write one, so a hand-edit through the obvious tool
    // bricks the file.
    let text = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();
    if text.len() != raw.len() {
        log_warn("WORKSPACE", "load_from_disk: stripped a UTF-8 BOM from workspaces.json");
    }
    log_debug("WORKSPACE", &format!("load_from_disk: read {} bytes", text.len()));
    let mut file: WorkspacesFile = serde_json::from_str(&text)
        .map_err(|e| format!("parse {:?}: {e}", path))?;
    // Base for the three-way merge in save_to_disk.
    remember_file_text(&text);
    log_debug("WORKSPACE", &format!(
        "load_from_disk: parsed OK, version={}, {} workspaces, active={:?}",
        file.version,
        file.workspaces.len(),
        file.active_workspace_id
    ));
    // Loading is always allowed — serde carries the unknown keys nowhere,
    // but reading a newer file does no harm and refusing would lock the
    // user out of their own workspaces. Saving is where this gets stopped
    // (see save_to_disk); say it here so the log shows the cause before it
    // shows the first refusal.
    if file.version > WORKSPACES_SCHEMA_VERSION {
        log_warn("WORKSPACE", &format!(
            "load_from_disk: workspaces.json is schema v{} but this build writes v{} \
             \u{2014} a newer ymux has written this config dir. Saving is disabled \
             until that build stops using it.",
            file.version, WORKSPACES_SCHEMA_VERSION
        ));
    }

    let mut migrated = false;
    // 2026-08-19: WSL workspaces become plain local ones. Runs FIRST so no
    // later pass has to know about a connection kind that no longer has a
    // spawn path behind it.
    if migrate_wsl_workspaces(&mut file) > 0 {
        migrated = true;
    }
    // v2/v3 project folders become real workspaces before anything else
    // touches the tree, so the repair pass below sees the final shape.
    if migrate_legacy_project_folders(&mut file, &text) > 0 {
        migrated = true;
    }
    if normalize_parents(&mut file) > 0 {
        migrated = true;
    }
    for ws in file.workspaces.iter_mut() {
        if ws.layout.is_none() {
            // Legacy: workspace existed without a layout. Build a
            // single Terminal pane and seed its connection from the
            // workspace's legacy `connection` field. Keep the same
            // value on the workspace too (Phase 23.D: workspace.connection
            // is now canonical, not consumed).
            let conn = ws
                .connection
                .clone()
                .unwrap_or(Connection::Local { shell: None });
            ws.connection = Some(conn.clone());
            ws.layout = Some(LayoutNode::Pane {
                pane_id: new_pane_id(),
                pane_kind: PaneKind::Terminal,
                connection: Some(conn),
                browser: None,
                title: None,
                auto_title: None,
                annotation: None,
                color: None,
                emoji: None,
                help_topic: None,
                diff_source: None,
                smart_bidi: None,
            });
            migrated = true;
        }
        // Phase 23.D: ensure every workspace has a canonical
        // `connection` field. Old files where the connection lived
        // only on the first Terminal pane get back-filled here. This
        // is what lets pane_connect / split / the frontend dropdown
        // fall back to the workspace's intended connection when a
        // pane doesn't have one of its own (FileManager / Browser /
        // ClaudeChat panes, or a fresh pane added later).
        if ws.connection.is_none() {
            if let Some(layout) = ws.layout.as_ref() {
                if let Some(conn) = first_terminal_connection(layout) {
                    ws.connection = Some(conn);
                    migrated = true;
                }
            }
        }
        // Phase 24.D: rescue Terminal panes that have no connection
        // — most commonly those are former ClaudeChat (Phase 22) or
        // ClaudeLog (Phase 24.B) panes whose PaneKind got aliased
        // back to Terminal at deserialize time but whose connection
        // field was always None. Backfill from ws.connection (which
        // by now is guaranteed to be Some via the block just above)
        // so they're usable instead of dead.
        if let Some(layout) = ws.layout.take() {
            let (new_layout, changed) =
                backfill_terminal_connections(layout, &ws.connection);
            ws.layout = Some(new_layout);
            if changed {
                migrated = true;
                log_info("WORKSPACE", &format!(
                    "load_from_disk: ws={} backfilled Terminal pane connections \
                     (claudechat/claudelog → Terminal migration)",
                    ws.id
                ));
            }
        }
    }
    // beta.3 (ws-dragdrop): backfill `sort_order` on any workspace or
    // group that never went through the reorder path. The pre-beta.3
    // sidebar rendered in insertion order, so that's the stable ordering
    // we crystallize into consecutive 0..N-1 keys — per group_id scope
    // for workspaces, and across the group list for groups. Idempotent:
    // if every entry already has Some(_) this branch is a no-op.
    if backfill_sort_orders(&mut file) {
        migrated = true;
    }
    if migrated {
        log_info("WORKSPACE", "load_from_disk: migration ran — saving migrated layout");
        match save_to_disk(&file) {
            Ok(()) => log_info("WORKSPACE", "load_from_disk: migration save OK"),
            Err(e) => log_warn("WORKSPACE", &format!("load_from_disk: migration save FAILED: {e}")),
        }
    }
    Ok(file)
}

// beta.3 (ws-dragdrop): fill in any missing `sort_order` values with a
// consecutive 0..N-1 sequence per scope. Returns true if anything was
// changed (so the caller knows to save). Runs once at load time and
// then implicitly at each reorder call — every reorder renumbers the
// affected scope(s) fully, so the file stays dense.
fn backfill_sort_orders(file: &mut WorkspacesFile) -> bool {
    let mut changed = false;

    // Workspaces: bucket by group_id (None = Ungrouped scope). Assign
    // 0..N-1 within each bucket using CURRENT insertion order for the
    // ones missing a sort_order; already-numbered entries keep their
    // key. The result: any pre-beta.3 file gets a stable initial order,
    // and any post-beta.3 file with a fresh workspace grafted at the
    // end (create-flow) gets that fresh workspace's None coerced to
    // `max_existing + 1` (i.e. appended below its siblings).
    let mut scopes: std::collections::HashMap<Option<String>, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, w) in file.workspaces.iter().enumerate() {
        scopes.entry(w.group_id.clone()).or_default().push(idx);
    }
    for (_scope, indices) in scopes.iter() {
        // Everything already numbered keeps its key; the max of those
        // keys anchors where new (None) entries append. If the whole
        // scope is None-only we hand out 0..N-1 in insertion order.
        let mut next = indices
            .iter()
            .filter_map(|i| file.workspaces[*i].sort_order)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        for i in indices {
            if file.workspaces[*i].sort_order.is_none() {
                file.workspaces[*i].sort_order = Some(next);
                next += 1;
                changed = true;
            }
        }
    }

    // Groups: single scope, same logic.
    {
        let mut next = file
            .groups
            .iter()
            .filter_map(|g| g.sort_order)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        for g in file.groups.iter_mut() {
            if g.sort_order.is_none() {
                g.sort_order = Some(next);
                next += 1;
                changed = true;
            }
        }
    }

    changed
}

// beta.3 (ws-dragdrop): renumber all workspaces in `scope` to
// consecutive 0..N-1 based on their CURRENT sort_order (ties broken by
// insertion order). Any missing sort_order is treated as +∞ so it lands
// at the end. Used by workspace_reorder after it has slotted the moved
// workspace at the target index. Pure over the file, safe to call
// inside the workspaces lock.
fn renumber_workspace_scope(file: &mut WorkspacesFile, scope: Option<&str>) {
    // Collect indices in this scope, tagged with (sort_order or +∞,
    // insertion_order) so the sort is total.
    let mut in_scope: Vec<(usize, i32, usize)> = file
        .workspaces
        .iter()
        .enumerate()
        .filter(|(_, w)| w.parent_id.is_none() && w.group_id.as_deref() == scope)
        .map(|(idx, w)| (idx, w.sort_order.unwrap_or(i32::MAX), idx))
        .collect();
    in_scope.sort_by_key(|(_, order, ins)| (*order, *ins));
    for (new_key, (idx, _, _)) in in_scope.into_iter().enumerate() {
        file.workspaces[idx].sort_order = Some(new_key as i32);
    }
}

// Diagnostic: tag every persist with its caller so debug.log shows the exact
// Tauri/RPC handler that triggered each save. Helpful while chasing autosave
// loops; safe to remove once the regression is closed out.
#[track_caller]
/// Phase 39.B: flip every workspace whose `auto_port_forward` is true
/// to false. Returns how many were changed (0 on a second run — the
/// migration is idempotent at the data level too, independent of the
/// settings flag).
pub(crate) fn disable_all_auto_port_forward(file: &mut WorkspacesFile) -> usize {
    let mut n = 0;
    for ws in file.workspaces.iter_mut() {
        if ws.auto_port_forward {
            ws.auto_port_forward = false;
            n += 1;
        }
    }
    n
}

/// Phase 53 (rebased): rewrite every PaneKind::Browser /
/// PaneKind::FileManager pane in the file to PaneKind::Terminal. The
/// per-pane Browser / FileManager surface was replaced by workspace-
/// level singleton floating windows; the leftover panes would render
/// as broken under the new layout, so we collapse them to Terminal
/// on first load post-upgrade and reset their `connection` (the
/// inheritance chain in `workspace_split` rehydrates a sensible
/// fallback the next time the pane is touched).
///
/// Returns the count of panes rewritten. Idempotent — a second call
/// finds none to flip.
#[allow(deprecated)]
pub(crate) fn rewrite_browser_filemanager_panes_to_terminal(
    file: &mut WorkspacesFile,
) -> usize {
    fn walk(node: &mut LayoutNode, count: &mut usize) {
        match node {
            LayoutNode::Pane { pane_kind, .. } => {
                if matches!(pane_kind, PaneKind::Browser | PaneKind::FileManager) {
                    *pane_kind = PaneKind::Terminal;
                    *count += 1;
                }
            }
            LayoutNode::Split { first, second, .. } => {
                walk(first, count);
                walk(second, count);
            }
        }
    }
    let mut n = 0;
    for ws in file.workspaces.iter_mut() {
        if let Some(layout) = ws.layout.as_mut() {
            walk(layout, &mut n);
        }
    }
    n
}

pub(crate) fn persist(state: &AppState) -> Result<(), String> {
    // Phase 59.E: caller-location trace demoted dlog → tracing::debug.
    // It fires on EVERY workspace mutation (ratio commits, title
    // edits, splits…) and is engineer diagnostics, not user-facing —
    // Rule 9 audience split. The REFUSING branches below stay on
    // dlog: those are the lines a user needs to see when their
    // workspaces.json stopped saving.
    let caller = std::panic::Location::caller();
    tracing::debug!("persist: called from {}:{}", caller.file(), caller.line());
    // SAFETY GATE: do not persist if load failed. We'd clobber existing data with our
    // empty default state.
    let load_state = *state.load_state.lock().unwrap();
    match load_state {
        Some(LoadState::Loaded) => {}
        Some(LoadState::Failed) => {
            log_error("WORKSPACE", "persist: REFUSING — load_state=Failed, would clobber existing data");
            return Err(
                "persistence disabled: workspaces.json failed to load earlier; \
                 fix the file and restart"
                    .into(),
            );
        }
        None => {
            log_warn("WORKSPACE", "persist: REFUSING — load_state=None (setup hasn't completed)");
            return Err("persistence not yet initialized".into());
        }
    }
    let file = state.workspaces.lock().unwrap().clone();
    save_to_disk(&file)
}

// ─── Tree operations ─────────────────────────────────────────────────────────

pub(crate) fn find_pane_connection(node: &LayoutNode, target: &str) -> Option<Connection> {
    match node {
        LayoutNode::Pane {
            pane_id, connection, ..
        } if pane_id == target => connection.clone(),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_pane_connection(first, target).or_else(|| find_pane_connection(second, target))
        }
    }
}

/// Phase 23.I: look up a pane's user-set title in a layout tree.
/// Used by `pane_connect` to derive a tmux session name from the
/// title (pane title IS the tmux session name).
pub(crate) fn find_pane_title(node: &LayoutNode, target: &str) -> Option<String> {
    match node {
        LayoutNode::Pane { pane_id, title, .. } if pane_id == target => title.clone(),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_pane_title(first, target).or_else(|| find_pane_title(second, target))
        }
    }
}

// Phase 8.A: existence check independent of pane kind. find_pane_connection returns
// None for browser panes (no connection), so callers that only need "does this pane
// exist somewhere in this layout" must use this instead.
pub(crate) fn pane_id_exists_in(node: &LayoutNode, target: &str) -> bool {
    match node {
        LayoutNode::Pane { pane_id, .. } => pane_id == target,
        LayoutNode::Split { first, second, .. } => {
            pane_id_exists_in(first, target) || pane_id_exists_in(second, target)
        }
    }
}

// Phase 24.D: update_chat_pane (Phase 22) and update_claudelog_pane
// (Phase 24.B) walkers were removed alongside the ClaudeChat /
// ClaudeLog pane kinds. The browser walker stays (active feature);
// claude_log_pane_set in claude_log.rs was also removed.

// Phase 51.B1: collect_panes + collect_panes_with_kind moved to ymux-core.

// Phase 8.A: `new_kind` decides whether the spawned sibling is a terminal (default,
// inherits the existing pane's connection) or a browser (with `new_browser_url` as
// the starting page).
// Phase 53 (rebased): `new_kind` should never be Browser or
// FileManager — the frontend's split menu no longer offers them.
// Both arms remain in the match below for back-compat (older
// frontends or RPC calls still typing those strings still work but
// produce deprecated panes; the load-time migration sweeps them
// away on the next restart).
#[allow(deprecated)]
pub(crate) fn split_pane_in(
    node: LayoutNode,
    target: &str,
    dir: SplitDirection,
    new_kind: PaneKind,
    new_browser_url: Option<String>,
    // Phase 23.C: workspace-derived fallback when the source pane has
    // no connection field (FileManager / Browser / ClaudeChat). The
    // caller is responsible for pre-computing this via
    // `first_terminal_connection` + `live_ssh_connection_for_workspace`.
    // Only used when `new_kind == Terminal`. Pass None to keep the
    // legacy Local-fallback behaviour.
    workspace_terminal_fallback: Option<Connection>,
    // Phase 33: help-topic seed for the spawned pane. Only used when
    // `new_kind == Help`. Pattern mirrors `new_browser_url`.
    new_help_topic: Option<String>,
) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            browser,
            title,
            auto_title,
            annotation,
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
        } => {
            if pane_id == target {
                // Phase 50: extended to 5-tuple — Diff panes carry a
                // diff_source. None on a non-Diff pane stays None.
                let (new_kind_resolved, new_conn, new_browser, new_help_t, new_diff_s) =
                    match new_kind {
                    PaneKind::Terminal => {
                        // Inherit chain: source pane's own connection →
                        // workspace-level fallback (any terminal pane or
                        // live SSH session) → Local. Splitting from a
                        // FileManager / Browser pane in an SSH workspace
                        // now correctly produces another SSH terminal,
                        // not a stray local cmd.
                        let conn = connection
                            .clone()
                            .or(workspace_terminal_fallback.clone())
                            .unwrap_or(Connection::Local { shell: None });
                        (PaneKind::Terminal, Some(conn), None, None, None)
                    }
                    PaneKind::Browser => {
                        let url = new_browser_url
                            .clone()
                            .unwrap_or_else(|| "about:blank".to_string());
                        let bs = BrowserState {
                            url: url.clone(),
                            home_url: Some(url),
                            history: Vec::new(),
                            forward_localhost: true,
                            last_loaded_url: None,
                        };
                        (PaneKind::Browser, None, Some(bs), None, None)
                    }
                    PaneKind::FileManager => {
                        // File-manager panes carry no per-pane state in
                        // workspaces.json — local cwd / show_hidden live in
                        // frontend signals; the right column uses whatever
                        // SSH session the workspace currently has.
                        (PaneKind::FileManager, None, None, None, None)
                    }
                    PaneKind::Help => {
                        // Phase 33: in-app help. Topic defaults to
                        // ssh-key-setup since that's the most common
                        // entry point (offered after a password-auth
                        // SSH connect).
                        let topic = new_help_topic
                            .clone()
                            .unwrap_or_else(|| "ssh-key-setup".to_string());
                        (PaneKind::Help, None, None, Some(topic), None)
                    }
                    PaneKind::Diff => {
                        // Phase 50: new Diff panes default to Working
                        // (git diff = working tree vs index). The user
                        // can switch via the source dropdown later.
                        (PaneKind::Diff, None, None, None, Some(DiffSource::Working))
                    }
                };
                let new_pane = LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    pane_kind: new_kind_resolved,
                    connection: new_conn,
                    browser: new_browser,
                    title: None,
                    auto_title: None,
                    annotation: None,
                    // Phase 31: new pane from a split inherits from the
                    // workspace by default (None = inherit). User can
                    // override later via pane_set_identity.
                    color: None,
                    emoji: None,
                    help_topic: new_help_t,
                    diff_source: new_diff_s,
                    smart_bidi: None,
                };
                let original = LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title,
                    auto_title,
                    annotation,
                    // Phase 31: preserve the original pane's identity
                    // across the split — it's the same logical pane,
                    // just relocated under a new Split node.
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                };
                (
                    LayoutNode::Split {
                        split_id: new_split_id(),
                        direction: dir,
                        first: Box::new(original),
                        second: Box::new(new_pane),
                        ratio: 0.5,
                    },
                    true,
                )
            } else {
                (
                    LayoutNode::Pane {
                        pane_id,
                        pane_kind,
                        connection,
                        browser,
                        title,
                        auto_title,
                        annotation,
                        color,
                        emoji,
                        help_topic,
                        diff_source,
                        smart_bidi,
                    },
                    false,
                )
            }
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            let (new_first, found1) = split_pane_in(
                *first,
                target,
                dir.clone(),
                new_kind,
                new_browser_url.clone(),
                workspace_terminal_fallback.clone(),
                new_help_topic.clone(),
            );
            if found1 {
                return (
                    LayoutNode::Split {
                        split_id,
                        direction,
                        first: Box::new(new_first),
                        second,
                        ratio,
                    },
                    true,
                );
            }
            let (new_second, found2) = split_pane_in(
                *second,
                target,
                dir,
                new_kind,
                new_browser_url,
                workspace_terminal_fallback,
                new_help_topic,
            );
            (
                LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(new_first),
                    second: Box::new(new_second),
                    ratio,
                },
                found2,
            )
        }
    }
}

/// Returns (new_root_or_None, removed_pane_id_if_any).
/// new_root is None if the entire tree was just one pane and it was the target (caller
/// should ignore the request; can't close last pane).
fn close_pane_in(node: LayoutNode, target: &str) -> (Option<LayoutNode>, Option<String>) {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            browser,
            title,
            auto_title,
            annotation,
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
        } => {
            // Last pane — can't remove; return unchanged whether or not target matches.
            let _ = pane_id == target;
            (
                Some(LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title,
                    auto_title,
                    annotation,
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                }),
                None,
            )
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            // Direct-leaf optimization: if either child is the target pane, collapse.
            if let LayoutNode::Pane { pane_id, .. } = first.as_ref() {
                if pane_id == target {
                    let removed = pane_id.clone();
                    return (Some(*second), Some(removed));
                }
            }
            if let LayoutNode::Pane { pane_id, .. } = second.as_ref() {
                if pane_id == target {
                    let removed = pane_id.clone();
                    return (Some(*first), Some(removed));
                }
            }
            // Recurse deeper.
            let (new_first_opt, removed1) = close_pane_in(*first, target);
            let new_first = new_first_opt.expect("non-leaf recursion preserves node");
            if removed1.is_some() {
                return (
                    Some(LayoutNode::Split {
                        split_id,
                        direction,
                        first: Box::new(new_first),
                        second,
                        ratio,
                    }),
                    removed1,
                );
            }
            let (new_second_opt, removed2) = close_pane_in(*second, target);
            let new_second = new_second_opt.expect("non-leaf recursion preserves node");
            (
                Some(LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(new_first),
                    second: Box::new(new_second),
                    ratio,
                }),
                removed2,
            )
        }
    }
}

/// Phase 7.A: update title and/or annotation on a pane leaf. Each `Option<Option<…>>`
/// arg has three states: `None` = leave unchanged, `Some(None)` = clear,
/// `Some(Some(value))` = set.
/// Phase 81: same tri-state for `new_auto_title` (the Claude-derived
/// fallback title set from the stop hook).
pub(crate) fn update_pane_in(
    node: LayoutNode,
    target: &str,
    new_title: Option<Option<String>>,
    new_annotation: Option<Option<String>>,
    new_auto_title: Option<Option<String>>,
) -> LayoutNode {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            browser,
            title,
            auto_title,
            annotation,
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
        } => {
            if pane_id == target {
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title: new_title.unwrap_or(title),
                    auto_title: new_auto_title.unwrap_or(auto_title),
                    annotation: new_annotation.unwrap_or(annotation),
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                }
            } else {
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title,
                    auto_title,
                    annotation,
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                }
            }
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => LayoutNode::Split {
            split_id,
            direction,
            first: Box::new(update_pane_in(
                *first,
                target,
                new_title.clone(),
                new_annotation.clone(),
                new_auto_title.clone(),
            )),
            second: Box::new(update_pane_in(
                *second,
                target,
                new_title,
                new_annotation,
                new_auto_title,
            )),
            ratio,
        },
    }
}

/// Phase 81: current auto_title of a pane leaf (None when the pane isn't
/// in this subtree or has no auto_title).
fn pane_auto_title_in(node: &LayoutNode, target: &str) -> Option<String> {
    match node {
        LayoutNode::Pane { pane_id, auto_title, .. } => {
            if pane_id == target { auto_title.clone() } else { None }
        }
        LayoutNode::Split { first, second, .. } => {
            pane_auto_title_in(first, target).or_else(|| pane_auto_title_in(second, target))
        }
    }
}

/// Phase 81.G: which pane does a hook push actually belong to?
///
/// The push carries `pane_id` from the hook process's `YMUX_PANE_ID`
/// env var, which the shell inherited when it was spawned. That value
/// goes stale: a tmux session outlives the pane that created it, and
/// `tmux set-environment -g` is tmux-SERVER-global (last-connector-wins),
/// so it cannot be re-read per session either — on a live box every
/// session reports no per-session value at all and the global one names
/// whichever pane connected most recently. A stale id means
/// `find_workspace_for_pane` misses and the title update silently does
/// nothing, which is exactly how the Phase 81 pane-header path went a
/// month without anyone noticing it never worked.
///
/// `tmux_session` is the trustworthy identifier: the CLI resolves it by
/// asking tmux itself (`session_meta::resolve_session_name`), precisely
/// because the env var can't be trusted. The desktop knows which pane is
/// attached to which tmux session right now, so map back through that.
///
/// Order: trust `pane_id` when the layout actually contains it (cheap,
/// and correct in the common case), else fall back to the tmux name.
pub(crate) fn resolve_hook_pane(
    state: &AppState,
    pane_id: Option<&str>,
    tmux_session: Option<&str>,
) -> Option<String> {
    if let Some(pid) = pane_id {
        let known = {
            let file = state.workspaces.lock().ok()?;
            find_workspace_for_pane(&file, pid).is_some()
        };
        if known {
            return Some(pid.to_string());
        }
    }
    let tmux = tmux_session?;
    let resolved = find_pane_by_tmux_session(state, tmux);
    match &resolved {
        // Rule #1: session/pane names only, never titles or prompts.
        Some(p) => log_debug(
            "RPC",
            &format!(
                "hook pane recovered via tmux: stale_pane_id={} tmux={tmux} pane_id={p}",
                pane_id.unwrap_or("(none)")
            ),
        ),
        None => log_warn(
            "RPC",
            &format!(
                "hook pane unresolved: pane_id={} tmux={tmux} — title update skipped",
                pane_id.unwrap_or("(none)")
            ),
        ),
    }
    resolved
}

/// Phase 81.G: reverse of `lookup_tmux_for_pane` — the pane currently
/// attached to a multiplexer session name. Covers WSL panes (Phase 80 gave
/// `LocalSession` a `tmux_session`) and, since the 2026-08-23 merge, native
/// Windows panes too — those store their ZELLIJ session name in the same
/// field. Only reached when a hook actually sends a name, which today means
/// a tmux-backed pane; the zellij case falls out for free if one ever does.
fn find_pane_by_tmux_session(state: &AppState, tmux_session: &str) -> Option<String> {
    let session_id = {
        let sessions = state.core.sessions.lock().ok()?;
        sessions
            .iter()
            .find(|(_, s)| {
                let name = match s {
                    Session::Ssh(ss) => ss.tmux_session.as_deref(),
                    Session::Local(ls) => ls.tmux_session.as_deref(),
                };
                name == Some(tmux_session)
            })
            .map(|(sid, _)| sid.clone())?
    };
    let pane_sessions = state.core.pane_sessions.lock().ok()?;
    pane_sessions
        .iter()
        .find(|(_, sid)| sid.as_str() == session_id)
        .map(|(pane_id, _)| pane_id.clone())
}

/// Phase 81: persist a Claude-derived title on a pane (stop-hook path,
/// rpc_server feed.push). Touches ONLY `auto_title` — the user's manual
/// `title` always wins in the UI. No-ops when the pane is gone or the
/// value is unchanged (a stop fires every turn; skip the disk churn).
pub(crate) fn update_pane_auto_title(
    state: &AppState,
    app: &AppHandle,
    pane_id: &str,
    new_title: &str,
) {
    let changed = {
        let mut file = state.workspaces.lock().unwrap();
        let Some(ws_id) = find_workspace_for_pane(&file, pane_id) else {
            // Phase 81.G: this used to return in total silence, which is
            // why a permanently broken pane-header path looked like a
            // working one. Callers should pre-resolve via
            // `resolve_hook_pane`; reaching here means even that failed.
            log_warn(
                "WORKSPACE",
                &format!("auto_title: no workspace holds pane_id={pane_id} — skipped"),
            );
            return;
        };
        let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == ws_id) else {
            return;
        };
        let unchanged = ws
            .layout
            .as_ref()
            .and_then(|l| pane_auto_title_in(l, pane_id))
            .as_deref()
            == Some(new_title);
        if unchanged {
            false
        } else {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_pane_in(
                    layout,
                    pane_id,
                    None,
                    None,
                    Some(Some(new_title.to_string())),
                ));
            }
            true
        }
    };
    if changed {
        if let Err(e) = persist(state) {
            log_warn("WORKSPACE", &format!("auto_title: persist failed: {e}"));
        }
        let _ = app.emit("workspaces:changed", ());
    }
}

/// Find the workspace_id whose layout contains the given pane_id. Used by RPC
/// callers (CLI on remote) that know only the pane_id.
pub(crate) fn find_workspace_for_pane(file: &WorkspacesFile, pane_id: &str) -> Option<String> {
    for ws in &file.workspaces {
        if let Some(layout) = &ws.layout {
            if pane_id_exists_in(layout, pane_id) {
                return Some(ws.id.clone());
            }
        }
    }
    None
}

/// beta.3 Fix 2: resolve a workspace id → user-visible name. Returns None
/// when the workspace was deleted or the caller passed an id that never
/// existed. Cheap linear scan (workspace count is small in practice).
pub(crate) fn workspace_name_by_id(file: &WorkspacesFile, id: &str) -> Option<String> {
    file.workspaces.iter().find(|w| w.id == id).map(|w| w.name.clone())
}

fn set_split_ratio_in(node: LayoutNode, target: &str, new_ratio: f32) -> LayoutNode {
    match node {
        p @ LayoutNode::Pane { .. } => p,
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            if split_id == target {
                LayoutNode::Split {
                    split_id,
                    direction,
                    first,
                    second,
                    ratio: new_ratio.clamp(0.05, 0.95),
                }
            } else {
                LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(set_split_ratio_in(*first, target, new_ratio)),
                    second: Box::new(set_split_ratio_in(*second, target, new_ratio)),
                    ratio,
                }
            }
        }
    }
}

// ─── Helpers (PTY events) ────────────────────────────────────────────────────

/// Phase 7.C: shell flavor for env-var syntax + setup-command line ending.
#[derive(Clone, Copy, Debug)]
enum ShellKind {
    PowerShell,
    Cmd,
    Posix,
}

fn detect_shell_kind(cmd: &str) -> ShellKind {
    let lower = cmd.to_ascii_lowercase();
    // Normalize separators before taking the stem, the same way
    // `path_basename` does. `Path::file_stem` only honours the HOST's
    // separator, so on a unix host `c:\windows\system32\powershell.exe`
    // is one long stem and classifies as Posix — which would hand a
    // PowerShell the wrong startup args. Nothing feeds it a Windows path
    // on macOS today, but the classification should not depend on which
    // machine is asking.
    let lower = lower.replace('\\', "/");
    let stem = std::path::Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&lower);
    match stem {
        "pwsh" | "powershell" => ShellKind::PowerShell,
        "cmd" => ShellKind::Cmd,
        _ => ShellKind::Posix,
    }
}

/// Startup args that force a local Windows shell into UTF-8.
///
/// A fresh ConPTY inherits the machine's OEM codepage (862 on a Hebrew
/// install, 437 on en-US), and Windows PowerShell 5.1 additionally
/// defaults `$OutputEncoding` to ASCII. Together that mojibakes every
/// non-Latin byte a native command prints, and turns Hebrew piped *to*
/// a native command into `?`. The SSH/WSL transports never hit this —
/// they land on Linux, which is UTF-8 by construction.
///
/// pwsh 7 is already UTF-8 everywhere, so the PowerShell line is a
/// harmless no-op there; keeping one branch for both avoids a second
/// version probe on every pane spawn. Posix shells (a user-picked
/// git-bash) are left alone.
fn utf8_shell_args(kind: ShellKind) -> Vec<&'static str> {
    match kind {
        // -NoExit keeps the pane interactive; the profile still loads
        // (no -NoProfile), so this only prepends the encoding setup.
        ShellKind::PowerShell => vec![
            "-NoExit",
            "-Command",
            "$null = chcp 65001; \
             [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); \
             $OutputEncoding = [Console]::OutputEncoding",
        ],
        ShellKind::Cmd => vec!["/K", "chcp 65001 >nul"],
        ShellKind::Posix => Vec::new(),
    }
}

fn format_env_line(kind: ShellKind, key: &str, value: &str) -> String {
    match kind {
        ShellKind::PowerShell => {
            // Single-quote in PS doesn't expand variables; double-quote expands.
            // We use single quotes for predictable behavior.
            let escaped = value.replace('\'', "''");
            format!("$env:{} = '{}'", key, escaped)
        }
        ShellKind::Cmd => {
            // cmd's `set` takes raw value; backslash and quotes pass through.
            // Strip newlines defensively.
            let one_line = value.replace(['\n', '\r'], " ");
            format!("set {}={}", key, one_line)
        }
        ShellKind::Posix => {
            // Single-quoted POSIX literal; embedded `'` becomes `'\''`.
            let escaped = value.replace('\'', "'\\''");
            format!("export {}='{}'", key, escaped)
        }
    }
}

fn line_ending_for(_kind: ShellKind) -> &'static str {
    // ConPTY accepts both, but Cmd is happiest with \r and PowerShell with either.
    // Posix prefers \n; \r\n also works for it.
    "\r\n"
}

/// Phase 61: Smart Connect (mode="cmd"/"claude") injection script, shaped
/// for the pane's shell. Phase 65 (bug FF): the command runs as a normal
/// child on every shell (no `exec`), so quitting it returns to the shell
/// prompt instead of dropping the PTY/SSH. Returns "" when there is
/// nothing to inject.
fn build_smart_connect_script(
    kind: ShellKind,
    mode: &str,
    cwd_override: Option<&str>,
    cmd: Option<&str>,
    claude_args: Option<&str>,
) -> String {
    // The command to exec. `None` for a plain connect (mode neither cmd
    // nor claude) — Phase 65 (bug AA): in that case we may still need to
    // `cd` (the "Open in directory" folder picker passes only cwd_override
    // with no mode; previously that produced an empty script → no cd was
    // ever sent and the pane stayed in $HOME).
    let run: Option<String> = match mode {
        "cmd" => match cmd {
            Some(c) if !c.trim().is_empty() => Some(c.trim().to_string()),
            _ => return String::new(),
        },
        "claude" => {
            let args = claude_args.unwrap_or("").trim();
            Some(if args.is_empty() {
                "claude".to_string()
            } else {
                format!("claude {args}")
            })
        }
        _ => None,
    };
    let cwd = cwd_override.map(|s| s.trim()).filter(|s| !s.is_empty());
    if run.is_none() && cwd.is_none() {
        return String::new();
    }
    match kind {
        // Phase 65 (bug FF round 2): run the command, THEN hand off to a
        // fresh interactive shell (`; exec "$SHELL"`). Just removing the
        // old `exec claude` wasn't enough — quitting Claude (Ctrl+C
        // Ctrl+C) still dropped the SSH channel (debug.log: "clean Eof /
        // exit 0", i.e. the interactive bash itself exited on an EOF right
        // after Claude). Chaining `; exec "$SHELL"` means the shell never
        // gets a chance to read that stray EOF — it replaces itself with a
        // brand-new interactive shell the moment Claude returns, so the
        // PTY/SSH stays alive and the user lands back at a prompt. (Yossi's
        // `claude; exec bash` idea, generalized to the user's $SHELL.)
        ShellKind::Posix => match (run.as_deref(), cwd) {
            (Some(r), Some(d)) => {
                format!("cd {} && {r}; exec \"${{SHELL:-bash}}\"\r\n", shell_quote(d))
            }
            (Some(r), None) => format!("{r}; exec \"${{SHELL:-bash}}\"\r\n"),
            (None, Some(d)) => format!("cd {}\r\n", shell_quote(d)),
            (None, None) => String::new(),
        },
        ShellKind::PowerShell => {
            // -LiteralPath: no wildcard expansion on [brackets] in paths.
            let setloc = |d: &str| {
                format!("Set-Location -LiteralPath '{}'", d.replace('\'', "''"))
            };
            match (run.as_deref(), cwd) {
                (Some(r), Some(d)) => format!("{}; {r}\r\n", setloc(d)),
                (Some(r), None) => format!("{r}\r\n"),
                (None, Some(d)) => format!("{}\r\n", setloc(d)),
                (None, None) => String::new(),
            }
        }
        ShellKind::Cmd => {
            // `/d` switches drive too. cmd can't escape `"` inside a quoted
            // arg — strip quotes/newlines instead.
            let cd = |d: &str| {
                let clean = d.replace(['"', '\n', '\r'], "");
                format!("cd /d \"{clean}\"")
            };
            match (run.as_deref(), cwd) {
                (Some(r), Some(d)) => format!("{} && {r}\r\n", cd(d)),
                (Some(r), None) => format!("{r}\r\n"),
                (None, Some(d)) => format!("{}\r\n", cd(d)),
                (None, None) => String::new(),
            }
        }
    }
}

/// Phase 7.C: after the shell has had a moment to print its banner and prompt,
/// inject the workspace's `env` exports + `setup_command` as if the user typed them.
/// Phase 11.A: tmux session names disallow `.` and `:` and (for sane shell
/// quoting) we also strip whitespace. Pane ids look like `p_<hex>_<n>`
/// already so this is a no-op in practice; the sanitizer is defensive
/// against future id format changes.
pub(crate) fn sanitize_tmux_session_name(pane_id: &str) -> String {
    let cleaned: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("ymux-{}", cleaned)
}

/// One character of a session name, judged by a single question: is it
/// safe to interpolate UNQUOTED into a command line typed into cmd.exe,
/// PowerShell or a POSIX shell?
///
/// A whitelist rather than a blacklist of metacharacters, because the
/// three shells do not agree on what is special and a blacklist silently
/// misses whatever the next one adds.
///
/// Non-ASCII passes wholesale, and that clause is what keeps Phase 23.I's
/// promise that a Hebrew or CJK title becomes a session of the same name:
/// every metacharacter in all three shells is ASCII, so no letter outside
/// it needs a special case — combining marks (niqqud) included. Controls
/// and separators are excluded first, which also catches the non-ASCII
/// line breaks (U+0085, U+2028, U+2029) that `is_ascii()` would wave past.
pub(crate) fn session_name_char_is_safe(c: char) -> bool {
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || !c.is_ascii()
}

/// Phase 23.I: derive a tmux/zellij session name from a user-supplied
/// pane title. Keeps Unicode (Hebrew, Arabic, CJK, etc.) so a title like
/// "מחקר X" becomes a session literally named "מחקר_X". Returns None
/// when the title is empty or becomes empty after sanitization; the
/// caller falls back to the pane-id-derived name in that case.
///
/// **This is a security boundary, not a cosmetic cleanup.** Fixed
/// 2026-08-23, FOLLOWUPS P1 of 2026-08-20. The name returned here is
/// interpolated into `zellij attach -c <name>` by
/// `build_zellij_attach_command`, which TYPES that line into the user's
/// cmd.exe or PowerShell. Until this change the only substitutions were
/// `.`, `:` and whitespace — tmux's own blockers — so `;`, `&`, `|`, `$`,
/// backticks, quotes and `>` rode a pane title straight into a shell
/// line, and a pane titled `work; calc` produced a session name that ran
/// `calc`. Rule #3 in spirit: the command was built by string
/// concatenation, and the comment at the concatenation site asserted a
/// sanitizer (`sanitize_session_name`) that has never existed here.
///
/// The whitelist lives HERE, at the source, rather than as quoting at
/// each use site, because there is no quoting that is correct in cmd.exe
/// and PowerShell at the same time — they disagree about `^`, `%`,
/// backticks and single quotes — and the attach line must parse in both.
///
/// Self-inflicted (the user's own title, their own shell), so this was
/// never a privilege boundary. The mundane consequence is the expensive
/// one: a title with a metacharacter produced a session under a DIFFERENT
/// name than the one ymux went on to track, which is a candidate
/// explanation for "Kill session did nothing".
pub(crate) fn sanitize_tmux_session_name_for_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_was_underscore = false;
    for c in trimmed.chars() {
        let replaced = if session_name_char_is_safe(c) { c } else { '_' };
        if replaced == '_' {
            // Collapse runs of underscores (from whitespace runs) to one.
            if prev_was_underscore {
                continue;
            }
            prev_was_underscore = true;
        } else {
            prev_was_underscore = false;
        }
        out.push(replaced);
    }
    // Trim leading/trailing underscores left over from the trim+replace.
    let trimmed_out = out.trim_matches('_').to_string();
    if trimmed_out.is_empty() {
        return None;
    }
    // Cap at 100 chars by char (not byte) count so we don't slice
    // mid-codepoint on Hebrew/Arabic/CJK titles.
    if trimmed_out.chars().count() > 100 {
        let truncated: String = trimmed_out.chars().take(100).collect();
        Some(truncated)
    } else {
        Some(trimmed_out)
    }
}

/// Phase 23.I's session-name precedence, extracted 2026-08-23:
///   1. Caller-supplied name (the picker chose an explicit existing session)
///   2. Sanitized pane title (the pane title IS the session name —
///      Hebrew/Arabic/CJK titles supported)
///   3. `None` — the spawn paths then fall back to
///      `sanitize_tmux_session_name(&pane_id)`, the legacy `ymux-<paneid>`.
///
/// Extracted rather than copied because `pane_target_session_state` has to
/// answer "does the session this pane is ABOUT to attach to already exist?",
/// and a second hand-written copy of this precedence that drifted from the
/// first would reintroduce exactly the bug that command exists to prevent:
/// the guard would check one name while the attach used another.
fn resolve_effective_session_name(
    explicit: Option<&str>,
    pane_title: Option<&str>,
) -> Option<String> {
    explicit
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| pane_title.and_then(sanitize_tmux_session_name_for_title))
}

/// The name a pane will actually land on, with rung 3 resolved. Only the
/// existence probe needs this — the spawn paths apply the fallback themselves
/// and must keep doing so, since `None` also carries "no explicit choice" to
/// code between here and there.
fn session_name_for_pane(
    explicit: Option<&str>,
    pane_title: Option<&str>,
    pane_id: &str,
) -> String {
    resolve_effective_session_name(explicit, pane_title)
        .unwrap_or_else(|| sanitize_tmux_session_name(pane_id))
}

fn schedule_setup_injection(
    sessions: SessionMap,
    session_id: String,
    shell_kind: ShellKind,
    env: Vec<EnvVar>,
    setup_command: Option<String>,
) {
    let setup = setup_command.filter(|s| !s.is_empty());
    if env.is_empty() && setup.is_none() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let mut bytes: Vec<u8> = Vec::new();
        let eol = line_ending_for(shell_kind);
        for v in &env {
            bytes.extend_from_slice(format_env_line(shell_kind, &v.key, &v.value).as_bytes());
            bytes.extend_from_slice(eol.as_bytes());
        }
        if let Some(s) = setup {
            bytes.extend_from_slice(s.as_bytes());
            bytes.extend_from_slice(eol.as_bytes());
        }
        if bytes.is_empty() {
            return;
        }
        let mut sessions = sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(&session_id) {
            match s {
                Session::Local(l) => {
                    use std::io::Write as _;
                    let _ = l.writer.write_all(&bytes);
                    let _ = l.writer.flush();
                }
                Session::Ssh(ssh) => {
                    let _ = ssh.try_send(SshCmd::Data(bytes));
                }
            }
        }
    });
}

fn pick_default_shell(requested: Option<String>) -> String {
    if let Some(s) = requested.filter(|s| !s.is_empty()) {
        return s;
    }
    pick_platform_default_shell()
}

#[cfg(windows)]
fn pick_platform_default_shell() -> String {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for candidate in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(candidate).is_file() {
                return candidate.to_string();
            }
        }
    }
    "cmd.exe".into()
}

/// macOS port: what Terminal.app would open — the user's login shell
/// (`$SHELL`), else zsh (the macOS default since Catalina), bash, sh.
#[cfg(not(windows))]
fn pick_platform_default_shell() -> String {
    if let Some(s) = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && Path::new(s).is_file())
    {
        return s;
    }
    for candidate in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    "/bin/sh".into()
}

/// The UTF-8 locale a Terminal.app window would get: the system locale
/// (`defaults read -g AppleLocale`, e.g. `he_IL`) if the OS ships it under
/// /usr/share/locale, else `en_US.UTF-8`. Probed once per process.
#[cfg(not(windows))]
fn default_utf8_locale() -> &'static str {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let apple = if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/defaults")
                .args(["read", "-g", "AppleLocale"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };
        apple
            // `he_IL@currency=ILS` style suffixes are not locale dirs.
            .map(|l| l.split('@').next().unwrap_or("").to_string())
            .filter(|l| {
                !l.is_empty()
                    && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && Path::new("/usr/share/locale").join(format!("{l}.UTF-8")).is_dir()
            })
            .map(|l| format!("{l}.UTF-8"))
            .unwrap_or_else(|| "en_US.UTF-8".to_string())
    })
    .as_str()
}

/// Split a user-typed local shell command into (program, args).
///
/// Windows never needed this — ConPTY takes one command line and
/// `CommandBuilder::new("wsl.exe bash -l")` just works. A unix `execvp`
/// wants argv, so `"zsh -l"` must become `["zsh", "-l"]` or the spawn
/// fails with "no such file". Whitespace split is enough for a shell
/// invocation (paths with spaces are not a thing for /bin/* shells);
/// no quoting rules on purpose — this is a shell picker, not a shell.
#[cfg_attr(windows, allow(dead_code))]
fn split_shell_command(cmd: &str) -> (String, Vec<String>) {
    let mut parts = cmd.split_whitespace().map(str::to_string);
    let program = parts.next().unwrap_or_default();
    (program, parts.collect())
}

fn emit_data(
    app: &AppHandle,
    session_id: &str,
    bytes: &[u8],
    // Per-stream UTF-8 reassembly state (see `pty_decode`). One instance
    // per byte stream — an SSH channel's stdout and stderr each get their
    // own, because splicing them into a single buffer would join the tail
    // of one character to the head of another.
    stream: &mut pty_decode::Utf8Stream,
    // Phase 35 (#1.2): OSC-notification side channel. The parser
    // observes the RAW bytes (OSC sequences are ASCII, so this is
    // independent of the utf8 reassembly below) and emits an
    // `osc-notification` event per detected sequence. The byte stream
    // forwarded to xterm.js is untouched.
    pane_id: &str,
    osc: &mut osc_notify::OscNotifyParser,
    // Phase 52 (BiDi 33B): per-pane bidi filter map. When the pane's
    // smart_bidi toggle is on, the chunk passes through `apply_to_pane`
    // before being decoded as UTF-8 and emitted. When off, this is a
    // memcpy (filter.enabled = false fast-path) and the bytes flow
    // through unchanged.
    bidi_filters: &bidi_filter::BidiFilterMap,
) {
    for n in osc.feed(bytes) {
        let _ = app.emit(
            "osc-notification",
            serde_json::json!({
                "pane_id": pane_id,
                "title": n.title,
                "body": n.body,
                "kind": n.kind.as_str(),
            }),
        );
    }

    // Phase 52: optional bidi rewrite. Operates on raw bytes BEFORE
    // UTF-8 reassembly so the filter's escape-sequence state machine
    // sees ANSI/CSI/OSC/DCS verbatim. The filter is itself a no-op
    // when smart_bidi is off for this pane.
    //
    // Note: unlike `stream` and `osc`, this state is keyed by pane and so
    // is still SHARED across an SSH channel's stdout and stderr. Giving
    // stderr its own entry would need a synthetic key, which would miss
    // the per-pane smart_bidi toggle and silently leave stderr unfiltered
    // — worse than the rare escape-splice it would prevent. Left shared
    // deliberately; see FOLLOWUPS.
    let filtered = bidi_filter::apply_to_pane(bidi_filters, pane_id, bytes);

    // Incremental UTF-8 reassembly: an incomplete trailing character is
    // carried to the next chunk (so Hebrew/emoji split across two reads
    // survive), while bytes that can never become valid are replaced with
    // U+FFFD and skipped. The skip is what keeps the pane alive — the old
    // decoder stalled forever on a leading invalid byte.
    let decoded = stream.push(&filtered);

    if let Some(n) = decoded.first_invalid {
        // Rule #1: metadata only — counts and ids, never the bytes.
        // Logged once per stream so a `cat` on a binary file can't flood
        // the log; later drops are silent.
        log_warn(
            "PTY",
            &format!(
                "utf8: dropped {n} invalid byte(s) on pane={pane_id} \
                 session={session_id}, substituted U+FFFD \
                 (binary output or a non-UTF-8 codepage?); \
                 further occurrences on this stream are not logged"
            ),
        );
    }

    if decoded.text.is_empty() {
        return;
    }
    let _ = app.emit(
        "pty:data",
        PtyDataEvent {
            session_id: session_id.to_string(),
            data: decoded.text,
        },
    );
}

/// Emits a transient status text for a pane. Used by remote-bootstrap to surface
/// progress/errors. The frontend listens on `pane:status` events.
pub(crate) fn emit_pane_status_event(app: &AppHandle, pane_id: &str, text: &str) {
    let _ = app.emit(
        "pane:status",
        serde_json::json!({ "pane_id": pane_id, "text": text }),
    );
}

/// Record whether a workspace's remote CLI matches the binary we embed, and
/// tell the frontend. The event drives a banner that persists until the state
/// changes — unlike `pane:status`, which self-clears and therefore hid this
/// exact condition. Hashes are truncated for display; the full pair is in
/// debug.log.
pub(crate) fn set_cli_alignment(
    app: &AppHandle,
    state: &AppState,
    workspace_id: &str,
    alignment: bootstrap_guard::Alignment,
) {
    state
        .bootstrap_guard
        .set_alignment(workspace_id, alignment.clone());
    let payload = match &alignment {
        bootstrap_guard::Alignment::Ok => serde_json::json!({
            "workspace_id": workspace_id,
            "aligned": true,
        }),
        bootstrap_guard::Alignment::Skew {
            expected,
            actual,
            reason,
        } => serde_json::json!({
            "workspace_id": workspace_id,
            "aligned": false,
            "expected": expected.chars().take(12).collect::<String>(),
            "actual": actual.chars().take(12).collect::<String>(),
            "reason": reason,
        }),
    };
    let _ = app.emit("workspace:cli-alignment", payload);
}

/// issue #4: emits the per-pane agent turn state for the chrome Ticker.
/// `started_at_ms` = None clears the live timer (turn ended); `avg_ms` = None
/// means no completed turns yet. The frontend ticks `M:SS` locally from
/// `started_at`, so this fires only on turn start / end — never per second.
///
/// Phase 84.B added `state` / `state_since` / `seq` for the traffic
/// light. The four original keys are unchanged on purpose: the Ticker
/// reads them as-is, and a frontend that predates this degrades to the
/// old behaviour instead of breaking.
pub(crate) fn emit_agent_run_event(
    app: &AppHandle,
    pane_id: &str,
    started_at_ms: Option<u128>,
    avg_ms: Option<u128>,
    state: PaneAgentState,
    state_since_ms: Option<u128>,
    seq: u32,
) {
    let _ = app.emit(
        "pane:agent-run",
        serde_json::json!({
            "pane_id": pane_id,
            "started_at": started_at_ms,
            "avg_ms": avg_ms,
            "running": started_at_ms.is_some(),
            "state": state.as_str(),
            "state_since": state_since_ms,
            "seq": seq,
        }),
    );
}

/// Phase 84.B: one pane's agent state, for the hydration command below.
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct PaneAgentSnapshot {
    pub(crate) state: String,
    // ts-rs maps u128 to `bigint`, but the Tauri event carries these same
    // values as plain JSON numbers and the frontend compares the two. Pin
    // them to `number` — epoch-ms is nowhere near f64's integer limit.
    // Same idiom `FeedItem::created_ms` already uses.
    #[ts(type = "number | null")]
    pub(crate) state_since: Option<u128>,
    #[ts(type = "number | null")]
    pub(crate) started_at: Option<u128>,
    #[ts(type = "number | null")]
    pub(crate) avg_ms: Option<u128>,
    pub(crate) seq: u32,
}

/// Phase 84.B: every pane's agent state at once.
///
/// Exists for the webview reload (F5, devtools reload, an HMR round in
/// dev) — far more common than an app restart, and without this every
/// light goes dark until the next hook happens to fire, which for an idle
/// agent could be never. Deliberately NOT persisted to disk: restoring an
/// eight-hour-old "running" after an app restart would be a lie, and the
/// first hook restores the truth anyway.
#[tauri::command]
fn pane_agent_states(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, PaneAgentSnapshot>, String> {
    let runs = state
        .agent_runs
        .lock()
        .map_err(|e| format!("agent_runs lock poisoned: {e}"))?;
    Ok(runs
        .iter()
        .map(|(pane_id, r)| {
            (
                pane_id.clone(),
                PaneAgentSnapshot {
                    state: r.state.as_str().to_string(),
                    state_since: r.state_since_ms(),
                    started_at: r.started_at_ms(),
                    avg_ms: r.avg_ms(),
                    seq: r.seq,
                },
            )
        })
        .collect())
}

/// Spawns a tokio task that clears a pane's status text after `secs` seconds.
pub(crate) fn schedule_status_clear(app: AppHandle, pane_id: String, secs: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        emit_pane_status_event(&app, &pane_id, "");
    });
}

fn emit_exit(app: &AppHandle, session_id: &str, reason: Option<String>) {
    // Phase 65 (bug FF): log every PTY/SSH exit + reason so a spurious
    // disconnect (e.g. "Claude quit closed the connection") is traceable
    // in debug.log — pair this with the ssh-disconnect Eof/Close/transport
    // lines to see WHY the channel ended (bash exit vs network vs RPC).
    log_info("PTY", &format!(
        "pty:exit session={session_id} reason={}",
        reason.as_deref().unwrap_or("(none)")
    ));
    let _ = app.emit(
        "pty:exit",
        PtyExitEvent {
            session_id: session_id.to_string(),
            reason,
        },
    );
}

fn cleanup_session_maps(
    sessions: &SessionMap,
    pane_sessions: &PaneSessionMap,
    pane_id: &str,
    session_id: &str,
) {
    let _ = sessions.lock().unwrap().remove(session_id);
    let mut p = pane_sessions.lock().unwrap();
    if p.get(pane_id).map(|s| s.as_str()) == Some(session_id) {
        p.remove(pane_id);
    }
}

// ─── Local PTY spawn ─────────────────────────────────────────────────────────

/// The two bundled files zellij needs, resolved together.
///
/// `config_dir` is exported as `ZELLIJ_CONFIG_DIR`, which is what makes
/// `default_layout "ymux"` in the kdl resolve: zellij's layout dir defaults to
/// a `layouts/` subdirectory of the config dir. Verified with
/// `zellij setup --check` on 0.44.3 (2026-08-20) — it also leaves DATA DIR and
/// PLUGIN DIR under `%APPDATA%\Zellij`, so this moves the layout lookup and
/// nothing else.
pub(crate) struct ZellijResources {
    pub(crate) config_file: std::path::PathBuf,
    pub(crate) config_dir: std::path::PathBuf,
}

/// Pick the first root that holds BOTH `ymux-zellij.kdl` and
/// `layouts/ymux.kdl`.
///
/// **All-or-nothing on purpose.** The moment the config says
/// `default_layout "ymux"`, it is a promise the layouts dir has to keep — and
/// zellij is not loud about an unknown layout name. Accepting a directory with
/// only the config would mean every new session starts with a `default_layout`
/// that cannot resolve. Refusing both degrades to the state that is already
/// supported and already logged: zellij uses the user's own config, the pane
/// works, and the chrome comes back.
///
/// Split out of `resolve_zellij_config` so it can be tested without an
/// `AppHandle` — it is the only branching logic here.
pub(crate) fn pick_zellij_resources(roots: &[std::path::PathBuf]) -> Option<ZellijResources> {
    for root in roots {
        for dir in [root.join("resources"), root.clone()] {
            let config_file = dir.join("ymux-zellij.kdl");
            let layout = dir.join("layouts").join("ymux.kdl");
            if config_file.is_file() && layout.is_file() {
                return Some(ZellijResources {
                    config_file,
                    config_dir: dir,
                });
            }
        }
    }
    None
}

/// Resolve the bundled zellij resources. Same shape as
/// `local_setup::resolve_ymux_cli` — installed builds carry them under the
/// Tauri resource dir, and a dev build finds them beside the binary.
///
/// `None` is a supported state, not a failure: without them zellij just uses
/// its own defaults and the pane still works, only with the chrome and the
/// keybinds back.
fn resolve_zellij_config(app: &AppHandle) -> Option<ZellijResources> {
    use tauri::Manager;
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(r) = app.path().resource_dir() {
        roots.push(r);
    }
    // 2026-08-19: also look beside the RUNNING BINARY.
    //
    // INERT ON WINDOWS — tauri's `resource_dir()` already returns the exe's
    // directory there, so this repeats the same two probes. Kept anyway: this
    // file carries no `cfg(target_os)` gates and `build-macos-intel.yml`
    // compiles it, and in a macOS bundle `resource_dir()` is
    // `Contents/Resources` while the exe lives in `Contents/MacOS`. Deleting
    // it would be a silent behaviour change on the one platform nobody here
    // tests, to save four stat calls.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
        }
    }
    pick_zellij_resources(&roots)
}

/// `persist_session`: when `Some`, the pane is PERSISTENT and its shell is
/// wrapped in a multiplexer ~900ms after spawn. The backend is the
/// platform's: `zellij attach -c <name>` on Windows, `tmux new-session -A -s
/// <name>` everywhere else (the same shape the WSL and SSH paths use).
/// `None` keeps the historical plain-shell behaviour.
fn spawn_local_pty(
    state: &AppState,
    pane_id: String,
    app: &AppHandle,
    shell: Option<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    // 2026-08-19 / Phase 82: `Some(name)` makes this a persistent pane - the
    // shell is spawned exactly as before and the attach line is typed into it
    // afterwards. zellij on Windows, tmux elsewhere; see the doc comment.
    persist_session: Option<String>,
) -> Result<String, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let shell_cmd = pick_default_shell(shell);
    #[cfg(windows)]
    let mut cmd = CommandBuilder::new(&shell_cmd);
    #[cfg(not(windows))]
    let mut cmd = {
        let (program, args) = split_shell_command(&shell_cmd);
        let mut c = CommandBuilder::new(&program);
        for a in &args {
            c.arg(a);
        }
        // macOS port: Terminal.app / iTerm spawn the shell as a LOGIN
        // shell, and that is where the PATH lives on a Mac (Homebrew's
        // `eval "$(brew shellenv)"` sits in ~/.zprofile, not ~/.zshrc).
        // Without `-l` a fresh pane can't find brew/node/claude. Only
        // added when the user didn't pass their own args — a typed
        // "zsh -c ..." / "bash --norc" is respected verbatim.
        if args.is_empty() && matches!(detect_shell_kind(&program), ShellKind::Posix) {
            c.arg("-l");
        }
        c
    };
    for a in utf8_shell_args(detect_shell_kind(&shell_cmd)) {
        cmd.arg(a);
    }
    if let Some(d) = cwd.as_deref() {
        if Path::new(d).is_dir() {
            cmd.cwd(d);
        }
    }
    // Phase 65 (J experiment): nudge CLIs (Claude Code) to emit OSC 8
    // hyperlinks so the terminal's linkHandler can make file links
    // clickable. CommandBuilder inherits the parent env, so these only
    // add/override. Engineer-only log (Rule #9). If this doesn't make
    // Claude emit OSC 8, J falls back to plan B (regex on visible text).
    cmd.env("FORCE_HYPERLINK", "1");
    cmd.env("FORCE_HYPERLINKS", "1");
    cmd.env("CLAUDE_CODE_FORCE_HYPERLINKS", "1");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM", "xterm-256color");
    // macOS port: a Finder-launched app inherits NO locale (Terminal.app
    // sets LANG itself from the system locale). Without a UTF-8 LC_CTYPE
    // zsh's line editor and tmux treat every non-ASCII byte as
    // unprintable — Hebrew keystrokes rendered as `_`. Only fill the gap;
    // a user-set LANG/LC_ALL/LC_CTYPE (launched from a shell) wins.
    #[cfg(not(windows))]
    if ["LANG", "LC_ALL", "LC_CTYPE"]
        .iter()
        .all(|k| std::env::var_os(k).map(|v| v.is_empty()).unwrap_or(true))
    {
        cmd.env("LANG", default_utf8_locale());
    }
    // ymux-tools (issue #4): tag the local pane's shell so claude-hook
    // invocations (pre-tool-use gating, the chrome Ticker's user-prompt-submit
    // / stop) pass the CLI's env-gate. Remote panes get this via the ssh
    // channel / tmux set-environment; local panes had no equivalent, so every
    // ymux-cli hook silently env-gated (main.rs `YMUX_PANE_ID unset`) and
    // the Ticker never fired outside manual injection.
    cmd.env("YMUX_PANE_ID", &pane_id);
    // winmux → ymux rename bridge: a `ymux-cli.exe` still on PATH from a
    // pre-rename install reads the old spelling and would otherwise
    // env-gate itself out. Drop once 0.5.0 is the floor.
    cmd.env("WINMUX_PANE_ID", &pane_id);
    // 2026-08-19: point zellij at ymux's own config; 2026-08-20: and at its
    // config DIRECTORY, which is how the single-pane layout gets found.
    //
    // ENV VARS rather than `zellij --config <path>` in the line we type into
    // the shell: the resource path contains spaces, that line has to parse in
    // both cmd.exe and PowerShell, and Rule #3 says do not assemble shell
    // strings. Both `--config` and `--config-dir` document their env var in
    // `--help`, so these are the same knobs without any quoting — and there is
    // no flag for the layout at all, since `zellij attach` takes no `--layout`.
    //
    // ZELLIJ_CONFIG_DIR moves the LAYOUT lookup to `<dir>\layouts` and nothing
    // else: DATA DIR and PLUGIN DIR stay under %APPDATA%\Zellij (checked with
    // `zellij setup --check`). It also becomes the base for `theme_dir`, which
    // is harmless — we ship no themes and set none.
    //
    // Skipped silently when the resources are missing — zellij falls back to
    // its own config and the pane works, just with the chrome and the keybinds.
    match resolve_zellij_config(app) {
        Some(z) => {
            cmd.env("ZELLIJ_CONFIG_FILE", &z.config_file);
            cmd.env("ZELLIJ_CONFIG_DIR", &z.config_dir);
            // Program paths, not user content — safe under Rule #1, and the
            // only way to tell "the lock does not work" apart from "the config
            // was never found". One grep for `zellij config:` answers both
            // halves, so the layout dir is named here too.
            log_info(
                "PTY",
                &format!(
                    "zellij config: {} (+ layouts/ymux.kdl)",
                    z.config_file.display()
                ),
            );
        }
        None => log_warn(
            "PTY",
            "zellij config: ymux-zellij.kdl + layouts/ymux.kdl NOT FOUND — \
             zellij keeps its own chrome, keybinds and mouse capture",
        ),
    }
    tracing::debug!("spawn_local_pty[{pane_id}]: injected hyperlink + YMUX_PANE_ID env vars");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn {shell_cmd} failed: {e}"))?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone_reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take_writer failed: {e}"))?;

    let id = next_session_id();
    let id_for_thread = id.clone();
    let pane_for_thread = pane_id.clone();
    let app_for_thread = app.clone();
    let sessions_for_thread = state.core.sessions.clone();
    let pane_sessions_for_thread = state.core.pane_sessions.clone();
    let bidi_for_thread = state.bidi_filters.clone();
    thread::spawn(move || {
        let mut stream = pty_decode::Utf8Stream::new();
        let mut osc = osc_notify::OscNotifyParser::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => emit_data(
                    &app_for_thread,
                    &id_for_thread,
                    &buf[..n],
                    &mut stream,
                    &pane_for_thread,
                    &mut osc,
                    &bidi_for_thread,
                ),
                Err(_) => break,
            }
        }
        let _ = child.wait();
        cleanup_session_maps(
            &sessions_for_thread,
            &pane_sessions_for_thread,
            &pane_for_thread,
            &id_for_thread,
        );
        emit_exit(&app_for_thread, &id_for_thread, None);
    });

    state.core.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Local(LocalSession {
            writer,
            master: pair.master,
            killer,
            // Native-local panes carry a multiplexer session name too - the
            // field is named for tmux but holds the zellij name on Windows.
            // This is what lights up `pane_persistence_list`, and with it the
            // persistence badge / Detach / Kill-session UI.
            tmux_session: persist_session.clone(),
            wsl_distro: None,
        }),
    );

    #[cfg(windows)]
    {
        // Persistent mode: type the attach-or-create line into the shell 900ms
        // after spawn — the same timing the WSL path uses, chosen to land after
        // `schedule_setup_injection` fires at 500ms so the user's env exports and
        // setup_command have settled first. Deliberately NOT spawning zellij as
        // the pane process: `pick_default_shell` + `utf8_shell_args` set up
        // `chcp 65001` for PowerShell, and losing that UTF-8 handling would put
        // this session's Hebrew work straight back on the floor.
        if let Some(name) = persist_session {
            let sessions_clone = state.core.sessions.clone();
            let id_clone = id.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                let Some(line) = build_zellij_attach_command(&name) else {
                    // The name derives from a user pane title, so it is
                    // logged by LENGTH and never by value (Rule #1 in
                    // spirit: user content stays out of the log).
                    log_warn(
                        "PTY",
                        &format!(
                            "zellij attach skipped for pane={id_clone}: session name is not shell-safe ({} chars)",
                            name.chars().count()
                        ),
                    );
                    return;
                };
                let mut sessions = sessions_clone.lock().unwrap();
                if let Some(Session::Local(l)) = sessions.get_mut(&id_clone) {
                    use std::io::Write as _;
                    // Rule #1: a fixed control string built from a sanitized
                    // session name — never PTY content.
                    let _ = l.writer.write_all(line.as_bytes());
                    let _ = l.writer.flush();
                }
            });
        }
    }
    // macOS port: persistent LOCAL panes — type the tmux attach-or-create
    // script into the shell 900ms after spawn, exactly like the WSL/SSH
    // paths (after env exports + setup_command at 500ms, before the
    // smart-connect command at 1100ms). The shell is a login shell (`-l`
    // above), so Homebrew's PATH is present and `command -v tmux`
    // resolves. No WINMUX_* env injection (empty socket_addr): the local
    // RPC bridge for hooks is not wired for mac panes yet, and
    // build_tmux_attach_script skips the exports entirely when the
    // address is empty. If tmux is missing the fallback echo leaves a
    // plain shell.
    #[cfg(not(windows))]
    {
        if let Some(name) = persist_session {
            // Only pass `-f ~/.ymux/tmux.conf` when the file actually exists
            // locally — otherwise tmux prints a "No such file" cause into the
            // pane on every attach. The local setup wizard writes it; until
            // then the user's own ~/.tmux.conf applies.
            let conf_present = local_setup::local_tmux_conf_ready();
            let use_ymux_tmux_conf = state
                .settings
                .lock()
                .ok()
                .map(|s| s.terminal.use_ymux_tmux_config)
                .unwrap_or(true)
                && conf_present;
            let sessions_clone = state.core.sessions.clone();
            let id_clone = id.clone();
            let pane_for_exec = pane_id.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                crate::log_info("PTY", &format!(
                    "tmux(local): new-session -A -s '{}' (pane {}, {} ymux conf)",
                    name,
                    pane_for_exec,
                    if use_ymux_tmux_conf { "with" } else { "without" }
                ));
                let script = build_tmux_attach_script(
                    &name,
                    "",
                    "",
                    &pane_for_exec,
                    use_ymux_tmux_conf,
                    "[ymux] tmux not installed — falling back to plain shell",
                );
                let mut sessions = sessions_clone.lock().unwrap();
                if let Some(Session::Local(l)) = sessions.get_mut(&id_clone) {
                    use std::io::Write as _;
                    let _ = l.writer.write_all(script.as_bytes());
                    let _ = l.writer.flush();
                }
            });
        }
    }
    Ok(id)
}

/// Phase 80: the tmux attach-or-create script typed into a fresh shell
/// ~900ms after spawn (after env exports + setup_command have settled).
/// Shared by persistent SSH panes (sent over the SSH channel) and WSL
/// panes (written straight to the local PTY). An empty `socket_addr`
/// skips the YMUX_* env injection into tmux's global environment
/// (WSL panes pass empty until the WSL RPC bridge hands them a real
/// address). `fallback_msg` must not contain single quotes.
/// 2026-08-19: the Zellij attach-or-create line typed into a native Windows
/// shell, the local counterpart of `build_tmux_attach_script`.
///
/// Deliberately a SEPARATE function rather than an arm inside that one: it is
/// pure POSIX (`command -v`, `exec`, `$HOME`, `\r\n` into a shell) and
/// load-bearing for SSH and WSL both, and Phase 65 records what a careless
/// edit there costs. None of it parses in cmd.exe or PowerShell anyway.
///
/// `zellij attach -c` is attach-or-create, the same semantics as tmux's
/// `new-session -A -s`. There is no `exec` on Windows, so the shell stays as
/// the parent process — when zellij exits the user lands back in a working
/// shell rather than a closed pane, which is the nicer failure mode.
///
/// **No guard around the command on purpose.** If zellij is missing, cmd and
/// PowerShell each print their own "not recognized" and leave a perfectly
/// usable plain shell. Writing a cross-shell existence check would mean two
/// dialects of quoting for a worse fallback than the one we get free.
/// How ymux invokes zellij from Rust (listing / killing sessions), as opposed
/// to the bare `zellij` word typed into the user's shell.
///
/// The winget MSI puts the binary at `%LOCALAPPDATA%\Zellij\zellij.exe` and
/// adds that directory to the USER PATH — but a PATH edit only reaches
/// processes started afterwards, so a ymux that was already running when
/// zellij got installed would not find it by name. Prefer the canonical path
/// when it exists and fall back to PATH, which also covers a scoop/cargo/
/// hand-placed install.
fn zellij_exe() -> String {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let p = std::path::Path::new(&local).join("Zellij").join("zellij.exe");
        if p.is_file() {
            return p.to_string_lossy().into_owned();
        }
    }
    "zellij".to_string()
}

/// Every zellij verb ymux sends, in one block.
///
/// Two kinds of caller, and the difference matters:
///   - `build_zellij_attach_command` is TYPED INTO THE USER'S SHELL, so it is
///     a string and has to be valid in both cmd.exe and PowerShell.
///   - everything else runs as a child process through `zellij_run`, argv
///     array only, never a shell (Rule #3).
///
/// The verbs, and why each one is here (checked against `zellij 0.44.3
/// --help` on 2026-08-19 rather than from memory):
///   - `attach -c <name>`      attach-or-create; also resurrects an EXITED
///                             session, which is the reattach path after a
///                             reboot.
///   - `list-sessions -n`      `--no-formatting`, which upstream documents as
///                             the parsing form. `-s` (bare names) drops the
///                             age and the EXITED marker, so it is not used.
///   - `delete-session -f <n>` destroy a session AND its serialized copy.
///                             `-f` is "kill it first if it is running", so
///                             one verb covers both the live and the
///                             already-exited case. See below for why this
///                             replaced a two-verb sequence.
///
/// Deliberately NOT sent:
///   - `kill-session <name>`. It stops a running session but LEAVES the
///     serialized copy, so the session reappears in the list marked EXITED
///     and `attach` still resurrects it. ymux sent it until 2026-08-20,
///     falling through to `delete-session` only when the kill FAILED — the
///     idea being that a live session is never destroyed by one click, and a
///     failure between the two verbs leaves something resurrectable rather
///     than nothing. Sound design, unreachable in practice: the first click
///     also removes the pane from `pane_sessions`, so a second one returns
///     early having sent no verb at all, and the button is gated on
///     `isConnected` / `isTmux()` — both false by then, so it is not even
///     rendered. `zellij_args_delete` had one call site that nothing could
///     reach. A safety valve nobody can operate is not a safety valve; it is
///     a Kill button that does not kill. One forcing verb plus an honest
///     outcome report (`KillSessionOutcome`) is the replacement.
///   - `attach -b` (create detached). It returns 0 and creates nothing when
///     invoked without a tty, so ymux would have no way to tell success from
///     silence. Panes create their session by attaching instead.
///   - `action rename-session`. ymux derives the session name from the pane
///     id so a cold start can find it again; renaming would break exactly
///     that lookup. Pane labels are stored app-side and never sent to zellij.
///   - `kill-all-sessions` / `delete-all-sessions`. Nothing in the UI means
///     "every session on this machine", including ones ymux did not create.
fn zellij_args_list() -> Vec<String> {
    vec!["list-sessions".into(), "-n".into()]
}

/// Destroy `name` and its serialized copy.
///
/// `-f` goes BEFORE the name, matching the 0.44.3 synopsis
/// `delete-session [OPTIONS] <TARGET_SESSION>`. Swapping them would make clap
/// read `-f` as the target session — a silent wrong-target destroy, which is
/// the worst thing this path can do, so the order has its own test.
fn zellij_args_delete_force(name: &str) -> Vec<String> {
    vec!["delete-session".into(), "-f".into(), name.to_string()]
}

/// Type `chars` into a named session, as if the user had typed them.
///
/// `-s <name>` is a ROOT option, so it comes before the subcommand — the same
/// targeting that made a rename attempt list the active sessions by name.
/// This is how the connect wizard's command reaches the shell INSIDE zellij
/// instead of the shell zellij was launched from; see `pane_connect`.
///
/// **PRECONDITION: exactly one pane per session.** No `--pane-id` is sent, so
/// this lands in the FOCUSED pane — which is correct only because
/// `resources/layouts/ymux.kdl` gives the session a single pane and the
/// cleared keybinds make a split unreachable. If that lock is ever relaxed,
/// this is where the wizard's command starts landing in the wrong shell.
/// Adding `--pane-id` is not a free hedge: the id would have to be discovered
/// with `list-panes --json` first, inside the already-tight attach window, and
/// a STALE id is worse than none — it fails silently instead of landing.
fn zellij_args_write_chars(session: &str, chars: &str) -> Vec<String> {
    vec![
        "-s".into(),
        session.to_string(),
        "action".into(),
        "write-chars".into(),
        chars.to_string(),
    ]
}

/// Phase 87: the viewport of a named session, on stdout (docs/ZELLIJ.md §4).
/// Same root-option targeting as `zellij_args_write_chars`. No `--full`: the
/// active-sessions overview wants what is on screen now, not the scrollback,
/// and no `--ansi`: the text goes to a model, not a terminal.
fn zellij_args_dump_screen(session: &str) -> Vec<String> {
    vec![
        "-s".into(),
        session.to_string(),
        "action".into(),
        "dump-screen".into(),
    ]
}

/// What actually happened when a zellij verb ran.
///
/// The distinction that matters is `Failed` vs `Missing`. `zellij_run` used to
/// return `false` for both, which is why a Kill on a machine with no zellij
/// installed looked exactly like a successful one from the UI: every verb
/// "failed", the log said nothing useful, and the frontend reported success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ZellijOutcome {
    Ok,
    /// Ran and exited non-zero. `code` is `None` if it died on a signal.
    Failed { code: Option<i32>, stderr: String },
    /// `zellij_exe()` did not resolve to anything runnable.
    Missing,
}

/// How much of zellij's stderr is worth keeping. It is zellij's own
/// diagnostic, never PTY content (Rule #1), but it does not need to be
/// unbounded to be useful — the messages that matter are one line.
const ZELLIJ_STDERR_CAP: usize = 200;

/// Classify a spawn error. Split out because it is the only part of the
/// missing-vs-failed distinction that a unit test can reach without a binary.
fn zellij_spawn_error_outcome(kind: std::io::ErrorKind, msg: &str) -> ZellijOutcome {
    if kind == std::io::ErrorKind::NotFound {
        ZellijOutcome::Missing
    } else {
        ZellijOutcome::Failed {
            code: None,
            stderr: msg.to_string(),
        }
    }
}

/// Run one zellij verb to completion and report what happened.
///
/// `hidden_cmd` gives CREATE_NO_WINDOW + piped stdio so a GUI parent never
/// flashes a console (local_setup.rs). A missing binary is not an error:
/// zellij being absent is a supported state everywhere this is called — but it
/// is now a distinguishable one.
async fn zellij_try(args: &[String], what: &str) -> ZellijOutcome {
    let mut c = local_setup::hidden_cmd(&zellij_exe());
    for a in args {
        c.arg(a);
    }
    match c.output().await {
        Ok(out) if out.status.success() => ZellijOutcome::Ok,
        Ok(out) => {
            let mut stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.chars().count() > ZELLIJ_STDERR_CAP {
                stderr = stderr.chars().take(ZELLIJ_STDERR_CAP).collect();
            }
            log_debug(
                "PTY",
                &format!(
                    "zellij {what}: exited {:?}: {stderr}",
                    out.status.code()
                ),
            );
            ZellijOutcome::Failed {
                code: out.status.code(),
                stderr,
            }
        }
        Err(e) => {
            let o = zellij_spawn_error_outcome(e.kind(), &e.to_string());
            match &o {
                ZellijOutcome::Missing => log_warn(
                    "PTY",
                    &format!("zellij {what}: zellij is not installed ({e})"),
                ),
                _ => log_warn("PTY", &format!("zellij {what}: spawn failed: {e}")),
            }
            o
        }
    }
}

/// Thin bool wrapper, kept so callers that only care whether the verb landed
/// stay byte-identical — notably the smart-connect `write-chars` injection,
/// which is a path worth not disturbing.
async fn zellij_run(args: &[String], what: &str) -> bool {
    matches!(zellij_try(args, what).await, ZellijOutcome::Ok)
}

/// Note for anyone tempted to add a flag here: **there is nowhere to put one.**
/// `zellij attach` has no `--layout`, and the root `-l` conflicts with
/// `attach`. Every setting reaches the session through the config file that
/// `spawn_local_pty` points `ZELLIJ_CONFIG_FILE` / `ZELLIJ_CONFIG_DIR` at.
///
/// Returns `None` for a name that is not safe to type unquoted (see
/// `session_name_char_is_safe`). This is the last gate, not the first:
/// a name derived from a pane title is already safe by construction, so
/// a name that fails here came from the picker — an EXISTING session
/// created outside ymux, e.g. `zellij -s 'a;calc'` typed by hand.
///
/// Refusing beats mangling. Mangling would attach to, or create, a
/// session under a name that is not the one the user picked, silently.
/// Refusing leaves the pane in a plain working shell — the same failure
/// mode this function already accepts when zellij is not installed.
fn build_zellij_attach_command(name: &str) -> Option<String> {
    if name.is_empty() || !name.chars().all(session_name_char_is_safe) {
        return None;
    }
    Some(format!("zellij attach -c {name}\r\n"))
}

/// Parse `zellij list-sessions -n` (`--no-formatting`, which upstream
/// documents as "useful for parsing").
///
/// Real output captured 2026-08-19 from zellij 0.44.3 on Windows:
/// ```text
/// spike [Created 12m 30s ago]
/// old-one [Created 3h 4m 1s ago] (EXITED - attach to resurrect)
/// current-one [Created 5s ago] (current)
/// ```
/// Written as its own parser rather than a branch in `parse_tmux_sessions`,
/// which decodes a completely different `<<<YMUX_META>>>` record joined
/// against `session-meta.json`.
///
/// Zellij lists EXITED sessions too — they are resurrectable, which is the
/// one thing it does that tmux cannot (they survive a reboot). They are
/// returned here, marked `attached: false`, so the picker can offer them.
fn parse_zellij_sessions(text: &str) -> Vec<TmuxSessionInfo> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut out: Vec<TmuxSessionInfo> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("No active zellij sessions") {
            continue;
        }
        let name = match line.split_whitespace().next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        // `[Created <dur> ago]` → seconds. Absent or unparseable simply means
        // "unknown age": fall back to `now` rather than dropping the session,
        // since a session we cannot date is still one the user can attach to.
        let age = line
            .split_once("[Created ")
            .and_then(|(_, rest)| rest.split_once(" ago]"))
            .map(|(dur, _)| parse_zellij_duration(dur))
            .unwrap_or(0);
        out.push(TmuxSessionInfo {
            name,
            created: now - age,
            // Zellij marks the session the CALLING client is inside as
            // "(current)". ymux always asks from outside a session, so this
            // is never us; treat it as "someone is attached".
            attached: line.contains("(current)"),
            // Zellij has no window count in this output. 1 is the honest
            // floor — a session always has at least one pane — and the
            // picker renders it as a hint, not a fact it acts on.
            windows: 1,
            last_attached: 0,
            // Zellij keeps a serialized copy of a session after its shell
            // exits and will rebuild it on the next attach. tmux has no such
            // state, so this is false on every tmux path.
            exited: line.contains("(EXITED"),
            label: None,
            claude_title: None,
            // Phase 81.F: zellij sessions carry no session-meta join yet —
            // the picker falls back to `name`, same as a pre-rename server.
            auto_name: None,
            claude_session_id: None,
            origin: None,
            // 2026-08-23: `zellij list-sessions -n` reports name, age and the
            // EXITED/current markers — no working directory in any form. This
            // is precisely why workspace scope cannot be cwd-only: on Windows
            // the ONLY signal is `owned`, from session-owners.json.
            cwd: None,
            owned: false,
            in_cwd: false,
            foreign: None,
            owner_cwd: None,
        });
    }
    // Newest first, matching parse_tmux_sessions' ordering contract.
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// `12m 30s` / `3h 4m 1s` / `5s` / `2days 1h` → seconds. Unknown units are
/// skipped rather than poisoning the whole duration, so a future zellij
/// spelling degrades to a slightly wrong age instead of a lost session.
fn parse_zellij_duration(s: &str) -> i64 {
    let mut total: i64 = 0;
    for tok in s.split_whitespace() {
        let digits: String = tok.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let n: i64 = match digits.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let unit = &tok[digits.len()..];
        total += match unit {
            u if u.starts_with('s') => n,
            u if u.starts_with("ms") => 0,
            u if u.starts_with('m') => n * 60,
            u if u.starts_with('h') => n * 3600,
            u if u.starts_with('d') => n * 86_400,
            _ => 0,
        };
    }
    total
}

fn build_tmux_attach_script(
    name: &str,
    socket_addr: &str,
    token: &str,
    pane_id: &str,
    use_ymux_tmux_conf: bool,
    fallback_msg: &str,
) -> String {
    let mut script = String::new();
    // Push the env vars into tmux's global environment so a re-attach to
    // a long-lived session sees the *current* YMUX_SOCKET_ADDR/
    // TUNNEL_TOKEN/PANE_ID rather than the stale ones from the original
    // creation. The `2>/dev/null` swallows the harmless "no server
    // running" message when this is the first attach.
    if !socket_addr.is_empty() {
        // Both spellings: `YMUX_*` for the current CLI, `WINMUX_*` for a
        // pre-rename `ymux-linux-x64` that a remote may still be running
        // until the next bootstrap re-uploads it. The new CLI promotes
        // WINMUX_* → YMUX_* at startup, so the pair is safe in either
        // direction. Drop the legacy triple once 0.5.0 is the floor.
        for (var, value) in [
            ("YMUX_SOCKET_ADDR", socket_addr),
            ("WINMUX_SOCKET_ADDR", socket_addr),
            ("YMUX_TUNNEL_TOKEN", token),
            ("WINMUX_TUNNEL_TOKEN", token),
            ("YMUX_PANE_ID", pane_id),
            ("WINMUX_PANE_ID", pane_id),
        ] {
            script.push_str(&format!(
                "tmux set-environment -g {} {} 2>/dev/null; ",
                var,
                shell_quote(value)
            ));
        }
    }
    // Phase tmux-conf: when enabled, point tmux at our bundled conf via
    // `-f ~/.ymux/tmux.conf`. Falls through to the user's own
    // ~/.tmux.conf if the file is absent (tmux logs a warning and uses
    // defaults — non-fatal). When the setting is off, omit -f so the
    // user's conf alone applies.
    let tmux_flags = if use_ymux_tmux_conf {
        "-f $HOME/.ymux/tmux.conf "
    } else {
        ""
    };
    // Phase 65 (bug EE): the bundled conf ships `mouse off` (the
    // known-good display config — `mouse on` in the conf garbled Claude
    // Code's live output). We still want tmux-native wheel scrollback,
    // so turn mouse on via the new-session command chain (`\; set -g
    // mouse on`) instead of in the conf.
    script.push_str(&format!(
        "command -v tmux >/dev/null 2>&1 && exec tmux {flags}new-session -A -s {name} \\; set -g mouse on || echo '{msg}'\r\n",
        flags = tmux_flags,
        name = shell_quote(name),
        msg = fallback_msg
    ));
    script
}

// ─── Phase 80: WSL PTY spawn ─────────────────────────────────────────────────

/// Spawn a pane inside a WSL distro via `wsl.exe`, optionally wrapped in
/// tmux for persistence — the exact mechanism persistent SSH panes use,
/// transported over wsl.exe instead of an SSH channel. The distro's
/// default (login) shell is used; `--cd` accepts a Windows path and
/// translates it to /mnt/... inside.
fn spawn_wsl_pty(
    state: &AppState,
    pane_id: String,
    app: &AppHandle,
    distro: Option<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    tmux_name: Option<String>,
) -> Result<String, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    // Rule #3: argv arrays only — no string-built command lines.
    let mut cmd = CommandBuilder::new("wsl.exe");
    if let Some(d) = distro.as_deref() {
        if !d.is_empty() {
            cmd.arg("-d");
            cmd.arg(d);
        }
    }
    if let Some(d) = cwd.as_deref() {
        if Path::new(d).is_dir() {
            cmd.arg("--cd");
            cmd.arg(d);
        }
    }
    // Same hyperlink/TERM nudges as spawn_local_pty — WSLENV is not
    // needed for these: wsl.exe forwards the env of the PTY child it
    // spawns only for WSLENV-listed vars, but TERM/COLORTERM are set by
    // the login shell inside anyway and the FORCE_HYPERLINK* vars ride
    // through WSLENV below.
    cmd.env("FORCE_HYPERLINK", "1");
    cmd.env("FORCE_HYPERLINKS", "1");
    cmd.env("CLAUDE_CODE_FORCE_HYPERLINKS", "1");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM", "xterm-256color");
    cmd.env(
        "WSLENV",
        "FORCE_HYPERLINK:FORCE_HYPERLINKS:CLAUDE_CODE_FORCE_HYPERLINKS:COLORTERM",
    );
    tracing::debug!("spawn_wsl_pty[{pane_id}]: distro={distro:?} tmux={tmux_name:?}");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn wsl.exe failed: {e}"))?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone_reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take_writer failed: {e}"))?;

    let id = next_session_id();
    let id_for_thread = id.clone();
    let pane_for_thread = pane_id.clone();
    let app_for_thread = app.clone();
    let sessions_for_thread = state.core.sessions.clone();
    let pane_sessions_for_thread = state.core.pane_sessions.clone();
    let bidi_for_thread = state.bidi_filters.clone();
    thread::spawn(move || {
        let mut stream = pty_decode::Utf8Stream::new();
        let mut osc = osc_notify::OscNotifyParser::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => emit_data(
                    &app_for_thread,
                    &id_for_thread,
                    &buf[..n],
                    &mut stream,
                    &pane_for_thread,
                    &mut osc,
                    &bidi_for_thread,
                ),
                Err(_) => break,
            }
        }
        let _ = child.wait();
        cleanup_session_maps(
            &sessions_for_thread,
            &pane_sessions_for_thread,
            &pane_for_thread,
            &id_for_thread,
        );
        emit_exit(&app_for_thread, &id_for_thread, None);
    });

    state.core.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Local(LocalSession {
            writer,
            master: pair.master,
            killer,
            tmux_session: tmux_name.clone(),
            wsl_distro: distro.clone(),
        }),
    );

    // Phase 80: persistent mode — type the tmux attach-or-create script
    // into the shell 900ms after spawn, exactly like the SSH path (after
    // env exports + setup_command). If tmux isn't installed in the
    // distro the fallback echo leaves a plain shell. The WSL RPC bridge
    // (TCP → named pipe, HMAC-gated) supplies the YMUX_* env the CLI
    // needs for hooks — the local twin of the SSH reverse tunnel.
    if let Some(name) = tmux_name {
        let use_ymux_tmux_conf = state
            .settings
            .lock()
            .ok()
            .map(|s| s.terminal.use_ymux_tmux_config)
            .unwrap_or(true);
        let sessions_clone = state.core.sessions.clone();
        let id_clone = id.clone();
        let pane_for_exec = pane_id.clone();
        let distro_for_task = distro.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
            // Best-effort bridge + env-file wiring; a failure just means
            // hooks stay silent for this pane (same posture as a remote
            // whose tunnel failed) — the terminal itself is unaffected.
            let mut socket_addr = String::new();
            let mut token = String::new();
            match local_setup::ensure_wsl_bridge().await {
                Ok((port, tok)) => {
                    if let Some(addr) =
                        local_setup::resolve_wsl_host_addr(distro_for_task.as_deref(), port).await
                    {
                        if let Err(e) = local_setup::write_wsl_env_file(
                            distro_for_task.as_deref(),
                            &addr,
                            &tok,
                            &pane_for_exec,
                        )
                        .await
                        {
                            crate::log_warn("RPC", &format!("wsl-bridge: env file write failed: {e}"));
                        }
                        socket_addr = addr;
                        token = tok.as_str().to_string();
                    } else {
                        crate::log_warn("RPC", "wsl-bridge: host address resolution failed — hooks disabled for this pane");
                    }
                }
                Err(e) => crate::log_warn("RPC", &format!("wsl-bridge: start failed: {e}")),
            }
            crate::log_info("PTY", &format!(
                "tmux(wsl): new-session -A -s '{}' (pane {}, {} ymux conf)",
                name,
                pane_for_exec,
                if use_ymux_tmux_conf { "with" } else { "without" }
            ));
            let script = build_tmux_attach_script(
                &name,
                &socket_addr,
                &token,
                &pane_for_exec,
                use_ymux_tmux_conf,
                "[ymux] tmux not installed in WSL — falling back to plain shell",
            );
            let mut sessions = sessions_clone.lock().unwrap();
            if let Some(Session::Local(l)) = sessions.get_mut(&id_clone) {
                use std::io::Write as _;
                let _ = l.writer.write_all(script.as_bytes());
                let _ = l.writer.flush();
            }
        });
    }
    Ok(id)
}

// Phase 51.B2: KnownHost + KnownHostsFile + load_known_hosts +
// save_known_hosts + iso_now + HostCheckOutcome + SshClient + impl
// Handler all moved to ymux-core. Only the symbols referenced from
// outside ymux-core (HostCheckOutcome default + SshClient itself)
// need re-exporting; the rest stay internal to the new crate.
pub(crate) use ymux_core::{HostCheckOutcome, SshClient};

// Phase 51.B2: SshClient + impl Handler moved to ymux-core
// (re-exported above). The construction sites below now pass a
// `bridge_spawner: Some(Arc::new(tunnel::spawn_bridge))` to plug the
// real tunnel impl into the russh handler without making ymux-core
// depend on tunnel.

// Phase 51.H: SSH auth primitives moved to ymux-ssh. Re-exported
// below so existing crate::pkwh / crate::pkwh_pub / crate::AuthMethod /
// crate::try_authenticate / crate::try_agent_auth / crate::key_load_needs_passphrase
// callsites resolve unchanged.
#[allow(unused_imports)]
pub(crate) use ymux_ssh::{
    key_load_needs_passphrase, pkwh, pkwh_pub, try_agent_auth, try_authenticate, AuthMethod,
};


// ─── Phase 32.B: SSH key offer + install ─────────────────────────────────

/// Path of the ymux-managed private key for a workspace.
fn ymux_key_path(workspace_id: &str) -> Result<PathBuf, String> {
    let mut p = config_dir()?;
    p.push("keys");
    std::fs::create_dir_all(&p).map_err(|e| format!("create {:?}: {e}", p))?;
    p.push(format!("{workspace_id}.key"));
    Ok(p)
}

/// True if the workspace already has a ymux-managed private key on
/// disk — we don't re-offer in that case.
fn ymux_managed_key_exists(workspace_id: &str) -> bool {
    ymux_key_path(workspace_id)
        .map(|p| p.exists())
        .unwrap_or(false)
}

#[tauri::command]
async fn ssh_key_offer_dismiss(
    state: State<'_, AppState>,
    app: AppHandle,
    dont_show_again: bool,
) -> Result<(), String> {
    if dont_show_again {
        {
            let mut s = state.settings.lock().map_err(|e| e.to_string())?;
            s.ssh_key_offer_disabled = true;
        }
        // Phase 59.E: was a direct std::fs::write — non-atomic
        // (violates Absolute Rule #7) and error-swallowing. Use the
        // settings module's tmp+rename save; failure is logged, not
        // raised (a failed "don't show again" persist shouldn't fail
        // the dismiss itself — worst case the offer pops once more).
        if let Ok(snapshot) = state.settings.lock().map(|s| s.clone()) {
            if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                log_warn("SSH", &format!("ssh_key_offer_dismiss: settings save failed: {e}"));
            }
        }
        let _ = app.emit("settings:changed", ());
    }
    Ok(())
}

// beta.3 (netfree, Track 1b): let the frontend cancel the reconnect toast.
// Frontend drives the retry loop in JS (backoff + attempt counter), but
// this command flips the server-side `reconnecting` flag on the *last*
// SshSession that matched the given pane so any subsequent reconnect-emit
// paths know the user opted out. Best-effort — pane may already be gone.
#[tauri::command]
async fn ssh_cancel_reconnect(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let sessions = state.core.sessions.lock().map_err(|e| e.to_string())?;
    // The pane→session map lookup can race with cleanup — treat "no session
    // found" as success (nothing to cancel). Rule #1: no pane content logged.
    let pane_sessions = state.core.pane_sessions.lock().map_err(|e| e.to_string())?;
    if let Some(sid) = pane_sessions.get(&pane_id) {
        if let Some(Session::Ssh(ssh)) = sessions.get(sid) {
            ssh.reconnecting.store(false, std::sync::atomic::Ordering::Relaxed);
            log_info("SSH", &format!("ssh_cancel_reconnect: pane={pane_id} flag cleared"));
        }
    }
    Ok(())
}

#[tauri::command]
async fn ssh_key_generate_and_install(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    ssh_user: String,
    ssh_host: String,
    ssh_port: u16,
    password: String,
    dont_show_again: bool,
) -> Result<String, String> {
    let _ = pane_id;
    let priv_path = ymux_key_path(&workspace_id)?;
    let pub_path: PathBuf = {
        let mut p = priv_path.clone();
        let mut s = p.file_name().unwrap().to_os_string();
        s.push(".pub");
        p.set_file_name(s);
        p
    };

    // 1) Generate ed25519 keypair via ssh-keygen.exe (ships with
    //    Windows 10+ OpenSSH). Same approach as the provisioning
    //    wizard's GenerateKeypair step.
    if priv_path.exists() {
        std::fs::remove_file(&priv_path).map_err(|e| format!("remove old key: {e}"))?;
    }
    if pub_path.exists() {
        std::fs::remove_file(&pub_path).map_err(|e| format!("remove old pubkey: {e}"))?;
    }
    let priv_str = priv_path.to_string_lossy().to_string();
    let out = tokio::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            &format!("ymux-{workspace_id}"),
            "-f",
            &priv_str,
        ])
        .output()
        .await
        .map_err(|e| format!("spawn ssh-keygen: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let pub_line =
        std::fs::read_to_string(&pub_path).map_err(|e| format!("read pubkey: {e}"))?;
    let pub_line_trim = pub_line.trim();

    // 2) Open a fresh SSH session using the password that just worked
    //    (the original handle isn't easily reusable here — opening a
    //    new short-lived one is simpler and the user already typed
    //    the password once for this flow).
    let target = format!("{ssh_host}:{ssh_port}");
    // Phase 38: keepalive (see spawn_ssh) — short-lived key-install
    // session, but keep it consistent with the rest.
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let mut handle = client::connect(
        config,
        (ssh_host.as_str(), ssh_port),
        SshClient::new_anonymous(target.clone()),
    )
    .await
    .map_err(|e| format!("ssh connect {target}: {e}"))?;
    let ok = handle
        .authenticate_password(&ssh_user, &password)
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    if !ok {
        return Err("authentication failed (password rejected)".into());
    }

    // 3) Append the public key to ~/.ssh/authorized_keys. No sudo —
    //    writes only to the user's own home, so this works even for
    //    a non-root user with no sudo at all.
    let install_cmd = format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && \
         touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && \
         (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys)",
        key = pub_line_trim.replace('\'', "'\\''"),
    );
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, install_cmd.as_str())
        .await
        .map_err(|e| e.to_string())?;
    let mut out_buf = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(russh::ChannelMsg::Data { data }) => out_buf.extend_from_slice(&data[..]),
            Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                out_buf.extend_from_slice(&data[..])
            }
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = exit_status as i32
            }
            Some(russh::ChannelMsg::Close)
            | Some(russh::ChannelMsg::Eof)
            | None => break,
            _ => {}
        }
    }
    if exit_code != 0 {
        let stderr = String::from_utf8_lossy(&out_buf).to_string();
        return Err(format!(
            "install pubkey failed (exit {exit_code}): {stderr}"
        ));
    }

    // 4) Update the workspace's stored Connection — switch from
    //    password to key. The next pane_connect will use the key path
    //    and skip the password prompt.
    {
        let mut file = state.workspaces.lock().map_err(|e| e.to_string())?;
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            // The Connection::Ssh variant has no `password` field —
            // passwords are transient (passed per-connect, not
            // persisted). Setting the key_path is all that's needed
            // so future pane_connect calls use the key.
            if let Some(Connection::Ssh { key_path: kp, .. }) = ws.connection.as_mut() {
                *kp = Some(priv_str.clone());
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());

    // 5) Persist "don't show again" if requested.
    if dont_show_again {
        {
            let mut s = state.settings.lock().map_err(|e| e.to_string())?;
            s.ssh_key_offer_disabled = true;
        }
        // Phase 59.E: atomic tmp+rename via the settings module
        // (was a direct std::fs::write — Rule #7 violation). Failure
        // logged, not raised: the key IS installed at this point and
        // the workspace persisted; a failed flag save just means the
        // offer may pop once more.
        if let Ok(snapshot) = state.settings.lock().map(|s| s.clone()) {
            if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                log_warn("SSH", &format!(
                    "ssh_key_generate_and_install: settings save failed: {e}"
                ));
            }
        }
        let _ = app.emit("settings:changed", ());
    }

    log_info("SSH", &format!(
        "ssh_key_generate_and_install: installed key for ws={workspace_id} user={ssh_user} host={ssh_host}"
    ));
    Ok(priv_str)
}

/// Phase 56-B: "Connect to existing server" provisioning shortcut.
///
/// The user already has an account on a remote server; they just want
/// ymux to:
///   1. Open a one-shot SSH session with their password,
///   2. Generate an ed25519 keypair (stored at
///      `%APPDATA%\ymux\keys\<workspace_id>.key`),
///   3. Append the pubkey to `~/.ssh/authorized_keys` on the remote,
///   4. Verify the key handshake works,
///   5. Persist a fresh workspace with the key path baked in.
///
/// The password is consumed in-memory only — never written to disk.
/// On any failure between steps 2 and 5 the partial keypair on disk
/// is left in place (the next attempt overwrites it); the workspace
/// is only persisted on a fully clean run.
///
/// Returns the new workspace_id so the frontend can switch to it.
#[tauri::command]
async fn provision_existing_install_key(
    state: State<'_, AppState>,
    app: AppHandle,
    host: String,
    port: u16,
    ssh_user: String,
    password: String,
    workspace_name: String,
) -> Result<String, String> {
    if host.is_empty() {
        return Err("host is required".into());
    }
    if ssh_user.is_empty() {
        return Err("user is required".into());
    }
    if password.is_empty() {
        return Err("password is required".into());
    }
    let workspace_id = new_workspace_id();

    // Compute key paths up front + clear any stale leftovers from a
    // previous attempt with the same (yet-to-be-persisted) id.
    let priv_path = ymux_key_path(&workspace_id)?;
    let pub_path: PathBuf = {
        let mut p = priv_path.clone();
        let mut s = p
            .file_name()
            .ok_or_else(|| "ymux_key_path: no file name".to_string())?
            .to_os_string();
        s.push(".pub");
        p.set_file_name(s);
        p
    };
    if priv_path.exists() {
        std::fs::remove_file(&priv_path).map_err(|e| format!("remove old key: {e}"))?;
    }
    if pub_path.exists() {
        std::fs::remove_file(&pub_path).map_err(|e| format!("remove old pubkey: {e}"))?;
    }
    let priv_str = priv_path.to_string_lossy().to_string();

    // 1) Generate the ed25519 keypair via the system ssh-keygen (ships
    //    with Windows 10+ OpenSSH; same call shape as the wizard's
    //    GenerateKeypair step + ssh_key_generate_and_install).
    let out = tokio::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            &format!("ymux-{workspace_id}"),
            "-f",
            &priv_str,
        ])
        .output()
        .await
        .map_err(|e| format!("spawn ssh-keygen: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let pub_line =
        std::fs::read_to_string(&pub_path).map_err(|e| format!("read pubkey: {e}"))?;
    let pub_line_trim = pub_line.trim().to_string();

    // 2) Connect with the password to validate creds + install the key.
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let target = format!("{host}:{port}");
    let mut handle = client::connect(
        config.clone(),
        (host.as_str(), port),
        SshClient::new_anonymous(target.clone()),
    )
    .await
    .map_err(|e| format!("ssh connect {target}: {e}"))?;
    let ok = handle
        .authenticate_password(&ssh_user, &password)
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    if !ok {
        return Err("authentication failed (password rejected)".into());
    }

    // 3) Append pubkey to ~/.ssh/authorized_keys. Idempotent (grep -qxF).
    let install_cmd = format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && \
         touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && \
         (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys)",
        key = pub_line_trim.replace('\'', "'\\''"),
    );
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, install_cmd.as_str())
        .await
        .map_err(|e| e.to_string())?;
    let mut out_buf = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(russh::ChannelMsg::Data { data }) => out_buf.extend_from_slice(&data[..]),
            Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                out_buf.extend_from_slice(&data[..])
            }
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = exit_status as i32
            }
            Some(russh::ChannelMsg::Close)
            | Some(russh::ChannelMsg::Eof)
            | None => break,
            _ => {}
        }
    }
    if exit_code != 0 {
        let stderr = String::from_utf8_lossy(&out_buf).to_string();
        return Err(format!(
            "install pubkey failed (exit {exit_code}): {stderr}"
        ));
    }
    // Drop the password-authenticated handle.
    drop(handle);

    // 4) Verify reconnect with the new key. Surfaces a clear error if
    //    sshd's PubkeyAuthentication is off or AuthorizedKeysFile is
    //    pointed somewhere unusual — before we persist a workspace
    //    that wouldn't work.
    let verify = connect_and_authenticate(
        &host,
        &ssh_user,
        port,
        Some(&priv_str),
        None,
        None,
        false,
    )
    .await
    .map_err(|e| format!("verify key: {e}"))?;
    drop(verify);

    // 5) Persist a fresh workspace. Connection mirrors workspace_create.
    let conn = Connection::Ssh {
        host: host.clone(),
        user: ssh_user.clone(),
        port,
        key_path: Some(priv_str.clone()),
    };
    let final_name = if workspace_name.trim().is_empty() {
        host.clone()
    } else {
        workspace_name.trim().to_string()
    };
    let ws = Workspace {
        id: workspace_id.clone(),
        name: final_name,
        connection: Some(conn.clone()),
        layout: Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            pane_kind: PaneKind::Terminal,
            connection: Some(conn),
            browser: None,
            title: None,
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }),
        ..Default::default()
    };
    {
        let mut file = state.workspaces.lock().map_err(|e| e.to_string())?;
        file.active_workspace_id = Some(workspace_id.clone());
        file.workspaces.push(ws);
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());

    log_info("WORKSPACE", &format!(
        "provision_existing_install_key: created ws={workspace_id} host={host} user={ssh_user}"
    ));
    Ok(workspace_id)
}

/// Run `echo $HOME` over a fresh exec channel. Returns (stdout, exit_code).
async fn remote_get_home(
    handle: &mut client::Handle<SshClient>,
) -> Result<(String, i32), String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, "echo $HOME").await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExitStatus { exit_status }) => code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    Ok((String::from_utf8_lossy(&out).to_string(), code))
}

/// Phase 41: result of the connect→host-key→authenticate handshake,
/// factored out of `spawn_ssh` so `workspace_ensure_connected` can
/// establish a reusable handle without a pane (no PTY / tmux / bootstrap
/// / reverse-tunnel — the caller owns those).
struct SshHandshake {
    handle: client::Handle<SshClient>,
    auth_method: AuthMethod,
    /// The reverse-tunnel HMAC token baked into the connection's handler.
    /// `spawn_ssh` forwards it to the remote for the CLI dial-back; the
    /// headless path ignores it.
    tunnel_token: Arc<String>,
}

/// Phase 41: connect to the SSH target, run the host-key check, and
/// authenticate. Shared by `spawn_ssh` (pane path) and
/// `workspace_ensure_connected` (headless background path). Surfaces the
/// same `UNKNOWN_HOST` / `HOST_KEY_MISMATCH` / auth-failure errors as
/// before. Includes the Phase 38 keepalive so headless handles also
/// survive idle NAT timeouts.
async fn connect_and_authenticate(
    host: &str,
    user: &str,
    port: u16,
    key_path: Option<&str>,
    key_passphrase: Option<&str>,
    password: Option<&str>,
    accept_unknown_host: bool,
) -> Result<SshHandshake, String> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let target = format!("{}:{}", host, port);
    let outcome_arc = Arc::new(Mutex::new(HostCheckOutcome::default()));
    let token = Arc::new(tunnel::generate_token());
    let sh = SshClient {
        target: target.clone(),
        accept_unknown: accept_unknown_host,
        result: outcome_arc.clone(),
        tunnel_token: Some(token.clone()),
        // Phase 51.B2 option β: inject the tunnel::spawn_bridge fn so
        // ymux-core's Handler impl can fire it on forwarded-tcpip
        // without taking a static dep on the tunnel module.
        bridge_spawner: Some(std::sync::Arc::new(tunnel::spawn_bridge)),
    };

    log_debug("SSH", &format!("ssh.connect: client::connect to {} starting", target));
    let connect_res = client::connect(config, (host, port), sh).await;
    log_debug("SSH", &format!(
        "ssh.connect: client::connect to {} returned (ok={})",
        target,
        connect_res.is_ok()
    ));
    let outcome = outcome_arc.lock().unwrap().clone();

    let mut handle = match connect_res {
        Ok(h) => h,
        Err(e) => {
            if outcome.is_unknown && !outcome.matched {
                return Err(format!(
                    "UNKNOWN_HOST:{}:{}:{}",
                    target, outcome.key_type, outcome.fingerprint
                ));
            }
            if let Some(old) = outcome.mismatch_old {
                return Err(format!(
                    "HOST_KEY_MISMATCH:{}:{}:{}:{}",
                    target, outcome.key_type, old, outcome.fingerprint
                ));
            }
            return Err(format!("connect {target}: {e}"));
        }
    };

    let auth_method = try_authenticate(&mut handle, user, key_path, key_passphrase, password).await?;
    let auth_method = match auth_method {
        Some(m) => m,
        None => return Err("authentication failed (agent, key, and password all failed)".into()),
    };

    Ok(SshHandshake {
        handle,
        auth_method,
        tunnel_token: token,
    })
}

async fn spawn_ssh(
    state: &AppState,
    pane_id: String,
    app: &AppHandle,
    workspace_id: String,
    host: String,
    user: String,
    port: u16,
    key_path: Option<String>,
    key_passphrase: Option<String>,
    password: Option<String>,
    accept_unknown_host: bool,
    cols: u16,
    rows: u16,
    persistent: bool,
    // Phase 23.F: when set, override the pane-id-derived tmux session
    // name. Passed through from pane_connect when the picker UI chose
    // a specific orphan session to attach to.
    tmux_session_name: Option<String>,
) -> Result<String, String> {
    log_debug("SSH", &format!(
        "spawn_ssh: entry ws={} pane={} target={}@{}:{}",
        workspace_id, pane_id, user, host, port
    ));
    // Phase 80: serialize connects per workspace. Held across the whole
    // function, so this pane's tunnel setup cannot interleave with a
    // headless `workspace_ensure_connected` for the same workspace — the
    // interleaving that allocated two rival forwards 38ms apart and left one
    // port-watcher orphaned on the server.
    //
    // LOCK ORDER: this one, then `bootstrap_guard::host_lock` further down.
    // `workspace_ensure_connected` takes only this one, and nothing takes
    // the host lock first, so the order is total and cycle-free. Two panes
    // on different workspaces sharing a host both wait on the same host
    // lock, which is a queue, not a cycle.
    let connect_lock = state.tunnel_registry.connect_lock(&workspace_id);
    let _connect_guard = connect_lock.lock().await;
    // Phase 41: connect + host-key + auth now live in the shared
    // `connect_and_authenticate` helper (includes the Phase 38 keepalive).
    log_debug("SSH", "spawn_ssh: connect_and_authenticate begin");
    let SshHandshake {
        mut handle,
        auth_method,
        tunnel_token: token,
    } = connect_and_authenticate(
        &host,
        &user,
        port,
        key_path.as_deref(),
        key_passphrase.as_deref(),
        password.as_deref(),
        accept_unknown_host,
    )
    .await?;
    log_info("SSH", &format!("spawn_ssh: authenticated method={auth_method:?}"));

    // Phase 32.B: offer to convert a password-auth connection to key
    // auth. Skipped when the user previously ticked "don't show again",
    // when auth already uses a key/agent, or when the workspace
    // already has a ymux-managed key on disk for this user@host.
    if auth_method == AuthMethod::Password {
        let suppressed = state
            .settings
            .lock()
            .ok()
            .map(|s| s.ssh_key_offer_disabled)
            .unwrap_or(false);
        if !suppressed && !ymux_managed_key_exists(&workspace_id) {
            let _ = app.emit(
                "ssh-key-offer",
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "pane_id": pane_id,
                    "ssh_user": user,
                    "ssh_host": host,
                    "ssh_port": port,
                }),
            );
        }
    }

    // Phase 6.2: best-effort bootstrap of the ymux Linux binary on the remote.
    // We never block the user's shell on this — failures are surfaced via pane:status.
    log_debug("SSH", "spawn_ssh: bootstrap starting");
    emit_pane_status_event(app, &pane_id, "bootstrapping ymux…");
    let hkey = bootstrap_guard::host_key(&user, &host, port);
    let wanted_sha = remote_bootstrap::embedded_manifest()
        .ok()
        .and_then(|m| m.get("x86_64-linux").map(|e| e.sha256.clone()))
        .unwrap_or_default();
    // One bootstrap per host at a time. Panes reconnect together after a
    // network drop, and without this each one opened its own SFTP channel
    // and pushed the same multi-megabyte payload at the same instant.
    let host_lock = state.bootstrap_guard.host_lock(&hkey);
    let _boot_guard = host_lock.lock().await;
    // A sibling pane may have converged the host while we waited on the
    // lock, so re-check the cache here rather than before it.
    let cached = state.bootstrap_guard.recent_failure(&hkey, &wanted_sha);
    let outcome = if let Some(msg) = cached {
        log_warn("SSH", &format!(
            "spawn_ssh: skipping bootstrap — the same upload to {hkey} failed recently ({msg})"
        ));
        Ok(remote_bootstrap::BootstrapStatus::Skew {
            expected: wanted_sha.clone(),
            actual: String::new(),
            reason: msg,
        })
    } else {
        remote_bootstrap::bootstrap(&mut handle, app, false).await
    };
    match outcome {
        Ok(remote_bootstrap::BootstrapStatus::AlreadyOk) => {
            state.bootstrap_guard.clear_failure(&hkey, &wanted_sha);
            set_cli_alignment(app, state, &workspace_id, bootstrap_guard::Alignment::Ok);
            emit_pane_status_event(app, &pane_id, "");
        }
        Ok(remote_bootstrap::BootstrapStatus::Uploaded { bytes, sha256: _ }) => {
            state.bootstrap_guard.clear_failure(&hkey, &wanted_sha);
            set_cli_alignment(app, state, &workspace_id, bootstrap_guard::Alignment::Ok);
            emit_pane_status_event(
                app,
                &pane_id,
                &format!("ymux installed ({} bytes)", bytes),
            );
            schedule_status_clear(app.clone(), pane_id.clone(), 3);
        }
        Ok(remote_bootstrap::BootstrapStatus::UnsupportedArch(arch)) => {
            emit_pane_status_event(
                app,
                &pane_id,
                &format!("remote arch '{}' not supported (no ymux binary)", arch),
            );
            schedule_status_clear(app.clone(), pane_id.clone(), 5);
        }
        // We could not converge the remote onto our binary. The shell still
        // works; the CLI-dependent features do not, and this stays on screen
        // until it's resolved instead of vanishing after five seconds — a
        // disappearing status line is exactly how a version-skewed CLI went
        // unnoticed for a whole debugging session.
        Ok(remote_bootstrap::BootstrapStatus::Skew {
            expected,
            actual,
            reason,
        }) => {
            log_warn("SSH", &format!(
                "spawn_ssh: remote CLI NOT aligned on {hkey} — expected {expected} got {} ({reason})",
                if actual.is_empty() { "<unknown>" } else { &actual }
            ));
            state
                .bootstrap_guard
                .record_failure(&hkey, &wanted_sha, &reason);
            set_cli_alignment(
                app,
                state,
                &workspace_id,
                bootstrap_guard::Alignment::Skew {
                    expected,
                    actual,
                    reason: reason.clone(),
                },
            );
            emit_pane_status_event(
                app,
                &pane_id,
                &format!("remote ymux CLI out of sync — {reason}"),
            );
        }
        Err(e) => {
            tracing::warn!("remote bootstrap failed: {e}");
            emit_pane_status_event(app, &pane_id, &format!("bootstrap failed: {e}"));
            schedule_status_clear(app.clone(), pane_id.clone(), 5);
        }
    }
    drop(_boot_guard);

    // Unified logging: converge this host on the desktop's log level
    // (`~/.ymux/log-level`, read by the Go server watcher + CLI hooks).
    // Best-effort — a failure never blocks the shell.
    {
        let level = state.settings.lock().unwrap().logs.level.clone();
        if let Err(e) = log_sync::push_log_level(&handle, &level).await {
            log_warn("LOGS", &format!("log-level push on connect failed: {e}"));
        }
    }

    // Phase 6.3 → 47.A: ask server to forward a port back to us. Forwarded
    // channels arrive in our Handler's `server_channel_open_forwarded_tcpip`
    // and get bridged to the local pipe. Phase 47.A factored this into
    // `setup_workspace_reverse_tunnel` so the headless connect path can
    // call the same setup — that helper also fires `spawn_port_watcher` and,
    // since Phase 80, writes `last.env` itself.
    //
    // The session id is minted HERE rather than just before the io-loop,
    // because the tunnel registration is keyed by the session that owns the
    // forward and has to be recorded before anything can fail.
    let id = next_session_id();
    let (remote_port, tunnel_lease) = match setup_workspace_reverse_tunnel(
        state,
        &mut handle,
        &workspace_id,
        &id,
        &token,
        Some(&pane_id),
    )
    .await
    {
        Some((p, lease)) => (p, Some(lease)),
        None => (0u16, None),
    };

    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open_session: {e}"))?;

    // Best-effort: try to set env vars on the shell. sshd's AcceptEnv may filter; if so,
    // the env-file fallback covers it.
    if remote_port != 0 {
        let socket_addr = format!("127.0.0.1:{}", remote_port);
        let _ = channel.set_env(false, "YMUX_SOCKET_ADDR", socket_addr).await;
        let _ = channel
            .set_env(false, "YMUX_TUNNEL_TOKEN", token.as_str().to_string())
            .await;
        let _ = channel
            .set_env(false, "YMUX_PANE_ID", pane_id.clone())
            .await;
    }

    // Phase 65 (J experiment): try to make remote CLIs (Claude Code)
    // emit OSC 8 hyperlinks. Best-effort — sshd's AcceptEnv may filter
    // these out, in which case J falls back to plan B (regex on the
    // visible `[file] …` text). Engineer-only log (Rule #9).
    for (k, v) in [
        ("FORCE_HYPERLINK", "1"),
        ("FORCE_HYPERLINKS", "1"),
        ("CLAUDE_CODE_FORCE_HYPERLINKS", "1"),
        ("COLORTERM", "truecolor"),
    ] {
        let _ = channel.set_env(false, k, v.to_string()).await;
    }
    tracing::debug!("spawn_ssh[{pane_id}]: requested hyperlink env vars (best-effort)");

    channel
        .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await
        .map_err(|e| format!("request_pty: {e}"))?;
    channel
        .request_shell(true)
        .await
        .map_err(|e| format!("request_shell: {e}"))?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SshCmd>();

    // Phase 8.B: wrap the handle in an Arc before the I/O task takes ownership.
    // russh's Handle isn't Clone, but its methods take &self, so multiple owners
    // of an Arc<Handle> can safely call channel_open_direct_tcpip concurrently
    // (each call is just a message into the underlying session task).
    let handle_arc = Arc::new(handle);
    let handle_for_task = Arc::clone(&handle_arc);
    let handle_for_state = Arc::clone(&handle_arc);
    let workspace_id_for_state = workspace_id.clone();

    // Phase 18: hooks-outdated probe. Fire-and-forget — never blocks
    // the SSH bring-up. Compares the version stamped into the
    // remote's ~/.claude/settings.json (under
    // `ymux_meta.hooks_version`) with the manifest's
    // `hooks.claude-code.version`. When the remote is older AND the
    // user hasn't dismissed that version, emit `hooks:outdated` so
    // the frontend banner appears.
    {
        let app_clone = app.clone();
        let state_clone: AppState = (*state).clone();
        let ws_id = workspace_id.clone();
        let pane_id_clone = pane_id.clone();
        let handle_for_hooks = Arc::clone(&handle_arc);
        tauri::async_runtime::spawn(async move {
            crate::updater::check_remote_hooks(
                &state_clone,
                &app_clone,
                &handle_for_hooks,
                &ws_id,
                &pane_id_clone,
            )
            .await;
        });
    }

    let id_for_task = id.clone();
    let pane_for_task = pane_id.clone();
    let app_for_task = app.clone();
    let sessions_for_task = state.core.sessions.clone();
    let pane_sessions_for_task = state.core.pane_sessions.clone();
    let forwards_for_task = state.core.forwards.clone();
    let workspace_for_task = workspace_id.clone();
    // Phase 39 → 80: clean up this session's reverse-tunnel registration
    // when the session ends. The sticky port deliberately survives, so the
    // reconnect can ask for it back and keep an already-running `claude`
    // reachable.
    let tunnel_registry_for_task = state.tunnel_registry.clone();
    let reverse_port_for_task = remote_port;
    // Phase JJ (port-watcher leak): clone the watcher maps so the
    // session-end task can abort this workspace's remote port-watcher when
    // its last SSH session goes away — otherwise the watcher's tokio task
    // lingers waiting on a dead channel (the channel-Eof self-clean is
    // unreliable when the transport drops), accumulating over a long
    // work session.
    let port_watchers_for_task = state.core.port_watchers.clone();
    let port_watcher_tasks_for_task = state.core.port_watcher_tasks.clone();
    let hosts_for_task = state.port_watcher_hosts.clone();
    let bidi_for_task = state.bidi_filters.clone();
    // beta.3 (netfree, Track 1b): capture the connection metadata so the
    // io-loop can emit a structured `ssh:disconnected` event on transport
    // drop. The frontend uses the payload to drive an auto-reconnect toast
    // (backoff loop lives in App.tsx, not here — keeps the server side
    // small and stops us from having to store credentials for the retry).
    let reconnect_host = host.clone();
    let reconnect_user = user.clone();
    let reconnect_port = port;
    let reconnect_key_path = key_path.clone();
    let reconnect_tmux_name = tmux_session_name.clone();
    let reconnect_persistent = persistent;
    // beta.3 (netfree, Track 1b): shared reconnect-in-progress flag. Created
    // here so both the io task (which flips it true on transport drop) and
    // the SshSession stored in the sessions map (so the cancel command can
    // clear it) point at the same AtomicBool.
    let reconnecting_flag =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reconnecting_for_task = std::sync::Arc::clone(&reconnecting_flag);
    let reconnecting_for_state = std::sync::Arc::clone(&reconnecting_flag);
    tokio::spawn(async move {
        // stdout and stderr are two independent byte streams, so each gets
        // its own UTF-8 reassembly buffer and its own OSC parser. Sharing
        // them (as this loop used to) splices the tail of a character or
        // an escape sequence on one stream onto the head of whatever
        // arrives on the other. A PTY was requested for this channel, so
        // ExtendedData is rare in practice — but "rare" is not "never",
        // and the corruption is silent when it happens.
        let mut stream_out = pty_decode::Utf8Stream::new();
        let mut stream_err = pty_decode::Utf8Stream::new();
        let mut osc = osc_notify::OscNotifyParser::new();
        let mut osc_err = osc_notify::OscNotifyParser::new();
        let mut exit_reason: Option<String> = None;
        // Phase 38: track last inbound data so disconnect logs carry a
        // "how long was it idle before dropping" age — distinguishes a
        // keepalive/NAT timeout (long idle) from an active-session drop.
        let mut last_data_at = std::time::Instant::now();
        // Phase 38: stable ids for the disconnect log line.
        let ch_id = channel.id();
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { data }) => {
                            last_data_at = std::time::Instant::now();
                            emit_data(&app_for_task, &id_for_task, &data[..], &mut stream_out, &pane_for_task, &mut osc, &bidi_for_task);
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                            last_data_at = std::time::Instant::now();
                            emit_data(&app_for_task, &id_for_task, &data[..], &mut stream_err, &pane_for_task, &mut osc_err, &bidi_for_task);
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            exit_reason = Some(format!("exit {exit_status}"));
                        }
                        Some(ChannelMsg::Eof) => {
                            log_info("SSH", &format!(
                                "ssh-disconnect: clean Eof, workspace={} pane={} channel={:?} last_activity_ms={}",
                                workspace_for_task, pane_for_task, ch_id, last_data_at.elapsed().as_millis()
                            ));
                            break;
                        }
                        Some(ChannelMsg::Close) => {
                            log_info("SSH", &format!(
                                "ssh-disconnect: clean Close, workspace={} pane={} channel={:?} last_activity_ms={}",
                                workspace_for_task, pane_for_task, ch_id, last_data_at.elapsed().as_millis()
                            ));
                            break;
                        }
                        None => {
                            log_warn("SSH", &format!(
                                "ssh-disconnect: transport dropped (likely network/keepalive timeout), workspace={} pane={} channel={:?} last_activity_ms={}",
                                workspace_for_task, pane_for_task, ch_id, last_data_at.elapsed().as_millis()
                            ));
                            // beta.3 (netfree, Track 1b): mark this session as
                            // reconnecting so the cancel command has a flag to
                            // clear, then emit the structured event so the
                            // frontend can show the reconnect toast and drive
                            // a backoff retry loop through the existing
                            // pane_connect command. Rule #1: no shell content
                            // in the payload — just connection identity + tmux
                            // hint. Guard: skip the emit if the flag was
                            // already set (a re-drop while a retry is in
                            // flight — the frontend is already showing the
                            // toast, no need to spawn a second one).
                            let was_already =
                                reconnecting_for_task.swap(true, std::sync::atomic::Ordering::Relaxed);
                            if was_already {
                                log_debug("SSH", &format!(
                                    "ssh-disconnect: reconnect already announced for pane={}, skipping duplicate emit",
                                    pane_for_task
                                ));
                                break;
                            }
                            let _ = app_for_task.emit(
                                "ssh:disconnected",
                                serde_json::json!({
                                    "workspace_id": workspace_for_task,
                                    "pane_id": pane_for_task,
                                    "host": reconnect_host,
                                    "user": reconnect_user,
                                    "port": reconnect_port,
                                    "key_path": reconnect_key_path,
                                    "tmux_session_name": reconnect_tmux_name,
                                    "persistent": reconnect_persistent,
                                    "reason": "transport-dropped",
                                }),
                            );
                            exit_reason = Some("transport dropped — reconnect toast emitted".into());
                            break;
                        }
                        _ => {}
                    }
                }
                cmd = rx.recv() => {
                    match cmd {
                        Some(SshCmd::Data(d)) => {
                            if channel.data(&d[..]).await.is_err() { break; }
                        }
                        Some(SshCmd::Resize(c, r)) => {
                            let _ = channel.window_change(c, r, 0, 0).await;
                        }
                        Some(SshCmd::Kill) | None => {
                            let _ = channel.close().await;
                            break;
                        }
                    }
                }
            }
        }
        let _ = handle_for_task
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
        cleanup_session_maps(
            &sessions_for_task,
            &pane_sessions_for_task,
            &pane_for_task,
            &id_for_task,
        );
        // Phase 39 → 80: drop this session's reverse-tunnel registration.
        // No `cancel_tcpip_forward` needed — the `disconnect` above tears the
        // whole transport down, and the forward dies with it.
        if reverse_port_for_task != 0 {
            tunnel_registry_for_task.unregister(&workspace_for_task, &id_for_task);
        }
        // Phase 8.B: if this was the last SSH session for the workspace, tear
        // down all of its port forwards.
        let still_alive = sessions_for_task
            .lock()
            .unwrap()
            .values()
            .any(|s| matches!(s, Session::Ssh(ssh) if ssh.workspace_id == workspace_for_task));
        if !still_alive {
            close_workspace_forwards(&forwards_for_task, &workspace_for_task);
            // Phase JJ: last SSH session for this workspace is gone — abort
            // the remote port-watcher task so it doesn't leak.
            let aborted = {
                let mut tasks = port_watcher_tasks_for_task.lock().unwrap();
                tasks.remove(&workspace_for_task).map(|h| h.abort()).is_some()
            };
            port_watchers_for_task
                .lock()
                .unwrap()
                .remove(&workspace_for_task);
            port_watcher_release_owner(&hosts_for_task, &workspace_for_task);
            if aborted {
                log_info("TUNNEL", &format!(
                    "port-watch[{workspace_for_task}]: workspace disconnected, watcher stopped"
                ));
            }
        }
        emit_exit(&app_for_task, &id_for_task, exit_reason);
    });

    // Phase 23.F: caller-supplied name wins (picker path); pane-id
    // fallback keeps the legacy auto-name behaviour.
    let tmux_name = if persistent {
        Some(
            tmux_session_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| sanitize_tmux_session_name(&pane_id)),
        )
    } else {
        None
    };
    state.core.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Ssh(SshSession {
            tx: Some(tx),
            handle: handle_for_state,
            workspace_id: workspace_id_for_state.clone(),
            tmux_session: tmux_name.clone(),
            host: host.clone(),
            user: user.clone(),
            port,
            key_path: key_path.clone(),
            // beta.3 (netfree): flipped true by the io-loop when the transport
            // drops so the reconnect-emit path is single-shot, and cleared by
            // the ssh_cancel_reconnect command when the user aborts the toast.
            reconnecting: reconnecting_for_state,
        }),
    );
    // The session owns its forward for real now — stop the lease from
    // rolling the registration back. Every `?` between the tunnel setup and
    // this line (channel_open_session, request_pty, request_shell) drops the
    // lease instead, which is the leak this replaced.
    if let Some(lease) = tunnel_lease {
        lease.commit();
    }

    // Phase 11.A: when the user picked persistent mode, wrap the freshly
    // started shell in `tmux new-session -A -s NAME`. The `-A` flag attaches
    // to an existing session of that name (so a reconnect resumes the same
    // shell with all in-flight processes intact) and creates a fresh one
    // otherwise. We `exec` it so the parent shell process is replaced —
    // killing the SSH channel then doesn't double-prompt for shell exit.
    //
    // We also push the env vars the SSH channel just acquired into tmux's
    // global environment so a re-attach to a long-lived session sees the
    // *current* YMUX_SOCKET_ADDR/TUNNEL_TOKEN/PANE_ID rather than the
    // stale ones from the original creation. The `2>/dev/null` swallows
    // the harmless "no server running" message when this is the first attach.
    if let Some(name) = &tmux_name {
        // Evidence log, before we attach. A user reported that unplugging a
        // laptop from AC left every tmux session on the server gone, and we
        // had no way to say WHEN they disappeared — only that they were
        // absent by the time anyone looked. One line per connect turns that
        // into a timestamped fact that can be lined up against the server's
        // own journal. Names only (Rule #1: never session content).
        match list_tmux_sessions_via_handle(&handle_arc).await {
            Ok(list) => log_info("SSH", &format!(
                "tmux-inventory: {} session(s) on {user}@{host} before attach: [{}]",
                list.len(),
                list.iter()
                    .map(|s| format!("{}(created={},attached={})", s.name, s.created, s.attached))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(e) => log_warn("SSH", &format!("tmux-inventory: could not list sessions: {e}")),
        }
        let sessions_clone = state.core.sessions.clone();
        let id_clone = id.clone();
        let name_clone = name.clone();
        let socket_addr = if remote_port != 0 {
            format!("127.0.0.1:{}", remote_port)
        } else {
            String::new()
        };
        let token_clone = token.as_str().to_string();
        let pane_for_exec = pane_id.clone();
        // Phase tmux-conf: read the user's setting BEFORE we hand
        // control to the spawned task (state.settings is not Send-
        // safe to hold across await points). Default true so users
        // who never touched Settings → Terminal get the bundled
        // scrollback-friendly behaviour out of the box.
        let use_ymux_tmux_conf = state
            .settings
            .lock()
            .ok()
            .map(|s| s.terminal.use_ymux_tmux_config)
            .unwrap_or(true);
        tokio::spawn(async move {
            // Wait a touch longer than schedule_setup_injection (which fires
            // at 500ms) so our exec lands AFTER the env exports + setup_command.
            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
            // Phase 65: log the exact session name + conf mode so tmux
            // persistence is debuggable. `new-session -A -s <name>`
            // attaches to <name> if it exists (reconnect resumes), else
            // creates it. <name> is deterministic per ymux pane unless
            // the picker supplied an explicit one.
            crate::log_debug("SSH", &format!(
                "tmux: new-session -A -s '{}' (pane {}, {} ymux conf)",
                name_clone,
                pane_for_exec,
                if use_ymux_tmux_conf { "with" } else { "without" }
            ));
            // Phase 80: script construction shared with WSL panes — see
            // build_tmux_attach_script for the env-injection + -f conf +
            // mouse-on rationale comments.
            let script = build_tmux_attach_script(
                &name_clone,
                &socket_addr,
                &token_clone,
                &pane_for_exec,
                use_ymux_tmux_conf,
                "[ymux] tmux not installed on remote — falling back to plain shell",
            );
            {
                let mut sessions = sessions_clone.lock().unwrap();
                if let Some(Session::Ssh(ssh)) = sessions.get_mut(&id_clone) {
                    let _ = ssh.try_send(SshCmd::Data(script.into_bytes()));
                }
            }
            // Phase 81: record this machine as the session's origin in the
            // server-side session-meta map (multi-machine sync). Separate
            // exec channel — NOT typed into the PTY — after the session has
            // had time to exist. `--origin-if-absent` means attaching to
            // another machine's session never steals its origin. Fire-and-
            // forget: an old/missing server CLI just errors quietly.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let origin_cmd = format!(
                "\"$HOME/.ymux/bin/ymux-linux-x64\" session-meta set --session {} --origin {} --origin-if-absent 2>/dev/null || true",
                shell_quote(&name_clone),
                shell_quote(&machine_id()),
            );
            let handle = {
                let sessions = sessions_clone.lock().unwrap();
                match sessions.get(&id_clone) {
                    Some(Session::Ssh(ssh)) => Some(ssh.handle.clone()),
                    _ => None,
                }
            };
            if let Some(h) = handle {
                if let Err(e) = crate::updater::ssh_exec_simple(&h, &origin_cmd).await {
                    log_warn("SSH", &format!("session-meta: origin write failed: {e}"));
                }
            }
        });
    }
    // Phase 8.B race fix: notify any browser pane in this workspace that a
    // fresh resolve is now possible (SSH handle is live → forwards can open).
    // Browser panes that loaded their iframe with `localhost refused to
    // connect` because SSH wasn't ready yet will pick this up and re-resolve.
    let _ = app.emit(
        "pane:browser:resolve-stale",
        serde_json::json!({ "workspace_id": workspace_id_for_state }),
    );
    Ok(id)
}

/// Phase 65 (DD): despite the name, for SSH this is a DETACH, not a
/// destroy — it closes the local SSH channel (`SshCmd::Kill` →
/// `channel.close()`), which makes the remote tmux *client* exit; the
/// tmux *session* keeps running on the server, so a later reconnect with
/// the same (pane-deterministic) name reattaches it.
///
/// 2026-08-20: **the same is true of a local zellij pane**, for a different
/// mechanism. `l.killer.kill()` kills the PTY child, which is PowerShell —
/// zellij is not the pane process, it is typed into that shell afterwards. So
/// the zellij *client* dies with the shell and the *server* keeps the session.
/// That is not a leak, it is the persistence feature.
///
/// The ONLY path that actually destroys a session — tmux or zellij — is
/// `pane_kill_session`, wired to the explicit "Kill session" button.
pub(crate) fn kill_session_inner(s: &mut Session) {
    match s {
        Session::Local(l) => {
            let _ = l.killer.kill();
        }
        Session::Ssh(ssh) => {
            log_info(
                "SSH",
                "session detach: closing SSH channel (tmux session stays alive on the server for reconnect)",
            );
            let _ = ssh.try_send(SshCmd::Kill);
        }
    }
}

// ─── Phase 8.B: SSH local port forwards ─────────────────────────────────────

// Find an SSH handle for the workspace by walking its connected terminal panes.
// Returns the first one found, or None if no terminal pane in the workspace
// currently has an active SSH session.
/// Rolls a tunnel registration back unless `commit()` is called.
///
/// Manual rollback was not an option: between `tcpip_forward` and the
/// `sessions.insert` that makes a registration legitimate there are three
/// `?` exits in `spawn_ssh` (channel_open_session, request_pty,
/// request_shell) plus the "a pane raced in" `return Ok(())` in
/// `workspace_ensure_connected`. Every one of them leaked a port, a token
/// and a watcher slot — which is how one workspace accumulated 20 dead
/// ports in a single session.
///
/// `Drop` is deliberately sync-only: it touches maps and aborts a task,
/// never the network. The forward itself dies with the russh handle, so
/// there is nothing async to undo.
struct TunnelLease {
    registry: tunnel_registry::TunnelRegistry,
    core: CoreState,
    hosts: PortWatcherHosts,
    workspace_id: String,
    session_id: String,
    committed: bool,
}

impl TunnelLease {
    /// The session is in `sessions` and really owns its forward. Keep it.
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for TunnelLease {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.registry
            .unregister(&self.workspace_id, &self.session_id);
        // Only stop the watcher when no sibling session still holds a
        // forward: a headless handle dropped because a pane won the race
        // must not take the pane's watcher down with it.
        if self.registry.current(&self.workspace_id).is_none() {
            let aborted = {
                let mut tasks = match self.core.port_watcher_tasks.lock() {
                    Ok(t) => t,
                    Err(e) => e.into_inner(),
                };
                tasks
                    .remove(&self.workspace_id)
                    .map(|h| h.abort())
                    .is_some()
            };
            match self.core.port_watchers.lock() {
                Ok(mut w) => {
                    w.remove(&self.workspace_id);
                }
                Err(e) => {
                    e.into_inner().remove(&self.workspace_id);
                }
            }
            port_watcher_release_owner(&self.hosts, &self.workspace_id);
            if aborted {
                log_debug("TUNNEL", &format!(
                    "tunnel-lease[{}]: rolled back, watcher stopped",
                    self.workspace_id
                ));
                return;
            }
        }
        log_debug("TUNNEL", &format!(
            "tunnel-lease[{}]: rolled back (session {} never committed)",
            self.workspace_id, self.session_id
        ));
    }
}

/// Ask sshd to forward `want` back to us; `want == 0` lets the kernel pick.
/// Returns the port we actually hold.
///
/// THE TRAP: russh documents `tcpip_forward` as *"If port == 0 the server
/// will choose a port that will be returned, returns 0 otherwise."* For a
/// specific-port request the granted port is the one we asked for and the
/// return value is 0 — reading it would hand us port 0 and a silently dead
/// tunnel, with no error anywhere to explain it.
async fn request_reverse_forward(
    handle: &mut client::Handle<SshClient>,
    want: u16,
) -> Result<u16, russh::Error> {
    let assigned = handle.tcpip_forward("127.0.0.1", want as u32).await?;
    Ok(if want == 0 { assigned as u16 } else { want })
}

#[cfg(test)]
mod tunnel_lease_tests {
    use super::*;

    /// Just the two pieces a lease actually touches.
    ///
    /// Deliberately NOT `AppState::default()`: AppState owns tauri's webview
    /// and tray maps, and naming it from test code drags the whole tauri
    /// runtime into the test binary — which then fails to load with
    /// STATUS_ENTRYPOINT_NOT_FOUND before a single test runs. The lease only
    /// needs the registry and CoreState, and CoreState is tauri-free.
    fn fixture() -> (tunnel_registry::TunnelRegistry, CoreState) {
        (tunnel_registry::TunnelRegistry::default(), CoreState::default())
    }

    fn lease(
        registry: &tunnel_registry::TunnelRegistry,
        core: &CoreState,
        ws: &str,
        session: &str,
    ) -> TunnelLease {
        TunnelLease {
            registry: registry.clone(),
            core: core.clone(),
            hosts: PortWatcherHosts::default(),
            workspace_id: ws.to_string(),
            session_id: session.to_string(),
            committed: false,
        }
    }

    #[test]
    fn dropping_an_uncommitted_lease_unregisters_the_port() {
        let (registry, core) = fixture();
        registry.register("w1", "s1", 44495, Arc::new("tok".to_string()));
        drop(lease(&registry, &core, "w1", "s1"));
        // This is the `return Ok(())` at the "a pane raced in" check, and the
        // three `?` exits in spawn_ssh: each used to leave a dead port, a
        // token and a watcher slot behind forever.
        assert!(
            registry.current("w1").is_none(),
            "an uncommitted lease must roll its registration back"
        );
    }

    #[test]
    fn a_committed_lease_survives_drop() {
        let (registry, core) = fixture();
        registry.register("w1", "s1", 44495, Arc::new("tok".to_string()));
        lease(&registry, &core, "w1", "s1").commit();
        let cur = registry.current("w1").expect("still registered");
        assert_eq!(cur.port, 44495);
    }

    #[test]
    fn dropping_a_lease_leaves_a_sibling_sessions_registration_alone() {
        let (registry, core) = fixture();
        // The normal shape: a headless handle and a pane both hold a forward.
        registry.register("w1", "__headless__w1", 1111, Arc::new("t_headless".into()));
        registry.register("w1", "s_pane", 2222, Arc::new("t_pane".into()));
        drop(lease(&registry, &core, "w1", "__headless__w1"));
        let cur = registry
            .current("w1")
            .expect("the pane's registration must survive");
        assert_eq!(cur.port, 2222);
        assert_eq!(cur.session_id, "s_pane");
    }

    #[test]
    fn a_rolled_back_lease_keeps_the_sticky_port_for_the_next_attempt() {
        let (registry, core) = fixture();
        registry.register("w1", "s1", 44495, Arc::new("tok".to_string()));
        drop(lease(&registry, &core, "w1", "s1"));
        // Rolling back the registration must NOT forget which port to ask
        // for — that port is still baked into a running claude's environment.
        assert_eq!(registry.sticky_port("w1"), Some(44495));
    }
}

/// Phase 47.A / Phase 80: workspace-level reverse-tunnel setup, called by
/// both `spawn_ssh` and the headless `workspace_ensure_connected`.
///
/// Phase 80 moved the env-file write in here. It used to live only in
/// `spawn_ssh` because it needs a pane id, so 15 of the 20 port allocations
/// in Yossi's 2026-08-18 log never updated the file the remote CLI reads.
///
/// That write is correctness, not the fix for the reported symptom: a hook
/// inherits `YMUX_SOCKET_ADDR` from an already-running `claude`, and
/// `load_fallback_env_file` skips the file entirely when that variable is
/// already set. The fix is the STICKY PORT — re-request the port this
/// workspace last held, so the address baked into that process stays valid.
///
/// Returns the port and a lease the caller MUST `commit()` once its session
/// is in the sessions map, or `None` if `tcpip_forward` failed (which still
/// leaves the SSH handle usable for tmux-list / file manager — just no
/// detection and no hooks).
async fn setup_workspace_reverse_tunnel(
    state: &AppState,
    handle: &mut client::Handle<SshClient>,
    workspace_id: &str,
    session_id: &str,
    token: &Arc<String>,
    pane_id: Option<&str>,
) -> Option<(u16, TunnelLease)> {
    // Sticky first. A denial is the NORMAL outcome when the remote has since
    // handed that port to something else, or when an orphaned forward from a
    // half-dead connection still holds it — hence debug, not warn, or it
    // would read as a fault on every other reconnect.
    let mut sticky_hit = None;
    if let Some(sticky) = state.tunnel_registry.sticky_port(workspace_id) {
        match request_reverse_forward(handle, sticky).await {
            Ok(p) => {
                log_info("TUNNEL", &format!(
                    "setup_workspace_reverse_tunnel[{workspace_id}]: reused sticky remote port {p}"
                ));
                sticky_hit = Some(p);
            }
            Err(e) => {
                log_debug("TUNNEL", &format!(
                    "setup_workspace_reverse_tunnel[{workspace_id}]: sticky port {sticky} unavailable ({e}) - asking for a fresh one"
                ));
            }
        }
    }
    let remote_port = match sticky_hit {
        Some(p) => p,
        None => match request_reverse_forward(handle, 0).await {
            Ok(p) => {
                log_info("TUNNEL", &format!(
                    "setup_workspace_reverse_tunnel[{workspace_id}]: tcpip_forward got remote port {p}"
                ));
                p
            }
            Err(e) => {
                log_warn("TUNNEL", &format!(
                    "setup_workspace_reverse_tunnel[{workspace_id}]: tcpip_forward failed: {e}"
                ));
                tracing::warn!("tcpip_forward[{workspace_id}] failed: {e}");
                return None;
            }
        },
    };
    if remote_port == 0 {
        return None;
    }
    // Port, token and owning session recorded as ONE triple. Reading a port
    // from one map and a token from another is what produced the
    // `-DENIED bad-mac` handshake rejections.
    state
        .tunnel_registry
        .register(workspace_id, session_id, remote_port, token.clone());
    let lease = TunnelLease {
        registry: state.tunnel_registry.clone(),
        core: state.core.clone(),
        hosts: state.port_watcher_hosts.clone(),
        workspace_id: workspace_id.to_string(),
        session_id: session_id.to_string(),
        committed: false,
    };
    // Best-effort env file so the CLI can dial back even when sshd's
    // AcceptEnv drops our per-channel `set_env`. Every allocation writes it
    // now, headless included — `pane_id: None` preserves whatever pane id
    // the file already carries, because its ABSENCE makes the remote hook
    // conclude ymux is not in this session and stop gating altogether.
    let socket_addr = format!("127.0.0.1:{remote_port}");
    match remote_get_home(handle).await {
        Ok((home_out, _)) => {
            let home = home_out.trim();
            if home.is_empty() {
                log_warn("TUNNEL", "tunnel: skip env-file write - $HOME came back empty");
            } else if let Err(e) =
                tunnel::write_remote_env_file(handle, home, &socket_addr, token, pane_id).await
            {
                log_warn("TUNNEL", &format!("tunnel: env-file write failed: {e}"));
            }
        }
        Err(e) => {
            log_warn("TUNNEL", &format!(
                "tunnel: skip env-file write - couldn't read $HOME: {e}"
            ));
        }
    }
    // Phase 47.A: best-effort watcher launch as part of tunnel setup.
    // spawn_port_watcher dedups via port_watchers so calling here AND
    // from try_ensure_port_watcher later is safe.
    let _ = spawn_port_watcher(state, handle, workspace_id, remote_port, token).await;
    Some((remote_port, lease))
}

/// Phase 86: per-host port-watcher sharing state. See `AppState::port_watcher_hosts`.
#[derive(Default, Debug)]
pub(crate) struct PortWatcherHost {
    /// Workspace whose watcher task is (or was) the live process. `None`
    /// once that task ended — the next `try_ensure_port_watcher` from any
    /// subscriber respawns and takes ownership.
    pub(crate) owner: Option<String>,
    pub(crate) subscribers: std::collections::HashSet<String>,
}
pub(crate) type PortWatcherHosts = Arc<Mutex<HashMap<String, PortWatcherHost>>>;

/// Phase 86: the host a workspace's port-watcher runs on. Workspace-level
/// `connection` first (canonical since 23.D), live SSH session as fallback
/// (the connection may still live only on a pane). `None` for anything
/// that isn't SSH — those never had a watcher.
fn port_watcher_host_key(state: &AppState, workspace_id: &str) -> Option<String> {
    let conn = state
        .workspaces
        .lock()
        .ok()
        .and_then(|f| f.workspaces.iter().find(|w| w.id == workspace_id)?.connection.clone())
        .or_else(|| live_ssh_connection_for_workspace(state, workspace_id));
    match conn {
        Some(Connection::Ssh { user, host, port, .. }) => {
            Some(bootstrap_guard::host_key(&user, &host, port))
        }
        _ => None,
    }
}

/// Phase 86: every workspace that should receive a port event reported
/// under `workspace_id` — the host's subscribers if it is known, otherwise
/// just itself (pre-sharing behaviour).
pub(crate) fn port_watcher_subscribers(state: &AppState, workspace_id: &str) -> Vec<String> {
    let hosts = match state.port_watcher_hosts.lock() {
        Ok(h) => h,
        Err(e) => e.into_inner(),
    };
    hosts
        .values()
        .find(|h| h.subscribers.contains(workspace_id))
        .map(|h| h.subscribers.iter().cloned().collect())
        .unwrap_or_else(|| vec![workspace_id.to_string()])
}

/// Phase 86: the owner's watcher task is gone — free the owner slot but
/// keep the subscribers, so the next ensure from any of them respawns.
fn port_watcher_release_owner(hosts: &PortWatcherHosts, workspace_id: &str) {
    let mut hosts = match hosts.lock() {
        Ok(h) => h,
        Err(e) => e.into_inner(),
    };
    for h in hosts.values_mut() {
        if h.owner.as_deref() == Some(workspace_id) {
            h.owner = None;
        }
    }
}

/// Phase 86: the workspace stops taking part — unsubscribe everywhere, drop
/// ownership (caller has already aborted the task), drop empty hosts.
fn port_watcher_forget(hosts: &PortWatcherHosts, workspace_id: &str) {
    let mut hosts = match hosts.lock() {
        Ok(h) => h,
        Err(e) => e.into_inner(),
    };
    for h in hosts.values_mut() {
        h.subscribers.remove(workspace_id);
        if h.owner.as_deref() == Some(workspace_id) {
            h.owner = None;
        }
    }
    hosts.retain(|_, h| h.owner.is_some() || !h.subscribers.is_empty());
}

/// Guard for interpolating a workspace id into a remote shell command
/// (Rule #3). Ids are internally generated as `w_<hex>`; accept only
/// `[A-Za-z0-9_-]` so a malformed id can never inject shell/regex syntax.
fn is_safe_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Phase 47: spawn the remote `ymux port-watch` for a workspace.
/// Deduplicated via `state.core.port_watchers` — calling twice in a row is
/// a no-op the second time. Stores the spawned task's JoinHandle in
/// `state.core.port_watcher_tasks` so toggling detection off can `.abort()`
/// it. Returns Err on channel/exec failure; on success the task
/// detaches and the watcher streams events back through the reverse
/// tunnel (dispatched by `port.opened` / `port.closed` in rpc_server).
async fn spawn_port_watcher(
    state: &AppState,
    handle: &client::Handle<SshClient>,
    workspace_id: &str,
    remote_port: u16,
    token: &Arc<String>,
) -> Result<(), String> {
    // Phase 86: one watcher per HOST. If a sibling workspace on the same
    // host already owns a live watcher, subscribe to it and stop here —
    // `port.opened` / `port.closed` fan out to every subscriber.
    let hkey = port_watcher_host_key(state, workspace_id);
    if let Some(hk) = hkey.as_deref() {
        let owner = {
            let hosts = state.port_watcher_hosts.lock().map_err(|e| e.to_string())?;
            hosts.get(hk).and_then(|h| h.owner.clone())
        };
        if let Some(owner) = owner.filter(|o| o != workspace_id) {
            let alive = state
                .core
                .port_watcher_tasks
                .lock()
                .map_err(|e| e.to_string())?
                .get(&owner)
                .map(|h| !h.is_finished())
                .unwrap_or(false);
            if alive {
                let mut hosts = state.port_watcher_hosts.lock().map_err(|e| e.to_string())?;
                hosts
                    .entry(hk.to_string())
                    .or_default()
                    .subscribers
                    .insert(workspace_id.to_string());
                log_info("PORTWATCH", &format!(
                    "port-watch[{workspace_id}]: shares watcher of {owner} (host {hk})"
                ));
                return Ok(());
            }
            // Owner's task is dead but nobody freed the slot — take over.
            port_watcher_release_owner(&state.port_watcher_hosts, &owner);
        }
    }
    // Dedup, self-healing (Phase JJ). Skip ONLY if a LIVE watcher is
    // already running for this workspace. If the set still has the entry
    // but the task is finished/missing (e.g. a disconnect that didn't
    // reach the abort path, or a channel-Eof clean that raced), the slot
    // is stale — clear it and respawn fresh on the current session. Locks
    // are taken one at a time (never nested) to match the lock order in
    // clear_workspace_detection and avoid a deadlock.
    {
        let already = state
            .core
            .port_watchers
            .lock()
            .unwrap()
            .contains(workspace_id);
        if already {
            let alive = state
                .core
                .port_watcher_tasks
                .lock()
                .unwrap()
                .get(workspace_id)
                .map(|h| !h.is_finished())
                .unwrap_or(false);
            if alive {
                return Ok(());
            }
            // Stale slot — abort any finished handle and replace it.
            if let Some(h) = state
                .core
                .port_watcher_tasks
                .lock()
                .unwrap()
                .remove(workspace_id)
            {
                h.abort();
            }
            state.core.port_watchers.lock().unwrap().remove(workspace_id);
            log_debug("TUNNEL", &format!(
                "port-watch[{workspace_id}]: stale slot, replacing with fresh watcher"
            ));
        }
        state
            .core
            .port_watchers
            .lock()
            .unwrap()
            .insert(workspace_id.to_string());
    }
    // Phase JJ.2 (leak fix): the desktop-side dedup above only knows about
    // watchers THIS app process launched. A remote `ymux port-watch` does
    // NOT die when its SSH channel closes, so every desktop restart / reconnect
    // orphaned another one — Yossi's server had 49 of them for one workspace,
    // pinning the CPU. Before launching a fresh watcher, reap any stale remote
    // ones for THIS workspace so exactly one runs server-side. The `[-]` glob
    // trick keeps pkill from matching (and killing) its own launching shell.
    // Phase 86: runs BEFORE the CLI-version gate on purpose — a host whose
    // CLI is out of sync still gets its stale watchers reaped. It must stay
    // AFTER the live-dedup above: the pattern matches this workspace's own
    // live watcher too.
    if is_safe_workspace_id(workspace_id) {
        if let Ok(mut kchan) = handle.channel_open_session().await {
            let kill = format!(
                "pkill -f 'port-watch [-]-workspace {workspace_id}' 2>/dev/null; true"
            );
            if kchan.exec(true, kill.as_str()).await.is_ok() {
                loop {
                    match kchan.wait().await {
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                        _ => {}
                    }
                }
            }
        }
    }
    // The watcher IS the remote CLI (`ymux port-watch`), talking the RPC
    // protocol this desktop compiled against. Launching a binary we know is
    // the wrong build produces failures far from their cause — the repeating
    // reverse-tunnel handshake rejections in Yossi's log are what that looks
    // like. Refuse clearly instead.
    if !state.bootstrap_guard.is_aligned(workspace_id) {
        log_warn("TUNNEL", &format!(
            "port-watch[{workspace_id}]: skipped — remote ymux CLI is not the build this desktop embeds"
        ));
        state.core.port_watchers.lock().unwrap().remove(workspace_id);
        return Err("remote ymux CLI out of sync".into());
    }
    let mut wchan = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            log_warn("TUNNEL", &format!("port-watch[{workspace_id}]: channel_open_session failed: {e}"));
            state.core.port_watchers.lock().unwrap().remove(workspace_id);
            return Err(format!("channel_open: {e}"));
        }
    };
    let socket_addr = format!("127.0.0.1:{}", remote_port);
    let _ = wchan.set_env(false, "YMUX_SOCKET_ADDR", socket_addr).await;
    let _ = wchan
        .set_env(false, "YMUX_TUNNEL_TOKEN", token.as_str().to_string())
        .await;
    // Exec channels don't source the rc files that add ~/.ymux/bin to PATH,
    // so use the explicit path.
    let cmd = format!(
        "\"$HOME/.ymux/bin/ymux\" port-watch --workspace {}",
        shell_quote(workspace_id)
    );
    if let Err(e) = wchan.exec(true, cmd.as_str()).await {
        log_warn("TUNNEL", &format!("port-watch[{workspace_id}]: exec failed: {e}"));
        state.core.port_watchers.lock().unwrap().remove(workspace_id);
        return Err(format!("exec failed: {e}"));
    }
    let ws_guard = workspace_id.to_string();
    let watchers = state.core.port_watchers.clone();
    let tasks = state.core.port_watcher_tasks.clone();
    let hosts = state.port_watcher_hosts.clone();
    let task = tokio::spawn(async move {
        loop {
            match wchan.wait().await {
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        watchers.lock().unwrap().remove(&ws_guard);
        tasks.lock().unwrap().remove(&ws_guard);
        // Phase 86: subscribers stay; the next ensure from any of them
        // respawns and becomes owner.
        port_watcher_release_owner(&hosts, &ws_guard);
        log_debug("TUNNEL", &format!(
            "port-watch[{ws_guard}]: channel closed, watcher slot freed"
        ));
    });
    state.core
        .port_watcher_tasks
        .lock()
        .unwrap()
        .insert(workspace_id.to_string(), task);
    if let Some(hk) = hkey {
        let mut hosts = state.port_watcher_hosts.lock().map_err(|e| e.to_string())?;
        let h = hosts.entry(hk).or_default();
        h.owner = Some(workspace_id.to_string());
        h.subscribers.insert(workspace_id.to_string());
    }
    log_info("TUNNEL", &format!(
        "port-watch[{workspace_id}]: launched (remote_port={remote_port})"
    ));
    Ok(())
}

/// Phase 47: abort the watcher task + clear the workspace's detected
/// ports, and tell the FE to wipe its list. Idempotent — safe to call
/// when no watcher is running.
fn clear_workspace_detection(state: &AppState, app: &AppHandle, workspace_id: &str) {
    let aborted = {
        let mut tasks = state.core.port_watcher_tasks.lock().unwrap();
        tasks.remove(workspace_id).map(|h| {
            h.abort();
            true
        })
    };
    if aborted.is_some() {
        state.core.port_watchers.lock().unwrap().remove(workspace_id);
    }
    // Phase 86: leave the host's sharing group (drops ownership too).
    port_watcher_forget(&state.port_watcher_hosts, workspace_id);
    state.core
        .detected_ports
        .lock()
        .unwrap()
        .remove(workspace_id);
    let _ = app.emit(
        "port-detection-cleared",
        serde_json::json!({ "workspace_id": workspace_id }),
    );
    log_debug("TUNNEL", &format!(
        "port-watch[{workspace_id}]: detection cleared (was_running={})",
        aborted.is_some()
    ));
}

fn find_ssh_handle_for_workspace(
    state: &AppState,
    workspace_id: &str,
) -> Option<Arc<client::Handle<SshClient>>> {
    let sessions = state.core.sessions.lock().unwrap();
    for s in sessions.values() {
        if let Session::Ssh(ssh) = s {
            if ssh.workspace_id == workspace_id {
                return Some(Arc::clone(&ssh.handle));
            }
        }
    }
    None
}

/// Phase 36 (#2.2) → 36.A: open an auto-forward for a remote listening
/// port. We bind `127.0.0.1:0` and let the kernel hand us a free
/// ephemeral port — the user reaches the server at whatever local port
/// that is (shown in the Ports panel). This is simpler and race-free vs
/// trying to match the remote port: no +1..+9 fallback, no cross-
/// workspace collision when two servers both listen on :3000.
/// Idempotent on (workspace, remote_port).
pub(crate) async fn open_auto_forward(
    state: &AppState,
    app: &AppHandle,
    workspace_id: &str,
    remote_addr: &str,
    remote_port: u16,
) -> Result<u16, String> {
    {
        let m = state.core.forwards.lock().unwrap();
        if let Some(e) = m.get(&(workspace_id.to_string(), remote_port)) {
            return Ok(e.local_port);
        }
    }
    let handle = find_ssh_handle_for_workspace(state, workspace_id)
        .ok_or_else(|| "no active SSH session for this workspace".to_string())?;

    // Bind port 0 → kernel picks a free ephemeral port (Windows ~49152+).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let ws_for_task = workspace_id.to_string();
    let forwards_for_task = state.core.forwards.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                accept = listener.accept() => {
                    let (mut sock, peer) = match accept {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let h = Arc::clone(&handle);
                    tokio::spawn(async move {
                        let chan = match h
                            .channel_open_direct_tcpip(
                                "localhost",
                                remote_port as u32,
                                peer.ip().to_string(),
                                peer.port() as u32,
                            )
                            .await
                        {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        let mut chan_stream = chan.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut chan_stream).await;
                    });
                }
            }
        }
        forwards_for_task
            .lock()
            .unwrap()
            .remove(&(ws_for_task, remote_port));
    });

    state.core.forwards.lock().unwrap().insert(
        (workspace_id.to_string(), remote_port),
        ForwardEntry {
            local_port,
            cancel: Some(cancel_tx),
        },
    );
    // Phase 62.A (item F): the local tunnel listener is loopback-only
    // (127.0.0.1) — services are reachable from this machine, never the
    // LAN/external IP. Logged explicitly so a future "it's going through
    // my external IP" report can be ruled out from the debug.log.
    log_info("TUNNEL", &format!(
        "open_auto_forward[{}:{}]: bound 127.0.0.1:{} (loopback only, kernel-assigned)",
        workspace_id, remote_port, local_port
    ));

    // Phase 46: sanity-probe the bound local port before telling the FE
    // the forward is live. Catches the IPv4/IPv6 dual-stack pitfall and
    // any binds that look successful but aren't actually accepting yet —
    // so the user never opens a browser tab on a dead port.
    let probe_target = format!("127.0.0.1:{local_port}");
    let probe = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        tokio::net::TcpStream::connect(&probe_target),
    )
    .await;
    let probe_ok = matches!(probe, Ok(Ok(_)));
    if !probe_ok {
        let why = match probe {
            Ok(Err(e)) => format!("connect failed: {e}"),
            Err(_) => "200ms timeout".to_string(),
            Ok(Ok(_)) => unreachable!(),
        };
        log_warn("TUNNEL", &format!(
            "open_auto_forward[{}:{}]: sanity probe to {} FAILED ({why}) — tearing down",
            workspace_id, remote_port, probe_target
        ));
        close_one_forward(state, app, workspace_id, remote_port);
        return Err(format!(
            "forward bound but localhost:{local_port} unreachable ({why})"
        ));
    }
    log_debug("TUNNEL", &format!(
        "open_auto_forward[{}:{}]: sanity probe to {} OK",
        workspace_id, remote_port, probe_target
    ));
    let _ = app.emit(
        "port-forwarded",
        serde_json::json!({
            "workspace_id": workspace_id,
            "remote_addr": remote_addr,
            "remote_port": remote_port,
            "local_port": local_port,
        }),
    );
    Ok(local_port)
}

/// Phase 36: tear down a single (workspace, remote_port) forward.
pub(crate) fn close_one_forward(
    state: &AppState,
    app: &AppHandle,
    workspace_id: &str,
    remote_port: u16,
) {
    let removed = {
        let mut m = state.core.forwards.lock().unwrap();
        m.remove(&(workspace_id.to_string(), remote_port))
    };
    if let Some(mut e) = removed {
        if let Some(c) = e.cancel.take() {
            let _ = c.send(());
        }
        let _ = app.emit(
            "port-forward-stopped",
            serde_json::json!({
                "workspace_id": workspace_id,
                "remote_port": remote_port,
            }),
        );
    }
}

/// Cancel every forward task whose key has the given workspace_id.
pub(crate) fn close_workspace_forwards(forwards: &ForwardMap, workspace_id: &str) {
    let mut m = forwards.lock().unwrap();
    let keys: Vec<(String, u16)> = m
        .keys()
        .filter(|(w, _)| w == workspace_id)
        .cloned()
        .collect();
    for k in keys {
        if let Some(mut e) = m.remove(&k) {
            if let Some(c) = e.cancel.take() {
                let _ = c.send(());
            }
        }
    }
}

// Phase 23.B: does the layout contain a non-terminal pane that depends on
// a live workspace-level SSH handle? FileManager and Browser panes pull
// the SSH handle out of `state.core.sessions` at runtime via
// `pick_ssh_handle_for_workspace`; if we tear down the last terminal pane's
// SSH session, those panes go dark with no in-UI way to reconnect.
// ClaudeChat is local, doesn't count.
#[allow(deprecated)]
fn layout_has_ssh_consumer_pane(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Pane { pane_kind, .. } => {
            matches!(pane_kind, PaneKind::FileManager | PaneKind::Browser)
        }
        LayoutNode::Split { first, second, .. } => {
            layout_has_ssh_consumer_pane(first) || layout_has_ssh_consumer_pane(second)
        }
    }
}

// ─── Workspace mutation commands ─────────────────────────────────────────────

#[tauri::command]
fn workspaces_load(state: State<'_, AppState>) -> Result<WorkspacesFile, String> {
    let file = state.workspaces.lock().unwrap().clone();
    log_debug("WORKSPACE", &format!(
        "workspaces_load: returning {} workspaces, active={:?}",
        file.workspaces.len(),
        file.active_workspace_id
    ));
    Ok(file)
}

#[tauri::command]
fn workspace_create(
    state: State<'_, AppState>,
    input: CreateInput,
) -> Result<WorkspacesFile, String> {
    // Phase 23.D: workspace.connection is canonical from creation
    // onward. The first Terminal pane also carries it for
    // back-compat with older code paths that read pane.connection
    // directly; future panes added via split / programmatic add
    // inherit from the workspace level when their own field is None.
    let conn = input.connection.clone();
    let ws = Workspace {
        id: new_workspace_id(),
        name: input.name,
        color: input.color,
        cwd: input.cwd,
        connection: Some(conn.clone()),
        layout: Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            pane_kind: PaneKind::Terminal,
            connection: Some(conn),
            browser: None,
            title: None,
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }),
        setup_command: input.setup_command,
        teardown_command: input.teardown_command,
        env: input.env.unwrap_or_default(),
        ..Default::default()
    };
    {
        let mut file = state.workspaces.lock().unwrap();
        file.active_workspace_id = Some(ws.id.clone());
        file.workspaces.push(ws);
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 7.C: edit a workspace's mutable metadata fields. Each field is `Option`:
/// `None` = don't touch; `Some(...)` = update. For `setup_command`/`teardown_command`/
/// `cwd`, an empty string is treated as "clear". For `env`, an empty Vec replaces
/// the whole list with empty.
#[tauri::command]
fn workspace_update(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    name: Option<String>,
    color: Option<String>,
    cwd: Option<String>,
    setup_command: Option<String>,
    teardown_command: Option<String>,
    env: Option<Vec<EnvVar>>,
    // Phase 37: editable connection. When present, replaces the
    // workspace's canonical connection AND rewrites every Terminal
    // pane's connection so the next reconnect uses the new host / user /
    // port / key. Absent = leave the connection untouched.
    connection: Option<Connection>,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        if let Some(n) = name {
            if !n.is_empty() {
                ws.name = n;
            }
        }
        if let Some(c) = color {
            ws.color = if c.is_empty() { None } else { Some(c) };
        }
        if let Some(d) = cwd {
            ws.cwd = if d.is_empty() { None } else { Some(d) };
        }
        if let Some(s) = setup_command {
            ws.setup_command = if s.is_empty() { None } else { Some(s) };
        }
        if let Some(t) = teardown_command {
            ws.teardown_command = if t.is_empty() { None } else { Some(t) };
        }
        if let Some(e) = env {
            ws.env = e;
        }
        if let Some(conn) = connection {
            ws.connection = Some(conn.clone());
            if let Some(layout) = ws.layout.as_mut() {
                set_terminal_connections(layout, &conn);
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 37: rewrite the `connection` on every Terminal pane in the
/// layout to `conn`. Used when the user edits a workspace's connection
/// so existing panes reconnect with the new credentials. Non-terminal
/// panes (browser / file-manager / help) carry no connection — skipped.
fn set_terminal_connections(node: &mut LayoutNode, conn: &Connection) {
    match node {
        LayoutNode::Pane {
            pane_kind,
            connection,
            ..
        } => {
            if matches!(pane_kind, PaneKind::Terminal) {
                *connection = Some(conn.clone());
            }
        }
        LayoutNode::Split { first, second, .. } => {
            set_terminal_connections(first, conn);
            set_terminal_connections(second, conn);
        }
    }
}

/// Phase 8 fix v3: emergency reset for a workspace whose layout has been
/// corrupted (e.g. by the autosave loop that produced deeply nested splits).
/// Replaces the layout with a single fresh terminal pane using the workspace's
/// existing connection if it had one (terminal panes), else local default.
#[tauri::command]
fn workspace_reset_layout(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        // Pick a connection for the fresh pane:
        // 1. The first terminal pane in the (corrupted) layout, if any.
        // 2. The legacy `connection` field on the workspace.
        // 3. Default Local with no shell override.
        let inferred = ws
            .layout
            .as_ref()
            .and_then(first_terminal_connection)
            .or_else(|| ws.connection.clone())
            .unwrap_or(Connection::Local { shell: None });
        ws.layout = Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            pane_kind: PaneKind::Terminal,
            connection: Some(inferred),
            browser: None,
            title: None,
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        });
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

// Phase 51.B1: first_terminal_connection_pub moved to ymux-core.

/// Phase 23.C: visible to other modules (rpc_server) for the same
/// inheritance chain when splits come in via RPC.
pub(crate) fn live_ssh_connection_for_workspace_pub(
    state: &AppState,
    workspace_id: &str,
) -> Option<Connection> {
    live_ssh_connection_for_workspace(state, workspace_id)
}

// Phase 23.C: extract a `Connection` from a live SSH session for this
// workspace. Returns None if no SSH session is currently bound to the
// workspace. Used as a second-tier fallback in `workspace_split` so
// the user can re-add a terminal pane to an SSH workspace whose
// connection details no longer live in any pane (e.g. after closing
// the last terminal but the SSH handle is still alive because a
// FileManager pane kept it pinned).
fn live_ssh_connection_for_workspace(
    state: &AppState,
    workspace_id: &str,
) -> Option<Connection> {
    let sessions = state.core.sessions.lock().ok()?;
    for sess in sessions.values() {
        if let Session::Ssh(s) = sess {
            if s.workspace_id == workspace_id {
                return Some(Connection::Ssh {
                    host: s.host.clone(),
                    user: s.user.clone(),
                    port: s.port,
                    key_path: s.key_path.clone(),
                });
            }
        }
    }
    None
}

// Phase 51.B1: first_terminal_connection + backfill_terminal_connections
// moved to ymux-core.

// ─── project-folder helpers ─────────────────────────────────────────
//
// A project folder used to be its own entity at the file root, owning a
// `Connection` and rendered as a sidebar section. It is now simply a
// workspace: `is_project_root` marks the one whose `cwd` gets scanned
// for worktrees, and `parent_id` puts it under the workspace it was
// pinned from. Only these two helpers survived the entity.

/// Last path component, used as the default label. Handles both
/// separators because a Local workspace path is Windows-shaped and an
/// SSH one is not.
fn path_basename(path: &str) -> String {
    path.replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Cheap host identity. Two `None`s are the same host (this machine);
/// SSH compares user@host:port, ignoring the key path since that is a
/// credential, not an identity.
fn conn_same_host(a: &Option<Connection>, b: &Option<Connection>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Connection::Local { .. }), None) | (None, Some(Connection::Local { .. })) => true,
        (Some(Connection::Local { .. }), Some(Connection::Local { .. })) => true,
        (Some(Connection::Wsl { distro: x }), Some(Connection::Wsl { distro: y })) => x == y,
        (
            Some(Connection::Ssh { host: h1, user: u1, port: p1, .. }),
            Some(Connection::Ssh { host: h2, user: u2, port: p2, .. }),
        ) => h1 == h2 && u1 == u2 && p1 == p2,
        _ => false,
    }
}


// ─── project-folder / worktree workspace commands ───────────────────

/// Walk up from `id`, yielding ancestor ids nearest-first. Capped by the
/// workspace count so a cycle that slipped past `normalize_parents`
/// cannot spin here.
fn ancestors_of(file: &WorkspacesFile, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = id.to_string();
    for _ in 0..file.workspaces.len() {
        let parent = file
            .workspaces
            .iter()
            .find(|w| w.id == cur)
            .and_then(|w| w.parent_id.clone());
        match parent {
            Some(p) => {
                out.push(p.clone());
                cur = p;
            }
            None => break,
        }
    }
    out
}

/// Pin a git repo as a child workspace of `parent_workspace_id`.
///
/// The caller validates the path with `git_probe_worktrees` first, so a
/// directory that is not a repo never reaches here — this command only
/// persists. The child inherits a CLONE of the parent's connection: the
/// folder must keep working when the parent is disconnected, and the SSH
/// handle is resolved per call by user@host:port anyway.
#[tauri::command]
fn workspace_pin_project_folder(
    state: State<'_, AppState>,
    app: AppHandle,
    parent_workspace_id: String,
    path: String,
    name: Option<String>,
) -> Result<WorkspacesFile, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("project path is required".to_string());
    }
    let new_id = new_workspace_id();
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let parent = file
            .workspaces
            .iter()
            .find(|w| w.id == parent_workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        let conn = parent.connection.clone();

        // Two repos in one ancestor chain would each scan the same
        // worktrees and render them under different parents.
        if parent.is_project_root
            || ancestors_of(&file, &parent_workspace_id)
                .iter()
                .any(|a| {
                    file.workspaces
                        .iter()
                        .any(|w| &w.id == a && w.is_project_root)
                })
        {
            return Err("this workspace is already inside a project folder".to_string());
        }
        if file.workspaces.iter().any(|w| {
            w.parent_id.as_deref() == Some(parent_workspace_id.as_str())
                && w.cwd.as_deref() == Some(path.as_str())
        }) {
            return Err("this folder is already pinned here".to_string());
        }

        let label = match name.map(|n| n.trim().to_string()) {
            Some(n) if !n.is_empty() => n,
            _ => {
                let b = path_basename(&path);
                if b.is_empty() { path.clone() } else { b }
            }
        };
        let effective = conn.clone().unwrap_or(Connection::Local { shell: None });
        file.workspaces.push(Workspace {
            id: new_id.clone(),
            name: label,
            cwd: Some(path.clone()),
            connection: Some(effective.clone()),
            layout: Some(single_terminal_layout(effective)),
            parent_id: Some(parent_workspace_id.clone()),
            is_project_root: true,
            ..Default::default()
        });
        file.active_workspace_id = Some(new_id.clone());
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    log_info(
        "WORKSPACE",
        &format!("pinned a project folder under ws={parent_workspace_id} as ws={new_id}"),
    );
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Open one worktree of a project-folder workspace as its own child.
///
/// Idempotent: a worktree that already has a workspace is activated
/// rather than duplicated. The first pane is a plain interactive shell —
/// cmux#5032 spawned the setup command as PID 1 and the tab died the
/// moment that command exited.
#[tauri::command]
fn workspace_open_worktree(
    state: State<'_, AppState>,
    app: AppHandle,
    root_workspace_id: String,
    worktree_path: String,
    name: String,
) -> Result<WorkspacesFile, String> {
    let worktree_path = worktree_path.trim().to_string();
    if worktree_path.is_empty() {
        return Err("worktree path is required".to_string());
    }
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let root = file
            .workspaces
            .iter()
            .find(|w| w.id == root_workspace_id)
            .ok_or_else(|| "project folder workspace not found".to_string())?;
        let conn = root
            .connection
            .clone()
            .unwrap_or(Connection::Local { shell: None });

        // The repo root's own entry in `git worktree list` IS this
        // workspace. Opening it would create a child sharing its
        // parent's directory.
        if root.cwd.as_deref() == Some(worktree_path.as_str()) {
            file.active_workspace_id = Some(root_workspace_id.clone());
            drop(file);
            persist(&state)?;
            return Ok(state.workspaces.lock().unwrap().clone());
        }
        if let Some(existing) = file
            .workspaces
            .iter()
            .find(|w| {
                w.parent_id.as_deref() == Some(root_workspace_id.as_str())
                    && w.cwd.as_deref() == Some(worktree_path.as_str())
            })
            .map(|w| w.id.clone())
        {
            file.active_workspace_id = Some(existing);
            drop(file);
            persist(&state)?;
            return Ok(state.workspaces.lock().unwrap().clone());
        }

        let id = new_workspace_id();
        file.workspaces.push(Workspace {
            id: id.clone(),
            name,
            cwd: Some(worktree_path),
            connection: Some(conn.clone()),
            layout: Some(single_terminal_layout(conn)),
            parent_id: Some(root_workspace_id.clone()),
            ..Default::default()
        });
        file.active_workspace_id = Some(id);
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 87.B: where a session row goes in the tree.
///
/// The deepest pinned project folder under `root_id` whose `cwd` equals or
/// contains `session_cwd` — `path_is_within` insists on a separator
/// boundary, so `/srv/app2` is not inside `/srv/app`. No such folder, or no
/// cwd at all (a zellij session with no ownership row), and the row hangs
/// directly under the server. Never pins a folder on the user's behalf.
fn pick_session_parent(file: &WorkspacesFile, root_id: &str, session_cwd: Option<&str>) -> String {
    let Some(cwd) = session_cwd.map(str::trim).filter(|c| !c.is_empty()) else {
        return root_id.to_string();
    };
    let subtree = collect_subtree_ids(file, root_id);
    file.workspaces
        .iter()
        .filter(|w| w.is_project_root && subtree.iter().any(|id| id == &w.id))
        .filter_map(|w| w.cwd.as_deref().map(|c| (c, &w.id)))
        .filter(|(folder, _)| path_is_within(cwd, folder))
        .max_by_key(|(folder, _)| folder.len())
        .map(|(_, id)| id.clone())
        .unwrap_or_else(|| root_id.to_string())
}

/// Phase 87.B: open a multiplexer session on a screen of its own — a
/// persisted child workspace row under the machine (or under the pinned
/// project folder whose directory contains the session's), whose single
/// pane the frontend then attaches to the session.
///
/// Idempotent on `session_name`: a row already opened for this session
/// anywhere under the same root is activated instead of duplicated. The
/// dialog may have been opened from a project-folder child, so the root is
/// walked up first — sessions belong to the host, not to the row that
/// happened to be right-clicked. Same construction as the two sibling
/// commands: a CLONE of the root's connection, a single terminal layout,
/// no `sort_order` (the sidebar puts nulls last).
#[tauri::command]
fn workspace_open_session(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    session_name: String,
    display_name: String,
    cwd: Option<String>,
) -> Result<WorkspacesFile, String> {
    let session_name = session_name.trim().to_string();
    if session_name.is_empty() {
        return Err("session name is required".to_string());
    }
    let display_name = {
        let t = display_name.trim();
        if t.is_empty() { session_name.clone() } else { t.to_string() }
    };
    let cwd = cwd.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
    let mut created = false;
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        if !file.workspaces.iter().any(|w| w.id == workspace_id) {
            return Err("workspace not found".to_string());
        }
        let root_id = ancestors_of(&file, &workspace_id)
            .last()
            .cloned()
            .unwrap_or_else(|| workspace_id.clone());
        let subtree = collect_subtree_ids(&file, &root_id);
        if let Some(existing) = file
            .workspaces
            .iter()
            .find(|w| {
                subtree.iter().any(|id| id == &w.id)
                    && w.tmux_session.as_deref() == Some(session_name.as_str())
            })
            .map(|w| w.id.clone())
        {
            file.active_workspace_id = Some(existing);
        } else {
            let conn = file
                .workspaces
                .iter()
                .find(|w| w.id == root_id)
                .and_then(|w| w.connection.clone())
                .unwrap_or(Connection::Local { shell: None });
            let parent = pick_session_parent(&file, &root_id, cwd.as_deref());
            let id = new_workspace_id();
            file.workspaces.push(Workspace {
                id: id.clone(),
                name: display_name,
                cwd,
                connection: Some(conn.clone()),
                layout: Some(single_terminal_layout(conn)),
                parent_id: Some(parent),
                tmux_session: Some(session_name.clone()),
                ..Default::default()
            });
            file.active_workspace_id = Some(id);
            created = true;
        }
    }
    persist(&state)?;
    if created {
        // Session names are metadata (the picker already logs them); never
        // the screen behind them (Rule #1).
        log_info(
            "WORKSPACE",
            &format!("workspace_open_session: opened a row for session {session_name} under ws={workspace_id}"),
        );
        let _ = app.emit("workspaces:changed", ());
    }
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Demote a workspace that turned out not to be a git repo.
///
/// The sidebar calls this when a scan comes back with git's
/// "not a git repository". Without it the row keeps its repo affordances
/// and re-scans on every expand and every restart, asking a question
/// whose answer will not change. Clearing the flag makes it an ordinary
/// workspace that still opens panes in that directory — nothing is
/// deleted, and re-pinning is how you undo it.
#[tauri::command]
fn workspace_set_project_root(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    is_project_root: bool,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        if ws.is_project_root == is_project_root {
            return Ok(file.clone());
        }
        ws.is_project_root = is_project_root;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    log_info(
        "WORKSPACE",
        &format!("ws={workspace_id} is_project_root={is_project_root}"),
    );
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Persisted collapse state of a workspace's subtree.
///
/// Deliberately NOT routed through `workspace_set_active`, which stamps
/// `last_active_at` and would make a chevron click look like a visit.
#[tauri::command]
fn workspace_set_collapsed(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    collapsed: bool,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        ws.is_collapsed = collapsed;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 84.A: flip a workspace between the split grid and the tab strip.
///
/// Purely presentational — `layout` is never touched, so flipping to tabs
/// and back restores the exact split tree with its ratios. The frontend
/// derives the tab list from `collect_panes(layout)`; see the `tabs_mode`
/// doc comment in `ymux-types`.
#[tauri::command]
fn workspace_set_tabs_mode(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    tabs_mode: bool,
) -> Result<WorkspacesFile, String> {
    // Clone inside the guarded scope. `workspace_set_collapsed` above
    // re-locks and `unwrap()`s for its return value; that's grandfathered,
    // but Rule #4 forbids a new one.
    let snapshot = {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        ws.tabs_mode = tabs_mode;
        file.clone()
    };
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    log_info("WORKSPACE", &format!("ws={workspace_id} tabs_mode={tabs_mode}"));
    Ok(snapshot)
}

// ─── tree repair + the v2/v3 project-folder migration ───────────────

/// Repair `parent_id` so every consumer may assume a forest.
///
/// Nothing in the UI can produce a bad edge — the tree is written only
/// by the two create commands, and there is no re-parent gesture. These
/// guards exist because workspaces.json is hand-editable and the two
/// consumers that walk parent edges both fail badly on a cycle: the
/// sidebar renderer recurses (a hung UI with no error card and nothing
/// in debug.log — strictly worse than a crash) and the cascade delete
/// walks the same edges.
fn normalize_parents(file: &mut WorkspacesFile) -> usize {
    use std::collections::{HashMap, HashSet};

    let ids: HashSet<String> = file.workspaces.iter().map(|w| w.id.clone()).collect();
    let mut fixed = 0usize;
    for ws in file.workspaces.iter_mut() {
        let bad = match ws.parent_id.as_deref() {
            Some(p) if p == ws.id => Some("parented to itself"),
            Some(p) if !ids.contains(p) => Some("parent no longer exists"),
            _ => None,
        };
        if let Some(why) = bad {
            log_warn(
                "WORKSPACE",
                &format!("load: ws={} {why} → detached to the root list", ws.id),
            );
            ws.parent_id = None;
            fixed += 1;
        }
    }

    // Break cycles one link at a time: walk up from every node with a
    // visited set, and detach the first node that revisits.
    loop {
        let parents: HashMap<String, String> = file
            .workspaces
            .iter()
            .filter_map(|w| w.parent_id.clone().map(|p| (w.id.clone(), p)))
            .collect();
        let mut culprit: Option<String> = None;
        for start in parents.keys() {
            let mut seen: HashSet<String> = HashSet::new();
            let mut cur = start.clone();
            loop {
                if !seen.insert(cur.clone()) {
                    culprit = Some(cur.clone());
                    break;
                }
                match parents.get(&cur) {
                    Some(p) => cur = p.clone(),
                    None => break,
                }
            }
            if culprit.is_some() {
                break;
            }
        }
        match culprit {
            Some(id) => {
                if let Some(w) = file.workspaces.iter_mut().find(|w| w.id == id) {
                    log_warn(
                        "WORKSPACE",
                        &format!("load: ws={id} sits on a parent cycle → detached"),
                    );
                    w.parent_id = None;
                    fixed += 1;
                }
            }
            None => break,
        }
    }
    fixed
}

/// One Terminal pane, the shape every workspace-create path uses.
fn single_terminal_layout(conn: Connection) -> LayoutNode {
    LayoutNode::Pane {
        pane_id: new_pane_id(),
        pane_kind: PaneKind::Terminal,
        connection: Some(conn),
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

/// v2/v3 project folders → the workspace tree. One shot, at load.
///
/// The old shape kept a `project_folders` array at the file root, each
/// entry owning its own `Connection`, and bound its worktree workspaces
/// with a per-workspace `project_folder_id` + `worktree_path`. All three
/// keys are gone from the structs, so serde drops them silently and the
/// next `persist` would erase a pin the user actually made — `save_to_disk`
/// rewrites the whole file from the in-memory struct rather than merging.
///
/// So read them off the raw JSON once and rebuild them as real
/// workspaces. A folder whose host matches no existing workspace is kept
/// as a root rather than discarded: a pin is data the user created.
fn migrate_legacy_project_folders(file: &mut WorkspacesFile, text: &str) -> usize {
    use std::collections::HashMap;

    let root: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let folders = match root.get("project_folders").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return 0,
    };

    // ws id → (legacy project_folder_id, legacy worktree_path)
    let mut legacy: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    if let Some(arr) = root.get("workspaces").and_then(|v| v.as_array()) {
        for w in arr {
            let id = match w.get("id").and_then(|v| v.as_str()) {
                Some(i) => i.to_string(),
                None => continue,
            };
            legacy.insert(
                id,
                (
                    w.get("project_folder_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    w.get("worktree_path")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                ),
            );
        }
    }

    let mut created = 0usize;
    for f in folders {
        let path = match f.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => continue,
        };
        let folder_id = f
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let name = match f.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => path_basename(&path),
        };
        let conn: Option<Connection> = f
            .get("connection")
            .cloned()
            .and_then(|c| serde_json::from_value(c).ok());
        let is_collapsed = f
            .get("is_collapsed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // The parent is the first ROOT workspace on the same host that
        // is not itself one of this folder's worktree workspaces —
        // without that second condition a worktree could end up as the
        // parent of the folder it belongs to.
        let parent_id = file
            .workspaces
            .iter()
            .find(|w| {
                w.parent_id.is_none()
                    && !legacy
                        .get(&w.id)
                        .map(|(pf, _)| pf.is_some())
                        .unwrap_or(false)
                    && conn_same_host(&w.connection, &conn)
            })
            .map(|w| w.id.clone());

        let new_id = new_workspace_id();
        let mut adopted = 0usize;
        for ws in file.workspaces.iter_mut() {
            let matches = legacy
                .get(&ws.id)
                .map(|(pf, _)| pf.as_deref() == Some(folder_id.as_str()))
                .unwrap_or(false);
            if !matches || folder_id.is_empty() {
                continue;
            }
            ws.parent_id = Some(new_id.clone());
            // Folder membership and group membership were already
            // mutually exclusive in the old model; keep it that way.
            ws.group_id = None;
            ws.sort_order = None;
            if ws.cwd.as_deref().map(str::trim).unwrap_or("").is_empty() {
                ws.cwd = legacy.get(&ws.id).and_then(|(_, wt)| wt.clone());
            }
            adopted += 1;
        }

        let effective = conn.clone().unwrap_or(Connection::Local { shell: None });
        file.workspaces.push(Workspace {
            id: new_id,
            name: name.clone(),
            cwd: Some(path),
            connection: Some(effective.clone()),
            layout: Some(single_terminal_layout(effective)),
            parent_id: parent_id.clone(),
            is_project_root: true,
            is_collapsed,
            ..Default::default()
        });
        created += 1;
        log_info(
            "WORKSPACE",
            &format!(
                "migrate: project folder {name:?} → workspace (parented={}, {adopted} worktree \
                 workspace(s) adopted)",
                parent_id.is_some()
            ),
        );
    }
    created
}

// ─── cmux-A A2: workspace group commands ────────────────────────────

fn new_group_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("g_{:x}", t)
}

#[tauri::command]
fn workspace_group_create(
    state: State<'_, AppState>,
    app: AppHandle,
    name: String,
    color: String,
) -> Result<WorkspaceGroup, String> {
    let trimmed = name.trim().to_string();
    if trimmed.is_empty() {
        return Err("group name is required".to_string());
    }
    let group = WorkspaceGroup {
        id: new_group_id(),
        name: trimmed,
        color,
        is_collapsed: false,
        // beta.3 (ws-dragdrop): freshly-created groups get no
        // sort_order; they append at the end of the group list. The
        // first `workspace_group_reorder` call renumbers everyone into
        // consecutive 0..N-1.
        sort_order: None,
    };
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        file.groups.push(group.clone());
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(group)
}

#[tauri::command]
fn workspace_group_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: Option<String>,
    color: Option<String>,
    is_collapsed: Option<bool>,
) -> Result<(), String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let g = file
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or_else(|| format!("no group {id}"))?;
        if let Some(n) = name {
            let trimmed = n.trim().to_string();
            if !trimmed.is_empty() {
                g.name = trimmed;
            }
        }
        if let Some(c) = color {
            g.color = c;
        }
        if let Some(collapsed) = is_collapsed {
            g.is_collapsed = collapsed;
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(())
}

#[tauri::command]
fn workspace_group_delete(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<(), String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        // Move any workspaces in this group back to ungrouped before
        // dropping the group itself — no orphaned group_id references.
        for ws in file.workspaces.iter_mut() {
            if ws.group_id.as_deref() == Some(id.as_str()) {
                ws.group_id = None;
            }
        }
        file.groups.retain(|g| g.id != id);
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(())
}

#[tauri::command]
fn workspace_set_group(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    group_id: Option<String>,
) -> Result<(), String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        // Validate that group_id, if set, exists — otherwise the
        // workspace would be assigned to a dangling group.
        if let Some(gid) = group_id.as_deref() {
            if !file.groups.iter().any(|g| g.id == gid) {
                return Err(format!("no group {gid}"));
            }
        }
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        ws.group_id = group_id;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(())
}

// beta.3 (ws-dragdrop): direct drag-reorder of workspaces in the
// sidebar. `group_id` is the destination scope (None = Ungrouped); if
// the workspace was in a different scope before, both scopes get
// renumbered so they stay dense. `new_index` is clamped to the
// destination scope's valid range (0..=N where N is the size AFTER the
// move, so appending at the end works). Returns the full
// `WorkspacesFile` so the frontend can drop its old snapshot without a
// second round-trip.
#[tauri::command]
fn workspace_reorder(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    group_id: Option<String>,
    new_index: i32,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;

        // Snapshot the workspace's scope. A CHILD is scoped to its
        // parent, not to a group: it has no group, and renumbering it
        // against a group's members would order it against workspaces
        // it is not a sibling of. The frontend only ever offers
        // same-level drops, so `group_id` is simply ignored here.
        let (old_group, parent_id, ws_index) = {
            let (idx, ws) = file
                .workspaces
                .iter()
                .enumerate()
                .find(|(_, w)| w.id == workspace_id)
                .ok_or_else(|| format!("no workspace {workspace_id}"))?;
            (ws.group_id.clone(), ws.parent_id.clone(), idx)
        };
        let group_id = if parent_id.is_some() { old_group.clone() } else { group_id };

        // Validate destination group_id if provided.
        if let Some(gid) = group_id.as_deref() {
            if !file.groups.iter().any(|g| g.id == gid) {
                return Err(format!("no group {gid}"));
            }
        }

        // Reassign group_id first — that changes the workspace's scope
        // membership, which the renumber pass below relies on.
        if old_group.as_deref() != group_id.as_deref() {
            file.workspaces[ws_index].group_id = group_id.clone();
        }

        // Compute the destination scope's ordered id list AFTER the
        // move (skip the target if it was already there, then splice
        // at new_index). Assign 0..N-1 by walking that list.
        let dest_scope = group_id.as_deref();
        let in_scope = |w: &Workspace| match parent_id.as_deref() {
            Some(pid) => w.parent_id.as_deref() == Some(pid),
            None => w.parent_id.is_none() && w.group_id.as_deref() == dest_scope,
        };
        let mut dest_ids: Vec<String> = file
            .workspaces
            .iter()
            .filter(|w| in_scope(w) && w.id != workspace_id)
            .map(|w| {
                // Sort by current sort_order (missing = end), tie-break
                // by insertion; the collect step below reorders.
                w.id.clone()
            })
            .collect();
        // Stable sort dest_ids by the sibling's current sort_order so
        // "insert at new_index" is meaningful against the on-screen
        // ordering, not against arbitrary vec order.
        {
            // Build a lookup: id -> (sort_order or +∞, insertion idx).
            let mut key: std::collections::HashMap<&str, (i32, usize)> =
                std::collections::HashMap::new();
            for (idx, w) in file.workspaces.iter().enumerate() {
                key.insert(w.id.as_str(), (w.sort_order.unwrap_or(i32::MAX), idx));
            }
            dest_ids.sort_by(|a, b| {
                let ka = key.get(a.as_str()).copied().unwrap_or((i32::MAX, 0));
                let kb = key.get(b.as_str()).copied().unwrap_or((i32::MAX, 0));
                ka.cmp(&kb)
            });
        }
        let insert_at = (new_index.max(0) as usize).min(dest_ids.len());
        dest_ids.insert(insert_at, workspace_id.clone());

        // Write consecutive 0..N-1 keys into the destination scope,
        // matching dest_ids' new order.
        for (new_key, id) in dest_ids.into_iter().enumerate() {
            if let Some(w) = file.workspaces.iter_mut().find(|w| w.id == id) {
                w.sort_order = Some(new_key as i32);
            }
        }

        // If the source scope was different, renumber it too — the
        // hole left by the moved workspace collapses to 0..M-1.
        if old_group.as_deref() != group_id.as_deref() {
            renumber_workspace_scope(&mut file, old_group.as_deref());
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state
        .workspaces
        .lock()
        .map_err(|e| format!("workspaces lock poisoned: {e}"))?
        .clone())
}

// beta.3 (ws-dragdrop): drag-reorder a group among its siblings. Same
// clamp/renumber pattern as `workspace_reorder` but with a single
// scope (the whole group list).
#[tauri::command]
fn workspace_group_reorder(
    state: State<'_, AppState>,
    app: AppHandle,
    group_id: String,
    new_index: i32,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;

        if !file.groups.iter().any(|g| g.id == group_id) {
            return Err(format!("no group {group_id}"));
        }

        // Build the ordered id list minus the target, then splice.
        let mut ordered: Vec<String> = file
            .groups
            .iter()
            .filter(|g| g.id != group_id)
            .map(|g| g.id.clone())
            .collect();
        {
            let mut key: std::collections::HashMap<&str, (i32, usize)> =
                std::collections::HashMap::new();
            for (idx, g) in file.groups.iter().enumerate() {
                key.insert(g.id.as_str(), (g.sort_order.unwrap_or(i32::MAX), idx));
            }
            ordered.sort_by(|a, b| {
                let ka = key.get(a.as_str()).copied().unwrap_or((i32::MAX, 0));
                let kb = key.get(b.as_str()).copied().unwrap_or((i32::MAX, 0));
                ka.cmp(&kb)
            });
        }
        let insert_at = (new_index.max(0) as usize).min(ordered.len());
        ordered.insert(insert_at, group_id.clone());

        for (new_key, id) in ordered.into_iter().enumerate() {
            if let Some(g) = file.groups.iter_mut().find(|g| g.id == id) {
                g.sort_order = Some(new_key as i32);
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state
        .workspaces
        .lock()
        .map_err(|e| format!("workspaces lock poisoned: {e}"))?
        .clone())
}

// beta.3 (pane-dragdrop): swap the layout positions of two panes
// inside a workspace. Called by paneDrag.ts on pointerup after the
// user drops a pane on another pane.
//
// The whole `LayoutNode::Pane { .. }` node (including its `pane_id`,
// connection, browser state, title, colour, everything) is moved as
// a unit — so PTY sessions keyed by pane_id keep their state and
// there's no PTY kill/respawn (Rule #1). The frontend's PaneView has
// a createEffect on p.pane.pane_id: when a slot's pane_id changes
// after the swap, it detaches the previous xterm container and
// attaches the new one from the g_terminals registry, so xterm
// scrollback + connection state survive the reorder untouched.
//
// Same-pane no-ops early. Missing panes → clean error to the
// frontend (Rule #6). workspaces.json write goes through `persist`
// (atomic tmp+rename, Rule #7).
fn take_pane_from_layout(
    node: &mut LayoutNode,
    target: &str,
    replacement: LayoutNode,
) -> Result<LayoutNode, LayoutNode> {
    // Returns Ok(the extracted pane node) on hit, Err(replacement) on
    // miss so the caller can hand the same replacement to the next
    // sibling without cloning it every level. The tree is a binary
    // split-or-leaf shape, so depth stays shallow (<= number of panes).
    match node {
        LayoutNode::Pane { pane_id, .. } if pane_id == target => {
            Ok(std::mem::replace(node, replacement))
        }
        LayoutNode::Pane { .. } => Err(replacement),
        LayoutNode::Split { first, second, .. } => {
            match take_pane_from_layout(first.as_mut(), target, replacement) {
                Ok(v) => Ok(v),
                Err(replacement) => {
                    take_pane_from_layout(second.as_mut(), target, replacement)
                }
            }
        }
    }
}

fn make_swap_placeholder_pane(pane_id: String) -> LayoutNode {
    LayoutNode::Pane {
        pane_id,
        pane_kind: PaneKind::default(),
        connection: None,
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

fn swap_two_panes_in_layout(
    layout: &mut LayoutNode,
    pane_a_id: &str,
    pane_b_id: &str,
) -> Result<(), String> {
    if pane_a_id == pane_b_id {
        return Ok(());
    }
    // Marker id is guaranteed not to collide with any real pane_id
    // (pane_ids are UUIDs). The marker never persists — it's replaced
    // in step 3 below, and if step 2 or 3 fails, the caller wraps the
    // Result and rejects the whole mutation (frontend won't see it).
    let marker = format!("__ymux_swap_placeholder__{pane_a_id}");
    let placeholder = make_swap_placeholder_pane(marker.clone());
    // Step 1: take A out, leave placeholder in A's slot.
    let pane_a = take_pane_from_layout(layout, pane_a_id, placeholder)
        .map_err(|_| format!("no pane {pane_a_id} in workspace layout"))?;
    // Step 2: take B out, drop pane_a into B's slot. Now the tree has
    // pane_a where B was, and the marker placeholder where A was.
    let pane_b = match take_pane_from_layout(layout, pane_b_id, pane_a) {
        Ok(v) => v,
        Err(pane_a) => {
            // B wasn't found — put pane_a back into A's slot to
            // undo step 1's mutation, so the layout is unchanged on
            // error. The caller sees a clean Err.
            let _ = take_pane_from_layout(layout, &marker, pane_a);
            return Err(format!("no pane {pane_b_id} in workspace layout"));
        }
    };
    // Step 3: replace the marker with pane_b.
    take_pane_from_layout(layout, &marker, pane_b).map_err(|_| {
        "internal: swap placeholder not found on final pass".to_string()
    })?;
    Ok(())
}

#[tauri::command]
fn workspace_swap_panes(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_a_id: String,
    pane_b_id: String,
) -> Result<WorkspacesFile, String> {
    if pane_a_id == pane_b_id {
        // No-op swap: return the current file unchanged so the
        // frontend doesn't do a wasted state update.
        return Ok(state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?
            .clone());
    }
    {
        let mut file = state
            .workspaces
            .lock()
            .map_err(|e| format!("workspaces lock poisoned: {e}"))?;
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_mut()
            .ok_or_else(|| format!("workspace {workspace_id} has no layout"))?;
        swap_two_panes_in_layout(layout, &pane_a_id, &pane_b_id)?;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state
        .workspaces
        .lock()
        .map_err(|e| format!("workspaces lock poisoned: {e}"))?
        .clone())
}

#[tauri::command]
fn workspace_rename(
    state: State<'_, AppState>,
    workspace_id: String,
    name: String,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.name = name;
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

// Phase 30: dedicated identity command for live preview. The full
// `workspace_update` path is still used by the modal's Save button;
// this one lets a swatch click instant-save without rebuilding the
// whole field set. Validates: hex must be `#rrggbb`, emoji must be
// <= 16 UTF-8 bytes. Returns the updated workspace.
#[tauri::command]
async fn workspace_set_identity(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    color: Option<String>,
    emoji: Option<String>,
) -> Result<Workspace, String> {
    if let Some(c) = color.as_deref() {
        let bytes = c.as_bytes();
        let ok = bytes.len() == 7
            && bytes[0] == b'#'
            && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(format!("invalid color (want #rrggbb, got {c:?})"));
        }
    }
    if let Some(e) = emoji.as_deref() {
        if e.len() > 16 {
            return Err(format!("emoji too long ({} bytes, max 16)", e.len()));
        }
    }
    let updated: Workspace;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        ws.color = color;
        ws.emoji = emoji;
        updated = ws.clone();
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

// Phase 36 (#2.2): toggle auto port forwarding for a workspace.
// Persists the flag. When turned off, also tears down any forwards the
// watcher already opened for this workspace (the watcher keeps running
// remotely but its events are ignored — see the dispatch arms).
#[tauri::command]
async fn workspace_set_auto_port_forward(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    enabled: bool,
) -> Result<Workspace, String> {
    let updated: Workspace;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        ws.auto_port_forward = enabled;
        updated = ws.clone();
    }
    if !enabled {
        // Phase 47: turning detection off should ACTUALLY stop the
        // watcher (not just suppress events) and wipe what we've seen.
        clear_workspace_detection(&state, &app, &workspace_id);
        close_workspace_forwards(&state.core.forwards, &workspace_id);
    } else {
        // Phase 47: turning detection on while a session is already up
        // should start the watcher immediately. Best-effort: no-op if
        // no pane-backed SSH session has set up a tunnel yet.
        try_ensure_port_watcher(&state, &workspace_id).await;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

/// Phase 78: flip the per-workspace "uses a different Claude account" flag.
/// When set, the Claude-usage indicator fetches this workspace's own
/// subscription usage instead of reusing the global (single-account) value.
#[tauri::command]
async fn workspace_set_claude_separate_account(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    enabled: bool,
) -> Result<Workspace, String> {
    let updated: Workspace;
    {
        let mut file = state.workspaces.lock().map_err(|e| e.to_string())?;
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        ws.claude_separate_account = enabled;
        updated = ws.clone();
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

/// Phase 47: try to start the workspace's port-watcher. Best-effort —
/// returns silently (with a dlog) when no pane-backed SSH session has
/// set up a reverse tunnel yet (headless connect from Phase 41 doesn't
/// open one). spawn_ssh's own watcher launch will pick up later when a
/// terminal pane connects. Used by the activation effect, the toggle,
/// and the explicit `workspace_ensure_port_watcher` command.
async fn try_ensure_port_watcher(state: &AppState, workspace_id: &str) {
    // Phase 80: port, token and owning session come from ONE registration.
    // This used to read the port out of a HashSet with `.iter().next()` (an
    // arbitrary element, in practice often an hour-old one), the token out
    // of a separate map that nothing ever cleared, and the handle out of
    // "first session that matches the workspace" — three independently
    // stale sources whose combination produced `-DENIED bad-mac`.
    let reg = match state.tunnel_registry.current(workspace_id) {
        Some(r) => r,
        None => {
            log_debug("TUNNEL", &format!(
                "ensure_port_watcher[{workspace_id}]: no live reverse tunnel — skip"
            ));
            return;
        }
    };
    let handle = {
        let sessions = match state.core.sessions.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        match sessions.get(&reg.session_id) {
            Some(Session::Ssh(ssh)) => Some(Arc::clone(&ssh.handle)),
            _ => None,
        }
    };
    let handle = match handle {
        Some(h) => h,
        None => {
            // The session that owned this forward is gone; so is the forward.
            state
                .tunnel_registry
                .unregister(workspace_id, &reg.session_id);
            log_debug("TUNNEL", &format!(
                "ensure_port_watcher[{workspace_id}]: owning session {} is gone — registration dropped",
                reg.session_id
            ));
            return;
        }
    };
    let _ = spawn_port_watcher(state, &handle, workspace_id, reg.port, &reg.token).await;
}

/// Phase 47: explicit command — frontend calls this on workspace
/// activation (when detection is on) to make sure a watcher is up.
/// Idempotent via spawn_port_watcher's dedup. Always Ok.
#[tauri::command]
async fn workspace_ensure_port_watcher(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    try_ensure_port_watcher(&state, &workspace_id).await;
    Ok(())
}

/// Phase 47: serializable shape for the snapshot endpoint.
#[derive(Clone, Serialize)]
pub(crate) struct DetectedPortInfo {
    pub remote_port: u16,
    pub addr: String,
    pub family: String,
}

/// Phase 47: snapshot the workspace's current detected_ports. Frontend
/// calls this on workspace switch to populate PortsWindow from state —
/// events alone aren't enough because they only fire while the FE was
/// already listening with the right workspace_id.
#[tauri::command]
async fn list_detected_ports(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<DetectedPortInfo>, String> {
    let m = state.core.detected_ports.lock().unwrap();
    let mut out: Vec<DetectedPortInfo> = m
        .get(&workspace_id)
        .map(|ports| {
            ports
                .iter()
                .map(|(port, (addr, family))| DetectedPortInfo {
                    remote_port: *port,
                    addr: addr.clone(),
                    family: family.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|d| d.remote_port);
    Ok(out)
}

// Phase 36 (#2.2): manually stop one forward (Ports panel "Stop
// forward" menu item).
#[tauri::command]
async fn port_forward_stop(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    remote_port: u16,
) -> Result<(), String> {
    close_one_forward(&state, &app, &workspace_id, remote_port);
    Ok(())
}

// Phase 46: open a forward on demand — driven by a user click on a
// detected port in PortsWindow. The watcher only detects now; this
// command is what actually opens the tunnel. Looks up the remote
// bind addr from `detected_ports` (falls back to "127.0.0.1" if
// missing) and hands off to `open_auto_forward`, which now runs a
// TCP sanity probe before reporting success. Idempotent — returns
// the existing local port if a forward already exists.
#[tauri::command]
async fn forward_port_start(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    remote_port: u16,
) -> Result<u16, String> {
    let addr = {
        let m = state.core.detected_ports.lock().unwrap();
        m.get(&workspace_id)
            .and_then(|ports| ports.get(&remote_port))
            .map(|(addr, _family)| addr.clone())
            .unwrap_or_else(|| "127.0.0.1".to_string())
    };
    open_auto_forward(&state, &app, &workspace_id, &addr, remote_port).await
}

// Phase 46: TCP sanity probe used by `open_auto_forward` to verify
// that a freshly-bound listener is actually reachable on 127.0.0.1
// before telling the FE the forward is live. Returns Ok if a
// connection succeeded within the timeout, Err with a reason
// otherwise. Pulled out as a free function so it's straightforward
// to unit-test against a known-good (just-bound) listener and a
// known-bad (vacant) port. Caller drops the returned stream — we
// only need to know that connect() succeeded.
#[cfg(test)]
pub(crate) async fn tcp_probe(
    target: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(target)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("connect failed: {e}")),
        Err(_) => Err(format!("timeout after {}ms", timeout.as_millis())),
    }
}

// Phase 31: per-pane identity. Same validation as the workspace
// command. Walks the workspace's layout to find the matching pane and
// updates its color/emoji fields. Returns a serializable snapshot of
// the pane after the update so the frontend can refresh its local
// state without waiting for the `workspaces:changed` round-trip.
#[derive(Clone, Serialize)]
pub(crate) struct PaneIdentity {
    pub(crate) pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) emoji: Option<String>,
}

fn set_pane_identity_in_layout(
    node: &mut LayoutNode,
    target: &str,
    new_color: &Option<String>,
    new_emoji: &Option<String>,
) -> Option<PaneIdentity> {
    match node {
        LayoutNode::Pane {
            pane_id,
            color,
            emoji,
            ..
        } if pane_id == target => {
            *color = new_color.clone();
            *emoji = new_emoji.clone();
            Some(PaneIdentity {
                pane_id: pane_id.clone(),
                color: color.clone(),
                emoji: emoji.clone(),
            })
        }
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            set_pane_identity_in_layout(first, target, new_color, new_emoji)
                .or_else(|| set_pane_identity_in_layout(second, target, new_color, new_emoji))
        }
    }
}

#[tauri::command]
async fn pane_set_identity(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    color: Option<String>,
    emoji: Option<String>,
) -> Result<PaneIdentity, String> {
    if let Some(c) = color.as_deref() {
        let bytes = c.as_bytes();
        let ok = bytes.len() == 7
            && bytes[0] == b'#'
            && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(format!("invalid color (want #rrggbb, got {c:?})"));
        }
    }
    if let Some(e) = emoji.as_deref() {
        if e.len() > 16 {
            return Err(format!("emoji too long ({} bytes, max 16)", e.len()));
        }
    }
    let updated: PaneIdentity;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_mut()
            .ok_or_else(|| format!("workspace {workspace_id} has no layout"))?;
        updated = set_pane_identity_in_layout(layout, &pane_id, &color, &emoji)
            .ok_or_else(|| format!("no pane {pane_id} in workspace {workspace_id}"))?;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

#[tauri::command]
fn workspace_delete(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspacesFile, String> {
    // A workspace now owns its subtree: pinned project folders and the
    // worktree workspaces opened under them go with it. The directories
    // on the host are never touched — but the SESSIONS are, which is why
    // the frontend confirm names what is about to die.
    //
    // Collect ids and pane lists under ONE short lock, then drop it: the
    // teardown below is I/O (PTY kill, webview close, filesystem) and
    // must never run under the workspaces mutex.
    let (ids, panes_by_ws) = {
        let file = state.workspaces.lock().unwrap();
        let ids = collect_subtree_ids(&file, &workspace_id);
        let panes: Vec<(String, Vec<String>)> = ids
            .iter()
            .map(|id| {
                let panes = file
                    .workspaces
                    .iter()
                    .find(|w| &w.id == id)
                    .and_then(|w| w.layout.as_ref())
                    .map(|l| {
                        let mut v = Vec::new();
                        collect_panes(l, &mut v);
                        v
                    })
                    .unwrap_or_default();
                (id.clone(), panes)
            })
            .collect();
        (ids, panes)
    };
    if ids.len() > 1 {
        log_info(
            "WORKSPACE",
            &format!(
                "delete ws={workspace_id} cascading to {} descendant workspace(s)",
                ids.len() - 1
            ),
        );
    }

    for (id, panes) in &panes_by_ws {
        teardown_workspace_runtime(&state, &app, id, panes);
    }

    // One retain, one reassignment, one persist. Doing this per id would
    // leave the tree half-removed if a later teardown panicked.
    {
        let mut file = state.workspaces.lock().unwrap();
        file.workspaces.retain(|w| !ids.contains(&w.id));
        if file
            .active_workspace_id
            .as_deref()
            .map(|a| ids.iter().any(|i| i == a))
            .unwrap_or(false)
        {
            // Prefer a root: falling back to `first()` could land the
            // user inside some unrelated repo's worktree.
            file.active_workspace_id = file
                .workspaces
                .iter()
                .find(|w| w.parent_id.is_none())
                .or_else(|| file.workspaces.first())
                .map(|w| w.id.clone());
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Every id in the subtree rooted at `workspace_id`, including itself.
///
/// BFS with a visited set: `normalize_parents` guarantees a forest at
/// load, but a delete that looped would take the whole file with it, so
/// this does not rely on that guarantee.
fn collect_subtree_ids(file: &WorkspacesFile, workspace_id: &str) -> Vec<String> {
    let mut out = vec![workspace_id.to_string()];
    let mut seen: std::collections::HashSet<String> =
        std::iter::once(workspace_id.to_string()).collect();
    let mut queue = vec![workspace_id.to_string()];
    while let Some(cur) = queue.pop() {
        for w in file.workspaces.iter() {
            if w.parent_id.as_deref() == Some(cur.as_str()) && seen.insert(w.id.clone()) {
                out.push(w.id.clone());
                queue.push(w.id.clone());
            }
        }
    }
    out
}

/// Runtime teardown for one workspace: everything except removing it
/// from the file. Split out of `workspace_delete` so the cascade runs
/// the identical sequence per id.
fn teardown_workspace_runtime(
    state: &State<'_, AppState>,
    app: &AppHandle,
    workspace_id: &str,
    panes_to_kill: &[String],
) {
    // Phase 53 (rebased): drop the workspace-level Browser Webview
    // (at most one per workspace, keyed by workspace_id) and delete
    // the per-workspace browser-sessions directory (cookies /
    // localStorage / cache). Sessions DO survive transient hide/show
    // cycles; this is the only cleanup path that should wipe them.
    let webview = state.workspace_browsers.lock().unwrap().remove(workspace_id);
    if let Some(w) = webview {
        let _ = w.close();
    }
    // Phase 85.C: and the OS window it may have been popped out into,
    // which would otherwise outlive the workspace it belongs to. Its
    // `Destroyed` handler does the rest of the cleanup.
    workspace_browser::close_popout_window(app, workspace_id);
    workspace_browser::cleanup_workspace_sessions(workspace_id);
    // Drop the CLI-alignment verdict with the workspace it described.
    // Deliberately NOT dropped on mere disconnect: an unresolved skew should
    // outlive the connection, so the features it gates stay off until a
    // bootstrap actually proves the remote binary matches.
    //
    // Merge note: this arrived on main while the delete was being turned
    // into a cascade. It lives in the per-workspace teardown, so it now
    // runs for every descendant rather than only the clicked row — which
    // is what it wanted in the first place.
    state.bootstrap_guard.forget(workspace_id);
    // Phase 80: and its reverse-tunnel state, sticky port included. Only
    // here, never on a mere disconnect — the whole value of the sticky port
    // is that it outlives the connection that held it.
    state.tunnel_registry.forget_workspace(workspace_id);
    // Phase 86: abort its port-watcher (this path never did — the task and
    // the `port_watchers` slot leaked past delete) and leave the host group.
    clear_workspace_detection(state, app, workspace_id);
    for pane_id in panes_to_kill {
        if let Some(sid) = state.core.pane_sessions.lock().unwrap().remove(pane_id) {
            if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
                kill_session_inner(&mut s);
            }
        }
    }
    // Phase 8.B: tear down any port forwards for the workspace.
    close_workspace_forwards(&state.core.forwards, workspace_id);
    // Phase 39: drop the workspace's notes (the UI warns first when any
    // exist). Best-effort — failure here shouldn't block the delete.
    notes::delete_for_workspace(state, app, workspace_id);
}

#[tauri::command]
fn workspace_set_active(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        file.active_workspace_id = workspace_id.clone();
        // Phase 49-C: stamp the activation timestamp on the workspace
        // being activated so the auto-destroy sweep can age it correctly.
        if let Some(id) = workspace_id.as_ref() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == *id) {
                ws.last_active_at = now;
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

// Phase 49-B: anchor a workspace to a fresh git worktree.
//
// Runs `git worktree add <root>/<workspace_id>-<branch> -b <branch>
// <base>` from the workspace's cwd, then rewrites the workspace's cwd
// (and stamps `git_worktree`) so future panes spawn inside the worktree.
// Only valid for Local workspaces with an existing cwd that is itself
// a git repo. <root> defaults to `<config_dir>/worktrees`.
//
// Branch and base names are passed as standalone args to Command::new
// (no shell concatenation, per Absolute Rule #3). Branch name is also
// validated against an allow-list to keep it filesystem-safe.
#[tauri::command]
fn workspace_create_worktree(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    branch_name: String,
    base_branch: String,
) -> Result<WorkspacesFile, String> {
    // Sanitize the branch name for filesystem use. git itself allows
    // a wider set, but we own the directory naming so we constrain it.
    // Shared with the project-folder path so there is one rule.
    let safe_branch = worktrees::sanitize_branch_name(&branch_name)?;
    if base_branch.trim().is_empty() {
        return Err("base branch is required".to_string());
    }
    // Snapshot the source cwd while holding the lock briefly.
    let src_cwd = {
        let file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        match ws.connection {
            Some(Connection::Local { .. }) | None => {}
            _ => return Err("worktrees only apply to local workspaces".to_string()),
        }
        ws.cwd
            .clone()
            .ok_or_else(|| "workspace has no cwd to anchor a worktree to".to_string())?
    };
    let src_path = PathBuf::from(&src_cwd);
    if !src_path.join(".git").exists() {
        // .git can be a dir (regular repo) or file (submodule / worktree).
        return Err(format!("{src_cwd} is not a git repository"));
    }
    // Replace forward slashes in the branch with hyphens for the
    // directory name so feature/foo doesn't create nested dirs.
    let dir_branch = worktrees::branch_dir_component(&safe_branch);
    let root = config_dir()?.join("worktrees");
    std::fs::create_dir_all(&root).map_err(|e| format!("create worktrees root: {e}"))?;
    let target = root.join(format!("{workspace_id}-{dir_branch}"));
    if target.exists() {
        return Err(format!("target already exists: {}", target.display()));
    }
    let out = std::process::Command::new("git")
        .arg("worktree")
        .arg("add")
        .arg(&target)
        .arg("-b")
        .arg(&branch_name)
        .arg(&base_branch)
        .current_dir(&src_path)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git worktree add failed: {}", stderr.trim()));
    }
    // Stamp the workspace and re-anchor its cwd to the new worktree.
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.cwd = Some(target.to_string_lossy().into_owned());
            ws.git_worktree = Some(target.clone());
        }
    }
    persist(&state)?;
    log_info("WORKSPACE", &format!(
        "[worktree] created {} for ws={} branch={}",
        target.display(),
        workspace_id,
        branch_name,
    ));
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_split(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
    direction: SplitDirection,
    // Phase 8.A: kind defaults to Terminal (back-compat). Browser also accepts a
    // starting URL — falls back to about:blank if absent.
    pane_kind: Option<PaneKind>,
    browser_url: Option<String>,
    // Phase 33: optional help-topic seed; used when pane_kind = Help.
    // None means "let split_pane_in pick the default topic".
    help_topic: Option<String>,
) -> Result<WorkspacesFile, String> {
    let kind = pane_kind.unwrap_or(PaneKind::Terminal);
    // Phase 23.C: when the new pane will be a Terminal, derive a
    // fallback connection BEFORE we mutate the layout. Three-tier
    // lookup:
    //   1. The source pane's own connection (handled inside split_pane_in).
    //   2. Any other terminal pane in this workspace.
    //   3. A live SSH session bound to this workspace (FileManager /
    //      Browser pane may be keeping it alive even when no terminal
    //      pane remains).
    // This fixes the bug where splitting from a FileManager/Browser
    // pane fell back to Local cmd instead of the workspace's SSH
    // connection.
    let fallback_conn: Option<Connection> = if matches!(kind, PaneKind::Terminal) {
        // Phase 23.D: four-tier fallback chain for the new pane's
        // connection — the workspace-level `connection` is now the
        // canonical truth, with the others as belt-and-suspenders
        // for older JSON / mid-session edge cases.
        //   1. first Terminal pane's connection in the layout
        //   2. workspace.connection (canonical)
        //   3. live SSH session bound to the workspace
        //   4. Local (only if all of the above are absent)
        let (layout_fallback, ws_conn) = {
            let file = state.workspaces.lock().unwrap();
            let ws = file.workspaces.iter().find(|w| w.id == workspace_id);
            (
                ws.and_then(|w| w.layout.as_ref().and_then(first_terminal_connection)),
                ws.and_then(|w| w.connection.clone()),
            )
        };
        layout_fallback
            .or(ws_conn)
            .or_else(|| live_ssh_connection_for_workspace(&state, &workspace_id))
    } else {
        None
    };
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                let (new_layout, _) = split_pane_in(
                    layout,
                    &pane_id,
                    direction,
                    kind,
                    browser_url,
                    fallback_conn,
                    help_topic,
                );
                ws.layout = Some(new_layout);
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Phase 8.A: browser-pane commands ───────────────────────────────────────

/// Unified frontend log sink: writes a `[UI:TAG]` line to debug.log through
/// the leveled logger AND pushes into the dev ring buffer so `ymux dev
/// console-tail` keeps working. The frontend logger filters client-side too,
/// but this gate is authoritative (popouts that never load settings still
/// behave). Replaces the old `diag_log` + `dev_console_log` pair.
#[tauri::command]
fn ui_log(
    state: State<'_, AppState>,
    level: String,
    tag: String,
    message: String,
) -> Result<(), String> {
    let lvl = ymux_core::LogLevel::from_str(&level);
    ymux_core::log_at(lvl, &format!("UI:{tag}"), &message);
    dev::push_console(
        &state.console_buffer,
        dev::ConsoleEntry {
            level: lvl.as_str().to_string(),
            message: format!("[{}] {message}", tag.to_uppercase()),
            ts: chrono::Utc::now().timestamp_millis(),
        },
    );
    Ok(())
}

#[tauri::command]
fn workspace_close_pane(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
) -> Result<WorkspacesFile, String> {
    let removed_pane: Option<String>;
    // Phase 23.B: capture whether the workspace still has any
    // SSH-consuming non-terminal panes AFTER the close. If yes, we
    // must keep the SSH handle alive even though the terminal pane
    // is gone — the file-manager / browser uses
    // `pick_ssh_handle_for_workspace` which scans the live sessions
    // for one matching the workspace_id. Killing the SSH session
    // here would leave those panes dead with no UI to reconnect.
    let keep_ssh_alive: bool;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .take()
            .ok_or_else(|| "no layout".to_string())?;
        let (new_root, removed) = close_pane_in(layout, &pane_id);
        keep_ssh_alive = new_root
            .as_ref()
            .map(layout_has_ssh_consumer_pane)
            .unwrap_or(false);
        ws.layout = new_root;
        removed_pane = removed;
    }
    if let Some(pid) = removed_pane.as_ref() {
        // Phase 50: stop any diff-pane watcher bound to the removed
        // pane. Idempotent — no-op for non-Diff panes.
        diff_pane::stop_watcher(&state, pid);
    }
    if let Some(pid) = removed_pane {
        // Always unbind the pane from its session — the pane is gone.
        let sid_opt = state.core.pane_sessions.lock().unwrap().remove(&pid);
        if let Some(sid) = sid_opt {
            // Decide whether to actually drop the session. If the
            // session is SSH AND the workspace still has a consumer
            // (file-manager / browser pane), keep it alive so those
            // panes stay functional. Otherwise drop and clean up.
            let is_ssh_for_workspace = {
                let sessions = state.core.sessions.lock().unwrap();
                matches!(
                    sessions.get(&sid),
                    Some(Session::Ssh(ssh)) if ssh.workspace_id == workspace_id
                )
            };
            if is_ssh_for_workspace && keep_ssh_alive {
                tracing::info!(
                    "workspace_close_pane: keeping SSH session {sid} alive — workspace {workspace_id} still has FileManager/Browser pane(s)"
                );
                // Leave the session in state.core.sessions; it has no pane
                // binding now but `pick_ssh_handle_for_workspace`
                // will still find it via its workspace_id.
            } else if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
                // Closing a pane is not killing its session. `kill_session_inner`
                // is a detach on every backend, so a persistent session OUTLIVES
                // the pane — deliberately, and symmetrically with SSH and WSL,
                // where making the X destructive would be a nasty surprise.
                //
                // But the pane id is retired, so nothing will ever re-attach
                // automatically and the restore hint is pruned with the pane.
                // The session is still reachable — it is listed by name in the
                // Connect picker — and this is the only line that records the
                // name anywhere, so "where did my session go" has a trail.
                if let Session::Local(l) = &s {
                    if let (Some(name), None) = (&l.tmux_session, &l.wsl_distro) {
                        log_info(
                            "PTY",
                            &format!(
                                "workspace_close_pane: pane {pid} closed; its zellij session \
                                 '{name}' keeps running and can be re-attached from the \
                                 Connect picker"
                            ),
                        );
                    }
                }
                kill_session_inner(&mut s);
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_set_split_ratio(
    state: State<'_, AppState>,
    workspace_id: String,
    split_id: String,
    ratio: f32,
) -> Result<(), String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(set_split_ratio_in(layout, &split_id, ratio));
            }
        }
    }
    persist(&state)?;
    Ok(())
}

/// Phase 55-B: walk the workspace's layout tree and reset every
/// internal split's ratio to 0.5. Since the tree is binary, "1/N"
/// per the spec reduces to 0.5 per split — and that's also the
/// default new splits already get from split_pane_in, so a
/// distribute-evenly call effectively undoes every drag the user
/// has done on the dividers. Returns the updated WorkspacesFile so
/// the frontend can replace its local snapshot atomically.
fn reset_all_split_ratios(node: &mut LayoutNode) -> usize {
    match node {
        LayoutNode::Pane { .. } => 0,
        LayoutNode::Split {
            first,
            second,
            ratio,
            ..
        } => {
            *ratio = 0.5;
            1 + reset_all_split_ratios(first) + reset_all_split_ratios(second)
        }
    }
}

#[tauri::command]
fn workspace_distribute_evenly(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspacesFile, String> {
    let count;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        count = ws
            .layout
            .as_mut()
            .map(reset_all_split_ratios)
            .unwrap_or(0);
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    log_debug("WORKSPACE", &format!(
        "workspace_distribute_evenly: ws={workspace_id} reset {count} split(s)"
    ));
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Pane metadata (title / annotation) ─────────────────────────────────────

// Phase 52 (BiDi 33B): toggle the opt-in PTY-stream bidi filter on the
// given pane. Persists the bool onto the pane node (so the toggle
// survives reloads) AND updates the runtime filter map so the very
// next chunk is filtered (or not).
fn set_pane_smart_bidi_in_layout(node: &mut LayoutNode, target: &str, enabled: bool) -> bool {
    match node {
        LayoutNode::Pane {
            pane_id,
            smart_bidi,
            ..
        } if pane_id == target => {
            *smart_bidi = Some(enabled);
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            set_pane_smart_bidi_in_layout(first, target, enabled)
                || set_pane_smart_bidi_in_layout(second, target, enabled)
        }
    }
}

#[tauri::command]
fn pane_set_smart_bidi(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    enabled: bool,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_mut()
            .ok_or_else(|| format!("workspace {workspace_id} has no layout"))?;
        if !set_pane_smart_bidi_in_layout(layout, &pane_id, enabled) {
            return Err(format!("no pane {pane_id} in workspace {workspace_id}"));
        }
    }
    persist(&state)?;
    // Flip the runtime filter for this pane right now so the next PTY
    // chunk takes the new state.
    bidi_filter::set_pane_enabled(&state.bidi_filters, &pane_id, enabled);
    let _ = app.emit("workspaces:changed", ());
    log_debug("PTY", &format!(
        "[bidi] pane_set_smart_bidi: ws={} pane={} enabled={}",
        workspace_id, pane_id, enabled
    ));
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn pane_set_title(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    title: Option<String>,
) -> Result<WorkspacesFile, String> {
    let normalized = title.filter(|s| !s.is_empty());
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_pane_in(layout, &pane_id, Some(normalized.clone()), None, None));
            }
        }
    }
    persist(&state)?;

    // Phase 23.K: if the pane has a live tmux session, update the
    // LOCAL label for it. Pure disk write (no SSH, no spawned task)
    // — sidesteps the Phase 23.I crash entirely. The picker reads
    // this map back in `tmux_labels_get` and shows the label as the
    // primary line, with the raw tmux session name as secondary.
    //
    // The Phase 23.J disabled remote-tmux-rename side-effect stays
    // disabled — labels give us the user-friendly Hebrew title
    // experience without crossing the FFI panic boundary.
    let tmux_target = lookup_tmux_for_pane(&state, &pane_id);
    if let (Some(label_text), Some((_, _, tmux_name))) = (normalized.as_deref(), tmux_target.as_ref())
    {
        set_tmux_label_internal(&workspace_id, tmux_name, label_text);
    }

    // Phase 81: mirror the label into the server-side session-meta map so
    // EVERY machine's picker shows it (the local tmux-labels.json above is
    // per-machine). Label travels as hex-UTF8 — no shell quoting for
    // Hebrew, and no tmux rename (the Phase 23.J constraint stands).
    // Fire-and-forget over a separate exec channel.
    if let Some((_sid, handle, tmux_name)) = tmux_target {
        let cmd = match normalized.as_deref() {
            Some(label) => format!(
                "\"$HOME/.ymux/bin/ymux-linux-x64\" session-meta set --session {} --label-hex {} 2>/dev/null || true",
                shell_quote(&tmux_name),
                hex_utf8(label),
            ),
            None => format!(
                "\"$HOME/.ymux/bin/ymux-linux-x64\" session-meta set --session {} --clear-label 2>/dev/null || true",
                shell_quote(&tmux_name),
            ),
        };
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::updater::ssh_exec_simple(&handle, &cmd).await {
                log_warn("SSH", &format!("session-meta: label write failed: {e}"));
            }
        });
    }

    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 23.I helper: look up the SSH session bound to a pane and
/// return (session_id, ssh handle clone, current tmux session name).
/// Returns None if the pane has no session, the session is not SSH,
/// or it has no tmux wrapper.
/// Phase 81: re-enabled — pane_set_title uses it to mirror labels
/// into the server-side session-meta map (no tmux rename involved).
fn lookup_tmux_for_pane(
    state: &AppState,
    pane_id: &str,
) -> Option<(String, Arc<client::Handle<SshClient>>, String)> {
    let pane_sessions = state.core.pane_sessions.lock().ok()?;
    let sid = pane_sessions.get(pane_id)?.clone();
    drop(pane_sessions);
    let sessions = state.core.sessions.lock().ok()?;
    match sessions.get(&sid) {
        Some(Session::Ssh(s)) => s
            .tmux_session
            .as_ref()
            .map(|t| (sid.clone(), s.handle.clone(), t.clone())),
        _ => None,
    }
}

/// Phase 23.I helper: run `tmux rename-session -t <old> <new>` over an
/// existing SSH handle. Shared by pane_set_title and the legacy 23.G
/// tmux_rename_session tauri command. Validates names defensively
/// (no spaces/dots/colons) — `sanitize_tmux_session_name_for_title`
/// already collapses those, but a direct CLI caller might not.
async fn tmux_rename_session_via_handle(
    handle: &client::Handle<SshClient>,
    old_name: &str,
    new_name: &str,
) -> Result<(), String> {
    if new_name.is_empty() {
        return Err("name cannot be empty".into());
    }
    if new_name.chars().any(|c| c == '.' || c == ':') {
        return Err("name cannot contain dots or colons".into());
    }
    // `=` pins an exact session match (a bare `-t` prefix-matches).
    let cmd = format!(
        "tmux rename-session -t {} {} 2>&1",
        shell_quote(&format!("={old_name}")),
        shell_quote(new_name),
    );
    use russh::ChannelMsg;
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let mut exit_code: Option<u32> = None;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    let stderr_text = String::from_utf8_lossy(&stdout).trim().to_string();
    match exit_code {
        Some(0) => Ok(()),
        Some(code) => Err(if stderr_text.is_empty() {
            format!("tmux exit {code}")
        } else {
            stderr_text
        }),
        None => Err("tmux rename-session did not return an exit status".into()),
    }
}

#[tauri::command]
fn pane_set_annotation(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    annotation: Option<String>,
) -> Result<WorkspacesFile, String> {
    let normalized = annotation.filter(|s| !s.is_empty());
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_pane_in(layout, &pane_id, None, Some(normalized), None));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Pane connect / disconnect ───────────────────────────────────────────────

/// Phase 41: establish a background ("headless") SSH session for a
/// workspace without opening a pane, so the tmux session picker and the
/// remote file manager populate immediately on workspace select.
///
/// Idempotent — a no-op if any SSH session (headless or pane-backed)
/// already serves the workspace. Only agent/key auth is attempted
/// (`password: None`); password-mode workspaces are skipped silently with
/// a dlog (no UI to prompt from here — they connect when the user opens a
/// terminal pane). An unknown host key also skips silently rather than
/// auto-accepting in the background.
#[tauri::command]
async fn workspace_ensure_connected(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    // Fast idempotency check before doing any network work.
    if live_ssh_connection_for_workspace_pub(&state, &workspace_id).is_some() {
        return Ok(());
    }

    // Phase 80: claim the workspace, don't just peek at it. The check above
    // is advisory — `PaneView.smartConnect` calls this and then
    // `pane_connect`, so two tunnel setups for one workspace was the NORMAL
    // path: two `tcpip_forward`s 38ms apart, one watcher slot, the loser
    // orphaned on the server. Under the lock, the loser re-checks and returns
    // before opening a socket at all.
    //
    // Lock order is connect_lock BEFORE bootstrap_guard::host_lock, which is
    // the order `spawn_ssh` takes them in. This path never takes the host
    // lock, so no cycle is possible.
    let connect_lock = state.tunnel_registry.connect_lock(&workspace_id);
    let _connect_guard = connect_lock.lock().await;
    if live_ssh_connection_for_workspace_pub(&state, &workspace_id).is_some() {
        log_debug("SSH", &format!(
            "workspace_ensure_connected: {workspace_id} connected while we queued — nothing to do"
        ));
        return Ok(());
    }

    // Resolve the workspace's canonical SSH target.
    let conn = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| w.connection.clone())
    };
    let (host, user, port, key_path) = match conn {
        Some(Connection::Ssh {
            host,
            user,
            port,
            key_path,
        }) => (host, user, port, key_path),
        // Local workspace or no connection — nothing to auto-connect.
        _ => return Ok(()),
    };

    // agent/key only; never auto-accept an unknown host key in the background.
    match connect_and_authenticate(&host, &user, port, key_path.as_deref(), None, None, false).await
    {
        // Phase 47.A: capture tunnel_token (Phase 41 dropped it) and
        // keep `handle` mutable — `tcpip_forward` inside
        // `setup_workspace_reverse_tunnel` needs &mut, so the tunnel
        // setup must happen BEFORE the handle is moved into Arc and
        // stored in the session.
        Ok(SshHandshake {
            mut handle,
            auth_method,
            tunnel_token,
        }) => {
            // Quick idempotency pre-check (a pane may have already
            // connected). If so, drop the spare handle now.
            {
                let sessions = state.core.sessions.lock().unwrap();
                let already = sessions
                    .values()
                    .any(|s| matches!(s, Session::Ssh(ssh) if ssh.workspace_id == workspace_id));
                if already {
                    log_debug("SSH", &format!(
                        "workspace_ensure_connected: {workspace_id} connected by a pane mid-auth — dropping spare headless handle"
                    ));
                    return Ok(());
                }
            }
            // Phase 47.A: bootstrap the reverse tunnel before Arc-wrapping
            // so port detection works without a terminal pane. Best-effort:
            // failure leaves the session usable for tmux-list / file
            // manager, just no detection (matches pre-47.A behavior).
            //
            // `pane_id: None` — this path has no pane, so the env-file write
            // carries over whatever pane id the remote file already holds
            // rather than blanking it.
            let session_id = format!("__headless__{workspace_id}");
            let tunnel_lease = setup_workspace_reverse_tunnel(
                &state,
                &mut handle,
                &workspace_id,
                &session_id,
                &tunnel_token,
                None,
            )
            .await
            .map(|(_, lease)| lease);
            // Re-check + insert under the lock. If a pane raced in during
            // tunnel setup, drop the spare (its handle Drop tears the tunnel
            // down with it) — and let the lease roll the registration back,
            // which is what nothing did before.
            let mut sessions = state.core.sessions.lock().unwrap();
            let already = sessions
                .values()
                .any(|s| matches!(s, Session::Ssh(ssh) if ssh.workspace_id == workspace_id));
            if already {
                log_debug("SSH", &format!(
                    "workspace_ensure_connected: {workspace_id} connected by a pane mid-tunnel-setup — dropping spare headless handle"
                ));
                return Ok(());
            }
            sessions.insert(
                session_id.clone(),
                Session::Ssh(SshSession {
                    tx: None,
                    handle: Arc::new(handle),
                    workspace_id: workspace_id.clone(),
                    tmux_session: None,
                    host,
                    user,
                    port,
                    key_path,
                    // beta.3 (netfree): headless sessions have no PTY / io-loop,
                    // so the reconnect flow never fires on them — flag is inert.
                    reconnecting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                }),
            );
            drop(sessions);
            if let Some(lease) = tunnel_lease {
                lease.commit();
            }
            log_info("SSH", &format!(
                "workspace_ensure_connected: headless session up for {workspace_id} (method={auth_method:?})"
            ));
            Ok(())
        }
        Err(e) => {
            // Most commonly: no key/agent → password-only, which we can't
            // prompt for here. Skip silently; the terminal-pane path handles it.
            log_debug("SSH", &format!(
                "workspace_ensure_connected: skipped for {workspace_id}: {e}"
            ));
            Ok(())
        }
    }
}

#[tauri::command]
async fn pane_connect(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    password: Option<String>,
    key_passphrase: Option<String>,
    accept_unknown_host: Option<bool>,
    cols: u16,
    rows: u16,
    // Phase 11.A: when true the shell is wrapped in a multiplexer so
    // reconnects resume it — `tmux new-session -A` over SSH, `zellij attach
    // -c` on a native Windows pane (2026-08-19). Every connection kind
    // honours it now.
    persistent: Option<bool>,
    // Phase 12.B Smart Connect: when set, after the shell is up we inject a
    // mode-specific command. `mode` is one of: "default" (current behavior),
    // "tmux" (alias for persistent=true), "plain" (no tmux even if workspace
    // says persistent), "cmd" (run cmd in cwd), "claude" (launch claude in
    // cwd with claude_args).
    mode: Option<String>,
    cwd_override: Option<String>,
    cmd: Option<String>,
    claude_args: Option<String>,
    // Phase 23.F: when set AND we're in a persistent flow, override
    // the auto-derived tmux session name. Lets the user attach to a
    // previously-orphaned session whose original pane was closed.
    tmux_session_name: Option<String>,
) -> Result<String, String> {
    // Look up connection from workspaces state. Phase 7.C: also lift `env` and
    // `setup_command` from the workspace so we can inject them after the shell is up.
    // Phase 23.I: also lift the pane's title so the persistent (tmux) flow can
    // derive a session name from it instead of the opaque pane-id default.
    let (conn, cwd, ws_env, ws_setup, pane_title) = {
        let file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_ref()
            .ok_or_else(|| "no layout".to_string())?;
        // Phase 23.D: prefer the pane's own connection, but fall
        // back to the workspace-level `connection` when the pane
        // doesn't carry one. This lets the user reconnect to the
        // workspace's intended target from a fresh terminal pane
        // (e.g. one added via split off a FileManager/Browser)
        // even if pane.connection was never set, AND enforces "an
        // SSH workspace never accidentally spawns a local shell"
        // semantics requested by Yossi.
        let conn = find_pane_connection(layout, &pane_id)
            .or_else(|| {
                if pane_id_exists_in(layout, &pane_id) {
                    ws.connection.clone()
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                if pane_id_exists_in(layout, &pane_id) {
                    format!("pane {pane_id} is not a terminal pane and workspace has no connection")
                } else {
                    format!("no pane {pane_id}")
                }
            })?;
        let title = find_pane_title(layout, &pane_id);
        (
            conn,
            ws.cwd.clone(),
            ws.env.clone(),
            ws.setup_command.clone(),
            title,
        )
    };

    let effective_tmux_name: Option<String> =
        resolve_effective_session_name(tmux_session_name.as_deref(), pane_title.as_deref());

    // 2026-08-23 — THE ATTACH-ONLY GUARD. Ask BEFORE spawning anything,
    // because the spawn is what creates the session: 200ms later the answer
    // is "yes, it exists" for every pane and the question becomes useless.
    //
    // What it prevents: `build_tmux_attach_script` runs `new-session -A -s`,
    // attach-OR-create. On a name that is already live the pane joins a
    // running session, and the smart-connect injection then types the
    // wizard's command into whatever holds that session's foreground — a
    // shell that gets restarted, or a live `claude` that receives
    // `cd … && claude --resume …` as a chat message. Yossi's report, exactly.
    //
    // WHEN THE HOST CANNOT BE ASKED the fallback is deliberately asymmetric,
    // and the asymmetry is the whole design:
    //   - an EXPLICIT `tmux_session_name` came from the picker, which only
    //     ever lists sessions that exist → live, no question asked;
    //   - a DERIVED name (pane title / `ymux-<paneid>`) on an unreachable host
    //     falls back to "not live", i.e. today's behaviour. Assuming "live"
    //     instead would silently drop the command on every FIRST connect to an
    //     SSH workspace, where there is no handle yet by definition — trading
    //     a real bug for a worse one.
    // The frontend closes that residual gap: the wizard asks
    // `pane_target_session_state` (after `workspace_ensure_connected`) and
    // simply does not send a command when the session already exists.
    let target_name = session_name_for_pane(
        tmux_session_name.as_deref(),
        pane_title.as_deref(),
        &pane_id,
    );
    let explicit_pick = tmux_session_name
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    // Only ask when the answer can change what we do. A plain connect injects
    // nothing either way, and this probe is a `tmux list-sessions` over SSH or
    // a `zellij list-sessions` subprocess — real latency on the critical path
    // between the user's click and a visible shell.
    let would_inject = matches!(mode.as_deref(), Some("cmd") | Some("claude"))
        || cwd_override.as_deref().is_some_and(|s| !s.trim().is_empty());
    let target_was_live = if !would_inject {
        false
    } else if explicit_pick {
        true
    } else if workspace_sessions_reachable(&state, &workspace_id) {
        list_workspace_tmux_sessions(&state, &workspace_id)
            .await
            .unwrap_or_default()
            .iter()
            .any(|s| s.name == target_name)
    } else {
        log_debug(
            "PTY",
            &format!(
                "attach-guard: cannot reach ws={workspace_id} to check '{target_name}'; \
                 assuming it is not live (first-connect case)"
            ),
        );
        false
    };

    // Resolve shell kind for env-line formatting (need this BEFORE we move `conn`).
    let shell_kind = match &conn {
        Connection::Local { shell } => detect_shell_kind(&pick_default_shell(shell.clone())),
        Connection::Ssh { .. } => ShellKind::Posix,
        // Phase 80: WSL panes run the distro's login shell — POSIX, so
        // env-line formatting + smart-connect scripts come out right.
        Connection::Wsl { .. } => ShellKind::Posix,
    };

    // Kill any prior session for this pane.
    if let Some(old_sid) = state.core.pane_sessions.lock().unwrap().remove(&pane_id) {
        if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&old_sid) {
            kill_session_inner(&mut s);
        }
    }

    // 2026-08-19: set by the Local arm below when the pane is persistent, so
    // the smart-connect injection at the end of this function can address the
    // zellij session directly instead of typing into the shell in front of it.
    // Assigned only in the cfg(windows) arm below; elsewhere it stays None
    // and the smart-connect injection falls back to typing into the shell.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut zellij_target: Option<String> = None;
    let session_id = match conn {
        // 2026-08-19: native Windows panes are persistent by default too,
        // via zellij. This arm used to DROP `persistent` / `mode` /
        // `effective_tmux_name` on the floor, which is why the connect
        // wizard's persistence toggle was a live control with no effect.
        Connection::Local { shell } => {
            // One concept, two backends. Windows panes are persistent by
            // default - zellij is the whole point of the native-local path.
            // Elsewhere the default stays a plain shell and tmux is opt-in,
            // which is the behaviour the macOS port shipped.
            #[cfg(windows)]
            let persist_name: Option<String> = {
                let effective_persistent = match mode.as_deref() {
                    Some("tmux") => true,
                    Some("plain") => false,
                    _ => persistent.unwrap_or(true),
                };
                if effective_persistent {
                    Some(
                        effective_tmux_name
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| sanitize_tmux_session_name(&pane_id)),
                    )
                } else {
                    None
                }
            };
            #[cfg(not(windows))]
            let persist_name: Option<String> = {
                let effective_persistent = match mode.as_deref() {
                    Some("tmux") => true,
                    Some("plain") => false,
                    _ => persistent.unwrap_or(false),
                };
                if effective_persistent {
                    Some(
                        effective_tmux_name
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| sanitize_tmux_session_name(&pane_id)),
                    )
                } else {
                    None
                }
            };
            // Only the zellij path can be addressed by name after the fact;
            // the tmux path types into the shell, as it always has.
            #[cfg(windows)]
            {
                zellij_target = persist_name.clone();
            }
            spawn_local_pty(
                &state,
                pane_id.clone(),
                &app,
                shell,
                cwd,
                cols,
                rows,
                persist_name,
            )?
        }
        // Phase 80: WSL panes default to PERSISTENT (tmux) — persistence
        // is the point of the smart local setup. mode="plain" still
        // forces a bare shell, mirroring the SSH mode override.
        Connection::Wsl { distro } => {
            let effective_persistent = match mode.as_deref() {
                Some("tmux") => true,
                Some("plain") => false,
                _ => persistent.unwrap_or(true),
            };
            let tmux_name = if effective_persistent {
                Some(
                    effective_tmux_name
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| sanitize_tmux_session_name(&pane_id)),
                )
            } else {
                None
            };
            spawn_wsl_pty(
                &state,
                pane_id.clone(),
                &app,
                distro,
                cwd,
                cols,
                rows,
                tmux_name,
            )?
        }
        Connection::Ssh {
            host,
            user,
            port,
            key_path,
        } => {
            // Phase 12.B: derive effective persistence from mode if given.
            // mode="tmux" → persistent regardless of caller; mode="plain"
            // → forced plain; otherwise honor `persistent` flag.
            let effective_persistent = match mode.as_deref() {
                Some("tmux") => true,
                Some("plain") => false,
                _ => persistent.unwrap_or(false),
            };
            spawn_ssh(
                &state,
                pane_id.clone(),
                &app,
                workspace_id.clone(),
                host,
                user,
                port,
                key_path,
                key_passphrase,
                password,
                accept_unknown_host.unwrap_or(false),
                cols,
                rows,
                effective_persistent,
                effective_tmux_name.clone(),
            )
            .await?
        }
    };
    state.core
        .pane_sessions
        .lock()
        .unwrap()
        .insert(pane_id.clone(), session_id.clone());

    // 2026-08-23: is this pane actually multiplexer-wrapped? Read it off the
    // session the spawn arms just built rather than re-deriving
    // `effective_persistent` here — that value is computed independently in
    // three cfg-split arms, and a fourth copy of the rule is a fourth chance
    // to disagree with the other three.
    let pane_is_persistent = {
        let sessions = state.core.sessions.lock().unwrap();
        match sessions.get(&session_id) {
            Some(Session::Local(l)) => l.tmux_session.is_some(),
            Some(Session::Ssh(s)) => s.tmux_session.is_some(),
            None => false,
        }
    };

    // 2026-08-23: record which workspace this session belongs to. Done for a
    // picker attach as much as for a fresh session — choosing a session by
    // hand is just as much a statement of "this is mine" as creating one —
    // and it is the ONLY workspace signal available on Windows, where zellij
    // reports no working directory at all.
    if pane_is_persistent {
        let (host_key, ws_cwd) = {
            let file = state.workspaces.lock().unwrap();
            let ws = file.workspaces.iter().find(|w| w.id == workspace_id);
            (
                session_owner_host_key(ws.and_then(|w| w.connection.as_ref())),
                ws.and_then(|w| w.cwd.clone()),
            )
        };
        claim_session_owner(
            &host_key,
            &target_name,
            &workspace_id,
            cwd_override.as_deref().or(ws_cwd.as_deref()),
        );
    }

    // Phase 7.C: inject env exports + setup_command after a 500ms grace period.
    schedule_setup_injection(
        state.core.sessions.clone(),
        session_id.clone(),
        shell_kind,
        ws_env,
        ws_setup,
    );

    // Phase 12.B Smart Connect: when mode is "cmd" or "claude", inject the
    // command after a 1.1s delay (after env exports + setup_command + tmux
    // wrap have all settled). cwd_override changes directory first.
    // Phase 61: the script is shaped per shell_kind, so local PowerShell /
    // Cmd panes get working syntax too (POSIX `exec` form unchanged).
    let smart_mode = mode.clone();
    // Phase 65 (bug AA): also fire for a plain connect that carries a
    // cwd_override (the folder picker) — build_smart_connect_script then
    // emits a `cd <dir>` so "Open in directory" actually changes dir.
    let has_cwd = cwd_override
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    // 2026-08-23 — ATTACH-ONLY. A persistent pane joining a session that was
    // ALREADY running gets nothing typed into it: not the command, and not
    // the `cd` either. The `cd` is not the lesser evil — `cd /srv/app` landing
    // in a live `claude` is the same bug with a shorter payload.
    //
    // Only persistence makes this dangerous. A plain shell is this pane's own
    // fresh process, so injecting there is correct even if some tmux session
    // elsewhere happens to share the name.
    if pane_is_persistent && target_was_live {
        log_info(
            "PTY",
            &format!(
                "attach-only: pane {pane_id} joined existing session '{target_name}' — \
                 nothing injected (mode={:?}, cwd_override={})",
                smart_mode.as_deref().unwrap_or("none"),
                if has_cwd { "yes" } else { "no" }
            ),
        );
        // Tell the UI, so a command the user chose is never silently dropped.
        let _ = app.emit(
            "pane-connect-notice",
            serde_json::json!({
                "pane_id": pane_id,
                "session_name": target_name,
                "skipped": "attach-only",
                "had_command": matches!(smart_mode.as_deref(), Some("cmd") | Some("claude")),
            }),
        );
        return Ok(session_id);
    }

    if matches!(smart_mode.as_deref(), Some("cmd") | Some("claude")) || has_cwd {
        let script = build_smart_connect_script(
            shell_kind,
            smart_mode.as_deref().unwrap_or_default(),
            cwd_override.as_deref(),
            cmd.as_deref(),
            claude_args.as_deref(),
        );
        if !script.is_empty() {
            let sessions_clone = state.core.sessions.clone();
            let session_id_clone = session_id.clone();
            let zellij_target = zellij_target.clone();
            // 2026-08-23: the tmux counterpart of `zellij_target` — the name
            // to wait for before typing, so the command cannot land in the
            // outer shell that is still busy running `tmux new-session`.
            let tmux_target: Option<String> = if pane_is_persistent && zellij_target.is_none() {
                Some(target_name.clone())
            } else {
                None
            };
            let app_for_poll = app.clone();
            let ws_for_poll = workspace_id.clone();
            tokio::spawn(async move {
                // 2026-08-19: a persistent local pane runs its shell INSIDE
                // zellij, and the attach line is typed at 900ms while this
                // fired at 1100ms. Those 200ms were the only thing keeping
                // `claude --continue` from landing in the outer PowerShell —
                // no margin at all on a cold start of a 48MB binary.
                //
                // Address the session instead of racing it: wait for it to
                // appear, then have zellij type the script into it. Pure argv,
                // no shell in between (Rule #3).
                if let Some(name) = zellij_target {
                    let mut appeared = false;
                    for _ in 0..8 {
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                        if list_zellij_sessions()
                            .await
                            .iter()
                            .any(|s| s.name == name && !s.exited)
                        {
                            appeared = true;
                            break;
                        }
                    }
                    if appeared
                        && zellij_run(
                            &zellij_args_write_chars(&name, &script),
                            "write-chars",
                        )
                        .await
                    {
                        return;
                    }
                    // Fell through: zellij never came up, or refused the
                    // write. The pane is still a working shell, so type into
                    // it directly rather than dropping the user's command.
                    log_debug(
                        "PTY",
                        &format!(
                            "smart-connect: zellij delivery unavailable (appeared={appeared}), typing into the pane"
                        ),
                    );
                } else if let Some(name) = tmux_target {
                    // 2026-08-23: the same "address the session, don't race
                    // it" fix zellij got, applied to tmux. The old code slept
                    // a flat 1100ms while the attach line was typed at 900ms
                    // — a 200ms margin for `command -v tmux` plus a
                    // new-session over an SSH channel, which the comment above
                    // already describes as "no margin at all".
                    //
                    // Unlike zellij there is no out-of-band `write-chars`
                    // verb here, so the pane's own PTY stays the delivery
                    // path; waiting for the session to EXIST is what makes
                    // that PTY point inside tmux rather than at the shell in
                    // front of it. Bounded at 900ms + 8×400ms; on timeout we
                    // type anyway, because dropping the user's command is
                    // worse than typing it at a prompt.
                    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                    let mut appeared = false;
                    for _ in 0..8 {
                        let st = app_for_poll.state::<AppState>();
                        if list_workspace_tmux_sessions(st.inner(), &ws_for_poll)
                            .await
                            .unwrap_or_default()
                            .iter()
                            .any(|s| s.name == name)
                        {
                            appeared = true;
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    }
                    if !appeared {
                        log_debug(
                            "PTY",
                            &format!(
                                "smart-connect: tmux session '{name}' never appeared; typing into the pane anyway"
                            ),
                        );
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
                }
                let mut sessions = sessions_clone.lock().unwrap();
                if let Some(s) = sessions.get_mut(&session_id_clone) {
                    match s {
                        Session::Local(l) => {
                            use std::io::Write as _;
                            let _ = l.writer.write_all(script.as_bytes());
                            let _ = l.writer.flush();
                        }
                        Session::Ssh(ssh) => {
                            let _ = ssh.try_send(SshCmd::Data(script.into_bytes()));
                        }
                    }
                }
            });
        }
    }

    Ok(session_id)
}

/// Phase 23.F: tmux session metadata returned by
/// pane_list_tmux_sessions for the Connect (tmux) picker modal.
#[derive(Clone, Serialize)]
pub(crate) struct TmuxSessionInfo {
    pub name: String,
    pub created: i64,
    pub attached: bool,
    pub windows: u32,
    pub last_attached: i64,
    /// 2026-08-19: zellij only. The session's shell has exited but zellij
    /// still holds a serialized copy, so attaching RESURRECTS it — including
    /// across a reboot, which tmux cannot do. Always false for tmux.
    pub exited: bool,
    /// Phase 81: joined from the server-side `~/.ymux/session-meta.json`.
    /// Picker display precedence: label > auto_name > claude_title > name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_title: Option<String>,
    /// Stable "<two words> · <date time>" derived from the session's first
    /// prompt. Beats `claude_title`, which Claude rewrites as it goes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// Machine id that created the session (see `machine_id()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// 2026-08-23: the session's working directory, from tmux's
    /// `#{session_path}`. `None` on zellij (its `list-sessions` reports no
    /// cwd at all) and on a tmux old enough that the 6th field is missing —
    /// both are "unknown", never "elsewhere", so the picker must not treat a
    /// `None` as evidence that a session belongs to some other folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 2026-08-23: ymux created or attached this session for the workspace
    /// the caller asked about (`session-owners.json`). This is the half of
    /// the workspace scope that works on Windows, where zellij gives no cwd.
    #[serde(default)]
    pub owned: bool,
    /// 2026-08-23: `cwd` is the caller's `project_path`, or lives under it.
    /// False whenever `cwd` is unknown — see the note on `cwd`.
    #[serde(default)]
    pub in_cwd: bool,
    /// 2026-08-24: we can positively place this session somewhere that is NOT
    /// the asking workspace. This is the "Whole server" view's mess-guard —
    /// a row nobody can place is free to attach, a row we CAN place already
    /// belongs to someone.
    ///
    /// `None` means "no evidence", and an unknown `cwd` with no ownership row
    /// is ALWAYS no evidence (zellij reports no directory at all). Unknown
    /// must never render as "elsewhere" — see the note on `cwd`.
    ///
    /// Invariant, enforced in `annotate_scope_with` and asserted in the tests:
    /// never `Some` while `owned || in_cwd`. The picker's "This folder" view is
    /// exactly the complement of this field, so the badge cannot appear there
    /// by construction and the frontend needs no scope conditional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreign: Option<ForeignScope>,
    /// Phase 87: the cwd recorded in `session-owners.json` when ymux claimed
    /// this session, regardless of WHICH workspace claimed it. Exists for the
    /// active-sessions overview, which groups rows by directory and would
    /// otherwise have nothing to group a zellij session under (no live cwd).
    /// It is a claim-time snapshot and can be stale; it feeds no scope
    /// verdict — `owned` / `in_cwd` / `foreign` never read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_cwd: Option<String>,
}

/// 2026-08-24: where a session belongs, when it is not here.
///
/// Carries facts rather than a ready-made sentence because every string the
/// picker shows is i18n'd in four languages — the backend supplies the facts,
/// `PaneView.tsx` composes the words. `kind` is what lets it pick between
/// "belongs to workspace X" and "runs in folder Y", which are different
/// warnings.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ForeignKind {
    /// A different workspace claimed this session in `session-owners.json`.
    /// The only signal that survives on Windows, where zellij has no cwd.
    Workspace,
    /// Nobody claimed it, but its live `cwd` is demonstrably outside the
    /// caller's folder.
    Folder,
}

#[derive(Clone, Serialize)]
pub(crate) struct ForeignScope {
    pub kind: ForeignKind,
    /// The owning workspace's name, or the folder's last path segment. Never a
    /// user-facing sentence. `None` is reachable and real: nothing prunes
    /// `session-owners.json` when a workspace is deleted, so a stale row can
    /// name a workspace that no longer exists AND have recorded no cwd. That
    /// still warrants a warning — the picker just words it generically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Full path for the row tooltip: the live `#{session_path}` when known,
    /// else the cwd recorded at claim time (which may be stale).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Phase 81: deserialization mirror of the Linux CLI's session-meta file
/// (cli/src/session_meta.rs — keep the shapes in sync). Unknown fields
/// are ignored so CLI-side schema additions never break the desktop.
#[derive(Deserialize, Default)]
struct SessionMetaFileMirror {
    #[serde(default)]
    sessions: HashMap<String, SessionMetaEntryMirror>,
}

#[derive(Deserialize, Default)]
struct SessionMetaEntryMirror {
    #[serde(default)]
    claude_session_id: Option<String>,
    #[serde(default)]
    claude_title: Option<String>,
    #[serde(default)]
    auto_name: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    origin: Option<String>,
}

/// Phase 23.F: enumerate the tmux sessions live on a workspace's
/// host. Returns Ok([]) when tmux isn't installed or no sessions
/// exist. Used by the Connect (tmux) split-button to populate a
/// picker so users can attach to an orphan session whose original
/// pane was closed.
/// Ask zellij for its sessions. Used by both the workspace-level list and
/// the per-pane probe; unlike SSH there is no handle or auth involved, so
/// this answers from cold whenever the binary is present.
///
/// A missing binary is NOT an error here — it is an empty list. `zellij_exe`
/// falls back to the bare word, so a machine that never ran the installer
/// simply has no sessions, and the picker renders its "new session" row.
async fn list_zellij_sessions() -> Vec<TmuxSessionInfo> {
    let mut c = local_setup::hidden_cmd(&zellij_exe());
    for a in zellij_args_list() {
        c.arg(a);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(6), c.output()).await {
        Ok(Ok(out)) => {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            parse_zellij_sessions(&text)
        }
        Ok(Err(e)) => {
            log_debug("PTY", &format!("list_zellij_sessions: spawn failed: {e}"));
            Vec::new()
        }
        Err(_) => {
            log_warn("PTY", "list_zellij_sessions: timed out after 6s");
            Vec::new()
        }
    }
}

/// Destroy a zellij session BY NAME, with no pane involved.
///
/// The picker lists every zellij session on the machine, including EXITED ones
/// (they are resurrectable — that is the point of listing them). Two things
/// put sessions there that no pane will ever reclaim automatically: closing a
/// pane leaves its session running on purpose, and a reboot leaves everything
/// EXITED. Without this the list was append-only — the user could resurrect a
/// corpse but never bury one.
///
/// Zellij-only on purpose, not out of laziness: the delete affordance is shown
/// only for `exited` rows, and `TmuxSessionInfo::exited` is documented as
/// always false on tmux. There is no tmux case to handle.
#[tauri::command]
async fn zellij_delete_session(name: String) -> Result<KillSessionOutcome, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("no session name".into());
    }
    // 2026-08-23: zellij is always the LOCAL multiplexer, so the ownership
    // claim can be released without knowing which workspace asked — there is
    // only one host key it could be under.
    release_session_owner("local", &name);
    // Argv only (Rule #3) — the name goes into one slot and is never reparsed.
    Ok(
        match zellij_try(&zellij_args_delete_force(&name), "delete-session -f").await {
            ZellijOutcome::Ok => KillSessionOutcome::new("killed", "zellij", Some(name)),
            ZellijOutcome::Missing => {
                KillSessionOutcome::new("multiplexer_missing", "zellij", Some(name))
            }
            ZellijOutcome::Failed { code, stderr } => {
                let r = if stderr.contains("not found") {
                    "already_gone"
                } else {
                    "failed"
                };
                log_debug(
                    "PTY",
                    &format!("zellij_delete_session: {r} (exit {code:?}): {stderr}"),
                );
                KillSessionOutcome::new(r, "zellij", Some(name)).with_detail(stderr)
            }
        },
    )
}

/// 2026-08-23: the host-side list, with no scoping and no visibility filter.
///
/// Split out of `pane_list_tmux_sessions` because two callers now need the
/// UNFILTERED truth and would be wrong with anything less:
///   - the picker, which applies both filters itself afterwards;
///   - `pane_target_session_state`, whose whole job is "does this session
///     already exist?". Running that question through the visibility filter
///     would answer "no" for a session another machine created, and the
///     caller would then type a command straight into it — the exact bug the
///     guard exists to prevent.
async fn list_workspace_tmux_sessions(
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<TmuxSessionInfo>, String> {
    // Phase 80: WSL workspaces list their sessions via wsl.exe — no SSH
    // handle involved, and it works before any pane has connected (the
    // tmux server inside the distro is reachable whenever wsl.exe is).
    let wsl_distro: Option<Option<String>> = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| match &w.connection {
                Some(Connection::Wsl { distro }) => Some(distro.clone()),
                _ => None,
            })
    };
    if let Some(distro) = wsl_distro {
        let (_code, text) = local_setup::wsl_exec(distro.as_deref(), None, &tmux_list_script())
            .await
            .unwrap_or((-1, String::new()));
        return Ok(parse_tmux_sessions(&text));
    }

    // 2026-08-19: native Windows workspaces answer from zellij. Without this
    // arm a local workspace fell through to the SSH branch below and got a
    // silent Ok([]), which is why the picker never appeared for local panes.
    //
    // 2026-08-23: `cfg(windows)` ADDED. This arm shipped ungated, and since
    // it returns unconditionally for any local workspace it shadowed the
    // `cfg(not(windows))` macOS branch further down — so a Mac asked ZELLIJ
    // for its sessions, got an empty list (no zellij there), and never
    // reached its own tmux server. Found while extracting this function;
    // fixed here rather than filed, because the workspace-scoped picker is
    // built on this call and would have been silently empty on macOS.
    #[cfg(windows)]
    {
        let is_local = {
            let file = state.workspaces.lock().unwrap();
            file.workspaces
                .iter()
                .find(|w| w.id == workspace_id)
                .map(|w| matches!(w.connection, None | Some(Connection::Local { .. })))
                .unwrap_or(false)
        };
        if is_local {
            return Ok(list_zellij_sessions().await);
        }
    }

    // Phase 23.H: silent Ok([]) fallback when no live SSH handle.
    // Previously we errored ("no active SSH session for this workspace"),
    // but the user typically clicks Connect (tmux) BEFORE any pane has
    // authenticated — the whole point is to pick an orphan session before
    // connecting. Returning Ok([]) lets the picker render its "New session"
    // option + the "No existing sessions" empty-state line, which is
    // accurate ("no sessions visible from ymux right now") and avoids
    // surfacing a red error for the most common access pattern. Once a
    // terminal pane authenticates, re-opening the picker will list the
    // real sessions over the now-live handle.
    //
    // 2026-08-23 CAUTION for the attach-only guard: this empty answer means
    // "we could not ask", NOT "no session exists". `pane_target_session_state`
    // reports `reachable: false` in that case and the caller must treat an
    // unreachable host as "might already be live", never as "safe to inject".
    let handle = {
        let sessions = state.core.sessions.lock().unwrap();
        sessions
            .iter()
            .find_map(|(_sid, sess)| match sess {
                Session::Ssh(s) if s.workspace_id == workspace_id => Some(s.handle.clone()),
                _ => None,
            })
    };
    let handle = match handle {
        Some(h) => h,
        None => {
            // macOS port: a LOCAL workspace (explicit `Local` connection, or
            // none — the codebase's "no connection = local" default) lists
            // the sessions of the tmux server on this machine. Windows keeps
            // the empty answer: local panes there are ConPTY, never tmux.
            #[cfg(not(windows))]
            {
                let is_local = {
                    let file = state.workspaces.lock().unwrap();
                    file.workspaces
                        .iter()
                        .find(|w| w.id == workspace_id)
                        .map(|w| matches!(w.connection, Some(Connection::Local { .. }) | None))
                        .unwrap_or(false)
                };
                if is_local {
                    return Ok(list_local_tmux_sessions().await);
                }
            }
            log_debug("SSH", &format!(
                "list_workspace_tmux_sessions: no live SSH handle for ws={workspace_id}, returning empty list"
            ));
            return Ok(vec![]);
        }
    };
    list_tmux_sessions_via_handle(&handle).await
}

/// Can we currently ask this workspace's host anything at all?
///
/// The distinction matters only to the attach-only guard: a local or WSL host
/// is always reachable, but an SSH workspace with no live handle yields an
/// empty list that means "unknown", not "empty".
fn workspace_sessions_reachable(state: &AppState, workspace_id: &str) -> bool {
    let needs_handle = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| matches!(w.connection, Some(Connection::Ssh { .. })))
            .unwrap_or(false)
    };
    if !needs_handle {
        return true;
    }
    let sessions = state.core.sessions.lock().unwrap();
    sessions
        .iter()
        .any(|(_sid, sess)| matches!(sess, Session::Ssh(s) if s.workspace_id == workspace_id))
}

#[tauri::command]
async fn pane_list_tmux_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
    project_path: Option<String>,
) -> Result<Vec<TmuxSessionInfo>, String> {
    // 2026-08-23: `project_path` mirrors the parameter of the same name on
    // `pane_list_claude_sessions` rather than inventing a second spelling for
    // the same idea — that command has scoped its list to a folder since
    // project-folders v4, and the tmux list was the one thing in the same
    // wizard that never got it.
    //
    // It is OPTIONAL and `None` means "no scope", which matters more than it
    // looks: session restore (App.tsx) and `pane_probe_tmux_sessions` share
    // these list paths and MUST see every session. A pane whose session sits
    // outside the current folder still has to come back on the next boot.
    // Scope is a picker concern, exactly like `session_visibility` below.
    let project_path = project_path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    // The annotate step needs the workspace's host to key ownership by, its cwd
    // as the scope root when the caller did not name one, and — 2026-08-24 —
    // every workspace's display name, so a session claimed by a DIFFERENT
    // workspace can say whose it is instead of just "not yours". All three come
    // off the one lock this block already takes.
    let (host_key, ws_cwd, ws_names) = {
        let file = state.workspaces.lock().unwrap();
        let ws = file.workspaces.iter().find(|w| w.id == workspace_id);
        (
            session_owner_host_key(ws.and_then(|w| w.connection.as_ref())),
            ws.and_then(|w| w.cwd.clone()),
            file.workspaces
                .iter()
                .map(|w| (w.id.clone(), w.name.clone()))
                .collect::<HashMap<_, _>>(),
        )
    };
    // The wizard passes the folder anchor explicitly; a caller that does not
    // still gets scoping when the workspace itself is folder-anchored.
    let scope_path = project_path.or(ws_cwd);
    let scope_path = scope_path.as_deref().filter(|p| !p.trim().is_empty());

    let mut out = list_workspace_tmux_sessions(&state, &workspace_id).await?;

    // Phase 81: visibility scope (picker-only). "shared" (default) = every
    // session on the server; "local" = only sessions this machine created.
    // Origin-less sessions (pre-81, or an old CLI) stay visible — fail-open so
    // the filter can never hide something the user can't get back.
    let visibility = state
        .settings
        .lock()
        .ok()
        .map(|s| s.session_visibility.clone())
        .unwrap_or_else(|| "shared".to_string());
    if visibility == "local" {
        let my_id = machine_id();
        out.retain(|s| s.origin.as_deref().map_or(true, |o| o.is_empty() || o == my_id));
    }
    // 2026-08-23: the workspace scope is a SECOND, independent axis, and the
    // two do not know about each other. It is applied as annotation, not as a
    // retain, so the picker's toggle switches views without another round trip
    // to the host — and so a session outside the scope stays one click away
    // instead of vanishing.
    annotate_session_scope(&mut out, &host_key, &workspace_id, scope_path, &ws_names);
    Ok(out)
}

/// 2026-08-23: "what will this pane land on, and is it already live?"
///
/// THE BUG THIS EXISTS FOR: the connect wizard let you pick TMUX + a command,
/// and `build_tmux_attach_script` types `tmux new-session -A -s <name>` —
/// attach-OR-create. When the name already existed, the pane joined a running
/// session and the smart-connect injection 200ms later typed the command into
/// whatever that session had in the foreground: a restarted shell, or a live
/// `claude` receiving `cd … && claude --resume …` as a chat prompt.
///
/// `reachable` is not decoration. An SSH workspace with no live handle cannot
/// answer, and "cannot answer" must never be read as "nothing is running".
#[derive(Clone, Serialize)]
pub(crate) struct TargetSessionState {
    /// The name resolved through the same precedence `pane_connect` uses.
    pub name: String,
    pub exists: bool,
    /// A client is already attached to it (tmux `#{session_attached}`).
    pub attached: bool,
    /// False when the host could not be asked; `exists` is then meaningless.
    pub reachable: bool,
}

#[tauri::command]
async fn pane_target_session_state(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
    tmux_session_name: Option<String>,
) -> Result<TargetSessionState, String> {
    let pane_title = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| w.layout.as_ref())
            .and_then(|layout| find_pane_title(layout, &pane_id))
    };
    let name = session_name_for_pane(
        tmux_session_name.as_deref(),
        pane_title.as_deref(),
        &pane_id,
    );
    let reachable = workspace_sessions_reachable(&state, &workspace_id);
    // Unfiltered on purpose — see list_workspace_tmux_sessions' doc comment.
    let sessions = list_workspace_tmux_sessions(&state, &workspace_id)
        .await
        .unwrap_or_default();
    let hit = sessions.iter().find(|s| s.name == name);
    Ok(TargetSessionState {
        exists: hit.is_some(),
        attached: hit.is_some_and(|s| s.attached),
        reachable,
        name,
    })
}

/// The `tmux list-sessions` half of `pane_list_tmux_sessions`, split out so the
/// Phase 80 restore probe can reuse it over a handle it opened itself instead
/// of one belonging to a live pane.
/// Read the system clipboard as text, host-side.
///
/// Why this exists at all: in the terminal, Copy worked and Paste silently
/// did nothing. `navigator.clipboard.writeText` is allowed in WebView2, but
/// `readText` sits behind a clipboard-read permission the host has to grant
/// and Tauri does not — so the promise rejected, and the only handler was a
/// `console.warn` nobody sees. Reading here sidesteps the web permission
/// model entirely.
///
/// Returns an empty string when the clipboard holds no text (an image, or
/// nothing) — that is not an error, and the caller simply pastes nothing.
#[tauri::command]
fn clipboard_read_text() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard open: {e}"))?;
    match cb.get_text() {
        Ok(t) => Ok(t),
        // arboard reports "nothing here" as an error; the UI wants "".
        Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
        Err(e) => Err(format!("clipboard read: {e}")),
    }
}

/// tmux listing plus the session-meta side-car, in ONE round trip.
///
/// Phase 81: the same call also fetches the server-side session-meta map
/// (Claude titles / labels / origins written by the Linux CLI). A missing
/// file leaves an empty segment after the marker → no metadata, which is
/// exactly the pre-81 behaviour, so `parse_tmux_sessions` degrades cleanly.
/// The Phase 80 restore probe reuses this and ignores the meta segment.
///
/// Shared by the SSH and WSL paths deliberately. The WSL branch used to
/// carry its own copy WITHOUT the meta segment, so a WSL tmux picker showed
/// bare session names while an SSH one showed Claude titles — a difference
/// nobody chose. One const means the format string cannot drift again.
///
/// 2026-08-23: built from `TMUX_LIST_FORMAT` rather than repeating it. The
/// paragraph above already claimed the format string "cannot drift again"
/// while this very function carried a hand-typed second copy of it — and the
/// two DID drift the moment `#{session_path}` was added. One allocation per
/// list call removes the failure mode.
fn tmux_list_script() -> String {
    format!(
        "tmux list-sessions -F '{TMUX_LIST_FORMAT}' 2>/dev/null; printf '\\n{TMUX_META_MARKER}\\n'; cat \"$HOME/.ymux/session-meta.json\" 2>/dev/null; true"
    )
}

async fn list_tmux_sessions_via_handle(
    handle: &client::Handle<SshClient>,
) -> Result<Vec<TmuxSessionInfo>, String> {
    let script = tmux_list_script();
    use russh::ChannelMsg;
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, script.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    // Pure list (with Phase 81 meta joined by parse_tmux_sessions). Visibility
    // filtering is a picker concern applied by the caller that owns `state`
    // (pane_list_tmux_sessions); the Phase 80 restore probe deliberately keeps
    // the full list so it can re-attach a pane regardless of session origin.
    Ok(parse_tmux_sessions(&String::from_utf8_lossy(&stdout)))
}

/// `tmux list-sessions -F` format shared by every list path (SSH, WSL,
/// local unix) — one line per session, parsed by `parse_tmux_sessions`.
const TMUX_LIST_FORMAT: &str =
    "#{session_name}|#{session_created}|#{session_attached}|#{session_windows}|#{session_last_attached}|#{session_path}";
/// Separator between the session list and the appended session-meta JSON.
const TMUX_META_MARKER: &str = "<<<YMUX_META>>>";

/// macOS/Linux: resolve the local tmux binary. A Finder-launched app
/// inherits launchd's minimal PATH (no /opt/homebrew/bin), so the PATH
/// lookup is backed by the canonical Homebrew (arm64 / intel), MacPorts
/// and system locations.
#[cfg(not(windows))]
fn local_tmux_binary() -> Option<PathBuf> {
    local_wizard::which("tmux").or_else(|| {
        [
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/opt/local/bin/tmux",
            "/usr/bin/tmux",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    })
}

/// macOS/Linux: run the local tmux binary with an argv array (Rule #3) and
/// return (exit_code, stdout). `Err` only when tmux is absent or fails to
/// spawn — a non-zero exit (e.g. `no server running`) is `Ok((1, ""))`.
#[cfg(not(windows))]
async fn local_tmux_output(args: &[&str]) -> Result<(Option<i32>, String), String> {
    let tmux = local_tmux_binary().ok_or_else(|| "tmux not found on this machine".to_string())?;
    let out = tokio::process::Command::new(&tmux)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("spawn {}: {e}", tmux.display()))?;
    Ok((out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// macOS/Linux: sessions of the tmux server on this machine, joined with
/// the local `~/.ymux/session-meta.json` when present. Missing tmux, no
/// server running (`tmux` exits 1) or a spawn failure all yield an empty
/// list — the picker then shows its "New session" line, never an error.
#[cfg(not(windows))]
async fn list_local_tmux_sessions() -> Vec<TmuxSessionInfo> {
    let mut text = match local_tmux_output(&["list-sessions", "-F", TMUX_LIST_FORMAT]).await {
        Ok((_code, stdout)) => stdout,
        Err(e) => {
            log_debug("PTY", &format!("list_local_tmux_sessions: {e} — returning empty list"));
            return vec![];
        }
    };
    if let Some(meta) = dirs::home_dir()
        .and_then(|h| {
            // winmux -> ymux rename: read the current spelling, fall back to
            // the pre-rename one for a Mac set up by an older build.
            std::fs::read_to_string(h.join(".ymux").join("session-meta.json"))
                .or_else(|_| std::fs::read_to_string(h.join(".winmux").join("session-meta.json")))
                .ok()
        })
    {
        text.push('\n');
        text.push_str(TMUX_META_MARKER);
        text.push('\n');
        text.push_str(&meta);
    }
    parse_tmux_sessions(&text)
}

/// Phase 80: parse `tmux list-sessions -F '<name>|<created>|<attached>|
/// <windows>|<last_attached>'` output — shared by the SSH and WSL list
/// paths. Phase 81: the SSH script appends the server-side session-meta
/// JSON after a `TMUX_META_MARKER` marker; when present, label /
/// claude_title / origin are joined onto the sessions. Garbled or absent
/// JSON degrades to no metadata, never to an error.
fn parse_tmux_sessions(text: &str) -> Vec<TmuxSessionInfo> {
    let (list_text, meta_text) = match text.split_once(TMUX_META_MARKER) {
        Some((a, b)) => (a, b),
        None => (text, ""),
    };
    let meta: SessionMetaFileMirror =
        serde_json::from_str(meta_text.trim()).unwrap_or_default();
    let mut out = Vec::new();
    for line in list_text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 5 { continue; }
        let m = meta.sessions.get(parts[0]);
        out.push(TmuxSessionInfo {
            name: parts[0].to_string(),
            created: parts[1].parse().unwrap_or(0),
            attached: parts[2] == "1",
            windows: parts[3].parse().unwrap_or(0),
            last_attached: parts[4].parse().unwrap_or(0),
            exited: false, // tmux does not keep dead sessions around
            label: m.and_then(|m| m.label.clone()),
            claude_title: m.and_then(|m| m.claude_title.clone()),
            auto_name: m.and_then(|m| m.auto_name.clone()),
            claude_session_id: m.and_then(|m| m.claude_session_id.clone()),
            origin: m.and_then(|m| m.origin.clone()),
            // 2026-08-23: the 6th field. `get(5)` rather than `parts[5]`
            // deliberately — a tmux old enough to predate this change (or a
            // remote that has not been re-bootstrapped) emits five fields,
            // and the length guard above still admits that line. Unknown cwd
            // must stay `None`; see the field doc for why that is not the
            // same as "this session is somewhere else".
            cwd: parts
                .get(5)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            // Scope bits are stamped by the caller that knows which workspace
            // is asking (`annotate_session_scope`); the parser has no idea.
            owned: false,
            in_cwd: false,
            foreign: None,
            owner_cwd: None,
        });
    }
    out.sort_by(|a, b| b.last_attached.max(b.created).cmp(&a.last_attached.max(a.created)));
    out
}

/// Phase 80 (session restore): "is the session this pane was on still alive?",
/// answered over the PANE's own connection.
///
/// `pane_list_tmux_sessions` can only answer for a workspace that has a live
/// SSH handle, which at app start it doesn't — and `workspace_ensure_connected`
/// can't open one for a workspace whose panes each carry their own connection
/// (it reads `workspace.connection` and no-ops without it). That left restore
/// with no way to check liveness for those workspaces, and attaching blind
/// would make `tmux new-session -A` CREATE an empty session under the
/// remembered name — the opposite of what a user wants when their session is
/// gone. This gives it a real answer.
///
/// Three-way return, and the distinction is the whole point:
///   `Some(list)` — we asked the host: these sessions exist (possibly none).
///   `None`       — we could NOT ask (not SSH, or the headless connect failed:
///                  password-only, passphrase-locked, unknown host key, host
///                  down). The caller must leave the pane on [Connect] rather
///                  than guess.
/// Never `Err` for an unreachable host: "couldn't ask" is an answer here, not
/// a failure to report to the user.
///
/// Same auth posture as `workspace_ensure_connected`: agent/key only, no
/// password, and never auto-accepts an unknown host key — a restore probe must
/// not be able to raise a prompt or trust a new key on the user's behalf.
#[tauri::command]
async fn pane_probe_tmux_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
) -> Result<Option<Vec<TmuxSessionInfo>>, String> {
    // A pane that's already connected (or a sibling on the same workspace)
    // gives us a handle for free — no second handshake.
    let live = {
        let sessions = state.core.sessions.lock().unwrap();
        sessions.iter().find_map(|(_sid, sess)| match sess {
            Session::Ssh(s) if s.workspace_id == workspace_id => Some(s.handle.clone()),
            _ => None,
        })
    };
    if let Some(h) = live {
        return list_tmux_sessions_via_handle(&h).await.map(Some);
    }

    // Resolve the pane's effective connection: its own first, then the
    // workspace's — the same precedence `pane_connect` uses.
    let conn = {
        let file = state.workspaces.lock().unwrap();
        let ws = match file.workspaces.iter().find(|w| w.id == workspace_id) {
            Some(w) => w,
            None => return Ok(None),
        };
        let layout = match ws.layout.as_ref() {
            Some(l) => l,
            None => return Ok(None),
        };
        find_pane_connection(layout, &pane_id).or_else(|| {
            if pane_id_exists_in(layout, &pane_id) {
                ws.connection.clone()
            } else {
                None
            }
        })
    };
    let (host, user, port, key_path) = match conn {
        Some(Connection::Ssh {
            host,
            user,
            port,
            key_path,
        }) => (host, user, port, key_path),
        // macOS port: a local pane asks the tmux server on this machine —
        // a definite answer (possibly "no sessions"), never "couldn't ask".
        #[cfg(not(windows))]
        Some(Connection::Local { .. }) => return Ok(Some(list_local_tmux_sessions().await)),
        // A WSL pane answers without any handshake — wsl.exe reaches the
        // distro's tmux server cold. Returning Ok(None) here (the old `_`
        // arm) meant "couldn't ask", and the restore loop correctly left
        // such panes alone — so a WSL pane carrying its OWN connection
        // inside a connection-less workspace could never restore.
        Some(Connection::Wsl { distro }) => {
            let (_code, text) =
                local_setup::wsl_exec(distro.as_deref(), None, &tmux_list_script())
                    .await
                    .unwrap_or((-1, String::new()));
            return Ok(Some(parse_tmux_sessions(&text)));
        }
        // 2026-08-19: a native Windows pane answers from zellij, from cold —
        // no handle, no auth. Previously it fell into the `_` arm below and
        // returned None ("couldn't ask"), so boot-time session restore
        // skipped local panes entirely.
        //
        // 2026-08-23: cfg-split, same fix as in `list_workspace_tmux_sessions`
        // and for the same reason — this arm asked ZELLIJ on every platform,
        // so a macOS local pane (whose persistence is tmux, by the deliberate
        // split in CLAUDE.md § Platforms) was told "no sessions" and its
        // restore was skipped. One multiplexer per platform, everywhere.
        None | Some(Connection::Local { .. }) => {
            #[cfg(windows)]
            {
                return Ok(Some(list_zellij_sessions().await));
            }
            #[cfg(not(windows))]
            {
                return Ok(Some(list_local_tmux_sessions().await));
            }
        }
        _ => return Ok(None),
    };

    match connect_and_authenticate(&host, &user, port, key_path.as_deref(), None, None, false).await
    {
        Ok(SshHandshake { handle, .. }) => {
            let out = list_tmux_sessions_via_handle(&handle).await;
            // The probe's handle is disposable — one exec and gone. The real
            // pane connect opens its own; keeping this one alive would leave a
            // second authenticated session per host with nothing reading it.
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            out.map(Some)
        }
        Err(e) => {
            log_debug("SSH", &format!(
                "pane_probe_tmux_sessions: cannot reach {user}@{host}:{port} headlessly ({e}) — pane {pane_id} stays on Connect"
            ));
            Ok(None)
        }
    }
}

// ─── Phase 23.K: local tmux session labels ─────────────────────────────────
//
// User-friendly Hebrew/Arabic/CJK label for each tmux session, stored
// locally on the Windows host (NOT in tmux itself). The Phase 23.I
// experiment of actually renaming the remote tmux session crashed on
// Hebrew (see Phase 23.J root-cause notes). Labels sidestep that
// entirely: tmux session names stay ASCII / safe, but the picker UI
// shows whatever the user typed in the pane title.
//
// File: %APPDATA%/ymux/tmux-labels.json
// Schema: { version: 1, labels: { workspace_id: { session_name: label } } }

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct TmuxLabelsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub labels: HashMap<String, HashMap<String, String>>,
}

fn tmux_labels_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("tmux-labels.json"))
}

fn load_tmux_labels() -> TmuxLabelsFile {
    let path = match tmux_labels_path() {
        Ok(p) => p,
        Err(_) => return TmuxLabelsFile::default(),
    };
    if !path.exists() {
        return TmuxLabelsFile::default();
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            log_warn("WORKSPACE", &format!("tmux-labels: read failed: {e}"));
            return TmuxLabelsFile::default();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log_warn("WORKSPACE", &format!("tmux-labels: parse failed: {e}"));
        TmuxLabelsFile::default()
    })
}

fn save_tmux_labels(file: &TmuxLabelsFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = tmux_labels_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("tmux-labels.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Internal helper used by both the tauri command and pane_set_title's
/// auto-label hook. Empty label clears the entry; clearing the last
/// entry in a workspace also removes the workspace key for cleanliness.
fn set_tmux_label_internal(workspace_id: &str, session_name: &str, label: &str) {
    let mut file = load_tmux_labels();
    let trimmed = label.trim();
    if trimmed.is_empty() {
        if let Some(ws_map) = file.labels.get_mut(workspace_id) {
            ws_map.remove(session_name);
            if ws_map.is_empty() {
                file.labels.remove(workspace_id);
            }
        }
    } else {
        file.labels
            .entry(workspace_id.to_string())
            .or_insert_with(HashMap::new)
            .insert(session_name.to_string(), trimmed.to_string());
    }
    if let Err(e) = save_tmux_labels(&file) {
        log_warn("WORKSPACE", &format!("tmux-labels: save failed: {e}"));
    }
}

/// Phase 87: a real `tmux rename-session` moved the session; move its local
/// label with it so the picker does not drop the user's title on the floor.
fn rename_tmux_label(workspace_id: &str, old_name: &str, new_name: &str) -> Option<String> {
    let mut file = load_tmux_labels();
    let ws_map = file.labels.get_mut(workspace_id)?;
    let label = ws_map.remove(old_name)?;
    ws_map.insert(new_name.to_string(), label.clone());
    if let Err(e) = save_tmux_labels(&file) {
        log_warn("WORKSPACE", &format!("tmux-labels: save failed: {e}"));
    }
    Some(label)
}

#[tauri::command]
fn tmux_labels_get(workspace_id: String) -> HashMap<String, String> {
    let file = load_tmux_labels();
    file.labels.get(&workspace_id).cloned().unwrap_or_default()
}

#[tauri::command]
fn tmux_label_set(
    workspace_id: String,
    session_name: String,
    label: Option<String>,
) -> Result<(), String> {
    if session_name.is_empty() {
        return Err("session_name cannot be empty".into());
    }
    set_tmux_label_internal(&workspace_id, &session_name, label.as_deref().unwrap_or(""));
    Ok(())
}

// ─── 2026-08-23: session ownership ───────────────────────────────────────────
// File: %APPDATA%/ymux/session-owners.json
// Schema: { version: 1, owners: { host_key: { session_name: SessionOwner } } }
//
// WHY THIS EXISTS AT ALL, given tmux reports `#{session_path}`: zellij does
// not. `zellij list-sessions -n` emits a name, an age and the EXITED/current
// markers, full stop — so on Windows, where every local pane is a zellij
// session, a cwd-based scope would match nothing and the picker's "this
// folder" view would always be empty. This is the other half of the answer,
// and it is the half that also covers a tmux session whose cwd has moved.
//
// Keyed by host FIRST because a session name is only unique per tmux/zellij
// server: two boxes each running a session called `dev` are two sessions, and
// collapsing them would let one workspace claim the other's.

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct SessionOwner {
    pub workspace_id: String,
    /// The workspace cwd at the time of the claim. Recorded for diagnostics
    /// and for a future "the folder moved" repair; the scope check does not
    /// read it (it uses the LIVE `#{session_path}` instead, which cannot go
    /// stale the way a cached copy can).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct SessionOwnersFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub owners: HashMap<String, HashMap<String, SessionOwner>>,
}

fn session_owners_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("session-owners.json"))
}

fn load_session_owners() -> SessionOwnersFile {
    let path = match session_owners_path() {
        Ok(p) => p,
        Err(_) => return SessionOwnersFile::default(),
    };
    if !path.exists() {
        return SessionOwnersFile::default();
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            log_warn("WORKSPACE", &format!("session-owners: read failed: {e}"));
            return SessionOwnersFile::default();
        }
    };
    // A corrupt file degrades to "nothing is owned", which costs the user a
    // wider picker list — never a lost session.
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log_warn("WORKSPACE", &format!("session-owners: parse failed: {e}"));
        SessionOwnersFile::default()
    })
}

/// Rule #7: write to `<file>.tmp`, fsync, rename. Same shape as
/// `save_tmux_labels` — deliberately, so the two files cannot acquire
/// different durability behaviour by accident.
fn save_session_owners(file: &SessionOwnersFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = session_owners_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("session-owners.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// The host a workspace's sessions live on, as a stable key. `"local"` covers
/// both `Connection::Local` and the codebase's "no connection = local"
/// default; WSL is keyed by distro because two distros are two tmux servers.
fn session_owner_host_key(conn: Option<&Connection>) -> String {
    match conn {
        Some(Connection::Ssh {
            user, host, port, ..
        }) => bootstrap_guard::host_key(user, host, *port),
        Some(Connection::Wsl { distro }) => {
            format!("wsl:{}", distro.as_deref().unwrap_or("default"))
        }
        Some(Connection::Local { .. }) | None => "local".to_string(),
    }
}

/// Record that `workspace_id` created or attached `session_name` on `host_key`.
/// Idempotent; the newest claim wins. Called from `pane_connect` for BOTH a
/// fresh session and an explicit picker attach — picking a session by hand is
/// exactly as much a statement of "this belongs to my workspace" as making one.
fn claim_session_owner(host_key: &str, session_name: &str, workspace_id: &str, cwd: Option<&str>) {
    if session_name.is_empty() || workspace_id.is_empty() {
        return;
    }
    let mut file = load_session_owners();
    file.owners
        .entry(host_key.to_string())
        .or_default()
        .insert(
            session_name.to_string(),
            SessionOwner {
                workspace_id: workspace_id.to_string(),
                cwd: cwd.map(|s| s.to_string()).filter(|s| !s.is_empty()),
                ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            },
        );
    if let Err(e) = save_session_owners(&file) {
        log_warn("WORKSPACE", &format!("session-owners: save failed: {e}"));
    }
}

/// Drop a claim — a killed session must not keep colouring the picker.
fn release_session_owner(host_key: &str, session_name: &str) {
    let mut file = load_session_owners();
    let Some(host) = file.owners.get_mut(host_key) else {
        return;
    };
    if host.remove(session_name).is_none() {
        return;
    }
    if host.is_empty() {
        file.owners.remove(host_key);
    }
    if let Err(e) = save_session_owners(&file) {
        log_warn("WORKSPACE", &format!("session-owners: save failed: {e}"));
    }
}

/// Phase 87: carry a claim across a real `tmux rename-session`. Without this
/// the renamed session would read as unowned in every picker and the old name
/// would keep a claim on a session that no longer exists.
fn rename_session_owner(host_key: &str, old_name: &str, new_name: &str) {
    let mut file = load_session_owners();
    let Some(host) = file.owners.get_mut(host_key) else {
        return;
    };
    let Some(owner) = host.remove(old_name) else {
        return;
    };
    host.insert(new_name.to_string(), owner);
    if let Err(e) = save_session_owners(&file) {
        log_warn("WORKSPACE", &format!("session-owners: save failed: {e}"));
    }
}

/// Is `path` the same directory as `root`, or inside it?
///
/// A bare `starts_with` gets this wrong in the way that matters: `/srv/app2`
/// starts with `/srv/app`, and a user with `app` and `app2` side by side would
/// see one folder's sessions leak into the other's. The boundary has to be a
/// separator. Trailing separators are trimmed first so `/srv/app/` and
/// `/srv/app` are one directory, and both separators are accepted because the
/// same comparison runs against Windows paths from zellij-era ownership rows.
fn path_is_within(path: &str, root: &str) -> bool {
    let trim = |s: &str| s.trim().trim_end_matches(['/', '\\']).to_string();
    let (path, root) = (trim(path), trim(root));
    if root.is_empty() || path.is_empty() {
        return false;
    }
    if path == root {
        return true;
    }
    path.strip_prefix(&root)
        .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

/// The last segment of a path, accepting BOTH separators.
///
/// One picker list mixes POSIX paths straight off a Linux host with Windows
/// paths recorded by a zellij-era ownership row, so `Path::file_name` (which
/// is `cfg`-dependent about `/`) is the wrong tool. Trailing separators are
/// trimmed first, so `/srv/app/` names `app` and not the empty string.
fn last_path_segment(p: &str) -> Option<&str> {
    let p = p.trim().trim_end_matches(['/', '\\']);
    if p.is_empty() {
        // A bare root ("/", "C:\\") trims to nothing and has no segment to name.
        return None;
    }
    p.rsplit(['/', '\\']).next().filter(|seg| !seg.is_empty())
}

/// Stamp `owned` / `in_cwd` / `foreign` on a freshly listed set of sessions.
///
/// Annotating rather than filtering is deliberate: the picker's scope toggle
/// then flips between two views of ONE response instead of re-querying the
/// host, and a session that falls outside the scope is still one click away
/// rather than gone.
///
/// Loads `session-owners.json` and hands off to `annotate_scope_with`, which
/// holds the actual verdicts and is the thing the tests exercise.
fn annotate_session_scope(
    sessions: &mut [TmuxSessionInfo],
    host_key: &str,
    workspace_id: &str,
    project_path: Option<&str>,
    ws_names: &HashMap<String, String>,
) {
    let owners = load_session_owners();
    annotate_scope_with(
        sessions,
        owners.owners.get(host_key),
        workspace_id,
        project_path,
        ws_names,
    );
}

/// The scope verdicts, with the owners map injected — no file I/O, so the
/// rules below are unit-testable.
///
/// `foreign` answers "do we KNOW this belongs somewhere else?", in this order:
///
/// 1. Anything inside our own scope (`owned || in_cwd`) is never foreign — the
///    badge is therefore structurally impossible in the picker's "This folder"
///    view, which is exactly the complement of that predicate.
/// 2. An ownership row naming a DIFFERENT workspace. This is the only signal
///    that survives on Windows, where zellij reports no directory at all.
/// 3. A live `cwd` that is not under the caller's `project_path`.
/// 4. Otherwise not foreign — which includes "no cwd, no owner", i.e. we have
///    nothing to say. Silence, never a guess: `cwd: None` is unknown, and the
///    picker must not paint an unclaimed session as another project's.
fn annotate_scope_with(
    sessions: &mut [TmuxSessionInfo],
    host: Option<&HashMap<String, SessionOwner>>,
    workspace_id: &str,
    project_path: Option<&str>,
    ws_names: &HashMap<String, String>,
) {
    for s in sessions.iter_mut() {
        let owner = host.and_then(|h| h.get(&s.name));
        s.owned = owner.is_some_and(|o| o.workspace_id == workspace_id);
        // Phase 87: surfaced for grouping only — see the field doc.
        s.owner_cwd = owner.and_then(|o| o.cwd.clone());
        s.in_cwd = match (s.cwd.as_deref(), project_path) {
            (Some(cwd), Some(root)) => path_is_within(cwd, root),
            // Unknown cwd is not evidence of anything. See TmuxSessionInfo::cwd.
            _ => false,
        };
        s.foreign = if s.owned || s.in_cwd {
            // Inside the workspace's own scope. Two workspaces pinned to the
            // same folder both belong there; warning about that would be a lie,
            // and it would put the badge in the "This folder" view.
            None
        } else if let Some(o) = owner {
            // Claimed by a DIFFERENT workspace — the same-id case is `owned`,
            // which the arm above already took.
            Some(ForeignScope {
                kind: ForeignKind::Workspace,
                // The workspace name is the most useful thing we can say. When
                // it has since been deleted we still know WHERE it was, so fall
                // through to the folder before giving up on a name.
                label: ws_names
                    .get(&o.workspace_id)
                    .map(|n| n.trim())
                    .filter(|n| !n.is_empty())
                    .map(|n| n.to_string())
                    .or_else(|| {
                        o.cwd
                            .as_deref()
                            .and_then(last_path_segment)
                            .map(|seg| seg.to_string())
                    }),
                path: s.cwd.clone().or_else(|| o.cwd.clone()),
            })
        } else if let (Some(cwd), Some(_root)) = (s.cwd.as_deref(), project_path) {
            // Unclaimed, but demonstrably somewhere else. Only reachable with a
            // KNOWN cwd and a root to compare it against: `in_cwd` was already
            // false above, and with no root the "no evidence" arm below wins.
            Some(ForeignScope {
                kind: ForeignKind::Folder,
                label: last_path_segment(cwd).map(|seg| seg.to_string()),
                path: Some(cwd.to_string()),
            })
        } else {
            // Unknown cwd, nobody claimed it. We have nothing to say.
            None
        };
    }
}

/// Phase 87: is `name` something we are willing to hand `tmux rename-session`?
///
/// ASCII letters, digits, `_` and `-` only, at most 64 chars. Phase 23.I's
/// experiment with real renames crashed on a Hebrew name — a
/// `STATUS_STACK_BUFFER_OVERRUN` in the Windows process with no Rust trace,
/// never root-caused — which is why labels exist and why the standing rule
/// was "no tmux rename anywhere". The active-sessions overview lifts that for
/// an EXPLICIT user action, inside this whitelist; anything else keeps going
/// through labels. Stricter than `session_name_char_is_safe`, deliberately.
fn validate_tmux_rename_target(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name cannot be empty".into());
    }
    if name.len() > 64 {
        return Err("name is too long (max 64 characters)".into());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("name may only contain ASCII letters, digits, '_' and '-'".into());
    }
    Ok(())
}

/// Phase 23.G / Phase 87: rename a multiplexer session for real.
///
/// Registered since 23.G with no frontend caller (the in-picker Rename was
/// removed in 23.I when the pane title became the session name). Phase 87's
/// active-sessions overview is the caller now, and it made the command grow
/// three things: an ASCII whitelist (`validate_tmux_rename_target`), the
/// local-tmux and WSL arms, and the migration of everything ymux keys by
/// session name — live `Session.tmux_session` fields (so Kill / reconnect on
/// the attached pane keep working), `session-owners.json`, `tmux-labels.json`
/// and, over SSH, the server-side session-meta label. `auto_name` /
/// `claude_title` for the old key are lost until the next Claude turn
/// rewrites them; accepted.
///
/// zellij is refused: `docs/ZELLIJ.md` §1 keeps `action rename-session` out
/// on purpose, because a zellij session's name is derived from the pane id so
/// a cold start can find it again.
#[tauri::command]
async fn tmux_rename_session(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    if old_name.is_empty() {
        return Err("old_name cannot be empty".into());
    }
    validate_tmux_rename_target(&new_name)?;
    if new_name == old_name {
        return Err("new name is the same as the old one".into());
    }
    let conn = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.connection.clone())
            .ok_or_else(|| "workspace not found".to_string())?
    };
    let host_key = session_owner_host_key(conn.as_ref());
    // `=` forces an exact session match; a bare `-t` prefix-matches, and a
    // rename that landed on `dev-2` when the user meant `dev` is the kind of
    // surprise this dialog exists to prevent.
    let exact_old = format!("={old_name}");
    let ssh_handle = match &conn {
        Some(Connection::Ssh { .. }) => {
            let handle = {
                let sessions = state.core.sessions.lock().unwrap();
                sessions
                    .iter()
                    .find_map(|(_sid, sess)| match sess {
                        Session::Ssh(s) if s.workspace_id == workspace_id => {
                            Some(s.handle.clone())
                        }
                        _ => None,
                    })
            }
            .ok_or_else(|| "no active SSH session for this workspace".to_string())?;
            tmux_rename_session_via_handle(&handle, &old_name, &new_name).await?;
            Some(handle)
        }
        Some(Connection::Wsl { distro }) => {
            // Pure argv — tmux receives both names verbatim (Rule #3).
            let mut c = local_setup::wsl_cmd();
            if let Some(d) = distro.as_deref().filter(|d| !d.is_empty()) {
                c.arg("-d").arg(d);
            }
            c.arg("--")
                .arg("tmux")
                .arg("rename-session")
                .arg("-t")
                .arg(&exact_old)
                .arg(&new_name);
            let out = c
                .output()
                .await
                .map_err(|e| format!("wsl.exe: {e}"))?;
            if !out.status.success() {
                let err = local_setup::clean_wsl_output(&out.stderr).trim().to_string();
                return Err(if err.is_empty() {
                    format!("tmux exit {:?}", out.status.code())
                } else {
                    err
                });
            }
            None
        }
        #[cfg(windows)]
        Some(Connection::Local { .. }) | None => {
            return Err("renaming a zellij session is not supported".into());
        }
        #[cfg(not(windows))]
        Some(Connection::Local { .. }) | None => {
            match local_tmux_output(&["rename-session", "-t", &exact_old, &new_name]).await? {
                (Some(0), _) => None,
                (code, out) => {
                    let out = out.trim().to_string();
                    return Err(if out.is_empty() {
                        format!("tmux exit {code:?}")
                    } else {
                        out
                    });
                }
            }
        }
    };

    // The multiplexer agreed. Now move everything ymux keys by the old name.
    {
        let mut sessions = state.core.sessions.lock().unwrap();
        for sess in sessions.values_mut() {
            match sess {
                Session::Ssh(s)
                    if s.workspace_id == workspace_id
                        && s.tmux_session.as_deref() == Some(old_name.as_str()) =>
                {
                    s.tmux_session = Some(new_name.clone());
                }
                Session::Local(l)
                    if ssh_handle.is_none()
                        && l.tmux_session.as_deref() == Some(old_name.as_str()) =>
                {
                    l.tmux_session = Some(new_name.clone());
                }
                _ => {}
            }
        }
    }
    rename_session_owner(&host_key, &old_name, &new_name);
    let label = rename_tmux_label(&workspace_id, &old_name, &new_name);
    // Phase 87.B: a session row in the tree is keyed by this name too.
    let rows_moved = {
        let mut file = state.workspaces.lock().unwrap();
        let mut n = 0;
        for w in file.workspaces.iter_mut() {
            if conn_same_host(&w.connection, &conn)
                && w.tmux_session.as_deref() == Some(old_name.as_str())
            {
                w.tmux_session = Some(new_name.clone());
                n += 1;
            }
        }
        n
    };
    if rows_moved > 0 {
        persist(&state)?;
        let _ = app.emit("workspaces:changed", ());
    }
    log_info(
        "WORKSPACE",
        &format!("tmux_rename_session: renamed on {host_key} (ws={workspace_id})"),
    );
    // Phase 81's server-side session-meta is keyed by name too. There is no
    // rename verb there; re-set the label under the new name (the CLI
    // lazy-prunes the orphaned old key on its next write). Fire-and-forget,
    // same shape as `pane_set_title`.
    if let (Some(handle), Some(label)) = (ssh_handle, label) {
        let cmd = format!(
            "\"$HOME/.ymux/bin/ymux-linux-x64\" session-meta set --session {} --label-hex {} 2>/dev/null || true",
            shell_quote(&new_name),
            hex_utf8(&label),
        );
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::updater::ssh_exec_simple(&handle, &cmd).await {
                log_warn("SSH", &format!("session-meta: label write failed: {e}"));
            }
        });
    }
    Ok(())
}

/// Phase 12.B: Claude Code session metadata returned by
/// pane_list_claude_sessions for the session-picker modal.
#[derive(Clone, Serialize)]
pub(crate) struct ClaudeSessionInfo {
    pub session_id: String,
    pub project_path: String,
    pub jsonl_path: String,
    pub mtime_unix: i64,
    /// First user message preview (best-effort; first ~80 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_user: Option<String>,
    /// Last assistant message preview (best-effort).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant: Option<String>,
    /// v0.4.4-beta.2: true when the transcript is a sub-agent (Task) sidechain
    /// (`"isSidechain":true` in the JSONL) rather than a main user session.
    /// Drives the Main/Sub/All filter in the resume picker. Defaults false.
    #[serde(default)]
    pub is_subagent: bool,
}

/// Phase 12.B: list recent Claude Code sessions on the workspace's host.
/// For SSH workspaces with a live session, reuses the existing SSH handle
/// to open a fresh exec channel (no extra auth round-trip). For local
/// workspaces, reads `~/.claude/projects/*/sessions/*.jsonl` directly.
/// Best-effort: if the path doesn't exist or jq isn't installed we still
/// return what we can (path + mtime, no previews).
#[tauri::command]
async fn pane_list_claude_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
    limit: Option<usize>,
    project_path: Option<String>,
) -> Result<Vec<ClaudeSessionInfo>, String> {
    let limit = limit.unwrap_or(30).min(200);
    let scope = project_path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    // Locate any live SSH handle for this workspace. The shell command runs
    // on the remote where Claude Code is actually installed.
    let handle_opt = {
        let sessions = state.core.sessions.lock().unwrap();
        sessions
            .iter()
            .find_map(|(_sid, sess)| match sess {
                Session::Ssh(s) if s.workspace_id == workspace_id => Some(s.handle.clone()),
                _ => None,
            })
    };

    // Claude Code stores sessions under ~/.claude/projects/<encoded-cwd>/,
    // where <encoded-cwd> is the absolute working directory with every
    // non-alphanumeric character replaced by `-`. Names over 200 chars are
    // truncated and hashed, so this narrows with a PREFIX glob (quoted
    // segment, bare `*`) and leaves the authoritative check to the `cwd`
    // field inside the JSONL — the directory name is a lossy encoding.
    let root = match scope.as_deref() {
        Some(p) => format!(
            "\"$HOME/.claude/projects\"/{}*",
            shell_quote(&claude_project_dir_prefix(p))
        ),
        None => "\"$HOME/.claude/projects\"".to_string(),
    };
    let script = format!(
        "find {root} -maxdepth 4 -name '*.jsonl' \
         -printf '%T@\\t%p\\n' 2>/dev/null | sort -rn | head -{} | \
         while IFS=$'\\t' read -r mt path; do \
           cwd=$(head -50 \"$path\" 2>/dev/null | \
             grep -m1 -oE '\"cwd\"[[:space:]]*:[[:space:]]*\"[^\"]*\"' | \
             sed -E 's/.*:[[:space:]]*\"([^\"]*)\".*/\\1/'); \
           sub=$(grep -qm1 '\"isSidechain\"[[:space:]]*:[[:space:]]*true' \"$path\" 2>/dev/null && echo 1 || echo 0); \
           first_user=$(head -100 \"$path\" 2>/dev/null | \
             grep -m1 -E '\"role\"\\s*:\\s*\"user\"' | head -c 600); \
           last_asst=$(tail -200 \"$path\" 2>/dev/null | \
             grep -E '\"role\"\\s*:\\s*\"assistant\"' | tail -1 | head -c 600); \
           printf '%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"$mt\" \"$path\" \"$cwd\" \"$sub\" \"$first_user\" \"$last_asst\"; \
         done",
        limit
    );

    let raw = if let Some(handle) = handle_opt {
        // Run via SSH exec.
        let mut ch = handle
            .channel_open_session()
            .await
            .map_err(|e| format!("channel_open: {e}"))?;
        ch.exec(true, script.as_bytes())
            .await
            .map_err(|e| format!("exec: {e}"))?;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while let Some(msg) = ch.wait().await {
                match msg {
                    russh::ChannelMsg::Data { ref data } => out.extend_from_slice(data),
                    russh::ChannelMsg::ExtendedData { .. } => {}
                    russh::ChannelMsg::Eof | russh::ChannelMsg::Close | russh::ChannelMsg::ExitStatus { .. } => break,
                    _ => {}
                }
            }
        })
        .await;
        let _ = ch.close().await;
        String::from_utf8_lossy(&out).to_string()
    } else {
        // No SSH session live → run locally on Windows. Translate to a small
        // walk of %USERPROFILE%\.claude\projects\*\*.jsonl. We don't try to
        // mirror the full bash pipeline — just enumerate, sort by mtime,
        // return path + mtime; previews are skipped.
        return list_claude_sessions_local(limit, scope.as_deref());
    };

    let mut out = Vec::new();
    for line in raw.lines() {
        // Phase 65 (bug Y): fields are now mt \t path \t cwd \t first_user
        // \t last_asst. cwd is the REAL project dir read from inside the
        // JSONL (`"cwd":"…"`) — the source of truth. The old code derived
        // project_path from the on-disk dir name, which Claude encodes by
        // replacing `/` with `-` (`-home-runner-tax`); resuming with that
        // produced `cd '-home-runner-tax'` → `cd: -h: invalid option`.
        let parts: Vec<&str> = line.splitn(6, '\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let mtime = parts[0]
            .split('.')
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let path = parts[1].to_string();
        let cwd_field = parts.get(2).map(|s| s.trim()).filter(|s| !s.is_empty());
        // The directory glob above is only a narrowing hint; THIS is the
        // check. A session whose real cwd is elsewhere never belongs in a
        // folder-scoped list.
        if let Some(want) = scope.as_deref() {
            if cwd_field.map(|c| !paths_equal(c, want)).unwrap_or(true) {
                continue;
            }
        }
        let is_subagent = parts.get(3).map(|s| s.trim() == "1").unwrap_or(false);
        let last_user = parts.get(4).map(|s| extract_text_field(s));
        let last_asst = parts.get(5).map(|s| extract_text_field(s));
        let session_id = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let project_path = cwd_field.map(|s| s.to_string()).unwrap_or_else(|| {
            // Fallback (no cwd in the JSONL): the encoded dir name. Only
            // used for display — the frontend won't `cd` to a path that
            // doesn't start with `/`.
            std::path::Path::new(&path)
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string()
        });
        out.push(ClaudeSessionInfo {
            session_id,
            project_path,
            jsonl_path: path,
            mtime_unix: mtime,
            last_user: last_user.filter(|s| !s.is_empty()),
            last_assistant: last_asst.filter(|s| !s.is_empty()),
            is_subagent,
        });
    }
    Ok(out)
}

/// Encode an absolute path the way Claude Code names its project dirs:
/// every non-alphanumeric character becomes `-`. Used only to NARROW a
/// search, never to reconstruct a path — the encoding is lossy in both
/// directions (Phase 65 bug: `cd '-home-runner-tax'`).
fn claude_project_dir_prefix(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Compare two directory paths for the session scope: separators and a
/// trailing slash must not decide whether a session belongs to a folder.
fn paths_equal(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    norm(a) == norm(b)
}

/// Read the `"cwd"` field out of a session transcript. That field is the
/// source of truth for which project a session belongs to; the on-disk
/// directory NAME is the lossy encoding above.
fn claude_session_cwd(path: &std::path::Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(f).lines().take(50) {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                if !c.is_empty() {
                    return Some(c.to_string());
                }
            }
        }
    }
    None
}

fn list_claude_sessions_local(
    limit: usize,
    scope: Option<&str>,
) -> Result<Vec<ClaudeSessionInfo>, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let root = home.join(".claude").join("projects");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(std::path::PathBuf, i64)> = Vec::new();
    if let Ok(it) = std::fs::read_dir(&root) {
        for proj in it.flatten() {
            if let Ok(it2) = std::fs::read_dir(proj.path()) {
                for f in it2.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        let mtime = f
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| {
                                t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
                            })
                            .unwrap_or(0);
                        entries.push((p, mtime));
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    // Filter BEFORE truncating, or a scoped list silently comes back
    // short because the newest N sessions belonged to other projects.
    if let Some(want) = scope {
        entries.retain(|(p, _)| {
            claude_session_cwd(p)
                .map(|c| paths_equal(&c, want))
                .unwrap_or(false)
        });
    }
    entries.truncate(limit);
    let mut out = Vec::new();
    for (p, mtime) in entries {
        let session_id = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        // Same contract as the SSH path: `cwd` from inside the JSONL is the
        // real project dir (what `claude --resume` must be launched from);
        // the encoded dir name (`-Users-yossi-dev-foo`) is a display-only
        // fallback the frontend refuses to `cd` to. Without this a picked
        // session on a local workspace resumed from $HOME and Claude
        // couldn't find it. Supersedes `claude_session_cwd` here (still used
        // by the scope filter above): one read yields cwd, previews and the
        // sidechain flag instead of three.
        let peek = peek_claude_jsonl(&p);
        let project_path = peek.cwd.unwrap_or_else(|| {
            p.parent()
                .and_then(|q| q.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string()
        });
        out.push(ClaudeSessionInfo {
            session_id,
            project_path,
            jsonl_path: p.to_string_lossy().to_string(),
            mtime_unix: mtime,
            last_user: peek.first_user,
            last_assistant: peek.last_assistant,
            is_subagent: peek.is_subagent,
        });
    }
    Ok(out)
}

/// What the local session picker needs from one transcript, without
/// reading the whole file (sessions run to tens of MB): the first
/// `PEEK_BYTES` give cwd / sidechain flag / first user line, the last
/// `PEEK_BYTES` give the latest assistant line — the same head/tail
/// window the SSH-side shell pipeline uses.
#[derive(Default)]
struct ClaudeJsonlPeek {
    cwd: Option<String>,
    is_subagent: bool,
    first_user: Option<String>,
    last_assistant: Option<String>,
}

const CLAUDE_PEEK_BYTES: u64 = 256 * 1024;

fn peek_claude_jsonl(path: &Path) -> ClaudeJsonlPeek {
    use std::io::{Seek as _, SeekFrom};
    let mut out = ClaudeJsonlPeek::default();
    let Ok(mut f) = std::fs::File::open(path) else {
        return out;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);

    let mut head = Vec::new();
    let _ = std::io::Read::by_ref(&mut f).take(CLAUDE_PEEK_BYTES).read_to_end(&mut head);
    let head = String::from_utf8_lossy(&head);
    for line in head.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if out.cwd.is_none() {
            out.cwd = v
                .get("cwd")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) {
            out.is_subagent = true;
        }
        if out.first_user.is_none() && v.get("type").and_then(|x| x.as_str()) == Some("user") {
            let text = extract_text_field(line);
            if !text.is_empty() {
                out.first_user = Some(text);
            }
        }
        if out.cwd.is_some() && out.first_user.is_some() {
            break;
        }
    }

    // Tail: last full line of type "assistant". Skip the first (likely
    // partial) line when we seeked into the middle of the file.
    let tail_start = len.saturating_sub(CLAUDE_PEEK_BYTES);
    if f.seek(SeekFrom::Start(tail_start)).is_ok() {
        let mut tail = Vec::new();
        let _ = f.read_to_end(&mut tail);
        let tail = String::from_utf8_lossy(&tail);
        let lines: Vec<&str> = tail.lines().collect();
        let skip = usize::from(tail_start > 0);
        for line in lines.iter().skip(skip).rev() {
            if !line.contains("\"assistant\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("type").and_then(|x| x.as_str()) == Some("assistant") {
                let text = extract_text_field(line);
                if !text.is_empty() {
                    out.last_assistant = Some(text);
                    break;
                }
            }
        }
    }
    out
}

/// Best-effort extractor: pulls the first occurrence of `"text":"…"` (or
/// `"content":"…"` as a fallback) out of a fragment of a JSONL line, with
/// the JSON-escape sequences decoded. Sufficient for the preview column;
/// not a full JSON parser. Returns the trimmed first ~80 chars.
fn extract_text_field(fragment: &str) -> String {
    fn extract_one(s: &str, key: &str) -> Option<String> {
        let needle = format!("\"{}\":\"", key);
        let idx = s.find(&needle)?;
        let mut chars = s[idx + needle.len()..].chars().peekable();
        let mut out = String::new();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => out.push('"'),
                    Some('n') => out.push(' '),
                    Some('t') => out.push(' '),
                    Some('r') => {}
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some(other) => out.push(other),
                    None => break,
                }
            } else if c == '"' {
                break;
            } else {
                out.push(c);
            }
            if out.len() > 600 {
                break;
            }
        }
        Some(out)
    }
    let extracted = extract_one(fragment, "text")
        .or_else(|| extract_one(fragment, "content"))
        .unwrap_or_default();
    let trimmed = extracted.trim();
    if trimmed.chars().count() <= 80 {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(80).collect();
        out.push('…');
        out
    }
}

/// Phase 11.A: introspection — is this pane currently bound to a tmux
/// persistent session? Used by the frontend to render the `T` badge and
/// to decide whether the disconnect dropdown should expose "Kill session".
#[tauri::command]
fn pane_persistence_get(
    state: State<'_, AppState>,
    pane_id: String,
) -> Option<String> {
    let sessions_map = state.core.pane_sessions.lock().unwrap();
    let sid = sessions_map.get(&pane_id)?.clone();
    drop(sessions_map);
    let sessions = state.core.sessions.lock().unwrap();
    match sessions.get(&sid) {
        Some(Session::Ssh(s)) => s.tmux_session.clone(),
        // Phase 80: WSL panes carry their tmux name on LocalSession.
        Some(Session::Local(l)) => l.tmux_session.clone(),
        None => None,
    }
}

/// Phase 11.A: list every (pane_id → tmux_session_name) currently active.
/// Frontend uses this on workspaces:changed / pty:exit to refresh badges
/// without having to query each pane individually.
#[tauri::command]
fn pane_persistence_list(
    state: State<'_, AppState>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let pane_sessions = state.core.pane_sessions.lock().unwrap().clone();
    let sessions = state.core.sessions.lock().unwrap();
    for (pane, sid) in pane_sessions {
        // Phase 80: Local sessions carry a tmux name too (WSL panes).
        let name = match sessions.get(&sid) {
            Some(Session::Ssh(s)) => s.tmux_session.clone(),
            Some(Session::Local(l)) => l.tmux_session.clone(),
            None => None,
        };
        if let Some(name) = name {
            out.insert(pane, name);
        }
    }
    out
}

/// What a kill actually achieved, as reported to the frontend and the CLI.
///
/// Before 2026-08-20 `pane_kill_session` returned `Ok(())` on every branch —
/// including the early return where it did no work at all — so the frontend
/// inferred success from "the invoke did not throw". With zellij uninstalled
/// that produced a Kill that destroyed nothing and said it worked.
///
/// String-tagged rather than a Rust enum with data because this crosses both
/// Tauri IPC and the JSON-RPC surface, and serde's externally-tagged enums
/// make awkward JSON for the CLI to print.
///
/// `Err(String)` (Rule #6) stays reserved for "we could not even try". "This
/// pane has no session" is a normal answer, so it is `no_session`, not an
/// error.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct KillSessionOutcome {
    /// `killed` | `already_gone` | `no_session` | `multiplexer_missing` | `failed`
    pub(crate) result: String,
    /// `zellij` | `tmux` | `none`
    pub(crate) backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    /// Short diagnostic for the log and the toast. The multiplexer's own
    /// stderr, never PTY content (Rule #1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

impl KillSessionOutcome {
    fn new(result: &str, backend: &str, session: Option<String>) -> Self {
        Self {
            result: result.to_string(),
            backend: backend.to_string(),
            session,
            detail: None,
        }
    }
    fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
    /// Did the session end up gone? `no_session` counts: there was nothing to
    /// destroy, so the caller's goal is satisfied either way.
    pub(crate) fn is_gone(&self) -> bool {
        matches!(
            self.result.as_str(),
            "killed" | "already_gone" | "no_session"
        )
    }
}

/// The multiplexer session a kill should hit, resolved without holding any
/// lock across an `.await` — russh's Handle is shared as Arc<> so this is
/// cheap. Phase 80: WSL panes (Session::Local with a tmux name) kill their
/// session via wsl.exe instead of an SSH exec channel.
///
/// Phase 87: lifted out of `kill_pane_session_inner` so the active-sessions
/// overview can kill a session BY NAME (no pane attached to it) through the
/// exact same verbs. Two implementations of "kill" is how the zellij arm
/// once lied about success; there is still one.
pub(crate) enum KillTarget {
    Ssh(Arc<client::Handle<SshClient>>, String),
    Wsl(Option<String>, String),
    // 2026-08-19: a native Windows pane's zellij session.
    #[cfg(windows)]
    Zellij(String),
    /// A persistent LOCAL pane (no distro) off Windows - kill via the
    /// local tmux binary.
    #[cfg(not(windows))]
    LocalUnix(String),
    None,
}

/// Run the kill verb for one target and report what happened. Pure
/// multiplexer work: no pane bookkeeping, no owner release — the callers do
/// that, because they know whether a pane was involved.
pub(crate) async fn kill_target(target: KillTarget) -> KillSessionOutcome {
    match target {
        #[cfg(not(windows))]
        KillTarget::LocalUnix(name) => {
            // Pure argv - tmux receives the name verbatim (Rule #3).
            match local_tmux_output(&["kill-session", "-t", &name]).await {
                Ok((Some(0), _)) => KillSessionOutcome::new("killed", "tmux", Some(name)),
                Ok((code, out)) => {
                    log_warn(
                        "PTY",
                        &format!(
                            "pane_kill_session(local): tmux kill-session exited {code:?}"
                        ),
                    );
                    // tmux says this when the session is already gone, which
                    // is the caller's goal, not a failure.
                    let r = if out.contains("can't find session") {
                        "already_gone"
                    } else {
                        "failed"
                    };
                    KillSessionOutcome::new(r, "tmux", Some(name)).with_detail(out)
                }
                Err(e) => {
                    log_warn("PTY", &format!("pane_kill_session(local): {e}"));
                    // `local_tmux_output` reports a missing binary this way.
                    let r = if e.contains("tmux not found") {
                        "multiplexer_missing"
                    } else {
                        "failed"
                    };
                    KillSessionOutcome::new(r, "tmux", Some(name)).with_detail(e)
                }
            }
        }
        KillTarget::Ssh(handle, name) => {
            // `2>&1` STAYS — it is how tmux's own message reaches us instead
            // of vanishing into the server's stderr. `|| true` is GONE: it
            // forced exit 0, and the drain loop below already receives
            // ExitStatus and used to throw it away, so this arm could only
            // ever claim success. It is the same shape of lie the zellij arm
            // had, one layer down.
            //
            // shell_quote is correct here and stays: unlike every other verb
            // in this function, this one genuinely goes through a remote
            // shell, so Rule #3's argv rule has nothing to apply to.
            let cmd = format!("tmux kill-session -t {} 2>&1", shell_quote(&name));
            let mut status: Option<u32> = None;
            let mut output = String::new();
            let mut transport_err: Option<String> = None;
            match handle.channel_open_session().await {
                Ok(mut ch) => {
                    if let Err(e) = ch.exec(true, cmd.as_bytes()).await {
                        log_warn("SSH", &format!("pane_kill_session: exec failed: {e}"));
                        transport_err = Some(e.to_string());
                    }
                    // Drain the channel briefly so the server completes the
                    // exec — and keep what it said, which is the whole point.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(800),
                        async {
                            while let Some(msg) = ch.wait().await {
                                match msg {
                                    ChannelMsg::Data { ref data } => {
                                        output.push_str(&String::from_utf8_lossy(data));
                                    }
                                    ChannelMsg::ExitStatus { exit_status } => {
                                        status = Some(exit_status);
                                        break;
                                    }
                                    ChannelMsg::Eof | ChannelMsg::Close => break,
                                    _ => {}
                                }
                            }
                        },
                    )
                    .await;
                    let _ = ch.close().await;
                }
                Err(e) => {
                    log_warn("SSH", &format!("pane_kill_session: channel_open failed: {e}"));
                    transport_err = Some(e.to_string());
                }
            }
            let output = output.trim().to_string();
            match (transport_err, status) {
                // Could not reach the host at all. Not a failed kill — an
                // un-attempted one, and it must not drop the restore hint.
                (Some(e), _) => KillSessionOutcome::new("failed", "tmux", Some(name))
                    .with_detail(e),
                (None, Some(0)) => KillSessionOutcome::new("killed", "tmux", Some(name)),
                (None, Some(code)) => {
                    // tmux's wording when the session is already gone. That is
                    // the caller's goal, so it is not a failure.
                    let r = if output.contains("can't find session")
                        || output.contains("session not found")
                    {
                        "already_gone"
                    } else {
                        "failed"
                    };
                    log_warn(
                        "SSH",
                        &format!("pane_kill_session: tmux exited {code}: {output}"),
                    );
                    KillSessionOutcome::new(r, "tmux", Some(name)).with_detail(output)
                }
                // Drained to EOF without an ExitStatus, or timed out. The verb
                // very likely ran, but this does not KNOW that — say so rather
                // than pick a side.
                (None, None) => KillSessionOutcome::new("attempted", "tmux", Some(name))
                    .with_detail(output),
            }
        }
        KillTarget::Wsl(distro, name) => {
            // Pure argv — tmux receives the name verbatim, no shell in
            // between, so no quoting is needed at all here (Rule #3).
            let mut c = local_setup::wsl_cmd();
            if let Some(d) = distro.as_deref() {
                if !d.is_empty() {
                    c.arg("-d").arg(d);
                }
            }
            c.arg("--").arg("tmux").arg("kill-session").arg("-t").arg(&name);
            match c.output().await {
                Ok(out) if out.status.success() => {
                    KillSessionOutcome::new("killed", "tmux", Some(name))
                }
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    log_warn(
                        "PTY",
                        &format!(
                            "pane_kill_session(wsl): tmux kill-session exited {:?}: {err}",
                            out.status.code()
                        ),
                    );
                    // tmux says this when the session is already gone, which
                    // is the caller's goal, not a failure.
                    let r = if err.contains("can't find session") {
                        "already_gone"
                    } else {
                        "failed"
                    };
                    KillSessionOutcome::new(r, "tmux", Some(name)).with_detail(err)
                }
                Err(e) => {
                    log_warn("PTY", &format!("pane_kill_session(wsl): spawn failed: {e}"));
                    let r = if e.kind() == std::io::ErrorKind::NotFound {
                        "multiplexer_missing"
                    } else {
                        "failed"
                    };
                    KillSessionOutcome::new(r, "tmux", Some(name)).with_detail(e.to_string())
                }
            }
        }
        #[cfg(windows)]
        KillTarget::Zellij(name) => {
            // ONE verb. `-f` kills it first if it is running, then deletes the
            // serialized copy, so this covers both the live and the
            // already-exited case — and "Kill session" finally means the
            // session is gone rather than resurrectable.
            //
            // This replaced `kill-session` + a conditional `delete-session`;
            // the doc block on `zellij_args_delete_force` says why that
            // two-step could never be reached from the UI.
            match zellij_try(&zellij_args_delete_force(&name), "delete-session -f").await {
                ZellijOutcome::Ok => KillSessionOutcome::new("killed", "zellij", Some(name)),
                ZellijOutcome::Missing => {
                    KillSessionOutcome::new("multiplexer_missing", "zellij", Some(name))
                }
                ZellijOutcome::Failed { code, stderr } => {
                    // Captured from 0.44.3 on 2026-08-20: deleting a name that
                    // does not exist exits 2 and prints
                    // `Session: "<name>" not found.` on stderr. Nothing to
                    // destroy is the caller's goal, not a failure.
                    let r = if stderr.contains("not found") {
                        "already_gone"
                    } else {
                        "failed"
                    };
                    log_debug(
                        "PTY",
                        &format!(
                            "pane_kill_session(zellij): {r} (exit {code:?}): {stderr}"
                        ),
                    );
                    KillSessionOutcome::new(r, "zellij", Some(name)).with_detail(stderr)
                }
            }
        }
        KillTarget::None => KillSessionOutcome::new("no_session", "none", None),
    }
}

/// Phase 11.A: hard-kill the multiplexer session bound to this pane — tmux
/// over SSH or WSL, zellij on a native Windows pane. Falls through to a plain
/// disconnect for non-persistent panes so `ymux pane-disconnect --kill` is
/// always meaningful regardless of which mode the pane was started in.
///
/// Free function rather than only a `#[tauri::command]` so the JSON-RPC
/// handler can call the SAME code. It used to carry its own hand-rolled copy
/// that matched only `Session::Ssh` — a zellij pane fell through it, no verb
/// ran, and it still answered `killed: true`. Two implementations of "kill"
/// is how that happened; there is now one.
pub(crate) async fn kill_pane_session_inner(
    state: &AppState,
    pane_id: &str,
) -> KillSessionOutcome {
    let sid_opt = state.core.pane_sessions.lock().unwrap().get(pane_id).cloned();
    let Some(sid) = sid_opt else {
        return KillSessionOutcome::new("no_session", "none", None);
    };
    // Snapshot the kill target without holding the lock across the
    // .await — russh's Handle is shared as Arc<> so this is cheap.
    // Phase 80: WSL panes (Session::Local with a tmux name) kill their
    // session via wsl.exe instead of an SSH exec channel.
    let target = {
        let sessions = state.core.sessions.lock().unwrap();
        match sessions.get(&sid) {
            Some(Session::Ssh(s)) => match &s.tmux_session {
                Some(name) => KillTarget::Ssh(s.handle.clone(), name.clone()),
                None => KillTarget::None,
            },
            // `Session::Local` covers BOTH a WSL pane and a native local pane,
            // so the distro - not the variant - is what tells them apart.
            // A native-local kill routed through the single `Some(name)` arm
            // would become `wsl.exe -- tmux kill-session` against the DEFAULT
            // distro. Split explicitly rather than relying on ordering.
            Some(Session::Local(l)) => match (&l.tmux_session, &l.wsl_distro) {
                (Some(name), Some(distro)) => {
                    KillTarget::Wsl(Some(distro.clone()), name.clone())
                }
                #[cfg(windows)]
                (Some(name), None) => KillTarget::Zellij(name.clone()),
                #[cfg(not(windows))]
                (Some(name), None) => KillTarget::LocalUnix(name.clone()),
                (None, _) => KillTarget::None,
            },
            None => KillTarget::None,
        }
    };
    let outcome = kill_target(target).await;
    // Now close the shell + remove session bookkeeping. This re-uses the
    // existing pane_disconnect logic by removing from pane_sessions and
    // killing the underlying session.
    //
    // Unconditional: the PTY goes either way. A multiplexer session we failed
    // to destroy is reported through `outcome`, not by leaving a dead pane
    // wired up in the maps.
    let sid = state.core.pane_sessions.lock().unwrap().remove(pane_id);
    if let Some(sid) = sid {
        if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
            kill_session_inner(&mut s);
        }
    }
    // 2026-08-23: drop the ownership claim. Released on `already_gone` too —
    // the session is not there either way, and a claim on a name that no
    // longer exists would keep colouring the picker for a session the user
    // can never see again. `failed` deliberately keeps the claim: the session
    // is still alive and still this workspace's.
    if matches!(outcome.result.as_str(), "killed" | "already_gone") {
        if let Some(name) = outcome.session.as_deref() {
            let host_key = {
                let file = state.workspaces.lock().unwrap();
                let conn = file
                    .workspaces
                    .iter()
                    .find(|w| {
                        w.layout
                            .as_ref()
                            .is_some_and(|l| pane_id_exists_in(l, pane_id))
                    })
                    .and_then(|w| w.connection.clone());
                session_owner_host_key(conn.as_ref())
            };
            release_session_owner(&host_key, name);
        }
    }
    outcome
}

#[tauri::command]
async fn pane_kill_session(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<KillSessionOutcome, String> {
    Ok(kill_pane_session_inner(&state, &pane_id).await)
}

#[tauri::command]
async fn pane_disconnect(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let sid = state.core.pane_sessions.lock().unwrap().remove(&pane_id);
    let Some(sid) = sid else {
        return Ok(());
    };

    // Phase 7.C: if the workspace has a teardown_command, send it and give the
    // shell ~500ms to run it before we drop the channel.
    let teardown = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| {
                w.layout
                    .as_ref()
                    .map(|l| find_pane_connection(l, &pane_id).is_some())
                    .unwrap_or(false)
            })
            .and_then(|w| w.teardown_command.clone())
            .filter(|s| !s.is_empty())
    };
    if let Some(t) = teardown {
        let bytes = format!("{}\r\n", t).into_bytes();
        {
            let mut sessions = state.core.sessions.lock().unwrap();
            if let Some(s) = sessions.get_mut(&sid) {
                match s {
                    Session::Local(l) => {
                        use std::io::Write as _;
                        let _ = l.writer.write_all(&bytes);
                        let _ = l.writer.flush();
                    }
                    Session::Ssh(ssh) => {
                        let _ = ssh.try_send(SshCmd::Data(bytes));
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
        kill_session_inner(&mut s);
    }
    Ok(())
}

// ─── Session-level commands (write/resize) ───────────────────────────────────

pub(crate) fn write_to_session(state: &AppState, session_id: &str, data: &[u8]) -> Result<(), String> {
    let mut sessions = state.core.sessions.lock().unwrap();
    let s = sessions
        .get_mut(session_id)
        .ok_or_else(|| format!("no such session {session_id}"))?;
    match s {
        Session::Local(l) => {
            l.writer.write_all(data).map_err(|e| e.to_string())?;
            l.writer.flush().map_err(|e| e.to_string())?;
        }
        Session::Ssh(ssh) => {
            ssh.try_send(SshCmd::Data(data.to_vec()))?;
        }
    }
    Ok(())
}

#[tauri::command]
fn pty_write(state: State<'_, AppState>, session_id: String, data: String) -> Result<(), String> {
    write_to_session(&state, &session_id, data.as_bytes())
}

#[tauri::command]
fn notifications_list(state: State<'_, AppState>) -> Vec<NotificationItem> {
    state.notifications.lock().unwrap().clone()
}

#[tauri::command]
fn notifications_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.notifications.lock().unwrap().clear();
    Ok(())
}

#[tauri::command]
fn pane_status_get(state: State<'_, AppState>) -> HashMap<String, String> {
    state.pane_status.lock().unwrap().clone()
}

/// Phase 6.5: shared decision logic for feed items. Used both by the Tauri command
/// `feed_decide` (called by the frontend's Allow/Deny buttons) and by the RPC handler
/// when the timeout expires or sender drops.
pub(crate) fn decide_feed(
    state: &AppState,
    app: &AppHandle,
    request_id: &str,
    decision: &str,
) -> Result<(), String> {
    let new_state = match decision {
        "allow" => FeedItemState::Allowed,
        "deny" => FeedItemState::Denied,
        "timeout" => FeedItemState::Timedout,
        other => return Err(format!("unknown decision: {other}")),
    };
    let tx = {
        let mut store = state.feed.lock().unwrap();
        for item in store.items.iter_mut() {
            if item.request_id == request_id {
                item.state = new_state.clone();
            }
        }
        store.pending.remove(request_id)
    };
    let _ = app.emit(
        "feed:item-resolved",
        serde_json::json!({ "request_id": request_id, "decision": decision }),
    );
    if let Some(tx) = tx {
        let _ = tx.send(decision.to_string());
    }
    Ok(())
}

#[tauri::command]
fn feed_list(state: State<'_, AppState>) -> Vec<FeedItem> {
    state.feed.lock().unwrap().items.iter().cloned().collect()
}

#[tauri::command]
fn feed_decide(
    state: State<'_, AppState>,
    app: AppHandle,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    decide_feed(&state, &app, &request_id, &decision)
}

// Phase 48-C: build the /doctor diagnostic snapshot. Process-cheap
// signals only — no shell-outs, no FS scans beyond a small log tail.
// Reused by the `doctor` tauri command, the `doctor` RPC method, and
// the `ymux doctor` CLI subcommand.
pub(crate) fn build_doctor_snapshot(state: &AppState) -> serde_json::Value {
    use std::sync::atomic::Ordering;
    let workspaces = state.workspaces.lock().unwrap().workspaces.clone();
    let workspace_count = workspaces.len();
    // Count which workspaces have a live SSH session (any pane or the
    // headless Phase 41 entry counts).
    let sessions = state.core.sessions.lock().unwrap();
    let mut ssh_connected = std::collections::HashSet::new();
    let mut pty_count = 0usize;
    for s in sessions.values() {
        pty_count += 1;
        if let Session::Ssh(ssh) = s {
            ssh_connected.insert(ssh.workspace_id.clone());
        }
    }
    let ssh_connected_count = ssh_connected.len();
    drop(sessions);

    let bundled_cli_sha256: Option<String> = (|| {
        // v0.2.7 scrub fix: embed the manifest content at COMPILE time
        // via include_str! rather than read it at runtime from a path
        // that only exists on the build machine. The previous
        // env!("CARGO_MANIFEST_DIR") + fs::read_to_string approach
        // leaked the developer's absolute build path into the release
        // binary (RUSTFLAGS --remap-path-prefix only scrubs debug
        // info, not env!() string expansions) AND silently failed at
        // runtime on every user machine where that path didn't exist.
        const MANIFEST: &str = include_str!("../resources/remote-manifest.json");
        let m: serde_json::Value = serde_json::from_str(MANIFEST.trim_start_matches('\u{FEFF}'))
            .ok()?;
        m.get("x86_64-linux")?
            .get("sha256")?
            .as_str()
            .map(|s| s.to_string())
    })();

    // Last few lines from debug.log filtered to ERROR/WARN. Best-effort.
    let recent_errors: Vec<String> = (|| -> Option<Vec<String>> {
        let path = config_dir_pub().ok()?.join("debug.log");
        let s = std::fs::read_to_string(&path).ok()?;
        let mut out: Vec<String> = s
            .lines()
            .rev()
            // The unified logger writes "[<ts>] [LEVEL] [TAG] <msg>" — match
            // the level column exactly so a message merely *containing* the
            // word ERROR (e.g. a logged shell snippet's own `echo "ERROR …"`)
            // can't pollute the doctor report.
            .filter(|l| {
                l.starts_with('[') && (l.contains("] [ERROR] [") || l.contains("] [WARN ] ["))
            })
            .take(10)
            .map(|s| s.to_string())
            .collect();
        out.reverse();
        Some(out)
    })()
    .unwrap_or_default();

    serde_json::json!({
        "ymux_version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "workspaces": {
            "total": workspace_count,
            "ssh_connected": ssh_connected_count,
        },
        "pty_sessions": pty_count,
        "rpc_server": {
            "pipe_name": rpc_server::pipe_name(),
            "listener_pool_size": rpc_server::listener_count(),
            "handlers_served": rpc_server::HANDLER_SEQ.load(Ordering::Relaxed),
            // Some(...) means the local endpoint never came up: no detected
            // ports, no CLI hooks, no tunnel RPC — while SSH panes still
            // work, which is what makes it easy to misdiagnose.
            "bind_error": rpc_server::bind_error(),
        },
        "bundled_linux_cli_sha256": bundled_cli_sha256,
        "recent_errors": recent_errors,
    })
}

#[tauri::command]
fn doctor(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    Ok(build_doctor_snapshot(&state))
}

/// The OS the desktop app is running on, as `std::env::consts::OS`
/// (`"windows"` / `"macos"` / `"linux"`). The frontend had NO platform
/// awareness at all before the macOS port, so every local path was joined
/// with `\` and every drag-drop position was divided by devicePixelRatio —
/// both correct on Windows only. `app/src/platform.ts` resolves this once
/// at boot and hands the answer to those call sites synchronously.
#[tauri::command]
fn host_platform() -> Result<String, String> {
    Ok(std::env::consts::OS.to_string())
}

#[tauri::command]
fn pty_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = state.core.sessions.lock().unwrap();
    let s = sessions
        .get(&session_id)
        .ok_or_else(|| format!("no such session {session_id}"))?;
    match s {
        Session::Local(l) => l
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string()),
        Session::Ssh(ssh) => ssh.try_send(SshCmd::Resize(cols as u32, rows as u32)),
    }
}

/// Round B (#4): pop a live terminal pane out into its own OS window.
///
/// The popout loads `index.html?popout=<sid>`; index.tsx early-bails to a
/// full-screen `<PopoutTerminal>` that attaches to the SAME session_id.
/// `pty:data` / `pty:exit` are emitted app-wide (see `emit_data` /
/// `emit_exit`), so the popout receives the stream alongside the main
/// window. The popout owns input + resize while open; the origin pane
/// detaches to a read-only mirror (frontend) to avoid a SIGWINCH tug-of-war.
///
/// Scoped by `capabilities/popout.json` (`windows: ["popout-*"]`) to the
/// minimum: core events (listen/emit) + window close. Does not touch the
/// `main` capability.
// IMPORTANT: this MUST stay `async`. On Windows, `WebviewWindowBuilder`
// deadlocks when driven from a SYNCHRONOUS command (documented on docs.rs) —
// the window shell appears but the webview never initializes (blank white).
// An async command runs off the main event-loop thread and builds cleanly,
// which is also why the workspace Browser (add_child) command is async.
#[tauri::command]
async fn popout_pane(
    app: AppHandle,
    session_id: String,
    title: String,
    cols: Option<u32>,
    rows: Option<u32>,
    dir: Option<String>,
) -> Result<(), String> {
    // Window labels only allow [a-zA-Z0-9-/:_]. session_id is our own
    // UUID-ish token, but sanitize defensively.
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        return Err("invalid session id".into());
    }
    let label = format!("popout-{safe}");

    // Already popped out? Focus the existing window instead of erroring on
    // the duplicate label.
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }

    // Rough pixel size from the pane's current grid. The terminal fits
    // itself on mount, so this only needs to be in the right ballpark.
    let cols = f64::from(cols.unwrap_or(100).clamp(20, 400));
    let rows = f64::from(rows.unwrap_or(30).clamp(6, 200));
    let win_w = (cols * 8.5 + 24.0).clamp(480.0, 2400.0);
    let win_h = (rows * 18.0 + 48.0).clamp(320.0, 1600.0);

    // CLEAN url — no query, no fragment. In a built app Tauri's asset
    // protocol treats ANY suffix (`index.html?x` or `index.html#x`) as a
    // literal path and serves a blank page. The frontend reads the session
    // id from the window LABEL (`popout-<sid>`) instead, so the URL never
    // needs to carry it. (`dir` is currently derived frontend-side from the
    // per-line RTL observer, so it isn't threaded through the URL.)
    let _ = &dir;

    // Retry the transient WebView2 0x8007139F (ERROR_INVALID_STATE) a few
    // times with a short backoff, mirroring workspace_browser_show. The
    // builder is consumed by build(), so it's rebuilt each attempt.
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    let mut built = None;
    for attempt in 1..=MAX_ATTEMPTS {
        log_debug("APP", &format!(
            "popout_pane: building window label={label} size={win_w}x{win_h} (attempt {attempt}/{MAX_ATTEMPTS})"
        ));
        match tauri::WebviewWindowBuilder::new(
            &app,
            &label,
            tauri::WebviewUrl::App("index.html".into()),
        )
        .title(title.clone())
        .inner_size(win_w, win_h)
        .center()
        // Same reason as the main window — see Phase 82.E in Cargo.toml.
        // A popout pane is ymux's own UI showing PTY output.
        .devtools(false)
        .build()
        {
            Ok(w) => {
                built = Some(w);
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                log_warn("APP", &format!(
                    "popout_pane: build attempt {attempt}/{MAX_ATTEMPTS} FAILED: {last_err}"
                ));
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
            }
        }
    }
    let win = built
        .ok_or_else(|| format!("popout window failed after {MAX_ATTEMPTS} attempts: {last_err}"))?;
    log_info("APP", &format!("popout_pane: window {label} built ok"));

    // Tell the app when the popout closes (user X, or the frontend closing
    // itself on pty:exit) so the origin pane can re-attach input + resize.
    let app2 = app.clone();
    let sid = session_id.clone();
    win.on_window_event(move |e| {
        if matches!(e, tauri::WindowEvent::Destroyed) {
            let _ = app2.emit("popout:closed", sid.clone());
        }
    });

    Ok(())
}

// ─── Entrypoint ──────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .ok();

    // Phase 23.J: capture every panic to debug.log so the next
    // reproduction tells us EXACTLY what panicked, instead of dying
    // silently to WinDbg with no info. The Phase 23.I Hebrew-title
    // crash was a STATUS_STACK_BUFFER_OVERRUN (__fastfail(7)) with
    // no Rust panic trace anywhere — we had to reverse-engineer the
    // cause from WER event metadata and 5-second timing. This hook
    // eliminates that guesswork for next time.
    //
    // RUST_BACKTRACE=1 is set unconditionally before the hook so
    // `Backtrace::capture()` always returns frames (otherwise the
    // env var defaults to off and capture() returns "disabled").
    // Safe to set in dev builds; revisit for release.
    std::env::set_var("RUST_BACKTRACE", "1");
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        let bt = std::backtrace::Backtrace::capture();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string();
        log_error("APP", &format!(
            "PANIC at {location}: {msg}\n  thread: {thread_name}\n  backtrace:\n{bt}"
        ));
        // Phase 80: writes are queued, and a panic may be on its way to an
        // abort — get the backtrace onto disk before anything else happens.
        ymux_core::flush_log();
        // Re-emit to stderr so any wrapping process (cargo run, tauri
        // dev server, etc.) can also surface it inline.
        eprintln!("PANIC at {location}: {msg}");
    }));

    // Unshipped-fivefer (#3): browser session persistence. Point WebView2 at a
    // single stable profile folder under our config dir so cookies/logins in
    // workspace browsers survive restarts. One app-wide folder (shared by the
    // main window + every workspace browser) — deliberately NOT a per-workspace
    // --user-data-dir, which reintroduces the 0x8007139F "user data folder in
    // use" crash (WebView2 allows one environment per process). Must be set
    // before any webview is created, hence here at the top of run().
    //
    // Windows-only by construction: WKWebView (macOS) and WebKitGTK ignore
    // WEBVIEW2_USER_DATA_FOLDER entirely. Running this block there created a
    // `webview2` directory nothing would ever read and made the toggle a
    // silent no-op — worse than an honest one, because "clear on restart"
    // appeared to work. The UI disables the toggle off Windows to match.
    #[cfg(target_os = "windows")]
    if let Ok(base) = config_dir() {
        let profile = base.join("webview2");
        // Toggle off → wipe the profile on this launch (clear-on-restart), then
        // keep using the same path so the next session starts clean too until
        // re-enabled.
        if !settings::persist_browser_sessions_flag() {
            let _ = std::fs::remove_dir_all(&profile);
        }
        let _ = std::fs::create_dir_all(&profile);
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &profile);
        log_debug("APP", &format!("webview2 profile folder: {}", profile.display()));
    }
    #[cfg(not(target_os = "windows"))]
    log_debug(
        "APP",
        "browser session persistence is WebView2-only — the setting has no effect on this platform",
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .setup(|app| {
            let state: State<AppState> = app.state();
            log_debug("APP", "─── setup() starting ───");
            // Phase 8.E hotfix: log the exact config dir up front so we can
            // tell whether the binary is resolving the right path. Honors
            // `YMUX_CONFIG_DIR` env var override if set.
            let cfg_dir = config_dir().ok();
            log_info("APP", &format!(
                "setup: config_dir = {:?} (override env YMUX_CONFIG_DIR = {:?})",
                cfg_dir,
                std::env::var("YMUX_CONFIG_DIR").ok()
            ));
            tracing::info!("ymux config_dir: {:?}", cfg_dir);

            // Phase 53.G: was Phase 8.F.1 — the iframe-bridge
            // initialization script was the parent-side companion to
            // the per-pane iframe Browser. With the per-pane Browser
            // surface gone (53.D moved Browser to a workspace-level
            // child Webview via Window::add_child) the bridge is
            // dead. The main window is still created programmatically
            // because tauri.conf.json's `windows: []` skips the
            // default — same title + inner size as before.
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("YMUX")
            .inner_size(1100.0, 700.0)
            // Mandatory companion to the `devtools` feature in Cargo.toml
            // (Phase 82.E): the feature flips the runtime default to
            // `true` for every webview, and this one renders live PTY
            // output — Rule #1. Only the workspace Browser child webview
            // opts in, and only on macOS.
            .devtools(false)
            .build()
            .map_err(|e| Box::<dyn std::error::Error>::from(format!("main window: {e}")))?;
            log_debug("APP", "setup: main webview created");
            // Unshipped-fivefer (#2): system tray. Best-effort — a failure
            // just means no tray + no close-to-tray (see on_window_event).
            match tray::init(app.handle()) {
                Ok(()) => log_debug("APP", "setup: system tray created"),
                Err(e) => log_warn("APP", &format!("setup: tray init failed (continuing): {e}")),
            }
            match load_from_disk() {
                Ok(file) => {
                    *state.workspaces.lock().unwrap() = file;
                    *state.load_state.lock().unwrap() = Some(LoadState::Loaded);
                    log_info("APP", "setup: load_state = Loaded");
                }
                Err(e) => {
                    *state.load_state.lock().unwrap() = Some(LoadState::Failed);
                    log_error("APP", &format!(
                        "setup: load FAILED: {e} — load_state = Failed (persists will refuse)"
                    ));
                    tracing::warn!("workspaces load failed: {e}");
                }
            }
            // Phase 7.B: load notes (best-effort; missing file is fine).
            match notes::load_notes_from_disk() {
                Ok(nf) => {
                    let count = nf.notes.len();
                    *state.notes.lock().unwrap() = nf;
                    log_info("APP", &format!("setup: notes loaded ({count} notes)"));
                }
                Err(e) => {
                    log_warn("APP", &format!("setup: notes load failed: {e} (starting empty)"));
                }
            }
            // Phase 12.C: load recent paths history (or empty on first run).
            match local_wizard::load_recent_from_disk() {
                Ok(rf) => {
                    let count = rf.entries.len();
                    *state.recent_paths.lock().unwrap() = rf;
                    log_info("APP", &format!("setup: recent_paths loaded ({count} entries)"));
                }
                Err(e) => {
                    log_warn("APP", &format!("setup: recent_paths load failed: {e} (starting empty)"));
                }
            }
            // Phase 9.A: load settings (or write defaults on first run).
            match settings::load_from_disk() {
                Ok(s) => {
                    log_info("APP", &format!("setup: settings loaded (theme.preset={})", s.theme.preset));
                    *state.settings.lock().unwrap() = s;
                }
                Err(e) => {
                    log_warn("APP", &format!("setup: settings load failed: {e} (using defaults)"));
                }
            }
            // Phase 75: prune stale debug logs so they can't accumulate.
            // Unified logging: apply the persisted threshold before anything
            // else logs (save_to_disk keeps it in sync from here on), then
            // start the remote-log sync loop (pulls server/hooks/install
            // logs into the local debug.log every 60s).
            {
                let logs = state.settings.lock().unwrap().logs.clone();
                ymux_core::set_log_level(ymux_core::LogLevel::from_str(&logs.level));
                prune_logs(logs.retention_days);
            }
            log_sync::spawn_log_sync(app.handle().clone());
            // Phase 39.B: one-time migration. Workspaces created before
            // Phase 39 flipped the auto_port_forward default still have
            // `true` saved and keep auto-forwarding on every connect
            // (the YMUX-CHALLENGE / pipe-storm path). Flip them to
            // false once; users re-enable per workspace. The flag on
            // Settings keeps this from re-running and undoing a later
            // opt-in. Skipped if workspaces failed to load (load_state
            // != Loaded) so we never persist over a clobbered file.
            {
                let load_ok =
                    *state.load_state.lock().unwrap() == Some(LoadState::Loaded);
                let already_done = state
                    .settings
                    .lock()
                    .unwrap()
                    .migrations
                    .phase_39_auto_port_forward_default_flipped;
                if load_ok && !already_done {
                    let changed = {
                        let mut f = state.workspaces.lock().unwrap();
                        disable_all_auto_port_forward(&mut f)
                    };
                    if changed > 0 {
                        match persist(&state) {
                            Ok(()) => log_info("APP", &format!(
                                "migration phase_39: flipped {changed} workspace(s) auto_port_forward to false"
                            )),
                            Err(e) => log_warn("APP", &format!("migration phase_39: save failed: {e}")),
                        }
                    } else {
                        log_debug("APP", "migration phase_39: no workspaces needed flipping");
                    }
                    // Mark done + persist settings (do this regardless of
                    // `changed` so the migration never re-runs).
                    let snapshot = {
                        let mut s = state.settings.lock().unwrap();
                        s.migrations.phase_39_auto_port_forward_default_flipped = true;
                        s.clone()
                    };
                    if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                        log_warn("APP", &format!("migration phase_39: settings save failed: {e}"));
                    }
                }
            }
            // Phase 53 (rebased): one-time rewrite of any leftover
            // PaneKind::Browser / PaneKind::FileManager panes to
            // Terminal. These pane kinds were removed from the
            // create-pane menu when the Browser + Files surfaces moved
            // to workspace-level floating windows; the leftover panes
            // would render as broken under the new layout. Same
            // safety gate as the phase_39 migration: skip if
            // load_state != Loaded.
            {
                let load_ok =
                    *state.load_state.lock().unwrap() == Some(LoadState::Loaded);
                let already_done = state
                    .settings
                    .lock()
                    .unwrap()
                    .migrations
                    .phase_53_remove_browser_filemanager_panes;
                if load_ok && !already_done {
                    let changed = {
                        let mut f = state.workspaces.lock().unwrap();
                        rewrite_browser_filemanager_panes_to_terminal(&mut f)
                    };
                    if changed > 0 {
                        match persist(&state) {
                            Ok(()) => log_info("APP", &format!(
                                "migration phase_53: rewrote {changed} Browser/FileManager pane(s) to Terminal"
                            )),
                            Err(e) => log_warn("APP", &format!("migration phase_53: save failed: {e}")),
                        }
                    } else {
                        log_debug("APP", "migration phase_53: no legacy Browser/FileManager panes found");
                    }
                    let snapshot = {
                        let mut s = state.settings.lock().unwrap();
                        s.migrations.phase_53_remove_browser_filemanager_panes = true;
                        s.clone()
                    };
                    if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                        log_warn("APP", &format!("migration phase_53: settings save failed: {e}"));
                    }
                }
            }
            // Phase 49-C: auto-destroy sweep. Opt-in via
            // settings.auto_destroy_empty_workspaces_days. A workspace is
            // a candidate when it has no panes (empty layout) AND its
            // last_active_at is older than the configured TTL. Sessions
            // aren't checked — startup runs BEFORE any spawn_ssh, so
            // there's nothing live yet. last_active_at = 0 (never
            // activated since the field was added) is grace-treated as
            // "recent" so the first run after upgrade doesn't nuke
            // never-touched workspaces. Silent — the user opted in via
            // the setting; no toast.
            {
                let load_ok = *state.load_state.lock().unwrap() == Some(LoadState::Loaded);
                let ttl_days = state
                    .settings
                    .lock()
                    .unwrap()
                    .auto_destroy_empty_workspaces_days;
                if load_ok {
                    if let Some(days) = ttl_days {
                        if days > 0 {
                            let ttl_secs = (days as u64) * 86_400;
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let removed = {
                                let mut f = state.workspaces.lock().unwrap();
                                let before = f.workspaces.len();
                                f.workspaces.retain(|w| {
                                    let stale = w.last_active_at > 0
                                        && now.saturating_sub(w.last_active_at) > ttl_secs;
                                    let empty = w.layout.is_none();
                                    if stale && empty {
                                        log_info("WORKSPACE", &format!(
                                            "auto-destroy: removing workspace {} ({}) — empty + last_active {} days ago",
                                            w.id,
                                            w.name,
                                            now.saturating_sub(w.last_active_at) / 86_400
                                        ));
                                        false
                                    } else {
                                        true
                                    }
                                });
                                before - f.workspaces.len()
                            };
                            if removed > 0 {
                                if let Err(e) = persist(&state) {
                                    log_warn("WORKSPACE", &format!("auto-destroy: save failed: {e}"));
                                }
                            }
                        }
                    }
                }
            }
            // Phase 9.B: spawn the update checker if enabled. Fully best-effort —
            // never blocks startup; failures (offline, manifest missing, repo
            // private) just log to debug.log and emit nothing.
            {
                let s = state.settings.lock().unwrap().clone();
                if s.updates.check_on_startup {
                    let app_handle = app.handle().clone();
                    let state_clone: AppState = (*state).clone();
                    tauri::async_runtime::spawn(async move {
                        // Small delay so the splash + initial render finish first.
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        let _ = updater::check(&state_clone, &app_handle).await;
                    });
                }
            }
            // Spawn JSON-RPC server on a per-user named pipe.
            let state_clone: AppState = (*state).clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                rpc_server::run(state_clone, app_handle).await;
            });
            log_info("APP", &format!("setup: rpc server spawned on {}", rpc_server::pipe_name()));
            log_debug("APP", "─── setup() done ───");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            clipboard_read_text,
            // Phase 68.B: add-on framework commands.
            addons::addon_list,
            addons::addon_install,
            addons::addon_uninstall,
            addons::addon_update,
            addons::addon_logs,
            // Phase 68.D: Monitor — pull from the insights daemon.
            addons::insights_fetch,
            addons::insights_docker_action,
            addons::insights_hygiene_kill,
            // ymux-tools skills registry: per-workspace skill installer.
            skills::skills_list,
            skills::skill_install,
            skills::skill_uninstall,
            skills::skills_installed,
            // beta.3-lh-insights: native local Insights commands. The panel
            // primarily goes through `insights_fetch` (which routes local↔SSH
            // internally), but we expose these too for direct callers.
            insights_local::insights_local_current,
            insights_local::insights_local_docker,
            insights_local::insights_local_processes,
            insights_local::insights_local_hygiene,
            insights_local::insights_local_logs,
            insights_local::insights_local_docker_action,
            // Phase 70: mobile pairing (nginx + Cloudflare + Let's Encrypt).
            pairing::mobile_pairing_init,
            pairing::mobile_pairing_status,
            pairing::mobile_pairing_disconnect,
            pairing::mobile_pairing_generate_qr,
            pairing::mobile_pairing_list_devices,
            pairing::mobile_pairing_revoke,
            pairing::mobile_pairing_rename,
            workspaces_load,
            workspace_create,
            workspace_update,
            workspace_rename,
            workspace_set_identity,
            // cmux-A A2: workspace groups (sidebar collapsible sections).
            workspace_group_create,
            workspace_group_update,
            workspace_group_delete,
            workspace_set_group,
            // beta.3 (ws-dragdrop): direct drag reorder from the sidebar.
            workspace_reorder,
            workspace_group_reorder,
            // beta.3 (pane-dragdrop): swap two panes' positions in the
            // layout tree (drag a pane header onto another pane).
            workspace_swap_panes,
            workspace_set_auto_port_forward,
            workspace_set_claude_separate_account,
            port_forward_stop,
            forward_port_start,
            workspace_ensure_port_watcher,
            list_detected_ports,
            log_dir_path,
            read_log_tail,
            clear_debug_log_cmd,
            pane_set_identity,
            pane_set_smart_bidi,
            workspace_browser::workspace_browser_show,
            workspace_browser::workspace_browser_hide,
            workspace_browser::workspace_browser_navigate,
            workspace_browser::workspace_browser_eval,
            workspace_browser::workspace_browser_open_devtools,
            workspace_browser::workspace_browser_close,
            workspace_browser::workspace_browser_resize,
            workspace_browser::browser_popout_open,
            tickets::tickets_list,
            tickets::tickets_resolve_project,
            tickets::tickets_dir_path,
            tickets::tickets_screenshot,
            tickets::tickets_create,
            tickets::tickets_update,
            tickets::tickets_delete,
            ssh_key_offer_dismiss,
            // beta.3 (netfree, Track 1b): frontend calls this when the user
            // clicks "בטל" on the reconnect toast.
            ssh_cancel_reconnect,
            ssh_key_generate_and_install,
            provision_existing_install_key,
            workspace_delete,
            workspace_set_active,
            workspace_create_worktree,
            workspace_pin_project_folder,
            workspace_open_worktree,
            workspace_open_session,
            workspace_set_collapsed,
            workspace_set_project_root,
            workspace_set_tabs_mode,
            pane_agent_states,
            worktrees::git_probe_worktrees,
            worktrees::workspace_list_worktrees,
            worktrees::workspace_create_project_worktree,
            workspace_split,
            workspace_close_pane,
            workspace_set_split_ratio,
            workspace_distribute_evenly,
            workspace_ensure_connected,
            pane_connect,
            pane_disconnect,
            pane_kill_session,
            zellij_delete_session,
            pane_persistence_get,
            pane_persistence_list,
            pane_list_claude_sessions,
            claude_usage::claude_usage_fetch,
            pane_list_tmux_sessions,
            pane_probe_tmux_sessions,
            pane_target_session_state,
            tmux_rename_session,
            sessions_overview::sessions_overview_summarize,
            sessions_overview::sessions_kill_by_name,
            tmux_labels_get,
            tmux_label_set,
            pane_set_title,
            pane_set_annotation,
            workspace_reset_layout,
            ui_log,
            pty_write,
            pty_resize,
            doctor,
            host_platform,
            notifications_list,
            notifications_clear,
            pane_status_get,
            feed_list,
            feed_decide,
            notes::notes_load,
            notes::notes_add,
            notes::notes_update,
            notes::notes_delete,
            settings::settings_load,
            settings::settings_save,
            settings::settings_get_presets,
            settings::settings_apply_preset,
            settings::settings_reset,
            settings::list_system_fonts,
            fonts::font_catalog,
            fonts::font_install,
            fonts::font_uninstall,
            updater::check_for_updates_now,
            updater::download_and_install_update,
            updater::updater_skip_version,
            updater::updater_remind_later,
            // Phase 71: version manager — list + install-specific-version.
            updater::updater_list_versions,
            updater::updater_install_version,
            updater::ssh_exec_in_workspace,
            connect_wizard::parse_ssh_config,
            connect_wizard::list_ssh_keys,
            connect_wizard::check_key_permissions,
            connect_wizard::fix_key_permissions,
            connect_wizard::test_ssh_connect,
            provisioning::provisioning_inspect,
            provisioning::provisioning_start,
            provisioning::connect_existing_discover,
            provisioning::connect_existing_execute,
            provisioning::provisioning_profiles_list,
            provisioning::provisioning_profile_save,
            provisioning::provisioning_profile_delete,
            provisioning::provisioning_step_catalog,
            // Phase 80: local smart setup (wizard "local → new").
            local_setup::local_setup_inspect,
            local_setup::local_setup_preflight,
            local_setup::local_setup_start,
            local_setup::restart_windows,
            file_manager::file_list_local,
            file_manager::file_list_remote,
            file_manager::file_home_local,
            file_manager::file_home_remote,
            file_manager::file_delete_local,
            file_manager::file_delete_remote,
            file_manager::file_rename_local,
            file_manager::file_rename_remote,
            file_manager::file_copy_remote,
            file_manager::file_mkdir_local,
            file_manager::file_copy_local,
            file_manager::file_mkdir_remote,
            file_manager::file_create_local,
            file_manager::file_create_remote,
            file_manager::file_upload,
            file_manager::pane_upload_dropped,
            diff_pane::diff_pane_set_source,
            diff_pane::diff_pane_refresh,
            file_manager::file_download,
            file_manager::fm_transfer_cancel,
            file_manager::download_remote_file_via_osc,
            file_manager::file_open_local,
            file_manager::file_open_remote,
            file_manager::file_read_local,
            file_manager::file_read_remote,
            file_manager::file_write_local,
            file_manager::file_write_remote,
            file_manager::file_large_threshold,
            file_manager::file_manager_zip_local,
            file_manager::file_manager_unzip_local,
            file_manager::file_manager_zip_remote,
            file_manager::file_manager_targz_remote,
            file_manager::file_manager_unzip_remote,
            file_manager::file_manager_unzip_local_check,
            file_manager::file_manager_unzip_remote_check,
            stt::stt_transcribe_local,
            claude_summary::claude_summarize,
            // Phase 24.D: claude_log_* commands KEPT (registered but
            // no FE caller) for a future unified-view rebuild.
            // claude_log_pane_set was removed alongside the pane kind.
            // claude_chat_* commands deleted with the module.
            claude_log::claude_log_sync,
            claude_log::claude_log_list,
            claude_log::claude_log_read,
            local_wizard::detect_local_shells,
            local_wizard::list_recent_paths,
            local_wizard::record_recent_path,
            // Unshipped-fivefer (#2): taskbar badge from the frontend.
            tray::set_tray_badge,
            // Unshipped-fivefer (#4): pop a terminal pane into its own window.
            popout_pane,
        ])
        // #2 (feedback): close-to-tray removed — closing the window quits
        // normally (the minimize-to-tray surprise was confusing). The tray
        // icon + badge stay for quick access; quit is either the window close
        // or the tray menu.
        // Phase 80: `.build(...).run(|_, event|)` instead of `.run(ctx)`,
        // purely so the queued log tail reaches disk on the way out. The run
        // loop is otherwise unchanged, and the `.expect` is still the boot
        // path Rule #4 exempts.
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                ymux_core::flush_log();
            }
        });
}

#[cfg(test)]
mod port_forward_tests {
    // Phase 36 (#2.2) → 36.A: the forwards map is keyed by
    // (workspace_id, remote_port); local_port is now whatever the
    // kernel assigned at bind time (no longer derived from remote_port).
    // These exercise the insert / lookup / remove mechanics that
    // open_auto_forward + close_one_forward rely on, without a live
    // russh channel (cancel = None). The local_port values below stand
    // in for arbitrary kernel-assigned ephemeral ports.
    use super::{ForwardEntry, ForwardMap};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn empty_map() -> ForwardMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn insert_lookup_remove() {
        let m = empty_map();
        // Remote :3000, kernel handed back an ephemeral local port.
        let key = ("ws1".to_string(), 3000u16);
        m.lock().unwrap().insert(
            key.clone(),
            ForwardEntry {
                local_port: 49231,
                cancel: None,
            },
        );
        assert_eq!(m.lock().unwrap().get(&key).map(|e| e.local_port), Some(49231));
        let removed = m.lock().unwrap().remove(&key);
        assert!(removed.is_some());
        assert!(m.lock().unwrap().get(&key).is_none());
    }

    #[test]
    fn distinct_workspaces_same_remote_port_dont_collide() {
        // Two workspaces both expose remote :8080 — under 36.A each gets
        // its own kernel-assigned local port, so no collision.
        let m = empty_map();
        m.lock().unwrap().insert(
            ("a".to_string(), 8080),
            ForwardEntry { local_port: 49500, cancel: None },
        );
        m.lock().unwrap().insert(
            ("b".to_string(), 8080),
            ForwardEntry { local_port: 49777, cancel: None },
        );
        assert_eq!(m.lock().unwrap().len(), 2);
        assert_eq!(
            m.lock().unwrap().get(&("b".to_string(), 8080)).map(|e| e.local_port),
            Some(49777)
        );
    }
}

#[cfg(test)]
mod tcp_probe_tests {
    // Phase 46: tcp_probe is the post-bind sanity check inside
    // open_auto_forward — confirms a freshly bound local port is
    // actually reachable on 127.0.0.1 before we tell the FE the
    // forward is live (saves opening a browser tab on a dead port).
    use super::tcp_probe;
    use std::time::Duration;

    #[tokio::test]
    async fn probe_succeeds_for_listening_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = format!("127.0.0.1:{port}");
        // Accept loop in background so the probe's connect handshake completes.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let r = tcp_probe(&target, Duration::from_millis(500)).await;
        assert!(r.is_ok(), "expected Ok, got {:?}", r);
    }

    #[tokio::test]
    async fn probe_fails_for_vacant_port() {
        // Bind+drop reserves a port number then frees it; the probe
        // hits a port the OS just freed so it returns ECONNREFUSED
        // (immediate, no timeout needed).
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let target = format!("127.0.0.1:{port}");
        let r = tcp_probe(&target, Duration::from_millis(300)).await;
        assert!(r.is_err(), "expected Err for vacant port, got {:?}", r);
    }
}

#[cfg(test)]
mod pane_swap_tests {
    // beta.3 (pane-dragdrop): unit tests for the layout-tree pane
    // swap. These don't touch the AppState / Tauri command layer —
    // they exercise `swap_two_panes_in_layout` directly against a
    // hand-built LayoutNode tree, which is what matters for
    // correctness (the command wrapper is just lock + persist +
    // emit).
    use super::{swap_two_panes_in_layout, LayoutNode, PaneKind, SplitDirection};

    fn pane(id: &str) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.to_string(),
            pane_kind: PaneKind::Terminal,
            connection: None,
            browser: None,
            title: Some(format!("title-{id}")),
            auto_title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }
    }

    fn pane_id_of(node: &LayoutNode) -> &str {
        match node {
            LayoutNode::Pane { pane_id, .. } => pane_id,
            LayoutNode::Split { .. } => panic!("expected Pane, got Split"),
        }
    }

    fn title_of(node: &LayoutNode) -> Option<&str> {
        match node {
            LayoutNode::Pane { title, .. } => title.as_deref(),
            LayoutNode::Split { .. } => None,
        }
    }

    #[test]
    fn swap_two_leaves_in_a_split() {
        let mut layout = LayoutNode::Split {
            split_id: "s".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("A")),
            second: Box::new(pane("B")),
            ratio: 0.5,
        };
        swap_two_panes_in_layout(&mut layout, "A", "B").unwrap();
        match &layout {
            LayoutNode::Split { first, second, .. } => {
                // The whole pane node moved: id AND title. That's how
                // xterm content stays "with" the pane_id — the pane_id
                // travels with the connection state to the new slot.
                assert_eq!(pane_id_of(first), "B");
                assert_eq!(pane_id_of(second), "A");
                assert_eq!(title_of(first), Some("title-B"));
                assert_eq!(title_of(second), Some("title-A"));
            }
            _ => panic!("expected Split at root"),
        }
    }

    #[test]
    fn swap_across_nested_splits() {
        // Tree: Split[ Split[A, B], Split[C, D] ]. Swap A with D.
        let mut layout = LayoutNode::Split {
            split_id: "root".into(),
            direction: SplitDirection::Vertical,
            first: Box::new(LayoutNode::Split {
                split_id: "L".into(),
                direction: SplitDirection::Horizontal,
                first: Box::new(pane("A")),
                second: Box::new(pane("B")),
                ratio: 0.5,
            }),
            second: Box::new(LayoutNode::Split {
                split_id: "R".into(),
                direction: SplitDirection::Horizontal,
                first: Box::new(pane("C")),
                second: Box::new(pane("D")),
                ratio: 0.5,
            }),
            ratio: 0.5,
        };
        swap_two_panes_in_layout(&mut layout, "A", "D").unwrap();
        // A ↔ D crossed the root split; B and C stay put.
        let (left, right) = match &layout {
            LayoutNode::Split { first, second, .. } => (first, second),
            _ => panic!(),
        };
        let (a_slot, b_slot) = match left.as_ref() {
            LayoutNode::Split { first, second, .. } => (first, second),
            _ => panic!(),
        };
        let (c_slot, d_slot) = match right.as_ref() {
            LayoutNode::Split { first, second, .. } => (first, second),
            _ => panic!(),
        };
        assert_eq!(pane_id_of(a_slot), "D");
        assert_eq!(pane_id_of(b_slot), "B");
        assert_eq!(pane_id_of(c_slot), "C");
        assert_eq!(pane_id_of(d_slot), "A");
    }

    #[test]
    fn swap_same_id_is_noop() {
        let mut layout = LayoutNode::Split {
            split_id: "s".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("A")),
            second: Box::new(pane("B")),
            ratio: 0.5,
        };
        swap_two_panes_in_layout(&mut layout, "A", "A").unwrap();
        // Nothing moved.
        match &layout {
            LayoutNode::Split { first, second, .. } => {
                assert_eq!(pane_id_of(first), "A");
                assert_eq!(pane_id_of(second), "B");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn swap_missing_pane_errors_and_leaves_layout_untouched() {
        let mut layout = LayoutNode::Split {
            split_id: "s".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("A")),
            second: Box::new(pane("B")),
            ratio: 0.5,
        };
        // First missing → early error, layout untouched.
        let err = swap_two_panes_in_layout(&mut layout, "ZZ", "B").unwrap_err();
        assert!(err.contains("ZZ"));
        match &layout {
            LayoutNode::Split { first, second, .. } => {
                assert_eq!(pane_id_of(first), "A");
                assert_eq!(pane_id_of(second), "B");
            }
            _ => panic!(),
        }
        // Second missing → the code path takes A out, fails to find
        // ZZ, must put A back so the tree is unchanged.
        let err = swap_two_panes_in_layout(&mut layout, "A", "ZZ").unwrap_err();
        assert!(err.contains("ZZ"));
        match &layout {
            LayoutNode::Split { first, second, .. } => {
                assert_eq!(pane_id_of(first), "A");
                assert_eq!(pane_id_of(second), "B");
                assert_eq!(title_of(first), Some("title-A"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn swap_root_leaf_only_workspace_has_no_partner() {
        // Trivial: a single-pane workspace has no other pane to swap
        // with. Frontend won't invoke this, but the backend must not
        // panic if it does.
        let mut layout = pane("only");
        let err = swap_two_panes_in_layout(&mut layout, "only", "other").unwrap_err();
        assert!(err.contains("other"));
        assert_eq!(pane_id_of(&layout), "only");
    }
}

#[cfg(test)]
mod migration_tests {
    // Phase 39.B: the auto_port_forward flip. MigrationFlags default
    // is exercised in settings.rs; here we test the data-level flip +
    // its idempotency.
    use super::{disable_all_auto_port_forward, Workspace, WorkspacesFile};

    fn ws(id: &str, apf: bool) -> Workspace {
        Workspace {
            id: id.to_string(),
            name: id.to_string(),
            auto_port_forward: apf,
            ..Default::default()
        }
    }

    #[test]
    fn flips_only_true_workspaces_and_is_idempotent() {
        let mut file = WorkspacesFile {
            workspaces: vec![ws("a", true), ws("b", false), ws("c", true)],
            ..Default::default()
        };
        // First run flips the two `true` ones.
        assert_eq!(disable_all_auto_port_forward(&mut file), 2);
        assert!(file.workspaces.iter().all(|w| !w.auto_port_forward));
        // Second run is a no-op — nothing left to flip.
        assert_eq!(disable_all_auto_port_forward(&mut file), 0);
    }

    #[test]
    fn empty_or_all_false_changes_nothing() {
        let mut empty = WorkspacesFile::default();
        assert_eq!(disable_all_auto_port_forward(&mut empty), 0);
        let mut all_off = WorkspacesFile {
            workspaces: vec![ws("a", false), ws("b", false)],
            ..Default::default()
        };
        assert_eq!(disable_all_auto_port_forward(&mut all_off), 0);
    }

    // Phase 53 (rebased): Browser/FileManager → Terminal rewrite.
    use super::{
        rewrite_browser_filemanager_panes_to_terminal, LayoutNode, PaneKind,
        SplitDirection,
    };

    fn pane(id: &str, kind: PaneKind) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.to_string(),
            pane_kind: kind,
            connection: None,
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

    fn ws_with_layout(id: &str, layout: LayoutNode) -> Workspace {
        Workspace {
            id: id.to_string(),
            name: id.to_string(),
            layout: Some(layout),
            ..Default::default()
        }
    }

    #[test]
    #[allow(deprecated)]
    fn phase_53_rewrites_browser_and_filemanager_panes_and_is_idempotent() {
        // Nested layout: Split(Browser, Split(FileManager, Terminal))
        let inner = LayoutNode::Split {
            split_id: "s2".into(),
            direction: SplitDirection::Vertical,
            first: Box::new(pane("p2", PaneKind::FileManager)),
            second: Box::new(pane("p3", PaneKind::Terminal)),
            ratio: 0.5,
        };
        let layout = LayoutNode::Split {
            split_id: "s1".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("p1", PaneKind::Browser)),
            second: Box::new(inner),
            ratio: 0.5,
        };
        let mut file = WorkspacesFile {
            workspaces: vec![ws_with_layout("w1", layout)],
            ..Default::default()
        };
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            2,
            "expected p1 (Browser) + p2 (FileManager) to be rewritten"
        );
        // Walk the migrated layout and confirm everything is Terminal.
        fn assert_all_terminal(n: &LayoutNode) {
            match n {
                LayoutNode::Pane { pane_kind, .. } => {
                    assert_eq!(*pane_kind, PaneKind::Terminal);
                }
                LayoutNode::Split { first, second, .. } => {
                    assert_all_terminal(first);
                    assert_all_terminal(second);
                }
            }
        }
        assert_all_terminal(file.workspaces[0].layout.as_ref().unwrap());
        // Second run is a no-op.
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            0
        );
    }

    #[test]
    fn phase_53_leaves_help_and_diff_alone() {
        let layout = LayoutNode::Split {
            split_id: "s1".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("p1", PaneKind::Help)),
            second: Box::new(pane("p2", PaneKind::Diff)),
            ratio: 0.5,
        };
        let mut file = WorkspacesFile {
            workspaces: vec![ws_with_layout("w1", layout)],
            ..Default::default()
        };
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            0
        );
    }

    #[test]
    fn phase_53_handles_workspace_with_no_layout() {
        let mut file = WorkspacesFile {
            workspaces: vec![Workspace {
                id: "w1".into(),
                name: "w1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            0
        );
    }
}

#[cfg(test)]
mod claude_session_scope_tests {
    use super::{claude_project_dir_prefix, paths_equal};

    #[test]
    fn dir_prefix_matches_claude_code_encoding() {
        // Every non-alphanumeric becomes '-', per the Agent SDK docs.
        assert_eq!(claude_project_dir_prefix("/Users/me/proj"), "-Users-me-proj");
        assert_eq!(
            claude_project_dir_prefix("/home/runner/src/ymux-feature.x"),
            "-home-runner-src-ymux-feature-x"
        );
        // Windows paths encode too — that is what the local branch globs.
        assert_eq!(claude_project_dir_prefix(r"C:\Users\y\p"), "C--Users-y-p");
    }

    #[test]
    fn dir_prefix_is_only_a_hint_never_a_path() {
        // Lossy on purpose: '-' collapses '/', '.' and '-' alike, so two
        // different repos can share a prefix. That is exactly why the
        // authoritative check is the cwd read out of the JSONL — trusting
        // this string as a path is Phase 65's `cd '-home-runner-tax'` bug.
        assert_eq!(
            claude_project_dir_prefix("/a/b-c"),
            claude_project_dir_prefix("/a-b/c")
        );
    }

    #[test]
    fn scope_comparison_ignores_separators_and_trailing_slash() {
        assert!(paths_equal("/srv/p", "/srv/p/"));
        assert!(paths_equal(r"C:\src\p", "C:/src/p"));
        assert!(!paths_equal("/srv/p", "/srv/p2"));
        // A worktree is NOT its repo: sessions must not leak between them.
        assert!(!paths_equal("/srv/p", "/srv/p-feature"));
    }
}

#[cfg(test)]
mod project_folder_migration_tests {
    use super::{
        collect_subtree_ids, migrate_legacy_project_folders, normalize_parents, Connection,
        Workspace, WorkspacesFile,
    };

    /// A v2/v3 file: one SSH workspace, one pinned folder on that host,
    /// and one worktree workspace bound to the folder.
    fn v3_file() -> (WorkspacesFile, String) {
        let text = r#"{
          "version": 1,
          "active_workspace_id": "w_root",
          "workspaces": [
            { "id": "w_root", "name": "runner",
              "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 } },
            { "id": "w_wt", "name": "feature/x",
              "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 },
              "project_folder_id": "pf_1",
              "worktree_path": "/home/runner/src/ymux-feature-x" }
          ],
          "project_folders": [
            { "id": "pf_1", "name": "ymux", "path": "/home/runner/src/ymux",
              "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 },
              "is_collapsed": true }
          ]
        }"#;
        let file: WorkspacesFile = serde_json::from_str(text).unwrap();
        (file, text.to_string())
    }

    #[test]
    fn a_pinned_folder_becomes_a_child_workspace() {
        let (mut file, text) = v3_file();
        // serde already dropped the removed keys — that is exactly the
        // data loss this migration exists to prevent.
        assert_eq!(file.workspaces.len(), 2);

        assert_eq!(migrate_legacy_project_folders(&mut file, &text), 1);
        assert_eq!(file.workspaces.len(), 3);

        let folder = file
            .workspaces
            .iter()
            .find(|w| w.is_project_root)
            .expect("folder workspace created");
        assert_eq!(folder.name, "ymux");
        assert_eq!(folder.cwd.as_deref(), Some("/home/runner/src/ymux"));
        assert_eq!(folder.parent_id.as_deref(), Some("w_root"));
        assert!(folder.is_collapsed, "collapse state carries over");
        assert!(matches!(folder.connection, Some(Connection::Ssh { .. })));
    }

    #[test]
    fn worktree_workspaces_are_re_parented_and_keep_their_directory() {
        let (mut file, text) = v3_file();
        migrate_legacy_project_folders(&mut file, &text);

        let folder_id = file
            .workspaces
            .iter()
            .find(|w| w.is_project_root)
            .unwrap()
            .id
            .clone();
        let wt = file.workspaces.iter().find(|w| w.id == "w_wt").unwrap();
        assert_eq!(wt.parent_id.as_deref(), Some(folder_id.as_str()));
        // worktree_path is gone from the struct; the cwd carries it now.
        assert_eq!(
            wt.cwd.as_deref(),
            Some("/home/runner/src/ymux-feature-x"),
            "the worktree path must survive as the cwd"
        );
        assert!(!wt.is_project_root, "a worktree is not a repo to scan");
    }

    #[test]
    fn a_folder_with_no_matching_host_is_kept_as_a_root() {
        // Discarding it would silently delete a pin the user made.
        let text = r#"{
          "version": 1,
          "workspaces": [],
          "project_folders": [
            { "id": "pf_1", "name": "orphan", "path": "/srv/orphan",
              "connection": { "type": "ssh", "host": "198.51.100.9", "user": "x", "port": 22 } }
          ]
        }"#;
        let mut file: WorkspacesFile = serde_json::from_str(text).unwrap();
        assert_eq!(migrate_legacy_project_folders(&mut file, text), 1);
        let w = &file.workspaces[0];
        assert!(w.parent_id.is_none());
        assert!(w.is_project_root);
        assert_eq!(w.cwd.as_deref(), Some("/srv/orphan"));
    }

    #[test]
    fn a_worktree_workspace_is_never_chosen_as_the_folders_parent() {
        // Both workspaces sit on the same host, but one of them belongs
        // to the folder being migrated — picking it would parent the
        // folder to its own child.
        let text = r#"{
          "version": 1,
          "workspaces": [
            { "id": "w_wt", "name": "feature/x",
              "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 },
              "project_folder_id": "pf_1", "worktree_path": "/srv/p-x" },
            { "id": "w_root", "name": "runner",
              "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 } }
          ],
          "project_folders": [
            { "id": "pf_1", "name": "p", "path": "/srv/p",
              "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 } }
          ]
        }"#;
        let mut file: WorkspacesFile = serde_json::from_str(text).unwrap();
        migrate_legacy_project_folders(&mut file, text);
        let folder = file.workspaces.iter().find(|w| w.is_project_root).unwrap();
        assert_eq!(folder.parent_id.as_deref(), Some("w_root"));
        assert_eq!(normalize_parents(&mut file), 0, "no repair needed");
    }

    #[test]
    fn a_file_without_the_legacy_key_is_untouched() {
        let text = r#"{ "version": 1, "workspaces": [{ "id": "w1", "name": "a" }] }"#;
        let mut file: WorkspacesFile = serde_json::from_str(text).unwrap();
        assert_eq!(migrate_legacy_project_folders(&mut file, text), 0);
        assert_eq!(file.workspaces.len(), 1);
    }

    fn child(id: &str, parent: Option<&str>) -> Workspace {
        Workspace {
            id: id.to_string(),
            name: id.to_string(),
            parent_id: parent.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn a_utf8_bom_does_not_brick_the_workspaces_file() {
        // Reached for real: restoring two fields by hand with PowerShell
        // 5.1's `Set-Content -Encoding utf8` prepended a BOM, serde_json
        // failed with "expected value at line 1 column 1", load_state went
        // Failed, and the app came up with ZERO workspaces and persistence
        // switched off. The parse has to survive the mark.
        let body = r#"{"version":1,"workspaces":[{"id":"w1","name":"a"}]}"#;
        let with_bom = format!("\u{feff}{body}");
        assert!(
            serde_json::from_str::<WorkspacesFile>(&with_bom).is_err(),
            "serde still rejects a BOM — the strip below is what saves us"
        );

        let stripped = with_bom.strip_prefix('\u{feff}').unwrap_or(&with_bom);
        let file: WorkspacesFile = serde_json::from_str(stripped).unwrap();
        assert_eq!(file.workspaces.len(), 1);
        // A file without one is untouched by the same strip.
        assert_eq!(body.strip_prefix('\u{feff}').unwrap_or(body), body);
    }

    #[test]
    fn the_tree_fields_survive_a_full_file_round_trip() {
        // Yossi lost parent_id AND is_project_root off two pinned folders
        // with nothing in debug.log to explain it. Every command that
        // writes a workspace mutates in place, so if they can vanish it
        // has to be here, in the WorkspacesFile round trip that
        // save_to_disk/load_from_disk perform on every persist.
        let json = r#"{
          "version": 1,
          "active_workspace_id": "w_child",
          "workspaces": [
            { "id": "w_root", "name": "runner1", "auto_port_forward": false,
              "last_active_at": 0, "claude_separate_account": false },
            { "id": "w_child", "name": "club", "cwd": "/home/runner/club",
              "parent_id": "w_root", "is_project_root": true,
              "auto_port_forward": false, "last_active_at": 0,
              "claude_separate_account": false }
          ],
          "groups": []
        }"#;
        let file: WorkspacesFile = serde_json::from_str(json).unwrap();
        let child = file.workspaces.iter().find(|w| w.id == "w_child").unwrap();
        assert_eq!(child.parent_id.as_deref(), Some("w_root"), "parent_id lost on LOAD");
        assert!(child.is_project_root, "is_project_root lost on LOAD");

        // And back out again, the way save_to_disk writes it.
        let text = serde_json::to_string_pretty(&file).unwrap();
        assert!(text.contains("\"parent_id\""), "parent_id lost on SAVE:
{text}");
        assert!(text.contains("\"is_project_root\""), "is_project_root lost on SAVE:
{text}");

        let again: WorkspacesFile = serde_json::from_str(&text).unwrap();
        let child = again.workspaces.iter().find(|w| w.id == "w_child").unwrap();
        assert_eq!(child.parent_id.as_deref(), Some("w_root"));
        assert!(child.is_project_root);
    }

    #[test]
    fn subtree_collects_the_whole_branch_and_nothing_else() {
        // root → folder → two worktrees, plus an unrelated root.
        let mut file = WorkspacesFile {
            workspaces: vec![
                child("root", None),
                child("folder", Some("root")),
                child("wt1", Some("folder")),
                child("wt2", Some("folder")),
                child("other", None),
            ],
            ..Default::default()
        };
        let mut ids = collect_subtree_ids(&file, "root");
        ids.sort();
        assert_eq!(ids, vec!["folder", "root", "wt1", "wt2"]);

        // Deleting the folder takes only its own branch.
        let mut ids = collect_subtree_ids(&file, "folder");
        ids.sort();
        assert_eq!(ids, vec!["folder", "wt1", "wt2"]);

        // A leaf is just itself.
        assert_eq!(collect_subtree_ids(&file, "wt1"), vec!["wt1"]);

        // And a cycle must not take the whole file with it: even though
        // normalize_parents repairs these at load, a delete that looped
        // would be unrecoverable.
        file.workspaces[0].parent_id = Some("wt1".to_string());
        let ids = collect_subtree_ids(&file, "root");
        assert_eq!(ids.len(), 4, "visited set caps the walk");
    }

    #[test]
    fn normalize_detaches_self_parents_and_dangling_ids() {
        let mut file = WorkspacesFile {
            workspaces: vec![
                child("a", Some("a")),
                child("b", Some("ghost")),
                child("c", None),
            ],
            ..Default::default()
        };
        assert_eq!(normalize_parents(&mut file), 2);
        assert!(file.workspaces.iter().all(|w| w.parent_id.is_none()));
    }

    #[test]
    fn normalize_breaks_a_cycle_instead_of_hanging() {
        // a → b → c → a. A recursive sidebar renderer on this spins
        // forever with no error card and nothing in debug.log.
        let mut file = WorkspacesFile {
            workspaces: vec![
                child("a", Some("b")),
                child("b", Some("c")),
                child("c", Some("a")),
            ],
            ..Default::default()
        };
        assert!(normalize_parents(&mut file) >= 1);
        assert_eq!(normalize_parents(&mut file), 0, "must converge");

        // Walking up from every node now terminates.
        for start in ["a", "b", "c"] {
            let mut cur = start.to_string();
            for _ in 0..8 {
                match file
                    .workspaces
                    .iter()
                    .find(|w| w.id == cur)
                    .and_then(|w| w.parent_id.clone())
                {
                    Some(p) => cur = p,
                    None => break,
                }
            }
            assert!(
                file.workspaces
                    .iter()
                    .find(|w| w.id == cur)
                    .map(|w| w.parent_id.is_none())
                    .unwrap_or(true),
                "walking up from {start} must reach a root"
            );
        }
    }
}

#[cfg(test)]
mod smart_connect_tests {
    // Phase 61: Smart Connect injection became shell-aware so local
    // PowerShell / Cmd panes can launch Claude Code too. Phase 65 (bug FF
    // round 2): POSIX no longer uses `exec <cmd>` — it runs the command then
    // hands off to a fresh interactive shell (`; exec "${SHELL:-bash}"`) so
    // the SSH channel survives Claude exiting. The other two shells must not
    // contain `exec` (it doesn't exist there).
    use super::{
        build_smart_connect_script, resolve_effective_session_name, session_name_for_pane,
        ShellKind,
    };

    // 2026-08-23: the attach-only guard checks one name and the attach uses
    // another the moment these two disagree, which is the failure mode the
    // extraction was done to remove. Pin the precedence.
    #[test]
    fn session_name_precedence_is_explicit_then_title_then_pane_id() {
        assert_eq!(
            resolve_effective_session_name(Some("picked"), Some("My Title")).as_deref(),
            Some("picked")
        );
        assert_eq!(
            resolve_effective_session_name(None, Some("My Title")).as_deref(),
            Some("My_Title")
        );
        assert_eq!(resolve_effective_session_name(None, None), None);
        // An empty explicit name is "no choice", not a session called "".
        assert_eq!(
            resolve_effective_session_name(Some(""), Some("My Title")).as_deref(),
            Some("My_Title")
        );
    }

    #[test]
    fn pane_fallback_name_matches_the_legacy_auto_name() {
        assert_eq!(session_name_for_pane(None, None, "p_1a2b"), "ymux-p_1a2b");
        assert_eq!(
            session_name_for_pane(None, Some("  "), "p_1a2b"),
            "ymux-p_1a2b"
        );
        assert_eq!(session_name_for_pane(Some("picked"), None, "p_1a2b"), "picked");
    }

    #[test]
    fn posix_claude_hands_off_to_fresh_shell() {
        assert_eq!(
            build_smart_connect_script(ShellKind::Posix, "claude", None, None, None),
            "claude; exec \"${SHELL:-bash}\"\r\n"
        );
        assert_eq!(
            build_smart_connect_script(
                ShellKind::Posix,
                "claude",
                Some("/home/x/my proj"),
                None,
                Some("--resume abc"),
            ),
            "cd '/home/x/my proj' && claude --resume abc; exec \"${SHELL:-bash}\"\r\n"
        );
    }

    #[test]
    fn powershell_claude_no_exec_and_quotes_escaped() {
        assert_eq!(
            build_smart_connect_script(ShellKind::PowerShell, "claude", None, None, Some("--continue")),
            "claude --continue\r\n"
        );
        assert_eq!(
            build_smart_connect_script(
                ShellKind::PowerShell,
                "claude",
                Some(r"C:\Users\yo'si\code"),
                None,
                None,
            ),
            "Set-Location -LiteralPath 'C:\\Users\\yo''si\\code'; claude\r\n"
        );
    }

    #[test]
    fn cmd_claude_uses_cd_slash_d_and_strips_quotes() {
        assert_eq!(
            build_smart_connect_script(
                ShellKind::Cmd,
                "claude",
                Some(r#"D:\pro"j"#),
                None,
                None,
            ),
            "cd /d \"D:\\proj\" && claude\r\n"
        );
    }

    #[test]
    fn cmd_mode_runs_command_and_empty_returns_nothing() {
        assert_eq!(
            build_smart_connect_script(ShellKind::PowerShell, "cmd", None, Some("npm run dev"), None),
            "npm run dev\r\n"
        );
        assert_eq!(
            build_smart_connect_script(ShellKind::Posix, "cmd", None, Some("htop"), None),
            "htop; exec \"${SHELL:-bash}\"\r\n"
        );
        // Empty / whitespace command → nothing to inject, even with a cwd.
        assert_eq!(
            build_smart_connect_script(ShellKind::Cmd, "cmd", Some(r"C:\x"), Some("  "), None),
            ""
        );
        // Unknown mode → nothing.
        assert_eq!(
            build_smart_connect_script(ShellKind::Posix, "default", None, None, None),
            ""
        );
    }
}

#[cfg(test)]
mod session_workspace_tests {
    // Phase 87.B: where a session row lands in the tree.
    use super::pick_session_parent;
    use super::WorkspacesFile;

    fn file() -> WorkspacesFile {
        serde_json::from_str(
            r#"{
              "version": 1,
              "workspaces": [
                { "id": "srv", "name": "runner",
                  "connection": { "type": "ssh", "host": "203.0.113.5", "user": "runner", "port": 22 } },
                { "id": "app", "name": "app", "parent_id": "srv", "is_project_root": true,
                  "cwd": "/srv/app" },
                { "id": "app2", "name": "app2", "parent_id": "srv", "is_project_root": true,
                  "cwd": "/srv/app2" },
                { "id": "deep", "name": "deep", "parent_id": "srv", "is_project_root": true,
                  "cwd": "/srv/app/packages/web" },
                { "id": "wt", "name": "wt", "parent_id": "app", "cwd": "/srv/app-wt" },
                { "id": "other", "name": "other-server", "is_project_root": true, "cwd": "/srv/app",
                  "connection": { "type": "ssh", "host": "198.51.100.9", "user": "x", "port": 22 } }
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn no_cwd_lands_under_the_root() {
        assert_eq!(pick_session_parent(&file(), "srv", None), "srv");
        assert_eq!(pick_session_parent(&file(), "srv", Some("  ")), "srv");
    }

    #[test]
    fn exact_folder_and_nested_path_both_match() {
        assert_eq!(pick_session_parent(&file(), "srv", Some("/srv/app2")), "app2");
        assert_eq!(pick_session_parent(&file(), "srv", Some("/srv/app2/src")), "app2");
    }

    #[test]
    fn prefix_without_a_separator_boundary_is_not_inside() {
        // /srv/app2x starts with /srv/app2 but is a sibling, not a child.
        assert_eq!(pick_session_parent(&file(), "srv", Some("/srv/app2x")), "srv");
    }

    #[test]
    fn deepest_matching_folder_wins() {
        assert_eq!(
            pick_session_parent(&file(), "srv", Some("/srv/app/packages/web/src")),
            "deep"
        );
        assert_eq!(pick_session_parent(&file(), "srv", Some("/srv/app/packages")), "app");
    }

    #[test]
    fn only_project_roots_under_this_root_are_candidates() {
        // `wt` shares no boundary; `other` is a folder on ANOTHER server with
        // the same path — neither may catch the row.
        assert_eq!(pick_session_parent(&file(), "srv", Some("/srv/app-wt")), "srv");
        assert_eq!(pick_session_parent(&file(), "other", Some("/srv/app/x")), "other");
    }
}

#[cfg(test)]
mod tmux_list_parse_tests {
    // Shared parser for every `tmux list-sessions -F` path (SSH, WSL and
    // the macOS local server). Sample lines are exactly what tmux prints
    // for TMUX_LIST_FORMAT.
    use super::{
        annotate_scope_with, last_path_segment, parse_tmux_sessions, path_is_within,
        tmux_list_script, ForeignKind, SessionOwner, TmuxSessionInfo, TMUX_LIST_FORMAT,
        TMUX_META_MARKER,
    };
    use std::collections::HashMap;

    // ── 2026-08-24 scope-annotation fixtures ───────────────────────────────
    // Sessions are built through the real parser so a format change breaks
    // these tests too, rather than leaving them asserting against a shape
    // tmux no longer emits.

    fn session(name: &str, cwd: Option<&str>) -> TmuxSessionInfo {
        let line = match cwd {
            Some(c) => format!("{name}|1|0|1|2|{c}\n"),
            None => format!("{name}|1|0|1|2\n"),
        };
        parse_tmux_sessions(&line).pop().expect("one session")
    }

    fn owner(workspace_id: &str, cwd: Option<&str>) -> SessionOwner {
        SessionOwner {
            workspace_id: workspace_id.to_string(),
            cwd: cwd.map(|c| c.to_string()),
            ts: 0,
        }
    }

    fn owners(rows: &[(&str, SessionOwner)]) -> HashMap<String, SessionOwner> {
        rows.iter()
            .map(|(n, o)| (n.to_string(), o.clone()))
            .collect()
    }

    fn names(rows: &[(&str, &str)]) -> HashMap<String, String> {
        rows.iter()
            .map(|(id, n)| (id.to_string(), n.to_string()))
            .collect()
    }

    #[test]
    fn format_has_six_pipe_separated_fields() {
        // 2026-08-23: six, not five — `#{session_path}` was appended so the
        // picker can scope a list to a project folder.
        assert_eq!(TMUX_LIST_FORMAT.split('|').count(), 6);
        assert!(TMUX_LIST_FORMAT.starts_with("#{session_name}"));
        assert!(TMUX_LIST_FORMAT.ends_with("#{session_path}"));
    }

    #[test]
    fn list_script_is_built_from_the_format_const() {
        // The two used to be independent copies of the same string, and they
        // drifted the moment a field was added. This is the guard against a
        // third copy appearing.
        let script = tmux_list_script();
        assert!(script.contains(TMUX_LIST_FORMAT), "script: {script}");
        assert!(script.contains(TMUX_META_MARKER));
    }

    #[test]
    fn empty_and_no_server_output_parse_to_nothing() {
        assert!(parse_tmux_sessions("").is_empty());
        assert!(parse_tmux_sessions("\n").is_empty());
        // Garbage lines with fewer than five fields are skipped, not errors.
        assert!(parse_tmux_sessions("no server running on /tmp/tmux-501/default").is_empty());
    }

    #[test]
    fn parses_lines_and_sorts_most_recent_first() {
        let text = "winmux-a1b2|1700000000|0|1|1700000500\nmain|1700001000|1|3|1700002000\n";
        let out = parse_tmux_sessions(text);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "main");
        assert!(out[0].attached);
        assert_eq!(out[0].windows, 3);
        assert_eq!(out[0].last_attached, 1700002000);
        assert_eq!(out[1].name, "winmux-a1b2");
        assert!(!out[1].attached);
        assert!(out[1].label.is_none());
    }

    #[test]
    fn sixth_field_is_the_session_cwd() {
        let out = parse_tmux_sessions("main|1|0|1|2|/srv/app\n");
        assert_eq!(out[0].cwd.as_deref(), Some("/srv/app"));
        // The parser never decides scope; that is annotate_session_scope's job.
        assert!(!out[0].owned);
        assert!(!out[0].in_cwd);
    }

    #[test]
    fn five_field_lines_still_parse_with_unknown_cwd() {
        // A remote that has not been re-bootstrapped, or any tmux answering
        // the OLD format, emits five fields. Dropping those lines would empty
        // the picker on exactly the servers that need it most.
        let out = parse_tmux_sessions("main|1|0|1|2\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "main");
        assert!(out[0].cwd.is_none());
    }

    #[test]
    fn empty_sixth_field_reads_as_unknown_not_root() {
        let out = parse_tmux_sessions("main|1|0|1|2|\n");
        assert_eq!(out.len(), 1);
        assert!(out[0].cwd.is_none());
    }

    #[test]
    fn folder_scope_stops_at_a_separator() {
        // The whole reason path_is_within exists: `/srv/app2` starts with
        // `/srv/app`, and a plain starts_with would leak one project folder's
        // sessions into its neighbour's list.
        assert!(path_is_within("/srv/app", "/srv/app"));
        assert!(path_is_within("/srv/app/sub", "/srv/app"));
        assert!(path_is_within("/srv/app/", "/srv/app"));
        assert!(!path_is_within("/srv/app2", "/srv/app"));
        assert!(!path_is_within("/srv", "/srv/app"));
        assert!(!path_is_within("/other", "/srv/app"));
        // Windows paths reach this via zellij-era ownership rows.
        assert!(path_is_within(r"C:\src\app\sub", r"C:\src\app"));
        assert!(!path_is_within(r"C:\src\app2", r"C:\src\app"));
        // An empty root would otherwise match everything.
        assert!(!path_is_within("/srv/app", ""));
        assert!(!path_is_within("", "/srv/app"));
    }

    #[test]
    fn joins_session_meta_after_marker() {
        let text = format!(
            "main|1|0|1|2\nother|3|0|1|4\n{TMUX_META_MARKER}\n{{\"sessions\":{{\"main\":{{\"label\":\"Prod\",\"origin\":\"m1\"}}}}}}\n"
        );
        let out = parse_tmux_sessions(&text);
        assert_eq!(out.len(), 2);
        let main = out.iter().find(|s| s.name == "main").expect("main");
        assert_eq!(main.label.as_deref(), Some("Prod"));
        assert_eq!(main.origin.as_deref(), Some("m1"));
        let other = out.iter().find(|s| s.name == "other").expect("other");
        assert!(other.label.is_none());
    }

    #[test]
    fn garbled_meta_degrades_to_no_metadata() {
        let text = format!("main|1|0|1|2\n{TMUX_META_MARKER}\nnot json at all");
        let out = parse_tmux_sessions(&text);
        assert_eq!(out.len(), 1);
        assert!(out[0].label.is_none());
    }

    // ── 2026-08-24: "this session belongs to another folder" ───────────────

    #[test]
    fn basename_stops_at_either_separator() {
        // Sibling of folder_scope_stops_at_a_separator: one picker list mixes
        // POSIX paths off a Linux host with Windows paths from zellij-era
        // ownership rows, so both separators have to work.
        assert_eq!(last_path_segment("/srv/app"), Some("app"));
        assert_eq!(last_path_segment("/srv/app/"), Some("app"));
        assert_eq!(last_path_segment("  /srv/app  "), Some("app"));
        assert_eq!(last_path_segment(r"C:\src\app"), Some("app"));
        assert_eq!(last_path_segment(r"C:\src\app\"), Some("app"));
        assert_eq!(last_path_segment("app"), Some("app"));
        // A bare root names nothing, and neither does an empty string.
        assert_eq!(last_path_segment("/"), None);
        assert_eq!(last_path_segment(""), None);
        assert_eq!(last_path_segment("   "), None);
    }

    #[test]
    fn a_foreign_badge_is_impossible_inside_the_workspace_view() {
        // THE invariant the picker leans on: "This folder" is exactly the
        // complement of `foreign`, so the frontend needs no scope conditional
        // on the badge. Every way a session can be in scope, checked.
        let mut sessions = vec![
            session("owned-and-in-cwd", Some("/srv/app")),
            session("owned-only", Some("/elsewhere")),
            session("in-cwd-only", Some("/srv/app/sub")),
        ];
        let host = owners(&[
            ("owned-and-in-cwd", owner("ws-a", Some("/srv/app"))),
            ("owned-only", owner("ws-a", None)),
        ]);
        annotate_scope_with(
            &mut sessions,
            Some(&host),
            "ws-a",
            Some("/srv/app"),
            &names(&[("ws-a", "Mine")]),
        );
        for s in &sessions {
            assert!(s.owned || s.in_cwd, "{} should be in scope", s.name);
            assert!(s.foreign.is_none(), "{} must not be foreign", s.name);
        }
    }

    #[test]
    fn a_claim_by_another_workspace_names_that_workspace() {
        let mut sessions = vec![session("theirs", Some("/srv/other"))];
        let host = owners(&[("theirs", owner("ws-b", Some("/srv/other")))]);
        annotate_scope_with(
            &mut sessions,
            Some(&host),
            "ws-a",
            Some("/srv/app"),
            &names(&[("ws-a", "Mine"), ("ws-b", "Server API")]),
        );
        let f = sessions[0].foreign.as_ref().expect("foreign");
        assert!(matches!(f.kind, ForeignKind::Workspace));
        assert_eq!(f.label.as_deref(), Some("Server API"));
        // The tooltip gets the LIVE path, not the one recorded at claim time.
        assert_eq!(f.path.as_deref(), Some("/srv/other"));
    }

    #[test]
    fn a_claim_survives_on_windows_where_there_is_no_cwd_at_all() {
        // The zellij case, and the reason ownership exists as a second signal:
        // no session cwd anywhere, so the claim is the ONLY thing that can
        // place this session. It must still warn, and the tooltip falls back
        // to the folder recorded when the claim was made.
        let mut sessions = vec![session("zj", None)];
        let host = owners(&[("zj", owner("ws-b", Some(r"C:\src\other")))]);
        annotate_scope_with(
            &mut sessions,
            Some(&host),
            "ws-a",
            None,
            &names(&[("ws-b", "Other")]),
        );
        let f = sessions[0].foreign.as_ref().expect("foreign");
        assert!(matches!(f.kind, ForeignKind::Workspace));
        assert_eq!(f.label.as_deref(), Some("Other"));
        assert_eq!(f.path.as_deref(), Some(r"C:\src\other"));
    }

    #[test]
    fn a_claim_by_a_deleted_workspace_still_warns() {
        // Nothing prunes session-owners.json when a workspace is deleted, so a
        // row can name an id that resolves to nothing. Fall back to the folder
        // it recorded; and when it recorded none, warn anyway with no label —
        // the picker words that case generically rather than staying silent.
        let mut sessions = vec![session("orphan", None), session("nameless", None)];
        let host = owners(&[
            ("orphan", owner("ws-gone", Some("/srv/other"))),
            ("nameless", owner("ws-gone", None)),
        ]);
        annotate_scope_with(&mut sessions, Some(&host), "ws-a", None, &names(&[]));
        let orphan = sessions[0].foreign.as_ref().expect("orphan foreign");
        assert_eq!(orphan.label.as_deref(), Some("other"));
        let nameless = sessions[1].foreign.as_ref().expect("nameless foreign");
        assert!(nameless.label.is_none());
        assert!(nameless.path.is_none());
    }

    #[test]
    fn an_unclaimed_session_outside_the_root_is_flagged_by_folder() {
        let mut sessions = vec![
            session("elsewhere", Some("/srv/other")),
            // The neighbour case: the badge inherits path_is_within's
            // separator boundary instead of re-deriving containment.
            session("neighbour", Some("/srv/app2")),
        ];
        annotate_scope_with(&mut sessions, None, "ws-a", Some("/srv/app"), &names(&[]));
        for (s, want) in sessions.iter().zip(["other", "app2"]) {
            let f = s.foreign.as_ref().expect("foreign");
            assert!(matches!(f.kind, ForeignKind::Folder));
            assert_eq!(f.label.as_deref(), Some(want));
        }
    }

    #[test]
    fn an_unknown_cwd_is_never_evidence_of_elsewhere() {
        // The direct guard on the 2026-08-23 rule. A five-field tmux and every
        // zellij session land here; painting them as another project's would
        // be a fabrication, and it is the regression that matters most.
        let mut sessions = vec![session("mystery", None)];
        annotate_scope_with(&mut sessions, None, "ws-a", Some("/srv/app"), &names(&[]));
        assert!(sessions[0].foreign.is_none());
    }

    #[test]
    fn no_root_means_no_folder_verdict() {
        // A workspace that is not folder-anchored, and the `projectPath: null`
        // call shape session restore uses. Without a root there is nothing to
        // be outside of, so an unclaimed session must not light up.
        let mut sessions = vec![session("somewhere", Some("/srv/other"))];
        annotate_scope_with(&mut sessions, None, "ws-a", None, &names(&[]));
        assert!(sessions[0].foreign.is_none());
    }
}

#[cfg(test)]
mod claude_jsonl_peek_tests {
    use super::peek_claude_jsonl;

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("winmux-peek-{name}-{}.jsonl", std::process::id()));
        std::fs::write(&p, body).expect("write tmp");
        p
    }

    #[test]
    fn reads_cwd_previews_and_sidechain_from_head_and_tail() {
        let body = concat!(
            "{\"type\":\"user\",\"cwd\":\"/Users/yossi/dev/foo\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hello there\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"last answer\"}]}}\n",
        );
        let p = write_tmp("main", body);
        let peek = peek_claude_jsonl(&p);
        let _ = std::fs::remove_file(&p);
        assert_eq!(peek.cwd.as_deref(), Some("/Users/yossi/dev/foo"));
        assert!(!peek.is_subagent);
        assert_eq!(peek.first_user.as_deref(), Some("hello there"));
        assert_eq!(peek.last_assistant.as_deref(), Some("last answer"));
    }

    #[test]
    fn sidechain_flag_and_missing_file() {
        let p = write_tmp("side", "{\"type\":\"user\",\"isSidechain\":true,\"cwd\":\"C:\\\\dev\\\\x\"}\n");
        let peek = peek_claude_jsonl(&p);
        let _ = std::fs::remove_file(&p);
        assert!(peek.is_subagent);
        assert_eq!(peek.cwd.as_deref(), Some("C:\\dev\\x"));
        let none = peek_claude_jsonl(std::path::Path::new("/nonexistent/winmux/x.jsonl"));
        assert!(none.cwd.is_none() && none.last_assistant.is_none());
    }
}

#[cfg(test)]
mod utf8_shell_tests {
    // A fresh ConPTY inherits the machine's OEM codepage (862 on a Hebrew
    // install), and Windows PowerShell 5.1 defaults $OutputEncoding to
    // ASCII — together they mojibake every Hebrew byte a native command
    // prints. These args are the only thing forcing the local transports
    // to UTF-8; SSH/WSL get it free by landing on Linux.
    use super::{detect_shell_kind, utf8_shell_args, ShellKind};

    #[test]
    fn powershell_forces_utf8_and_stays_interactive() {
        let args = utf8_shell_args(ShellKind::PowerShell);
        assert_eq!(args[0], "-NoExit", "the pane must stay interactive");
        assert_eq!(args[1], "-Command");
        let script = args[2];
        assert!(script.contains("chcp 65001"), "console codepage not set");
        assert!(
            script.contains("[Console]::OutputEncoding"),
            "native-command output decoding not set"
        );
        assert!(
            script.contains("$OutputEncoding"),
            "PS 5.1 pipes to native commands stay ASCII without this"
        );
        // -NoProfile would silently drop the user's profile; we only prepend.
        assert!(!script.contains("-NoProfile"));
        assert!(!args.contains(&"-NoProfile"));
        // Nested double quotes would not survive CommandBuilder quoting.
        assert!(!script.contains('"'), "script must stay double-quote free");
    }

    #[test]
    fn cmd_forces_utf8_and_stays_interactive() {
        assert_eq!(utf8_shell_args(ShellKind::Cmd), vec!["/K", "chcp 65001 >nul"]);
    }

    #[test]
    fn posix_shell_is_left_alone() {
        assert!(utf8_shell_args(ShellKind::Posix).is_empty());
    }

    #[test]
    fn unix_shell_paths_are_posix() {
        // macOS port: what detect_local_shells / $SHELL hand back.
        for sh in ["/bin/zsh", "/bin/bash", "/usr/local/bin/fish", "zsh"] {
            assert!(matches!(super::detect_shell_kind(sh), ShellKind::Posix), "{sh}");
        }
        assert!(matches!(
            super::detect_shell_kind("/usr/local/bin/pwsh"),
            ShellKind::PowerShell
        ));
    }

    #[test]
    fn custom_command_splits_into_argv() {
        let (p, a) = super::split_shell_command("  zsh   -l ");
        assert_eq!(p, "zsh");
        assert_eq!(a, vec!["-l"]);
        let (p, a) = super::split_shell_command("/bin/bash");
        assert_eq!(p, "/bin/bash");
        assert!(a.is_empty());
        let (p, a) = super::split_shell_command("");
        assert_eq!(p, "");
        assert!(a.is_empty());
    }

    #[test]
    fn shell_kinds_route_to_the_right_args() {
        // pick_default_shell can hand back a bare name or a full path.
        for ps in ["pwsh.exe", "powershell.exe", r"C:\Windows\System32\powershell.exe"] {
            assert!(
                !utf8_shell_args(detect_shell_kind(ps)).is_empty(),
                "{ps} should get the UTF-8 preamble"
            );
        }
        assert_eq!(utf8_shell_args(detect_shell_kind("cmd.exe"))[0], "/K");
        // A user-picked git-bash must not receive PowerShell args.
        assert!(utf8_shell_args(detect_shell_kind("bash.exe")).is_empty());
    }
}

/// Live ConPTY proof for the local-shell UTF-8 preamble.
///
/// The unit tests above only assert the argv we build. These spawn a real
/// PTY the same way `spawn_local_pty` does and check what actually comes
/// back, because the bug they close is invisible to a compile: on a Hebrew
/// Windows the console starts at CP862 and PS 5.1's `$OutputEncoding` is
/// ASCII, so Hebrew from a native command arrives as mojibake.
#[cfg(all(test, windows))]
mod utf8_pty_live_tests {
    use super::{utf8_shell_args, ShellKind};
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    /// Drive a real powershell.exe over a PTY and return everything it
    /// printed. `extra` is prepended exactly as spawn_local_pty does.
    fn run_in_pty(extra: &[&'static str], script: &str) -> String {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("powershell.exe");
        for a in extra {
            cmd.arg(a);
        }
        let mut child = pair.slave.spawn_command(cmd).expect("spawn powershell");
        drop(pair.slave);

        let mut writer = pair.master.take_writer().expect("writer");
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let collector = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Reads unblock when the child exits and the master closes.
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            buf
        });

        // PowerShell 5.1 needs a moment before it consumes stdin; writing
        // into a not-yet-ready ConPTY loses the line.
        std::thread::sleep(Duration::from_millis(1500));
        write!(writer, "{script}\r\nexit\r\n").expect("write script");
        writer.flush().expect("flush");

        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        // Both the writer and the master must outlive the child, or
        // ConPTY tears the session down before the shell has spoken.
        drop(writer);
        drop(pair.master);
        // Lossy on purpose: the whole point is to see whether the bytes
        // that came back are valid UTF-8 Hebrew or replacement junk.
        String::from_utf8_lossy(&collector.join().unwrap_or_default()).into_owned()
    }

    #[test]
    fn preamble_puts_a_real_powershell_pane_on_utf8() {
        let out = run_in_pty(
            &utf8_shell_args(ShellKind::PowerShell),
            "Write-Output ('CP=' + [Console]::OutputEncoding.CodePage) ; \
             Write-Output ('PIPE=' + $OutputEncoding.WebName)",
        );
        assert!(out.contains("CP=65001"), "console codepage not 65001:\n{out}");
        assert!(out.contains("PIPE=utf-8"), "$OutputEncoding not utf-8:\n{out}");
    }

    /// The control: the same pane WITHOUT the preamble is what shipped in
    /// v0.4.5-beta.1. If this ever starts reporting utf-8 the preamble has
    /// become redundant — but on Windows PowerShell 5.1 it never is.
    #[test]
    fn without_the_preamble_powershell_5_1_pipes_ascii() {
        let out = run_in_pty(&[], "Write-Output ('PIPE=' + $OutputEncoding.WebName)");
        assert!(
            out.contains("PIPE=us-ascii"),
            "expected the un-preambled 5.1 default; got:\n{out}"
        );
    }

    /// The user-visible bug: Hebrew emitted by a NATIVE command (not by
    /// PowerShell itself) has to survive the console codepage round-trip.
    #[test]
    fn hebrew_from_a_native_command_survives_the_pty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hebrew.txt");
        // Raw UTF-8, no BOM — exactly what a Linux-authored file or a
        // modern CLI writes.
        std::fs::write(&path, "שלום עולם".as_bytes()).expect("write fixture");

        let script = format!("cmd /c type \"{}\"", path.display());
        let out = run_in_pty(&utf8_shell_args(ShellKind::PowerShell), &script);
        assert!(
            out.contains("שלום עולם"),
            "Hebrew did not survive the local PTY:\n{out}"
        );
        assert!(
            !out.contains('\u{FFFD}'),
            "replacement chars in the stream:\n{out}"
        );
    }
}

#[cfg(test)]
mod zellij_tests {
    // Parser tests written against output CAPTURED from zellij 0.44.3 on
    // Windows on 2026-08-19, not from the docs — the spike session Yossi left
    // running produced `spike [Created 12m 30s ago]` verbatim.
    use super::{
        build_zellij_attach_command, parse_zellij_duration, parse_zellij_sessions,
        pick_zellij_resources, sanitize_tmux_session_name_for_title,
        session_name_char_is_safe, zellij_args_delete_force, zellij_args_list,
        zellij_args_write_chars, zellij_spawn_error_outcome, KillSessionOutcome,
        ZellijOutcome,
    };

    // ── The shipped resources ────────────────────────────────────────────
    //
    // Embedded with `include_str!`, not read from a path at runtime — the same
    // choice remote-manifest.json makes above, for the same reason: a runtime
    // read passes on a dev box and fails on a user's machine, while a missing
    // or renamed file here fails the BUILD. That is exactly the class of
    // failure 3709c53 spent a round chasing.
    const ZELLIJ_KDL: &str = include_str!("../resources/ymux-zellij.kdl");
    const ZELLIJ_LAYOUT: &str = include_str!("../resources/layouts/ymux.kdl");
    const TAURI_CONF: &str = include_str!("../tauri.conf.json");

    /// Strip `//` comment lines so an assertion cannot pass on prose. Both kdl
    /// files are mostly comments, and every one of them mentions the keys.
    fn code_only(kdl: &str) -> String {
        kdl.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Zellij is a persistence layer under a ymux pane, not a multiplexer the
    /// user drives. Each key here is one piece of that; a silent drop would
    /// show up only as chrome reappearing on someone's screen.
    #[test]
    fn zellij_config_is_the_full_lock() {
        let code = code_only(ZELLIJ_KDL);
        for key in [
            "pane_frames false",
            "default_layout \"ymux\"",
            "keybinds clear-defaults=true {",
            "default_mode \"locked\"",
            "mouse_mode true",
            "show_release_notes false",
            "show_startup_tips false",
        ] {
            assert!(code.contains(key), "ymux-zellij.kdl lost `{key}`:\n{code}");
        }
    }

    /// `keybinds clear-defaults=true` with NO children fails to parse —
    /// "keybindings with no children", from `zellij setup --check` on
    /// 2026-08-20. The empty body is load-bearing, so it is pinned.
    #[test]
    fn the_cleared_keybinds_block_keeps_its_body() {
        let code = code_only(ZELLIJ_KDL);
        let at = code
            .find("keybinds clear-defaults=true")
            .expect("keybinds line");
        assert!(
            code[at..].starts_with("keybinds clear-defaults=true {"),
            "the empty `{{ }}` body is required by zellij's parser:\n{}",
            &code[at..],
        );
    }

    /// One pane, no plugin rows. The two plugin rows in zellij's DEFAULT
    /// layout are what "the frame is still there" actually was — `pane_frames
    /// false` never touched them.
    #[test]
    fn zellij_layout_is_one_pane_and_no_plugins() {
        let code = code_only(ZELLIJ_LAYOUT);
        assert!(
            !code.contains("plugin location="),
            "a plugin row is chrome the pane cannot afford:\n{code}"
        );
        assert!(!code.contains("tab "), "no explicit tab node:\n{code}");
        assert_eq!(
            code.matches("pane").count(),
            1,
            "exactly one pane — ymux owns splitting:\n{code}"
        );
    }

    /// The config selects the layout BY NAME. Renaming one file without the
    /// other leaves a `default_layout` that resolves to nothing, and zellij is
    /// not loud about that.
    #[test]
    fn the_config_names_the_layout_file_that_ships_with_it() {
        assert!(
            code_only(ZELLIJ_KDL).contains("default_layout \"ymux\""),
            "default_layout must name layouts/ymux.kdl"
        );
    }

    /// Wrote the file, forgot the bundle entry — an installed build would then
    /// find neither and fall back to zellij's own config.
    #[test]
    fn bundled_zellij_resources_are_declared_in_tauri_conf() {
        let conf: serde_json::Value =
            serde_json::from_str(TAURI_CONF).expect("tauri.conf.json must parse");
        let res = conf["bundle"]["resources"]
            .as_array()
            .expect("bundle.resources must be an array");
        for want in ["resources/ymux-zellij.kdl", "resources/layouts/ymux.kdl"] {
            assert!(
                res.iter().any(|v| v.as_str() == Some(want)),
                "tauri.conf.json does not bundle `{want}`"
            );
        }
    }

    /// All-or-nothing: a directory holding only the config would promise a
    /// `default_layout` the layouts dir cannot keep.
    #[test]
    fn zellij_resources_are_all_or_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let roots = vec![root.clone()];

        assert!(
            pick_zellij_resources(&roots).is_none(),
            "an empty root resolves to nothing"
        );

        std::fs::write(root.join("ymux-zellij.kdl"), "pane_frames false\n").expect("write config");
        assert!(
            pick_zellij_resources(&roots).is_none(),
            "config without the layout must be refused"
        );

        std::fs::create_dir_all(root.join("layouts")).expect("mkdir layouts");
        std::fs::write(root.join("layouts").join("ymux.kdl"), "layout {\n pane\n}\n")
            .expect("write layout");
        let got = pick_zellij_resources(&roots).expect("both present resolves");
        assert_eq!(got.config_dir, root);
        assert_eq!(got.config_file, root.join("ymux-zellij.kdl"));
    }

    /// The layout-only half of the same rule, kept separate so a failure names
    /// which direction broke.
    #[test]
    fn a_layout_without_its_config_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("layouts")).expect("mkdir layouts");
        std::fs::write(root.join("layouts").join("ymux.kdl"), "layout {\n pane\n}\n")
            .expect("write layout");
        assert!(pick_zellij_resources(&[root]).is_none());
    }

    #[test]
    fn parses_the_real_captured_line() {
        let out = parse_zellij_sessions("spike [Created 12m 30s ago]\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "spike");
        assert!(!out[0].attached);
        // created = now - 750s; allow a couple of seconds of clock drift.
        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - out[0].created;
        assert!((748..=752).contains(&age), "age was {age}, expected ~750");
    }

    #[test]
    /// 2026-08-20: this is no longer just "do not drop rows" — it is the
    /// REBOOT-RESTORE path. A Windows reboot leaves every zellij session
    /// EXITED, and App.tsx feeds this list to `attach -c` to bring them back.
    /// The guarantee that a KILLED session stays dead is `delete-session -f`
    /// in pane_kill_session, not a filter here.
    fn an_exited_session_is_kept_because_it_can_be_resurrected() {
        // Surviving a reboot is the one thing zellij does that tmux cannot.
        // Dropping these rows would throw away the whole point.
        let out = parse_zellij_sessions(
            "old [Created 3h 4m 1s ago] (EXITED - attach to resurrect)\n",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "old");
        assert!(!out[0].attached);
    }

    #[test]
    fn current_marks_the_session_as_attached() {
        let out = parse_zellij_sessions("mine [Created 5s ago] (current)\n");
        assert!(out[0].attached);
    }

    #[test]
    fn the_empty_message_is_not_a_session() {
        // zellij prints this sentence on stdout rather than an empty body.
        assert!(parse_zellij_sessions("No active zellij sessions found.\n").is_empty());
        assert!(parse_zellij_sessions("").is_empty());
        assert!(parse_zellij_sessions("   \n\n").is_empty());
    }

    #[test]
    fn sessions_come_back_newest_first() {
        let out = parse_zellij_sessions(
            "older [Created 2h ago]\nnewest [Created 3s ago]\nmiddle [Created 10m ago]\n",
        );
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["newest", "middle", "older"]);
    }

    #[test]
    fn an_undateable_row_is_still_offered() {
        // A future zellij could reword the bracket. Losing the age is fine;
        // losing a session the user can still attach to is not.
        let out = parse_zellij_sessions("weird-format-session\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "weird-format-session");
    }

    #[test]
    fn durations_add_up_across_units() {
        assert_eq!(parse_zellij_duration("30s"), 30);
        assert_eq!(parse_zellij_duration("12m 30s"), 750);
        assert_eq!(parse_zellij_duration("3h 4m 1s"), 3 * 3600 + 4 * 60 + 1);
        assert_eq!(parse_zellij_duration("2days 1h"), 2 * 86_400 + 3600);
        // Unknown units are skipped, not treated as seconds.
        assert_eq!(parse_zellij_duration("5fortnights 10s"), 10);
        assert_eq!(parse_zellij_duration(""), 0);
    }

    #[test]
    fn ms_does_not_get_read_as_minutes() {
        // "m" is a prefix of "ms"; ordering the match arms wrong would turn
        // 500ms into 500 minutes and date every session eight hours old.
        assert_eq!(parse_zellij_duration("500ms"), 0);
        assert_eq!(parse_zellij_duration("5m"), 300);
    }

    #[test]
    fn zellij_verbs_are_the_ones_0_44_3_documents() {
        // Captured from `zellij 0.44.3 --help` on Windows, 2026-08-19. If a
        // future zellij renames one of these, this test is where it shows up
        // rather than in a silent no-op at runtime.
        assert_eq!(zellij_args_list(), vec!["list-sessions", "-n"]);
        assert_eq!(
            zellij_args_delete_force("ymux-p_1a2b_0"),
            vec!["delete-session", "-f", "ymux-p_1a2b_0"]
        );
    }

    /// THE test on this path. `delete-session [OPTIONS] <TARGET_SESSION>` —
    /// swap the two and clap reads `-f` as the session to destroy, which is a
    /// silent wrong-target destroy, the worst thing a Kill button can do.
    #[test]
    fn the_force_flag_comes_before_the_session_name() {
        let a = zellij_args_delete_force("ymux-p_1a2b_0");
        assert_eq!(a[0], "delete-session");
        assert_eq!(a[1], "-f", "-f must precede the session name");
        assert_eq!(a[2], "ymux-p_1a2b_0");
    }

    /// One click, one verb. The old two-step (`kill-session`, then
    /// `delete-session` only if the kill failed) left a resurrectable corpse
    /// and could not be reached a second time from the UI.
    #[test]
    fn one_click_is_one_verb() {
        let a = zellij_args_delete_force("s");
        assert_eq!(a.len(), 3, "exactly one verb with one flag and one name");
        assert!(
            !a.iter().any(|x| x == "kill-session"),
            "kill-session leaves a resurrectable copy — it is not what Kill means"
        );
    }

    /// "zellij is not installed" and "the verb failed" used to be the same
    /// `false`, which is why a Kill with no zellij present reported success.
    #[test]
    fn a_missing_binary_is_not_a_failed_verb() {
        use std::io::ErrorKind;
        assert_eq!(
            zellij_spawn_error_outcome(ErrorKind::NotFound, "not found"),
            ZellijOutcome::Missing
        );
        assert!(matches!(
            zellij_spawn_error_outcome(ErrorKind::PermissionDenied, "denied"),
            ZellijOutcome::Failed { .. }
        ));
    }

    /// The IPC contract App.tsx and rpc_server.rs both read. Nothing else
    /// pins these field names, and a rename would show up as a false "kill
    /// failed" toast on a kill that worked.
    #[test]
    fn kill_outcome_serializes_the_field_names_the_frontend_reads() {
        let o = KillSessionOutcome::new("killed", "zellij", Some("ymux-p_1".into()));
        let v = serde_json::to_value(&o).expect("serialize");
        assert_eq!(v["result"], "killed");
        assert_eq!(v["backend"], "zellij");
        assert_eq!(v["session"], "ymux-p_1");
        assert!(
            v.get("detail").is_none(),
            "skip_serializing_if must drop absent fields, not send null"
        );
    }

    /// `no_session` means there was nothing to destroy, which satisfies the
    /// caller — so it must not read as a failure and pop an error toast.
    #[test]
    fn nothing_to_destroy_counts_as_gone() {
        assert!(KillSessionOutcome::new("killed", "zellij", None).is_gone());
        assert!(KillSessionOutcome::new("already_gone", "zellij", None).is_gone());
        assert!(KillSessionOutcome::new("no_session", "none", None).is_gone());
        assert!(!KillSessionOutcome::new("failed", "zellij", None).is_gone());
        assert!(!KillSessionOutcome::new("multiplexer_missing", "zellij", None).is_gone());
    }

    #[test]
    fn zellij_write_chars_targets_the_session_before_the_subcommand() {
        // `-s` is a ROOT option in zellij's CLI, so it has to precede `action`.
        // Getting that order wrong is a silent no-op: zellij would look for a
        // session named "action" and the wizard's command would vanish.
        let a = zellij_args_write_chars("ymux-p_1a2b_0", "claude --continue
");
        assert_eq!(
            a,
            vec!["-s", "ymux-p_1a2b_0", "action", "write-chars", "claude --continue
"]
        );
    }

    #[test]
    fn zellij_write_chars_keeps_the_script_in_one_argv_slot() {
        // The script carries spaces, quotes, backslashes and a CRLF. It must
        // reach zellij as ONE argument with no shell in between (Rule #3) — a
        // split here would execute fragments of the user's command.
        let script = "cd \"C:\\Program Files\\x\" && claude --continue\r\n";
        let a = zellij_args_write_chars("s", script);
        assert_eq!(a.len(), 5);
        assert_eq!(a[4], script);
        assert!(a[4].contains(' '), "spaces stay inside the slot");
        assert!(a[4].ends_with("\r\n"), "the newline is what submits it");
    }

    #[test]
    fn zellij_verbs_pass_the_name_as_one_argv_slot() {
        // Rule #3: no shell in between, so a name is never re-parsed. This
        // is the SAFE half of the pair, and the reason the argv shape is
        // pinned: a hostile name is harmless here precisely because
        // nothing re-parses it — which is exactly what
        // `build_zellij_attach_command` cannot rely on, since it types its
        // line into a shell, so it refuses such a name instead. (This
        // comment used to cite `sanitize_session_name`, a function that
        // has never existed in this tree.)
        let args = zellij_args_delete_force("weird name; rm -rf");
        assert_eq!(args.len(), 3);
        assert_eq!(args[2], "weird name; rm -rf");
    }

    #[test]
    fn zellij_sessions_flag_exited_ones() {
        // The EXITED marker is what tells the picker "attaching resurrects
        // this" instead of "this is live". Dropping it is how a dead session
        // became indistinguishable from a running one.
        let out = parse_zellij_sessions(
            "live-one [Created 5s ago] 
dead-one [Created 3h 4m 1s ago] (EXITED - attach to resurrect)
",
        );
        assert_eq!(out.len(), 2);
        let dead = out.iter().find(|s| s.name == "dead-one").expect("dead-one parsed");
        let live = out.iter().find(|s| s.name == "live-one").expect("live-one parsed");
        assert!(dead.exited, "EXITED row must be flagged");
        assert!(!live.exited, "a live row must not be");
        assert!(!dead.attached, "an exited session has nobody attached");
    }

    #[test]
    fn zellij_attach_command_is_a_single_safe_line() {
        let cmd = build_zellij_attach_command("ymux-p_1a2b_0")
            .expect("a pane-id-derived name is always safe");
        assert_eq!(cmd, "zellij attach -c ymux-p_1a2b_0\r\n");
        // One line typed into a shell: a stray newline would run whatever
        // followed it as a second command.
        assert_eq!(cmd.matches('\n').count(), 1);
        assert!(cmd.ends_with("\r\n"));
    }

    // ── The P1 that produced all of the above ───────────────────────────
    //
    // Filed 2026-08-20: a pane title could break its own session name. The
    // test that was supposed to be guarding this line (above) only ever
    // fed it `ymux-p_1a2b_0` — a name that could not have failed — so it
    // passed for as long as the hole existed. These feed it the input that
    // matters.

    #[test]
    fn attach_command_refuses_a_name_it_cannot_safely_type() {
        // Each of these ends the `zellij attach` command and starts another
        // one in at least one of cmd.exe / PowerShell / a POSIX shell.
        for hostile in [
            "work; calc",
            "work && calc",
            "work | calc",
            "work & calc",
            "$(calc)",
            "`calc`",
            "work > out.txt",
            "work\ncalc",
            "work\r\ncalc",
            "a\u{2028}calc",
            "it's",
            "say \"hi\"",
            "50%PATH%",
            "up^caret",
            "",
        ] {
            assert!(
                build_zellij_attach_command(hostile).is_none(),
                "must refuse to type {hostile:?} into a shell"
            );
        }
    }

    #[test]
    fn a_title_cannot_smuggle_a_second_command_through_the_sanitizer() {
        // The real path: title -> sanitizer -> attach line. Whatever the
        // title, the produced name must survive the last gate, and the
        // command must stay ONE line.
        for title in [
            "work; calc",
            "work && calc",
            "$(calc)",
            "`calc`",
            "a|b",
            "a>b",
            "it's mine",
            "say \"hi\"",
            "50%PATH%",
            "tab\there",
            "nl\nhere",
        ] {
            let name = sanitize_tmux_session_name_for_title(title)
                .unwrap_or_else(|| panic!("{title:?} should still yield a name"));
            let cmd = build_zellij_attach_command(&name)
                .unwrap_or_else(|| panic!("{title:?} -> {name:?} was refused"));
            assert_eq!(
                cmd.matches('\n').count(),
                1,
                "{title:?} produced more than one line: {cmd:?}"
            );
            for bad in [';', '&', '|', '$', '`', '>', '<', '^', '%', '\'', '"', '(', ')'] {
                assert!(
                    !name.contains(bad),
                    "{title:?} left {bad:?} in the session name {name:?}"
                );
            }
        }
    }

    #[test]
    fn the_whitelist_still_keeps_phase_23i_promise_about_unicode_titles() {
        // The whole point of the original sanitizer: a Hebrew title becomes
        // a session of the same name. Tightening it for shell safety must
        // not quietly turn that back into underscores.
        assert_eq!(
            sanitize_tmux_session_name_for_title("מחקר X").as_deref(),
            Some("מחקר_X")
        );
        assert_eq!(
            sanitize_tmux_session_name_for_title("作業").as_deref(),
            Some("作業")
        );
        // Niqqud are combining marks, not alphanumerics — the rule is
        // "non-ASCII passes" precisely so they survive rather than being
        // punched out of the middle of a word.
        let pointed = "עבודה\u{05B8}";
        assert_eq!(
            sanitize_tmux_session_name_for_title(pointed).as_deref(),
            Some(pointed)
        );
        assert!(build_zellij_attach_command(pointed).is_some());
    }

    #[test]
    fn sanitizer_keeps_its_old_contract_where_it_was_already_right() {
        // Regressions guarded: tmux's own blockers still go, runs of
        // replacements still collapse to one underscore, the result is
        // still trimmed of leading/trailing underscores, an all-junk title
        // still yields None so the caller falls back to the pane id, and
        // the 100-CHAR (not byte) cap still holds on a Hebrew title.
        assert_eq!(
            sanitize_tmux_session_name_for_title("a.b:c").as_deref(),
            Some("a_b_c")
        );
        assert_eq!(
            sanitize_tmux_session_name_for_title("a   b").as_deref(),
            Some("a_b")
        );
        assert_eq!(
            sanitize_tmux_session_name_for_title("  ...x...  ").as_deref(),
            Some("x")
        );
        assert_eq!(sanitize_tmux_session_name_for_title(""), None);
        assert_eq!(sanitize_tmux_session_name_for_title("   "), None);
        assert_eq!(sanitize_tmux_session_name_for_title(";;;"), None);
        let long = "מחקר".repeat(60);
        let capped = sanitize_tmux_session_name_for_title(&long).expect("non-empty");
        assert_eq!(capped.chars().count(), 100);
    }

    #[test]
    fn safe_char_rule_is_the_one_both_call_sites_share() {
        for c in ['a', 'Z', '7', '_', '-', 'מ', '作'] {
            assert!(session_name_char_is_safe(c), "{c:?} should be allowed");
        }
        for c in [
            ';', '&', '|', '$', '`', '>', '<', '^', '%', '(', ')', '{', '}',
            '\'', '"', ' ', '\t', '\n', '\r', '.', ':', '\u{0085}', '\u{2028}',
            '\u{00A0}', '\u{0000}',
        ] {
            assert!(!session_name_char_is_safe(c), "{c:?} should be rejected");
        }
    }
}

#[cfg(test)]
mod workspaces_schema_tests {
    // The gate that stops an older ymux writing over a newer one's file.
    // Covered because the failure it guards is silent by nature: the bug that
    // produced it (FOLLOWUPS P1, 2026-08-18) erased workspace nesting on every
    // save by a stale build and left nothing in the new build's log.
    use super::{
        schema_gate, schema_version_of, SchemaGate, WorkspacesFile,
        WORKSPACES_SCHEMA_VERSION,
    };

    #[test]
    fn a_save_always_stamps_the_current_schema_version() {
        // The point of `serialize_with`: whatever the struct is CARRYING —
        // a legacy 1, or a 0 from Default — what reaches disk is current.
        for carried in [0, 1, WORKSPACES_SCHEMA_VERSION] {
            let file = WorkspacesFile {
                version: carried,
                ..Default::default()
            };
            let text = serde_json::to_string(&file).expect("serialize");
            assert_eq!(
                schema_version_of(&text),
                Some(WORKSPACES_SCHEMA_VERSION),
                "carrying v{carried} must still write v{}",
                WORKSPACES_SCHEMA_VERSION
            );
        }
    }

    #[test]
    fn a_legacy_file_without_a_version_key_still_loads() {
        // Pre-versioning files exist in the wild; they must not become
        // unreadable, and they must read as v1 rather than as "newer".
        let file: WorkspacesFile =
            serde_json::from_str(r#"{"workspaces":[]}"#).expect("parse");
        assert_eq!(file.version, 1);
        assert!(file.version <= WORKSPACES_SCHEMA_VERSION);
    }

    #[test]
    fn version_reader_is_tolerant_because_a_refusal_hangs_off_it() {
        // `None` means "carry on saving". Anything that is not a usable
        // version must land there rather than tripping the gate: a refusal
        // triggered by a malformed file would lock the user out of saving.
        assert_eq!(schema_version_of(r#"{"version":2}"#), Some(2));
        assert_eq!(schema_version_of(r#"{"version":99}"#), Some(99));
        // A BOM is stripped first — Windows tooling writes them routinely and
        // one already bricked this file once.
        assert_eq!(schema_version_of("\u{feff}{\"version\":2}"), Some(2));
        for shrug in [
            "",
            "   ",
            "not json at all",
            r#"{"workspaces":[]}"#,
            r#"{"version":null}"#,
            r#"{"version":"2"}"#,
            r#"{"version":-1}"#,
            r#"{"version":1.5}"#,
            "[1,2,3]",
        ] {
            assert_eq!(
                schema_version_of(shrug),
                None,
                "{shrug:?} must read as no-usable-version, not as a version"
            );
        }
    }

    #[test]
    fn the_gate_only_refuses_the_direction_that_loses_data() {
        let ours = WORKSPACES_SCHEMA_VERSION;

        // The one hard stop: a newer build owns the file.
        assert_eq!(schema_gate(Some(ours + 1), Some(ours)), SchemaGate::Refuse);
        assert_eq!(schema_gate(Some(ours + 9), None), SchemaGate::Refuse);

        // Ordinary saves.
        assert_eq!(schema_gate(Some(ours), Some(ours)), SchemaGate::Write);

        // An older build rewrote it since we last wrote: warn, never refuse.
        // Refusing here would leave the user unable to save at all for as
        // long as the old build is open, which is worse than letting the
        // merge repair it.
        assert_eq!(
            schema_gate(Some(ours - 1), Some(ours)),
            SchemaGate::WarnDowngrade
        );

        // A legacy file we have not written over yet is NOT a downgrade —
        // there is nothing to have lost.
        assert_eq!(schema_gate(Some(ours - 1), None), SchemaGate::Write);
        assert_eq!(
            schema_gate(Some(ours - 1), Some(ours - 1)),
            SchemaGate::Write
        );
    }

    #[test]
    fn an_unreadable_file_never_blocks_a_save() {
        // `None` is every case the reader shrugged at: absent, empty,
        // malformed, version-less. None of them may become a refusal — a
        // user whose file got a stray byte must still be able to save.
        let ours = WORKSPACES_SCHEMA_VERSION;
        assert_eq!(schema_gate(None, None), SchemaGate::Write);
        assert_eq!(schema_gate(None, Some(ours)), SchemaGate::Write);
        assert_eq!(schema_gate(None, Some(ours + 1)), SchemaGate::Write);
    }
}

#[cfg(test)]
mod wsl_migration_tests {
    // The WSL removal is the part of this change that can cost a user every
    // workspace they have, so it gets covered before it ships. The failure
    // being guarded against is concrete: `Connection` is serde-tagged and
    // lives in workspaces.json, so an unparseable variant fails the WHOLE
    // file and load_from_disk then refuses all later saves.
    use super::{migrate_wsl_workspaces, Connection, LayoutNode, PaneKind, WorkspacesFile};
    use ymux_types::Workspace;

    fn pane(conn: Option<Connection>) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: "p_test".into(),
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

    fn ws(conn: Option<Connection>, layout: Option<LayoutNode>) -> Workspace {
        Workspace {
            id: "w_test".into(),
            name: "test".into(),
            connection: conn,
            layout,
            ..Default::default()
        }
    }

    #[test]
    fn a_wsl_workspace_becomes_local_and_keeps_everything_else() {
        let mut f = WorkspacesFile {
            version: 1,
            workspaces: vec![ws(
                Some(Connection::Wsl { distro: Some("Ubuntu".into()) }),
                Some(pane(None)),
            )],
            ..Default::default()
        };
        assert_eq!(migrate_wsl_workspaces(&mut f), 1);
        assert!(matches!(
            f.workspaces[0].connection,
            Some(Connection::Local { .. })
        ));
        assert_eq!(f.workspaces[0].name, "test", "the workspace itself survives");
        assert!(f.workspaces[0].layout.is_some(), "the layout survives");
    }

    #[test]
    fn a_wsl_connection_on_a_PANE_is_migrated_too() {
        // A pane can carry its own connection that overrides the workspace's.
        // Migrating only the workspace would leave a wsl pane inside a local
        // workspace, with no spawn path left behind it.
        let mut f = WorkspacesFile {
            version: 1,
            workspaces: vec![ws(
                Some(Connection::Local { shell: None }),
                Some(LayoutNode::Split {
                    split_id: "sp_1".into(),
                    direction: ymux_types::SplitDirection::Horizontal,
                    ratio: 0.5,
                    first: Box::new(pane(Some(Connection::Wsl { distro: None }))),
                    second: Box::new(pane(Some(Connection::Local { shell: None }))),
                }),
            )],
            ..Default::default()
        };
        assert_eq!(migrate_wsl_workspaces(&mut f), 1, "the nested pane counts");
    }

    #[test]
    fn ssh_and_local_workspaces_are_left_completely_alone() {
        let mut f = WorkspacesFile {
            version: 1,
            workspaces: vec![
                ws(
                    Some(Connection::Ssh {
                        host: "h".into(),
                        user: "u".into(),
                        port: 22,
                        key_path: None,
                    }),
                    Some(pane(None)),
                ),
                ws(Some(Connection::Local { shell: Some("pwsh.exe".into()) }), None),
            ],
            ..Default::default()
        };
        assert_eq!(migrate_wsl_workspaces(&mut f), 0, "nothing to do → no persist");
        assert!(matches!(f.workspaces[0].connection, Some(Connection::Ssh { .. })));
        // The chosen shell must not be flattened away by a careless rewrite.
        match &f.workspaces[1].connection {
            Some(Connection::Local { shell }) => assert_eq!(shell.as_deref(), Some("pwsh.exe")),
            _ => panic!("the local workspace's connection was altered"),
        }
    }

    #[test]
    fn migration_is_idempotent() {
        let mut f = WorkspacesFile {
            version: 1,
            workspaces: vec![ws(Some(Connection::Wsl { distro: None }), None)],
            ..Default::default()
        };
        assert_eq!(migrate_wsl_workspaces(&mut f), 1);
        assert_eq!(migrate_wsl_workspaces(&mut f), 0, "second pass must be a no-op");
    }

    #[test]
    fn a_workspaces_json_containing_wsl_still_DESERIALISES() {
        // The whole reason the enum variant is kept. If this ever fails, every
        // user with a wsl workspace loses their entire file on upgrade.
        let json = r#"{"version":1,"active_workspace_id":null,"workspaces":[
            {"id":"w1","name":"WSL","connection":{"type":"wsl","distro":"Ubuntu"}}
        ]}"#;
        let mut f: WorkspacesFile =
            serde_json::from_str(json).expect("a wsl connection must still parse");
        assert_eq!(f.workspaces.len(), 1);
        assert_eq!(migrate_wsl_workspaces(&mut f), 1);
    }
}
