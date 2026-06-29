# QMvir WAL Writer / Group Commit Architecture Pass

Audit date: 2026-06-14

## Verdict

`GROUP_COMMIT_VALIDATED`

This pass adds an engine-native durable group commit prototype for NativeSqlEngine. It does not change the default strict `per_commit_sync` behavior and does not use relaxed mode for any strict claim.

## Summary

- Added policy: `group_commit_sync` / `group-commit-sync`.
- Added an opportunistic group commit coordinator using shared `Mutex` + `Condvar` state.
- Callers append WAL first, enter the group queue, and only return after the leader completes `sync_all()`.
- Added group commit metrics exposed through Python: total groups, total commits, max group size, wait time.
- Fixed `new_session()` so cloned sessions have separate transaction state while sharing tables, indexes, WAL writer, and group coordinator.
- Released the GIL around Python `execute()` so Python concurrent benchmark threads can exercise the Rust engine concurrently.
- Added concurrent benchmark script: `scripts/compare_postgres_group_commit.py`.

## Files Changed

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/compare_postgres_native_sql.py`
- `scripts/compare_postgres_group_commit.py`
- `docs/WAL_WRITER_GROUP_COMMIT_ARCHITECTURE_PASS_2026_06_14.md`

## Architecture

This pass implements Option 2 from the prompt: synchronous opportunistic group commit.

Flow:

1. A mutating autocommit statement or explicit transaction COMMIT appends WAL records.
2. If policy is `group_commit_sync`, the caller enters shared group commit state.
3. First caller in a generation becomes leader.
4. Leader waits up to `250us` or until `64` pending commits.
5. Leader calls the existing strict `wal_sync()` path, which performs `flush()` + `File::sync_all()`.
6. Leader advances the generation and wakes followers.
7. Followers return only after generation advances, meaning after the group sync completes.

This is not a background WAL writer yet. It is the first architecture step: commit queue, group sync, shared metrics, and concurrent benchmark surface.

## Policy Semantics

| Policy | Sync behavior | Acknowledge before sync? | Durable after return? | Default |
|---|---|---:|---:|---:|
| `per_commit_sync` | `flush()` + `sync_all()` per commit/autocommit | No | Yes | No |
| `per_commit_sync_data` | `flush()` + `sync_data()` per commit/autocommit | No | Data-sync semantics; not claimed identical to `sync_all` metadata semantics | No |
| `group_commit_sync` | Append WAL, join group, one `sync_all()` per group | No | Yes, after group sync returns | No |
| `relaxed_os_buffered` | Append + `flush()` only | Yes | No strict crash-durable-at-return claim | No |

Default strict behavior remains unchanged.

## Tests Added

- `group_commit_sync_single_commit_recovers_after_return`
- `group_commit_sync_batches_concurrent_wal_appends_and_recovers`
- `group_commit_sync_rollback_does_not_append_transaction_wal`

The concurrent unit test appends 16 WAL commits concurrently, verifies fewer syncs than commits, verifies max group size >= 2, then reopens the engine and confirms all acknowledged rows recover.

## Benchmark

Artifacts:

- `/tmp/qmvir_group_commit/group_commit_concurrent.json`
- `/tmp/qmvir_group_commit/group_commit_concurrent_16_quick.json`
- `/tmp/qmvir_group_commit/group_commit_smoke.json`
- `/tmp/qmvir_group_commit/group_commit_tx_smoke.json`
- `/tmp/qmvir_group_commit/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`

macOS benchmark scope:

- Full workload matrix: concurrency `1,4,8`, 10 operations per worker.
- Quick autocommit concurrency-16 check: concurrency `16`, 10 operations per worker.
- PostgreSQL ran with `fsync=on` and `synchronous_commit=on`.
- A longer `1,2,4,8,16` full transaction matrix was stopped because it exceeded practical turn time on macOS; Linux should be the source of truth for release performance.

## Group Commit Evidence

Selected QM results:

| Workload | Concurrency | p50 ms | p95 ms | Throughput ops/s | Sync count | Commits | Avg group | Max group |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| autocommit_insert | 1 | 3.056 | 4.206 | 278.1 | 10 | 10 | 1.00 | 1 |
| autocommit_insert | 8 | 3.123 | 21.645 | 1593.6 | 10 | 80 | 8.00 | 8 |
| autocommit_insert | 16 | 2.937 | 4.063 | 4924.9 | 11 | 160 | 14.55 | 16 |
| autocommit_update_by_pk | 1 | 3.010 | 3.685 | 344.7 | 10 | 10 | 1.00 | 1 |
| autocommit_update_by_pk | 8 | 3.322 | 19.725 | 1255.0 | 10 | 80 | 8.00 | 8 |
| autocommit_update_by_pk | 16 | 2.894 | 3.156 | 5765.7 | 11 | 160 | 14.55 | 16 |
| autocommit_delete_by_pk | 1 | 5.805 | 22.615 | 122.2 | 20 | 20 | 1.00 | 1 |
| autocommit_delete_by_pk | 8 | 6.070 | 7.778 | 1390.7 | 20 | 160 | 8.00 | 8 |
| autocommit_delete_by_pk | 16 | 5.887 | 6.167 | 2897.3 | 21 | 320 | 15.24 | 16 |
| transaction_insert_10_commit | 1 | 3.004 | 6.735 | 293.4 | 10 | 10 | 1.00 | 1 |
| transaction_insert_10_commit | 8 | 4.106 | 9.856 | 1447.0 | 14 | 80 | 5.71 | 8 |
| transaction_insert_100_commit | 1 | 4.028 | 7.009 | 239.9 | 10 | 10 | 1.00 | 1 |
| transaction_insert_100_commit | 8 | 14.489 | 64.759 | 313.3 | 25 | 80 | 3.33 | 8 |
| mixed_dml_100_commit | 1 | 4.189 | 33.956 | 128.3 | 10 | 10 | 1.00 | 1 |
| mixed_dml_100_commit | 8 | 10.646 | 71.031 | 337.4 | 35 | 80 | 3.64 | 5 |

Interpretation:

- Group commit works: sync count drops sharply under concurrency.
- Best evidence is autocommit insert/update/delete and transaction_insert_10.
- Heavy transaction workloads still have execution/lock contention and need a real WAL writer plus better transaction/session isolation work before claiming parity.

## PostgreSQL Comparison

PostgreSQL remains faster on macOS.

Concurrency 16 quick autocommit comparison:

| Workload | QM p50 ms | QM throughput | PostgreSQL p50 ms | PostgreSQL throughput |
|---|---:|---:|---:|---:|
| autocommit_insert | 2.937 | 4924.9 | 0.314 | 8624.3 |
| autocommit_update_by_pk | 2.894 | 5765.7 | 0.292 | 9882.3 |
| autocommit_delete_by_pk | 5.887 | 2897.3 | 0.978 | 6569.2 |

Group commit improves QM throughput by reducing sync calls, but it is not a PostgreSQL win on this host.

## Regression Gates

Passed:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m maturin build --release --out /tmp/qmvir_group_commit/wheels`
- `python3 -m pip install --force-reinstall /tmp/qmvir_group_commit/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features group_commit_sync -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture`
- `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX`
- `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX`
- `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8`
- `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke`

