//! `qm cluster` — HA topology status, health probes, enterprise readiness.

use super::cluster_guide;
use super::i18n::Lang;
use std::time::Duration;

use crate::cluster::{
    apply_chaos_certification, cluster_runtime_from_config, collect_peer_addrs, enterprise_tier,
    evaluate_certification, evaluate_enterprise_readiness, probe_peers,
    readiness_score_percent, render_cluster_metrics, run_chaos_battery, topology,
    ClusterNodeConfig, NodeClient, ReadinessLevel, ShardEndpointRegistry, TopologyJoinMsg,
};
use std::net::SocketAddr;

/// Print cluster configuration and routing summary from environment.
pub fn run_status() {
    let cfg = ClusterNodeConfig::from_env();

    println!("QM Cluster HA");
    println!("  QM_CLUSTER_ENABLE:        {}", cfg.enabled);
    println!("  QM_CLUSTER_NODE_ID:       {}", cfg.node_id);
    println!("  QM_CLUSTER_META_LEADER:   {}", cfg.meta_leader_id);
    println!("  QM_CLUSTER_SHARDS/GROUP:  {}", cfg.shards_per_group);
    println!(
        "  transport:                {}",
        cfg.transport_port
            .map(|p| format!("{}:{}", cfg.bind_host, p))
            .unwrap_or_else(|| "(not set)".into())
    );
    if let Some(reg) = ShardEndpointRegistry::from_env() {
        println!("  shard endpoints:          {} entries", reg.len());
        for (shard_id, addr) in reg.iter() {
            println!("    shard {shard_id} -> {addr}");
        }
    } else {
        println!("  shard endpoints:          (default: local transport)");
    }
    println!(
        "  active:                   {}",
        if cfg.is_active() { "yes" } else { "no" }
    );
    if let Some(rt) = cluster_runtime_from_config(&cfg) {
        let eps = crate::cluster::unique_primary_endpoints(&rt);
        if eps.len() > 1 {
            println!("  ddl fan-out:              enabled ({} nodes)", eps.len());
        }
    }
    if cfg.wal_replicate {
        println!(
            "  wal replicate:            {} ({} peers{})",
            if cfg.wal_sync { "sync" } else { "async" },
            cfg.wal_peers.len(),
            if cfg.wal_peers.is_empty() {
                ", no peers configured"
            } else {
                ""
            }
        );
        for peer in &cfg.wal_peers {
            println!("    wal peer -> {peer}");
        }
    }

    if cfg.two_pc_enabled {
        println!("  cross-shard 2PC:          enabled (QM DISTRIBUTED batches)");
    }
    if cfg.failover_enabled {
        println!(
            "  failover:                 enabled (interval {}s)",
            cfg.failover_interval_secs
        );
    }
    if cfg.witness_enabled {
        println!("  witness mode:             enabled (Raft voter only)");
    } else if !cfg.witness_peers.is_empty() {
        println!(
            "  witness peers:            {} arbiter(s)",
            cfg.witness_peers.len()
        );
    }

    let Some(rt) = cluster_runtime_from_config(&cfg) else {
        if cfg.enabled {
            println!("\nCluster enabled but inactive — set QM_CLUSTER_TRANSPORT_PORT.");
        }
        return;
    };

    println!("\nRouting (sample plans):");
    for (sql, key) in [
        ("INSERT INTO users (id) VALUES (1)", 1_u64),
        (
            "SELECT id FROM docs ORDER BY embedding <-> '[0,0,0]' LIMIT 1",
            2_u64,
        ),
    ] {
        if let Some(plan) = rt.route_sql_if_active(sql, key) {
            println!(
                "  {:?} shard={} primary={:?}",
                plan.workload, plan.shard_id, plan.primary
            );
        }
    }
}

/// Live ping probes against configured shard / WAL peers.
pub fn run_health() {
    let cfg = ClusterNodeConfig::from_env();
    let peers = collect_peer_addrs(&cfg);

    println!("QM Cluster Health");
    if peers.is_empty() {
        println!("  no remote peers configured (single-node or local-only)");
        return;
    }

    let timeout = Duration::from_secs(3);
    let probes = probe_peers(&peers, timeout);
    let ok = probes.iter().filter(|p| p.ok).count();

    for probe in &probes {
        let status = if probe.ok { "UP" } else { "DOWN" };
        let rtt = probe
            .rtt_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".into());
        println!("  {}  {}  rtt={}", probe.addr, status, rtt);
    }
    println!("\n  summary: {ok}/{} peers reachable", probes.len());
}

/// Enterprise HA readiness scorecard (static + optional live probes).
pub fn run_readiness() {
    let cfg = ClusterNodeConfig::from_env();
    let checks = evaluate_enterprise_readiness(&cfg);
    let score = readiness_score_percent(&checks);
    let tier = enterprise_tier(score);

    println!("QM Enterprise HA Readiness");
    println!("  score:  {score}%");
    println!("  tier:   {tier}");
    println!();

    for check in &checks {
        println!(
            "  [{:7}] {} — {}",
            check.level.label(),
            check.title,
            check.detail
        );
    }

    let peers = collect_peer_addrs(&cfg);
    if !peers.is_empty() {
        println!("\nLive peer probes:");
        let probes = probe_peers(&peers, Duration::from_secs(3));
        let peer_ok = probes.iter().all(|p| p.ok);
        let peer_level = if peer_ok {
            ReadinessLevel::Pass
        } else if probes.iter().any(|p| p.ok) {
            ReadinessLevel::Partial
        } else {
            ReadinessLevel::Fail
        };
        for probe in &probes {
            let status = if probe.ok { "UP" } else { "DOWN" };
            println!("    {} {}", probe.addr, status);
        }
        println!(
            "\n  peer connectivity: {} ({}/{} up)",
            peer_level.label(),
            probes.iter().filter(|p| p.ok).count(),
            probes.len()
        );
    }

    println!("\nProduction multi-DC (full enterprise) status:");
    println!("  • Networked meta Raft quorum — MSG_RAFT_* over transport");
    println!("  • STONITH primary lease — QM_CLUSTER_STONITH=1");
    println!("  • WAL write quorum + durable catch-up");
    println!("  • PG BEGIN/COMMIT distributed txn — QM_CLUSTER_PG_DISTRIBUTED=1");
    println!("\nFull guide: qm cluster guide  (docs/ENTERPRISE_HA_GUIDE_VI.md)");
    println!("\nOptional hardening:");
    println!("  • Full mTLS client-auth between all nodes");
    println!("  • Cross-DC witness / region-aware routing");
    println!("  • Long production soak — scripts/cluster_production_soak.sh");
}

