# Persistent WAL / PostgreSQL / Checkpoint Audit - 2026-06-02

## Summary

This pass added an explicit persistent NativeSqlEngine WAL benchmark mode and reran PostgreSQL comparison without weakening PostgreSQL durability settings.

Result:

- Memory/autocommit NativeSqlEngine still beats PostgreSQL on all measured scalar workloads.
- Persistent NativeSqlEngine WAL mode now uses a real `data_dir`, explicit WAL sync after timed mutations, and reopen durability checks.
- Persistent WAL mode does not justify a durable PostgreSQL-equivalent performance win. QM wins read/index/count workloads, but loses fsync-heavy mutating workloads by a large margin.
- Checkpoint/WAL pressure remains a release-performance caveat: dirty checkpoint p50 is still around 13-16 ms.

## Files Changed

| File | Change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | Added PyO3 `NativeSqlEngine.sync_wal()` so benchmark code can force WAL durability without silently using memory mode. |
| `scripts/compare_postgres_native_sql.py` | Added `--qm-mode memory|persistent-wal`, persistent temp `data_dir`, reopen sanity checks, WAL sync after persistent mutations, and explicit durability metadata. |
| `docs/postgres_comparison_memory_latest.json` | New memory-mode comparison output. |
| `docs/postgres_comparison_persistent_wal_latest.json` | New persistent-WAL comparison output. |
| `docs/native_sql_perf_investigation_last.json` | Refreshed checkpoint/WAL pressure investigation output. |

## Benchmark Modes

### Memory Mode

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode memory \
  --output docs/postgres_comparison_memory_latest.json \
  --strict
```

Scope:

- `qm_mode=memory`
- `qm_durability_mode=native_sql_default_in_memory_autocommit`
- `wal_enabled=false`
- `fsync_enabled=false`
- Claim scope: local in-memory/autocommit scalar comparison, not durability-equivalent.

### Persistent WAL Mode

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --output docs/postgres_comparison_persistent_wal_latest.json \
  --strict
```

Scope:

- `qm_mode=persistent-wal`
- `qm_durability_mode=persistent_native_sql_wal_with_explicit_wal_sync_per_mutation`
- `wal_enabled=true`
- `fsync_enabled=true`
- `reopen_validation_passed=true`
- Claim scope: persistent NativeSqlEngine WAL with explicit `sync_wal()` after each timed mutation; durability sanity checked by reopen.

## Durability Sanity Checks

Persistent mode aborts before timing if any check fails:

- committed insert survives reopen
- rolled-back insert is not visible after reopen
- committed update survives reopen
- committed delete survives reopen
- indexed select after reopen returns expected row
- range count after reopen returns expected row count

Latest result: passed.

## PostgreSQL Settings

From latest strict run:

- PostgreSQL version: 17.9 Homebrew
- `fsync=on`
- `synchronous_commit=on`
- DSN recorded sanitized in JSON

PostgreSQL settings were not weakened.

## Memory Mode Results

| Workload | QM p50 ms | PostgreSQL p50 ms | Winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 0.004208 | 0.103166 | QM | 24.442 |
| select_by_pk | 0.002958 | 0.036292 | QM | 12.057 |
| update_by_pk | 0.002500 | 0.105167 | QM | 40.854 |
| delete_by_pk | 0.006917 | 0.203750 | QM | 29.058 |
| indexed_integer_equality | 0.004458 | 0.039291 | QM | 8.817 |
| indexed_string_equality_duplicate_heavy | 0.016375 | 0.051458 | QM | 3.113 |
| count_indexed_equality | 0.003292 | 0.041458 | QM | 12.330 |
| predicate_range | 0.004500 | 0.049209 | QM | 11.069 |
| transaction_commit | 0.003584 | 0.153042 | QM | 40.577 |
| transaction_rollback | 0.005333 | 0.083959 | QM | 15.071 |
| indexed_string_equality_unique | 0.003750 | 0.037458 | QM | 9.815 |

Approved claim: QM beats PostgreSQL on these local in-memory/autocommit scalar workloads.

## Persistent WAL Results

| Workload | QM p50 ms | PostgreSQL p50 ms | Winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 2.990083 | 0.071917 | PostgreSQL | 0.032 |
| select_by_pk | 0.008459 | 0.036208 | QM | 3.983 |
| update_by_pk | 2.998500 | 0.094500 | PostgreSQL | 0.034 |
| delete_by_pk | 2.997250 | 0.173667 | PostgreSQL | 0.066 |
| indexed_integer_equality | 0.011959 | 0.039417 | QM | 3.102 |
| indexed_string_equality_duplicate_heavy | 0.028666 | 0.051500 | QM | 1.744 |
| count_indexed_equality | 0.004708 | 0.049958 | QM | 10.552 |
| predicate_range | 0.006250 | 0.049083 | QM | 7.792 |
| transaction_commit | 2.989334 | 0.134833 | PostgreSQL | 0.051 |
| transaction_rollback | 0.015958 | 0.083209 | QM | 3.998 |
| indexed_string_equality_unique | 0.011292 | 0.037375 | QM | 3.636 |

