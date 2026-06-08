/*
 * Replica Management — Phase 13b (High Availability)
 *
 * Manages data replication across nodes for fault tolerance.
 * Uses a primary-secondary model with configurable consistency levels.
 *
 * Replication modes:
 *   - Sync:  write acknowledged after ALL replicas confirm (strong consistency)
 *   - Async: write acknowledged after primary, replicas catch up in background
 *   - Quorum: write acknowledged after majority confirms (balance)
 */

use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::shard::ShardId;

/// Replication configuration.
#[derive(Clone, Debug)]
pub struct ReplicationConfig {
    /// Number of replicas per shard (including primary).
    pub replication_factor: usize,
    /// Write consistency level.
    pub write_consistency: ConsistencyLevel,
    /// Read consistency level.
    pub read_consistency: ConsistencyLevel,
    /// Heartbeat interval for liveness checking.
    pub heartbeat_interval: Duration,
    /// Maximum lag (in operations) before replica is marked stale.
    pub max_replication_lag: u64,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            replication_factor: 3,
            write_consistency: ConsistencyLevel::Quorum,
            read_consistency: ConsistencyLevel::One,
            heartbeat_interval: Duration::from_secs(5),
            max_replication_lag: 1000,
        }
    }
}

/// Consistency level for reads/writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsistencyLevel {
    /// Succeed after one node confirms.
    One,
    /// Succeed after majority confirms.
    Quorum,
    /// Succeed after all nodes confirm.
    All,
}

/// State of a single replica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplicaState {
    /// Active and caught up.
    Active,
    /// Catching up (replication lag > 0 but within threshold).
    Syncing,
    /// Too far behind or unresponsive.
    Stale,
    /// Permanently removed.
    Removed,
}

/// Per-replica metadata.
#[derive(Clone, Debug)]
pub struct ReplicaInfo {
    pub shard_id: ShardId,
    pub node_id: u32,
    pub state: ReplicaState,
    pub is_primary: bool,
    /// Last confirmed operation sequence number.
    pub last_ack_seq: u64,
    /// Last heartbeat timestamp.
    pub last_heartbeat: Instant,
}

/// A set of replicas for a single shard.
pub struct ReplicaSet {
    shard_id: ShardId,
    replicas: Arc<RwLock<Vec<ReplicaInfo>>>,
    /// Monotonic sequence number for operations on this shard.
    op_seq: AtomicU64,
    config: ReplicationConfig,
}

impl ReplicaSet {
    /// Create a new replica set for a shard.
    pub fn new(shard_id: ShardId, node_ids: &[u32], config: ReplicationConfig) -> Self {
        let replicas: Vec<ReplicaInfo> = node_ids
            .iter()
            .enumerate()
            .map(|(i, &node_id)| ReplicaInfo {
                shard_id,
                node_id,
                state: ReplicaState::Active,
                is_primary: i == 0,
                last_ack_seq: 0,
                last_heartbeat: Instant::now(),
            })
            .collect();

        ReplicaSet {
            shard_id,
            replicas: Arc::new(RwLock::new(replicas)),
            op_seq: AtomicU64::new(0),
            config,
        }
    }

    /// Shard served by this replica set.
    pub fn shard_id(&self) -> ShardId {
        self.shard_id
    }

    /// Get the primary replica's node ID.
    pub fn primary(&self) -> Option<u32> {
        self.replicas
            .read()
            .iter()
            .find(|r| r.is_primary && r.state == ReplicaState::Active)
            .map(|r| r.node_id)
    }

    /// Get all active replica node IDs.
    pub fn active_replicas(&self) -> Vec<u32> {
        self.replicas
            .read()
            .iter()
            .filter(|r| r.state == ReplicaState::Active || r.state == ReplicaState::Syncing)
            .map(|r| r.node_id)
            .collect()
    }

