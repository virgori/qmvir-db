//! Live 2-node HA integration — validates enterprise certification gates in-process.
//!
//! Run: cargo test --test cluster_certify_live -- --test-threads=1

use qm_engine::cluster::{
    cluster_metrics, evaluate_certification, execute_routed, find_shard_key_for_shard,
    ClusterNodeConfig, ClusterRuntime, MetaCluster, NodeClient, ShardEndpointRegistry,
    TransportServer, WalApplyTracker,
};
use qm_engine::cluster::two_phase_commit::TwoPhaseParticipant;
use qm_engine::gateway::NativeSqlEngine;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

fn set_env(key: &str, value: &str) {
    unsafe {
        std::env::set_var(key, value);
    }
}

fn write_dev_tls(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use rcgen::{CertificateParams, KeyPair, SanType};
    use std::net::{IpAddr, Ipv4Addr};

    let key_pair = KeyPair::generate().expect("generate dev tls key");
    let mut params =
        CertificateParams::new(vec!["localhost".to_string()]).expect("cert params");
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    let cert = params.self_signed(&key_pair).expect("self-signed cert");
    let cert_path = dir.join("cluster-dev.crt");
    let key_path = dir.join("cluster-dev.key");
    std::fs::write(&cert_path, cert.pem()).expect("write cert");
    std::fs::write(&key_path, key_pair.serialize_pem()).expect("write key");
    (cert_path, key_path)
}

async fn spawn_node(engine: Arc<NativeSqlEngine>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let participant = Arc::new(TwoPhaseParticipant::new(Arc::clone(&engine)));
    let wal_tracker = Arc::new(WalApplyTracker::new());
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let eng = Arc::clone(&engine);
            let part = Arc::clone(&participant);
            let wal = Arc::clone(&wal_tracker);
            tokio::spawn(async move {
                TransportServer::accept_and_serve(stream, eng, part, wal).await;
            });
        }
    });
    (addr, handle)
}

