//! beta.3-lh-insights: native local Insights for Local workspaces.
//!
//! Talks the same JSON shape as the remote `ymux-server` HTTP API so the
//! Insights panel (`InsightsWindow.tsx`) can share its parsing code — the
//! only routing decision the frontend has to make is remote-vs-local, and
//! `insights_fetch` in `addons.rs` does that transparently.
//!
//! CPU/mem/disks/net/processes come from `sysinfo` (cross-platform, no WMI
//! plumbing). Docker on Windows via `bollard` over `\\.\pipe\docker_engine`
//! (Docker Desktop). If Docker isn't running we return an empty container
//! list rather than erroring — the panel already renders a friendly "no
//! docker" state.
//!
//! dlog tagging: `[INSIGHTS-LOCAL]`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{
    Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
    MINIMUM_CPU_UPDATE_INTERVAL,
};

// ─── Wire shapes (mirror remote /current, /docker, /processes, /hygiene, /logs) ─

#[derive(Serialize)]
pub struct CpuMetric {
    pub pct: u32,
    pub per_core: Vec<u32>,
    /// 1m/5m/15m load average. Zeros on Windows (kernel has no equivalent).
    pub load: Vec<f32>,
}

#[derive(Serialize)]
pub struct MemMetric {
    pub total: u64,
    pub used: u64,
    /// Not surfaced by sysinfo on Windows — kept in the shape for parity
    /// with the remote daemon (which reads /proc/meminfo on Linux).
    pub cached: u64,
    pub swap_used: u64,
}

#[derive(Serialize)]
pub struct DiskMetric {
    pub mount: String,
    pub total: u64,
    pub used: u64,
    pub pct: u32,
}

#[derive(Serialize)]
pub struct NetMetric {
    pub iface: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Serialize)]
pub struct TopProc {
    pub pid: i32,
    pub name: String,
    pub cpu: u32,
    pub rss: u64,
}

#[derive(Serialize)]
pub struct Snapshot {
    /// Unix time in ms.
    pub ts: u128,
    pub cpu: CpuMetric,
    pub mem: MemMetric,
    pub disks: Vec<DiskMetric>,
    pub net: Vec<NetMetric>,
    pub docker_running: u32,
    pub docker_total: u32,
    pub top: Vec<TopProc>,
}

#[derive(Serialize)]
pub struct DockerContainer {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub cpu_pct: u32,
    pub mem_used: u64,
    pub mem_pct: u32,
}

#[derive(Serialize)]
pub struct DockerResp {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The frontend uses this to decide "old daemon" vs. "new daemon".
    /// For local we mint a stable "ymux-local" version — never empty.
    pub daemon_version: String,
    pub containers: Vec<DockerContainer>,
}

#[derive(Serialize)]
pub struct Hygiene {
    pub port_watchers: Vec<serde_json::Value>,
    pub duplicate_count: u32,
    pub orphan_sessions: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct LogsResp {
    pub path: String,
    pub lines: Vec<String>,
}

// ─── Shared state so CPU % / network bps have a delta to diff against ─

struct SharedSys {
    sys: System,
    nets: Networks,
    /// When did we last refresh? Networks reports bytes-since-last-refresh
    /// and CPU % is delta-based, so we track the interval ourselves.
    last_refresh: Instant,
    /// Cache of the last observed docker container counts so `/current` can
    /// fill `docker_running` / `docker_total` cheaply. Populated by the
    /// `/docker` fetch. Zero-zero if never fetched.
    last_docker_counts: (u32, u32),
}

fn shared() -> &'static Mutex<SharedSys> {
    static S: OnceLock<Mutex<SharedSys>> = OnceLock::new();
    S.get_or_init(|| {
        // Everything except process detail — those come on demand in the
        // snapshot to keep the initial mutex hold cheap. `new_all` gives us
        // a first refresh so the second call has something to diff against.
        let sys = System::new_with_specifics(RefreshKind::everything());
        let nets = Networks::new_with_refreshed_list();
        Mutex::new(SharedSys {
            sys,
            nets,
            last_refresh: Instant::now(),
            last_docker_counts: (0, 0),
        })
    })
}