    /// Record a write operation. Returns the new sequence number.
    pub fn record_write(&self) -> u64 {
        // M-07: Use SeqCst for cross-thread visibility of op_seq.
        self.op_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Acknowledge that a replica has caught up to a given sequence.
    pub fn ack_replica(&self, node_id: u32, seq: u64) {
        let mut replicas = self.replicas.write();
        if let Some(r) = replicas.iter_mut().find(|r| r.node_id == node_id) {
            // H-03: Reject backward sequence numbers to prevent regression.
            r.last_ack_seq = r.last_ack_seq.max(seq);
            r.last_heartbeat = Instant::now();
            if r.state == ReplicaState::Syncing || r.state == ReplicaState::Stale {
                let lag = self
                    .op_seq
                    .load(Ordering::SeqCst)
                    .saturating_sub(r.last_ack_seq);
                if lag == 0 {
                    r.state = ReplicaState::Active;
                } else if lag <= self.config.max_replication_lag {
                    r.state = ReplicaState::Syncing;
                }
            }
        }
    }

    /// Check if a write at the given consistency level is satisfied.
    pub fn is_write_satisfied(&self, seq: u64, level: ConsistencyLevel) -> bool {
        let replicas = self.replicas.read();
        let acked = replicas
            .iter()
            .filter(|r| r.last_ack_seq >= seq && r.state != ReplicaState::Removed)
            .count();
        let total = replicas
            .iter()
            .filter(|r| r.state != ReplicaState::Removed)
            .count();

        match level {
            ConsistencyLevel::One => acked >= 1,
            ConsistencyLevel::Quorum => acked > total / 2,
            ConsistencyLevel::All => acked >= total,
        }
    }

    /// Mark heartbeat-stale replicas.
    pub fn check_health(&self) {
        let now = Instant::now();
        let mut replicas = self.replicas.write();
        for r in replicas.iter_mut() {
            if r.state != ReplicaState::Removed {
                let lag = self
                    .op_seq
                    .load(Ordering::Relaxed)
                    .saturating_sub(r.last_ack_seq);
                if now.duration_since(r.last_heartbeat) > self.config.heartbeat_interval * 3 {
                    r.state = ReplicaState::Stale;
                } else if lag > self.config.max_replication_lag {
                    r.state = ReplicaState::Stale;
                }
            }
        }
    }

    /// Promote a secondary to primary (failover).
    pub fn promote(&self, node_id: u32) -> bool {
        let mut replicas = self.replicas.write();
        // Demote current primary
        for r in replicas.iter_mut() {
            if r.is_primary {
                r.is_primary = false;
            }
        }
        // Promote the chosen node
        if let Some(r) = replicas.iter_mut().find(|r| r.node_id == node_id) {
            r.is_primary = true;
            true
        } else {
            false
        }
    }

    /// Get replication lag statistics: (min_lag, max_lag, avg_lag).
    pub fn lag_stats(&self) -> (u64, u64, f64) {
        let current_seq = self.op_seq.load(Ordering::Relaxed);
        let replicas = self.replicas.read();
        let lags: Vec<u64> = replicas
            .iter()
            .filter(|r| r.state != ReplicaState::Removed)
            .map(|r| current_seq.saturating_sub(r.last_ack_seq))
            .collect();

        if lags.is_empty() {
            return (0, 0, 0.0);
        }
        let min = *lags.iter().min().unwrap();
        let max = *lags.iter().max().unwrap();
        let avg = lags.iter().sum::<u64>() as f64 / lags.len() as f64;
        (min, max, avg)
    }

    /// Number of replicas.
    pub fn replica_count(&self) -> usize {
        self.replicas.read().len()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ReplicationConfig {
        ReplicationConfig {
            replication_factor: 3,
            write_consistency: ConsistencyLevel::Quorum,
            read_consistency: ConsistencyLevel::One,
            heartbeat_interval: Duration::from_secs(5),
            max_replication_lag: 100,
        }
    }

    #[test]
    fn test_replica_set_basic() {
        let rs = ReplicaSet::new(0, &[1, 2, 3], default_config());
        assert_eq!(rs.primary(), Some(1));
        assert_eq!(rs.active_replicas().len(), 3);
    }

    #[test]
    fn test_write_quorum() {
        let rs = ReplicaSet::new(0, &[1, 2, 3], default_config());

        let seq = rs.record_write();
        assert_eq!(seq, 1);

        // Not satisfied yet (0/3 acked)
        assert!(!rs.is_write_satisfied(seq, ConsistencyLevel::Quorum));

        // Ack from 2 nodes = quorum (2 > 3/2)
        rs.ack_replica(1, seq);
        rs.ack_replica(2, seq);
        assert!(rs.is_write_satisfied(seq, ConsistencyLevel::Quorum));

        // Not ALL yet
        assert!(!rs.is_write_satisfied(seq, ConsistencyLevel::All));

        rs.ack_replica(3, seq);
        assert!(rs.is_write_satisfied(seq, ConsistencyLevel::All));
    }

    #[test]
    fn test_failover() {
        let rs = ReplicaSet::new(0, &[1, 2, 3], default_config());
        assert_eq!(rs.primary(), Some(1));

        // Promote node 2
        assert!(rs.promote(2));
        assert_eq!(rs.primary(), Some(2));
    }

    #[test]
    fn test_lag_stats() {
        let rs = ReplicaSet::new(0, &[1, 2, 3], default_config());

        // Write 10 ops
        for _ in 0..10 {
            rs.record_write();
        }

        // Ack partially
        rs.ack_replica(1, 10);
        rs.ack_replica(2, 5);
        rs.ack_replica(3, 0);

        let (min, max, _avg) = rs.lag_stats();
        assert_eq!(min, 0); // node 1 fully caught up
        assert_eq!(max, 10); // node 3 has lag of 10
    }
}
