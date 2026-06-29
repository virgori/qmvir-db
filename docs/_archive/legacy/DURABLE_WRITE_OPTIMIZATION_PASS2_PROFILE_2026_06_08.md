# QMvir Durable Write Optimization Pass 2 - Deep Profile

## Verdict

`PASS2_IMPROVED_NEEDS_FULL_BENCH`

## Summary

- Full PostgreSQL benchmark status: deferred.
- PostgreSQL claim: none.
- Optimizations implemented:
  - Added gated native SQL profile counters via `QMVIR_NATIVE_SQL_PROFILE=1`.
  - Added Python APIs: `profile_snapshot()` and `reset_profile_snapshot()`.
  - Added QM-only write profiler: `scripts/native_sql_write_profile.py`.
  - Added `--warmup` support to `scripts/release_benchmark_native_sql.py`; default remains 10.
  - Reduced indexed INSERT write amplification for the common `ON CONFLICT`-free/no-`RETURNING` path by avoiding the extra `index_changes` row clone vector.
- Workloads improved: `insert`, `transaction_commit`, `transaction_insert_1000_commit` p95, `transaction_mixed_dml_100_commit`, `transaction_rollback_100`.
- Workloads still problematic: `update_by_pk`, `delete_by_pk`, `transaction_insert_10_commit`, `transaction_insert_100_commit`.
- Durability preserved: strict modes still use `acknowledged_before_fsync=false`; autocommit/COMMIT waits for `wal_sync()`, which still performs `flush()` and `sync_all()`.
- Full correctness gate: passed.

## Before/After Calibration

Label: `CALIBRATION_ONLY_NOT_RELEASE_CLAIM`

The exact first profile command with `--iterations 50 --warmup 5 --repeat 3` was stopped during `transaction_insert_1000_commit` because it was too slow for the optimization loop. The profiler now records both `iterations` and `effective_iterations`; heavy workloads are capped transparently while preserving the requested iteration value in JSON.

Artifacts:

- `/private/tmp/qmvir_pg_perf_pass2_2026_06_08/write_profile_before.json`
- `/private/tmp/qmvir_pg_perf_pass2_2026_06_08/write_profile_after_insert_fast_path.json`
- `/private/tmp/qmvir_pg_perf_pass2_2026_06_08/qm_calibration_after_insert_fast_path.json`

| Workload | Before p50 | Before p95 | After p50 | After p95 | Delta p50 | Delta p95 |
|---|---:|---:|---:|---:|---:|---:|
| insert | 3.026 | 4.630 | 3.008 | 3.984 | -0.018 | -0.646 |
| update_by_pk | 3.008 | 3.987 | 3.017 | 5.499 | +0.009 | +1.511 |
| delete_by_pk | 6.011 | 9.404 | 6.795 | 9.942 | +0.784 | +0.539 |
| transaction_commit | 9.974 | 12.011 | 9.440 | 11.990 | -0.533 | -0.021 |
| transaction_insert_10_commit | 13.861 | 17.187 | 14.108 | 27.451 | +0.247 | +10.264 |
| transaction_insert_100_commit | 30.083 | 41.005 | 31.931 | 47.171 | +1.848 | +6.166 |
| transaction_insert_1000_commit | 712.758 | 1279.802 | 788.259 | 1073.617 | +75.501 | -206.184 |
| transaction_mixed_dml_100_commit | 19.940 | 38.943 | 17.986 | 19.995 | -1.954 | -18.948 |
| transaction_rollback_100 | 11.713 | 17.342 | 10.492 | 12.053 | -1.221 | -5.289 |

## Profile Breakdown

| Workload | Dominant before | Dominant after | Evidence |
|---|---|---|---|
| update_by_pk | WAL sync dominates; index update is secondary | WAL sync still dominates | before `wal_sync_ns=401.3ms`, `index_update_ns=8.7ms`; after `wal_sync_ns=433.3ms`, `index_update_ns=14.0ms` |
| transaction_commit | COMMIT sync plus one-row transaction overhead | Slightly lower execute time | before `execute_ns=1480.2ms`; after `execute_ns=1444.0ms`; sync count unchanged at 150 |
| transaction_insert_1000_commit | Engine execute path dominates, not sync | Execute path still dominates but p95 improved | before `execute_ns=7024.3ms`, `wal_sync_ns=56.9ms`; after `execute_ns=6779.4ms`, `wal_sync_ns=30.8ms` |
| transaction_mixed_dml_100_commit | Execute path dominates; index updates visible but not dominant | Execute path reduced | before `execute_ns=682.3ms`, `index_update_ns=5.9ms`; after `execute_ns=525.3ms`, `index_update_ns=11.1ms` |

## Write Amplification Findings

| Area | Before | After | Evidence |
|---|---:|---:|---|
| table_rewrite_count | 0 | 0 | No checkpoint table rewrite during profiled workloads |
| index_rebuild_count | 0 | 0 | No checkpoint index rebuild during profiled workloads |
| checkpoint_count | 0 | 0 | No checkpoint triggered in profiled deltas |
| wal_bytes | unchanged | unchanged | Example `transaction_insert_1000_commit`: `875904 -> 875904` |
| sync_all_count | unchanged | unchanged | Example `transaction_insert_1000_commit`: `9 -> 9` |
| flush_count | unchanged | unchanged | Example `transaction_insert_1000_commit`: `9 -> 9` |
| index_update_ns | newly visible for INSERT fast path | bounded but measurable | `transaction_insert_1000_commit` after profile reports `145.2ms` index work |

