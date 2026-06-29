# Durable Write/Delete/Commit Optimization Pass 3 - 2026-06-15

## Verdict

`PASS3_PG_VALIDATED`

PostgreSQL comparison was executed successfully with a valid local PostgreSQL 17.9 server, `fsync=on`, `synchronous_commit=on`, and NativeSqlEngine persistent WAL `per_commit_sync`.

PostgreSQL claim: `partial`.

This is not a blanket "QM beats PostgreSQL" claim. QM still loses single-row durable write/delete/autocommit commit workloads because strict `sync_all()` dominates. QM is competitive or faster on several read/index/rollback and larger transaction workloads.

## Summary

This pass kept strict durability unchanged: autocommit and COMMIT in `per_commit_sync` still wait for WAL flush plus `sync_all()` before returning.

Implemented work:

- Added deeper gated NativeSqlEngine profile counters for transaction staging/commit/rollback, PK lookup, row undo count, WAL record count, and transaction sync count.
- Avoided full secondary-index snapshot cloning for ordinary transaction row undo when existing rollback logic can remove current index entries and restore old rows.
- Skipped FK check/action loops for UPDATE/DELETE when no child table references the target table.
- Added direct non-transaction fast paths for simple `UPDATE ... WHERE id = literal` and `DELETE ... WHERE id = literal` when there is no `RETURNING` and no FK referencing side effect.
- Rebuilt and reinstalled the local wheel before Python benchmark runs.

## Before/After Native Profile

Native profile command:

```bash
python3 scripts/native_sql_write_profile.py --iterations 50 --warmup 5 --repeat 3 --sync-policy per_commit_sync --trace --output /tmp/qmvir_pass3_after2.json
```

Baseline: `/tmp/qmvir_pass3_baseline_before.json`
After: `/tmp/qmvir_pass3_after2.json`

| workload | before p50 ms | after p50 ms | p50 delta | before p95 ms | after p95 ms | p95 delta |
|---|---:|---:|---:|---:|---:|---:|
| insert | 3.021 | 2.996 | -0.8% | 5.709 | 3.966 | -30.5% |
| update_by_pk | 2.994 | 3.000 | +0.2% | 3.596 | 4.119 | +14.6% |
| delete_by_pk | 2.989 | 2.998 | +0.3% | 3.477 | 3.122 | -10.2% |
| transaction_commit | 3.002 | 2.997 | -0.2% | 4.612 | 3.604 | -21.9% |
| transaction_insert_10_commit | 3.033 | 3.001 | -1.1% | 5.216 | 4.004 | -23.2% |
| transaction_insert_100_commit | 6.002 | 4.903 | -18.3% | 7.939 | 6.726 | -15.3% |
| transaction_insert_1000_commit | 14.012 | 14.045 | +0.2% | 15.046 | 15.028 | -0.1% |
| transaction_mixed_dml_100_commit | 6.020 | 5.990 | -0.5% | 8.427 | 7.071 | -16.1% |
| transaction_rollback_100 | 1.644 | 0.960 | -41.6% | 2.079 | 2.120 | +2.0% |

Measured improvements landed in at least two required clusters:

- Write/transaction write: `transaction_insert_10_commit`, `transaction_insert_100_commit`, and `transaction_mixed_dml_100_commit` p95 improved.
- Delete: `delete_by_pk` p95 improved.
- Commit/rollback: `transaction_commit` p95 and `transaction_rollback_100` p50 improved.

## Counter Breakdown

| workload | before execute ms | after execute ms | before sync ms | after sync ms | after tx_stage ms | after tx_commit ms |
|---|---:|---:|---:|---:|---:|---:|
| insert | 493.789 | 440.219 | 464.839 | 417.229 | 0.000 | 0.000 |
| update_by_pk | 420.686 | 462.861 | 399.412 | 433.337 | 0.000 | 0.000 |
| delete_by_pk | 430.761 | 446.944 | 413.676 | 432.806 | 0.000 | 0.000 |
| transaction_commit | 457.335 | 442.692 | 420.180 | 418.366 | 0.219 | 432.674 |
| transaction_insert_10_commit | 495.037 | 446.735 | 395.663 | 370.038 | 0.857 | 403.461 |
| transaction_insert_100_commit | 172.397 | 153.216 | 82.723 | 68.206 | 1.062 | 101.121 |
| transaction_insert_1000_commit | 121.098 | 123.051 | 25.700 | 27.197 | 1.314 | 56.704 |
| transaction_mixed_dml_100_commit | 186.457 | 170.282 | 91.458 | 88.394 | 1.261 | 127.409 |
| transaction_rollback_100 | 47.891 | 35.367 | 0.000 | 0.000 | 0.615 | 0.000 |

