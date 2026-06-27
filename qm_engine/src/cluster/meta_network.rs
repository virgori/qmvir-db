/*
 * Phase M — networked meta catalog replication over inter-node transport.
 *
 * Env: QM_CLUSTER_META_PEERS=host:port[,host:port...]
 */

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use super::config::ClusterNodeConfig;
use super::meta_cluster::MetaLogEntry;
use super::runtime::ClusterRuntime;
use super::shard_group::ShardGroupCatalog;
use super::transport::NodeClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaAppendMsg {
    pub leader_id: u32,
    pub entries: Vec<MetaLogEntry>,
}

pub fn parse_meta_peers() -> Vec<SocketAddr> {
    std::env::var("QM_CLUSTER_META_PEERS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn meta_network_configured() -> bool {
    !parse_meta_peers().is_empty()
}

/// Apply replicated meta log entries to local catalog view.
pub fn apply_meta_entries(entries: &[MetaLogEntry]) -> ShardGroupCatalog {
    use super::meta_cluster::MetaCommand;
    let mut catalog = ShardGroupCatalog::new();
    for entry in entries {
        if let MetaCommand::UpsertShardGroup { group } = &entry.command {
            catalog.upsert_group(group.clone());
        }
        if let MetaCommand::FencePrimary { epoch, .. } = &entry.command {
            super::fencing::set_epoch(*epoch);
        }
    }
    catalog
}

pub fn apply_meta_to_runtime(runtime: &ClusterRuntime, entries: &[MetaLogEntry]) {
    if entries.is_empty() {
        return;
    }
    let cat = apply_meta_entries(entries);
    if cat
        .group_for_kind(super::shard_group::ShardGroupKind::Oltp)
        .is_some()
    {
        runtime.replace_catalog(cat);
    }
}

/// Ship catalog entries to configured meta peers (best-effort).
pub fn replicate_meta_entries(cfg: &ClusterNodeConfig, entries: Vec<MetaLogEntry>) {
    if entries.is_empty() {
        return;
    }
    let peers = parse_meta_peers();
    if peers.is_empty() {
        return;
    }
    let msg = MetaAppendMsg {
        leader_id: cfg.meta_leader_id,
        entries,
    };
    let payload = match bincode::serialize(&msg) {
        Ok(p) => p,
        Err(_) => return,
    };
    for peer in peers {
        if cfg.local_addr.is_some_and(|l| l == peer) {
            continue;
        }
        let peer = peer;
        let body = payload.clone();
        std::thread::spawn(move || {
            let client = NodeClient::new(0, peer);
            let _ = client.send_meta_append_blocking(&body);
        });
    }
}