## Bottleneck Diagnosis Before Editing

| Workload | p50/p95 issue | Dominant cost | Evidence | Candidate fix |
|---|---|---|---|---|
| update_by_pk | p95 regression | strict WAL sync dominates | `wal_sync_ns=401.3ms` over 150 samples; index update `8.7ms` | no safe durability-preserving fix in this pass |
| transaction_commit | p50/p95 worse from Pass 1 calibration | COMMIT sync plus small transaction overhead | `sync_all_count=150`; `wal_sync_ns=413.8ms` | avoid unnecessary row clone/write amplification where possible |
| transaction_insert_1000_commit | very slow; prior p95 regression | execute path, indexed insert clone/index maintenance | `execute_ns=7024.3ms`, sync only `56.9ms`; source showed extra `index_changes` row clone vector | indexed INSERT fast path without extra clone vector |
| transaction_mixed_dml_100_commit | p95 regression | execute path plus mixed index work | `execute_ns=682.3ms`, `index_update_ns=5.9ms`, sync `75.7ms` | reduce insert-side clone amplification |

## Optimizations

| Optimization | File | Why safe | Tests |
|---|---|---|---|
| Gated profile counters and Python snapshot/reset APIs | `qm_engine/src/gateway/native_sql.rs` | Trace is off by default; counters do not alter execution results | full Rust/Python gates |
| Batch/write/sync/checkpoint/index timing instrumentation | `qm_engine/src/gateway/native_sql.rs` | Observability only; durability code path unchanged | full Rust/Python gates |
| Indexed INSERT no-conflict/no-returning fast path avoids `index_changes` clone vector | `qm_engine/src/gateway/native_sql.rs` | Rows are still inserted into table before index entries; WAL and fsync semantics unchanged | native SQL, WAL, checkpoint, crash recovery, full gates |
| QM-only write profiler | `scripts/native_sql_write_profile.py` | Writes output only to requested path; calibration-only label | `py_compile`, profile before/after |
| `--warmup` option for release benchmark | `scripts/release_benchmark_native_sql.py` | Default remains 10, preserving existing behavior when not specified | `py_compile`, calibration run |

## Durability Semantics

| Mode | acknowledged_before_fsync | sync_before_commit_return | Notes |
|---|---:|---:|---|
| `per_commit_sync` | false | true | Used by profiler and calibration; COMMIT/autocommit waits for `wal_sync()` |
| `per_mutation_sync` | false | true | Existing semantics retained |
| append-only/profile modes | true | false | Not used for durability claims |

## Regression Guard

| Check | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture` | PASS |
| `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX` | PASS, `126 passed` |
| `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX` | PASS, `59 passed` |
| `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8` | PASS |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | PASS |
| Full Rust gate | PASS, `451 passed; 15 ignored`; integration suites passed; doc-tests `2 ignored` |
| Full Python gate | PASS, `1163 passed, 12 skipped` |

Artifact scan note:

- The requested regex returned legitimate source/script paths containing `snapshot` in their names:
  - `qm_engine/src/backup/snapshot_diff.rs`
  - `qm_engine/src/storage/snapshot.rs`
  - `scripts/check_local_release_snapshot.py`
  - `scripts/create_local_release_snapshot.py`
  - `scripts/validate_local_release_snapshot.sh`
  - `tests/test_snapshot_checkpoint.py`
- No generated JSON, WAL logs, static libs, npm binaries, `.env`, `.DS_Store`, `dist`, `target`, `node_modules`, `__pycache__`, `.pytest_cache`, `.tgz`, or `npm/qm-*` artifacts were staged or added.

## Remaining Work

- Create valid PostgreSQL role/database for full benchmark.
- Rerun full 1000-iteration PG comparison on a cool/idle machine.
- Add deeper row/constraint/index micro-counters if `transaction_insert_10_commit` and `transaction_insert_100_commit` remain unstable.
- Consider strict group commit only after per-commit path is fully characterized.

## Suggested Commit Split

| Slice | Files | Commit message |
|---|---|---|
| Pass 1 WAL batching | `qm_engine/src/gateway/native_sql.rs`, `docs/DURABLE_WRITE_OPTIMIZATION_FIRST_PASS_2026_06_08.md` | `Optimize strict WAL transaction append path` |
| Pass 2 profiling | `qm_engine/src/gateway/native_sql.rs`, `scripts/native_sql_write_profile.py` | `Add native SQL durable write profiler` |
| Pass 2 insert amplification | `qm_engine/src/gateway/native_sql.rs` | `Reduce indexed insert clone amplification` |
| Benchmark harness hygiene | `scripts/release_benchmark_native_sql.py` | `Add configurable release benchmark warmup` |
| Pass 2 report | `docs/DURABLE_WRITE_OPTIMIZATION_PASS2_PROFILE_2026_06_08.md` | `Document durable write optimization pass 2` |

## Final Note

Do not publish PostgreSQL win/loss claim from this pass.

