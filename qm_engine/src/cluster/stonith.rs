/*
 * STONITH-style primary lease + coordinated promote via meta Raft.
 *
 * Env:
 *   QM_CLUSTER_STONITH=1
 *   QM_CLUSTER_STONITH_LEASE_SECS=30
 */

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::config::ClusterNodeConfig;
use super::fencing;
use super::meta_cluster::MetaCommand;
use super::meta_raft_network;
use super::runtime::ClusterRuntime;

#[derive(Debug)]
struct LeaseRecord {
    node_id: u32,
    epoch: u64,
    expires_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn stonith_enabled(cfg: &ClusterNodeConfig) -> bool {
    cfg.stonith_enabled
}

pub fn lease_path(data_dir: &Path) -> PathBuf {
    data_dir.join("cluster_primary.lease")
}

fn parse_lease(raw: &str) -> Option<LeaseRecord> {
    let mut node_id = None;
    let mut epoch = None;
    let mut expires_ms = None;
    for part in raw.split_whitespace() {
        if let Some(v) = part.strip_prefix("node=") {
            node_id = v.parse().ok();
        } else if let Some(v) = part.strip_prefix("epoch=") {
            epoch = v.parse().ok();
        } else if let Some(v) = part.strip_prefix("expires=") {
            expires_ms = v.parse().ok();
        }
    }
    Some(LeaseRecord {
        node_id: node_id?,
        epoch: epoch?,
        expires_ms: expires_ms?,
    })
}

pub fn read_lease(data_dir: &Path) -> Option<LeaseRecord> {
    let raw = fs::read_to_string(lease_path(data_dir)).ok()?;
    parse_lease(&raw)
}

pub fn write_lease(data_dir: &Path, node_id: u32, epoch: u64, ttl: Duration) -> Result<(), String> {
    let expires = now_ms().saturating_add(ttl.as_millis() as u64);
    let body = format!("node={node_id} epoch={epoch} expires={expires}\n");
    let path = lease_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("lease dir: {e}"))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| format!("lease open: {e}"))?;
    f.write_all(body.as_bytes())
        .map_err(|e| format!("lease write: {e}"))?;
    f.sync_all().map_err(|e| format!("lease sync: {e}"))?;
    Ok(())
}

pub fn holds_valid_lease(data_dir: &Path, node_id: u32) -> bool {
    let Some(lease) = read_lease(data_dir) else {
        return false;
    };
    lease.node_id == node_id
        && lease.epoch >= fencing::current_epoch()
        && lease.expires_ms > now_ms()
}

/// Reject local writes when STONITH is on and lease is missing/expired.
pub fn require_write_lease(data_dir: Option<&Path>, cfg: &ClusterNodeConfig) -> Result<(), String> {
    if !stonith_enabled(cfg) {
        return Ok(());
    }
    let Some(dir) = data_dir else {
        return Err("STONITH requires persistent data_dir".into());
    };
    if holds_valid_lease(dir, cfg.node_id) {
        Ok(())
    } else {
        Err(format!(
            "STONITH: node {} lacks valid primary lease (epoch={})",
            cfg.node_id,
            fencing::current_epoch()
        ))
    }
}

/// Replicate fence + epoch bump via meta quorum, then write local lease.
pub fn promote_with_fence(
    cfg: &ClusterNodeConfig,
    runtime: &ClusterRuntime,
    data_dir: Option<&Path>,
    shard_id: u32,
    failed_primary: SocketAddr,
    new_primary: SocketAddr,
) -> Result<u64, String> {
    let epoch = fencing::current_epoch().saturating_add(1);
    if super::meta_raft_network::networked_meta_ready(cfg) {
        let cmd = MetaCommand::FencePrimary {
            shard_id,
            new_primary,
            epoch,
        };
        super::meta_raft_network::propose_with_quorum(cfg, cmd)
            .ok_or_else(|| "STONITH: meta quorum fence propose failed".to_string())?;
    } else {
        fencing::set_epoch(epoch);
    }

    if stonith_enabled(cfg) {
        let dir = data_dir.ok_or_else(|| "STONITH lease requires data_dir".to_string())?;
        let ttl = Duration::from_secs(cfg.stonith_lease_secs.max(5));
        write_lease(dir, cfg.node_id, epoch, ttl)?;
    }

    if let Some(catalog) = runtime.router_snapshot() {
        let updated =
            super::failover::apply_failover_to_catalog(&catalog, shard_id, failed_primary, new_primary);
        runtime.replace_catalog(updated);
    }

    Ok(epoch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn lease_roundtrip_and_expiry() {
        let dir = std::env::temp_dir().join(format!(
            "qm_stonith_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let epoch = fencing::current_epoch().saturating_add(1);
        fencing::set_epoch(epoch);
        write_lease(&dir, 7, epoch, Duration::from_secs(60)).unwrap();
        assert!(holds_valid_lease(&dir, 7));
        assert!(!holds_valid_lease(&dir, 8));
        let _ = fs::remove_dir_all(&dir);
    }
}
