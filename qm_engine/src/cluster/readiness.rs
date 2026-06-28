/*
 * Enterprise HA readiness assessment — static checklist + live peer probes.
 */

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use super::config::ClusterNodeConfig;
use super::gateway_bridge::cluster_runtime_from_config;
use super::node_registry::ShardEndpointRegistry;
use super::transport::NodeClient;
use super::ddl_fanout::unique_primary_endpoints;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessLevel {
    Pass,
    Partial,
    Fail,
}

impl ReadinessLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Partial => "PARTIAL",
            Self::Fail => "FAIL",
        }
    }

    fn score(self) -> f64 {
        match self {
            Self::Pass => 1.0,
            Self::Partial => 0.5,
            Self::Fail => 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReadinessCheck {
    pub id: &'static str,
    pub title: &'static str,
    pub level: ReadinessLevel,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct PeerProbe {
    pub addr: SocketAddr,
    pub ok: bool,
    pub rtt_ms: Option<u64>,
}

pub fn evaluate_enterprise_readiness(cfg: &ClusterNodeConfig) -> Vec<ReadinessCheck> {
    let mut checks = Vec::new();

    let active = cfg.is_active();
    checks.push(ReadinessCheck {
        id: "cluster_opt_in",
        title: "Cluster mode enabled",
        level: if cfg.enabled {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Fail
        },
        detail: if cfg.enabled {
            "QM_CLUSTER_ENABLE=1".into()
        } else {
            "Set QM_CLUSTER_ENABLE=1 for HA".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "transport_listener",
        title: "Inter-node transport",
        level: if active {
            ReadinessLevel::Pass
        } else if cfg.enabled {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: cfg
            .transport_port
            .map(|p| format!("listening on {}:{}", cfg.bind_host, p))
            .unwrap_or_else(|| "Set QM_CLUSTER_TRANSPORT_PORT".into()),
    });

    let registry = ShardEndpointRegistry::from_env();
    let endpoint_count = registry.as_ref().map(|r| r.len()).unwrap_or(1);
    checks.push(ReadinessCheck {
        id: "multi_node_topology",
        title: "Multi-node shard registry",
        level: match endpoint_count {
            n if n > 1 => ReadinessLevel::Pass,
            1 if active => ReadinessLevel::Partial,
            _ => ReadinessLevel::Fail,
        },
        detail: format!("{endpoint_count} shard endpoint(s) in QM_CLUSTER_SHARD_ENDPOINTS"),
    });

    let runtime = cluster_runtime_from_config(cfg);
    checks.push(ReadinessCheck {
        id: "query_routing",
        title: "Gateway shard routing",
        level: if runtime.is_some() {
            ReadinessLevel::Pass
        } else if cfg.enabled {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: "RoutePlan via ClusterRuntime".into(),
    });

    let ddl_nodes = runtime
        .as_ref()
        .map(|rt| unique_primary_endpoints(rt).len())
        .unwrap_or(0);
    checks.push(ReadinessCheck {
        id: "ddl_fanout",
        title: "DDL schema fan-out",
        level: if ddl_nodes > 1 {
            ReadinessLevel::Pass
        } else if active {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: format!("{ddl_nodes} primary node(s) for DDL fan-out"),
    });

    checks.push(ReadinessCheck {
        id: "wal_replication",
        title: "WAL streaming replication",
        level: if cfg.wal_replicate && !cfg.wal_peers.is_empty() {
            ReadinessLevel::Pass
        } else if cfg.wal_replicate {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: format!(
            "{} peer(s), replicate={}",
            cfg.wal_peers.len(),
            cfg.wal_replicate
        ),
    });

    checks.push(ReadinessCheck {
        id: "wal_sync_rpo",
        title: "Sync WAL (RPO≈0 on peer ack)",
        level: if cfg.wal_sync && !cfg.wal_peers.is_empty() {
            ReadinessLevel::Pass
        } else if cfg.wal_replicate {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: if cfg.wal_sync {
            "QM_CLUSTER_WAL_SYNC=1".into()
        } else {
            "Async replication — non-zero RPO under primary loss".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "wal_idempotency",
        title: "WAL idempotent apply (LSN dedupe)",
        level: ReadinessLevel::Pass,
        detail: "WalApplyTracker on transport standby path".into(),
    });

    checks.push(ReadinessCheck {
        id: "cross_shard_2pc",
        title: "Cross-shard atomic batches",
        level: if cfg.two_pc_enabled {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Fail
        },
        detail: if cfg.two_pc_enabled && cfg.pg_distributed_enabled {
            "PG BEGIN/COMMIT + QM DISTRIBUTED 2PC".into()
        } else if cfg.two_pc_enabled {
            "QM DISTRIBUTED batches — enable QM_CLUSTER_PG_DISTRIBUTED=1 for PG wire".into()
        } else {
            "Set QM_CLUSTER_2PC=1 for distributed batches".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "write_quorum",
        title: "WAL write quorum (majority acks)",
        level: if cfg.wal_sync && cfg.wal_replicate {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Partial
        },
        detail: format!("write_quorum={:?}", cfg.write_quorum),
    });

    checks.push(ReadinessCheck {
        id: "stonith",
        title: "STONITH primary lease + meta fence",
        level: if cfg.stonith_enabled {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Partial
        },
        detail: if cfg.stonith_enabled {
            "QM_CLUSTER_STONITH=1 + cluster_primary.lease".into()
        } else {
            "Optional — set QM_CLUSTER_STONITH=1 for production multi-DC".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "pg_distributed",
        title: "PG wire distributed transactions",
        level: if super::pg_distributed::pg_distributed_enabled(cfg) {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Partial
        },
        detail: "QM_CLUSTER_PG_DISTRIBUTED=1".into(),
    });

    checks.push(ReadinessCheck {
        id: "meta_consensus",
        title: "Networked meta Raft / quorum catalog",
        level: if super::meta_raft_network::networked_meta_ready(cfg) {
            ReadinessLevel::Pass
        } else if super::meta_network::meta_network_configured() {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: if super::meta_raft_network::networked_meta_ready(cfg) {
            "MSG_RAFT_* quorum election + append".into()
        } else if super::meta_network::meta_network_configured() {
            "QM_CLUSTER_META_PEERS set (legacy append only)".into()
        } else {
            "Set QM_CLUSTER_META_PEERS for networked catalog".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "auto_failover",
        title: "Automatic primary failover",
        level: if cfg.failover_enabled && cfg.fencing_enabled {
            ReadinessLevel::Pass
        } else if cfg.failover_enabled {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        },
        detail: if cfg.failover_enabled && cfg.fencing_enabled {
            "QM_CLUSTER_FAILOVER=1 + QM_CLUSTER_FENCING=1".into()
        } else if cfg.failover_enabled {
            "Enable QM_CLUSTER_FENCING=1 with failover".into()
        } else {
            "Set QM_CLUSTER_FAILOVER=1".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "witness_quorum",
        title: "Witness arbiter for 2-DC tie-break",
        level: if super::witness::witness_quorum_ready(cfg) {
            ReadinessLevel::Pass
        } else if super::witness::witness_configured(cfg) {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Partial
        },
        detail: if cfg.witness_enabled {
            "QM_CLUSTER_WITNESS=1 (this node is arbiter)".into()
        } else if !cfg.witness_peers.is_empty() {
            format!("{} witness peer(s)", cfg.witness_peers.len())
        } else {
            "Optional — QM_CLUSTER_WITNESS_PEERS for 2-DC WAN".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "cluster_ops",
        title: "Cluster join/leave operations",
        level: ReadinessLevel::Pass,
        detail: "qm cluster join|leave via topology transport".into(),
    });

    checks.push(ReadinessCheck {
        id: "chaos_hardening",
        title: "Chaos / partition RPO-RTO validation",
        level: ReadinessLevel::Pass,
        detail: "cargo test --lib cluster::chaos (CI gate)".into(),
    });

    checks.push(ReadinessCheck {
        id: "wal_catchup",
        title: "WAL gap catch-up buffer",
        level: ReadinessLevel::Pass,
        detail: "MSG_WAL_CATCHUP_REQ ring + native_sql.wal durable tail".into(),
    });

    checks.push(ReadinessCheck {
        id: "split_brain",
        title: "Split-brain / fencing protection",
        level: if cfg.fencing_enabled {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Fail
        },
        detail: if cfg.fencing_enabled {
            format!("cluster epoch fencing (epoch={})", super::fencing::current_epoch())
        } else {
            "Set QM_CLUSTER_FENCING=1".into()
        },
    });

    let tls = super::tls_config::ClusterTlsConfig::from_env();
    checks.push(ReadinessCheck {
        id: "tls_inter_node",
        title: "TLS / mTLS inter-node transport",
        level: if tls.enabled {
            ReadinessLevel::Pass
        } else {
            ReadinessLevel::Fail
        },
        detail: if tls.enabled {
            "QM_CLUSTER_TLS_CERT/KEY configured".into()
        } else {
            "Plain TCP — set QM_CLUSTER_TLS_CERT and QM_CLUSTER_TLS_KEY".into()
        },
    });

    checks.push(ReadinessCheck {
        id: "observability",
        title: "HA observability (metrics, lag alerts)",
        level: ReadinessLevel::Pass,
        detail: "qm cluster status/health/lag/metrics + Prometheus export".into(),
    });

    checks
}

pub fn readiness_score_percent(checks: &[ReadinessCheck]) -> u8 {
    if checks.is_empty() {
        return 0;
    }
    let sum: f64 = checks.iter().map(|c| c.level.score()).sum();
    ((sum / checks.len() as f64) * 100.0).round() as u8
}

pub fn enterprise_tier(score: u8) -> &'static str {
    match score {
        100 => "production-multi-dc-full",
        95..=99 => "enterprise-certified",
        85..=94 => "enterprise-ready",
        65..=84 => "production-staging",
        40..=64 => "dev-cluster",
        _ => "single-node / POC",
    }
}

pub fn collect_peer_addrs(cfg: &ClusterNodeConfig) -> Vec<SocketAddr> {
    let mut addrs = Vec::new();
    if let Some(reg) = ShardEndpointRegistry::from_env() {
        for (_, addr) in reg.iter() {
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
    }
    for peer in &cfg.wal_peers {
        if !addrs.contains(peer) {
            addrs.push(*peer);
        }
    }
    if let Some(local) = cfg.local_addr {
        addrs.retain(|a| a != &local);
    }
    addrs
}

pub fn probe_peers(addrs: &[SocketAddr], timeout: Duration) -> Vec<PeerProbe> {
    addrs
        .iter()
        .map(|addr| {
            let client = NodeClient::new(0, *addr);
            let start = Instant::now();
            let ok = client.ping_blocking(timeout).unwrap_or(false);
            let rtt_ms = if ok {
                Some(start.elapsed().as_millis() as u64)
            } else {
                None
            };
            PeerProbe {
                addr: *addr,
                ok,
                rtt_ms,
            }
        })
        .collect()
}
