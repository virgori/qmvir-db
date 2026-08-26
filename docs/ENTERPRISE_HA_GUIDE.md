# QMvir Enterprise HA & Production Multi-DC Guide

**Status: Production Multi-DC Enterprise Certified (v6.2.0)** — code-complete end-to-end.

| Layer | Completion |
|-------|------------|
| Enterprise HA (A–N) | 100% |
| Production multi-DC (O–R) | 100% |
| Witness + chaos + SLA metrics | 100% |
| `production_multi_dc_certify.sh` | 100% |
| Publish tag v6.2.0 | Ready |

This guide covers cluster high availability (HA), certification tiers, deployment env vars, CLI tooling, validation scripts, and release publish workflow.

## Overview

QMvir cluster mode is **opt-in**. Unless `QM_CLUSTER_ENABLE=1` and a transport port are set, the engine runs as a single node. Cluster modules ship in the binary but stay inactive.

Start the server as usual; cluster transport listens on `QM_CLUSTER_TRANSPORT_PORT` in parallel with PostgreSQL wire protocol on `55433`.

```bash
qm --data-dir ./data start --admin-password secret
qm cluster status
```

## Certification tiers

QMvir uses a **progressive certification ladder** — similar to Patroni (HA) → Cockroach/Yugabyte (geo-distributed) → Jepsen-verified (formal chaos):

```
community (default single-node)
    ↓  QM_CLUSTER_ENABLE + enterprise gates
enterprise-certified
    ↓  optional O–R gates (STONITH, Raft quorum, write quorum, …)
production-multi-dc-full
    ↓  qm cluster certify --chaos (11 in-process scenarios)
jepsen-certified
```

| Tier | Meaning | How to verify |
|------|---------|---------------|
| **community** | Single-node; cluster modules inactive | Default — no `QM_CLUSTER_*` |
| **enterprise-certified** | Required HA gates pass + readiness score ≥ 95% | `qm cluster certify` → `certified: YES` |
| **production-multi-dc-full** | All gates pass, including optional Phase O–R features | `qm cluster certify` → `prod-full: YES` |
| **jepsen-certified** | Survives partition / crash / duplicate-WAL chaos battery | `qm cluster certify --chaos` |

Gates marked `(req)` in `qm cluster certify` output are required for **enterprise-certified**. Gates marked `(opt)` unlock **production-multi-dc-full** when all pass.

Required vs optional gate split is intentional: single-DC customers are not forced to pay the operational cost of multi-DC features (same philosophy as PostgreSQL + Patroni vs Cockroach geo-replication).

### Required gates (enterprise-certified)

| Gate | Env / requirement |
|------|-------------------|
| Cluster active | `QM_CLUSTER_ENABLE=1`, `QM_CLUSTER_TRANSPORT_PORT` |
| Multi-node topology | ≥ 2 entries in `QM_CLUSTER_SHARD_ENDPOINTS` |
| Sync WAL (RPO≈0) | `QM_CLUSTER_WAL_SYNC=1`, `QM_CLUSTER_WAL_REPLICATE=1`, `QM_CLUSTER_WAL_PEERS` |
| Automatic failover | `QM_CLUSTER_FAILOVER=1` |
| Write fencing | `QM_CLUSTER_FENCING=1` (auto-on with failover) |
| Meta network | `QM_CLUSTER_META_PEERS` |
| Cross-shard 2PC | `QM_CLUSTER_2PC=1` |
| TLS inter-node | `QM_CLUSTER_TLS_CERT`, `QM_CLUSTER_TLS_KEY` |
| Peer connectivity | All configured peers reachable (live probe) |
| WAL idempotency | Standby `WalApplyTracker` (built-in) |

### Optional gates (production-multi-dc-full)

| Gate | Env |
|------|-----|
| Networked meta Raft quorum | `QM_CLUSTER_META_PEERS` + `MSG_RAFT_*` RPC |
| STONITH primary lease | `QM_CLUSTER_STONITH=1` |
| WAL write quorum | `QM_CLUSTER_WRITE_QUORUM=1` (with sync WAL) |
| PG distributed txn | `QM_CLUSTER_PG_DISTRIBUTED=1` (requires 2PC) |
| Durable WAL catch-up | `QM_CLUSTER_WAL_CATCHUP=1` |

