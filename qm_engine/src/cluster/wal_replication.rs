/*
 * Async/sync WAL replication — ship DML to standby peers after local primary commit.
 *
 * Env:
 *   QM_CLUSTER_WAL_REPLICATE=1
 *   QM_CLUSTER_WAL_SYNC=1          (optional: wait for all replica acks)
 *   QM_CLUSTER_WAL_PEERS=host:port[,host:port...]
 */

use std::sync::atomic::{AtomicU64, Ordering};

use super::config::ClusterNodeConfig;
use super::transport::{NodeClient, WalEntry};

static WAL_LSN: AtomicU64 = AtomicU64::new(1);

/// DML statements replayed via WAL shipping.
pub fn is_replicable_dml(sql: &str) -> bool {
    let up = sql.trim().to_ascii_uppercase();
    up.starts_with("INSERT ")
        || up.starts_with("UPDATE ")
        || up.starts_with("DELETE ")
        || up.starts_with("REPLACE ")
}

pub fn wal_entry_for_sql(sql: &str) -> WalEntry {
    let lsn = WAL_LSN.fetch_add(1, Ordering::Relaxed);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(sql.as_bytes());
    WalEntry {
        lsn,
        sql: sql.to_string(),
        checksum: hasher.finalize(),
    }
}

/// After a successful local primary write, ship WAL to configured peers.
pub fn replicate_after_local_write(cfg: &ClusterNodeConfig, sql: &str) -> Result<(), String> {
    if !cfg.wal_replicate || !is_replicable_dml(sql) || cfg.wal_peers.is_empty() {
        return Ok(());
    }

    let entry = wal_entry_for_sql(sql);
    super::cluster_metrics::set_primary_wal_lsn(entry.lsn);
    super::wal_buffer::record_shipped(&entry);
    for peer in &cfg.wal_peers {
        if cfg.local_addr.is_some_and(|local| local == *peer) {
            continue;
        }
        if cfg.wal_sync {
            ship_wal_blocking(*peer, &entry)?;
        } else {
            let peer = *peer;
            let ship = entry.clone();
            std::thread::spawn(move || {
                let _ = ship_wal_blocking(peer, &ship);
            });
        }
    }
    Ok(())
}

pub fn primary_wal_lsn() -> u64 {
    WAL_LSN.load(Ordering::Relaxed).saturating_sub(1)
}

fn ship_wal_blocking(peer: std::net::SocketAddr, entry: &WalEntry) -> Result<(), String> {
    let client = NodeClient::new(0, peer);
    let ack = client
        .send_wal_entry_blocking(entry)
        .map_err(|e| format!("wal replicate to {peer}: {e}"))?;
    if ack != entry.lsn {
        return Err(format!(
            "wal replicate to {peer}: ack lsn {ack} != sent {}",
            entry.lsn
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_replicable_dml() {
        assert!(is_replicable_dml("INSERT INTO t VALUES (1)"));
        assert!(is_replicable_dml("UPDATE t SET x=1"));
        assert!(!is_replicable_dml("SELECT * FROM t"));
        assert!(!is_replicable_dml("CREATE TABLE t (id INT)"));
    }

    #[test]
    fn wal_entry_checksum_matches_sql() {
        let entry = wal_entry_for_sql("INSERT INTO t VALUES (1)");
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(entry.sql.as_bytes());
        assert_eq!(entry.checksum, hasher.finalize());
    }
}
