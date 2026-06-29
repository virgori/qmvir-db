/*
 * Cluster runtime — opt-in only. Single-node deployments never attach this.
 *
 * Contract:
 *   - Default: disabled, zero routing work.
 *   - Enabled only via ClusterRuntime::enable_with_catalog() or QM_CLUSTER_ROUTER=1
 *     plus an explicit catalog attach (never implicit on engine startup).
 *   - NativeSqlEngine::execute does NOT call into this module.
 */

use parking_lot::RwLock;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use super::router::{QmRouter, RoutePlan};
use super::shard_group::ShardGroupCatalog;

/// Lazily read QM_CLUSTER_ROUTER. Unset or any value other than "1" => off.
pub fn cluster_router_env_enabled() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        matches!(
            env::var("QM_CLUSTER_ROUTER").ok().as_deref(),
            Some("1") | Some("true") | Some("TRUE")
        )
    })
}

/// Optional HA router attachment. Cheap to keep as `Arc<ClusterRuntime::disabled()>` on gateways.
pub struct ClusterRuntime {
    armed: AtomicBool,
    router: RwLock<Option<QmRouter>>,
}

impl ClusterRuntime {
    /// Default for single-node: no catalog, no routing, one relaxed atomic load if queried.
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self {
            armed: AtomicBool::new(false),
            router: RwLock::new(None),
        })
    }

    pub fn with_catalog(catalog: ShardGroupCatalog) -> Arc<Self> {
        Arc::new(Self {
            armed: AtomicBool::new(false),
            router: RwLock::new(Some(QmRouter::new(catalog))),
        })
    }

    /// Arm routing only when both env (optional) and explicit enable are set.
    pub fn enable(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub fn disable(&self) {
        self.armed.store(false, Ordering::Release);
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.armed.load(Ordering::Acquire) && self.router.read().is_some()
    }

    /// Hot path for gateways: single branch, no work when inactive.
    #[inline]
    pub fn route_sql_if_active(&self, sql: &str, shard_key: u64) -> Option<RoutePlan> {
        if !self.armed.load(Ordering::Relaxed) {
            return None;
        }
        self.route_sql_cold(sql, shard_key)
    }

    #[cold]
    fn route_sql_cold(&self, sql: &str, shard_key: u64) -> Option<RoutePlan> {
        if !self.is_active() {
            return None;
        }
        let router = self.router.read();
        router.as_ref()?.route_sql(sql, shard_key).ok()
    }

    pub fn replace_catalog(&self, catalog: ShardGroupCatalog) {
        *self.router.write() = Some(QmRouter::new(catalog));
    }

    /// Snapshot catalog for DDL fan-out / admin (cold path).
    pub fn router_snapshot(&self) -> Option<ShardGroupCatalog> {
        if !self.is_active() {
            return None;
        }
        self.router.read().as_ref().map(|r| r.catalog().clone())
    }

    /// Analytics read path: return replica endpoint when workload is OLAP/vector.
    #[inline]
    pub fn route_analytics_read_if_active(
        &self,
        sql: &str,
        shard_key: u64,
    ) -> Option<(RoutePlan, Option<std::net::SocketAddr>)> {
        if !self.armed.load(Ordering::Relaxed) {
            return None;
        }
        if !self.is_active() {
            return None;
        }
        let router = self.router.read();
        let router = router.as_ref()?;
        router.route_analytics_read(sql, shard_key).ok()
    }
}

/// Gateway helper: env alone is never enough to route off-node.
#[inline]
pub fn may_forward_to_remote(runtime: &ClusterRuntime) -> bool {
    cluster_router_env_enabled() && runtime.is_active()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::meta_cluster::MetaCluster;
    use crate::cluster::shard_group::ShardGroupKind;

    #[test]
    fn disabled_runtime_never_routes() {
        let rt = ClusterRuntime::disabled();
        assert!(!rt.is_active());
        assert!(rt.route_sql_if_active("INSERT INTO t VALUES (1)", 1).is_none());
    }

    #[test]
    fn catalog_without_enable_stays_inactive() {
        let cluster = MetaCluster::new(&[1, 2, 3]);
        cluster.bootstrap_default_groups(1, 4);
        let rt = ClusterRuntime::with_catalog(cluster.catalog_on(1).unwrap());
        assert!(!rt.is_active());
        assert!(rt.route_sql_if_active("SELECT 1", 0).is_none());
    }

    #[test]
    fn enable_without_catalog_stays_inactive() {
        let rt = ClusterRuntime::disabled();
        rt.enable();
        assert!(!rt.is_active());
    }

    #[test]
    fn enabled_runtime_routes_oltp() {
        let cluster = MetaCluster::new(&[1, 2, 3]);
        cluster.bootstrap_default_groups(1, 4);
        let rt = ClusterRuntime::with_catalog(cluster.catalog_on(1).unwrap());
        rt.enable();
        let plan = rt
            .route_sql_if_active("INSERT INTO users (id) VALUES (1)", 99)
            .expect("route");
        assert_eq!(plan.group_id, 1);
        assert_eq!(plan.workload, super::super::router::WorkloadClass::Oltp);
        assert!(cluster.catalog_on(1).unwrap().group_for_kind(ShardGroupKind::Vector).is_some());
    }
}
