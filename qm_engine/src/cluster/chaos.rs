/*
 * Phase I — chaos / partition RPO & RTO integration tests.
 */

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::cluster::failover::{apply_failover_to_catalog, check_and_failover};
    use crate::cluster::gateway_bridge::execute_routed;
    use crate::cluster::meta_cluster::MetaCluster;
    use crate::cluster::node_registry::{find_shard_key_for_shard, ShardEndpointRegistry};
    use crate::cluster::runtime::ClusterRuntime;
    use crate::cluster::shard_group::{ShardEndpoint, ShardGroup, ShardGroupCatalog, ShardGroupKind};
    use crate::cluster::transport::NodeClient;
    use crate::cluster::wal_apply::WalApplyTracker;
    use crate::cluster::{ClusterNodeConfig, cluster_metrics};
    use crate::gateway::native_sql::NativeSqlEngine;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    async fn spawn_transport(
        engine: Arc<NativeSqlEngine>,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        use crate::cluster::transport::TransportServer;
        use crate::cluster::two_phase_commit::TwoPhaseParticipant;
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
                    let _ =
                        TransportServer::serve_connection_shared(stream, eng, part, wal).await;
                });
            }
        });
        ready_rx.await.expect("ready");
        (local, handle)
    }

    #[test]
    fn sync_wal_rpo_zero_after_primary_write() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");

        let primary = Arc::new(NativeSqlEngine::new());
        let standby = Arc::new(NativeSqlEngine::new());
        let (addr_p, _hp) = rt.block_on(spawn_transport(Arc::clone(&primary)));
        let (addr_s, _hs) = rt.block_on(spawn_transport(Arc::clone(&standby)));

        primary
            .execute("CREATE TABLE chaos_rpo (id INTEGER PRIMARY KEY, v TEXT)")
            .expect("ddl");
        standby
            .execute("CREATE TABLE chaos_rpo (id INTEGER PRIMARY KEY, v TEXT)")
            .expect("ddl standby");

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
        let sql = format!("INSERT INTO chaos_rpo (id, v) VALUES ({key}, 'sync')");
        execute_routed(&runtime, &cfg, &primary, Some(addr_p), &sql).expect("write");

        let lag = cluster_metrics::wal_lag_lsn();
        assert_eq!(lag, 0, "sync WAL should yield RPO=0 (lag 0)");
    }

    #[test]
    fn failover_reroute_after_primary_partition() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");

        let node_a = Arc::new(NativeSqlEngine::new());
        let node_b = Arc::new(NativeSqlEngine::new());
        let (addr_a, ha) = rt.block_on(spawn_transport(Arc::clone(&node_a)));
        let (addr_b, _hb) = rt.block_on(spawn_transport(Arc::clone(&node_b)));

        let mut registry = ShardEndpointRegistry::new();
        registry.insert(0, addr_a);
        registry.insert_replica(0, addr_b);
        let cluster = MetaCluster::new(&[1]);
        cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
        let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
        runtime.enable();

        // Simulate partition: stop node_a transport (primary down).
        ha.abort();

        let cfg = ClusterNodeConfig {
            enabled: true,
            local_addr: Some(addr_b),
            failover_enabled: true,
            wal_peers: vec![addr_b],
            ..ClusterNodeConfig::default()
        };

        let start = Instant::now();
        let promoted = check_and_failover(&runtime, &cfg);
        let rto_ms = start.elapsed().as_millis();

        assert!(promoted >= 1, "expected failover promotion");
        let cat = runtime.router_snapshot().expect("catalog");
        let ep = cat
            .group_for_kind(ShardGroupKind::Oltp)
            .unwrap()
            .endpoints
            .iter()
            .find(|e| e.shard_id == 0)
            .unwrap();
        assert_eq!(ep.primary, addr_b);
        assert!(rto_ms < 5_000, "RTO should be sub-second probe scale, got {rto_ms}ms");
    }

    #[test]
    fn manual_failover_catalog_swap() {
        let mut cat = ShardGroupCatalog::new();
        let mut g = ShardGroup::new(1, ShardGroupKind::Oltp, 2);
        g.endpoints.push(ShardEndpoint {
            shard_id: 0,
            primary: addr(55441),
            replicas: vec![addr(55442)],
        });
        cat.upsert_group(g);

        let next = apply_failover_to_catalog(&cat, 0, addr(55441), addr(55442));
        let primary = next
            .group_for_kind(ShardGroupKind::Oltp)
            .unwrap()
            .primary_for_shard(0);
        assert_eq!(primary, Some(addr(55442)));
    }

    #[test]
    fn fencing_rejects_stale_epoch_after_bump() {
        let _net = crate::cluster::test_sync::NETWORK.lock();
        let cur = super::super::fencing::current_epoch();
        super::super::fencing::bump_epoch();
        assert!(!super::super::fencing::accept_epoch(cur, true));
    }
}
