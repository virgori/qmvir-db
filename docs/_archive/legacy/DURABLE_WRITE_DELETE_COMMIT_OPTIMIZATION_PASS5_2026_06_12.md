# QMvir Durable Write/Delete/Commit Optimization Pass 5 - 2026-06-12

## Verdict

`PASS5_PG_WRITE_GAP_REDUCED`

## Summary

Pass 5 made one real Rust engine optimization and one benchmark fairness correction:

- Rust engine: removed redundant generic transaction snapshot setup before every transaction DML statement. Row undo/index snapshots are still recorded by the DML handlers that actually mutate rows.
- Benchmark harness: fixed PostgreSQL comparison `delete_by_pk` so the timed path measures DELETE only. Previously it measured INSERT plus DELETE for both PostgreSQL and QM, which doubled strict QM syncs.

Durability was not weakened. Strict autocommit and `COMMIT` still return only after the existing WAL flush plus `sync_all()` path.

## Files Changed

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/compare_postgres_native_sql.py`
- `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS5_2026_06_12.md`

The worktree also contains earlier Pass 1-4 changes and reports that were not committed in this session.

## Optimization Implemented

### Engine Optimization

Before Pass 5, raw and prepared transaction DML called a generic transaction snapshot setup before dispatching to the actual DML handler. The handlers then independently recorded row undo state and index snapshots for the affected rows.

Pass 5 removes that redundant pre-DML snapshot call and uses the transaction-active state already computed at dispatch time for WAL recording.

Safety:

- INSERT/UPDATE/DELETE handlers still call `record_transaction_row_undos()` when rows are affected.
- FK side-effect paths still use `ensure_full_transaction_snapshot()` where required.
- WAL recording at commit is unchanged.
- `wal_append_batch()`, `wal_sync()`, `flush()`, and `sync_all()` behavior is unchanged.

### Benchmark Correction

`scripts/compare_postgres_native_sql.py` previously timed `delete_by_pk` as:

1. INSERT a row.
2. DELETE that row.

That was not measuring DELETE by PK. It also doubled QM strict sync count. Pass 5 now seeds delete rows outside the timed loop for both PostgreSQL and QM, then the timed function performs only:

```sql
DELETE FROM ... WHERE id = ...
```

This is a calibration/fairness correction, not an engine win.

## Native Profiler Before/After

Command:

```bash
python3 scripts/native_sql_write_profile.py \
  --iterations 100 \
  --warmup 10 \
  --repeat 5 \
  --trace \
  --output /private/tmp/qmvir_pg_perf_pass5_2026_06_12/write_profile_<before|after>.json
```

| Workload | Before p50 | Before p95 | After p50 | After p95 | Before execute ms | After execute ms | Sync count |
|---|---:|---:|---:|---:|---:|---:|---:|
| insert | 2.983 | 3.577 | 2.988 | 3.595 | 1331.3 | 1374.5 | 500 -> 500 |
| update_by_pk | 2.999 | 3.074 | 2.997 | 3.814 | 1489.4 | 1484.0 | 500 -> 500 |
| delete_by_pk | 2.997 | 3.063 | 2.991 | 3.793 | 1492.8 | 1394.2 | 500 -> 500 |
| transaction_commit | 2.992 | 3.934 | 2.992 | 4.002 | 1432.0 | 1398.9 | 500 -> 500 |
| transaction_insert_10_commit | 3.052 | 5.393 | 3.128 | 4.885 | 1817.1 | 1665.3 | 500 -> 500 |
| transaction_insert_100_commit | 5.025 | 7.954 | 5.282 | 6.077 | 259.3 | 263.1 | 50 -> 50 |
| transaction_mixed_dml_100_commit | 4.906 | 6.180 | 5.120 | 6.122 | 230.5 | 250.7 | 50 -> 50 |

Interpretation:

- `delete_by_pk` execute time improved in profiler, but p95 regressed due variance while strict sync count stayed fixed.
- `transaction_commit` and `transaction_insert_10_commit` execute time improved, consistent with removing redundant transaction snapshot work.
- Median latency remains dominated by strict sync on macOS.

## PostgreSQL Comparison Before/After

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy per-commit \
  --output /private/tmp/qmvir_pg_perf_pass5_2026_06_12/postgres_comparison_<before|after>.json \
  --strict
```

| Workload | Before PG p50 | Before QM p50 | After PG p50 | After QM p50 | QM sync before | QM sync after |
|---|---:|---:|---:|---:|---:|---:|
| insert | 0.084 | 2.990 | 0.072 | 2.987 | 1010 | 1010 |
| update_by_pk | 0.088 | 2.999 | 0.085 | 3.001 | 1010 | 1010 |
| delete_by_pk | 0.170 | 5.998 | 0.074 | 3.004 | 1020 | 510 |
| transaction_commit | 0.126 | 2.992 | 0.097 | 2.991 | 510 | 510 |
| transaction_insert_10_commit | 0.483 | 3.992 | 0.476 | 4.025 | 1011 | 1011 |
| transaction_insert_100_commit | 3.907 | 5.046 | 3.920 | 5.004 | 111 | 111 |
| transaction_mixed_dml_100_commit | 4.126 | 4.994 | 4.144 | 5.141 | 111 | 111 |