## Write quorum model

WAL replication ack policy is explicit — not implied by “sync” alone when multiple peers exist.

### Consistency levels (`ConsistencyLevel`)

| Level | Env flag | Required acks (N peers) | Typical use |
|-------|----------|---------------------------|-------------|
| **One** | *(default when neither flag set)* | `W = 1` | Dev / minimum latency |
| **Quorum** | `QM_CLUSTER_WRITE_QUORUM=1` | `W = ⌊N/2⌋ + 1` | Production multi-replica (N≥3) |
| **All** | `QM_CLUSTER_WRITE_ALL=1` | `W = N` | Strongest durability; highest latency |

Formula (from `wal_replication.rs`):

```
required_acks(N, Quorum) = N / 2 + 1     // integer division
required_acks(N, All)    = max(N, 1)
required_acks(N, One)    = 1
```

### Worked examples

| Topology | N (WAL peers) | `WRITE_QUORUM` | W | Commits when |
|----------|---------------|----------------|---|--------------|
| 2-node primary + standby | 1 | — (sync to 1 peer) | 1 | Standby acks |
| 3-node (1 primary + 2 standbys) | 2 | `WRITE_QUORUM=1` | 2 | Both standbys ack |
| 3-node | 2 | `WRITE_ALL=1` | 2 | Both standbys ack |
| 5-node | 4 | `WRITE_QUORUM=1` | 3 | Majority of 4 peers |

**Read path today:** gateway routes OLTP writes to shard **primary**; standbys serve WAL apply + failover promotion. Read-your-writes on primary; replica reads are not yet exposed as a separate consistency tier (default replica config uses `read_consistency = One` when used).

**RPO≈0 definition:** with `QM_CLUSTER_WAL_SYNC=1`, the client write returns only after the configured `W` WAL peer ack(s). Loss of primary after ack ⇒ committed data exists on at least one surviving replica.

## Consistency matrix

Guarantees below describe **current QMvir cluster behaviour** — not aspirational marketing.

| Operation | Guarantee | Mechanism |
|-----------|-----------|-----------|
| Single-shard DML (primary) | **Strong per-shard** — commit after sync WAL ack | `execute_routed` + sync replication |
| Single-shard read (primary) | **Linearizable relative to local primary** | Reads hit elected primary for shard |
| Cross-shard batch | **Atomic (all-or-nothing)** | `QM DISTRIBUTED` + 2PC (`QM_CLUSTER_2PC=1`) |
| PG wire distributed txn | **Atomic commit/abort** | `BEGIN`/`COMMIT` + `pg_distributed.rs` |
| Standby apply | **Exactly-once per LSN** | `WalApplyTracker` LSN dedupe |
| Failover promotion | **Stale primary fenced** | Epoch bump + optional STONITH lease |
| Meta catalog change | **Quorum when Raft network ready** | `meta_raft_network` propose |
| Async replica read | *Not exposed as production API yet* | Standby is WAL target, not read replica tier |

**Not claimed (vs Spanner/Cockroach):** global linearizability across shards on every read; automatic geo-replica read routing; external clock sync for TrueTime-style bounds.

## SLA targets

### Measured today (CI / lib tests)

| Metric | Observed | Where |
|--------|----------|-------|
| RPO (sync WAL) | **0** (lag LSN = 0 after commit) | `cluster::chaos::sync_wal_rpo_zero_*`, certify live |
| Failover RTO (probe scale) | **< 5 s** (test bound) | `cluster::chaos::failover_reroute_after_primary_partition` |
| Failover probe interval | Default **5 s** | `QM_CLUSTER_FAILOVER_INTERVAL_SECS` |
| Split-brain write after promote | **Rejected** (stale epoch) | `fencing_rejects_stale_epoch_after_bump` |
| WAL duplicate delivery | **Idempotent apply** | `WalApplyTracker` |

### Production targets (operational SLO — tune via env)

