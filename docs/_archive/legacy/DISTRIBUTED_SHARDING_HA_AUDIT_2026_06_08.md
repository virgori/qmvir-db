# QMvir Distributed / Sharding / HA Audit - 2026-06-08

## Executive Summary

Verdict: **BASIC_SHARD_ROUTING_ONLY**, with **EXPERIMENTAL_CLUSTER_FEATURES** present in the tree.

QMvir currently has usable source-level support for deterministic shard routing and local sharded indexes. It also contains experimental distributed components for gossip, Raft-like consensus, replication, scatter/gather query execution, and distributed transaction patterns, but those components are not proven as an integrated production HA system.

The safe release claim is narrow:

- QMvir has basic deterministic shard routing.
- QMvir has single-process sharded index execution for selected index paths.
- QMvir has experimental distributed/sharding/replication modules with smoke and unit tests.

The unsafe release claims are:

- Production high availability.
- Automatic failover correctness.
- Network partition safety.
- Linearizable distributed writes.
- Serializable distributed transactions.
- CockroachDB/PostgreSQL-style distributed correctness.
- Durable cross-shard two-phase commit.

## Repository State

The worktree is still heavily dirty from the broader release work. This audit did not commit, delete source files, rewrite engine behavior, or modify benchmark numbers.

Required inspection commands were run:

```bash
git status --short
find qm_engine/src/cluster -maxdepth 3 -type f 2>/dev/null | sort
find qm_engine/src/hub_engine -maxdepth 3 -type f 2>/dev/null | sort
find tests -maxdepth 2 -type f | rg 'distributed|shard|cluster|replica|failover|hub|satellite|partition|consensus|two_phase|2pc|raft|transport' || true
find qm_engine/tests -maxdepth 2 -type f | rg 'distributed|shard|cluster|replica|failover|partition|consensus|two_phase|2pc|raft|transport' || true
```

Additional structure and call-site searches were run with `rg` across `qm_engine`, `qm_core`, and `tests`.

## Module Inventory

| File / Module | Purpose | Status | Current Use | Release Risk |
|---|---:|---:|---:|---:|
| `qm_engine/src/cluster/shard.rs` | Consistent hash ring, shard manager, Python wrappers | IMPLEMENTED | Exported by Rust lib; tested | Medium |
| `qm_engine/src/cluster/replica.rs` | Replica set metadata, quorum math, promotion state | PARTIALLY_IMPLEMENTED | Exported by Rust lib; unit tested | High |
| `qm_engine/src/cluster/transport.rs` | TCP frame protocol, node client/server, WAL and 2PC message types | DEFINED_BUT_UNUSED | Not declared by `cluster/mod.rs` | Critical |
| `qm_engine/src/cluster/two_phase_commit.rs` | Rust 2PC coordinator/participant | DEFINED_BUT_UNUSED | Not declared by `cluster/mod.rs`; 0 matching tests | Critical |
| `qm_engine/src/index/sharded.rs` | Single-process sharded HNSW/inverted indexes | IMPLEMENTED | Rust tests and benchmark-style tests | Medium |
| `qm_engine/src/storage/wal_streaming.rs` | WAL stream sender using cluster transport | DEFINED_BUT_UNUSED / NOT_WIRED | References `crate::cluster::transport`, which is not exported | Critical |
| `qm_core/distributed/shard.py` | Python shard manager and routing model | SMOKE_ONLY | Python tests | Medium |
| `qm_core/distributed/gossip.py` | SWIM-like membership protocol | SMOKE_ONLY | Python tests with in-memory/callback transport | High |
| `qm_core/distributed/consensus.py` | Raft-inspired leader election and log replication | SMOKE_ONLY | Python tests; in-memory state | High |
| `qm_core/distributed/raft_persistence.py` | Persistent Raft state helper | PARTIALLY_IMPLEMENTED | Not proven as integrated HA path | High |
| `qm_core/distributed/tcp_transport.py` | TCP transport for Raft RPCs | PARTIALLY_IMPLEMENTED | Standalone transport; not proof of integrated cluster HA | High |
| `qm_core/distributed/cluster.py` | Cluster coordinator and metadata plane | SMOKE_ONLY | Python tests | High |
| `qm_core/distributed/replication.py` | WAL sender/receiver and replica manager | SMOKE_ONLY | Python callback/in-memory tests | High |
| `qm_core/distributed/dist_query.py` | Scatter/gather distributed query executor | SMOKE_ONLY | Python mock executor/router tests | Medium |
| `qm_core/distributed/dist_txn.py` | Python 2PC and saga orchestration | SMOKE_ONLY | Python callback tests; in-memory decision log | Critical |
| `tests/test_distributed.py` | Broad distributed module tests | SMOKE_ONLY | Passed | High |
| `tests/test_distributed_sharding.py` | Rust shard routing and Python replica/hub tests | SMOKE_ONLY / UNIT | Passed | Medium |
| `tests/test_hub_satellite_arch.py` | Hub/satellite architecture tests | UNIT / SMOKE_ONLY | Passed | Low for HA claims |

