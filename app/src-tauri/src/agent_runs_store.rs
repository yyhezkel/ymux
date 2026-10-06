//! Persistence for `AppState.agent_runs` (the per-pane traffic-light state).
//!
//! Saved on `RunEvent::Exit`, restored in `setup` after the workspaces
//! load. `<config>/agent-runs.json`, separate from `workspaces.json` so
//! `WORKSPACES_SCHEMA_VERSION` never moves. `apply_hook` is untouched: a
//! restored `Running` is corrected by the next hook like a live one.
//!
//! Staleness is enforced in `restore` only, and every load path goes
//! through it. Logs carry counts only (CLAUDE.md Rule #1).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{AgentRunState, AppState, PaneAgentState};
use ymux_core::{config_dir, log_debug, log_info, log_warn};

const TAG: &str = "AGENT";
const FILE_VERSION: u32 = 1;

/// Entries this old or older are dropped on restore. Mirrors
/// `STALE_AFTER_MS` in app/src/paneAgentState.ts.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct AgentRunsFile {
    pub(crate) version: u32,
    pub(crate) runs: HashMap<String, PersistedRun>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedRun {
    state: PaneAgentState,
    // Optional so a hand-edited or truncated record is dropped by
    // `restore` instead of failing the whole file.
    #[serde(default)]
    state_since_ms: Option<u64>,
    #[serde(default)]
    turn_started_at_ms: Option<u64>,
    sum_ms: u64,
    count: u32,
    seq: u32,
}

fn to_ms(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn from_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

/// Persistable view of the live map. Skips `Unknown` and entries with no
/// `state_since` — neither can render a light after a restart.
pub(crate) fn snapshot(runs: &HashMap<String, AgentRunState>) -> AgentRunsFile {
    let mut out = HashMap::new();
    for (id, r) in runs {
        if r.state == PaneAgentState::Unknown {
            continue;
        }
        let Some(since) = r.state_since.and_then(to_ms) else {
            continue;
        };
        out.insert(
            id.clone(),
            PersistedRun {
                state: r.state,
                state_since_ms: Some(since),
                turn_started_at_ms: r.turn_started_at.and_then(to_ms),
                sum_ms: u64::try_from(r.sum_ms).unwrap_or(u64::MAX),
                count: r.count,
                seq: r.seq,
            },
        );
    }
    AgentRunsFile {
        version: FILE_VERSION,
        runs: out,
    }
}

/// Pure restore: drops Unknown, timestamp-less, `>= STALE_AFTER` old and
/// unknown-pane entries. A future stamp (clock moved back) counts as age 0.
pub(crate) fn restore(
    file: AgentRunsFile,
    now: SystemTime,
    known_pane: impl Fn(&str) -> bool,
) -> HashMap<String, AgentRunState> {
    let (mut kept, mut stale, mut unknown_pane) = (0usize, 0usize, 0usize);
    let mut out = HashMap::new();
    for (id, p) in file.runs {
        let Some(since_ms) = p.state_since_ms else {
            stale += 1;
            continue;
        };
        if p.state == PaneAgentState::Unknown {
            stale += 1;
            continue;
        }
        let since = from_ms(since_ms);
        let age = now.duration_since(since).unwrap_or(Duration::ZERO);
        if age >= STALE_AFTER {
            stale += 1;
            continue;
        }
        if !known_pane(&id) {
            unknown_pane += 1;
            continue;
        }
        kept += 1;
        out.insert(
            id,
            AgentRunState {
                turn_started_at: p.turn_started_at_ms.map(from_ms),
                sum_ms: u128::from(p.sum_ms),
                count: p.count,
                state: p.state,
                state_since: Some(since),
                seq: p.seq,
            },
        );
    }
    log_info(
        TAG,
        &format!("agent-runs restore: kept={kept} dropped_stale={stale} dropped_unknown_pane={unknown_pane}"),
    );
    out
}

/// Missing, unreadable, corrupt or wrong-version file → empty. Never Err.
pub(crate) fn load(path: &Path) -> AgentRunsFile {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log_debug(TAG, "agent-runs: no file");
            return AgentRunsFile::default();
        }
        Err(e) => {
            log_warn(TAG, &format!("agent-runs: read failed: {e}"));
            return AgentRunsFile::default();
        }
    };
    match serde_json::from_str::<AgentRunsFile>(&text) {
        Ok(f) if f.version == FILE_VERSION => f,
        Ok(f) => {
            log_warn(TAG, &format!("agent-runs: unknown version {}", f.version));
            AgentRunsFile::default()
        }
        Err(e) => {
            log_warn(TAG, &format!("agent-runs: parse failed: {e}"));
            AgentRunsFile::default()
        }
    }
}

