# Persistent WAL Checkpoint Profile - 2026-06-02

## Summary

This pass added Rust-side checkpoint profiling and separated persistent WAL append, WAL sync, and checkpoint costs in the benchmark output.

Key result: persistent read/index/count workloads still beat PostgreSQL locally, but durable mutating workloads do not. The losing path is now isolated: explicit per-mutation WAL sync costs about 3 ms p50, and dirty checkpoint costs about 8-9 ms p50 with the remaining time dominated by file sync.

## Code Paths Changed

| File | Change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | Added `CheckpointProfile`, `NativeSqlEngine.checkpoint_profile()`, checkpoint byte/timing/fsync counters, no-op profile output, manifest/marker rewrite elision, `sync_data()` for checkpoint temp files, and shorter table-lock scope by serializing under lock then writing/fsyncing after lock release. |
| `scripts/perf_investigate_native_sql.py` | Added checkpoint profile capture on WAL/checkpoint workloads and split `persistent.insert_wal_append_only` from `persistent.insert_sync_wal_only`. |
| `docs/native_sql_perf_investigation_last.json` | Refreshed full perf investigation with checkpoint profile breakdown. |
| `docs/postgres_comparison_memory_latest.json` | Refreshed strict PostgreSQL comparison for non-durable QM memory mode. |
| `docs/postgres_comparison_persistent_wal_latest.json` | Refreshed strict PostgreSQL comparison for persistent QM WAL mode. |
| `docs/profiles/native_sql_core_bench_profile_summary.txt` | Refreshed Rust profile wrapper summary. |
| `docs/profiles/native_sql_core_bench_timing_summary.json` | Refreshed Rust profile wrapper timing JSON. |

## Checkpoint Profile Results

Command:

```bash
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
```

| Workload | p50 ms | p95 ms | Rust profile total ms | sync ms | lock-held ms | table bytes | fsync count |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `checkpoint.no_dirty_tables` | 0.001959 | 0.002250 | 0.001750 | 0.000000 | 0.000000 | 0 | 0 |
| `checkpoint.dirty_small_table` | 8.099208 | 9.674583 | 9.510208 | 6.186668 | 0.923917 | 104014 | 2 |
| `wal.commit_with_checkpoint_pressure` | 9.084417 | 10.960333 | 7.809125 | 6.003334 | 0.439417 | 174364 | 2 |
| `checkpoint.dirty_large_table` | 8.139542 | 9.938167 | 6.074125 | 4.364833 | 0.391709 | 211214 | 2 |
| `wal.bulk_insert_1000_then_checkpoint` | 24.857209 | 24.857209 | 9.350167 | 7.445374 | 0.956875 | 278214 | 2 |

Interpretation:

- Clean checkpoint remains effectively free and performs no fsync.
- The checkpoint lock-scope fix reduced lock-held time to sub-ms for dirty checkpoint cases.
- Remaining dirty checkpoint latency is mostly `sync_data()`/fsync-like I/O, not table lock contention.
- Checkpoint is still table-level. A one-row dirty table still rewrites the table snapshot file, so page/segment checkpointing remains the next real ceiling.

## WAL Cost Split

| Workload | p50 ms | p95 ms | Throughput ops/s |
| --- | ---: | ---: | ---: |
| `persistent.insert_wal_append_only` | 0.006458 | 0.007375 | 147199.53 |
| `persistent.insert_sync_wal_only` | 2.990917 | 3.556250 | 359.79 |
| `wal.commit_latency` | 0.019917 | 0.035042 | 44259.21 |

Interpretation:

- WAL append itself is not the bottleneck.
- Per-mutation durable sync is the bottleneck for insert/update/delete/commit in persistent mode.
- The current strict comparison intentionally syncs after each timed mutation, so the PostgreSQL losses are real for that policy.

## PostgreSQL Comparison

PostgreSQL strict comparisons ran successfully with:

- PostgreSQL 17.9 Homebrew
- `fsync=on`
- `synchronous_commit=on`
- DSN sanitized in JSON

Memory-mode command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode memory --output docs/postgres_comparison_memory_latest.json --strict
```

Persistent-WAL command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --output docs/postgres_comparison_persistent_wal_latest.json --strict
```

### Memory Mode

Memory mode is not durability-equivalent. It is valid only for local in-memory/autocommit claims.

| Workload | Winner | QM p50 ms | PG p50 ms | QM ops ratio |
| --- | --- | ---: | ---: | ---: |
| insert | QM | 0.004375 | 0.087750 | 20.466 |
| select_by_pk | QM | 0.003125 | 0.036333 | 11.577 |
| update_by_pk | QM | 0.002583 | 0.084583 | 33.929 |
| delete_by_pk | QM | 0.007334 | 0.183458 | 24.975 |
| indexed_integer_equality | QM | 0.004292 | 0.041000 | 9.437 |
| indexed_string_equality_duplicate_heavy | QM | 0.016000 | 0.051292 | 3.029 |
| count_indexed_equality | QM | 0.003209 | 0.042083 | 12.760 |
| predicate_range | QM | 0.004542 | 0.047750 | 9.321 |
| transaction_commit | QM | 0.003708 | 0.131291 | 35.126 |
| transaction_rollback | QM | 0.005292 | 0.083958 | 15.675 |
| indexed_string_equality_unique | QM | 0.003833 | 0.039541 | 9.933 |

### Persistent WAL Mode

Persistent WAL mode uses a real data directory, explicit `sync_wal()` after each timed mutation, and reopen durability sanity checks. `reopen_validation_passed=true`.

