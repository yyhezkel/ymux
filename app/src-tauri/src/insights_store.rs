//! Local metrics history — SQLite store mirroring the remote daemon's
//! `server/internal/insights/store.go` (same schema, 7-day retention).
//!
//! One connection behind a `Mutex`; callers run it on `spawn_blocking`.

// Consumers (sampler, analytics route) land in later tasks.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

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
}
