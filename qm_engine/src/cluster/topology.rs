/*
 * Phase J — dynamic shard join/leave via catalog mutation.
 */

use std::net::SocketAddr;

use super::runtime::ClusterRuntime;
use super::shard::ShardId;
use super::shard_group::{ShardGroup, ShardGroupCatalog, ShardGroupKind};

/// Add or update a shard endpoint across all workload groups.
pub fn join_shard(
    catalog: &ShardGroupCatalog,
    shard_id: ShardId,
    primary: SocketAddr,
    replicas: Vec<SocketAddr>,
) -> ShardGroupCatalog {
    let mut next = catalog.clone();
    next.join_shard(shard_id, primary, replicas);
    next
}

/// Remove a shard from all workload groups.
pub fn leave_shard(catalog: &ShardGroupCatalog, shard_id: ShardId) -> ShardGroupCatalog {
    let mut next = catalog.clone();
    next.leave_shard(shard_id);
    next
}

pub fn apply_join(
    runtime: &ClusterRuntime,
    shard_id: ShardId,
    primary: SocketAddr,
    replicas: Vec<SocketAddr>,
) -> bool {
    let Some(catalog) = runtime.router_snapshot() else {
        return false;
    };
    runtime.replace_catalog(join_shard(&catalog, shard_id, primary, replicas));
    true
}

pub fn apply_leave(runtime: &ClusterRuntime, shard_id: ShardId) -> bool {
    let Some(catalog) = runtime.router_snapshot() else {
        return false;
    };
    runtime.replace_catalog(leave_shard(&catalog, shard_id));
    true
}

/// Bootstrap catalog with registry primaries + replicas (dev join).
pub fn bootstrap_minimal_catalog(
    shards_per_group: u32,
    registry: &super::node_registry::ShardEndpointRegistry,
) -> ShardGroupCatalog {
    let mut cat = ShardGroupCatalog::new();
    for kind in ShardGroupKind::all() {
        let group_id = match kind {
            ShardGroupKind::Oltp => 1,
            ShardGroupKind::Vector => 2,
            ShardGroupKind::Search => 3,
            ShardGroupKind::Analytics => 4,
        };
        let mut group = ShardGroup::new(group_id, kind, shards_per_group);
        for shard_id in 0..shards_per_group {
            let Some(primary) = registry.primary_for_shard(shard_id) else {
                continue;
            };
            group.endpoints.push(super::shard_group::ShardEndpoint {
                shard_id,
                primary,
                replicas: registry.replicas_for_shard(shard_id),
            });
        }
        cat.upsert_group(group);
    }
    cat
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p)
    }

    #[test]
    fn join_and_leave_shard() {
        let reg = super::super::node_registry::ShardEndpointRegistry::with_default_primary(addr(1));
        let cat = bootstrap_minimal_catalog(2, &reg);
        let joined = join_shard(&cat, 1, addr(9), vec![addr(8)]);
        assert_eq!(
            joined
                .group_for_kind(ShardGroupKind::Oltp)
                .unwrap()
                .primary_for_shard(1),
            Some(addr(9))
        );
        let left = leave_shard(&joined, 1);
        assert!(
            left.group_for_kind(ShardGroupKind::Oltp)
                .unwrap()
                .primary_for_shard(1)
                .is_none()
        );
    }
}
