/*
 * Shard Groups — workload-isolated shard pools under QM Router.
 *
 *   OLTP      — transactional SQL / primary row store
 *   Vector    — HNSW / embedding KNN
 *   Search    — FTS / inverted / trigram
 *   Analytics — columnar / OLAP-style scans
 */

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use super::shard::{ConsistentHashRing, ShardId};

/// Workload family routed by QM Router into a shard group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardGroupKind {
    Oltp,
    Vector,
    Search,
    Analytics,
}

impl ShardGroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Oltp => "oltp",
            Self::Vector => "vector",
            Self::Search => "search",
            Self::Analytics => "analytics",
        }
    }

    pub fn all() -> [Self; 4] {
        [
            Self::Oltp,
            Self::Vector,
            Self::Search,
            Self::Analytics,
        ]
    }
}

/// One physical shard inside a group (primary + optional replica endpoints).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShardEndpoint {
    pub shard_id: ShardId,
    pub primary: SocketAddr,
    pub replicas: Vec<SocketAddr>,
}

/// A logical shard group: hash ring + member endpoints replicated via Meta Cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardGroup {
    pub group_id: u32,
    pub kind: ShardGroupKind,
    pub ring_shards: u32,
    pub vnodes_per_shard: usize,
    pub endpoints: Vec<ShardEndpoint>,
}

impl ShardGroup {
    pub fn new(group_id: u32, kind: ShardGroupKind, ring_shards: u32) -> Self {
        Self {
            group_id,
            kind,
            ring_shards,
            vnodes_per_shard: 128,
            endpoints: Vec::new(),
        }
    }

    pub fn ring(&self) -> ConsistentHashRing {
        ConsistentHashRing::new(self.ring_shards, self.vnodes_per_shard)
    }

    pub fn primary_for_shard(&self, shard_id: ShardId) -> Option<SocketAddr> {
        self.endpoints
            .iter()
            .find(|e| e.shard_id == shard_id)
            .map(|e| e.primary)
    }
}

/// Authoritative catalog of shard groups (applied from Meta Cluster Raft log).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ShardGroupCatalog {
    groups: HashMap<u32, ShardGroup>,
    by_kind: HashMap<ShardGroupKind, u32>,
}

impl ShardGroupCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_group(&mut self, group: ShardGroup) {
        self.by_kind.insert(group.kind, group.group_id);
        self.groups.insert(group.group_id, group);
    }

    pub fn group(&self, group_id: u32) -> Option<&ShardGroup> {
        self.groups.get(&group_id)
    }

    pub fn group_for_kind(&self, kind: ShardGroupKind) -> Option<&ShardGroup> {
        self.by_kind
            .get(&kind)
            .and_then(|id| self.groups.get(id))
    }

    pub fn kinds(&self) -> impl Iterator<Item = ShardGroupKind> + '_ {
        self.by_kind.keys().copied()
    }

    /// Promote standby when a shard primary fails (all workload groups).
    pub fn failover_shard(
        &mut self,
        shard_id: ShardId,
        failed_primary: SocketAddr,
        new_primary: SocketAddr,
    ) {
        for g in self.groups.values_mut() {
            for ep in &mut g.endpoints {
                if ep.shard_id == shard_id && ep.primary == failed_primary {
                    ep.replicas.retain(|r| *r != new_primary);
                    if !ep.replicas.contains(&failed_primary) {
                        ep.replicas.push(failed_primary);
                    }
                    ep.primary = new_primary;
                }
            }
        }
    }

    pub fn join_shard(
        &mut self,
        shard_id: ShardId,
        primary: SocketAddr,
        replicas: Vec<SocketAddr>,
    ) {
        for g in self.groups.values_mut() {
            if let Some(ep) = g.endpoints.iter_mut().find(|e| e.shard_id == shard_id) {
                ep.primary = primary;
                ep.replicas = replicas.clone();
            } else {
                g.endpoints.push(ShardEndpoint {
                    shard_id,
                    primary,
                    replicas: replicas.clone(),
                });
            }
        }
    }

    pub fn leave_shard(&mut self, shard_id: ShardId) {
        for g in self.groups.values_mut() {
            g.endpoints.retain(|e| e.shard_id != shard_id);
        }
    }
}

/// Thread-safe view used by QM Router on data nodes.
pub struct SharedShardGroupCatalog {
    inner: RwLock<ShardGroupCatalog>,
}

impl SharedShardGroupCatalog {
    pub fn new(catalog: ShardGroupCatalog) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(catalog),
        })
    }

    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, ShardGroupCatalog> {
        self.inner.read()
    }

    pub fn replace(&self, catalog: ShardGroupCatalog) {
        *self.inner.write() = catalog;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn catalog_routes_by_kind() {
        let mut cat = ShardGroupCatalog::new();
        let mut g = ShardGroup::new(1, ShardGroupKind::Oltp, 4);
        g.endpoints.push(ShardEndpoint {
            shard_id: 0,
            primary: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 55001),
            replicas: vec![],
        });
        cat.upsert_group(g);
        assert!(cat.group_for_kind(ShardGroupKind::Oltp).is_some());
        assert!(cat.group_for_kind(ShardGroupKind::Vector).is_none());
    }
}