/// Rule #7: tmp, fsync, rename.
pub(crate) fn save(path: &Path, file: &AgentRunsFile) -> Result<(), String> {
    use std::io::Write as _;
    let dir = path.parent().ok_or_else(|| "no parent dir".to_string())?;
    let tmp = dir.join(format!("agent-runs.{}.tmp", std::process::id()));
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
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))
}

/// Setup glue: call after the workspaces are loaded.
pub(crate) fn restore_into(state: &AppState) {
    let Ok(dir) = config_dir() else { return };
    let file = load(&dir.join("agent-runs.json"));
    if file.runs.is_empty() {
        return;
    }
    let restored = {
        let Ok(ws) = state.workspaces.lock() else {
            log_warn(TAG, "agent-runs: workspaces lock poisoned, skipping restore");
            return;
        };
        restore(file, SystemTime::now(), |id| {
            crate::find_workspace_for_pane(&ws, id).is_some()
        })
    };
    match state.agent_runs.lock() {
        Ok(mut runs) => runs.extend(restored),
        Err(e) => log_warn(TAG, &format!("agent-runs: lock poisoned: {e}")),
    }
}

/// Exit glue: failure only logs, never panics.
pub(crate) fn save_from(state: &AppState) {
    let file = match state.agent_runs.lock() {
        Ok(runs) => snapshot(&runs),
        Err(e) => {
            log_warn(TAG, &format!("agent-runs: lock poisoned, not saved: {e}"));
            return;
        }
    };
    let path = match config_dir() {
        Ok(d) => d.join("agent-runs.json"),
        Err(e) => {
            log_warn(TAG, &format!("agent-runs: no config dir: {e}"));
            return;
        }
    };
    if let Err(e) = save(&path, &file) {
        log_warn(TAG, &format!("agent-runs: save failed: {e}"));
    }
}