Approved claim: persistent NativeSqlEngine read/index/count workloads are still faster in this local run.

Rejected claim: persistent NativeSqlEngine durable mutating workloads do not beat PostgreSQL. Insert, update, delete, and transaction commit are much slower when every mutation is followed by explicit WAL fsync.

## Checkpoint/WAL Pressure

Command:

```bash
python3 scripts/perf_investigate_native_sql.py \
  --iterations 1000 \
  --output docs/native_sql_perf_investigation_last.json \
  --build-mode release \
  --feature-flags default
```

Latest selected results:

| Workload | p50 ms | p95 ms | Throughput ops/s |
| --- | ---: | ---: | ---: |
| `wal.record_construction` | 0.000125 | 0.000125 | 5773372.0 |
| `wal.file_write` | 0.001458 | 0.002041 | 635172.7 |
| `wal.file_fsync` | 0.000417 | 0.000500 | 1927701.6 |
| `checkpoint.no_dirty_tables` | 0.000125 | 0.000166 | 4848413.7 |
| `wal.commit_latency` | 0.007375 | 0.008292 | 130423.4 |
| `checkpoint.dirty_small_table` | 12.996125 | 15.988416 | 77.0 |
| `checkpoint.dirty_large_table` | 14.172375 | 18.205958 | 66.7 |
| `wal.commit_with_checkpoint_pressure` | 15.724084 | 18.494166 | 64.3 |
| `recovery.load_from_checkpoint` | 1.616667 | 1.616667 | 618.6 |

## Root Cause Analysis

The refreshed measurements separate three facts:

- WAL record construction, file write, and isolated fsync microbenchmarks are not the main latency source.
- Clean checkpoint is effectively eliminated and remains sub-micro/micro level.
- Dirty checkpoint and commit-with-checkpoint pressure are still dominated by synchronous checkpoint work.

Current checkpoint path still performs table-level checkpointing:

- dirty table names are collected at table granularity;
- dirty table snapshots are serialized as whole table files;
- temp files are `sync_all()`ed before rename;
- manifest is serialized and synced;
- index catalog may be rewritten when index dirty state is set;
- compatibility `native_sql.snap` marker is written/synced;
- WAL is truncated and reopened;
- checkpoint runs synchronously in the commit/checkpoint-pressure path.

This explains why dirty small and dirty large table checkpoints are close in latency: the fixed sync/rename/metadata cost and table-level serialization dominate, not the size of a single small row mutation.

## Remaining Bottlenecks

| Area | Status | Impact |
| --- | --- | --- |
| Per-mutation durable WAL sync | Persistent WAL insert/update/delete/commit p50 around 3ms | PostgreSQL wins durable mutating workloads. |
| Synchronous checkpoint | Dirty checkpoint p50 around 13-16ms | Commit-with-checkpoint pressure remains high. |
| Table-level checkpoint granularity | Dirty small table still serializes table-level state | Small updates pay too much checkpoint cost. |
| Index checkpoint granularity | Index metadata/catalog can be rewritten broadly | Indexed dirty workloads pay extra sync/write cost. |
| Directory sync accounting | Not yet separately instrumented in Rust checkpoint profile | Need deeper Rust-side spans for exact file-system breakdown. |

## Next Fix Direction

Do not optimize by disabling fsync. The next valid optimization path is:

1. Group commit / WAL batching with explicit durability policy.
2. Page/segment-level dirty tracking to avoid full table rewrite for tiny updates.
3. Incremental index checkpointing or index delta persistence.
4. Reduce compatibility marker and manifest rewrite frequency when contents are unchanged.
5. Rust-side checkpoint profile struct with bytes written, table count, index bytes, fsync count, rename time, and lock wait time.
6. Async/background checkpoint only after crash/recovery ordering is specified and tested.

## Validation

Commands run:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml
python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels
python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
python3 -m pytest -q -rxX
bash scripts/check_no_space_number_duplicates.sh
python3 -m py_compile scripts/compare_postgres_native_sql.py scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/vector_search_audit_benchmark.py
```

Results:

- `cargo fmt`: passed.
- `cargo check --no-default-features`: passed.
- `cargo check`: passed.
- `cargo test --no-default-features`: lib tests `412 passed, 15 ignored`; integration and crash/recovery targets passed.
- default `cargo test` with PyO3 link flags: lib tests `412 passed, 15 ignored`; integration and crash/recovery targets passed.
- maturin release wheel build/install: passed.
- `python3 -m pytest -q -rxX`: `1131 passed, 12 skipped`.
- duplicate hygiene check: passed.
- script py_compile: passed.
- memory PostgreSQL comparison: passed.
- persistent WAL PostgreSQL comparison: passed.
- perf investigation: passed.

## Final Decision

Durable PostgreSQL-equivalent performance claim is not justified.

Supported statement:

- NativeSqlEngine memory/autocommit scalar path beats PostgreSQL on measured local workloads.
- Persistent WAL mode passes reopen durability sanity checks.
- Persistent WAL read/index/count paths beat PostgreSQL in this run.
- Persistent WAL mutating durability path is currently slower than PostgreSQL due to explicit WAL sync and synchronous checkpoint/table-level persistence costs.