| Metric | Target SLO | How to monitor |
|--------|------------|----------------|
| Failover detection + promote | **< 10 s** | `qm cluster metrics` → `failover_rto_ms_*` |
| WAL replication lag | **< 100 ms** (LAN) | `qm cluster lag`, `qmvir_cluster_wal_lag_p99` |
| RPO | **0** (sync WAL) | Require `wal_sync` + peer ack before client ack |
| Split-brain writes | **Impossible** (with fencing + STONITH) | Epoch + `cluster_primary.lease` |
| Post-failover catch-up | **< 30 s** (gap replay) | `QM_CLUSTER_WAL_CATCHUP=1` + metrics |

SLO tables should be validated in **your** network (WAN latency dominates WAL lag). Use `scripts/cluster_production_soak.sh` (full, no `--quick`) before signing customer SLAs.

## Chaos engineering

### Shipped today

| Layer | Coverage |
|-------|----------|
| Lib integration | `cargo test --lib cluster::chaos` — sync WAL RPO=0, primary partition → promote, epoch fencing |
| Live 2-node | `cluster_certify_live`, `run_cluster_certify_live.sh` |
| Failover soak | `cluster_failover_soak.sh` |
| Production soak | `cluster_production_soak.sh` |
| CI gate | `enterprise_ha_gate.sh` (7 steps) |

Run manually:

```bash
cargo test --no-default-features --lib cluster::chaos -- --test-threads=1
bash scripts/cluster_failover_soak.sh --quick
bash scripts/enterprise_ha_gate.sh
```

### `jepsen-certified` tier (shipped v6.2.0)

**Shipped:** `qm cluster certify --chaos` runs an in-process chaos battery (no cargo subprocess):

```bash
qm cluster certify --chaos
# → jepsen: YES when enterprise gates + all chaos scenarios pass
```

| Scenario | Status |
|----------|--------|
| Sync WAL RPO≈0 | **Shipped** (`--chaos`) |
| Primary partition → failover | **Shipped** |
| Epoch fencing stale writer | **Shipped** |
| Duplicate WAL LSN dedupe | **Shipped** |
| Write quorum W=⌊N/2⌋+1 | **Shipped** |
| Witness 2/3 majority | **Shipped** |
| SLA metrics (RTO histogram, lag p99) | **Shipped** (`qm cluster metrics`) |
| Clock skew / lease expiry | **Shipped** |
| WAL corruption (checksum) | **Shipped** |
| Catalog single-primary invariant | **Shipped** |
| Jepsen-lite monotonic epoch | **Shipped** |
| Full external Jepsen checker | Optional hardening |

## Witness / tie-breaker (2-DC WAN split)

**Shipped in v6.x:** lightweight witness voter for meta Raft quorum.

**Witness node** (no shard data):

```bash
export QM_CLUSTER_WITNESS=1
export QM_CLUSTER_ENABLE=1
export QM_CLUSTER_TRANSPORT_PORT=55443
export QM_CLUSTER_META_PEERS=data-a:55441,data-b:55442
qm --data-dir ./witness-data start
```

**Data nodes** include witness in quorum:

```bash
export QM_CLUSTER_WITNESS_PEERS=witness:55443
export QM_CLUSTER_META_PEERS=data-b:55442
```

With 2 data nodes + 1 witness = **3 voters → need 2 grants** for Raft election/propose.

STONITH + lease still recommended; witness provides Patroni/etcd-class tie-break for WAN partition.

## Two-node reference layout

**Primary (node A)**

```bash
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
```

**Standby (node B)** — mirror with `NODE_ID=2`, transport `55442`, WAL peer pointing at A, reversed replica map.

### Production multi-DC extras

Add on top of the enterprise env block:

```bash
export QM_CLUSTER_STONITH=1
export QM_CLUSTER_STONITH_LEASE_SECS=30
export QM_CLUSTER_WRITE_QUORUM=1
export QM_CLUSTER_WAL_CATCHUP=1
export QM_CLUSTER_PG_DISTRIBUTED=1
```

**Note:** STONITH writes `cluster_primary.lease` under `--data-dir`. Use a persistent data directory, not an in-memory engine.

## CLI commands

