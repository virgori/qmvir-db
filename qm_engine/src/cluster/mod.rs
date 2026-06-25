/*
 * Cluster Module — Horizontal Scaling Infrastructure
 *
 * Phase 13: Distributed sharding and replication for multi-node deployment.
 *
 * Single-node safety contract:
 *   - `NativeSqlEngine` does not import or call cluster routing.
 *   - HA routing lives in `runtime::ClusterRuntime` and must be explicitly enabled.
 *   - Default builds/runs behave exactly as before (no meta cluster, no remote forward).
 */

pub mod meta_cluster;
pub mod replica;
pub mod router;
pub mod runtime;
pub mod shard;
pub mod shard_group;
pub mod transport;
pub mod two_phase_commit;

pub use meta_cluster::{MetaCluster, MetaCommand, MetaRaftNode, RaftRole};
pub use replica::{ReplicaSet, ReplicaState, ReplicationConfig};
pub use router::{QmRouter, RoutePlan, WorkloadClass};
pub use runtime::{cluster_router_env_enabled, may_forward_to_remote, ClusterRuntime};
pub use shard::{ConsistentHashRing, ShardId, ShardManager, VNodeId};
pub use shard_group::{ShardGroup, ShardGroupCatalog, ShardGroupKind, SharedShardGroupCatalog};
pub use transport::{
    NodeClient, NodeFrame, TransportServer, MSG_COMMIT_REQ, MSG_FORWARD_QUERY, MSG_FORWARD_RESULT,
    MSG_PING, MSG_PONG, MSG_PREPARE_REQ, MSG_WAL_ACK, MSG_WAL_ENTRY, WalEntry,
};
pub use two_phase_commit::{TwoPhaseCoordinator, TwoPhaseParticipant, TxnPhase};
