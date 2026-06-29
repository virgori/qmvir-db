# QMvir Rust WAL Sync Path Optimization Pass 7

Audit date: 2026-06-12

## Verdict

`PASS7_SYNC_DATA_POLICY_ADDED`

Pass 7 added a Rust WAL sync floor benchmark, added an explicit non-default `per_commit_sync_data` policy, rebuilt the Python wheel, and benchmarked strict `sync_all` separately from `sync_data`.

The main finding is direct: NativeSqlEngine strict write latency on this macOS host is explained by Rust file sync cost. A NativeSql-like Rust `BufWriter<File>` append + flush + `File::sync_all()` has p50 near 2.8ms, matching the engine's roughly 3.0ms strict DML floor. `File::sync_data()` is not materially faster on this host.

## Files Changed

- `qm_engine/Cargo.toml`
- `qm_engine/src/bin/wal_sync_floor.rs`
- `qm_engine/src/gateway/native_sql.rs`
- `scripts/compare_postgres_native_sql.py`
- `scripts/native_sql_write_profile.py`
- `docs/DURABLE_RUST_WAL_SYNC_PATH_OPTIMIZATION_PASS7_2026_06_12.md`

## Rust Sync Floor Result

Artifact:

`/private/tmp/qmvir_pg_perf_pass7_2026_06_12/rust_wal_sync_floor.json`

Command:

```bash
cargo run --release --manifest-path qm_engine/Cargo.toml --no-default-features --bin wal_sync_floor -- \
  --iterations 200 \
  --output /private/tmp/qmvir_pg_perf_pass7_2026_06_12/rust_wal_sync_floor.json \
  --root /private/tmp/qmvir_pg_perf_pass7_2026_06_12/rust_wal_sync_floor
```

| Case | total p50 ms | total p95 ms | sync p50 ms | sync p95 ms |
|---|---:|---:|---:|---:|
| tiny append + flush + `sync_all` | 2.838 | 3.294 | 2.792 | 3.251 |
| 4KB append + flush + `sync_all` | 2.865 | 3.838 | 2.801 | 3.735 |
| batch 100 tiny + flush + `sync_all` | 2.868 | 2.994 | 2.712 | 2.862 |
| tiny append + flush + `sync_data` | 2.844 | 2.970 | 2.776 | 2.927 |
| 4KB append + flush + `sync_data` | 2.852 | 2.925 | 2.783 | 2.863 |
| batch 100 tiny + flush + `sync_data` | 2.866 | 3.725 | 2.711 | 3.436 |

This resolves the Pass 6 discrepancy. The Python microbenchmark did not reproduce the full engine floor because it did not measure the same Rust `BufWriter<File>` append/flush/sync path. The Rust benchmark does reproduce the floor.

## NativeSqlEngine Counters

Artifacts:

- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/write_profile_strict_after.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/write_profile_sync_data.json`

Strict `per_commit_sync`:

| Workload | p50 ms | p95 ms | sync ms/op | sync_all count | sync_data count |
|---|---:|---:|---:|---:|---:|
| insert | 2.988 | 3.577 | 2.608 | 500 | 0 |
| update_by_pk | 2.996 | 3.393 | 2.839 | 500 | 0 |
| delete_by_pk | 2.999 | 3.079 | 2.869 | 500 | 0 |
| transaction_commit | 2.994 | 3.088 | 2.519 | 500 | 0 |
| transaction_insert_10_commit | 3.363 | 5.018 | 2.746 | 500 | 0 |
| transaction_insert_100_commit | 4.892 | 6.090 | 2.656 | 50 | 0 |

`per_commit_sync_data`:

| Workload | p50 ms | p95 ms | sync ms/op | sync_all count | sync_data count |
|---|---:|---:|---:|---:|---:|
| insert | 2.991 | 3.549 | 2.649 | 0 | 500 |
| update_by_pk | 2.998 | 3.594 | 2.859 | 0 | 500 |
| delete_by_pk | 2.998 | 3.227 | 2.828 | 0 | 500 |
| transaction_commit | 2.992 | 3.873 | 2.562 | 0 | 500 |
| transaction_insert_10_commit | 3.002 | 4.036 | 2.491 | 0 | 500 |
| transaction_insert_100_commit | 5.016 | 6.995 | 2.437 | 0 | 50 |

## New Policy Semantics

| Policy | Engine string | Sync call before return | Durability after return | Metadata semantics | Default |
|---|---|---|---|---|---|
| Per-commit sync | `per_commit_sync` / `per_commit` | `File::sync_all()` | Yes | Data + metadata sync as exposed by Rust std | No |
| Per-commit sync data | `per_commit_sync_data` / `per-commit-sync-data` | `File::sync_data()` | Data sync before return | Not claimed identical to `sync_all`; WAL file is created before this path is selected | No |
| Relaxed OS buffered | `relaxed_os_buffered` / `relaxed-os-buffered` | None; `flush()` only | No strict crash-durable-at-return claim | WAL may be lost on OS crash/power loss before later sync | No |

Default strict behavior was not changed.

## PostgreSQL Comparison

Artifacts:

- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/postgres_comparison_strict_after.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/postgres_comparison_sync_data.json`

