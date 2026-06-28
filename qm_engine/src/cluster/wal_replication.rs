/*
 * Async/sync WAL replication — ship DML to standby peers after local primary commit.
 */

use std::sync::atomic::{AtomicU64, Ordering};

use super::config::ClusterNodeConfig;
use super::replica::ConsistencyLevel;
use super::transport::{NodeClient, WalEntry};

static WAL_LSN: AtomicU64 = AtomicU64::new(1);

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

fn active_peers(cfg: &ClusterNodeConfig) -> Vec<std::net::SocketAddr> {
    cfg.wal_peers
        .iter()
        .copied()
        .filter(|peer| !cfg.local_addr.is_some_and(|local| local == *peer))
        .collect()
}

fn required_acks(peer_count: usize, level: ConsistencyLevel) -> usize {
    match level {
        ConsistencyLevel::One => 1,
        ConsistencyLevel::Quorum => peer_count / 2 + 1,
        ConsistencyLevel::All => peer_count.max(1),
    }
}

/// Public wrapper for chaos battery / docs (W = f(N, level)).
pub fn required_ack_count(peer_count: usize, level: ConsistencyLevel) -> usize {
    required_acks(peer_count, level)
}

pub fn replicate_after_local_write(cfg: &ClusterNodeConfig, sql: &str) -> Result<(), String> {
    if !cfg.wal_replicate || !is_replicable_dml(sql) || cfg.wal_peers.is_empty() {
        return Ok(());
    }

    let entry = wal_entry_for_sql(sql);
    super::cluster_metrics::set_primary_wal_lsn(entry.lsn);
    super::wal_buffer::record_shipped(&entry);

    let peers = active_peers(cfg);
    if peers.is_empty() {
        return Ok(());
    }

    if cfg.wal_sync {
        ship_wal_with_quorum(cfg, &entry, &peers)?;
    } else {
        for peer in peers {
            let ship = entry.clone();
            std::thread::spawn(move || {
                let _ = ship_wal_blocking(peer, &ship);
            });
        }
    }
    Ok(())
}

fn ship_wal_with_quorum(
    cfg: &ClusterNodeConfig,
    entry: &WalEntry,
    peers: &[std::net::SocketAddr],
) -> Result<(), String> {
    let need = required_acks(peers.len(), cfg.write_quorum);
    let mut acks = 0usize;
    let mut last_err = String::new();
    for peer in peers {
        match ship_wal_blocking(*peer, entry) {
            Ok(()) => {
                acks += 1;
                if acks >= need {
                    return Ok(());
                }
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!(
        "wal write quorum failed: {acks}/{need} acks ({})",
        if last_err.is_empty() {
            "no peers".into()
        } else {
            last_err
        }
    ))
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
        assert!(!is_replicable_dml("SELECT * FROM t"));
    }

    #[test]
    fn quorum_math_majority_of_three() {
        assert_eq!(required_acks(3, ConsistencyLevel::Quorum), 2);
        assert_eq!(required_acks(3, ConsistencyLevel::All), 3);
    }
}
