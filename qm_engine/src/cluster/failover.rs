/*
 * Phase G — health-driven failover and catalog reroute.
 */

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use super::cluster_metrics;
use super::config::ClusterNodeConfig;
use super::runtime::ClusterRuntime;
use super::shard_group::{ShardGroupCatalog, ShardGroupKind};
use super::transport::NodeClient;

/// Swap a failed primary with a reachable standby across all workload groups.
pub fn apply_failover_to_catalog(
    catalog: &ShardGroupCatalog,
    shard_id: super::shard::ShardId,
    failed_primary: SocketAddr,
    new_primary: SocketAddr,
) -> ShardGroupCatalog {
    let mut next = catalog.clone();
    next.failover_shard(shard_id, failed_primary, new_primary);
    next
}

fn pick_standby(
    ep: &super::shard_group::ShardEndpoint,
    cfg: &ClusterNodeConfig,
    failed_primary: SocketAddr,
    timeout: Duration,
) -> Option<SocketAddr> {
    for replica in &ep.replicas {
        if *replica == failed_primary {
            continue;
        }
        if peer_up(*replica, timeout) {
            return Some(*replica);
        }
    }
    for peer in &cfg.wal_peers {
        if *peer == failed_primary || cfg.local_addr.is_some_and(|l| l == *peer) {
            continue;
        }
        if peer_up(*peer, timeout) {
            return Some(*peer);
        }
    }
    None
}

fn peer_up(addr: SocketAddr, timeout: Duration) -> bool {
    let ok = NodeClient::new(0, addr)
        .ping_blocking(timeout)
        .unwrap_or(false);
    cluster_metrics::set_peer_up(addr, ok);
    ok
}

/// Probe shard primaries; promote standby when primary is down.
pub fn check_and_failover(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    data_dir: Option<&std::path::Path>,
) -> usize {
    if !cfg.failover_enabled || !runtime.is_active() {
        return 0;
    }

    let Some(catalog) = runtime.router_snapshot() else {
        return 0;
    };
    let Some(oltp) = catalog.group_for_kind(ShardGroupKind::Oltp) else {
        return 0;
    };

    let timeout = Duration::from_secs(2);
    let mut promotions = 0usize;

    for ep in &oltp.endpoints {
        let primary = ep.primary;
        if cfg.local_addr.is_some_and(|local| local == primary) {
            cluster_metrics::set_peer_up(primary, true);
            continue;
        }
        if peer_up(primary, timeout) {
            continue;
        }

        let Some(standby) = pick_standby(ep, cfg, primary, timeout) else {
            tracing::warn!("failover: shard {} primary {primary} down, no standby", ep.shard_id);
            cluster_metrics::inc_failover_skipped();
            continue;
        };

        if cfg.stonith_enabled {
            if super::stonith::promote_with_fence(
                cfg,
                runtime,
                data_dir,
                ep.shard_id,
                primary,
                standby,
            )
            .is_err()
            {
                cluster_metrics::inc_failover_skipped();
                continue;
            }
        } else {
            let updated = apply_failover_to_catalog(&catalog, ep.shard_id, primary, standby);
            runtime.replace_catalog(updated);
            super::fencing::bump_epoch();
        }
        promotions += 1;
        cluster_metrics::inc_failover();
        tracing::warn!(
            "failover: promoted shard {} {primary} -> {standby}",
            ep.shard_id
        );
    }

    promotions
}

/// Background health loop (spawned from gateway when failover enabled).
pub fn spawn_failover_loop(
    runtime: Arc<ClusterRuntime>,
    cfg: ClusterNodeConfig,
    data_dir: Option<std::path::PathBuf>,
    tokio: &tokio::runtime::Runtime,
) {
    if !cfg.failover_enabled {
        return;
    }
    let interval = Duration::from_secs(cfg.failover_interval_secs.max(1));
    tokio.spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let _ = check_and_failover(&runtime, &cfg, data_dir.as_deref());
        }
    });
}

/// Ordered forward targets: primary then replicas.
pub fn forward_targets(primary: SocketAddr, replicas: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut out = vec![primary];
    for r in replicas {
        if *r != primary && !out.contains(r) {
            out.push(*r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn apply_failover_swaps_primary_and_demotes_old() {
        use crate::cluster::shard_group::{ShardGroup, ShardGroupCatalog, ShardEndpoint};

        let mut cat = ShardGroupCatalog::new();
        let mut g = ShardGroup::new(1, ShardGroupKind::Oltp, 2);
        g.endpoints.push(ShardEndpoint {
            shard_id: 0,
            primary: addr(55441),
            replicas: vec![addr(55442)],
        });
        cat.upsert_group(g);

        let next = apply_failover_to_catalog(&cat, 0, addr(55441), addr(55442));
        let ep = next
            .group_for_kind(ShardGroupKind::Oltp)
            .unwrap()
            .endpoints
            .iter()
            .find(|e| e.shard_id == 0)
            .unwrap();
        assert_eq!(ep.primary, addr(55442));
        assert!(ep.replicas.contains(&addr(55441)));
    }

    #[test]
    fn forward_targets_lists_primary_then_replicas() {
        let t = forward_targets(addr(1), &[addr(2), addr(1)]);
        assert_eq!(t, vec![addr(1), addr(2)]);
    }
}
