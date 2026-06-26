/*
 * WAL apply deduplication — Phase F
 *
 * Standby nodes track seen LSNs so retried replication does not double-apply DML.
 */

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::gateway::native_sql::NativeSqlEngine;

use super::transport::WalEntry;

/// Tracks WAL entries applied on a replica / standby node.
pub struct WalApplyTracker {
    high_water: AtomicU64,
    seen: Mutex<HashSet<u64>>,
}

impl Default for WalApplyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl WalApplyTracker {
    pub fn new() -> Self {
        Self {
            high_water: AtomicU64::new(0),
            seen: Mutex::new(HashSet::new()),
        }
    }

    pub fn last_applied_lsn(&self) -> u64 {
        self.high_water.load(Ordering::Relaxed)
    }

    /// Returns `true` when this LSN is new and the caller should execute SQL.
    pub fn claim_lsn(&self, lsn: u64) -> bool {
        let mut seen = self.seen.lock().expect("wal seen lock");
        if !seen.insert(lsn) {
            return false;
        }

        let mut hw = self.high_water.load(Ordering::Relaxed);
        while lsn > hw {
            match self.high_water.compare_exchange_weak(
                hw,
                lsn,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(cur) => hw = cur,
            }
        }

        if seen.len() > 8192 {
            let floor = self.high_water.load(Ordering::Relaxed).saturating_sub(4096);
            seen.retain(|&x| x > floor);
        }
        true
    }
}

pub fn verify_wal_checksum(entry: &WalEntry) -> bool {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(entry.sql.as_bytes());
    hasher.finalize() == entry.checksum
}

/// Apply a shipped WAL entry idempotently.
///
/// Returns `Ok(true)` when SQL was executed, `Ok(false)` when skipped (duplicate/CRC).
pub fn apply_wal_entry(
    tracker: &WalApplyTracker,
    engine: &NativeSqlEngine,
    entry: &WalEntry,
) -> Result<bool, String> {
    if !verify_wal_checksum(entry) {
        tracing::warn!("WAL entry CRC mismatch at LSN {}, skipping", entry.lsn);
        return Ok(false);
    }
    if !tracker.claim_lsn(entry.lsn) {
        tracing::debug!("WAL entry LSN {} already applied, skipping", entry.lsn);
        return Ok(false);
    }
    engine.execute(&entry.sql)?;
    super::cluster_metrics::set_standby_wal_lsn(tracker.last_applied_lsn());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::transport::WalEntry;

    fn entry(lsn: u64, sql: &str) -> WalEntry {
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(sql.as_bytes());
        WalEntry {
            lsn,
            sql: sql.to_string(),
            checksum: hasher.finalize(),
        }
    }

    #[test]
    fn duplicate_lsn_is_skipped() {
        let tracker = WalApplyTracker::new();
        let engine = NativeSqlEngine::new();
        engine
            .execute("CREATE TABLE wal_dedup (id INTEGER PRIMARY KEY)")
            .expect("ddl");

        let e = entry(1, "INSERT INTO wal_dedup (id) VALUES (1)");
        assert!(apply_wal_entry(&tracker, &engine, &e).expect("first"));
        assert!(!apply_wal_entry(&tracker, &engine, &e).expect("dup"));
        assert_eq!(tracker.last_applied_lsn(), 1);

        let rows = engine
            .execute("SELECT id FROM wal_dedup")
            .expect("count");
        assert_eq!(rows.rows.len(), 1);
    }
}
