/*
 * Gateway ↔ Cluster bridge — routes SQL when ClusterRuntime is armed.
 * NativeSqlEngine is unchanged; only the Postgres gateway handler calls this.
 */

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Runtime;

use super::failover;
use super::shard_key::extract_shard_key;
use super::config::ClusterNodeConfig;
use super::meta_cluster::MetaCluster;
use super::node_registry::ShardEndpointRegistry;
use super::runtime::ClusterRuntime;
use super::transport::{NodeClient, TransportServer};
use crate::gateway::native_sql::NativeSqlEngine;
use crate::gateway::{AuthQueryHandler, QueryHandler, QueryResult};

static FORWARD_TXN_ID: AtomicU64 = AtomicU64::new(1);

/// Attached cluster state for one gateway / `qm start` process.
#[derive(Clone)]
pub struct ClusterGatewayAttach {
    pub runtime: Arc<ClusterRuntime>,
    pub local_addr: Option<SocketAddr>,
    pub config: ClusterNodeConfig,
}

/// Build catalog + runtime from env. Returns `None` when cluster is not active.
pub fn cluster_runtime_from_config(cfg: &ClusterNodeConfig) -> Option<Arc<ClusterRuntime>> {
    if !cfg.is_active() {
        return None;
    }
    let transport_port = cfg.transport_port?;

    let catalog = if super::meta_raft_network::networked_meta_ready(cfg) {
        super::meta_raft_network::init_local_meta(cfg.node_id);
        let mut networked = false;
        if let Some(mut registry) = ShardEndpointRegistry::from_env() {
            if let Some(local) = cfg.local_addr {
                registry.ensure_default_primary(local);
            }
            networked = super::meta_raft_network::bootstrap_catalog_with_network(cfg, &registry);
        }
        if networked {
            super::meta_raft_network::catalog_snapshot()
        } else {
            legacy_catalog_from_env(cfg, transport_port)?
        }
    } else {
        legacy_catalog_from_env(cfg, transport_port)?
    };

    let rt = ClusterRuntime::with_catalog(catalog);
    rt.enable();
    Some(rt)
}

fn legacy_catalog_from_env(
    cfg: &ClusterNodeConfig,
    transport_port: u16,
) -> Option<super::shard_group::ShardGroupCatalog> {
    let cluster = MetaCluster::new(&[cfg.meta_leader_id]);
    if let Some(mut registry) = ShardEndpointRegistry::from_env() {
        if let Some(local) = cfg.local_addr {
            registry.ensure_default_primary(local);
        }
        cluster.bootstrap_default_groups_with_registry(
            cfg.meta_leader_id,
            cfg.shards_per_group,
            &registry,
        );
    } else {
        cluster.bootstrap_default_groups_with_transport(
            cfg.meta_leader_id,
            cfg.shards_per_group,
            cfg.bind_host,
            transport_port,
        );
    }
    cluster.catalog_on(cfg.meta_leader_id)
}

/// Read env, optionally bootstrap runtime, spawn transport listener.
pub fn prepare_gateway_cluster(
    engine: Arc<NativeSqlEngine>,
    tokio: &Runtime,
) -> ClusterGatewayAttach {
    let cfg = ClusterNodeConfig::from_env();
    let runtime =
        cluster_runtime_from_config(&cfg).unwrap_or_else(ClusterRuntime::disabled);

    if let Some(port) = cfg.transport_port {
        spawn_transport_server(tokio, port, Arc::clone(&engine), runtime.clone());
        if cfg.enabled {
            if cfg.witness_enabled {
                tracing::info!(
                    "cluster witness on port {} (node_id={}) — Raft voter only",
                    port,
                    cfg.node_id
                );
            } else {
                tracing::info!(
                    "cluster transport on port {} (node_id={})",
                    port,
                    cfg.node_id
                );
            }
        }
    }

    if cfg.failover_enabled && runtime.is_active() && !cfg.witness_enabled {
        failover::spawn_failover_loop(
            runtime.clone(),
            cfg.clone(),
            engine.data_dir.clone(),
            tokio,
        );
        tracing::info!("cluster failover health loop enabled");
    }

    if cfg.wal_catchup_enabled && cfg.wal_replicate && !cfg.wal_peers.is_empty() && !cfg.witness_enabled {
        spawn_standby_heal_loop(engine.clone(), cfg.clone(), tokio);
    }

    ClusterGatewayAttach {
        runtime,
        local_addr: cfg.local_addr,
        config: cfg,
    }
}

