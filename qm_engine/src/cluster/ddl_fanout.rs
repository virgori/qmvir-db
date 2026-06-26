/*
 * DDL fan-out — schema statements run on every distinct shard primary.
 *
 * DML (INSERT/SELECT/...) still routes by shard key; DDL must exist on all nodes
 * before routed inserts succeed on remotes.
 */

use std::collections::HashSet;
use std::net::SocketAddr;

use super::runtime::ClusterRuntime;
use super::shard_group::ShardGroupKind;

/// Returns true for cluster-wide schema statements.
pub fn is_cluster_ddl(sql: &str) -> bool {
    let up = sql.trim().to_ascii_uppercase();
    if up.is_empty() {
        return false;
    }
    if up.starts_with("CREATE ") {
        return up.contains(" TABLE ")
            || up.contains(" INDEX ")
            || up.contains(" UNIQUE INDEX ")
            || up.starts_with("CREATE TABLE")
            || up.starts_with("CREATE INDEX")
            || up.starts_with("CREATE UNIQUE INDEX");
    }
    if up.starts_with("DROP ") {
        return up.contains(" TABLE ") || up.contains(" INDEX ") || up.starts_with("DROP TABLE");
    }
    if up.starts_with("ALTER ") {
        return up.contains(" TABLE ") || up.starts_with("ALTER TABLE");
    }
    up.starts_with("TRUNCATE ")
}

/// Collect distinct primary endpoints from the OLTP shard group catalog.
pub fn unique_primary_endpoints(runtime: &ClusterRuntime) -> Vec<SocketAddr> {
    let Some(catalog) = runtime.router_snapshot() else {
        return Vec::new();
    };
    let Some(group) = catalog.group_for_kind(ShardGroupKind::Oltp) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for ep in &group.endpoints {
        if seen.insert(ep.primary) {
            out.push(ep.primary);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::meta_cluster::MetaCluster;
    use crate::cluster::node_registry::ShardEndpointRegistry;
    use crate::cluster::runtime::ClusterRuntime;
    use std::net::{IpAddr, Ipv4Addr};

    fn localhost(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn detects_ddl_statements() {
        assert!(is_cluster_ddl("CREATE TABLE t (id INTEGER PRIMARY KEY)"));
        assert!(is_cluster_ddl("create index idx on t (id)"));
        assert!(is_cluster_ddl("DROP TABLE t"));
        assert!(is_cluster_ddl("ALTER TABLE t ADD COLUMN x TEXT"));
        assert!(!is_cluster_ddl("INSERT INTO t VALUES (1)"));
        assert!(!is_cluster_ddl("SELECT * FROM t"));
    }

    #[test]
    fn unique_endpoints_dedupes_nodes() {
        let cluster = MetaCluster::new(&[1]);
        let mut reg = ShardEndpointRegistry::new();
        reg.insert(0, localhost(55441));
        reg.insert(1, localhost(55441));
        reg.insert(2, localhost(55442));
        reg.insert(3, localhost(55442));
        cluster.bootstrap_default_groups_with_registry(1, 4, &reg);
        let rt = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
        rt.enable();
        let eps = unique_primary_endpoints(&rt);
        assert_eq!(eps.len(), 2);
        assert!(eps.contains(&localhost(55441)));
        assert!(eps.contains(&localhost(55442)));
    }
}