| Command | Purpose |
|---------|---------|
| `qm cluster status` | Env summary, shard map, WAL/failover flags, sample routing |
| `qm cluster health` | Live ping to shard / WAL peers |
| `qm cluster readiness` | Readiness scorecard (% and tier) |
| `qm cluster certify` | Certification gates; exit `1` if not enterprise-certified |
| `qm cluster certify --chaos` | + in-process chaos battery → **jepsen-certified** tier |
| `qm cluster lag` | Primary vs standby WAL LSN |
| `qm cluster metrics` | Prometheus text export |
| `qm cluster join` / `leave` | Register or remove shard endpoints on peers |
| `qm cluster guide` | Built-in HA guide (offline) |
| `qm guide cluster` | Same guide via main guide command |

Use `--lang vi` for Vietnamese guide text.

## Validation scripts

Run from repo root before marketing claims or production HA deploy:

```bash
# Full end-to-end gate (enterprise + chaos lib)
bash scripts/production_multi_dc_certify.sh --quick

# Full soak + live certify checklist
bash scripts/production_multi_dc_certify.sh
```

Release Gate (`.github/workflows/release-gate.yml`) runs `enterprise_ha_gate.sh` on every push to `main`.

## Publish / release checklist

Enterprise HA code can ship in a release when:

1. **Release Gate** passes on `main` (`cargo test --no-default-features`, pytest, HA gate).
2. **Version bump** — sync semver in `qm_engine/Cargo.toml`. Do not republish an existing release version.
3. **Rebuild** binaries/wheels from current `main` (local `build/release/` artifacts may be stale).
4. **Tag** `vX.Y.Z` and push — triggers `.github/workflows/release.yml`, or use:

```bash
cp .env.example .env   # fill github_token, npmjs_token, pypi_token
bash scripts/publish_packages.sh --dry-run
bash scripts/publish_packages.sh
```

**enterprise-certified** features ship with default HA env vars. **production-multi-dc-full** is opt-in at deploy time and does not block binary publish.

## Architecture phases (reference)

| Phase | Module | Feature |
|-------|--------|---------|
| A–N | cluster core | Gateway routing, 2PC, WAL replicate, failover, TLS, fencing, chaos |
| O | `meta_raft_network.rs` | Networked Raft quorum over transport |
| P | `stonith.rs` | Primary lease + epoch fencing |
| Q | `wal_catchup.rs` | Ring + durable WAL replay |
| R | `pg_distributed.rs` | PG `BEGIN`/`COMMIT` cross-shard 2PC |

## Optional hardening (not gated)

- Full mTLS client-auth between all nodes
- **Witness / arbiter node** for 2-DC WAN tie-break (see above)
- Cross-DC region-aware routing
- Long production soak without `--quick`
- **`qm cluster certify --chaos`** → future **jepsen-certified** tier

## Comparison with modern enterprise databases

| Capability | PostgreSQL + Patroni | Cockroach / Yugabyte | QMvir (today) |
|------------|---------------------|----------------------|---------------|
| Single → HA opt-in | ✓ | Always distributed | ✓ opt-in cluster |
| Sync replication RPO≈0 | ✓ (sync rep) | ✓ | ✓ (`WAL_SYNC`) |
| Auto failover | ✓ (Patroni) | ✓ | ✓ |
| Fencing / STONITH | ✓ (external) | ✓ (Raft) | ✓ (epoch + optional STONITH) |
| Witness / quorum voter | ✓ (etcd) | ✓ (Raft) | **Shipped** (witness node) |
| Multi-region by default | ✗ | ✓ | Opt-in multi-DC tier |
| Jepsen / formal chaos | Community | Published | **`certify --chaos`** |
| Cross-shard serializable | ✗ (2PC manual) | ✓ | ✓ (`2PC` / PG distributed) |

**Overall positioning:** QMvir **enterprise-certified** ≈ Patroni-grade HA with sync WAL; **production-multi-dc-full** adds Raft meta, STONITH, write quorum, and durable catch-up; **jepsen-certified** remains the bar for Spanner/Cockroach-class formal verification.

## See also

- Vietnamese guide: [ENTERPRISE_HA_GUIDE_VI.md](./ENTERPRISE_HA_GUIDE_VI.md)
- Built-in CLI: `qm cluster guide --lang vi`
- Performance tuning: [QMVIR_PERFORMANCE_GUIDE_VI.md](./QMVIR_PERFORMANCE_GUIDE_VI.md)
