# Root Cause Closure - 2026-05-18

Scope: QM Engine / NativeSqlEngine local release candidate. Git status is not
used as the release source of truth; the release evidence is the local source
tree plus a clean validated snapshot.

## Closure Table

| Item | Root cause | Affected layer | Current coverage | Release-blocking? | Required fix / boundary | Final status |
| --- | --- | --- | --- | --- | --- | --- |
| `current_commit_ts` returned next epoch instead of latest committed epoch | MVCC timestamp helper exposed the next allocation value rather than the last committed epoch, which made visibility assertions off by one epoch. | Rust MVCC metadata | `qm_engine/tests/mvcc_integration.rs`, release crash/recovery tests | Yes before stabilization | Use latest committed epoch for visibility decisions and tests. | Fixed |
| `add_write` did not update `touched_tables` | Transaction metadata missed write table registration, so dirty-table/checkpoint bookkeeping could be incomplete. | Rust MVCC/checkpoint metadata | MVCC integration and crash/recovery smoke | Yes before stabilization | Update touched table metadata when writes are registered. | Fixed |
| Active transaction visibility determinism | Active transaction ordering and visibility were not deterministic enough for release tests. | Rust MVCC metadata | MVCC integration tests | Yes before stabilization | Stabilize transaction snapshot/visibility expectations. | Fixed |
| Session cleanup / lock-order risk | Session and transaction cleanup could leave state behind or increase lock-order risk on error paths. | Native SQL sessions / transaction cleanup | Rust transaction tests, Python gateway tests | Yes before stabilization | Ensure cleanup paths remove transaction/session state. | Fixed with limitation |
| Missing dirty-page snapshot metadata silently wrote inconsistent metadata | Checkpoint metadata could silently omit dirty pages. | Storage/checkpoint metadata | `release_crash_recovery` fail-fast test | Yes | Fail fast instead of writing inconsistent snapshot metadata. | Fixed |
| Python MVCC write-write conflict detection | Python transaction engine committed both concurrent writers because visibility used row creation time and commit did not detect first-committer-wins conflicts. | Python `core_db.transaction_engine.mvcc` | `tests/test_core_internals.py::TestTransactionEngine`, `tests/test_mvcc.py`, full pytest | Yes | Add commit timestamp tracking, first-committer-wins detection, deterministic loser abort, rollback WAL record, and cleanup. | Fixed |
| HubEngine SQL passthrough xfails | HubEngine `execute_sql` passthrough is not wired for create/insert/select. | Distributed sharding / hub API | `tests/test_distributed_sharding.py` xfails | No for NativeSqlEngine local release | Keep xfails; out of NativeSqlEngine release scope. | Known limitation, non-blocking |
| pgwire float `BETWEEN` xfail | pgwire path still does not support `BETWEEN` over float columns. | pgwire SQL compatibility | `tests/test_full_engine.py::TestSelect::test_select_between_float` xfail | No for scoped NativeSqlEngine local release | Keep xfail; document as pgwire compatibility gap. | Known limitation, non-blocking |
| VectorQuantizer memory/compression accounting xfails | `memory_usage` and `compression_ratio` remain coarse and do not differentiate dtype pairs. | Vector utility accounting | `tests/test_vector_comprehensive.py` xfails | No for NativeSqlEngine release gate | Keep xfails; not a storage correctness blocker. | Known limitation, non-blocking |
| Storage-wide MVCC semantics | NativeSqlEngine transaction state is session-local rollback/WAL metadata; `mvcc_tx_mgr` is metadata and does not drive row visibility across the whole storage engine. | Native SQL, storage/cache/index/predicate paths | Rust MVCC metadata tests, Native SQL transaction tests, Python tests | Blocking only if docs claim full storage-wide MVCC for NativeSqlEngine | Scope release claims: NativeSqlEngine provides SQL transaction/WAL/checkpoint behavior, while full storage-wide MVCC remains an explicit boundary. | Fixed with limitation |
| WAL/crash safety limited to simulated restart tests | Prior release evidence only dropped/reopened engines and did not prove abrupt process termination recovery. | Native SQL WAL/checkpoint recovery | `release_crash_recovery`; new `release_crash_kill_recovery` | Yes before this pass | Add subprocess abort crash harness covering committed/uncommitted/checkpoint/delete/WAL replay/repeated cycles. | Fixed with limitation |
| Benchmark baseline placeholder | Baseline file existed but was not an approved performance reference. | Release benchmark gate | full release benchmark JSON, quick smoke benchmark | Yes before this pass | Run full local benchmark, record environment/latency/throughput/RSS, approve as local-machine baseline only. | Fixed |
| HNSW recall benchmark nondeterminism | HNSW level assignment used thread-local randomness, so the same benchmark data could build a different graph and fail release validation in a clean snapshot. | Rust vector index benchmark/integrity test | `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`, `bench_hnsw_recall` | Yes after discovery during snapshot validation | Make HNSW level generation deterministic per index configuration so clean snapshots reproduce the same graph. | Fixed |