/// Start inter-node transport listener (async task on gateway runtime).
pub fn spawn_transport_server(
    runtime: &Runtime,
    port: u16,
    engine: Arc<NativeSqlEngine>,
    cluster_runtime: Arc<ClusterRuntime>,
) {
    let server = TransportServer::new(port);
    runtime.spawn(async move {
        if let Err(e) = server.run(engine, Some(cluster_runtime)).await {
            tracing::warn!("cluster transport server stopped: {e}");
        }
    });
}

/// Build sync query handlers that route through `execute_routed` when armed.
pub fn routed_query_handlers(
    attach: &ClusterGatewayAttach,
    engine: Arc<NativeSqlEngine>,
) -> (QueryHandler, AuthQueryHandler) {
    use crate::gateway::session_pool::global_session_pool;

    let rt = attach.runtime.clone();
    let local_addr = attach.local_addr;
    let eng = engine.clone();
    let cfg = attach.config.clone();
    let handler: QueryHandler = Arc::new(move |sql: String| {
        let conn_id = crate::cluster::connection_id();
        let session = if conn_id != 0 {
            global_session_pool().session_for_connection(conn_id, &eng)
        } else {
            eng.new_session()
        };
        super::pg_distributed::execute_pg_routed(
            &rt,
            &cfg,
            &eng,
            local_addr,
            &sql,
            || execute_routed_inner(&rt, &cfg, &eng, &session, local_addr, &sql),
        )
    });

    let rt2 = attach.runtime.clone();
    let eng2 = engine;
    let cfg2 = attach.config.clone();
    let authed: AuthQueryHandler = Arc::new(move |sql: String, user: String| {
        let conn_id = crate::cluster::connection_id();
        let session = if conn_id != 0 {
            global_session_pool().session_for_connection(conn_id, &eng2)
        } else {
            eng2.new_session()
        };
        super::pg_distributed::execute_pg_routed(
            &rt2,
            &cfg2,
            &eng2,
            local_addr,
            &sql,
            || execute_routed_as_inner(&rt2, &cfg2, &eng2, &session, local_addr, &sql, &user),
        )
    });

    (handler, authed)
}

fn spawn_standby_heal_loop(
    engine: Arc<NativeSqlEngine>,
    cfg: ClusterNodeConfig,
    tokio: &Runtime,
) {
    use super::wal_apply::WalApplyTracker;
    let tracker = Arc::new(WalApplyTracker::new());
    let interval = Duration::from_secs(10);
    tokio.spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let eng = engine.clone();
            let c = cfg.clone();
            let tr = Arc::clone(&tracker);
            let _ = tokio::task::spawn_blocking(move || {
                let _ = super::wal_catchup::heal_standby_from_primary(
                    &c,
                    eng.data_dir.as_deref(),
                    |entry| super::wal_apply::apply_wal_entry(&tr, &eng, entry).map(|_| ()),
                );
            })
            .await;
        }
    });
}

fn forward_result_to_query_result(
    msg: super::transport::ForwardResultMsg,
) -> Result<QueryResult, String> {
    if !msg.ok {
        return Err(msg.error.unwrap_or_else(|| "remote forward failed".into()));
    }
    let rows = msg
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.map(|s| s.into_bytes()))
                .collect()
        })
        .collect();
    Ok(QueryResult {
        columns: Vec::new(),
        rows,
        command_tag: msg.command_tag,
    ..Default::default()
    })
}

