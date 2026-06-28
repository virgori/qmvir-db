/*
 * Witness / arbiter node — lightweight Raft voter for 2-DC tie-break.
 *
 * Witness node (no shard data):
 *   QM_CLUSTER_WITNESS=1
 *   QM_CLUSTER_ENABLE=1
 *   QM_CLUSTER_TRANSPORT_PORT=55443
 *   QM_CLUSTER_META_PEERS=data-a:55441,data-b:55442
 *
 * Data nodes include witness in quorum fan-out:
 *   QM_CLUSTER_WITNESS_PEERS=witness:55443
 */

use std::net::SocketAddr;

use super::config::ClusterNodeConfig;
use super::meta_network::{meta_network_configured, parse_meta_peers};

pub fn parse_witness_peers() -> Vec<SocketAddr> {
    std::env::var("QM_CLUSTER_WITNESS_PEERS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn witness_mode_from_env() -> bool {
    matches!(
        std::env::var("QM_CLUSTER_WITNESS").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// All remote voters for meta Raft (meta catalog peers + witness arbiters).
pub fn voter_peers(cfg: &ClusterNodeConfig) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for addr in parse_meta_peers().into_iter().chain(cfg.witness_peers.iter().copied()) {
        if cfg.local_addr.is_some_and(|l| l == addr) {
            continue;
        }
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    out
}

pub fn total_voters(cfg: &ClusterNodeConfig) -> usize {
    voter_peers(cfg).len() + 1
}

pub fn quorum_size(total_voters: usize) -> usize {
    total_voters / 2 + 1
}

pub fn required_votes(cfg: &ClusterNodeConfig) -> usize {
    quorum_size(total_voters(cfg))
}

/// Witness tie-breaker configured (peers on data nodes or witness mode on this node).
pub fn witness_configured(cfg: &ClusterNodeConfig) -> bool {
    cfg.witness_enabled || !cfg.witness_peers.is_empty()
}

/// Ready for production 2-DC: meta network + at least one witness voter.
pub fn witness_quorum_ready(cfg: &ClusterNodeConfig) -> bool {
    meta_network_configured() && witness_configured(cfg) && cfg.is_active()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn two_dc_plus_witness_needs_two_votes() {
        let cfg = ClusterNodeConfig {
            enabled: true,
            transport_port: Some(55441),
            local_addr: Some(addr(55441)),
            witness_peers: vec![addr(55443)],
            ..ClusterNodeConfig::default()
        };
        assert_eq!(voter_peers(&cfg).len(), 1);
        assert_eq!(total_voters(&cfg), 2);
        assert_eq!(required_votes(&cfg), 2);

        let cfg3 = ClusterNodeConfig {
            witness_peers: vec![addr(55443)],
            local_addr: Some(addr(55441)),
            ..cfg.clone()
        };
        unsafe {
            std::env::set_var("QM_CLUSTER_META_PEERS", "127.0.0.1:55442");
        }
        let voters = voter_peers(&cfg3);
        assert_eq!(voters.len(), 2);
        assert_eq!(total_voters(&cfg3), 3);
        assert_eq!(required_votes(&cfg3), 2);
        unsafe {
            std::env::remove_var("QM_CLUSTER_META_PEERS");
        }
    }
}
