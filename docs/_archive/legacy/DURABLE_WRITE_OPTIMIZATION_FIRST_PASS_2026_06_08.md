# QMvir Durable Write Optimization First Pass - 2026-06-08

Final verdict: `OPTIMIZATION_IMPROVED_BUT_NEEDS_FULL_BENCH`

## Scope

This pass paused the full PostgreSQL benchmark and optimized the NativeSqlEngine durable write path first. No PostgreSQL win/loss claim is made in this report.

Constraints kept:

- `acknowledged_before_fsync=false` for strict per-commit calibration.
- COMMIT/autocommit mutation still waits for `wal_sync()` in `per_commit_sync` and `per_mutation_sync`.
- No benchmark numbers were edited.
- No generated benchmark JSON was written into the repository.
- No commit was created.

## Code Change

Changed file:

- `qm_engine/src/gateway/native_sql.rs`

Implemented:

- Added optional WAL hot-path counters exposed as `NativeSqlEngine.wal_trace_snapshot()` when `QMVIR_WAL_TRACE=1`.
- Removed per-append `flush()` from `wal_append()`. Strict durability remains in `wal_sync()`, which still performs `flush()` followed by `sync_all()`.
- Added `wal_append_batch()` for explicit transaction commit, so transaction WAL statements are written under one WAL writer lock and then synced once in strict policy.
- Preserved checkpoint and recovery behavior; checkpoint may still perform its own sync when triggered.

Trace smoke artifact:

- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/wal_trace_smoke.json`

Trace smoke observed one autocommit append, one transaction batch append, and strict sync counters remained active.

## Calibration Artifacts

All generated artifacts for this pass are outside the repo:

- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/baseline_per_commit_100.json`
- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/calibration_after_slice_1.json`
- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/calibration_after_slice_1_100.json`
- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/qm_calibration_after_slice_1.json`
- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/qmvir_bridge_materialization_audit.json`
- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/qmvir_vector_search_medium_smoke.json`
- `/private/tmp/qmvir_pg_perf_warroom_2026_06_08/interrupted_benchmark_status.txt`

The earlier full PostgreSQL run was interrupted after 681 seconds and is not a valid comparison.

PostgreSQL was unavailable for calibration:

- DSN: `dbname=postgres user=postgres host=/tmp`
- Failure: role `postgres` does not exist
- Result: `postgresql_available=false`

## Calibration Summary

Mode: persistent NativeSqlEngine WAL, `per_commit_sync`, 100 iterations, calibration only.

| Workload | Before p50 ms | Before p95 ms | After p50 ms | After p95 ms | Sync delta |
|---|---:|---:|---:|---:|---:|
| insert | 3.001 | 4.123 | 2.981 | 3.064 | 110 -> 110 |
| update_by_pk | 2.999 | 3.647 | 3.007 | 7.491 | 110 -> 110 |
| delete_by_pk | 5.823 | 10.651 | 4.924 | 7.091 | 120 -> 120 |
| transaction_commit | 3.934 | 4.771 | 4.989 | 6.025 | 60 -> 60 |
| transaction_insert_10_commit | 18.006 | 39.992 | 15.985 | 36.020 | 110 -> 110 |
| transaction_insert_100_commit | 188.888 | 338.812 | 178.870 | 248.396 | 20 -> 20 |
| transaction_insert_1000_commit | 703.896 | 703.896 | 817.857 | 817.857 | 2 -> 2 |
| transaction_mixed_dml_100_commit | 688.801 | 805.263 | 681.973 | 1092.097 | 20 -> 20 |
| transaction_rollback_100 | 754.312 | 961.973 | 690.094 | 777.285 | 0 -> 0 |

Interpretation:

- Main improvement is visible in `transaction_insert_10_commit`, `transaction_insert_100_commit`, `delete_by_pk`, and `transaction_rollback_100`.
- `transaction_insert_1000_commit` and mixed DML still need profiling; no release claim should rely on them yet.
- Sync counts did not decrease because benchmark warmups and commit-level sync accounting still dominate the reported counter. The optimization reduces per-statement WAL writer lock/flush overhead inside explicit transactions.

## Verification

Passed:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture`
- `python3 -m pytest -q -rxX`
- `python3 -m pytest tests/test_full_engine.py tests/test_wal_concurrency.py tests/test_native_sql_python_bridge_identity_uuid_json.py tests/test_python_rust_bridge_true_zero_copy.py -q -rxX`
- `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX`
- `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8`
- `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke`

Observed full gate results:

- Rust no-default: `451 passed; 15 ignored`, integration suites passed, doc-tests `2 ignored`.
- Python: `1163 passed, 12 skipped`.
- Targeted Python bridge/native SQL: `146 passed`.
- Targeted zero-copy/vector: `59 passed`.

## Artifact Hygiene

Tracked artifact scan returned no matches for:

- `.DS_Store`
- `.env`
- `dist/`
- `target/`
- `node_modules/`
- `__pycache__`
- `.pytest_cache`
- static `.a` libraries
- benchmark/report JSON patterns under docs

Current remaining repo dirt after this pass:

- Modified: `qm_engine/src/gateway/native_sql.rs`
- Untracked report: `docs/DURABLE_WRITE_OPTIMIZATION_FIRST_PASS_2026_06_08.md`
- Untracked pre-existing/local paths: `_py_legacy/`, `tests/_probe5.py`

No staged changes are present.

## Release Gate Recommendation

Do not publish a PostgreSQL performance claim from this pass.

Safe next step is a dedicated second profiling pass for:

- mixed DML transaction commit path,
- 1000-row transaction insert path,
- benchmark warmup/sync accounting clarity,
- repeatable full PostgreSQL comparison after creating a valid benchmark role/database.