/// Gateway execution path: local engine or forward to routed primary.
pub fn execute_routed(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    local_engine: &Arc<NativeSqlEngine>,
    local_addr: Option<SocketAddr>,
    sql: &str,
) -> Result<QueryResult, String> {
    execute_routed_inner(runtime, cfg, local_engine, local_engine.as_ref(), local_addr, sql)
}

fn execute_routed_inner(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    shared_engine: &Arc<NativeSqlEngine>,
    session_engine: &NativeSqlEngine,
    local_addr: Option<SocketAddr>,
    sql: &str,
) -> Result<QueryResult, String> {
    if !runtime.is_active() {
        return session_engine.execute(sql);
    }

    if super::cross_shard::is_distributed_batch(sql) {
        return super::cross_shard::execute_distributed_batch(
            runtime,
            cfg,
            shared_engine,
            local_addr,
            sql,
        );
    }

    if super::ddl_fanout::is_cluster_ddl(sql) {
        let targets = super::ddl_fanout::unique_primary_endpoints(runtime);
        if targets.len() > 1 {
            return execute_ddl_fanout(session_engine, local_addr, sql, &targets);
        }
    }

    let shard_key = extract_shard_key(sql);
    let up = sql.trim().to_ascii_uppercase();
    // HA analytics read path: route SELECT analytics/vector to async replica when available.
    if up.starts_with("SELECT") {
        if let Some((plan, Some(replica))) =
            runtime.route_analytics_read_if_active(sql, shard_key)
        {
            if !local_addr.is_some_and(|local| local == replica) {
                return forward_sql_blocking_with_fallback(replica, &plan.replicas, sql, None);
            }
        }
    }

    let Some(plan) = runtime.route_sql_if_active(sql, shard_key) else {
        return execute_local_primary(cfg, session_engine, sql);
    };

    let Some(remote) = plan.primary else {
        return execute_local_primary(cfg, session_engine, sql);
    };

    if local_addr.is_some_and(|local| local == remote) {
        return execute_local_primary(cfg, session_engine, sql);
    }

    forward_sql_blocking_with_fallback(remote, &plan.replicas, sql, None)
}

fn execute_local_primary(
    cfg: &ClusterNodeConfig,
    local_engine: &NativeSqlEngine,
    sql: &str,
) -> Result<QueryResult, String> {
    super::stonith::require_write_lease(local_engine.data_dir.as_deref(), cfg)?;
    let result = local_engine.execute(sql)?;
    super::wal_replication::replicate_after_local_write(cfg, sql)?;
    Ok(result)
}

fn forward_sql_blocking_with_fallback(
    primary: SocketAddr,
    replicas: &[SocketAddr],
    sql: &str,
    user: Option<&str>,
) -> Result<QueryResult, String> {
    let txn_id = FORWARD_TXN_ID.fetch_add(1, Ordering::Relaxed);
    let targets = failover::forward_targets(primary, replicas);
    let mut last_err = String::new();
    for addr in targets {
        let client = NodeClient::new(0, addr);
        match client.forward_query_blocking(sql, txn_id, user) {
            Ok(msg) => return forward_result_to_query_result(msg),
            Err(e) => {
                super::cluster_metrics::inc_forward_errors();
                last_err = format!("cluster forward to {addr}: {e}");
            }
        }
    }
    Err(if last_err.is_empty() {
        "cluster forward: no targets".into()
    } else {
        last_err
    })
}

fn forward_sql_blocking(remote: SocketAddr, sql: &str) -> Result<QueryResult, String> {
    forward_sql_blocking_with_fallback(remote, &[], sql, None)
}