Strict `per_commit_sync`:

| Workload | QM p50 ms | QM p95 ms | sync count | Winner |
|---|---:|---:|---:|---|
| insert | 2.986 | 4.130 | 1010 | PostgreSQL |
| update_by_pk | 2.991 | 4.223 | 1010 | PostgreSQL |
| delete_by_pk | 2.994 | 4.825 | 510 | PostgreSQL |
| transaction_commit | 2.989 | 4.194 | 510 | PostgreSQL |
| transaction_insert_10_commit | 3.990 | 6.148 | 1011 | PostgreSQL |
| transaction_insert_100_commit | 4.943 | 6.018 | 111 | PostgreSQL |
| transaction_mixed_dml_100_commit | 5.972 | 7.960 | 111 | PostgreSQL |

`per_commit_sync_data`:

| Workload | QM p50 ms | QM p95 ms | sync count | Interpretation |
|---|---:|---:|---:|---|
| insert | 2.989 | 3.950 | 1010 | no material macOS win |
| update_by_pk | 3.003 | 4.242 | 1010 | no material macOS win |
| delete_by_pk | 3.003 | 4.310 | 510 | no material macOS win |
| transaction_commit | 2.991 | 4.579 | 510 | no material macOS win |
| transaction_insert_10_commit | 4.024 | 5.945 | 1011 | no material macOS win |
| transaction_insert_100_commit | 5.097 | 6.235 | 111 | no material macOS win |
| transaction_mixed_dml_100_commit | 5.043 | 6.139 | 111 | mixed variance; not a strict win claim |

PostgreSQL remains much faster for single-row strict write/delete and small commit workloads on this macOS setup.

## Regression Gates

Passed:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m maturin build --release --out /private/tmp/qmvir_pg_perf_pass7_2026_06_12/wheels`
- `python3 -m pip install --force-reinstall /private/tmp/qmvir_pg_perf_pass7_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture`
- `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX`
- `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX`
- `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8`
- `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke`

New native test:

- `per_commit_sync_data_policy_syncs_at_return_and_recovers`

## Artifacts Outside Repo

- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/rust_wal_sync_floor.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/write_profile_strict_after.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/write_profile_sync_data.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/postgres_comparison_strict_after.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/postgres_comparison_sync_data.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/qmvir_bridge_materialization_audit.json`
- `/private/tmp/qmvir_pg_perf_pass7_2026_06_12/qmvir_vector_search_medium_smoke.json`

## Linux Audit Implication

macOS is not the place to continue optimizing strict single-commit write latency after this pass. The Rust sync floor is now confirmed around the same latency as NativeSqlEngine strict DML, and `sync_data()` does not materially reduce it here.

Next audit should move to Linux and measure:

- `File::sync_all()`
- `File::sync_data()` / `fdatasync`
- `O_DSYNC` or equivalent open flags
- group commit under concurrent writers

Do not claim a strict write win on macOS from `per_commit_sync_data`.

## Suggested Commit Split

| Slice | Paths | Suggested message |
|---|---|---|
| Rust sync floor benchmark | `qm_engine/Cargo.toml`, `qm_engine/src/bin/wal_sync_floor.rs` | `Add Rust WAL sync floor benchmark` |
| Sync-data policy | `qm_engine/src/gateway/native_sql.rs`, `scripts/compare_postgres_native_sql.py`, `scripts/native_sql_write_profile.py` | `Add explicit NativeSqlEngine sync_data WAL policy` |
| Pass 7 report | `docs/DURABLE_RUST_WAL_SYNC_PATH_OPTIMIZATION_PASS7_2026_06_12.md` | `Document Rust WAL sync path optimization pass 7` |

## Final Release Interpretation

`per_commit_sync` remains the strict baseline and still sits near the macOS Rust sync floor.

`per_commit_sync_data` is a useful explicit policy to carry into Linux audit, but on this macOS host it does not materially improve strict write latency and must not be marketed as a PostgreSQL strict-write win.