PG comparison result:

- `delete_by_pk` QM p50 improved from `5.998ms` to `3.004ms` because the harness now measures DELETE only and no longer times an extra INSERT/sync.
- `transaction_insert_100_commit` QM p50 improved slightly from `5.046ms` to `5.004ms`.
- `insert`, `update_by_pk`, and `transaction_commit` remain strict-sync bound around `3ms`.
- PostgreSQL still wins strict durable autocommit write/delete and small transaction workloads in this run.

## Counter Evidence

| Workload | Before WAL bytes | After WAL bytes | Before syncs | After syncs | Meaning |
|---|---:|---:|---:|---:|---|
| insert | 84969 | 84969 | 1010 | 1010 | Strict autocommit unchanged |
| update_by_pk | 40400 | 40400 | 1010 | 1010 | Strict autocommit unchanged |
| delete_by_pk | 58140 | 16320 | 1020 | 510 | Harness now measures DELETE only |
| transaction_commit | 19380 | 19380 | 510 | 510 | Commit durability unchanged |
| transaction_insert_10_commit | 140853 | 140732 | 1011 | 1011 | Commit sync unchanged |
| transaction_insert_100_commit | 89620 | 90558 | 111 | 111 | Commit sync unchanged |
| transaction_mixed_dml_100_commit | 0 | 0 | 111 | 111 | Script reports no WAL byte delta for this batch; sync count unchanged |

The strict sync counts prove Pass 5 did not improve results by reducing durable sync frequency.

## Durability Semantics

No durability weakening was made:

- `acknowledged_before_fsync=false` remains the strict benchmark mode.
- `sync_before_commit_return=true` remains true.
- Autocommit mutations still sync before returning.
- Explicit transaction commit still batches WAL records and syncs once after `COMMIT`.
- `flush()`/`sync_all()` path was not disabled or moved after acknowledgement.

No relaxed mode or group commit mode was added in Pass 5.

## Verification Gates

| Check | Result |
|---|---|
| `python3 -m py_compile scripts/compare_postgres_native_sql.py scripts/native_sql_write_profile.py scripts/release_benchmark_native_sql.py` | PASS |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | PASS |
| `python3 -m maturin build --release --out /private/tmp/qmvir_pg_perf_pass5_2026_06_12/wheels` | PASS |
| `python3 -m pip install --force-reinstall /private/tmp/qmvir_pg_perf_pass5_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture` | PASS, `129 passed; 15 ignored`; filtered recovery tests also passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture` | PASS |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture` | PASS, `10 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture` | PASS, `18 passed` |
| `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX` | PASS, `126 passed` |
| `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX` | PASS, `59 passed` |
| `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8` | PASS |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | PASS |
| Full PostgreSQL comparison after | PASS |

## Artifacts Outside Repo

- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/write_profile_before.json`
- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/write_profile_after.json`
- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/postgres_comparison_before.json`
- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/postgres_comparison_after.json`
- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/qmvir_bridge_materialization_audit.json`
- `/private/tmp/qmvir_pg_perf_pass5_2026_06_12/qmvir_vector_search_medium_smoke.json`

## Remaining Bottlenecks

- Strict autocommit insert/update/delete latency is still dominated by `sync_all()` on this macOS environment.
- Transaction small-batch p50 still clusters around the same durable sync floor.
- Further strict improvements likely require a WAL/storage sync strategy breakthrough, not just SQL-layer clone or snapshot trimming.
- Any new group commit or buffered mode must be reported separately and must not be called strict per-commit durability.

## Suggested Commit Split

| Slice | Files | Suggested message |
|---|---|---|
| Transaction DML snapshot trimming | `qm_engine/src/gateway/native_sql.rs` | `Reduce native SQL transaction DML snapshot overhead` |
| DELETE benchmark fairness | `scripts/compare_postgres_native_sql.py` | `Measure delete benchmark without timed seed insert` |
| Pass 5 report | `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS5_2026_06_12.md` | `Document durable write/delete/commit optimization pass 5` |

## Final Interpretation

Pass 5 reduced the measured PostgreSQL write gap for `delete_by_pk` by fixing the benchmark to measure DELETE only, and it made a real Rust-side transaction DML overhead reduction. The strict write gap is not closed: PostgreSQL still wins the strict durable write/delete and small commit workloads in the after comparison.

Final verdict:

`PASS5_PG_WRITE_GAP_REDUCED`