#[cfg(test)]
mod agent_runs_store_tests {
    use super::*;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(10_000_000)
    }

    fn run(state: PaneAgentState, since: Option<SystemTime>) -> AgentRunState {
        AgentRunState {
            turn_started_at: Some(now() - Duration::from_secs(5)),
            sum_ms: 90_000,
            count: 3,
            state,
            state_since: since,
            seq: 7,
        }
    }

    fn restore_one(state: PaneAgentState, age: Duration) -> HashMap<String, AgentRunState> {
        let mut m = HashMap::new();
        m.insert("p1".to_string(), run(state, Some(now() - age)));
        restore(snapshot(&m), now(), |_| true)
    }

    #[test]
    fn snapshot_skips_unknown_and_entries_without_a_timestamp() {
        // Neither can render a light after restart; persisting them is noise.
        let mut m = HashMap::new();
        m.insert("a".to_string(), run(PaneAgentState::Unknown, Some(now())));
        m.insert("b".to_string(), run(PaneAgentState::Done, None));
        m.insert("c".to_string(), run(PaneAgentState::Done, Some(now())));
        let f = snapshot(&m);
        assert_eq!(f.runs.len(), 1);
        assert!(f.runs.contains_key("c"));
    }

    #[test]
    fn round_trip_preserves_every_field() {
        // A lossy field would show a different light/timer after restart.
        let since = now() - Duration::from_secs(60);
        let mut m = HashMap::new();
        m.insert("p1".to_string(), run(PaneAgentState::NeedsInput, Some(since)));
        let json = serde_json::to_string(&snapshot(&m)).unwrap();
        let back: AgentRunsFile = serde_json::from_str(&json).unwrap();
        let r = &restore(back, now(), |_| true)["p1"];
        assert_eq!(r.state, PaneAgentState::NeedsInput);
        assert_eq!(r.state_since, Some(since));
        assert_eq!(r.turn_started_at, Some(now() - Duration::from_secs(5)));
        assert_eq!((r.sum_ms, r.count, r.seq), (90_000, 3, 7));
    }

    #[test]
    fn state_serialises_as_kebab_case() {
        // File contract: running|done|needs-input, same as as_str().
        let mut m = HashMap::new();
        m.insert("p1".to_string(), run(PaneAgentState::NeedsInput, Some(now())));
        let json = serde_json::to_string(&snapshot(&m)).unwrap();
        assert!(json.contains("\"needs-input\""), "{json}");
    }

    #[test]
    fn restore_discards_entries_six_hours_or_older() {
        // Exactly STALE_AFTER is stale (>=), matching the frontend cutoff.
        assert!(restore_one(PaneAgentState::Done, STALE_AFTER).is_empty());
        assert!(restore_one(PaneAgentState::Done, STALE_AFTER + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn restore_keeps_entries_under_six_hours() {
        let r = restore_one(PaneAgentState::Done, STALE_AFTER - Duration::from_secs(1));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn restore_discards_entries_without_a_timestamp() {
        // Failure path: a hand-edited record must drop, not fail the file.
        let json = r#"{"version":1,"runs":{
            "p1":{"state":"done","sum_ms":0,"count":0,"seq":1},
            "p2":{"state":"done","state_since_ms":null,"sum_ms":0,"count":0,"seq":1}}}"#;
        let f: AgentRunsFile = serde_json::from_str(json).unwrap();
        assert!(restore(f, now(), |_| true).is_empty());
    }

    #[test]
    fn restore_keeps_a_future_timestamp() {
        // Clock moved back: age 0, same as the frontend treats it.
        let mut m = HashMap::new();
        m.insert("p1".to_string(), run(PaneAgentState::Done, Some(now() + Duration::from_secs(3600))));
        assert_eq!(restore(snapshot(&m), now(), |_| true).len(), 1);
    }

    #[test]
    fn restore_discards_panes_that_no_longer_exist() {
        let mut m = HashMap::new();
        m.insert("gone".to_string(), run(PaneAgentState::Done, Some(now())));
        assert!(restore(snapshot(&m), now(), |_| false).is_empty());
    }

    #[test]
    fn load_returns_empty_for_missing_corrupt_and_wrong_version() {
        // Failure path: a bad file must never block startup.
        let dir = std::env::temp_dir().join(format!("ymux-agent-runs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load(&dir.join("missing.json")).runs.is_empty());
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(load(&bad).runs.is_empty());
        let ver = dir.join("ver.json");
        std::fs::write(
            &ver,
            r#"{"version":99,"runs":{"p":{"state":"done","state_since_ms":1,"sum_ms":0,"count":0,"seq":0}}}"#,
        )
        .unwrap();
        assert!(load(&ver).runs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_round_trips_on_disk() {
        // Pins the atomic write path end to end.
        let dir = std::env::temp_dir().join(format!("ymux-agent-runs-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-runs.json");
        let mut m = HashMap::new();
        m.insert("p1".to_string(), run(PaneAgentState::Running, Some(now())));
        save(&path, &snapshot(&m)).unwrap();
        assert_eq!(load(&path).runs.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_running_state_is_corrected_by_the_next_hook() {
        // Restore must not freeze a pane on yellow: the next hook wins.
        let mut r = restore_one(PaneAgentState::Running, Duration::from_secs(10))
            .remove("p1")
            .unwrap();
        assert_eq!(r.seq, 7);
        assert!(r.apply_hook("stop", None));
        assert_eq!(r.state, PaneAgentState::Done);
        assert_eq!(r.seq, 8);
    }
}