| Workload | Winner | QM p50 ms | PG p50 ms | QM ops ratio |
| --- | --- | ---: | ---: | ---: |
| insert | PostgreSQL | 2.991125 | 0.095708 | 0.038 |
| select_by_pk | QM | 0.005167 | 0.036375 | 6.838 |
| update_by_pk | PostgreSQL | 3.000167 | 0.092958 | 0.032 |
| delete_by_pk | PostgreSQL | 2.996125 | 0.179292 | 0.067 |
| indexed_integer_equality | QM | 0.013333 | 0.039583 | 3.162 |
| indexed_string_equality_duplicate_heavy | QM | 0.027750 | 0.051541 | 1.501 |
| count_indexed_equality | QM | 0.004542 | 0.042000 | 8.985 |
| predicate_range | QM | 0.005750 | 0.049291 | 6.276 |
| transaction_commit | PostgreSQL | 2.989958 | 0.138292 | 0.053 |
| transaction_rollback | QM | 0.019416 | 0.082667 | 3.740 |
| indexed_string_equality_unique | QM | 0.008917 | 0.037583 | 3.584 |

## Release Benchmark

Command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

Selected results:

| Workload | p50 ms | p95 ms | Throughput ops/s |
| --- | ---: | ---: | ---: |
| `native_sql.simple_insert` | 0.003375 | 0.003792 | 279906.23 |
| `native_sql.simple_select` | 0.003083 | 0.003125 | 317577.98 |
| `native_sql.simple_update` | 0.002542 | 0.002625 | 381376.19 |
| `native_sql.simple_delete` | 0.005333 | 0.005625 | 183665.93 |
| `native_sql.mvcc_read_write` | 0.006667 | 0.007167 | 145102.80 |
| `native_sql.vector_cache_hot_path` | 0.004000 | 0.004042 | 246827.29 |

## Validation

Commands run and results:

| Command | Result |
| --- | --- |
| `cargo fmt --manifest-path qm_engine/Cargo.toml` | passed |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | passed |
| `cargo check --manifest-path qm_engine/Cargo.toml` | passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | passed; lib `412 passed, 15 ignored`; integration/crash/doc tests passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery` | passed; `7 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery` | passed; `7 passed` |
| `RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml` | passed; lib `412 passed, 15 ignored`; integration/crash/doc tests passed |
| `python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels` | passed |
| `python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl` | passed |
| `python3 -m pytest -q -rxX` | passed; `1131 passed, 12 skipped` |
| `bash scripts/check_no_space_number_duplicates.sh` | passed |
| `python3 -m py_compile scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/compare_postgres_native_sql.py` | passed |
| `python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default` | passed |
| `python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default` | passed |
| `ITERATIONS=1000 OUT_DIR=docs/profiles scripts/profile_native_sql_rust.sh` | passed |
| memory-mode PostgreSQL strict comparison | passed |
| persistent-WAL PostgreSQL strict comparison | passed |

`docs/postgres_comparison_latest.json` note: this path was blocked by macOS/sandbox `PermissionError: Operation not permitted` even after removing the old generated file. The valid comparison artifacts for this pass are `docs/postgres_comparison_memory_latest.json` and `docs/postgres_comparison_persistent_wal_latest.json`.

## Approved Claims

- QM beats PostgreSQL on the measured local in-memory/autocommit scalar workloads.
- QM persistent-WAL read/index/count workloads beat PostgreSQL in this local strict run.
- QM persistent-WAL mutating durability path does not beat PostgreSQL under per-mutation `sync_wal()`.
- Clean checkpoint no-op is effectively eliminated.
- Dirty checkpoint lock scope is no longer the main bottleneck; lock-held time is sub-ms after serializing under lock and performing file I/O after releasing the table read lock.
- Remaining dirty checkpoint latency is dominated by file sync and table-level snapshot rewrite.

## Rejected Claims

- Do not claim QM durable insert/update/delete/commit beats PostgreSQL.
- Do not claim page-level or segment-level checkpoint exists; current checkpoint is still table-level.
- Do not claim PostgreSQL-equivalent durability throughput. Persistent-WAL per-mutation sync loses by a large margin on mutating workloads.
- Do not claim vector/prepared/gateway results represent durable write performance.

## Remaining Bottlenecks

| Area | Status | Why it matters |
| --- | --- | --- |
| Per-mutation WAL sync | Still about 3 ms p50 | Direct cause of PostgreSQL losses on durable insert/update/delete/commit. |
| Table-level dirty checkpoint | Still rewrites whole dirty table file | Small dirty updates pay too much persistence cost. |
| File sync cost | 4-7 ms inside dirty checkpoint profile | Dominates dirty checkpoint latency after lock-scope fix. |
| Segment/page checkpoint | Not implemented | Needed to reduce dirty small update checkpoint bytes and sync pressure. |
| `postgres_comparison_latest.json` path | OS/sandbox blocks writes | Use memory/persistent-specific JSON outputs as source of truth. |

## Next Targets

1. Add explicit WAL sync policy modes: per-commit, group-commit, batch interval, and document claim scope for each.
2. Implement segment/page-level checkpoint with crash-safe manifest ordering.
3. Add directory fsync accounting and, if needed, explicit directory fsync in durable modes.
4. Add checkpoint write coalescing and table snapshot delta encoding.
5. Continue materialization work for duplicate-heavy result paths, but do not conflate it with durable write bottlenecks.

