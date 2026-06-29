# QMvir Durable WAL Sync Strategy / Commit Path Optimization Pass 6 - 2026-06-12

## Verdict

`PASS6_RELAXED_MODE_ADDED`

## Summary

Pass 6 added a new explicit WAL sync strategy, measured a standalone WAL sync floor, rebuilt the Python wheel, and benchmarked strict mode separately from the new mode.

- New mode: `relaxed-os-buffered` / engine policy `relaxed_os_buffered`.
- Strict default behavior is unchanged.
- Strict `per_commit_sync` still waits for WAL `flush()` plus `sync_all()` before returning.
- `relaxed_os_buffered` appends WAL and calls `flush()`, but does not call `sync_all()` at commit/autocommit return.
- `relaxed_os_buffered` is faster but is not strict crash-durable at return.

## Files Changed

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/compare_postgres_native_sql.py`
- `scripts/native_wal_sync_floor_benchmark.py`
- `docs/DURABLE_WAL_SYNC_STRATEGY_OPTIMIZATION_PASS6_2026_06_12.md`

## Strict Sync Floor Measurement

Artifact:

- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/wal_sync_floor.json`

Command:

```bash
python3 scripts/native_wal_sync_floor_benchmark.py \
  --iterations 200 \
  --output /private/tmp/qmvir_pg_perf_pass6_2026_06_12/wal_sync_floor.json
```

| Case | Total p50 | Total p95 | sync_all p50 | sync_all p95 | flush p50 |
|---|---:|---:|---:|---:|---:|
| no_write_sync | 0.001 | 0.002 | 0.000 | 0.001 | 0.000042 |
| tiny_append_flush_sync | 0.028 | 0.044 | 0.023 | 0.036 | 0.002917 |
| append_1kb_flush_sync | 0.031 | 0.052 | 0.026 | 0.042 | 0.003875 |
| append_4kb_flush_sync | 0.048 | 0.143 | 0.038 | 0.131 | 0.007792 |
| append_64kb_flush_sync | 0.069 | 0.100 | 0.050 | 0.076 | 0.012708 |
| batch_10_tiny_one_sync | 0.030 | 0.052 | 0.025 | 0.044 | 0.003042 |
| batch_100_tiny_one_sync | 0.047 | 0.062 | 0.036 | 0.049 | 0.006250 |
| batch_1000_tiny_one_sync | 0.103 | 0.118 | 0.047 | 0.058 | 0.011208 |

Interpretation:

- This standalone Python file microbenchmark does not reproduce the NativeSqlEngine `~3ms` strict write floor.
- NativeSqlEngine strict profiles still show the engine `sync_all()` path dominating strict p50 latency.
- The discrepancy means the Python microbenchmark is not a full substitute for the Rust `File::sync_all()` path, but it does prove SQL parsing/execution alone is not inherently a 3ms operation.

## New Sync Strategy

Implemented policy:

- CLI/API policy string: `relaxed-os-buffered`
- Engine policy string: `relaxed_os_buffered`
- Behavior: append WAL, flush Rust buffered writer to the OS, do not `sync_all()` on each autocommit/COMMIT return.

Implementation notes:

- Added `WalSyncPolicy::RelaxedOsBuffered`.
- Added `wal_flush_only()` beside `wal_sync()`.
- Autocommit and transaction commit dispatch now call `wal_flush_only()` for this policy.
- Explicit `sync_wal()` still performs full strict `flush()` plus `sync_all()`.
- Added native test `relaxed_os_buffered_policy_flushes_without_syncing_at_return`.

## Semantics Table

| Mode | Policy | Durable after return? | Crash loss window | Default? |
|---|---|---:|---|---:|
| Strict per commit | `per_commit_sync` | Yes | No committed-return window expected after successful `sync_all()` | No, but this is the strict benchmark mode |
| Strict per mutation | `per_mutation_sync` | Yes | No committed-return window expected after successful `sync_all()` | No |
| Append-only profile | `append-only-profile` | No | WAL may remain unsynced | Default internal initial policy |
| Relaxed OS buffered | `relaxed-os-buffered` / `relaxed_os_buffered` | No | WAL may be lost on OS crash/power loss before later sync | No |

Do not use `relaxed-os-buffered` to claim strict per-commit durability.

## Strict Mode Benchmark

Artifact:

- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/postgres_comparison_strict_after.json`

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy per-commit \
  --output /private/tmp/qmvir_pg_perf_pass6_2026_06_12/postgres_comparison_strict_after.json \
  --strict
```

| Workload | QM p50 | QM p95 | QM syncs | PostgreSQL winner? |
|---|---:|---:|---:|---|
| insert | 2.995 | 4.533 | 1010 | Yes |
| update_by_pk | 3.007 | 4.979 | 1010 | Yes |
| delete_by_pk | 2.990 | 4.120 | 510 | Yes |
| transaction_commit | 2.969 | 4.377 | 510 | Yes |
| transaction_insert_10_commit | 3.965 | 5.573 | 1011 | Yes |
| transaction_insert_100_commit | 5.699 | 8.534 | 111 | Yes |
| transaction_mixed_dml_100_commit | 5.035 | 6.133 | 111 | Yes |