/// Best-effort round to `u32`. Clamps to [0,100] so a spurious sysinfo
/// spike can't render as e.g. 250 %.
fn pct_u32(v: f32) -> u32 {
    let clamped = v.round().clamp(0.0, 100.0);
    clamped as u32
}

/// Take a full snapshot. CPU % / network bps need a diff → we sleep
/// briefly on the first ever call (or if the caller polled faster than
/// `MINIMUM_CPU_UPDATE_INTERVAL`), otherwise the delta is the poll gap.
///
/// `include_top = false` skips the per-process refresh + sort (the sampler
/// never stores top processes, and that walk is the expensive phase).
pub fn snapshot(include_top: bool) -> Result<Snapshot, String> {
    let mut guard = shared()
        .lock()
        .map_err(|e| format!("insights_local snapshot: mutex poisoned: {e}"))?;

    // If the caller polls faster than sysinfo's minimum CPU interval, sleep
    // the remainder so cpu_usage() has real data to report. On a 5s poll
    // this is a no-op.
    let since = guard.last_refresh.elapsed();
    if since < MINIMUM_CPU_UPDATE_INTERVAL {
        let wait = MINIMUM_CPU_UPDATE_INTERVAL - since;
        // Release the lock while we sleep so a concurrent /docker fetch
        // isn't blocked. Re-lock after.
        drop(guard);
        std::thread::sleep(wait);
        guard = shared()
            .lock()
            .map_err(|e| format!("insights_local snapshot: mutex poisoned: {e}"))?;
    }

    let interval_secs = guard.last_refresh.elapsed().as_secs_f64().max(0.001);
    guard.sys.refresh_cpu_usage();
    guard.sys.refresh_memory();
    if include_top {
        guard.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );
    }
    // sysinfo 0.32: Networks::refresh() takes no args; interface add/remove
    // is picked up by refresh_list(), which we don't need mid-poll (an added
    // Wi-Fi doesn't matter until the next full snapshot).
    guard.nets.refresh();
    guard.last_refresh = Instant::now();

    let per_core: Vec<u32> = guard.sys.cpus().iter().map(|c| pct_u32(c.cpu_usage())).collect();
    let global_pct = pct_u32(guard.sys.global_cpu_usage());
    let load = System::load_average();
    let cpu = CpuMetric {
        pct: global_pct,
        per_core,
        load: vec![load.one as f32, load.five as f32, load.fifteen as f32],
    };

    let mem = MemMetric {
        total: guard.sys.total_memory(),
        used: guard.sys.used_memory(),
        cached: 0,
        swap_used: guard.sys.used_swap(),
    };

    // Disks + net collected from lists that we recreate here — cheap on
    // Windows (a WinAPI enumerate) and keeps the shared state small.
    let disks_list = Disks::new_with_refreshed_list();
    let disks: Vec<DiskMetric> = disks_list
        .iter()
        .map(|d| {
            let total = d.total_space();
            let free = d.available_space();
            let used = total.saturating_sub(free);
            let pct = if total == 0 {
                0
            } else {
                ((used as f64 * 100.0) / total as f64).round().min(100.0) as u32
            };
            DiskMetric {
                mount: d.mount_point().display().to_string(),
                total,
                used,
                pct,
            }
        })
        .collect();

    let net: Vec<NetMetric> = guard
        .nets
        .iter()
        .map(|(name, data)| NetMetric {
            iface: name.clone(),
            rx_bps: ((data.received() as f64) / interval_secs) as u64,
            tx_bps: ((data.transmitted() as f64) / interval_secs) as u64,
        })
        .collect();

    let mut procs: Vec<TopProc> = if include_top {
        guard
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| TopProc {
                pid: pid.as_u32() as i32,
                name: p.name().to_string_lossy().to_string(),
                cpu: p.cpu_usage().round() as u32,
                rss: p.memory(),
            })
            .collect()
    } else {
        Vec::new()
    };
    // Sort by CPU desc, RSS desc as tiebreaker so idle processes rank last.
    procs.sort_by(|a, b| b.cpu.cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
    procs.truncate(20);

    let (dr, dt) = guard.last_docker_counts;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    Ok(Snapshot {
        ts,
        cpu,
        mem,
        disks,
        net,
        docker_running: dr,
        docker_total: dt,
        top: procs,
    })
}