## Important Wiring Finding

`qm_engine/src/cluster/mod.rs` currently declares only:

```rust
pub mod replica;
pub mod shard;
```

It re-exports only `ReplicaSet`, `ReplicaState`, `ReplicationConfig`, `ConsistentHashRing`, `ShardId`, `ShardManager`, and `VNodeId`.

Therefore:

- `qm_engine/src/cluster/transport.rs` exists but is not compiled/exported through the current `cluster` module.
- `qm_engine/src/cluster/two_phase_commit.rs` exists but is not compiled/exported through the current `cluster` module.
- The Rust 2PC and transport files should be treated as **defined but unused** for release claims.

This is the strongest reason QMvir must not claim production distributed transaction or HA behavior from the Rust engine.

## Capability Classification

### Cluster Model

| Capability | Classification | Evidence |
|---|---:|---|
| Real cluster node abstraction | PARTIALLY_IMPLEMENTED | Python `ClusterNode` and coordinator exist; Rust engine integration is not proven |
| Membership model | SMOKE_ONLY | Python gossip tests use in-memory/callback message flow |
| Stable node IDs | PARTIALLY_IMPLEMENTED | Node IDs exist in metadata; no durable identity lifecycle proof |
| Leader/coordinator | SMOKE_ONLY | Python Raft/coordinator tests exist; state is largely in memory |
| Heartbeat/liveness | SMOKE_ONLY | Gossip/replica state logic exists; no real failure matrix |
| Real networking | PARTIALLY_IMPLEMENTED | Python TCP Raft transport exists; Rust transport is not wired |
| Persistent cluster metadata | NOT_FOUND | No integrated durable cluster metadata path proven |

### Sharding Model

| Capability | Classification | Evidence |
|---|---:|---|
| Shard ownership representation | PARTIALLY_IMPLEMENTED | Rust `ShardManager`; Python shard metadata |
| Deterministic routing | IMPLEMENTED | Rust consistent hashing and Python routing tests passed |
| Routing policy | IMPLEMENTED_FOR_HASH | Hash routing is tested; broader policies are not production-proven |
| Shard metadata persistence | NOT_FOUND | No durable shard map recovery proof |
| Cross-shard read query | SMOKE_ONLY | Python scatter/gather and Rust local sharded index tests |
| Cross-shard writes | NOT_FOUND | No integrated write path using distributed commit |
| Rebalance / migration | STUB / SMOKE_ONLY | Metadata-level movement exists; no row movement correctness proof |
| Resharding safety | NOT_TESTED | No duplicate/missing record movement tests |
| SQL planner shard awareness | SMOKE_ONLY | Routing helpers exist; not proven as integrated SQL optimizer behavior |

### Replication

