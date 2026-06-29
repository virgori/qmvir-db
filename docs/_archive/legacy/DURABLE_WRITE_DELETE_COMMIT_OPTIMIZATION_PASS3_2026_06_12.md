# QMvir Durable Write/Delete/Commit Optimization Pass 3 - 2026-06-12

## Verdict

`PASS3_IMPROVED_NEEDS_FULL_PG_BENCH`

## Summary

- PostgreSQL benchmark: not run in this pass.
- PostgreSQL win/loss claim: none.
- Scope kept to durable write/delete/commit hygiene and calibration. No engine durability weakening was introduced.
- Strict autocommit and explicit `COMMIT` still wait for WAL `flush()` plus `sync_all()` through the existing strict sync path.
- The main measured improvement is a profiler correction for `delete_by_pk`, which now measures DELETE itself instead of INSERT plus DELETE.
- A small transaction staging optimization was added in Rust by preallocating the per-transaction WAL SQL vector.
- Rust linked tests and Python wheel rebuild were blocked by the local Xcode license state, so this is not a full release gate.

## Changes

| Area | File | Change | Durability impact |
|---|---|---|---|
| DELETE calibration | `scripts/native_sql_write_profile.py` | Seed DELETE rows up front and reset `next_id`, so `delete_by_pk` measures DELETE only | No engine behavior change |
| Transaction commit staging | `qm_engine/src/gateway/native_sql.rs` | Initialize transaction `wal_sql` with `Vec::with_capacity(128)` | No WAL ordering, append, flush, or sync behavior change |

## Calibration Artifacts

Generated artifacts were kept outside the repository:

- `/private/tmp/qmvir_pg_perf_pass3_2026_06_12/write_profile_before.json`
- `/private/tmp/qmvir_pg_perf_pass3_2026_06_12/write_profile_after_script_delete_fix_stale_wheel.json`

Important caveat: the after-profile used the existing Python extension wheel because `maturin build` was blocked by the local Xcode license. Therefore the after-profile validates the profiler DELETE correction against the existing wheel, but it does not fully calibrate the new Rust `wal_sql` preallocation change through Python.

## Before/After Calibration

Label: `CALIBRATION_ONLY_NOT_RELEASE_CLAIM`

Command shape:

```bash
python3 scripts/native_sql_write_profile.py --iterations 50 --warmup 5 --repeat 3 --trace --output <tmp-json>
```

| Workload | Before p50 ms | Before p95 ms | After p50 ms | After p95 ms | Delta p50 | Delta p95 |
|---|---:|---:|---:|---:|---:|---:|
| insert | 2.998 | 3.825 | 2.998 | 3.534 | +0.000 | -0.291 |
| update_by_pk | 2.995 | 3.816 | 3.000 | 3.060 | +0.005 | -0.756 |
| delete_by_pk | 5.999 | 6.980 | 2.997 | 3.048 | -3.002 | -3.931 |
| transaction_commit | 8.999 | 10.020 | 9.004 | 10.993 | +0.005 | +0.973 |
| transaction_insert_10_commit | 12.027 | 16.009 | 12.022 | 15.000 | -0.005 | -1.009 |
| transaction_insert_100_commit | 30.030 | 39.990 | 27.266 | 37.186 | -2.764 | -2.804 |
| transaction_insert_1000_commit | 649.888 | 1001.885 | 647.068 | 992.198 | -2.821 | -9.687 |
| transaction_mixed_dml_100_commit | 16.969 | 19.939 | 16.081 | 18.009 | -0.887 | -1.930 |
| transaction_rollback_100 | 9.951 | 10.827 | 9.991 | 10.678 | +0.040 | -0.149 |

## Counter Evidence

| Workload | Before sync_all_count | After sync_all_count | Before WAL bytes | After WAL bytes | Interpretation |
|---|---:|---:|---:|---:|---|
| insert | 150 | 150 | 14430 | 14430 | Strict sync count unchanged |
| update_by_pk | 150 | 150 | 11322 | 11322 | Strict sync count unchanged |
| delete_by_pk | 300 | 150 | 20580 | 5988 | Profiler now measures DELETE only, removing the hidden INSERT side effect |
| transaction_commit | 150 | 150 | 14430 | 14430 | Strict commit sync count unchanged |
| transaction_insert_10_commit | 150 | 150 | 143886 | 143886 | Strict commit sync count unchanged |
| transaction_insert_100_commit | 30 | 30 | 291960 | 291960 | Strict commit sync count unchanged |
| transaction_insert_1000_commit | 9 | 9 | 875904 | 875904 | Strict commit sync count unchanged |
| transaction_mixed_dml_100_commit | 30 | 30 | 272277 | 272277 | Strict commit sync count unchanged |
| transaction_rollback_100 | 0 | 0 | 0 | 0 | Rollback does not append committed WAL in this workload |

## Bottleneck Diagnosis

| Area | Status | Evidence |
|---|---|---|
| Autocommit INSERT/UPDATE | Still dominated by strict WAL sync | Baseline `insert` and `update_by_pk` each used 150 `sync_all` calls; most profile time was in `wal_sync_ns` |
| DELETE calibration | Improved and clarified | `delete_by_pk` sync count dropped from 300 to 150 and WAL bytes from 20580 to 5988 because the profiler no longer inserts a row inside the timed DELETE operation |
| Large transaction INSERT | Still mostly execute-path dominated | `transaction_insert_1000_commit` keeps only 9 strict syncs across samples, while total latency remains high |
| Commit staging | Small source-level optimization added | Transaction WAL staging now starts with capacity for common multi-statement transactions |

## Verification

| Check | Result |
|---|---|
| `python3 -m py_compile scripts/native_sql_write_profile.py scripts/release_benchmark_native_sql.py` | PASS |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | PASS |
| `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX` | PASS, `126 passed` |
| `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX` | PASS, `59 passed` |
| `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8` | PASS |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | PASS |
| `python3 -m maturin build ...` | BLOCKED, local Xcode license prevents linker invocation |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture` | BLOCKED at link step by local Xcode license |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture` | BLOCKED at link step by local Xcode license |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture` | BLOCKED at link step by local Xcode license |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture` | BLOCKED at link step by local Xcode license |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture` | BLOCKED at link step by local Xcode license |

The linker-blocked commands failed with the local environment error:

```text
You have not agreed to the Xcode license agreements.
```

This is an environment blocker, not an observed QMvir test assertion failure.

## Release Interpretation

This pass improved calibration quality and made a small safe source optimization, but it is not enough for a final release claim because:

- No full PostgreSQL benchmark was run.
- The Python extension wheel could not be rebuilt after the Rust source change.
- Linked Rust tests could not run because the local Xcode license blocks `cc`/`xcrun`.

## Required Next Gate

Before making a release performance claim:

- Accept/fix the local Xcode license state and rerun linked Rust tests.
- Rebuild/install the Python extension from the current Rust source.
- Rerun the Pass 3 profiler with the rebuilt wheel.
- Run the full PostgreSQL comparison benchmark on an idle machine with a valid benchmark database.
- Keep generated JSON, WAL logs, static libraries, npm binaries, `.env`, `.DS_Store`, `dist`, and benchmark artifacts out of the repository.

Final verdict for this pass remains:

`PASS3_IMPROVED_NEEDS_FULL_PG_BENCH`