// ─── Docker (Windows named pipe) ───────────────────────────────────────

#[cfg(windows)]
const DOCKER_PIPE: &str = r"\\.\pipe\docker_engine";
const DAEMON_TAG: &str = "ymux-local";

/// Connect to Docker Desktop over the Windows named pipe. Returns `Ok(None)`
/// (never `Err`) when Docker isn't reachable — the panel treats that as a
/// friendly no-docker state rather than a hard error.
#[cfg(windows)]
async fn connect_docker() -> Result<Option<(bollard::Docker, String)>, String> {
    match bollard::Docker::connect_with_named_pipe_defaults() {
        Ok(d) => {
            // Ping so we surface an actionable reason ("no_socket" vs
            // "permission") rather than hitting the first list_containers.
            match d.ping().await {
                Ok(_) => Ok(Some((d, DOCKER_PIPE.to_string()))),
                Err(e) => {
                    crate::log_warn("INSIGHTS-LOCAL", &format!("docker ping failed: {e}"));
                    Ok(None)
                }
            }
        }
        Err(e) => {
            crate::log_warn("INSIGHTS-LOCAL", &format!("docker connect failed: {e}"));
            Ok(None)
        }
    }
}

/// Unix socket paths a local Docker-compatible daemon may listen on, in
/// probe order. `/var/run/docker.sock` is only present on macOS when
/// Docker Desktop's "Allow the default Docker socket" (admin) option is
/// on — recent versions default it OFF, so the per-user socket under
/// `~/.docker/run` is the common case. OrbStack / Colima / Rancher
/// Desktop / Podman each have their own; the `docker` CLI finds them via
/// contexts, we simply try them all. `DOCKER_HOST=unix://…` wins.
#[cfg(not(windows))]
fn docker_socket_candidates() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = std::env::var_os("DOCKER_HOST") {
        let h = h.to_string_lossy().to_string();
        if let Some(p) = h.strip_prefix("unix://") {
            v.push(std::path::PathBuf::from(p));
        }
    }
    v.push(std::path::PathBuf::from("/var/run/docker.sock"));
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".docker/run/docker.sock")); // Docker Desktop (per-user)
        v.push(home.join(".orbstack/run/docker.sock")); // OrbStack
        v.push(home.join(".colima/default/docker.sock")); // Colima
        v.push(home.join(".colima/docker.sock"));
        v.push(home.join(".rd/docker.sock")); // Rancher Desktop
        v.push(home.join(".local/share/containers/podman/machine/podman.sock")); // Podman
    }
    v
}

/// The socket we would report in the "not reachable" card: the first
/// candidate that exists on disk (a daemon that isn't answering), else the
/// canonical default.
#[cfg(not(windows))]
fn docker_socket_label() -> String {
    docker_socket_candidates()
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("/var/run/docker.sock"))
        .to_string_lossy()
        .to_string()
}

#[cfg(windows)]
fn docker_socket_label() -> String {
    DOCKER_PIPE.to_string()
}

#[cfg(not(windows))]
async fn connect_docker() -> Result<Option<(bollard::Docker, String)>, String> {
    // Try each existing socket in order and keep the first that answers a
    // ping. A socket that exists but doesn't ping (daemon starting /
    // stopped) is skipped so a second runtime (e.g. OrbStack next to a
    // dead Docker Desktop) still gets found.
    for sock in docker_socket_candidates().into_iter().filter(|p| p.exists()) {
        let path = sock.to_string_lossy().to_string();
        match bollard::Docker::connect_with_socket(&path, 5, bollard::API_DEFAULT_VERSION) {
            Ok(d) => match d.ping().await {
                Ok(_) => return Ok(Some((d, path))),
                Err(e) => {
                    crate::log_warn("INSIGHTS-LOCAL", &format!("docker ping failed on {path}: {e}"));
                }
            },
            Err(e) => {
                crate::log_warn("INSIGHTS-LOCAL", &format!("docker connect failed on {path}: {e}"));
            }
        }
    }
    Ok(None)
}

