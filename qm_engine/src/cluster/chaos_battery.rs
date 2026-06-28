/*
 * In-process chaos battery — runnable from `qm cluster certify --chaos`
 * and from lib integration tests (no cargo subprocess required).
 */

use std::sync::Arc;
use std::time::Instant;

use super::cluster_metrics;
use super::config::ClusterNodeConfig;
use super::failover::check_and_failover;
use super::gateway_bridge::execute_routed;
use super::meta_cluster::MetaCluster;
use super::node_registry::{find_shard_key_for_shard, ShardEndpointRegistry};
use super::runtime::ClusterRuntime;
use super::transport::TransportServer;
use super::two_phase_commit::TwoPhaseParticipant;
use super::wal_apply::WalApplyTracker;
use super::witness;
use super::replica::ConsistencyLevel;
use crate::gateway::native_sql::NativeSqlEngine;

#[derive(Debug, Clone)]
pub struct ChaosScenarioResult {
    pub id: &'static str,
    pub title: &'static str,
    pub passed: bool,
    pub detail: String,
    pub duration_ms: u128,
}

fn scenario(id: &'static str, title: &'static str, passed: bool, detail: String, start: Instant) -> ChaosScenarioResult {
    ChaosScenarioResult {
        id,
        title,
        passed,
        detail,
        duration_ms: start.elapsed().as_millis(),
    }
}

async fn spawn_transport(
    engine: Arc<NativeSqlEngine>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let local = listener.local_addr().expect("addr");
    let participant = Arc::new(TwoPhaseParticipant::new(Arc::clone(&engine)));
    let wal_tracker = Arc::new(WalApplyTracker::new());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = ready_tx.send(());
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let eng = Arc::clone(&engine);
            let part = Arc::clone(&participant);
            let wal = Arc::clone(&wal_tracker);
            tokio::spawn(async move {
                let _ = TransportServer::serve_connection_shared(stream, eng, part, wal).await;
            });
        }
    });
    ready_rx.await.expect("ready");
    (local, handle)
}

use std::sync::LazyLock;
use parking_lot::Mutex;

static CHAOS_SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Run all chaos scenarios synchronously (uses internal tokio runtime).
pub fn run_chaos_battery() -> Vec<ChaosScenarioResult> {
    let _guard = CHAOS_SERIAL.lock();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");

    vec![
        rt.block_on(scenario_sync_wal_rpo_zero()),
        rt.block_on(scenario_failover_partition()),
        scenario_fencing_stale_epoch(),
        scenario_wal_lsn_dedupe(),
        scenario_write_quorum_math(),
        scenario_witness_quorum_math(),
        scenario_failover_rto_metric(),
    ]
}

async fn scenario_sync_wal_rpo_zero() -> ChaosScenarioResult {
    let start = Instant::now();
    let primary = Arc::new(NativeSqlEngine::new());
    let standby = Arc::new(NativeSqlEngine::new());
    let (addr_p, _hp) = spawn_transport(Arc::clone(&primary)).await;
    let (addr_s, _hs) = spawn_transport(Arc::clone(&standby)).await;

    let _ = primary.execute("CREATE TABLE chaos_bat (id INTEGER PRIMARY KEY, v TEXT)");
    let _ = standby.execute("CREATE TABLE chaos_bat (id INTEGER PRIMARY KEY, v TEXT)");

    let mut registry = ShardEndpointRegistry::with_default_primary(addr_p);
    registry.insert_replica(0, addr_s);
    let cluster = MetaCluster::new(&[1]);
    cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
    let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
    runtime.enable();

    let cfg = ClusterNodeConfig {
        enabled: true,
        local_addr: Some(addr_p),
        wal_replicate: true,
        wal_sync: true,
        wal_peers: vec![addr_s],
        ..ClusterNodeConfig::default()
    };

    let key = find_shard_key_for_shard(4, 0);
    let sql = format!("INSERT INTO chaos_bat (id, v) VALUES ({key}, 'bat')");
    let ok = execute_routed(&runtime, &cfg, &primary, Some(addr_p), &sql).is_ok();
    let lag = cluster_metrics::wal_lag_lsn();
    let passed = ok && lag == 0;
    scenario(
        "sync_wal_rpo0",
        "Sync WAL RPO≈0 after commit",
        passed,
        format!("ok={ok} lag_lsn={lag}"),
        start,
    )
}

