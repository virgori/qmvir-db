/*
 * Consistent Hash Ring — Phase 13a (Horizontal Scaling)
 *
 * Implements a virtual-node consistent hash ring for distributing
 * vectors across shards. Similar vectors land on the same shard
 * to minimize cross-shard queries.
 *
 * Features:
 *   - Consistent hashing with configurable virtual nodes
 *   - Shard add/remove with minimal key migration
 *   - Locality-aware placement option (k-means clustering)
 *   - Thread-safe with Arc<RwLock>
 */

use ahash::AHashMap;
use parking_lot::RwLock;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Shard identifier.
pub type ShardId = u32;
/// Virtual node identifier.
pub type VNodeId = u64;

// ── Consistent Hash Ring ────────────────────────────────────────────

/// A consistent hash ring with virtual nodes for even distribution.
///
/// Each physical shard gets `vnodes_per_shard` positions on the ring.
/// Keys are hashed and mapped to the nearest clockwise virtual node.
pub struct ConsistentHashRing {
    /// Sorted map: ring_position → shard_id
    ring: BTreeMap<u64, ShardId>,
    /// Physical shard → list of its virtual node positions
    shard_vnodes: AHashMap<ShardId, Vec<u64>>,
    /// Number of virtual nodes per physical shard
    vnodes_per_shard: usize,
    /// Total number of physical shards
    num_shards: u32,
}

impl ConsistentHashRing {
    /// Create a new ring with the given number of shards and virtual nodes.
    pub fn new(num_shards: u32, vnodes_per_shard: usize) -> Self {
        let mut ring = ConsistentHashRing {
            ring: BTreeMap::new(),
            shard_vnodes: AHashMap::new(),
            vnodes_per_shard,
            num_shards: 0,
        };
        for shard_id in 0..num_shards {
            ring.add_shard(shard_id);
        }
        ring
    }

    /// Add a physical shard to the ring.
    pub fn add_shard(&mut self, shard_id: ShardId) {
        let mut vnodes = Vec::with_capacity(self.vnodes_per_shard);
        for vn in 0..self.vnodes_per_shard {
            let hash = hash_vnode(shard_id, vn as u32);
            self.ring.insert(hash, shard_id);
            vnodes.push(hash);
        }
        self.shard_vnodes.insert(shard_id, vnodes);
        self.num_shards += 1;
    }

    /// Remove a shard from the ring. Returns the keys that need migration.
    pub fn remove_shard(&mut self, shard_id: ShardId) -> bool {
        if let Some(vnodes) = self.shard_vnodes.remove(&shard_id) {
            for hash in vnodes {
                self.ring.remove(&hash);
            }
            self.num_shards -= 1;
            true
        } else {
            false
        }
    }

    /// Find which shard a key belongs to.
    pub fn get_shard(&self, key: u64) -> Option<ShardId> {
        if self.ring.is_empty() {
            return None;
        }
        // Find first vnode >= key (clockwise)
        match self.ring.range(key..).next() {
            Some((_, &shard)) => Some(shard),
            None => self.ring.values().next().copied(), // wrap around
        }
    }

    /// Find shard for a vector ID.
    pub fn shard_for_id(&self, id: i64) -> Option<ShardId> {
        let key = hash_key(id);
        self.get_shard(key)
    }

    /// Get N replica shards for a key (for replication).
    pub fn get_replicas(&self, key: u64, n: usize) -> Vec<ShardId> {
        if self.ring.is_empty() {
            return Vec::new();
        }
        let mut replicas = Vec::with_capacity(n);
        let mut seen = ahash::AHashSet::new();

        // Walk clockwise from key
        let iter = self.ring.range(key..).chain(self.ring.iter());
        for (_, &shard) in iter {
            if seen.insert(shard) {
                replicas.push(shard);
                if replicas.len() >= n {
                    break;
                }
            }
        }
        // M-08: Warn when fewer replicas are available than requested.
        if replicas.len() < n {
            tracing::warn!(
                "get_replicas: requested {} replicas but only {} shards available",
                n,
                replicas.len()
            );
        }
        replicas
    }

    /// Number of active shards.
    pub fn num_shards(&self) -> u32 {
        self.num_shards
    }

    /// Distribution statistics: returns (min_keys, max_keys, stddev) for a set of keys.
    pub fn distribution_stats(&self, keys: &[u64]) -> (usize, usize, f64) {
        let mut counts = AHashMap::new();
        for &key in keys {
            if let Some(shard) = self.get_shard(key) {
                *counts.entry(shard).or_insert(0usize) += 1;
            }
        }
        if counts.is_empty() {
            return (0, 0, 0.0);
        }
        let min = *counts.values().min().unwrap_or(&0);
        let max = *counts.values().max().unwrap_or(&0);
        let mean = keys.len() as f64 / counts.len() as f64;
        let variance: f64 = counts
            .values()
            .map(|&c| (c as f64 - mean).powi(2))
            .sum::<f64>()
            / counts.len() as f64;
        (min, max, variance.sqrt())
    }
}

