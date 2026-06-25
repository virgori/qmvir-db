/*
 * QM Router — classifies workloads and picks shard group + shard id.
 *
 *        QM Meta Cluster (Raft)
 *                 ↓
 *            QM Router  ← this module
 *                 ↓
 *   OLTP | Vector | Search | Analytics
 */

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use super::shard::ShardId;
use super::shard_group::{ShardGroupCatalog, ShardGroupKind};

/// SQL / request workload class mapped to a shard group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadClass {
    Oltp,
    Vector,
    Search,
    Analytics,
}

impl WorkloadClass {
    pub fn shard_group_kind(self) -> ShardGroupKind {
        match self {
            Self::Oltp => ShardGroupKind::Oltp,
            Self::Vector => ShardGroupKind::Vector,
            Self::Search => ShardGroupKind::Search,
            Self::Analytics => ShardGroupKind::Analytics,
        }
    }
}

/// Result of routing one statement to a concrete shard primary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePlan {
    pub workload: WorkloadClass,
    pub group_id: u32,
    pub shard_id: ShardId,
    pub primary: Option<SocketAddr>,
    pub shard_key: u64,
}

/// QM Router: reads replicated catalog from Meta Cluster followers/leaders.
pub struct QmRouter {
    catalog: ShardGroupCatalog,
}

impl QmRouter {
    pub fn new(catalog: ShardGroupCatalog) -> Self {
        Self { catalog }
    }

    pub fn from_meta_catalog(catalog: ShardGroupCatalog) -> Self {
        Self::new(catalog)
    }

    pub fn catalog(&self) -> &ShardGroupCatalog {
        &self.catalog
    }

    /// Lightweight SQL classifier (gateway can override with planner hints).
    pub fn classify_sql(sql: &str) -> WorkloadClass {
        let up = sql.to_ascii_uppercase();
        if up.contains(" <-> ")
            || up.contains(" <=> ")
            || up.contains(" <#> ")
            || up.contains("VECTOR(")
            || up.contains(" USING HNSW")
        {
            return WorkloadClass::Vector;
        }
        if up.contains(" @@ ")
            || up.contains(" USING GIN")
            || up.contains(" USING GIN_TRGM")
            || up.contains(" TO_TSVECTOR")
            || up.contains(" PLAINTO_TSQUERY")
        {
            return WorkloadClass::Search;
        }
        if up.contains("GROUP BY")
            && (up.contains("SUM(")
                || up.contains("AVG(")
                || up.contains("COUNT(")
                || up.contains("PARQUET"))
            || up.contains("COPY ") && up.contains("PARQUET")
        {
            return WorkloadClass::Analytics;
        }
        WorkloadClass::Oltp
    }

    pub fn route_sql(&self, sql: &str, shard_key: u64) -> Result<RoutePlan, String> {
        let workload = Self::classify_sql(sql);
        self.route(workload, shard_key)
    }

    pub fn route(&self, workload: WorkloadClass, shard_key: u64) -> Result<RoutePlan, String> {
        let kind = workload.shard_group_kind();
        let group = self
            .catalog
            .group_for_kind(kind)
            .ok_or_else(|| format!("no shard group registered for {:?}", kind))?;
        let ring = group.ring();
        let shard_id = ring
            .get_shard(shard_key)
            .ok_or_else(|| format!("empty hash ring for group {}", group.group_id))?;
        let primary = group.primary_for_shard(shard_id);
        Ok(RoutePlan {
            workload,
            group_id: group.group_id,
            shard_id,
            primary,
            shard_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::meta_cluster::MetaCluster;
    use std::net::{IpAddr, Ipv4Addr};

    fn localhost_port(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn router_classifies_vector_and_search() {
        assert_eq!(
            QmRouter::classify_sql("SELECT id FROM t ORDER BY emb <-> '[0.1]' LIMIT 10"),
            WorkloadClass::Vector
        );
        assert_eq!(
            QmRouter::classify_sql("SELECT id FROM t WHERE body @@ 'needle'"),
            WorkloadClass::Search
        );
        assert_eq!(
            QmRouter::classify_sql("INSERT INTO users (id) VALUES (1)"),
            WorkloadClass::Oltp
        );
    }

    #[test]
    fn router_uses_meta_cluster_catalog() {
        let cluster = MetaCluster::new(&[1, 2, 3]);
        cluster.bootstrap_default_groups(1, 4);

        let catalog = cluster.catalog_on(1).unwrap();
        let mut cat = catalog;
        if let Some(mut group) = cat.group(1).cloned() {
            group.endpoints.push(crate::cluster::shard_group::ShardEndpoint {
                shard_id: 0,
                primary: localhost_port(55401),
                replicas: vec![],
            });
            cat.upsert_group(group);
        }

        let router = QmRouter::new(cat);
        let plan = router
            .route_sql("INSERT INTO users (id) VALUES (1)", 42)
            .expect("route");
        assert_eq!(plan.workload, WorkloadClass::Oltp);
        assert_eq!(plan.group_id, 1);
    }
}