fn execute_ddl_fanout(
    local_engine: &NativeSqlEngine,
    local_addr: Option<SocketAddr>,
    sql: &str,
    targets: &[SocketAddr],
) -> Result<QueryResult, String> {
    let mut last: Option<Result<QueryResult, String>> = None;
    for &addr in targets {
        let result = if local_addr.is_some_and(|local| local == addr) {
            local_engine.execute(sql)
        } else {
            forward_sql_blocking(addr, sql)
        };
        match &result {
            Ok(_) => last = Some(result),
            Err(e) => {
                // Idempotent DDL: ignore "already exists" on fan-out peers.
                if is_idempotent_ddl_conflict(sql, e) {
                    last = Some(Ok(QueryResult {
                        columns: Vec::new(),
                        rows: Vec::new(),
                        command_tag: "OK".into(),
                    ..Default::default()
                    }));
                } else {
                    return result;
                }
            }
        }
    }
    last.unwrap_or_else(|| Err("ddl fan-out: no targets".into()))
}

fn is_idempotent_ddl_conflict(sql: &str, err: &str) -> bool {
    let up_sql = sql.trim().to_ascii_uppercase();
    let err_l = err.to_ascii_lowercase();
    if up_sql.starts_with("CREATE ") {
        return err_l.contains("already exists");
    }
    if up_sql.starts_with("DROP ") {
        return err_l.contains("does not exist") || err_l.contains("not found");
    }
    false
}

/// Authenticated path — propagates user to remote primaries/replicas.
pub fn execute_routed_as(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    local_engine: &Arc<NativeSqlEngine>,
    local_addr: Option<SocketAddr>,
    sql: &str,
    user: &str,
) -> Result<QueryResult, String> {
    execute_routed_as_inner(runtime, cfg, local_engine, local_engine.as_ref(), local_addr, sql, user)
}