| Capability | Classification | Evidence |
|---|---:|---|
| Primary/replica model | PARTIALLY_IMPLEMENTED | Rust `ReplicaSet`; Python replica metadata |
| Sync/async replication | PARTIALLY_IMPLEMENTED | Configs and quorum math exist; not proven in full engine write path |
| WAL shipping | SMOKE_ONLY | Python WAL callback flow; Rust WAL transport path is not wired |
| LSN / sequence tracking | PARTIALLY_IMPLEMENTED | LSN/op sequence fields exist |
| Replica catch-up | SMOKE_ONLY | Buffer/state behavior exists; no restart/integration proof |
| Read from replica | SMOKE_ONLY | Replica selection exists; no consistency guarantee proof |
| Split-brain prevention | NOT_FOUND | No production quorum/fencing proof |
| Promotion/failover | SMOKE_ONLY | Metadata promotion tests exist; no in-flight write/durability proof |

### Transactions / 2PC

| Capability | Classification | Evidence |
|---|---:|---|
| Rust 2PC exists | DEFINED_BUT_UNUSED | `two_phase_commit.rs` is not declared in `cluster/mod.rs` |
| Python 2PC exists | SMOKE_ONLY | Callback-based tests pass |
| Used by actual write path | NOT_FOUND | No engine write path call-site found |
| Prepared state persisted | NOT_FOUND | Rust and Python implementations keep transaction state in memory |
| Coordinator crash recovery | SMOKE_ONLY | Python recovery test mutates in-memory phase; no process restart |
| Participant crash recovery | NOT_TESTED | No durable participant prepare log proof |
| Idempotent retry safety | NOT_TESTED | No duplicate/delayed message matrix |
| Partial commit prevention | NOT_TESTED | No adversarial failure test |
| Distributed isolation levels | NOT_FOUND | No cross-shard isolation model found |

## Failure Behavior Matrix

| Scenario | Status | Notes |
|---|---:|---|
| Node down before write | NOT_TESTED | No integrated distributed write path |
| Node down during write | NOT_TESTED | No write-path failure test |
| Crash after prepare before commit | NOT_TESTED | Python has in-memory phase recovery only |
| Coordinator crash | SMOKE_TESTED | No durable restart recovery proof |
| Participant crash | NOT_TESTED | No durable participant prepare state |
| Network timeout | NOT_TESTED | Timeout fields exist; no correctness matrix |
| Network partition | NOT_TESTED | No partition/fencing tests |
| Delayed messages | NOT_TESTED | No delayed RPC tests |
| Duplicate messages | NOT_TESTED | No idempotency tests |
| Retry storm | NOT_TESTED | No retry pressure tests |
| Stale shard map | NOT_TESTED | No stale-router correctness test |
| Replica lag | SMOKE_TESTED | Lag stats/state exist; no stale-read correctness proof |
| Failover with in-flight write | NOT_TESTED | No adversarial failover test |
| Recovery after process restart | NOT_TESTED_FOR_DISTRIBUTED | Broad local crash recovery exists, but not distributed HA recovery |

## Claim Safety

| Claim | Safety | Allowed wording |
|---|---:|---|
| Basic sharding | CONDITIONAL | "Basic deterministic shard routing and local sharded index paths" |
| Distributed query routing | CONDITIONAL | "Experimental scatter/gather query module with smoke tests" |
| Basic replica metadata | CONDITIONAL | "Replica metadata and quorum helper structures" |
| Automatic failover | UNSAFE | Do not claim |
| High availability | UNSAFE | Do not claim |
| Linearizable writes | UNSAFE | Do not claim |
| Serializable distributed transactions | UNSAFE | Do not claim |
| Network partition safety | UNSAFE | Do not claim |
| Production HA | UNSAFE | Do not claim |
| PostgreSQL/CockroachDB-style distributed correctness | UNSAFE | Do not claim |
| Durable distributed transaction correctness | UNSAFE | Do not claim |

## Tests Run