/// Register a shard primary (+ optional replicas) on all configured peers.
pub fn run_join(shard_id: u32, primary: SocketAddr, replicas: Vec<SocketAddr>) {
    let cfg = ClusterNodeConfig::from_env();
    let msg = TopologyJoinMsg {
        shard_id,
        primary,
        replicas: replicas.clone(),
    };

    let peers = collect_peer_addrs(&cfg);
    if peers.is_empty() {
        if let Some(rt) = cluster_runtime_from_config(&cfg) {
            topology::apply_join(&rt, shard_id, primary, replicas);
            println!("join shard {shard_id} applied locally (no remote peers)");
        } else {
            println!("cluster inactive — set QM_CLUSTER_ENABLE and transport port");
        }
        return;
    }

    for peer in peers {
        let client = NodeClient::new(0, peer);
        match client.send_topology_join_blocking(&msg) {
            Ok(()) => println!("join shard {shard_id} -> {peer} OK"),
            Err(e) => println!("join shard {shard_id} -> {peer} FAIL: {e}"),
        }
    }
}

/// Remove a shard from all configured peers.
pub fn run_leave(shard_id: u32) {
    let cfg = ClusterNodeConfig::from_env();
    let peers = collect_peer_addrs(&cfg);
    if peers.is_empty() {
        if let Some(rt) = cluster_runtime_from_config(&cfg) {
            topology::apply_leave(&rt, shard_id);
            println!("leave shard {shard_id} applied locally");
        } else {
            println!("cluster inactive");
        }
        return;
    }
    for peer in peers {
        let client = NodeClient::new(0, peer);
        match client.send_topology_leave_blocking(shard_id) {
            Ok(()) => println!("leave shard {shard_id} -> {peer} OK"),
            Err(e) => println!("leave shard {shard_id} -> {peer} FAIL: {e}"),
        }
    }
}

/// Print WAL replication lag (primary LSN vs standby applied LSN).
pub fn run_lag() {
    use crate::cluster::cluster_metrics;

    println!("QM Cluster WAL Lag");
    println!("  primary_lsn:   {}", cluster_metrics::primary_wal_lsn_metric());
    println!("  standby_lsn:   {}", cluster_metrics::standby_wal_lsn_metric());
    println!("  lag_lsn:       {}", cluster_metrics::wal_lag_lsn());
}

/// Export cluster HA metrics in Prometheus text format.
pub fn run_metrics() {
    print!("{}", render_cluster_metrics());
}

/// Enterprise certification gate — exit non-zero when not certified.
pub fn run_certify(strict: bool, chaos: bool) -> i32 {
    let cfg = ClusterNodeConfig::from_env();
    let mut report = evaluate_certification(&cfg);

    if chaos {
        println!("Running in-process chaos battery...\n");
        let chaos_results = run_chaos_battery();
        report = apply_chaos_certification(report, &chaos_results);
    }

    println!("QM Enterprise HA Certification");
    println!("  score:      {}%", report.score_percent);
    println!("  tier:       {}", report.tier);
    println!(
        "  certified:  {}",
        if report.enterprise_certified {
            "YES"
        } else {
            "NO"
        }
    );
    println!(
        "  prod-full:  {}",
        if report.production_multi_dc_full {
            "YES"
        } else {
            "NO"
        }
    );
    if chaos {
        println!(
            "  jepsen:     {}",
            if report.jepsen_certified {
                "YES"
            } else {
                "NO"
            }
        );
    }
    println!();

    for g in &report.gates {
        let mark = if g.passed { "PASS" } else { "FAIL" };
        let req = if g.required { "req" } else { "opt" };
        println!("  [{mark:4}] ({req}) {} — {}", g.title, g.detail);
    }

    if report.jepsen_certified {
        println!("\nJepsen-safe claims:");
        for claim in &report.marketing_claims {
            println!("  • {claim}");
        }
        return 0;
    }

    if report.enterprise_certified && !chaos {
        println!("\nMarketing-safe claims:");
        for claim in &report.marketing_claims {
            println!("  • {claim}");
        }
        return 0;
    }

    if strict {
        let msg = if chaos && !report.jepsen_certified {
            "Chaos certification FAILED — fix scenarios above."
        } else {
            "Certification FAILED — fix gates above before release."
        };
        println!("\n{msg}");
        return 1;
    }

    let checks = evaluate_enterprise_readiness(&cfg);
    let score = readiness_score_percent(&checks);
    println!("\nReadiness score (non-strict): {score}%");
    1
}

/// Built-in Enterprise HA / production multi-DC guide.
pub fn run_guide(lang: &Lang) {
    cluster_guide::run_cluster_guide(lang);
}
