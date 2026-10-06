//! Local metrics history — SQLite store mirroring the remote daemon's
//! `server/internal/insights/store.go` (same schema, 7-day retention).
//!
//! One connection behind a `Mutex`; callers run it on `spawn_blocking`.

// Consumers (sampler, analytics route) land in later tasks.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::Serialize;

/// Rolling window kept, seconds (store.go `retentionDays` = 7).
const RETENTION_SECS: i64 = 7 * 24 * 3600;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS samples (
  ts INTEGER NOT NULL,
  cpu_pct REAL, load1 REAL,
  mem_used INTEGER, mem_total INTEGER, swap_used INTEGER,
  net_rx_bps INTEGER, net_tx_bps INTEGER
);
CREATE INDEX IF NOT EXISTS idx_samples_ts ON samples(ts);
CREATE TABLE IF NOT EXISTS disk_samples (
  ts INTEGER, mount TEXT, used INTEGER, total INTEGER
);
CREATE INDEX IF NOT EXISTS idx_disk_ts ON disk_samples(ts);
CREATE TABLE IF NOT EXISTS docker_samples (
  ts INTEGER, cid TEXT, name TEXT, cpu_pct REAL, mem_used INTEGER, state TEXT
);
CREATE INDEX IF NOT EXISTS idx_docker_ts ON docker_samples(ts);
";

#[derive(Debug, Clone, Default)]
pub struct DiskSample {
    pub mount: String,
    pub used: i64,
    pub total: i64,
}

#[derive(Debug, Clone, Default)]
pub struct DockerSample {
    pub cid: String,
    pub name: String,
    pub cpu_pct: f64,
    pub mem_used: i64,
    pub state: String,
}

/// One sampler tick: core metrics + per-disk + per-container.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub ts: i64,
    pub cpu_pct: f64,
    pub load1: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub swap_used: i64,
    pub net_rx_bps: i64,
    pub net_tx_bps: i64,
    pub disks: Vec<DiskSample>,
    pub docker: Vec<DockerSample>,
}

/// Narrowest / widest window (analytics.go `analyticsMinSpan` / `analyticsMaxSpan`).
pub const ANALYTICS_MIN_SPAN_SECS: i64 = 5 * 60;
pub const ANALYTICS_MAX_SPAN_SECS: i64 = RETENTION_SECS;

/// A sample at or above this CPU % counts toward `busy_pct`.
const CPU_BUSY_PCT: f64 = 80.0;

/// Used-memory percent, guarding mem_total = 0 (analytics.go `memPctExpr`).
const MEM_PCT_EXPR: &str = "(CASE WHEN mem_total > 0 THEN mem_used * 100.0 / mem_total END)";

