/*
 * Networked meta Raft — multi-process control plane over inter-node transport.
 *
 * Each data node holds one `MetaRaftNode`. Leader election and log replication
 * use MSG_RAFT_* RPC (blocking + TLS when configured).
 */

use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use super::config::ClusterNodeConfig;
use super::meta_cluster::{
    AppendEntriesRequest, AppendEntriesResponse, MetaCommand, MetaLogEntry, MetaRaftNode,
    RaftRole, VoteRequest, VoteResponse,
};
use super::meta_network::parse_meta_peers;
use super::runtime::ClusterRuntime;
use super::shard_group::ShardGroupCatalog;
use super::transport::NodeClient;

static LOCAL_META: OnceLock<Mutex<MetaRaftNode>> = OnceLock::new();

pub fn local_meta_node() -> &'static Mutex<MetaRaftNode> {
    LOCAL_META.get_or_init(|| Mutex::new(MetaRaftNode::new(1)))
}

pub fn init_local_meta(node_id: u32) {
    let _ = local_meta_node();
    *local_meta_node().lock() = MetaRaftNode::new(node_id);
}

pub fn handle_vote_request(req: VoteRequest) -> VoteResponse {
    local_meta_node().lock().handle_vote_request(req)
}

pub fn handle_append_entries(req: AppendEntriesRequest) -> AppendEntriesResponse {
    local_meta_node().lock().handle_append_entries(req)
}

pub fn catalog_snapshot() -> ShardGroupCatalog {
    local_meta_node().lock().catalog.clone()
}

pub fn is_leader() -> bool {
    local_meta_node().lock().role == RaftRole::Leader
}

pub fn leader_catalog_for_runtime() -> Option<ShardGroupCatalog> {
    let node = local_meta_node().lock();
    if node.catalog.group_for_kind(super::shard_group::ShardGroupKind::Oltp).is_some() {
        Some(node.catalog.clone())
    } else {
        None
    }
}

fn peer_addrs(cfg: &ClusterNodeConfig) -> Vec<SocketAddr> {
    let mut peers = parse_meta_peers();
    if let Some(local) = cfg.local_addr {
        peers.retain(|p| *p != local);
    }
    peers
}

fn quorum_size(total_voters: usize) -> usize {
    total_voters / 2 + 1
}

/// Request votes from configured meta peers; become leader on majority grant.
pub fn try_network_election(cfg: &ClusterNodeConfig) -> bool {
    let peers = peer_addrs(cfg);
    let voters = peers.len() + 1;
    if voters < 2 {
        return false;
    }

    let (term, last_index, last_term, candidate_id) = {
        let mut node = local_meta_node().lock();
        node.node_id = cfg.node_id;
        let term = node.become_leader(&[]);
        (
            term,
            node.log_len(),
            node.tail_log_term(),
            node.node_id,
        )
    };

    let req = VoteRequest {
        term,
        candidate_id,
        last_log_index: last_index,
        last_log_term: last_term,
    };

    let mut votes = 1usize;
    for peer in peers {
        let client = NodeClient::new(0, peer);
        if client
            .send_raft_vote_blocking(&req)
            .map(|resp| resp.vote_granted && resp.term >= term)
            .unwrap_or(false)
        {
            votes += 1;
        }
    }

    let elected = votes >= quorum_size(voters);
    let mut node = local_meta_node().lock();
    if elected {
        node.role = RaftRole::Leader;
        node.current_term = term;
        node.voted_for = Some(candidate_id);
    } else {
        node.role = RaftRole::Follower;
    }
    elected
}

/// Leader proposes an entry and replicates to a majority before returning success.
pub fn propose_with_quorum(cfg: &ClusterNodeConfig, command: MetaCommand) -> Option<MetaLogEntry> {
    let peers = peer_addrs(cfg);
    let entry = {
        let mut node = local_meta_node().lock();
        node.propose(command)?
    };
    let term = entry.term;
    let index = entry.index;
    let prev_index = index.saturating_sub(1);
    let prev_term = if prev_index == 0 { 0 } else { term };

    let mut acks = 1usize;
    let voters = peers.len() + 1;

    for peer in peers {
        let req = AppendEntriesRequest {
            term,
            leader_id: cfg.node_id,
            prev_log_index: prev_index,
            prev_log_term: prev_term,
            entries: vec![entry.clone()],
            leader_commit: index,
        };
        let ok = NodeClient::new(0, peer)
            .send_raft_append_blocking(&req)
            .map(|resp| resp.success && resp.term >= term)
            .unwrap_or(false);
        if ok {
            acks += 1;
        }
    }

    if acks >= quorum_size(voters) {
        Some(entry)
    } else {
        None
    }
}

/// Bootstrap OLTP/vector/search/analytics groups via networked quorum propose.
pub fn bootstrap_catalog_with_network(
    cfg: &ClusterNodeConfig,
    registry: &super::node_registry::ShardEndpointRegistry,
) -> bool {
    if !try_network_election(cfg) {
        return false;
    }
    use super::shard_group::{ShardEndpoint, ShardGroup, ShardGroupKind};
    for kind in ShardGroupKind::all() {
        let group_id = match kind {
            ShardGroupKind::Oltp => 1,
            ShardGroupKind::Vector => 2,
            ShardGroupKind::Search => 3,
            ShardGroupKind::Analytics => 4,
        };
        let mut group = ShardGroup::new(group_id, kind, cfg.shards_per_group);
        for shard_id in 0..cfg.shards_per_group {
            let Some(primary) = registry.primary_for_shard(shard_id) else {
                continue;
            };
            group.endpoints.push(ShardEndpoint {
                shard_id,
                primary,
                replicas: registry.replicas_for_shard(shard_id),
            });
        }
        if propose_with_quorum(cfg, MetaCommand::UpsertShardGroup { group }).is_none() {
            return false;
        }
    }
    true
}

pub fn replicate_catalog_to_runtime(runtime: &ClusterRuntime, cfg: &ClusterNodeConfig) {
    if let Some(cat) = leader_catalog_for_runtime() {
        runtime.replace_catalog(cat);
        return;
    }
    if let Some(cat) = super::meta_cluster::MetaCluster::new(&[cfg.meta_leader_id])
        .catalog_on(cfg.meta_leader_id)
    {
        runtime.replace_catalog(cat);
    }
}

pub fn networked_meta_ready(cfg: &ClusterNodeConfig) -> bool {
    parse_meta_peers().len() >= 1 && cfg.is_active()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_meta_handles_vote_and_append() {
        init_local_meta(1);
        let resp = handle_vote_request(VoteRequest {
            term: 1,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        });
        assert!(resp.vote_granted);

        let append = handle_append_entries(AppendEntriesRequest {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![MetaLogEntry {
                term: 1,
                index: 1,
                command: MetaCommand::Noop,
            }],
            leader_commit: 1,
        });
        assert!(append.success);
    }
}