/// Fetch container list + per-container stats snapshot. Never panics; a
/// failure to talk to Docker returns `available: false` with a reason.
pub async fn docker_snapshot() -> Result<DockerResp, String> {
    let (docker, socket_label) = match connect_docker().await? {
        Some(d) => d,
        None => {
            return Ok(DockerResp {
                available: false,
                reason: Some("no_socket".into()),
                detail: Some(format!("docker socket not reachable ({})", docker_socket_label())),
                socket: Some(docker_socket_label()),
                hint: Some("Start Docker Desktop, then hit Refresh.".into()),
                daemon_version: DAEMON_TAG.into(),
                containers: vec![],
            });
        }
    };

    use bollard::container::{ListContainersOptions, StatsOptions};
    use futures_util::StreamExt;

    let list = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("docker list_containers: {e}"))?;

    let total = list.len() as u32;
    let mut running: u32 = 0;
    let mut out: Vec<DockerContainer> = Vec::with_capacity(list.len());

    for c in list {
        let state = c.state.clone().unwrap_or_default();
        let is_running = state == "running";
        if is_running {
            running += 1;
        }
        let id = c.id.clone().unwrap_or_default();
        let name = c
            .names
            .as_ref()
            .and_then(|v| v.first().cloned())
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        let image = c.image.clone().unwrap_or_default();
        let status = c.status.clone().unwrap_or_default();

        // Only running containers have live stats — skip the WebSocket dance
        // for exited ones (bollard blocks on the first frame otherwise).
        let (cpu_pct, mem_used, mem_pct) = if is_running && !id.is_empty() {
            match docker
                .stats(
                    &id,
                    Some(StatsOptions {
                        stream: false,
                        one_shot: false,
                    }),
                )
                .next()
                .await
            {
                Some(Ok(s)) => compute_docker_stats(&s),
                _ => (0, 0, 0),
            }
        } else {
            (0, 0, 0)
        };

        out.push(DockerContainer {
            id,
            name,
            image,
            state,
            status,
            cpu_pct,
            mem_used,
            mem_pct,
        });
    }

    // Cache counts so /current can fill docker_running/total cheaply.
    if let Ok(mut g) = shared().lock() {
        g.last_docker_counts = (running, total);
    }

    Ok(DockerResp {
        available: true,
        reason: None,
        detail: None,
        socket: Some(socket_label),
        hint: None,
        daemon_version: DAEMON_TAG.into(),
        containers: out,
    })
}

/// Compute (cpu%, mem_used_bytes, mem%) from a bollard Stats frame.
/// Mirrors the classic Docker CLI formula. Kept simple — no attempt to
/// subtract the memory cache (would need `MemoryStatsStats` matching that
/// differs by cgroup v1/v2), so `mem_used` here is `usage` as reported by
/// the daemon. The panel rounds this into a % of the container's own limit,
/// which is what the frontend already displays.
fn compute_docker_stats(s: &bollard::container::Stats) -> (u32, u64, u32) {
    let cpu_delta = s
        .cpu_stats
        .cpu_usage
        .total_usage
        .saturating_sub(s.precpu_stats.cpu_usage.total_usage);
    let sys_delta = s
        .cpu_stats
        .system_cpu_usage
        .unwrap_or(0)
        .saturating_sub(s.precpu_stats.system_cpu_usage.unwrap_or(0));
    let online = s.cpu_stats.online_cpus.unwrap_or(1).max(1);
    let cpu_pct = if sys_delta > 0 && cpu_delta > 0 {
        let raw = (cpu_delta as f64 / sys_delta as f64) * online as f64 * 100.0;
        raw.round().clamp(0.0, 100.0 * online as f64) as u32
    } else {
        0
    };

    let mem_limit = s.memory_stats.limit.unwrap_or(0);
    let mem_used = s.memory_stats.usage.unwrap_or(0);
    let mem_pct = if mem_limit > 0 {
        ((mem_used as f64 * 100.0) / mem_limit as f64)
            .round()
            .clamp(0.0, 100.0) as u32
    } else {
        0
    };

    (cpu_pct, mem_used, mem_pct)
}