// JSON field names are the wire contract with analytics.go / insightsReport.ts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AnalyticsTotals {
    pub samples: i64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub busy_pct: f64,
    pub mem_pct_avg: f64,
    pub mem_pct_max: f64,
    pub mem_used_max: u64,
    pub mem_total: u64,
    pub swap_max: u64,
    pub load_avg: f64,
    pub load_max: f64,
    pub rx_avg_bps: f64,
    pub tx_avg_bps: f64,
    pub rx_bytes: f64,
    pub tx_bytes: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AnalyticsPoint {
    pub t: i64,
    pub n: i64,
    pub cpu: f64,
    pub cpu_max: f64,
    pub mem_pct: f64,
    pub load: f64,
    pub rx_bps: f64,
    pub tx_bps: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AnalyticsPeriod {
    pub t: i64,
    pub n: i64,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub mem_pct_avg: f64,
    pub load_avg: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AnalyticsDisk {
    pub mount: String,
    pub used_avg: f64,
    pub used_last: u64,
    pub total: u64,
    pub pct_last: f64,
    pub growth_bytes: i64,
    pub n: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AnalyticsContainer {
    pub name: String,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub mem_avg: f64,
    pub mem_max: u64,
    pub uptime_pct: f64,
    pub n: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AnalyticsReport {
    pub bucketed: bool,
    pub since: i64,
    pub until: i64,
    pub bucket_s: i64,
    pub period_s: i64,
    pub totals: AnalyticsTotals,
    pub series: Vec<AnalyticsPoint>,
    pub by_period: Vec<AnalyticsPeriod>,
    pub by_disk: Vec<AnalyticsDisk>,
    pub by_container: Vec<AnalyticsContainer>,
}

/// Nullable aggregate → 0 when absent/non-finite, rounded to 2 decimals (analytics.go `nz`).
fn nz(v: Option<f64>) -> f64 {
    match v {
        Some(x) if x.is_finite() => (x * 100.0).round() / 100.0,
        _ => 0.0,
    }
}

/// Nullable aggregate → unsigned, 0 when absent or non-positive (analytics.go `nzU`).
fn nz_u(v: Option<f64>) -> u64 {
    match v {
        Some(x) if x > 0.0 => x as u64,
        _ => 0,
    }
}

/// Percent with 2 decimals from a part/whole pair.
fn pct2(part: f64, whole: f64) -> f64 {
    (part / whole * 10000.0).round() / 100.0
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> {
        let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        // journal_mode returns a row, so query it rather than execute it.
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
            .map_err(|e| format!("pragma journal_mode: {e}"))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=3000;")
            .map_err(|e| format!("pragma: {e}"))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("schema: {e}"))?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// `<config_dir>/insights-local.db`.
    pub fn open_default() -> Result<Store, String> {
        let dir = ymux_core::config_dir_pub()?;
        Store::open(&dir.join("insights-local.db"))
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "store lock poisoned".to_string())
    }

    /// One tick in a single transaction (rolled back on any failure).
    pub fn insert(&self, s: &Sample) -> Result<(), String> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
        tx.execute(
            "INSERT INTO samples (ts,cpu_pct,load1,mem_used,mem_total,swap_used,net_rx_bps,net_tx_bps)
             VALUES (?,?,?,?,?,?,?,?)",
            params![
                s.ts,
                s.cpu_pct,
                s.load1,
                s.mem_used,
                s.mem_total,
                s.swap_used,
                s.net_rx_bps,
                s.net_tx_bps
            ],
        )
        .map_err(|e| format!("insert samples: {e}"))?;
        for d in &s.disks {
            tx.execute(
                "INSERT INTO disk_samples (ts,mount,used,total) VALUES (?,?,?,?)",
                params![s.ts, d.mount, d.used, d.total],
            )
            .map_err(|e| format!("insert disk: {e}"))?;
        }
        for c in &s.docker {
            tx.execute(
                "INSERT INTO docker_samples (ts,cid,name,cpu_pct,mem_used,state) VALUES (?,?,?,?,?,?)",
                params![s.ts, c.cid, c.name, c.cpu_pct, c.mem_used, c.state],
            )
            .map_err(|e| format!("insert docker: {e}"))?;
        }
        tx.commit().map_err(|e| format!("commit: {e}"))
    }

    /// Aggregate one window into a report (port of analytics.go `analytics`).
    /// Caller clamps `since`/`until`/`points`; `points` < 1 is treated as 1.
    pub fn analytics(&self, since: i64, until: i64, points: i64) -> Result<AnalyticsReport, String> {
        let span = (until - since).max(1);
        let bucket = (span / points.max(1)).max(1);
        // Hour rows for short windows, day rows once the window is long.
        let period = if span >= 48 * 3600 { 86_400 } else { 3600 };
        let conn = self.lock()?;
        let q = |e: rusqlite::Error| format!("analytics: {e}");
        let mut rep = AnalyticsReport {
            bucketed: true,
            since,
            until,
            bucket_s: bucket,
            period_s: period,
            ..Default::default()
        };
        rep.totals = totals(&conn, since, until).map_err(q)?;
        rep.series = series(&conn, since, until, bucket).map_err(q)?;
        rep.by_period = by_period(&conn, since, until, period).map_err(q)?;
        rep.by_disk = by_disk(&conn, since, until).map_err(q)?;
        rep.by_container = by_container(&conn, since, until).map_err(q)?;
        Ok(rep)
    }

    /// Delete rows older than the retention window, then truncate the WAL.
    pub fn sweep(&self, now_unix: i64) -> Result<(), String> {
        let cut = now_unix - RETENTION_SECS;
        let conn = self.lock()?;
        for t in ["samples", "disk_samples", "docker_samples"] {
            conn.execute(&format!("DELETE FROM {t} WHERE ts < ?"), params![cut])
                .map_err(|e| format!("sweep {t}: {e}"))?;
        }
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .map_err(|e| format!("wal_checkpoint: {e}"))
    }
}

fn totals(c: &Connection, since: i64, until: i64) -> rusqlite::Result<AnalyticsTotals> {
    let sql = format!(
        "SELECT COUNT(*), MIN(ts), MAX(ts),
                AVG(cpu_pct), MAX(cpu_pct),
                SUM(CASE WHEN cpu_pct >= ? THEN 1 ELSE 0 END),
                AVG({m}), MAX({m}),
                MAX(mem_used), MAX(mem_total), MAX(swap_used),
                AVG(load1), MAX(load1),
                AVG(net_rx_bps), AVG(net_tx_bps)
         FROM samples WHERE ts >= ? AND ts <= ?",
        m = MEM_PCT_EXPR
    );
    c.query_row(&sql, params![CPU_BUSY_PCT, since, until], |r| {
        let n: i64 = r.get(0)?;
        let first: Option<i64> = r.get(1)?;
        let last: Option<i64> = r.get(2)?;
        let busy: Option<f64> = r.get(5)?;
        let mut t = AnalyticsTotals {
            samples: n,
            first_ts: first.unwrap_or(0),
            last_ts: last.unwrap_or(0),
            cpu_avg: nz(r.get(3)?),
            cpu_max: nz(r.get(4)?),
            mem_pct_avg: nz(r.get(6)?),
            mem_pct_max: nz(r.get(7)?),
            mem_used_max: nz_u(r.get(8)?),
            mem_total: nz_u(r.get(9)?),
            swap_max: nz_u(r.get(10)?),
            load_avg: nz(r.get(11)?),
            load_max: nz(r.get(12)?),
            rx_avg_bps: nz(r.get(13)?),
            tx_avg_bps: nz(r.get(14)?),
            ..Default::default()
        };
        if let (true, Some(b)) = (n > 0, busy) {
            t.busy_pct = pct2(b, n as f64);
        }
        // Rate x observed span: an estimate, the sampler stores rates not counters.
        let observed = t.last_ts - t.first_ts;
        if observed > 0 {
            t.rx_bytes = (t.rx_avg_bps * observed as f64).round();
            t.tx_bytes = (t.tx_avg_bps * observed as f64).round();
        }
        Ok(t)
    })
}

fn series(c: &Connection, since: i64, until: i64, bucket: i64) -> rusqlite::Result<Vec<AnalyticsPoint>> {
    let sql = format!(
        "SELECT (ts / ?) * ? AS b, COUNT(*),
                AVG(cpu_pct), MAX(cpu_pct), AVG({m}),
                AVG(load1), AVG(net_rx_bps), AVG(net_tx_bps)
         FROM samples WHERE ts >= ? AND ts <= ?
         GROUP BY b ORDER BY b ASC",
        m = MEM_PCT_EXPR
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params![bucket, bucket, since, until], |r| {
        Ok(AnalyticsPoint {
            t: r.get(0)?,
            n: r.get(1)?,
            cpu: nz(r.get(2)?),
            cpu_max: nz(r.get(3)?),
            mem_pct: nz(r.get(4)?),
            load: nz(r.get(5)?),
            rx_bps: nz(r.get(6)?),
            tx_bps: nz(r.get(7)?),
        })
    })?;
    rows.collect()
}

fn by_period(c: &Connection, since: i64, until: i64, period: i64) -> rusqlite::Result<Vec<AnalyticsPeriod>> {
    // Newest first (table render order), capped so 7d of hour rows can't return 168.
    let sql = format!(
        "SELECT (ts / ?) * ? AS b, COUNT(*),
                AVG(cpu_pct), MAX(cpu_pct), AVG({m}), AVG(load1)
         FROM samples WHERE ts >= ? AND ts <= ?
         GROUP BY b ORDER BY b DESC LIMIT 24",
        m = MEM_PCT_EXPR
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params![period, period, since, until], |r| {
        Ok(AnalyticsPeriod {
            t: r.get(0)?,
            n: r.get(1)?,
            cpu_avg: nz(r.get(2)?),
            cpu_max: nz(r.get(3)?),
            mem_pct_avg: nz(r.get(4)?),
            load_avg: nz(r.get(5)?),
        })
    })?;
    rows.collect()
}

fn by_disk(c: &Connection, since: i64, until: i64) -> rusqlite::Result<Vec<AnalyticsDisk>> {
    // Correlated subqueries pick first/last `used` per mount: their difference is growth.
    let mut st = c.prepare(
        "SELECT d.mount, COUNT(*), AVG(d.used), MAX(d.total),
                (SELECT f.used FROM disk_samples f
                  WHERE f.mount = d.mount AND f.ts >= ? AND f.ts <= ?
                  ORDER BY f.ts ASC LIMIT 1),
                (SELECT l.used FROM disk_samples l
                  WHERE l.mount = d.mount AND l.ts >= ? AND l.ts <= ?
                  ORDER BY l.ts DESC LIMIT 1)
         FROM disk_samples d WHERE d.ts >= ? AND d.ts <= ?
         GROUP BY d.mount ORDER BY 3 DESC",
    )?;
    let rows = st.query_map(params![since, until, since, until, since, until], |r| {
        let first: Option<f64> = r.get(4)?;
        let last: Option<f64> = r.get(5)?;
        let mut d = AnalyticsDisk {
            mount: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            n: r.get(1)?,
            used_avg: nz(r.get(2)?),
            total: nz_u(r.get(3)?),
            used_last: nz_u(last),
            ..Default::default()
        };
        if d.total > 0 {
            d.pct_last = pct2(d.used_last as f64, d.total as f64);
        }
        if let (Some(f), Some(l)) = (first, last) {
            d.growth_bytes = (l - f) as i64;
        }
        Ok(d)
    })?;
    rows.collect()
}

fn by_container(c: &Connection, since: i64, until: i64) -> rusqlite::Result<Vec<AnalyticsContainer>> {
    let mut st = c.prepare(
        "SELECT name, COUNT(*), AVG(cpu_pct), MAX(cpu_pct), AVG(mem_used), MAX(mem_used),
                SUM(CASE WHEN state = 'running' THEN 1 ELSE 0 END)
         FROM docker_samples WHERE ts >= ? AND ts <= ? AND name <> ''
         GROUP BY name ORDER BY 3 DESC LIMIT 20",
    )?;
    let rows = st.query_map(params![since, until], |r| {
        let n: i64 = r.get(1)?;
        let running: Option<f64> = r.get(6)?;
        let mut ct = AnalyticsContainer {
            name: r.get(0)?,
            n,
            cpu_avg: nz(r.get(2)?),
            cpu_max: nz(r.get(3)?),
            mem_avg: nz(r.get(4)?),
            mem_max: nz_u(r.get(5)?),
            ..Default::default()
        };
        if let (true, Some(run)) = (n > 0, running) {
            ct.uptime_pct = pct2(run, n as f64);
        }
        Ok(ct)
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(s: &Store, table: &str) -> i64 {
        let c = s.lock().unwrap();
        c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn sample(ts: i64) -> Sample {
        Sample {
            ts,
            cpu_pct: 12.5,
            disks: vec![DiskSample {
                mount: "/".into(),
                used: 1,
                total: 2,
            }],
            docker: vec![DockerSample {
                cid: "abc".into(),
                name: "web".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    // Pins retention: a sweep that keeps >7d rows would grow the DB forever.
    #[test]
    fn sweep_deletes_rows_older_than_retention() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let now = 10_000_000;
        s.insert(&sample(now - RETENTION_SECS - 1)).unwrap();
        s.insert(&sample(now - RETENTION_SECS)).unwrap();
        s.insert(&sample(now)).unwrap();
        s.sweep(now).unwrap();
        for t in ["samples", "disk_samples", "docker_samples"] {
            assert_eq!(count(&s, t), 2, "{t}");
        }
    }

    // Pins on-disk persistence: history must survive an app restart.
    #[test]
    fn store_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        let s = Store::open(&p).unwrap();
        s.insert(&sample(42)).unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(count(&s, "samples"), 1);
        let ts: i64 = s
            .lock()
            .unwrap()
            .query_row("SELECT ts FROM samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ts, 42);
    }

    // Pins boundary error: an uncreatable path must be Err, not a panic.
    #[test]
    fn open_uncreatable_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::open(&dir.path().join("no/such/dir/t.db")).is_err());
    }

    fn mem_store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("a.db")).unwrap();
        // keep the temp dir alive for the test's lifetime
        std::mem::forget(dir);
        s
    }

    // Pins wire parity with analytics.go: a renamed/missing key breaks the Analytics tab parse.
    #[test]
    fn analytics_report_has_remote_shape() {
        let s = mem_store();
        for i in 0..4 {
            let mut smp = sample(1000 + i * 5);
            smp.cpu_pct = if i < 2 { 90.0 } else { 10.0 };
            smp.mem_used = 50;
            smp.mem_total = 100;
            smp.net_rx_bps = 10;
            smp.disks[0].used = 100 + i * 10;
            smp.disks[0].total = 1000;
            s.insert(&smp).unwrap();
        }
        let rep = s.analytics(1000, 1100, 20).unwrap();
        let v = serde_json::to_value(&rep).unwrap();
        assert_eq!(v["bucketed"], true);
        assert_eq!(v["totals"]["samples"], 4);
        for k in [
            "bucketed", "since", "until", "bucket_s", "period_s", "totals", "series",
            "by_period", "by_disk", "by_container",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert_eq!(v["totals"]["busy_pct"], 50.0);
        assert_eq!(v["totals"]["mem_pct_avg"], 50.0);
        assert_eq!(v["totals"]["rx_bytes"], 150.0);
        assert_eq!(v["by_disk"][0]["growth_bytes"], 30);
        assert_eq!(v["by_container"][0]["name"], "web");
    }

    // Pins the day-period switch: a 7d window in hour rows would be a wall of numbers.
    #[test]
    fn analytics_7d_window_uses_day_periods() {
        let s = mem_store();
        let until = 10_000_000;
        for d in 0..6 {
            s.insert(&sample(until - d * 86_400 - 60)).unwrap();
        }
        let rep = s.analytics(until - RETENTION_SECS, until, 120).unwrap();
        assert_eq!(rep.period_s, 86_400);
        assert!(rep.by_period.len() >= 6);
        assert!(!rep.series.is_empty());
    }

    // Pins empty-store contract: no rows → zeros and empty arrays, never an error.
    #[test]
    fn analytics_empty_store_is_zeroed() {
        let s = mem_store();
        let rep = s.analytics(0, 100, 20).unwrap();
        assert_eq!(rep.totals.samples, 0);
        assert!(rep.series.is_empty() && rep.by_period.is_empty());
        assert!(rep.by_disk.is_empty() && rep.by_container.is_empty());
    }
}