#[tokio::test]
async fn enterprise_certify_live_two_node_cluster() {
    let tls_dir = tempfile::tempdir().expect("tls dir");
    let (cert_path, key_path) = write_dev_tls(tls_dir.path());

    let primary = Arc::new(NativeSqlEngine::new());
    let standby = Arc::new(NativeSqlEngine::new());

    primary
        .execute("CREATE TABLE certify_live (id INTEGER PRIMARY KEY, v TEXT)")
        .expect("ddl primary");
    standby
        .execute("CREATE TABLE certify_live (id INTEGER PRIMARY KEY, v TEXT)")
        .expect("ddl standby");

    let mut registry = ShardEndpointRegistry::new();
    // Placeholder addrs — updated after bind.
    let cluster = MetaCluster::new(&[1]);
    let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
    runtime.enable();

    let (addr_a, _ha) = spawn_node(Arc::clone(&primary)).await;
    let (addr_b, _hb) = spawn_node(Arc::clone(&standby)).await;
    tokio::time::sleep(Duration::from_millis(40)).await;

    registry.insert(0, addr_a);
    registry.insert(1, addr_a);
    registry.insert(2, addr_b);
    registry.insert(3, addr_b);
    registry.insert_replica(0, addr_b);
    registry.insert_replica(2, addr_b);

    let cluster2 = MetaCluster::new(&[1]);
    cluster2.bootstrap_default_groups_with_registry(1, 4, &registry);
    runtime.replace_catalog(cluster2.catalog_on(1).expect("cat"));

    let endpoints = format!(
        "0={addr_a},1={addr_a},2={addr_b},3={addr_b}"
    );
    let replicas = format!("0={addr_b},2={addr_b}");
    set_env("QM_CLUSTER_ENABLE", "1");
    set_env("QM_CLUSTER_TRANSPORT_PORT", &addr_a.port().to_string());
    set_env("QM_CLUSTER_LOCAL_ADDR", &addr_a.to_string());
    set_env("QM_CLUSTER_SHARD_ENDPOINTS", &endpoints);
    set_env("QM_CLUSTER_SHARD_REPLICAS", &replicas);
    set_env("QM_CLUSTER_WAL_REPLICATE", "1");
    set_env("QM_CLUSTER_WAL_SYNC", "1");
    set_env("QM_CLUSTER_WAL_PEERS", &addr_b.to_string());
    set_env("QM_CLUSTER_FAILOVER", "1");
    set_env("QM_CLUSTER_FENCING", "1");
    set_env("QM_CLUSTER_2PC", "1");
    set_env("QM_CLUSTER_META_PEERS", &addr_b.to_string());
    set_env("QM_CLUSTER_TLS_CERT", cert_path.to_str().unwrap());
    set_env("QM_CLUSTER_TLS_KEY", key_path.to_str().unwrap());
    set_env("QM_CLUSTER_TLS_CA", cert_path.to_str().unwrap());

    let cfg = ClusterNodeConfig::from_env();
    assert!(cfg.is_active(), "cluster must be active");

    let tls = qm_engine::cluster::ClusterTlsConfig::from_env();
    assert!(tls.enabled, "TLS must be enabled for enterprise certify");
    assert!(tls.server.is_some(), "TLS server config must load from dev cert");

    let ping_first = addr_b;
    let ping_ok = tokio::task::spawn_blocking(move || {
        NodeClient::new(0, ping_first).ping_blocking(Duration::from_secs(3))
    })
    .await
    .expect("join")
    .expect("ping");
    assert!(ping_ok, "TLS ping to standby must succeed before WAL write");

    let key = find_shard_key_for_shard(4, 0);
    let sql = format!("INSERT INTO certify_live (id, v) VALUES ({key}, 'certified')");
    let runtime_bg = Arc::clone(&runtime);
    let cfg_bg = cfg.clone();
    let primary_bg = Arc::clone(&primary);
    let sql_bg = sql.clone();
    tokio::task::spawn_blocking(move || {
        execute_routed(&runtime_bg, &cfg_bg, &primary_bg, Some(addr_a), &sql_bg)
    })
    .await
    .expect("join")
    .expect("sync wal write");

    let lag = cluster_metrics::wal_lag_lsn();
    assert_eq!(lag, 0, "sync WAL should yield lag_lsn=0 (RPO≈0)");

    let row = standby
        .execute(&format!("SELECT v FROM certify_live WHERE id = {key}"))
        .expect("standby row");
    assert_eq!(row.rows.len(), 1);

    let cfg_eval = cfg.clone();
    let report = tokio::task::spawn_blocking(move || evaluate_certification(&cfg_eval))
        .await
        .expect("join");
    for gate in &report.gates {
        assert!(
            gate.passed,
            "gate {} failed: {}",
            gate.id,
            gate.detail
        );
    }
    assert!(
        report.enterprise_certified,
        "expected enterprise_certified=true, score={}% tier={}",
        report.score_percent,
        report.tier
    );
    assert!(!report.marketing_claims.is_empty());
}

#[tokio::test]
async fn wal_catchup_fills_gap_after_standby_restart_simulation() {
    use qm_engine::cluster::transport::{NodeFrame, MSG_WAL_CATCHUP_REQ, MSG_WAL_CATCHUP_RESP};
    use qm_engine::cluster::wal_buffer;

    // Isolate from TLS env set by the enterprise certify test.
    for key in [
        "QM_CLUSTER_TLS_CERT",
        "QM_CLUSTER_TLS_KEY",
        "QM_CLUSTER_TLS_CA",
    ] {
        unsafe {
            std::env::remove_var(key);
        }
    }

    let engine = Arc::new(NativeSqlEngine::new());
    engine
        .execute("CREATE TABLE catchup (id INTEGER PRIMARY KEY)")
        .expect("ddl");

    let (addr, _h) = spawn_node(Arc::clone(&engine)).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let base = 800_000u64;
    for lsn in base + 1..=base + 3 {
        let sql = format!("INSERT INTO catchup (id) VALUES ({lsn})");
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(sql.as_bytes());
        wal_buffer::record_shipped(&qm_engine::cluster::WalEntry {
            lsn,
            sql,
            checksum: hasher.finalize(),
        });
    }

    #[derive(serde::Serialize)]
    struct WalCatchupReq {
        from_lsn: u64,
    }
    let req = bincode::serialize(&WalCatchupReq { from_lsn: base + 1 }).expect("serialize");
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    NodeFrame::new(MSG_WAL_CATCHUP_REQ, req)
        .write_to(&mut stream)
        .await
        .expect("write");
    let resp = NodeFrame::read_from(&mut stream).await.expect("read");
    assert_eq!(resp.msg_type, MSG_WAL_CATCHUP_RESP);
    let entries: Vec<qm_engine::cluster::WalEntry> =
        bincode::deserialize(&resp.payload).expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].lsn, base + 2);
}