## Python MVCC Conflict Closure

The Python MVCC engine now uses commit timestamps for visibility. Snapshot,
repeatable-read, and serializable transactions only see versions committed at or
before their snapshot timestamp; read-committed transactions see committed
versions as of each read.

Commit now performs first-committer-wins conflict detection over the write set.
If a row head was committed by another transaction after the losing transaction
started, the losing transaction is marked aborted, its write set is cleared, it
is removed from active transactions, a rollback WAL record is appended, and
`WriteConflictError` is raised.

Regression coverage includes:

- two Python transactions updating the same row
- loser rollback after conflict
- conflict after one transaction commits
- no false conflict for independent rows
- repeated conflicts do not poison later transactions
- snapshot transactions do not see versions committed after begin

Focused validation:

```text
python3 -m pytest tests/test_core_internals.py::TestTransactionEngine tests/test_mvcc.py -q -rxX
25 passed in 0.15s
```

Full pytest triage after the fix:

```text
python3 -m pytest -q -rxX
1113 passed, 12 skipped, 6 xfailed in 35.51s
```

No XPASS remains and no pytest warning summary was emitted.

## Remaining Xfails

| Test | Classification | Release decision |
| --- | --- | --- |
| `tests/test_distributed_sharding.py::TestHubEngineSQL::test_execute_sql_create_table` | Hub SQL passthrough gap | Known limitation, non-blocking for NativeSqlEngine local release |
| `tests/test_distributed_sharding.py::TestHubEngineSQL::test_execute_sql_insert` | Hub SQL passthrough gap | Known limitation, non-blocking for NativeSqlEngine local release |
| `tests/test_distributed_sharding.py::TestHubEngineSQL::test_execute_sql_select` | Hub SQL passthrough gap | Known limitation, non-blocking for NativeSqlEngine local release |
| `tests/test_full_engine.py::TestSelect::test_select_between_float` | pgwire float `BETWEEN` compatibility gap | Known limitation, non-blocking for NativeSqlEngine local release |
| `tests/test_vector_comprehensive.py::TestVectorQuantizerExtended::test_memory_usage` | Vector accounting precision gap | Known limitation, non-blocking for NativeSqlEngine local release |
| `tests/test_vector_comprehensive.py::TestVectorQuantizerExtended::test_compression_ratio` | Vector accounting precision gap | Known limitation, non-blocking for NativeSqlEngine local release |

## NativeSqlEngine MVCC Boundary

Full storage-wide MVCC is not claimed for this release. The NativeSqlEngine SQL
path has transaction, WAL, checkpoint, rollback, and recovery coverage, but the
row visibility mechanism is not a full storage-wide MVCC implementation spanning
all cache/index/predicate bypass paths. The release documentation was adjusted
to avoid claiming blanket PostgreSQL-style MVCC semantics for NativeSqlEngine.

This is non-blocking only under the scoped release claim:

- NativeSqlEngine release gate covers SQL transaction behavior, WAL/checkpoint
  recovery, deterministic crash smoke, and benchmark reproducibility.
- Full storage-wide MVCC across every storage/index/cache path remains outside
  this release claim and must not be marketed as complete.

## Crash Safety Boundary

Crash validation now includes both deterministic restart tests and subprocess
abort tests. The subprocess harness uses the test binary as a child process,
performs writes/checkpoints, calls `std::process::abort()`, then reopens the
database in the parent and verifies recovery.

Covered:

- committed transaction survives process abort
- uncommitted transaction is not visible after process abort
- checkpoint survives process abort
- delete before checkpoint survives WAL replay after process abort
- WAL delta after checkpoint replays after process abort
- repeated open/write/abort/reopen cycles recover

Limitation: this is process-abort recovery evidence, not torn-write, disk-full,
power-loss, or fsync-fault validation. The release claim must stay within that
boundary.
