//! Built-in Enterprise HA / production multi-DC guide (offline text).

use super::i18n::Lang;

pub fn run_cluster_guide(lang: &Lang) {
    print!("{}", cluster_guide_body(lang));
}

pub fn cluster_guide_body(lang: &Lang) -> String {
    match lang {
        Lang::Vi => guide_vi(),
        _ => guide_en(),
    }
}

fn guide_en() -> String {
    format!(
        "\
QMvir v{} — Enterprise HA & Production Multi-DC Guide

OVERVIEW
  QMvir cluster mode is opt-in (QM_CLUSTER_ENABLE=1). Without it, the engine
  runs single-node — cluster modules are linked but inactive.

CERTIFICATION LADDER
  community                  Single-node (default)
  enterprise-certified       Required gates + score >= 95%
  production-multi-dc-full   All gates incl. O–R optional
  jepsen-certified           Roadmap — formal chaos battery

  Check:  qm cluster certify
  Score:  qm cluster readiness

WRITE QUORUM (WAL ack policy)
  One     W=1 ack                    (default)
  Quorum  QM_CLUSTER_WRITE_QUORUM=1  W = floor(N/2)+1
  All     QM_CLUSTER_WRITE_ALL=1     W = N peers
  RPO≈0:  QM_CLUSTER_WAL_SYNC=1 + required W acks before client ack

CONSISTENCY (current behaviour)
  Single-shard write   Strong — sync WAL to W peers
  Cross-shard          Atomic — 2PC / QM DISTRIBUTED
  Standby apply        Exactly-once — WalApplyTracker LSN dedupe
  Failover             Stale primary fenced — epoch + optional STONITH

SLA (measured in CI / targets)
  RPO          0 (sync WAL, lag_lsn=0)
  Failover RTO <5s test bound; target <10s production
  WAL lag      target <100ms LAN — qm cluster lag

CHAOS (today)
  cargo test --lib cluster::chaos
  qm cluster certify --chaos     → jepsen-certified tier
  enterprise_ha_gate.sh / cluster_production_soak.sh

WITNESS (2-DC tie-break)
  Data:  QM_CLUSTER_WITNESS_PEERS=host:port
  Node:  QM_CLUSTER_WITNESS=1 (Raft voter only, no shard data)
  2 data + 1 witness = 3 voters, need 2 grants

SLA METRICS (qm cluster metrics)
  qmvir_cluster_failover_rto_ms_*  histogram
  qmvir_cluster_wal_lag_p99        rolling p99

REQUIRED GATES (enterprise-certified)
  • Multi-node topology (>=2 shard endpoints)
  • Sync WAL replication (RPO≈0)
  • Automatic failover + write fencing (epoch)
  • Networked meta catalog (QM_CLUSTER_META_PEERS)
  • Cross-shard 2PC (QM_CLUSTER_2PC=1)
  • TLS inter-node transport
  • Live peer connectivity
  • WAL idempotent apply on standby

OPTIONAL GATES (production-multi-dc-full)
  • Networked meta Raft quorum (MSG_RAFT_*)
  • STONITH primary lease (QM_CLUSTER_STONITH=1)
  • WAL write quorum replication
  • PG wire BEGIN/COMMIT distributed txn (QM_CLUSTER_PG_DISTRIBUTED=1)
  • Durable WAL segment catch-up (QM_CLUSTER_WAL_CATCHUP=1)

TWO-NODE MINIMUM (enterprise-certified)
  # Primary (node A)
  export QM_CLUSTER_ENABLE=1
  export QM_CLUSTER_NODE_ID=1
  export QM_CLUSTER_TRANSPORT_PORT=55441
  export QM_CLUSTER_LOCAL_ADDR=127.0.0.1:55441
  export QM_CLUSTER_SHARD_ENDPOINTS=0=127.0.0.1:55441,1=127.0.0.1:55441,2=127.0.0.1:55442,3=127.0.0.1:55442
  export QM_CLUSTER_SHARD_REPLICAS=0=127.0.0.1:55442,2=127.0.0.1:55442
  export QM_CLUSTER_WAL_REPLICATE=1
  export QM_CLUSTER_WAL_SYNC=1
  export QM_CLUSTER_WAL_PEERS=127.0.0.1:55442
  export QM_CLUSTER_FAILOVER=1
  export QM_CLUSTER_FENCING=1
  export QM_CLUSTER_2PC=1
  export QM_CLUSTER_META_PEERS=127.0.0.1:55442
  export QM_CLUSTER_TLS_CERT=/path/to/cert.pem
  export QM_CLUSTER_TLS_KEY=/path/to/key.pem
  qm --data-dir ./data-a start --admin-password secret

  # Standby (node B) — mirror env with NODE_ID=2, ports 55442, WAL peer -> A

PRODUCTION MULTI-DC (add on top of enterprise)
  export QM_CLUSTER_STONITH=1
  export QM_CLUSTER_STONITH_LEASE_SECS=30
  export QM_CLUSTER_WRITE_QUORUM=1
  export QM_CLUSTER_WAL_CATCHUP=1
  export QM_CLUSTER_PG_DISTRIBUTED=1
  # STONITH requires persistent --data-dir (not in-memory engine)

CLI COMMANDS
  qm cluster status       Topology, WAL, failover flags
  qm cluster health       Live ping to shard / WAL peers
  qm cluster readiness    Readiness scorecard (% + tier)
  qm cluster certify      Certification gates (exit 1 if not certified)
  qm cluster lag          WAL primary vs standby LSN
  qm cluster metrics      Prometheus text export
  qm cluster join/leave   Register or remove shard endpoints
  qm cluster guide        This guide

VALIDATION (before release / deploy)
  bash scripts/enterprise_ha_gate.sh              # CI gate (7 steps)
  bash scripts/cluster_production_soak.sh --quick # multi-DC smoke
  bash scripts/cluster_production_soak.sh         # full soak (30 min+)

PUBLISH / RELEASE
  • Bump version in qm_engine/Cargo.toml
  • CI Release Gate must pass on main
  • Tag vX.Y.Z and push; GitHub Actions builds release binaries:
      git tag vX.Y.Z
      git push origin vX.Y.Z
  • enterprise-certified ships with default HA env; production-multi-dc-full
    features are opt-in at deploy time — they do not block binary publish.

DOCS
  docs/en/ENTERPRISE_HA_GUIDE.md
  docs/vi/ENTERPRISE_HA_GUIDE.md

See also: qm guide notes | qm guide cli
",
        env!("CARGO_PKG_VERSION")
    )
}