Additional artifacts:

- `/tmp/qmvir_group_commit/qmvir_bridge_materialization_audit.json`
- `/tmp/qmvir_group_commit/qmvir_vector_search_medium_smoke.json`

## Remaining Bottlenecks

- Current implementation is opportunistic and synchronous; there is no dedicated WAL writer thread yet.
- Sync failures are not fully propagated because the existing `wal_sync()` path currently ignores `flush()`/`sync_all()` errors.
- Heavy transaction workloads show lock/execution contention at higher concurrency.
- Benchmark matrix on macOS is useful for logic validation but not release performance truth.
- Linux audit is still required for `fdatasync`, `O_DSYNC`, storage behavior, and realistic PostgreSQL comparison.

## Linux Next

Run on Linux:

- Rust WAL sync floor benchmark.
- `group_commit_sync` concurrent benchmark with concurrency `1,2,4,8,16,32`.
- PostgreSQL comparison with `fsync=on`, `synchronous_commit=on`.
- Crash/kill recovery gates.
- Linux wheel build/install.

Linux should be the source of truth for any 6.0/6.1 performance claim.

## Suggested Commit Split

| Slice | Paths | Suggested message |
|---|---|---|
| Group commit coordinator | `qm_engine/src/gateway/native_sql.rs` | `Add NativeSqlEngine durable group commit policy` |
| Concurrent benchmark | `scripts/compare_postgres_group_commit.py`, `scripts/compare_postgres_native_sql.py` | `Add PostgreSQL group commit comparison benchmark` |
| Report | `docs/WAL_WRITER_GROUP_COMMIT_ARCHITECTURE_PASS_2026_06_14.md` | `Document WAL writer group commit architecture pass` |

## Release Implication

This pass moves QMvir from direct per-commit fsync only toward a mature WAL/commit architecture. It validates that durable group commit can reduce sync count and raise concurrent throughput. It is not yet a release-grade PostgreSQL performance claim, and it should be followed by Linux validation and a real WAL writer/coordinator pass.