Strict interpretation:

- Strict write latency remains around the NativeSqlEngine durable sync floor.
- Strict mode continues to be slower than PostgreSQL for the write/delete/small-commit workloads above.
- Strict durability was not weakened to improve these numbers.

## Relaxed Mode Benchmark

Artifact:

- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/postgres_comparison_relaxed_os_buffered.json`

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy relaxed-os-buffered \
  --output /private/tmp/qmvir_pg_perf_pass6_2026_06_12/postgres_comparison_relaxed_os_buffered.json
```

| Workload | Strict QM p50 | Relaxed QM p50 | Relaxed QM p95 | Relaxed syncs | Waits for fsync? |
|---|---:|---:|---:|---:|---:|
| insert | 2.995 | 0.009 | 0.022 | 0 | false |
| update_by_pk | 3.007 | 0.007 | 0.009 | 0 | false |
| delete_by_pk | 2.990 | 0.008 | 0.009 | 0 | false |
| transaction_commit | 2.969 | 0.009 | 0.012 | 0 | false |
| transaction_insert_10_commit | 3.965 | 0.283 | 0.425 | 1 | false |
| transaction_insert_100_commit | 5.699 | 1.553 | 1.787 | 1 | false |
| transaction_mixed_dml_100_commit | 5.035 | 2.164 | 2.676 | 1 | false |

Relaxed interpretation:

- Removing per-return `sync_all()` collapses write latency by orders of magnitude.
- This confirms the durable sync boundary is the dominant latency source in strict mode.
- This mode is not strict durable-at-return and must not be used for strict release claims.

## Counter Evidence

| Mode | Workload | sync_before_commit_return | acknowledged_before_fsync | sync count delta | fsync count delta |
|---|---|---:|---:|---:|---:|
| strict | insert | true | false | 1010 | 1010 |
| strict | delete_by_pk | true | false | 510 | 510 |
| strict | transaction_commit | true | false | 510 | 510 |
| relaxed-os-buffered | insert | false | true | 0 | 0 |
| relaxed-os-buffered | delete_by_pk | false | true | 0 | 0 |
| relaxed-os-buffered | transaction_commit | false | true | 0 | 0 |

The mode split is visible in the benchmark metadata and counters.

## Regression Gates

| Check | Result |
|---|---|
| `python3 -m py_compile scripts/native_wal_sync_floor_benchmark.py scripts/compare_postgres_native_sql.py scripts/native_sql_write_profile.py scripts/release_benchmark_native_sql.py` | PASS |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | PASS |
| `python3 -m maturin build --release --out /private/tmp/qmvir_pg_perf_pass6_2026_06_12/wheels` | PASS |
| `python3 -m pip install --force-reinstall /private/tmp/qmvir_pg_perf_pass6_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture` | PASS, `130 passed; 15 ignored` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture` | PASS, `10 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture` | PASS, `18 passed` |
| `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX` | PASS, `126 passed` |
| `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX` | PASS, `59 passed` |
| `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8` | PASS |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | PASS |

## Artifacts Outside Repo

- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/wal_sync_floor.json`
- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/postgres_comparison_strict_after.json`
- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/postgres_comparison_relaxed_os_buffered.json`
- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/qmvir_bridge_materialization_audit.json`
- `/private/tmp/qmvir_pg_perf_pass6_2026_06_12/qmvir_vector_search_medium_smoke.json`

## Remaining Bottlenecks

- Strict per-commit write latency remains bounded by the engine `sync_all()` path.
- Python standalone `fsync` microbenchmark does not reproduce the full engine sync floor; a future Rust microbenchmark should measure `std::fs::File::sync_all()` using the same path and filesystem location as NativeSqlEngine.
- Group commit with durable acknowledgement after group sync remains the next honest path for improving throughput while preserving post-return durability.
- `relaxed-os-buffered` is useful for latency-sensitive workloads that accept a crash-loss window, but it is not a release replacement for strict mode.

## Suggested Commit Split

| Slice | Files | Suggested message |
|---|---|---|
| Relaxed WAL sync policy | `qm_engine/src/gateway/native_sql.rs` | `Add explicit relaxed OS-buffered WAL policy` |
| Sync floor benchmark | `scripts/native_wal_sync_floor_benchmark.py` | `Add WAL sync floor microbenchmark` |
| Benchmark policy metadata | `scripts/compare_postgres_native_sql.py` | `Expose relaxed OS-buffered benchmark mode` |
| Pass 6 report | `docs/DURABLE_WAL_SYNC_STRATEGY_OPTIMIZATION_PASS6_2026_06_12.md` | `Document WAL sync strategy optimization pass 6` |

## Final Release Interpretation

Pass 6 adds a real, explicit sync strategy mode and proves the performance split between strict durable-at-return and relaxed OS-buffered operation. Strict PostgreSQL write gap remains. The new mode can be benchmarked and offered as a separate, weaker durability option, but must not be marketed as strict per-commit durability.

Final verdict:

`PASS6_RELAXED_MODE_ADDED`