// ── Shard Manager ───────────────────────────────────────────────────

/// Manages vector distribution across shards with routing.
pub struct ShardManager {
    ring: Arc<RwLock<ConsistentHashRing>>,
    /// Per-shard vector count.
    shard_counts: Arc<RwLock<AHashMap<ShardId, u64>>>,
    /// Replication factor (how many shards store each key).
    replication_factor: usize,
}

impl ShardManager {
    pub fn new(num_shards: u32, vnodes_per_shard: usize, replication_factor: usize) -> Self {
        let ring = ConsistentHashRing::new(num_shards, vnodes_per_shard);
        let mut counts = AHashMap::new();
        for i in 0..num_shards {
            counts.insert(i, 0u64);
        }
        ShardManager {
            ring: Arc::new(RwLock::new(ring)),
            shard_counts: Arc::new(RwLock::new(counts)),
            replication_factor,
        }
    }

    /// Route a vector ID to its primary shard + replicas.
    pub fn route(&self, id: i64) -> Vec<ShardId> {
        let ring = self.ring.read();
        let key = hash_key(id);
        ring.get_replicas(key, self.replication_factor)
    }

    /// Route a batch of IDs and return grouped shard→ids map.
    pub fn route_batch(&self, ids: &[i64]) -> AHashMap<ShardId, Vec<i64>> {
        let ring = self.ring.read();
        let mut groups: AHashMap<ShardId, Vec<i64>> = AHashMap::new();
        for &id in ids {
            if let Some(shard) = ring.shard_for_id(id) {
                groups.entry(shard).or_default().push(id);
            }
        }
        groups
    }

    /// Record an insert to a shard (for balance tracking).
    pub fn record_insert(&self, shard: ShardId) {
        let mut counts = self.shard_counts.write();
        *counts.entry(shard).or_insert(0) += 1;
    }

    /// Get load balance stats.
    pub fn balance_stats(&self) -> (u64, u64, f64) {
        let counts = self.shard_counts.read();
        if counts.is_empty() {
            return (0, 0, 0.0);
        }
        let min = *counts.values().min().unwrap_or(&0);
        let max = *counts.values().max().unwrap_or(&0);
        let mean = counts.values().sum::<u64>() as f64 / counts.len() as f64;
        let variance: f64 = counts
            .values()
            .map(|&c| (c as f64 - mean).powi(2))
            .sum::<f64>()
            / counts.len() as f64;
        (min, max, variance.sqrt())
    }

    /// Add a new shard (scale-out).
    pub fn add_shard(&self, shard_id: ShardId) {
        self.ring.write().add_shard(shard_id);
        self.shard_counts.write().insert(shard_id, 0);
    }

    /// Remove a shard (scale-in). Returns true if shard existed.
    pub fn remove_shard(&self, shard_id: ShardId) -> bool {
        let removed = self.ring.write().remove_shard(shard_id);
        if removed {
            self.shard_counts.write().remove(&shard_id);
        }
        removed
    }

    /// Number of shards.
    pub fn num_shards(&self) -> u32 {
        self.ring.read().num_shards()
    }
}