Interpretation:

- Autocommit write/delete remains dominated by strict WAL sync latency, not row/index CPU time.
- The large rollback improvement comes from avoiding unnecessary transaction row/index snapshot work.
- The batch transaction improvements come from reducing transaction staging amplification and avoiding unnecessary FK/index snapshot work.
- `transaction_insert_1000_commit` is now mostly execute-path overhead and requires a larger transaction/WAL representation change to move materially.

## PostgreSQL Comparison

Command:

```bash
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --qm-sync-policy per-commit --postgres-dsn postgresql://qm_bench:qm_bench@localhost:5432/qm_bench --output /tmp/qmvir_pass3_pg_per_commit_after.json --strict
```

Environment:

- PostgreSQL: 17.9 Homebrew
- PostgreSQL settings: `fsync=on`, `synchronous_commit=on`
- NativeSqlEngine: persistent WAL, `per_commit_sync`
- QM durability: `sync_before_commit_return=true`, reopen validation passed

Selected results:

| workload | winner | QM/PG ops ratio | QM p50 ms | PG p50 ms | QM p95 ms | PG p95 ms |
|---|---|---:|---:|---:|---:|---:|
| insert | PostgreSQL | 0.031 | 2.991 | 0.078 | 3.797 | 0.148 |
| update_by_pk | PostgreSQL | 0.030 | 2.996 | 0.085 | 3.383 | 0.098 |
| delete_by_pk | PostgreSQL | 0.030 | 2.992 | 0.082 | 3.619 | 0.094 |
| transaction_commit | PostgreSQL | 0.052 | 2.974 | 0.132 | 3.991 | 0.146 |
| transaction_insert_10_commit | PostgreSQL | 0.208 | 3.005 | 0.462 | 4.459 | 1.607 |
| transaction_insert_100_commit | PostgreSQL | 0.889 | 4.248 | 3.876 | 7.597 | 6.405 |
| transaction_insert_1000_commit | QM | 1.942 | 15.374 | 35.969 | 54.439 | 41.406 |
| transaction_mixed_dml_100_commit | QM | 1.002 | 3.999 | 4.220 | 5.490 | 6.396 |
| transaction_rollback_100 | QM | 1.328 | 3.060 | 4.111 | 3.217 | 4.211 |
| select_by_pk | QM | 5.252 | 0.007 | 0.039 | 0.007 | 0.043 |
| count_indexed_equality | QM | 8.158 | 0.005 | 0.043 | 0.006 | 0.048 |

Claim boundary:

- Validated: QM wins selected read/index/rollback and some larger transaction workloads in this local benchmark.
- Not validated: QM does not beat PostgreSQL on strict durable single-row INSERT/UPDATE/DELETE/COMMIT.
- Root cause: PostgreSQL's local synchronous commit path is much faster than QM's current per-operation `sync_all()` path on this machine.

## Correctness Gates

Passed:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
python3 -m py_compile scripts/native_sql_write_profile.py
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features delete -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features transaction_ -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql_query_update_delete_audit -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture
python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX
python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --qm-sync-policy per-commit --postgres-dsn postgresql://qm_bench:qm_bench@localhost:5432/qm_bench --output /tmp/qmvir_pass3_pg_per_commit_after.json --strict
```

Not run:

- Full unfiltered Rust and Python suites were not rerun in this pass. The required filtered release gates were run and passed.

## Files Changed

Primary files touched by this pass:

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/native_sql_write_profile.py`
- `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS3_2026_06_15.md`

The worktree also contains pre-existing dirty changes and untracked files from earlier passes. This pass did not stage or commit anything.

## Remaining Bottlenecks

- Strict autocommit INSERT/UPDATE/DELETE are sync-bound. Further meaningful improvement requires WAL sync architecture work, not more row-path micro-optimization.
- `transaction_insert_1000_commit` execute path still has nontrivial staging/serialization overhead; moving further likely requires a compact transaction-local WAL representation or lower-allocation prepared-row staging.
- UPDATE/DELETE by PK fast paths reduce scan/FK overhead, but they cannot offset strict `sync_all()` latency for single-row durable writes.

## Suggested Commit Split

1. Native SQL profile counters and Python profile script exposure.
2. Transaction row-undo/index snapshot reduction.
3. FK reference gating and simple PK UPDATE/DELETE fast paths.
4. Pass 3 benchmark/report documentation.

