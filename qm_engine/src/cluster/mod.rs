/*
 * Cluster Module — Horizontal Scaling Infrastructure
 *
 * Phase 13: Distributed sharding and replication for multi-node deployment.
 */

pub mod replica;
pub mod shard;

pub use replica::{ReplicaSet, ReplicaState, ReplicationConfig};
pub use shard::{ConsistentHashRing, ShardId, ShardManager, VNodeId};