async fn scenario_failover_partition() -> ChaosScenarioResult {
    let start = Instant::now();
    let node_a = Arc::new(NativeSqlEngine::new());
    let node_b = Arc::new(NativeSqlEngine::new());
    let (addr_a, ha) = spawn_transport(Arc::clone(&node_a)).await;
    let (addr_b, _hb) = spawn_transport(Arc::clone(&node_b)).await;

    let mut registry = ShardEndpointRegistry::new();
    registry.insert(0, addr_a);
    registry.insert_replica(0, addr_b);
    let cluster = MetaCluster::new(&[1]);
    cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
    let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
    runtime.enable();

    ha.abort();

    let cfg = ClusterNodeConfig {
        enabled: true,
        local_addr: Some(addr_b),
        failover_enabled: true,
        wal_peers: vec![addr_b],
        ..ClusterNodeConfig::default()
    };

    let t0 = Instant::now();
    let promoted = check_and_failover(&runtime, &cfg, None);
    let rto_ms = t0.elapsed().as_millis();
    let passed = promoted >= 1 && rto_ms < 10_000;
    scenario(
        "failover_partition",
        "Promote standby after primary partition",
        passed,
        format!("promoted={promoted} rto_ms={rto_ms}"),
        start,
    )
}

fn scenario_fencing_stale_epoch() -> ChaosScenarioResult {
    let start = Instant::now();
    let cur = super::fencing::current_epoch();
    super::fencing::bump_epoch();
    let passed = !super::fencing::accept_epoch(cur, true);
    scenario(
        "fencing_stale_epoch",
        "Epoch fencing rejects stale writer",
        passed,
        format!("old_epoch={cur} current={}", super::fencing::current_epoch()),
        start,
    )
}

fn scenario_wal_lsn_dedupe() -> ChaosScenarioResult {
    let start = Instant::now();
    use super::transport::WalEntry;
    let tracker = WalApplyTracker::new();
    let entry = WalEntry {
        lsn: 42,
        sql: "INSERT INTO t VALUES (1);".into(),
        checksum: 0,
    };
    let first = tracker.claim_lsn(entry.lsn);
    let dup = tracker.claim_lsn(entry.lsn);
    let passed = first && !dup;
    scenario(
        "wal_lsn_dedupe",
        "Duplicate WAL LSN rejected on standby",
        passed,
        format!("first={first} duplicate={dup}"),
        start,
    )
}

fn scenario_write_quorum_math() -> ChaosScenarioResult {
    let start = Instant::now();
    let n3 = super::wal_replication::required_ack_count(3, ConsistencyLevel::Quorum);
    let n5 = super::wal_replication::required_ack_count(5, ConsistencyLevel::Quorum);
    let passed = n3 == 2 && n5 == 3;
    scenario(
        "write_quorum_math",
        "WAL write quorum W=⌊N/2⌋+1",
        passed,
        format!("N=3 W={n3} N=5 W={n5}"),
        start,
    )
}

fn scenario_witness_quorum_math() -> ChaosScenarioResult {
    let start = Instant::now();
    // 2 data nodes + 1 witness = 3 voters → need 2 grants
    let need = witness::quorum_size(3);
    let passed = need == 2;
    scenario(
        "witness_quorum",
        "2-DC + witness requires majority (2/3)",
        passed,
        format!("voters=3 required={need}"),
        start,
    )
}

fn scenario_failover_rto_metric() -> ChaosScenarioResult {
    let start = Instant::now();
    cluster_metrics::record_failover_rto_ms(250);
    cluster_metrics::record_wal_lag_sample(0);
    cluster_metrics::record_wal_lag_sample(5);
    cluster_metrics::record_wal_lag_sample(50);
    let p99 = cluster_metrics::wal_lag_p99_ms();
    let passed = p99 >= 5 && cluster_metrics::failover_rto_last_ms() == 250;
    scenario(
        "sla_metrics",
        "RTO + WAL lag p99 metrics recorded",
        passed,
        format!("rto_last_ms=250 lag_p99_ms={p99}"),
        start,
    )
}

pub fn all_passed(results: &[ChaosScenarioResult]) -> bool {
    results.iter().all(|r| r.passed)
}