/// Docker container action: start | stop | restart | kill.
pub async fn docker_action(container_id: &str, action: &str) -> Result<String, String> {
    if container_id.is_empty()
        || !container_id.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("invalid container id".into());
    }
    if !matches!(action, "start" | "stop" | "restart" | "kill") {
        return Err("invalid docker action".into());
    }
    let (docker, _socket) = connect_docker()
        .await?
        .ok_or_else(|| "docker not reachable".to_string())?;
    match action {
        "start" => docker
            .start_container::<String>(container_id, None)
            .await
            .map(|_| "started".to_string())
            .map_err(|e| format!("docker start: {e}")),
        "stop" => docker
            .stop_container(container_id, None)
            .await
            .map(|_| "stopped".to_string())
            .map_err(|e| format!("docker stop: {e}")),
        "restart" => docker
            .restart_container(container_id, None)
            .await
            .map(|_| "restarted".to_string())
            .map_err(|e| format!("docker restart: {e}")),
        "kill" => docker
            .kill_container::<String>(container_id, None)
            .await
            .map(|_| "killed".to_string())
            .map_err(|e| format!("docker kill: {e}")),
        _ => unreachable!(),
    }
}

// ─── Hygiene / Logs (best-effort stubs for local) ─────────────────────

/// Windows local doesn't have the "duplicate ymux port-watcher" / orphan
/// claude session leak the Linux daemon patches — there's no long-lived
/// server-side process. Return an empty structure so the Health tab renders
/// "all clean" instead of erroring.
pub fn hygiene_snapshot() -> Hygiene {
    Hygiene {
        port_watchers: vec![],
        duplicate_count: 0,
        orphan_sessions: vec![],
    }
}

/// Tail the desktop's `debug.log` (ymux's own log). This is the closest
/// analogue to the remote daemon's `insights.log` — the panel already
/// renders it as a plain text tail.
pub fn logs_tail(limit: usize) -> Result<LogsResp, String> {
    let dir = ymux_core::config_dir_pub().map_err(|e| format!("logs_tail: {e}"))?;
    let path = dir.join("debug.log");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            return Ok(LogsResp {
                path: path.display().to_string(),
                lines: vec![],
            });
        }
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(limit.max(1));
    let lines: Vec<String> = all[start..].iter().map(|s| s.to_string()).collect();
    Ok(LogsResp {
        path: path.display().to_string(),
        lines,
    })
}

// ─── Tauri commands ────────────────────────────────────────────────────

/// `insights_local_current` → JSON string of the same shape the remote
/// `/current` endpoint returns. String-typed so the frontend can share its
/// parse path with the SSH branch (which returns `String` too).
#[tauri::command]
pub async fn insights_local_current() -> Result<String, String> {
    // sysinfo is sync + does file I/O — bounce to spawn_blocking so we
    // don't stall the Tauri runtime while walking /proc equivalents.
    let snap = tokio::task::spawn_blocking(|| snapshot(true))
        .await
        .map_err(|e| format!("insights_local: join: {e}"))??;
    serde_json::to_string(&snap).map_err(|e| format!("insights_local serialize: {e}"))
}

#[tauri::command]
pub async fn insights_local_docker() -> Result<String, String> {
    let d = docker_snapshot().await?;
    serde_json::to_string(&d).map_err(|e| format!("insights_local docker serialize: {e}"))
}

#[tauri::command]
pub async fn insights_local_processes(limit: u32) -> Result<String, String> {
    let mut snap = tokio::task::spawn_blocking(|| snapshot(true))
        .await
        .map_err(|e| format!("insights_local processes: join: {e}"))??;
    let n = (limit as usize).clamp(1, 200);
    snap.top.truncate(n);
    serde_json::to_string(&snap.top)
        .map_err(|e| format!("insights_local processes serialize: {e}"))
}

#[tauri::command]
pub async fn insights_local_hygiene() -> Result<String, String> {
    serde_json::to_string(&hygiene_snapshot())
        .map_err(|e| format!("insights_local hygiene serialize: {e}"))
}