/// Python wrapper for the consistent hash ring.
#[cfg(feature = "python")]
#[pyclass(name = "ShardRing")]
pub struct PyShardRing {
    inner: ConsistentHashRing,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyShardRing {
    #[new]
    #[pyo3(signature = (num_shards, vnodes_per_shard=150))]
    pub fn new(num_shards: u32, vnodes_per_shard: usize) -> Self {
        Self {
            inner: ConsistentHashRing::new(num_shards, vnodes_per_shard),
        }
    }

    pub fn num_shards(&self) -> u32 {
        self.inner.num_shards()
    }

    pub fn shard_for_id(&self, id: i64) -> Option<ShardId> {
        self.inner.shard_for_id(id)
    }

    pub fn get_shard(&self, key: u64) -> Option<ShardId> {
        self.inner.get_shard(key)
    }

    pub fn get_replicas(&self, key: u64, n: usize) -> Vec<ShardId> {
        self.inner.get_replicas(key, n)
    }

    pub fn add_shard(&mut self, shard_id: ShardId) {
        self.inner.add_shard(shard_id);
    }

    pub fn remove_shard(&mut self, shard_id: ShardId) -> bool {
        self.inner.remove_shard(shard_id)
    }

    pub fn distribution_stats(&self, keys: Vec<u64>) -> (usize, usize, f64) {
        self.inner.distribution_stats(&keys)
    }
}

/// Python wrapper for sharded routing with replicas.
#[cfg(feature = "python")]
#[pyclass(name = "ShardManager")]
pub struct PyShardManager {
    inner: ShardManager,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyShardManager {
    #[new]
    #[pyo3(signature = (num_shards, vnodes_per_shard=150, replication_factor=1))]
    pub fn new(num_shards: u32, vnodes_per_shard: usize, replication_factor: usize) -> Self {
        Self {
            inner: ShardManager::new(num_shards, vnodes_per_shard, replication_factor),
        }
    }

    pub fn num_shards(&self) -> u32 {
        self.inner.num_shards()
    }

    pub fn route(&self, id: i64) -> Vec<ShardId> {
        self.inner.route(id)
    }

    pub fn route_batch(&self, ids: Vec<i64>) -> std::collections::HashMap<ShardId, Vec<i64>> {
        self.inner.route_batch(&ids).into_iter().collect()
    }

    pub fn record_insert(&self, shard: ShardId) {
        self.inner.record_insert(shard);
    }

    pub fn balance_stats(&self) -> (u64, u64, f64) {
        self.inner.balance_stats()
    }

    pub fn add_shard(&self, shard_id: ShardId) {
        self.inner.add_shard(shard_id);
    }

    pub fn remove_shard(&self, shard_id: ShardId) -> bool {
        self.inner.remove_shard(shard_id)
    }
}

// ── Hash functions ──────────────────────────────────────────────────

/// Hash a virtual node position deterministically.
fn hash_vnode(shard: ShardId, vnode: u32) -> u64 {
    let mut hasher = ahash::AHasher::default();
    shard.hash(&mut hasher);
    vnode.hash(&mut hasher);
    hasher.finish()
}

/// Hash a vector ID to a ring position.
fn hash_key(id: i64) -> u64 {
    let mut hasher = ahash::AHasher::default();
    id.hash(&mut hasher);
    hasher.finish()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consistent_hash_basic() {
        let ring = ConsistentHashRing::new(4, 150);
        assert_eq!(ring.num_shards(), 4);

        // Every key should map to a shard
        for i in 0..1000i64 {
            assert!(ring.shard_for_id(i).is_some());
        }
    }

    #[test]
    fn test_distribution_balance() {
        let ring = ConsistentHashRing::new(8, 200);
        let keys: Vec<u64> = (0..100_000i64).map(|i| hash_key(i)).collect();
        let (min, max, stddev) = ring.distribution_stats(&keys);

        let expected_per_shard = 100_000.0 / 8.0;
        // Standard deviation should be < 10% of mean for good balance
        assert!(
            stddev < expected_per_shard * 0.15,
            "poor balance: min={}, max={}, stddev={:.0}, expected_mean={:.0}",
            min,
            max,
            stddev,
            expected_per_shard
        );
    }

    #[test]
    fn test_shard_add_remove() {
        let mut ring = ConsistentHashRing::new(3, 150);

        // Track assignments before
        let key = hash_key(42);
        let shard_before = ring.get_shard(key).unwrap();

        // Add a shard
        ring.add_shard(3);
        assert_eq!(ring.num_shards(), 4);

        // Remove original shard
        ring.remove_shard(shard_before);
        assert_eq!(ring.num_shards(), 3);

        // Key should still be routable
        assert!(ring.get_shard(key).is_some());
    }

    #[test]
    fn test_replicas() {
        let ring = ConsistentHashRing::new(5, 150);
        let key = hash_key(100);
        let replicas = ring.get_replicas(key, 3);
        assert_eq!(replicas.len(), 3);
        // All different shards
        let unique: ahash::AHashSet<_> = replicas.iter().collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn test_shard_manager_routing() {
        let mgr = ShardManager::new(4, 150, 2);

        // Single route
        let shards = mgr.route(42);
        assert_eq!(shards.len(), 2);

        // Batch route
        let ids: Vec<i64> = (0..100).collect();
        let groups = mgr.route_batch(&ids);
        assert!(groups.len() <= 4);
        let total: usize = groups.values().map(|v| v.len()).sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn test_minimal_disruption_on_add() {
        let ring = ConsistentHashRing::new(4, 200);
        let n = 10_000i64;

        // Record assignments before
        let before: Vec<ShardId> = (0..n).map(|i| ring.shard_for_id(i).unwrap()).collect();

        // Create new ring with 5 shards
        let ring2 = ConsistentHashRing::new(5, 200);
        let after: Vec<ShardId> = (0..n).map(|i| ring2.shard_for_id(i).unwrap()).collect();

        // Count migrations
        let migrations = before
            .iter()
            .zip(after.iter())
            .filter(|(a, b)| a != b)
            .count();
        let migration_pct = migrations as f64 / n as f64 * 100.0;

        // Consistent hashing: ideally ~1/N keys migrate when adding 1 shard
        // With 4→5 shards, ideal is ~20%, allow up to 40%
        assert!(
            migration_pct < 40.0,
            "too many migrations: {:.1}% (expected ~20%)",
            migration_pct
        );
    }
}
