/*
 * Cluster node configuration — opt-in via environment (never implied on engine start).
 */

use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use super::replica::ConsistencyLevel;

/// Parsed HA settings for a data node. Default: disabled.
#[derive(Debug, Clone)]
pub struct ClusterNodeConfig {
    pub enabled: bool,
    pub node_id: u32,
    pub transport_port: Option<u16>,
    pub local_addr: Option<SocketAddr>,
    pub meta_leader_id: u32,
    pub shards_per_group: u32,
    pub bind_host: IpAddr,
    /// Ship DML to QM_CLUSTER_WAL_PEERS after local primary write.
    pub wal_replicate: bool,
    /// Block until WAL peers ack (stronger durability).
    pub wal_sync: bool,
    pub wal_peers: Vec<SocketAddr>,
    /// Enable durable + ring WAL catch-up on standby heal.
    pub wal_catchup_enabled: bool,
    /// Write ack policy for WAL replication (majority vs all).
    pub write_quorum: ConsistencyLevel,
    /// Enable `QM DISTRIBUTED` cross-shard 2PC batches.
    pub two_pc_enabled: bool,
    /// PG wire BEGIN/COMMIT distributed txn (requires 2PC).
    pub pg_distributed_enabled: bool,
    /// Background health probe + automatic primary promotion.
    pub failover_enabled: bool,
    pub failover_interval_secs: u64,
    /// Write fencing — reject stale primary writes after promote.
    pub fencing_enabled: bool,
    /// STONITH primary lease before accepting writes / after promote.
    pub stonith_enabled: bool,
    pub stonith_lease_secs: u64,
    /// Shared secret for forwarded auth user attestation (optional).
    pub forward_secret: Option<String>,
}

impl Default for ClusterNodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_id: 1,
            transport_port: None,
            local_addr: None,
            meta_leader_id: 1,
            shards_per_group: 4,
            bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            wal_replicate: false,
            wal_sync: false,
            wal_peers: Vec::new(),
            wal_catchup_enabled: false,
            write_quorum: ConsistencyLevel::Quorum,
            two_pc_enabled: false,
            pg_distributed_enabled: false,
            failover_enabled: false,
            failover_interval_secs: 5,
            fencing_enabled: false,
            stonith_enabled: false,
            stonith_lease_secs: 30,
            forward_secret: None,
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn parse_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_host_port(host: IpAddr, port: u16) -> SocketAddr {
    SocketAddr::new(host, port)
}

impl ClusterNodeConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        cfg.enabled = env_flag("QM_CLUSTER_ENABLE");
        cfg.node_id = parse_u32("QM_CLUSTER_NODE_ID", 1);
        cfg.meta_leader_id = parse_u32("QM_CLUSTER_META_LEADER", cfg.node_id);
        cfg.shards_per_group = parse_u32("QM_CLUSTER_SHARDS_PER_GROUP", 4).max(1);

        if let Ok(host) = env::var("QM_CLUSTER_HOST") {
            if let Ok(ip) = host.parse() {
                cfg.bind_host = ip;
            }
        }

        if let Ok(port_s) = env::var("QM_CLUSTER_TRANSPORT_PORT") {
            if let Ok(port) = port_s.parse::<u16>() {
                cfg.transport_port = Some(port);
                cfg.local_addr = Some(parse_host_port(cfg.bind_host, port));
            }
        }

        if let Ok(addr_s) = env::var("QM_CLUSTER_LOCAL_ADDR") {
            if let Ok(addr) = addr_s.parse() {
                cfg.local_addr = Some(addr);
                cfg.transport_port = Some(addr.port());
            }
        }

        cfg.wal_replicate = env_flag("QM_CLUSTER_WAL_REPLICATE");
        cfg.wal_sync = env_flag("QM_CLUSTER_WAL_SYNC");
        if let Ok(peers) = env::var("QM_CLUSTER_WAL_PEERS") {
            for part in peers.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if let Ok(addr) = part.parse::<SocketAddr>() {
                    cfg.wal_peers.push(addr);
                }
            }
        }
        if !cfg.wal_peers.is_empty() && !cfg.wal_replicate {
            cfg.wal_replicate = true;
        }

        cfg.two_pc_enabled = env_flag("QM_CLUSTER_2PC");
        cfg.pg_distributed_enabled = env_flag("QM_CLUSTER_PG_DISTRIBUTED");
        cfg.wal_catchup_enabled = env_flag("QM_CLUSTER_WAL_CATCHUP") || cfg.wal_replicate;
        if env_flag("QM_CLUSTER_WRITE_QUORUM") {
            cfg.write_quorum = ConsistencyLevel::Quorum;
        } else if env_flag("QM_CLUSTER_WRITE_ALL") {
            cfg.write_quorum = ConsistencyLevel::All;
        }
        cfg.failover_enabled = env_flag("QM_CLUSTER_FAILOVER");
        cfg.failover_interval_secs = parse_u32("QM_CLUSTER_FAILOVER_INTERVAL_SECS", 5) as u64;
        cfg.fencing_enabled = env_flag("QM_CLUSTER_FENCING") || cfg.failover_enabled;
        cfg.stonith_enabled = env_flag("QM_CLUSTER_STONITH");
        cfg.stonith_lease_secs = parse_u32("QM_CLUSTER_STONITH_LEASE_SECS", 30) as u64;
        cfg.forward_secret = env::var("QM_CLUSTER_FORWARD_SECRET").ok();

        cfg
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.transport_port.is_some()
    }
}
