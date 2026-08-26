/*
 * WAL catch-up — ring buffer + durable on-disk segment replay.
 */

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::transport::WalEntry;
use super::wal_buffer;

/// Collect WAL entries for standby gap heal: in-memory ring first, then durable file tail.
pub fn collect_catchup_entries(data_dir: Option<&Path>, after_lsn: u64) -> Vec<WalEntry> {
    let mut out = wal_buffer::entries_after_lsn(after_lsn);
    if let Some(dir) = data_dir {
        let durable = durable_entries_after(dir, after_lsn);
        merge_durable(&mut out, durable);
    }
    out.sort_by_key(|e| e.lsn);
    out.dedup_by_key(|e| e.lsn);
    out
}

const DURABLE_LSN_BASE: u64 = 1_000_000_000;

fn merge_durable(ring: &mut Vec<WalEntry>, durable: Vec<WalEntry>) {
    for entry in durable {
        if ring.iter().any(|e| e.lsn == entry.lsn || e.sql == entry.sql) {
            continue;
        }
        ring.push(entry);
    }
}

fn wal_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("native_sql.wal")
}

/// Replay durable append WAL lines as WalEntry records (line number = LSN).
pub fn durable_entries_after(data_dir: &Path, after_lsn: u64) -> Vec<WalEntry> {
    let path = wal_file_path(data_dir);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let lsn = DURABLE_LSN_BASE.saturating_add(idx as u64 + 1);
        // Preallocated WAL tail is zero-filled; first NUL = logical EOF.
        if matches!(&line, Ok(s) if s.as_bytes().first() == Some(&0)) {
            break;
        }
        if lsn <= after_lsn {
            continue;
        }
        let sql = match line {
            Ok(s) if !s.trim().is_empty() => s,
            _ => continue,
        };
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(sql.as_bytes());
        out.push(WalEntry {
            lsn,
            sql,
            checksum: hasher.finalize(),
        });
    }
    out
}

/// Standby heal loop: request catch-up from primary and apply locally.
pub fn heal_standby_from_primary(
    cfg: &super::config::ClusterNodeConfig,
    data_dir: Option<&Path>,
    apply: impl Fn(&WalEntry) -> Result<(), String>,
) -> Result<usize, String> {
    let primary = cfg
        .wal_peers
        .first()
        .copied()
        .or(cfg.local_addr)
        .ok_or_else(|| "no primary for catch-up".to_string())?;
    let last = super::cluster_metrics::standby_wal_lsn_metric();
    let client = super::transport::NodeClient::new(0, primary);
    let entries = client
        .request_wal_catchup_blocking(last)
        .map_err(|e| format!("catch-up from {primary}: {e}"))?;
    let mut applied = 0usize;
    for entry in entries {
        apply(&entry)?;
        super::cluster_metrics::set_standby_wal_lsn(entry.lsn);
        applied += 1;
    }
    if applied == 0 {
        if let Some(dir) = data_dir {
            for entry in durable_entries_after(dir, last) {
                apply(&entry)?;
                super::cluster_metrics::set_standby_wal_lsn(entry.lsn);
                applied += 1;
            }
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn durable_wal_replay_assigns_line_lsn() {
        let dir = std::env::temp_dir().join(format!(
            "qm_wal_catchup_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(wal_file_path(&dir)).unwrap();
        writeln!(f, "INSERT INTO t VALUES (1);").unwrap();
        writeln!(f, "INSERT INTO t VALUES (2);").unwrap();
        let entries = durable_entries_after(&dir, DURABLE_LSN_BASE + 1);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].lsn >= DURABLE_LSN_BASE);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
