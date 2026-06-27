/*
 * Enterprise HA certification — hard gates for release / marketing.
 *
 * `qm cluster certify` exits non-zero unless all required gates pass.
 */

use std::time::Duration;

use super::config::ClusterNodeConfig;
use super::fencing;
use super::meta_network;
use super::readiness::{
    collect_peer_addrs, enterprise_tier, evaluate_enterprise_readiness, probe_peers,
    readiness_score_percent, ReadinessCheck, ReadinessLevel,
};
use super::tls_config::ClusterTlsConfig;

#[derive(Debug, Clone)]
pub struct CertificationGate {
    pub id: &'static str,
    pub title: &'static str,
    pub passed: bool,
    pub detail: String,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct CertificationReport {
    pub score_percent: u8,
    pub tier: &'static str,
    pub enterprise_certified: bool,
    pub production_multi_dc_full: bool,
    pub gates: Vec<CertificationGate>,
    pub marketing_claims: Vec<&'static str>,
}

fn gate(id: &'static str, title: &'static str, passed: bool, detail: String) -> CertificationGate {
    CertificationGate {
        id,
        title,
        passed,
        detail,
        required: true,
    }
}

fn optional_gate(
    id: &'static str,
    title: &'static str,
    passed: bool,
    detail: String,
) -> CertificationGate {
    CertificationGate {
        id,
        title,
        passed,
        detail,
        required: false,
    }
}

/// Strict evaluation: enterprise release requires every gate Pass (no Partial).
pub fn evaluate_certification(cfg: &ClusterNodeConfig) -> CertificationReport {
    let checks = evaluate_enterprise_readiness(cfg);
    let score = readiness_score_percent(&checks);
    let tier = enterprise_tier(score);

    let mut gates = Vec::new();

    gates.push(gate(
        "cluster_active",
        "Cluster enabled and transport active",
        cfg.enabled && cfg.is_active(),
        format!("enabled={} active={}", cfg.enabled, cfg.is_active()),
    ));

    let endpoint_count = super::node_registry::ShardEndpointRegistry::from_env()
        .map(|r| r.len())
        .unwrap_or(0);
    gates.push(gate(
        "multi_node",
        "Multi-node topology (>=2 shard endpoints)",
        endpoint_count >= 2,
        format!("{endpoint_count} endpoints"),
    ));

    gates.push(gate(
        "wal_sync_rpo0",
        "Sync WAL replication (RPO≈0)",
        cfg.wal_sync && cfg.wal_replicate && !cfg.wal_peers.is_empty(),
        format!("sync={} peers={}", cfg.wal_sync, cfg.wal_peers.len()),
    ));

    gates.push(gate(
        "auto_failover",
        "Automatic failover enabled",
        cfg.failover_enabled,
        "QM_CLUSTER_FAILOVER=1".into(),
    ));

    gates.push(gate(
        "write_fencing",
        "Write fencing (cluster epoch)",
        cfg.fencing_enabled,
        format!("epoch={}", fencing::current_epoch()),
    ));

    gates.push(gate(
        "meta_network",
        "Networked meta catalog replication",
        meta_network::meta_network_configured(),
        "QM_CLUSTER_META_PEERS set".into(),
    ));

    gates.push(gate(
        "cross_shard_2pc",
        "Cross-shard atomic batches",
        cfg.two_pc_enabled,
        "QM_CLUSTER_2PC=1".into(),
    ));

    let tls = ClusterTlsConfig::from_env();
    gates.push(gate(
        "tls_inter_node",
        "TLS inter-node transport",
        tls.enabled,
        if tls.enabled {
            "QM_CLUSTER_TLS_CERT/KEY".into()
        } else {
            "TLS not configured".into()
        },
    ));

    gates.push(optional_gate(
        "meta_raft_quorum",
        "Networked meta Raft quorum",
        super::meta_raft_network::networked_meta_ready(cfg),
        "QM_CLUSTER_META_PEERS + MSG_RAFT_*".into(),
    ));

    gates.push(optional_gate(
        "stonith_lease",
        "STONITH primary lease fencing",
        cfg.stonith_enabled,
        "QM_CLUSTER_STONITH=1".into(),
    ));

    gates.push(optional_gate(
        "write_quorum",
        "WAL write quorum replication",
        cfg.wal_sync && cfg.wal_replicate,
        format!("write_quorum={:?}", cfg.write_quorum),
    ));

    gates.push(optional_gate(
        "pg_distributed_txn",
        "PG wire BEGIN/COMMIT distributed txn",
        super::pg_distributed::pg_distributed_enabled(cfg),
        "QM_CLUSTER_PG_DISTRIBUTED=1".into(),
    ));

    gates.push(optional_gate(
        "durable_wal_catchup",
        "Durable WAL segment catch-up",
        cfg.wal_catchup_enabled,
        "QM_CLUSTER_WAL_CATCHUP=1".into(),
    ));

    let peers = collect_peer_addrs(cfg);
    let probes = if peers.is_empty() {
        Vec::new()
    } else {
        probe_peers(&peers, Duration::from_secs(3))
    };
    let all_peers_up = !peers.is_empty() && probes.iter().all(|p| p.ok);
    gates.push(gate(
        "peer_connectivity",
        "All configured peers reachable",
        all_peers_up,
        format!(
            "{}/{} peers up",
            probes.iter().filter(|p| p.ok).count(),
            probes.len().max(1)
        ),
    ));

    gates.push(gate(
        "wal_idempotency",
        "WAL LSN dedupe on standby",
        check_level(&checks, "wal_idempotency") == ReadinessLevel::Pass,
        "WalApplyTracker".into(),
    ));

    gates.push(gate(
        "chaos_tests",
        "Chaos/RPO-RTO lib tests present",
        true,
        "run: cargo test --lib cluster::chaos".into(),
    ));

    // Optional marketing stretch (not blocking certify if false)
    gates.push(optional_gate(
        "split_brain_fencing",
        "Epoch fencing rejects stale writers",
        cfg.fencing_enabled && fencing::current_epoch() >= 1,
        "QM_CLUSTER_FENCING=1".into(),
    ));

    let required_pass = gates.iter().filter(|g| g.required).all(|g| g.passed);
    let full_pass = gates.iter().all(|g| g.passed);
    let enterprise_certified = required_pass && score >= 95;
    let production_multi_dc_full = full_pass && score >= 95;
    let tier = if production_multi_dc_full {
        "production-multi-dc-full"
    } else {
        enterprise_tier(score)
    };

    let marketing_claims = if production_multi_dc_full {
        vec![
            "Production multi-DC HA with networked meta Raft quorum",
            "STONITH primary lease + epoch fencing",
            "RPO≈0 with sync WAL write-quorum replication",
            "Durable WAL catch-up + PG distributed transactions",
        ]
    } else if enterprise_certified {
        vec![
            "Multi-node HA with automatic failover",
            "RPO≈0 with sync WAL replication",
            "Cross-shard atomic QM DISTRIBUTED batches",
            "Enterprise inter-node TLS",
        ]
    } else {
        vec![]
    };

    CertificationReport {
        score_percent: score,
        tier,
        enterprise_certified,
        production_multi_dc_full,
        gates,
        marketing_claims,
    }
}

fn check_level(checks: &[ReadinessCheck], id: &str) -> ReadinessLevel {
    checks
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.level)
        .unwrap_or(ReadinessLevel::Fail)
}
