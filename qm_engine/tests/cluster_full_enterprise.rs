//! Production multi-DC full enterprise integration tests.

use qm_engine::cluster::{
    meta_raft_network, pg_distributed, stonith, wal_catchup, AppendEntriesRequest, ClusterNodeConfig,
    MetaCommand, MetaLogEntry, VoteRequest,
};
use qm_engine::gateway::native_sql::NativeSqlEngine;

#[test]
fn meta_raft_local_vote_and_append_handlers() {
    meta_raft_network::init_local_meta(1);
    let resp = meta_raft_network::handle_vote_request(VoteRequest {
        term: 2,
        candidate_id: 2,
        last_log_index: 0,
        last_log_term: 0,
    });
    assert!(resp.vote_granted);

    let append = meta_raft_network::handle_append_entries(AppendEntriesRequest {
        term: 2,
        leader_id: 2,
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![MetaLogEntry {
            term: 2,
            index: 1,
            command: MetaCommand::Noop,
        }],
        leader_commit: 1,
    });
    assert!(append.success);
}

#[test]
fn stonith_lease_blocks_without_file() {
    let cfg = ClusterNodeConfig {
        stonith_enabled: true,
        ..ClusterNodeConfig::default()
    };
    let dir = std::env::temp_dir().join(format!(
        "qm_stonith_req_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(stonith::require_write_lease(Some(&dir), &cfg).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_wal_catchup_merges_with_ring() {
    use qm_engine::cluster::transport::WalEntry;
    use qm_engine::cluster::wal_buffer;

    let dir = std::env::temp_dir().join(format!(
        "qm_catchup_merge_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("native_sql.wal"),
        "INSERT INTO t VALUES (1);\nINSERT INTO t VALUES (2);\n",
    )
    .unwrap();
    wal_buffer::record_shipped(&WalEntry {
        lsn: 100,
        sql: "INSERT INTO t VALUES (100);".into(),
        checksum: 0,
    });
    let merged = wal_catchup::collect_catchup_entries(Some(&dir), 1);
    assert!(merged.len() >= 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pg_distributed_flag_requires_2pc() {
    let cfg = ClusterNodeConfig {
        pg_distributed_enabled: true,
        two_pc_enabled: false,
        ..Default::default()
    };
    assert!(!pg_distributed::pg_distributed_enabled(&cfg));
    let cfg2 = ClusterNodeConfig {
        pg_distributed_enabled: true,
        two_pc_enabled: true,
        ..Default::default()
    };
    assert!(pg_distributed::pg_distributed_enabled(&cfg2));
}

#[test]
fn chaos_battery_all_scenarios_pass() {
    let results = qm_engine::cluster::run_chaos_battery();
    assert!(results.len() >= 11, "expected >=11 chaos scenarios, got {}", results.len());
    for r in &results {
        assert!(r.passed, "scenario {} failed: {}", r.id, r.detail);
    }
}

#[test]
fn witness_quorum_three_voters() {
    use qm_engine::cluster::witness;
    assert_eq!(witness::quorum_size(3), 2);
}

#[test]
fn single_node_engine_unaffected_without_cluster_env() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE ent_iso (id INTEGER PRIMARY KEY)")
        .expect("ddl");
    assert!(engine
        .execute("INSERT INTO ent_iso (id) VALUES (1)")
        .is_ok());
}
