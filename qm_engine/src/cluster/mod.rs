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

pub mod certify;
pub mod chaos;
pub mod cluster_metrics;
pub mod config;
pub mod cross_shard;
pub mod ddl_fanout;
pub mod failover;
pub mod fencing;
pub mod gateway_bridge;
pub mod meta_cluster;
pub mod meta_network;
pub mod meta_raft_network;
pub mod node_registry;
pub mod pg_distributed;
pub mod readiness;
pub mod replica;
pub mod router;
pub mod runtime;
pub mod shard;
pub mod shard_group;
pub mod shard_key;
pub mod stonith;
pub mod tls_config;
pub mod topology;
pub mod transport;
pub mod two_phase_commit;
pub mod wal_apply;
pub mod wal_buffer;
pub mod wal_catchup;
pub mod wal_replication;

#[cfg(test)]
pub(crate) mod test_sync {
    use parking_lot::Mutex;

    /// Serialize in-process TCP cluster integration tests under parallel `cargo test`.
    pub static NETWORK: Mutex<()> = Mutex::new(());
}

pub use config::ClusterNodeConfig;
pub use cross_shard::{
    execute_distributed_batch, is_distributed_batch, parse_distributed_batch, participant_map,
};
pub use ddl_fanout::{is_cluster_ddl, unique_primary_endpoints};
pub use certify::{evaluate_certification, CertificationGate, CertificationReport};
pub use fencing::{accept_epoch, bump_epoch, current_epoch};
pub use meta_raft_network::{
    bootstrap_catalog_with_network, init_local_meta, networked_meta_ready, propose_with_quorum,
};
pub use pg_distributed::{
    clear_connection_id, execute_pg_routed, pg_distributed_enabled, set_connection_id,
};
pub use stonith::{promote_with_fence, require_write_lease, stonith_enabled};
pub use wal_catchup::{collect_catchup_entries, heal_standby_from_primary};
pub use failover::{
    apply_failover_to_catalog, check_and_failover, forward_targets, spawn_failover_loop,
};
pub use cluster_metrics::render_prometheus as render_cluster_metrics;
pub use tls_config::ClusterTlsConfig;
pub use topology::{apply_join, apply_leave, bootstrap_minimal_catalog, join_shard, leave_shard};
pub use shard_key::extract_shard_key;
pub use gateway_bridge::{
    cluster_runtime_from_config, execute_routed, execute_routed_as, prepare_gateway_cluster,
    routed_query_handlers, spawn_transport_server, ClusterGatewayAttach,
};
pub use node_registry::{
    find_shard_key_for_shard, ShardEndpointRegistry,
};
pub use replica::{ReplicaSet, ReplicaState, ReplicationConfig};
pub use wal_apply::{apply_wal_entry, verify_wal_checksum, WalApplyTracker};
pub use wal_replication::{is_replicable_dml, replicate_after_local_write};
pub use meta_network::{
    meta_network_configured, replicate_meta_entries, MetaAppendMsg,
};
pub use meta_cluster::{
    AppendEntriesRequest, AppendEntriesResponse, MetaCluster, MetaCommand, MetaLogEntry,
    MetaRaftNode, RaftRole, VoteRequest, VoteResponse,
};
pub use readiness::{
    collect_peer_addrs, enterprise_tier, evaluate_enterprise_readiness, probe_peers,
    readiness_score_percent, PeerProbe, ReadinessCheck, ReadinessLevel,
};
pub use router::{QmRouter, RoutePlan, WorkloadClass};
pub use runtime::{cluster_router_env_enabled, may_forward_to_remote, ClusterRuntime};
pub use shard::{ConsistentHashRing, ShardId, ShardManager, VNodeId};
pub use shard_group::{ShardGroup, ShardGroupCatalog, ShardGroupKind, SharedShardGroupCatalog};
pub use transport::{
    NodeClient, NodeFrame, TopologyJoinMsg, TopologyLeaveMsg, TransportServer, MSG_COMMIT_REQ,
    MSG_FORWARD_QUERY, MSG_FORWARD_RESULT, MSG_PING, MSG_PONG, MSG_PREPARE_REQ, MSG_WAL_ACK,
    MSG_WAL_ENTRY, WalEntry,
};
pub use two_phase_commit::{TwoPhaseCoordinator, TwoPhaseParticipant, TxnPhase};