| Command | Result |
|---|---:|
| `python3 -m pytest tests/test_distributed.py tests/test_distributed_sharding.py tests/test_hub_satellite_arch.py -q -rxX` | `164 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features cluster -- --nocapture` | `10 passed; 449 filtered out` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features distributed -- --nocapture` | `0 passed; 459 filtered out` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features shard -- --nocapture` | `11 passed; 448 filtered out` plus sharded benchmark-style tests passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features replica -- --nocapture` | `5 passed; 454 filtered out` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features two_phase -- --nocapture` | `0 passed; 459 filtered out` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed: `444` lib tests, plus integration/bin test suites; `15` ignored in lib tests |
| `python3 -m pytest -q -rxX` | `1139 passed, 12 skipped` |

The `distributed` and `two_phase` Rust filters are especially important: they ran successfully as commands, but they found **zero tests**. They do not provide Rust distributed/2PC coverage.

## Release Gate Assessment

QMvir is not ready for a release gate that advertises production HA or distributed database correctness.

QMvir can approach a release gate that advertises:

- Local engine correctness covered by the broader Rust and Python test suites.
- Basic shard routing and local sharded index support.
- Experimental distributed modules behind conservative documentation.

Recommended release note wording:

> QMvir includes experimental distributed coordination, sharding, replication, and distributed transaction modules. Current supported release-surface claims are limited to deterministic shard routing and local sharded index behavior. Production HA, automatic failover, network partition safety, and durable distributed transactions are not release claims for this build.

## Files Requiring Owner Review Before Any HA Claim

- `qm_engine/src/cluster/mod.rs`
- `qm_engine/src/cluster/transport.rs`
- `qm_engine/src/cluster/two_phase_commit.rs`
- `qm_engine/src/storage/wal_streaming.rs`
- `qm_engine/src/index/sharded.rs`
- `qm_core/distributed/cluster.py`
- `qm_core/distributed/consensus.py`
- `qm_core/distributed/raft_persistence.py`
- `qm_core/distributed/tcp_transport.py`
- `qm_core/distributed/replication.py`
- `qm_core/distributed/dist_query.py`
- `qm_core/distributed/dist_txn.py`
- `tests/test_distributed.py`
- `tests/test_distributed_sharding.py`
- `tests/test_hub_satellite_arch.py`

## Minimal Follow-Up Test Plan

These are tests to add before changing any distributed release claim:

| Test | Target | Scenario | Unlocks |
|---|---|---|---|
| Rust compile wiring test | `qm_engine/src/cluster/mod.rs` | Explicitly include or intentionally exclude transport/2PC modules | Accurate release surface |
| Rust 2PC unit test | `two_phase_commit.rs` | Prepare, commit, abort with fake participants | Basic 2PC coverage |
| Rust 2PC crash test | `two_phase_commit.rs` | Crash/recover coordinator after prepare | Any durable 2PC claim |
| Participant recovery test | Rust or Python 2PC | Restart participant with prepared txn | Partial commit safety |
| WAL shipping integration | `wal_streaming.rs` / replication | Real WAL entry sent, applied, and acknowledged | Replication claim |
| Replica stale-read test | replica manager | Lagged replica rejected or marked stale | Read consistency claim |
| Failover in-flight write test | cluster + replication | Primary fails during write | Failover claim |
| Stale shard map test | shard router | Route using old map during rebalance | Sharding safety claim |
| Rebalance data movement test | shard manager/storage | Move records with no duplicates/misses | Resharding claim |
| Network partition test | consensus/transport | Split cluster and heal | HA/partition claim |

## Final Recommendation

Keep the next release claim conservative: **basic shard routing only**, with distributed modules explicitly described as experimental.

Do not advertise QMvir as production distributed, HA-capable, partition-safe, or distributed-transaction-safe until the unused Rust distributed files are either wired and tested or removed from the release surface, and until failure/recovery tests cover real process restart and network fault scenarios.
