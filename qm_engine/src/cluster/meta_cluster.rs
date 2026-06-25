/*
 * QM Meta Cluster — Raft-replicated control plane for shard topology.
 *
 * HA path:
 *   QM Meta Cluster (Raft)  →  QM Router  →  Shard Groups
 *
 * Replicates catalog metadata only. Data durability uses WAL streaming per shard.
 */

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::shard_group::{ShardGroup, ShardGroupCatalog, ShardGroupKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaftRole {
    Follower,
    Candidate,
    Leader,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MetaCommand {
    UpsertShardGroup {
        group: ShardGroup,
    },
    Noop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaLogEntry {
    pub term: u64,
    pub index: u64,
    pub command: MetaCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteRequest {
    pub term: u64,
    pub candidate_id: u32,
    pub last_log_index: u64,
    pub last_log_term: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteResponse {
    pub term: u64,
    pub vote_granted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesRequest {
    pub term: u64,
    pub leader_id: u32,
    pub prev_log_index: u64,
    pub prev_log_term: u64,
    pub entries: Vec<MetaLogEntry>,
    pub leader_commit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesResponse {
    pub term: u64,
    pub success: bool,
}

/// Single Raft peer holding metadata log + materialized catalog.
pub struct MetaRaftNode {
    pub node_id: u32,
    pub role: RaftRole,
    pub current_term: u64,
    pub voted_for: Option<u32>,
    pub log: Vec<MetaLogEntry>,
    pub commit_index: u64,
    pub applied_index: u64,
    pub catalog: ShardGroupCatalog,
}

impl MetaRaftNode {
    pub fn new(node_id: u32) -> Self {
        Self {
            node_id,
            role: RaftRole::Follower,
            current_term: 0,
            voted_for: None,
            log: Vec::new(),
            commit_index: 0,
            applied_index: 0,
            catalog: ShardGroupCatalog::new(),
        }
    }

    fn last_log_index(&self) -> u64 {
        self.log.len() as u64
    }

    fn last_log_term(&self) -> u64 {
        self.log.last().map(|e| e.term).unwrap_or(0)
    }

    fn apply_entry(&mut self, entry: &MetaLogEntry) {
        match &entry.command {
            MetaCommand::UpsertShardGroup { group } => {
                self.catalog.upsert_group(group.clone());
            }
            MetaCommand::Noop => {}
        }
    }

    fn apply_committed(&mut self) {
        while self.commit_index > self.applied_index {
            let idx = self.applied_index as usize;
            let entry = self.log[idx].clone();
            self.apply_entry(&entry);
            self.applied_index += 1;
        }
    }

    pub fn handle_vote_request(&mut self, req: VoteRequest) -> VoteResponse {
        let mut grant = false;
        if req.term > self.current_term {
            self.current_term = req.term;
            self.role = RaftRole::Follower;
            self.voted_for = None;
        }
        if req.term == self.current_term
            && (self.voted_for.is_none() || self.voted_for == Some(req.candidate_id))
        {
            let up_to_date = req.last_log_term > self.last_log_term()
                || (req.last_log_term == self.last_log_term()
                    && req.last_log_index >= self.last_log_index());
            if up_to_date {
                self.voted_for = Some(req.candidate_id);
                grant = true;
            }
        }
        VoteResponse {
            term: self.current_term,
            vote_granted: grant,
        }
    }

    pub fn handle_append_entries(&mut self, req: AppendEntriesRequest) -> AppendEntriesResponse {
        if req.term < self.current_term {
            return AppendEntriesResponse {
                term: self.current_term,
                success: false,
            };
        }
        self.current_term = req.term;
        self.role = RaftRole::Follower;
        self.voted_for = Some(req.leader_id);

        if req.prev_log_index > 0 {
            let prev = self.log.get(req.prev_log_index as usize - 1);
            if prev.map(|e| e.term) != Some(req.prev_log_term) {
                return AppendEntriesResponse {
                    term: self.current_term,
                    success: false,
                };
            }
        }

        let mut next_index = req.prev_log_index as usize;
        for entry in req.entries {
            next_index += 1;
            if self.log.len() >= next_index {
                if self.log[next_index - 1].term != entry.term {
                    self.log.truncate(next_index - 1);
                } else {
                    continue;
                }
            }
            self.log.push(entry);
        }

        if req.leader_commit > self.commit_index {
            self.commit_index = req.leader_commit.min(self.last_log_index());
            self.apply_committed();
        }

        AppendEntriesResponse {
            term: self.current_term,
            success: true,
        }
    }

    pub fn become_leader(&mut self, peers: &[u32]) -> u64 {
        self.role = RaftRole::Leader;
        self.current_term += 1;
        self.voted_for = Some(self.node_id);
        let _ = peers;
        self.current_term
    }

    pub fn propose(&mut self, command: MetaCommand) -> Option<MetaLogEntry> {
        if self.role != RaftRole::Leader {
            return None;
        }
        let index = self.last_log_index() + 1;
        let entry = MetaLogEntry {
            term: self.current_term,
            index,
            command,
        };
        self.log.push(entry.clone());
        self.commit_index = index;
        self.apply_entry(&entry);
        self.applied_index = index;
        Some(entry)
    }
}

/// In-memory 3+ node meta cluster for tests and local dev.
pub struct MetaCluster {
    nodes: Mutex<HashMap<u32, MetaRaftNode>>,
    peers: Vec<u32>,
}

impl MetaCluster {
    pub fn new(node_ids: &[u32]) -> Arc<Self> {
        let mut nodes = HashMap::new();
        for &id in node_ids {
            nodes.insert(id, MetaRaftNode::new(id));
        }
        Arc::new(Self {
            nodes: Mutex::new(nodes),
            peers: node_ids.to_vec(),
        })
    }

    pub fn elect_leader(&self, candidate_id: u32) -> bool {
        let (term, last_index, last_term) = {
            let mut nodes = self.nodes.lock();
            let Some(candidate) = nodes.get_mut(&candidate_id) else {
                return false;
            };
            let term = candidate.become_leader(&self.peers);
            (term, candidate.last_log_index(), candidate.last_log_term())
        };

        let mut votes = 1usize;
        let mut nodes = self.nodes.lock();
        for peer in &self.peers {
            if *peer == candidate_id {
                continue;
            }
            let Some(peer_node) = nodes.get_mut(peer) else {
                continue;
            };
            let resp = peer_node.handle_vote_request(VoteRequest {
                term,
                candidate_id,
                last_log_index: last_index,
                last_log_term: last_term,
            });
            if resp.vote_granted {
                votes += 1;
            }
        }
        let quorum = self.peers.len() / 2 + 1;
        if votes >= quorum {
            for id in &self.peers {
                if let Some(n) = nodes.get_mut(id) {
                    if *id == candidate_id {
                        n.role = RaftRole::Leader;
                    } else {
                        n.role = RaftRole::Follower;
                        n.current_term = term;
                    }
                }
            }
            true
        } else if let Some(candidate) = nodes.get_mut(&candidate_id) {
            candidate.role = RaftRole::Follower;
            false
        } else {
            false
        }
    }

    pub fn replicate_from_leader(&self, leader_id: u32) -> bool {
        let mut nodes = self.nodes.lock();
        let Some(leader) = nodes.get(&leader_id) else {
            return false;
        };
        if leader.role != RaftRole::Leader {
            return false;
        }
        let term = leader.current_term;
        let commit = leader.commit_index;
        let entries = leader.log.clone();

        for peer in &self.peers {
            if *peer == leader_id {
                continue;
            }
            let Some(peer_node) = nodes.get_mut(peer) else {
                continue;
            };
            let follower_len = peer_node.log.len();
            let prev_index = follower_len as u64;
            let prev_term = if follower_len == 0 {
                0
            } else {
                peer_node.log[follower_len - 1].term
            };
            let new_entries = entries[follower_len..].to_vec();
            if new_entries.is_empty() && commit <= peer_node.commit_index {
                continue;
            }
            let resp = peer_node.handle_append_entries(AppendEntriesRequest {
                term,
                leader_id,
                prev_log_index: prev_index,
                prev_log_term: prev_term,
                entries: new_entries,
                leader_commit: commit,
            });
            if !resp.success {
                return false;
            }
        }
        true
    }

    pub fn propose_on_leader(&self, leader_id: u32, command: MetaCommand) -> Option<MetaLogEntry> {
        let entry = {
            let mut nodes = self.nodes.lock();
            let leader = nodes.get_mut(&leader_id)?;
            leader.propose(command)?
        };
        self.replicate_from_leader(leader_id);
        Some(entry)
    }

    pub fn catalog_on(&self, node_id: u32) -> Option<ShardGroupCatalog> {
        self.nodes
            .lock()
            .get(&node_id)
            .map(|n| n.catalog.clone())
    }

    pub fn leader_id(&self) -> Option<u32> {
        self.nodes
            .lock()
            .iter()
            .find(|(_, n)| n.role == RaftRole::Leader)
            .map(|(id, _)| *id)
    }

    pub fn bootstrap_default_groups(&self, leader_id: u32, shards_per_group: u32) -> bool {
        if !self.elect_leader(leader_id) {
            return false;
        }
        for kind in ShardGroupKind::all() {
            let group_id = match kind {
                ShardGroupKind::Oltp => 1,
                ShardGroupKind::Vector => 2,
                ShardGroupKind::Search => 3,
                ShardGroupKind::Analytics => 4,
            };
            let group = ShardGroup::new(group_id, kind, shards_per_group);
            self.propose_on_leader(leader_id, MetaCommand::UpsertShardGroup { group });
        }
        true
    }

    pub fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raft_replicates_shard_groups_to_followers() {
        let cluster = MetaCluster::new(&[1, 2, 3]);
        assert!(cluster.bootstrap_default_groups(1, 8));
        assert_eq!(cluster.leader_id(), Some(1));

        let cat_follower = cluster.catalog_on(2).expect("follower catalog");
        assert!(cat_follower.group_for_kind(ShardGroupKind::Oltp).is_some());
        assert!(cat_follower.group_for_kind(ShardGroupKind::Vector).is_some());
        assert!(cat_follower.group_for_kind(ShardGroupKind::Search).is_some());
        assert!(cat_follower.group_for_kind(ShardGroupKind::Analytics).is_some());
    }
}
