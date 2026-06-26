/*
 * Shard endpoint registry — maps shard_id → primary SocketAddr for multi-node HA.
 *
 * Env: QM_CLUSTER_SHARD_ENDPOINTS
 *   Format: shard_id=host:port[,shard_id=host:port...]
 *   Also accepts shard_id@host:port
 *
 * Example (2 nodes, 4 shards):
 *   0=127.0.0.1:55441,1=127.0.0.1:55441,2=127.0.0.1:55442,3=127.0.0.1:55442
 */

use std::collections::BTreeMap;
use std::env;
use std::net::SocketAddr;

use super::shard::ShardId;

/// Per-shard primary endpoints replicated across all workload shard groups.
#[derive(Debug, Clone, Default)]
pub struct ShardEndpointRegistry {
    by_shard: BTreeMap<ShardId, SocketAddr>,
    by_replica: BTreeMap<ShardId, Vec<SocketAddr>>,
    default_primary: Option<SocketAddr>,
}

impl ShardEndpointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_primary(addr: SocketAddr) -> Self {
        Self {
            by_shard: BTreeMap::new(),
            by_replica: BTreeMap::new(),
            default_primary: Some(addr),
        }
    }

    pub fn set_default_primary(&mut self, addr: SocketAddr) {
        self.default_primary = Some(addr);
    }

    /// Fill default primary only when no explicit default exists.
    pub fn ensure_default_primary(&mut self, addr: SocketAddr) {
        if self.default_primary.is_none() {
            self.default_primary = Some(addr);
        }
    }

    pub fn insert(&mut self, shard_id: ShardId, addr: SocketAddr) {
        self.by_shard.insert(shard_id, addr);
    }

    pub fn insert_replica(&mut self, shard_id: ShardId, addr: SocketAddr) {
        self.by_replica.entry(shard_id).or_default().push(addr);
    }

    pub fn replicas_for_shard(&self, shard_id: ShardId) -> Vec<SocketAddr> {
        self.by_replica.get(&shard_id).cloned().unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.by_shard.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_shard.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ShardId, SocketAddr)> + '_ {
        self.by_shard.iter().map(|(&id, &addr)| (id, addr))
    }

    pub fn primary_for_shard(&self, shard_id: ShardId) -> Option<SocketAddr> {
        self.by_shard
            .get(&shard_id)
            .copied()
            .or(self.default_primary)
    }

    /// Parse `QM_CLUSTER_SHARD_ENDPOINTS` when set; merges `QM_CLUSTER_SHARD_REPLICAS`.
    pub fn from_env() -> Option<Self> {
        let raw = env::var("QM_CLUSTER_SHARD_ENDPOINTS").ok()?;
        let mut registry = Self::parse(&raw)?;
        if registry.is_empty() {
            return None;
        }
        if let Ok(rep) = env::var("QM_CLUSTER_SHARD_REPLICAS") {
            for (shard_id, addr) in Self::parse_replica_pairs(&rep) {
                registry.insert_replica(shard_id, addr);
            }
        }
        Some(registry)
    }

    fn parse_replica_pairs(raw: &str) -> Vec<(ShardId, SocketAddr)> {
        let mut out = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (shard_s, addr_s) = if let Some((a, b)) = part.split_once('=') {
                (a.trim(), b.trim())
            } else if let Some((a, b)) = part.split_once('@') {
                (a.trim(), b.trim())
            } else {
                continue;
            };
            let Ok(shard_id) = shard_s.parse::<ShardId>() else {
                continue;
            };
            let Ok(addr) = addr_s.parse::<SocketAddr>() else {
                continue;
            };
            out.push((shard_id, addr));
        }
        out
    }

    /// Parse comma-separated shard endpoint map.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut registry = Self::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (shard_s, addr_s) = if let Some((a, b)) = part.split_once('=') {
                (a.trim(), b.trim())
            } else if let Some((a, b)) = part.split_once('@') {
                (a.trim(), b.trim())
            } else {
                continue;
            };
            let shard_id: ShardId = shard_s.parse().ok()?;
            let addr: SocketAddr = addr_s.parse().ok()?;
            registry.insert(shard_id, addr);
        }
        if registry.is_empty() {
            None
        } else {
            Some(registry)
        }
    }
}

/// Brute-force a shard_key that lands on `target_shard` (tests + tooling).
pub fn find_shard_key_for_shard(ring_shards: u32, target_shard: ShardId) -> u64 {
    use super::shard::ConsistentHashRing;
    let ring = ConsistentHashRing::new(ring_shards, 128);
    if let Some(key) = ring.sample_key_for_shard(target_shard) {
        debug_assert_eq!(ring.get_shard(key), Some(target_shard));
        return key;
    }
    for key in 0..1_000_000u64 {
        if ring.get_shard(key) == Some(target_shard) {
            return key;
        }
    }
    panic!("no shard_key maps to shard {target_shard} with {ring_shards} shards");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn localhost(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn parse_shard_endpoint_map() {
        let reg = ShardEndpointRegistry::parse(
            "0=127.0.0.1:55441,1@127.0.0.1:55441,2=127.0.0.1:55442",
        )
        .expect("parse");
        assert_eq!(reg.len(), 3);
        assert_eq!(reg.primary_for_shard(2), Some(localhost(55442)));
        assert_eq!(reg.primary_for_shard(99), None);
    }

    #[test]
    fn default_primary_fallback() {
        let mut reg = ShardEndpointRegistry::with_default_primary(localhost(55440));
        assert_eq!(reg.primary_for_shard(0), Some(localhost(55440)));
        reg.insert(1, localhost(55441));
        assert_eq!(reg.primary_for_shard(1), Some(localhost(55441)));
        assert_eq!(reg.primary_for_shard(2), Some(localhost(55440)));
    }

    #[test]
    fn find_shard_key_targets_shard() {
        let key = find_shard_key_for_shard(4, 2);
        let ring = crate::cluster::ConsistentHashRing::new(4, 128);
        assert_eq!(ring.get_shard(key), Some(2));
    }
}