fn execute_routed_as_inner(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    shared_engine: &Arc<NativeSqlEngine>,
    session_engine: &NativeSqlEngine,
    local_addr: Option<SocketAddr>,
    sql: &str,
    user: &str,
) -> Result<QueryResult, String> {
    if !runtime.is_active() {
        return session_engine.execute_as(sql, user);
    }

    if super::cross_shard::is_distributed_batch(sql) {
        return super::cross_shard::execute_distributed_batch(
            runtime,
            cfg,
            shared_engine,
            local_addr,
            sql,
        );
    }

    let shard_key = extract_shard_key(sql);
    let up = sql.trim().to_ascii_uppercase();
    if up.starts_with("SELECT") {
        if let Some((plan, Some(replica))) =
            runtime.route_analytics_read_if_active(sql, shard_key)
        {
            if !local_addr.is_some_and(|local| local == replica) {
                return forward_sql_blocking_with_fallback(
                    replica,
                    &plan.replicas,
                    sql,
                    Some(user),
                );
            }
        }
    }

    let Some(plan) = runtime.route_sql_if_active(sql, shard_key) else {
        return session_engine.execute_as(sql, user);
    };
    let Some(remote) = plan.primary else {
        return session_engine.execute_as(sql, user);
    };
    if local_addr.is_some_and(|local| local == remote) {
        return session_engine.execute_as(sql, user);
    }
    forward_sql_blocking_with_fallback(remote, &plan.replicas, sql, Some(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    fn test_cfg(local_addr: Option<SocketAddr>) -> ClusterNodeConfig {
        ClusterNodeConfig {
            enabled: true,
            local_addr,
            ..ClusterNodeConfig::default()
        }
    }

    #[test]
    fn extract_shard_key_from_where_id() {
        assert_eq!(
            super::super::shard_key::extract_shard_key("SELECT * FROM users WHERE id = 42"),
            42
        );
    }

    #[test]
    fn cluster_config_defaults_off() {
        let cfg = ClusterNodeConfig::default();
        assert!(!cfg.is_active());
    }

    #[test]
    fn bootstrap_assigns_transport_endpoints() {
        for key in [
            "QM_CLUSTER_SHARD_ENDPOINTS",
            "QM_CLUSTER_SHARD_REPLICAS",
            "QM_CLUSTER_ENABLE",
        ] {
            unsafe {
                std::env::remove_var(key);
            }
        }
        let cfg = ClusterNodeConfig {
            enabled: true,
            transport_port: Some(55440),
            local_addr: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 55440)),
            ..ClusterNodeConfig::default()
        };
        let rt = cluster_runtime_from_config(&cfg).expect("runtime");
        assert!(rt.is_active());
        let plan = rt
            .route_sql_if_active("INSERT INTO t (id) VALUES (7)", 7)
            .expect("route");
        assert_eq!(
            plan.primary,
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 55440))
        );
    }

    async fn spawn_transport(
        engine: Arc<NativeSqlEngine>,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        use super::super::transport::TransportServer;
        use super::super::two_phase_commit::TwoPhaseParticipant;
        use super::super::wal_apply::WalApplyTracker;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
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
                    let _ =
                        TransportServer::serve_connection_shared(stream, eng, part, wal).await;
                });
            }
        });
        ready_rx.await.expect("listener ready");
        (addr, handle)
    }

    #[test]
    fn two_node_shard_registry_routes_insert_to_peer() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        use super::super::meta_cluster::MetaCluster;
        use super::super::node_registry::{find_shard_key_for_shard, ShardEndpointRegistry};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let node_a = Arc::new(NativeSqlEngine::new());
        let node_b = Arc::new(NativeSqlEngine::new());

        let (addr_a, _ha) = rt.block_on(spawn_transport(Arc::clone(&node_a)));
        let (addr_b, _hb) = rt.block_on(spawn_transport(Arc::clone(&node_b)));

        let mut registry = ShardEndpointRegistry::new();
        registry.insert(0, addr_a);
        registry.insert(1, addr_a);
        registry.insert(2, addr_b);
        registry.insert(3, addr_b);

        let cluster = MetaCluster::new(&[1]);
        cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
        let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
        runtime.enable();

        let gateway = Arc::clone(&node_a);
        let cfg = test_cfg(Some(addr_a));
        execute_routed(
            &runtime,
            &cfg,
            &gateway,
            Some(addr_a),
            "CREATE TABLE ha_peer (id INTEGER PRIMARY KEY, v TEXT)",
        )
        .expect("ddl fan-out");

        let shard_key = find_shard_key_for_shard(4, 2);
        let sql = format!(
            "INSERT INTO ha_peer (id, v) VALUES ({shard_key}, 'from_gateway')"
        );
        let plan = runtime
            .route_sql_if_active(&sql, shard_key)
            .expect("route plan");
        assert_eq!(plan.primary, Some(addr_b));

        execute_routed(&runtime, &cfg, &gateway, Some(addr_a), &sql).expect("forward insert");

        let local_cnt = node_a
            .execute(&format!("SELECT COUNT(*) FROM ha_peer WHERE id = {shard_key}"))
            .expect("local count");
        let local_n = local_cnt.rows[0][0]
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        assert_eq!(local_n, "0");

        let peer_cnt = node_b
            .execute(&format!("SELECT COUNT(*) FROM ha_peer WHERE id = {shard_key}"))
            .expect("peer count");
        let peer_n = peer_cnt.rows[0][0]
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        assert_eq!(peer_n, "1");
    }

    #[test]
    fn ddl_fanout_creates_on_both_nodes() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        use super::super::meta_cluster::MetaCluster;
        use super::super::node_registry::ShardEndpointRegistry;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let node_a = Arc::new(NativeSqlEngine::new());
        let node_b = Arc::new(NativeSqlEngine::new());
        let (addr_a, _ha) = rt.block_on(spawn_transport(Arc::clone(&node_a)));
        let (addr_b, _hb) = rt.block_on(spawn_transport(Arc::clone(&node_b)));

        let mut registry = ShardEndpointRegistry::new();
        registry.insert(0, addr_a);
        registry.insert(1, addr_a);
        registry.insert(2, addr_b);
        registry.insert(3, addr_b);

        let cluster = MetaCluster::new(&[1]);
        cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
        let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
        runtime.enable();

        let gateway = Arc::clone(&node_a);
        let cfg = test_cfg(Some(addr_a));
        execute_routed(
            &runtime,
            &cfg,
            &gateway,
            Some(addr_a),
            "CREATE TABLE ddl_fan (id INTEGER PRIMARY KEY)",
        )
        .expect("fan-out create");

        assert!(node_a.execute("SELECT id FROM ddl_fan LIMIT 1").is_ok());
        assert!(node_b.execute("SELECT id FROM ddl_fan LIMIT 1").is_ok());
    }

    #[test]
    fn wal_replication_ships_local_dml_to_standby() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        use super::super::meta_cluster::MetaCluster;
        use super::super::node_registry::{find_shard_key_for_shard, ShardEndpointRegistry};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let primary = Arc::new(NativeSqlEngine::new());
        let standby = Arc::new(NativeSqlEngine::new());
        let (addr_p, _hp) = rt.block_on(spawn_transport(Arc::clone(&primary)));
        let (addr_s, _hs) = rt.block_on(spawn_transport(Arc::clone(&standby)));

        primary
            .execute("CREATE TABLE wal_rep (id INTEGER PRIMARY KEY, v TEXT)")
            .expect("ddl primary");
        standby
            .execute("CREATE TABLE wal_rep (id INTEGER PRIMARY KEY, v TEXT)")
            .expect("ddl standby");

        let mut registry = ShardEndpointRegistry::with_default_primary(addr_p);
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
        let sql = format!("INSERT INTO wal_rep (id, v) VALUES ({key}, 'replicated')");
        execute_routed(&runtime, &cfg, &primary, Some(addr_p), &sql).expect("local insert");

        let row = standby
            .execute(&format!("SELECT v FROM wal_rep WHERE id = {key}"))
            .expect("standby row");
        assert_eq!(row.rows.len(), 1);
    }

    #[test]
    fn wal_replication_skips_duplicate_lsn_on_retry() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        use super::super::meta_cluster::MetaCluster;
        use super::super::node_registry::{find_shard_key_for_shard, ShardEndpointRegistry};
        use super::super::transport::NodeClient;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let primary = Arc::new(NativeSqlEngine::new());
        let standby = Arc::new(NativeSqlEngine::new());
        let (addr_p, _hp) = rt.block_on(spawn_transport(Arc::clone(&primary)));
        let (addr_s, _hs) = rt.block_on(spawn_transport(Arc::clone(&standby)));

        primary
            .execute("CREATE TABLE wal_dup (id INTEGER PRIMARY KEY)")
            .expect("ddl primary");
        standby
            .execute("CREATE TABLE wal_dup (id INTEGER PRIMARY KEY)")
            .expect("ddl standby");

        let mut registry = ShardEndpointRegistry::with_default_primary(addr_p);
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
        let sql = format!("INSERT INTO wal_dup (id) VALUES ({key})");
        execute_routed(&runtime, &cfg, &primary, Some(addr_p), &sql).expect("local insert");

        // Force duplicate ship of the same LSN by re-sending last entry manually.
        let client = NodeClient::new(1, addr_s);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(sql.as_bytes());
        let entry = super::super::transport::WalEntry {
            lsn: 1,
            sql: sql.clone(),
            checksum: hasher.finalize(),
        };
        rt.block_on(async {
            client.send_wal_entry(&entry).await.expect("dup ship");
        });

        let rows = standby
            .execute("SELECT id FROM wal_dup")
            .expect("standby count");
        assert_eq!(rows.rows.len(), 1);
    }
}
