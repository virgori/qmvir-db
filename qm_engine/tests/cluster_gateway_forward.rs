//! Gateway cluster forward: SQL routed to remote transport node.

use qm_engine::cluster::{
    cluster_runtime_from_config, execute_routed, find_shard_key_for_shard, ClusterNodeConfig,
    ClusterRuntime, MetaCluster, ShardEndpointRegistry, TransportServer,
};
use qm_engine::gateway::NativeSqlEngine;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

fn idle_cfg() -> ClusterNodeConfig {
    ClusterNodeConfig::default()
}

#[tokio::test]
async fn gateway_forward_executes_on_remote_engine() {
    let remote = Arc::new(NativeSqlEngine::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let serve_engine = Arc::clone(&remote);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let eng = Arc::clone(&serve_engine);
            tokio::spawn(async move {
                let _ = TransportServer::serve_connection(stream, eng).await;
            });
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    let cfg = ClusterNodeConfig {
        enabled: true,
        meta_leader_id: 1,
        transport_port: Some(addr.port()),
        local_addr: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 59999)),
        bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ..ClusterNodeConfig::default()
    };
    let runtime = cluster_runtime_from_config(&cfg).expect("runtime");
    assert!(runtime.is_active());

    let local = Arc::new(NativeSqlEngine::new());
    let sql = "CREATE TABLE gw_fwd (id INTEGER PRIMARY KEY, v TEXT)".to_string();
    let runtime_bg = Arc::clone(&runtime);
    let cfg_bg = cfg.clone();
    let local_bg = Arc::clone(&local);
    let local_addr = cfg.local_addr;
    tokio::task::spawn_blocking(move || {
        execute_routed(&runtime_bg, &cfg_bg, &local_bg, local_addr, &sql)
    })
    .await
    .expect("join")
    .expect("forward create");

    let rows = remote
        .execute("SELECT COUNT(*) FROM gw_fwd")
        .expect("remote count");
    assert_eq!(rows.rows.len(), 1);
}

#[test]
fn local_addr_match_skips_forward_loop() {
    let engine = Arc::new(NativeSqlEngine::new());
    let rt = ClusterRuntime::disabled();
    let sql = "CREATE TABLE loop_guard (id INTEGER PRIMARY KEY)";
    execute_routed(&rt, &idle_cfg(), &engine, None, sql).expect("local execute");
    let cnt = engine
        .execute("SELECT COUNT(*) FROM loop_guard")
        .expect("count");
    assert_eq!(cnt.rows.len(), 1);
}

#[tokio::test]
async fn two_node_registry_routes_to_correct_peer() {
    let node_a = Arc::new(NativeSqlEngine::new());
    let node_b = Arc::new(NativeSqlEngine::new());
    node_b
        .execute("CREATE TABLE reg_peer (id INTEGER PRIMARY KEY, tag TEXT)")
        .expect("peer ddl");

    let (addr_a, ha) = spawn_listener(Arc::clone(&node_a)).await;
    let (addr_b, hb) = spawn_listener(Arc::clone(&node_b)).await;

    let mut registry = ShardEndpointRegistry::new();
    registry.insert(0, addr_a);
    registry.insert(1, addr_a);
    registry.insert(2, addr_b);
    registry.insert(3, addr_b);

    let cluster = MetaCluster::new(&[1]);
    cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
    let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("catalog"));
    runtime.enable();

    let key = find_shard_key_for_shard(4, 3);
    let sql = format!("INSERT INTO reg_peer (id, tag) VALUES ({key}, 'n3')");
    let gateway = Arc::clone(&node_a);
    let cfg = ClusterNodeConfig {
        enabled: true,
        local_addr: Some(addr_a),
        ..ClusterNodeConfig::default()
    };
    let runtime_bg = Arc::clone(&runtime);
    let cfg_bg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        execute_routed(&runtime_bg, &cfg_bg, &gateway, Some(addr_a), &sql)
    })
    .await
    .expect("join")
    .expect("routed insert");

    let on_b = node_b
        .execute(&format!("SELECT tag FROM reg_peer WHERE id = {key}"))
        .expect("peer row");
    assert_eq!(on_b.rows.len(), 1);

    drop(ha);
    drop(hb);
}

async fn spawn_listener(
    engine: Arc<NativeSqlEngine>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let eng = Arc::clone(&engine);
            tokio::spawn(async move {
                let _ = TransportServer::serve_connection(stream, eng).await;
            });
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    (addr, handle)
}
