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

CERTIFICATION TIERS
  enterprise-certified       Required HA gates pass + readiness score >= 95%
  production-multi-dc-full   ALL gates pass (incl. optional O–R features)

  Check:  qm cluster certify
  Score:  qm cluster readiness

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
  • Bump version in pyproject.toml, Cargo.toml, npm/package.json (same semver)
  • CI Release Gate must pass on main
  • Rebuild binaries, tag vX.Y.Z, then:
      bash scripts/publish_packages.sh --dry-run
      bash scripts/publish_packages.sh
  • enterprise-certified ships with default HA env; production-multi-dc-full
    features are opt-in at deploy time — they do not block binary publish.

DOCS
  docs/ENTERPRISE_HA_GUIDE.md
  docs/ENTERPRISE_HA_GUIDE_VI.md

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

HAI CAP CHUNG NHAN
  enterprise-certified       Gate bat buoc dat + diem readiness >= 95%
  production-multi-dc-full   Tat ca gate (bao gom tinh nang O–R tuy chon)

  Kiem tra:  qm cluster certify
  Diem so:   qm cluster readiness

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
  • Bump version dong bo (pyproject.toml, Cargo.toml, npm/package.json)
  • CI Release Gate pass tren main
  • Build binary moi, tag vX.Y.Z, publish:
      bash scripts/publish_packages.sh --dry-run
  • enterprise-certified ship voi env HA mac dinh; production-multi-dc-full
    bat them khi deploy — khong chan publish binary.

TAI LIEU DAY DU
  docs/ENTERPRISE_HA_GUIDE_VI.md
  docs/ENTERPRISE_HA_GUIDE.md
",
        env!("CARGO_PKG_VERSION")
    )
}