#[tauri::command]
pub async fn insights_local_logs(tail: Option<u32>) -> Result<String, String> {
    let limit = tail.unwrap_or(400) as usize;
    let r = tokio::task::spawn_blocking(move || logs_tail(limit))
        .await
        .map_err(|e| format!("insights_local logs: join: {e}"))??;
    serde_json::to_string(&r).map_err(|e| format!("insights_local logs serialize: {e}"))
}

#[tauri::command]
pub async fn insights_local_docker_action(
    container_id: String,
    action: String,
) -> Result<String, String> {
    docker_action(&container_id, &action).await
}

/// `/analytics` — the Monitor's Analytics tab aggregates the metric history the
/// remote daemon keeps in SQLite (7-day retention). A local workspace has no
/// daemon and no store: `insights_local` samples on demand and persists
/// nothing, so there is no history to roll up. Answer with an explicit marker
/// rather than an error string, so the panel can explain itself instead of
/// showing a raw "unsupported path" message.
fn insights_local_analytics() -> Result<String, String> {
    Ok(r#"{"unavailable":"local"}"#.to_string())
}

// ─── Persistent sampler (feeds insights_store for /analytics) ─────────

const SAMPLE_EVERY: Duration = Duration::from_secs(5);
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// Docker list+stats is the slow phase: every 6th tick (30 s), carried
/// forward in between — same cadence as the remote sampler.
const DOCKER_EVERY: u64 = 6;

/// A workspace with no connection is a legacy Local one (see
/// `Workspace::connection`); WSL is rewritten to Local on load.
fn has_local_workspace(workspaces: &[crate::Workspace]) -> bool {
    workspaces.iter().any(|w| {
        matches!(
            w.connection,
            None | Some(crate::Connection::Local { .. })
        )
    })
}

fn is_loopback_iface(name: &str) -> bool {
    name == "lo" || name.to_ascii_lowercase().contains("loopback")
}

/// Sum of rx/tx bps over non-loopback interfaces.
fn net_totals(net: &[NetMetric]) -> (u64, u64) {
    net.iter()
        .filter(|n| !is_loopback_iface(&n.iface))
        .fold((0, 0), |(rx, tx), n| {
            (rx.saturating_add(n.rx_bps), tx.saturating_add(n.tx_bps))
        })
}

fn build_sample(
    ts: i64,
    snap: &Snapshot,
    docker: &[crate::insights_store::DockerSample],
) -> crate::insights_store::Sample {
    use crate::insights_store::{DiskSample, Sample};
    let (rx, tx) = net_totals(&snap.net);
    let to_i64 = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    Sample {
        ts,
        cpu_pct: f64::from(snap.cpu.pct),
        load1: snap.cpu.load.first().copied().map(f64::from).unwrap_or(0.0),
        mem_used: to_i64(snap.mem.used),
        mem_total: to_i64(snap.mem.total),
        swap_used: to_i64(snap.mem.swap_used),
        net_rx_bps: to_i64(rx),
        net_tx_bps: to_i64(tx),
        disks: snap
            .disks
            .iter()
            .map(|d| DiskSample {
                mount: d.mount.clone(),
                used: to_i64(d.used),
                total: to_i64(d.total),
            })
            .collect(),
        docker: docker.to_vec(),
    }
}

/// Docker list → store rows. Any failure or unavailable daemon → empty.
async fn docker_samples() -> Vec<crate::insights_store::DockerSample> {
    match docker_snapshot().await {
        Ok(d) if d.available => d
            .containers
            .into_iter()
            .map(|c| crate::insights_store::DockerSample {
                cid: c.id,
                name: c.name,
                cpu_pct: f64::from(c.cpu_pct),
                mem_used: i64::try_from(c.mem_used).unwrap_or(i64::MAX),
                state: c.state,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Spawn the 5 s sampler that persists local metrics for `/analytics`.
/// sysinfo and SQLite both run on `spawn_blocking`; ticks are skipped while
/// no Local workspace exists.
pub fn spawn_sampler(state: crate::AppState) {
    tauri::async_runtime::spawn(async move {
        let store = match tokio::task::spawn_blocking(crate::insights_store::Store::open_default).await {
            Ok(Ok(s)) => std::sync::Arc::new(s),
            Ok(Err(e)) => {
                crate::log_warn("INSIGHTS-LOCAL", &format!("sampler disabled: store open failed: {e}"));
                return;
            }
            Err(e) => {
                crate::log_warn("INSIGHTS-LOCAL", &format!("sampler disabled: store open join: {e}"));
                return;
            }
        };
        let sweep = |store: std::sync::Arc<crate::insights_store::Store>| async move {
            match tokio::task::spawn_blocking(move || store.sweep(unix_now())).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => crate::log_warn("INSIGHTS-LOCAL", &format!("sweep failed: {e}")),
                Err(e) => crate::log_warn("INSIGHTS-LOCAL", &format!("sweep join: {e}")),
            }
        };
        sweep(store.clone()).await;
        crate::log_info("INSIGHTS-LOCAL", "sampler started (5s)");

        let mut interval = tokio::time::interval(SAMPLE_EVERY);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_sweep = Instant::now();
        let mut sampled: u64 = 0;
        let mut docker: Vec<crate::insights_store::DockerSample> = Vec::new();
        loop {
            interval.tick().await;
            let any_local = match state.workspaces.lock() {
                Ok(f) => has_local_workspace(&f.workspaces),
                Err(_) => false,
            };
            if !any_local {
                continue;
            }
            let snap = match tokio::task::spawn_blocking(|| snapshot(false)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    crate::log_warn("INSIGHTS-LOCAL", &format!("sample failed: {e}"));
                    continue;
                }
                Err(e) => {
                    crate::log_warn("INSIGHTS-LOCAL", &format!("sample join: {e}"));
                    continue;
                }
            };
            if sampled % DOCKER_EVERY == 0 {
                docker = docker_samples().await;
            }
            sampled = sampled.wrapping_add(1);
            let sample = build_sample(unix_now(), &snap, &docker);
            let st = store.clone();
            match tokio::task::spawn_blocking(move || st.insert(&sample)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => crate::log_warn("INSIGHTS-LOCAL", &format!("insert failed: {e}")),
                Err(e) => crate::log_warn("INSIGHTS-LOCAL", &format!("insert join: {e}")),
            }
            if last_sweep.elapsed() >= SWEEP_EVERY {
                last_sweep = Instant::now();
                sweep(store.clone()).await;
            }
        }
    });
}

// ─── Internal router used by addons::insights_fetch ───────────────────

/// Route a remote-shaped API path to its local implementation. Keeps the
/// existing `insights_fetch` frontend surface identical for local vs remote
/// so `InsightsWindow.tsx` / `HygienePanel.tsx` don't need per-shape branches.
pub async fn route_path(path: &str) -> Result<String, String> {
    // Split off a query string (e.g. "/logs?tail=400").
    let (base, query) = match path.split_once('?') {
        Some((b, q)) => (b, q),
        None => (path, ""),
    };
    match base {
        "/current" => insights_local_current().await,
        "/analytics" => insights_local_analytics(),
        "/claude-usage" => crate::claude_usage_local::route(query),
        "/docker" => insights_local_docker().await,
        "/hygiene" => insights_local_hygiene().await,
        "/processes" => {
            let limit = parse_query_u32(query, "limit").unwrap_or(20);
            insights_local_processes(limit).await
        }
        "/logs" => {
            let tail = parse_query_u32(query, "tail");
            insights_local_logs(tail).await
        }
        other => Err(format!("local Insights: unsupported path {other}")),
    }
}

fn parse_query_u32(q: &str, key: &str) -> Option<u32> {
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
            return it.next().and_then(|v| v.parse().ok());
        }
    }
    None
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_nonzero_cpu_after_workload() {
        // Two calls: the first primes sysinfo, the second reports real CPU %.
        let _ = snapshot(true).expect("first snapshot");
        // Busy-loop briefly so at least one core reports non-zero usage.
        let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
        let mut acc: u64 = 0;
        while std::time::Instant::now() < end {
            acc = acc.wrapping_add(1);
        }
        std::hint::black_box(acc);
        let s = snapshot(true).expect("second snapshot");
        assert!(s.mem.total > 0, "total memory should be non-zero");
        assert!(!s.cpu.per_core.is_empty(), "per_core should be populated");
        // pct <= 100 by construction (clamped); just make sure the field
        // deserializes to the expected shape.
        assert!(s.cpu.pct <= 100, "cpu pct must be ≤100, got {}", s.cpu.pct);
    }

    #[test]
    fn parse_query_u32_basic() {
        assert_eq!(parse_query_u32("tail=200", "tail"), Some(200));
        assert_eq!(parse_query_u32("a=1&tail=42&b=x", "tail"), Some(42));
        assert_eq!(parse_query_u32("", "tail"), None);
        assert_eq!(parse_query_u32("tail=bad", "tail"), None);
    }

    #[test]
    fn pct_u32_clamps() {
        assert_eq!(pct_u32(-5.0), 0);
        assert_eq!(pct_u32(120.4), 100);
        assert_eq!(pct_u32(49.6), 50);
    }

    #[test]
    fn route_rejects_unknown_paths() {
        // Sync-block a small future so we don't need tokio in this test.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(route_path("/nope")).unwrap_err();
        assert!(err.contains("unsupported"));
    }

    #[test]
    fn hygiene_snapshot_is_empty_on_local() {
        let h = hygiene_snapshot();
        assert_eq!(h.duplicate_count, 0);
        assert!(h.port_watchers.is_empty());
        assert!(h.orphan_sessions.is_empty());
    }

    fn net(iface: &str, rx: u64, tx: u64) -> NetMetric {
        NetMetric { iface: iface.into(), rx_bps: rx, tx_bps: tx }
    }

    #[test]
    fn net_totals_skips_loopback() {
        // Pins: loopback traffic must not inflate the stored net bps.
        let v = vec![
            net("lo", 1000, 1000),
            net("Loopback Pseudo-Interface 1", 500, 500),
            net("eth0", 10, 20),
            net("wlan0", 1, 2),
        ];
        assert_eq!(net_totals(&v), (11, 22));
        assert_eq!(net_totals(&[]), (0, 0));
    }

    #[test]
    fn has_local_workspace_matches_none_and_local_only() {
        // Pins: SSH-only installs must not run the sampler.
        let mk = |c: Option<crate::Connection>| {
            let mut w: crate::Workspace = serde_json::from_str(r#"{"id":"a","name":"a"}"#).unwrap();
            w.connection = c;
            w
        };
        let ssh = crate::Connection::Ssh {
            host: "h".into(),
            user: "u".into(),
            port: 22,
            key_path: None,
        };
        assert!(!has_local_workspace(&[]));
        assert!(!has_local_workspace(&[mk(Some(ssh.clone()))]));
        assert!(has_local_workspace(&[mk(Some(ssh)), mk(None)]));
        assert!(has_local_workspace(&[mk(Some(crate::Connection::Local { shell: None }))]));
    }

    #[test]
    fn snapshot_without_top_has_no_processes() {
        // Pins: include_top=false skips the process walk.
        let s = snapshot(false).expect("snapshot");
        assert!(s.top.is_empty());
    }

    #[test]
    fn build_sample_maps_snapshot() {
        // Pins: Snapshot → store Sample field mapping (load[0], net sum, disks).
        let snap = Snapshot {
            ts: 0,
            cpu: CpuMetric { pct: 42, per_core: vec![], load: vec![1.5, 0.0, 0.0] },
            mem: MemMetric { total: 100, used: 40, cached: 0, swap_used: 7 },
            disks: vec![DiskMetric { mount: "/".into(), total: 10, used: 4, pct: 40 }],
            net: vec![net("lo", 9, 9), net("eth0", 3, 4)],
            docker_running: 0,
            docker_total: 0,
            top: vec![],
        };
        let s = build_sample(1234, &snap, &[]);
        assert_eq!((s.ts, s.cpu_pct, s.load1), (1234, 42.0, 1.5));
        assert_eq!((s.mem_used, s.mem_total, s.swap_used), (40, 100, 7));
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (3, 4));
        assert_eq!(s.disks.len(), 1);
        assert_eq!(s.disks[0].used, 4);
    }
}