fn guide_vi() -> String {
    format!(
        "\
QMvir v{} — Huong dan Enterprise HA & Production Multi-DC

TONG QUAN
  Che do cluster bat bang QM_CLUSTER_ENABLE=1. Khong set = single-node.

THANG CHUNG NHAN
  community → enterprise-certified → production-multi-dc-full
  jepsen-certified (roadmap — qm cluster certify --chaos)

WRITE QUORUM
  One=1 ack | Quorum=QM_CLUSTER_WRITE_QUORUM (W=⌊N/2⌋+1)
  All=QM_CLUSTER_WRITE_ALL (W=N) | RPO≈0 cần WAL_SYNC

CONSISTENCY
  Single-shard: manh (sync WAL) | Cross-shard: atomic (2PC)
  Standby: exactly-once LSN | Failover: epoch fence + STONITH (opt)

SLA: RPO=0 | RTO <10s muc tieu | WAL lag <100ms LAN

CHAOS: cluster::chaos + enterprise_ha_gate (co)
       --chaos / Jepsen tier (roadmap)

WITNESS: chua co v6.x — can N>=3 voter hoac etcd

GATE BAT BUOC (enterprise-certified)
  • Topology >= 2 shard endpoint
  • WAL sync replication (RPO≈0)
  • Failover tu dong + fencing epoch
  • Meta catalog qua mang (QM_CLUSTER_META_PEERS)
  • Cross-shard 2PC (QM_CLUSTER_2PC=1)
  • TLS giua cac node
  • Peer reachable (live probe)
  • WAL idempotent tren standby

GATE TUY CHON (production-multi-dc-full)
  • Meta Raft quorum (MSG_RAFT_*)
  • STONITH primary lease (QM_CLUSTER_STONITH=1)
  • WAL write quorum
  • PG BEGIN/COMMIT phan tan (QM_CLUSTER_PG_DISTRIBUTED=1)
  • WAL catch-up durable (QM_CLUSTER_WAL_CATCHUP=1)

CAU HINH TOI THIEU 2 NODE (enterprise)
  export QM_CLUSTER_ENABLE=1
  export QM_CLUSTER_TRANSPORT_PORT=55441
  export QM_CLUSTER_LOCAL_ADDR=127.0.0.1:55441
  export QM_CLUSTER_SHARD_ENDPOINTS=0=127.0.0.1:55441,1=127.0.0.1:55441,2=127.0.0.1:55442,3=127.0.0.1:55442
  export QM_CLUSTER_SHARD_REPLICAS=0=127.0.0.1:55442,2=127.0.0.1:55442
  export QM_CLUSTER_WAL_REPLICATE=1
  export QM_CLUSTER_WAL_SYNC=1
  export QM_CLUSTER_WAL_PEERS=127.0.0.1:55442
  export QM_CLUSTER_FAILOVER=1
  export QM_CLUSTER_FENCING=1
  export QM_CLUSTER_2PC=1
  export QM_CLUSTER_META_PEERS=127.0.0.1:55442
  export QM_CLUSTER_TLS_CERT=/path/to/cert.pem
  export QM_CLUSTER_TLS_KEY=/path/to/key.pem

PRODUCTION MULTI-DC (them len enterprise)
  export QM_CLUSTER_STONITH=1
  export QM_CLUSTER_WRITE_QUORUM=1
  export QM_CLUSTER_WAL_CATCHUP=1
  export QM_CLUSTER_PG_DISTRIBUTED=1
  # STONITH can --data-dir persistent (khong dung in-memory)

LENH CLI
  qm cluster status | health | readiness | certify | lag | metrics
  qm cluster join --shard-id N --primary HOST:PORT [--replicas ...]
  qm cluster guide        # huong dan nay

KIEM TRA TRUOC DEPLOY / RELEASE
  bash scripts/enterprise_ha_gate.sh
  bash scripts/cluster_production_soak.sh --quick

PUBLISH
  • Bump version dong bo trong qm_engine/Cargo.toml
  • CI Release Gate pass tren main
  • Tag vX.Y.Z va push; GitHub Actions build binary release:
      git tag vX.Y.Z
      git push origin vX.Y.Z
  • enterprise-certified ship voi env HA mac dinh; production-multi-dc-full
    bat them khi deploy — khong chan publish binary.

TAI LIEU DAY DU
  docs/vi/ENTERPRISE_HA_GUIDE.md
  docs/en/ENTERPRISE_HA_GUIDE.md
",
        env!("CARGO_PKG_VERSION")
    )
}
