# Full Performance Investigation - 2026-05-18

Updated on 2026-05-19 after the extreme hot-path optimization pass.

## Executive Summary

Final status: **Ready with performance caveats**.

Correctness, crash/recovery, pytest, duplicate hygiene, and release benchmark gates are green. The pass found a real insert-path O(n^2) implementation bug in vector dimension validation and fixed it instead of treating slow batch insert/delete results as architectural. The same pass also reduced MVCC read-only overhead, reduced insert stats/index bookkeeping overhead, and optimized literal-address gateway binding.

The main performance caveats left are scoped and documented: OR predicate scans, string secondary-index equality, synchronous checkpoint pressure, and gateway full lifecycle overhead. None blocks the current NativeSqlEngine release claim, but none should be described as solved.

## PostgreSQL-Class Target

The supported NativeSqlEngine architecture target remains PostgreSQL-class performance or better for comparable supported workloads. After this pass, the local PostgreSQL comparison script was run, but PostgreSQL comparison did not execute because the local server rejected the default DSN:

```text
role "postgres" does not exist
```

Therefore this report does not reuse the older PostgreSQL table as current evidence. Current release readiness is based on QM release-build benchmarks, regression tests, and the fact that the previously severe simple DML bottlenecks were traced to code inefficiencies and fixed.

## Methodology

- Build mode: release wheel built with `maturin build --release`
- Feature flags: default
- Durability mode: NativeSqlEngine default WAL/checkpoint behavior for persistent engines; in-memory for non-persistent microbenchmarks
- Dataset size: 10,000 rows for the full investigation script
- Iterations: 1,000 for normal latency loops, reduced where the workload itself is large
- Warmup: 10 warmup iterations for latency loops unless overridden; one warmup for large batch loops
- Cache policy: warm-cache unless workload name says coldish/recovery
- Environment: macOS-26.5 arm64, 8 CPU count, 16 GiB RAM, Python 3.13.3, rustc 1.94.0

## QM Benchmark Summary

Source: `docs/native_sql_perf_investigation_last.json`

| Workload | p50 ms | p95 ms | p99 ms | Throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| single.insert_one | 0.0035 | 0.0045 | 0.0062 | 267242.70 |
| single.select_by_pk | 0.0030 | 0.0031 | 0.0063 | 307546.39 |
| single.update_by_pk | 0.0025 | 0.0027 | 0.0034 | 383895.58 |
| single.delete_by_pk | 0.0052 | 0.0054 | 0.0055 | 188533.97 |
| batch.insert_100 | 0.3270 | 0.4676 | 0.6055 | 2873.07 batches/s |
| batch.insert_1000 | 3.2938 | 3.7384 | 3.9450 | 295.57 batches/s |
| batch.insert_10000 | 34.3215 | 39.7327 | 39.7327 | 28.28 batches/s |
| batch.delete_500_by_pk | 3.1037 | 6.6577 | 11.9556 | 242.47 batches/s |
| predicate.or_scan | 0.7861 | 0.9535 | 1.3030 | 1224.24 |
| mvcc.read_only_transaction | 0.0036 | 0.0041 | 0.0047 | 263440.96 |
| mvcc.write_transaction | 0.1264 | 0.1475 | 0.1649 | 7875.23 |
| mvcc.concurrent_writer_readers | 0.7846 | 1.0100 | 1.1374 | 1261.41 |
| index.insert_with_secondary | 0.0047 | 0.0055 | 0.0060 | 205786.00 |
| index.delete_indexed_row | 0.0069 | 0.0071 | 0.0072 | 143667.84 |
| wal.bulk_insert_1000_then_checkpoint | 21.7920 | 21.7920 | 21.7920 | 45.89 batches/s |
| gateway.simple_query_pgwire | 0.0296 | 0.0676 | 0.0779 | 25962.53 |
| gateway.startup_shutdown_callback | 0.3382 | 0.4386 | 0.4427 | 2884.59 |
| vector.insert | 0.0042 | 0.0045 | 0.0055 | 233624.39 |

Peak RSS for the full investigation run: 46.63 MB.

## Release Baseline Summary

Source: `docs/native_sql_benchmark_baseline.json`

| Workload | p50 ms | p95 ms | p99 ms | Throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.0033 | 0.0038 | 0.0056 | 278862.24 |
| native_sql.simple_select | 0.0028 | 0.0030 | 0.0031 | 343736.16 |
| native_sql.simple_update | 0.0025 | 0.0026 | 0.0027 | 392631.72 |
| native_sql.simple_delete | 0.0051 | 0.0053 | 0.0054 | 192249.13 |
| native_sql.predicate_path | 0.0023 | 0.0025 | 0.0080 | 379356.77 |
| native_sql.mvcc_read_write | 0.0326 | 0.0563 | 0.0702 | 29606.53 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 238547.55 |
| native_sql.vector_cache_hot_path | 0.0040 | 0.0042 | 0.0055 | 236910.69 |
| python_gateway.startup_shutdown | 0.2652 | 0.5115 | 0.7172 | 3425.32 |

## Fixes Implemented

- Fixed B+Tree duplicate-key lookup/delete across split leaves in `qm_engine/src/index/bplus_tree.rs`.
- Added `WHERE id = <integer>` fast paths for `UPDATE` and `DELETE` row targeting without bypassing FK, constraint, index, RETURNING, or cache logic.
- Optimized primary-key UNIQUE enforcement on insert by using `table.rows.contains_key()` for the `id` column.
- Fixed the insert O(n^2) bug where `validate_insert_vector_dimensions` scanned existing rows for every column on every insert, even when incoming rows contained no vectors.
- Added batched `IndexManager::record_writes` and `record_numeric_values` APIs and used them from insert.
- Avoided insert row cloning for the no-index/no-returning/no-conflict fast path.
- Made transaction rollback snapshots lazy; read-only transactions no longer clone table/tombstone/index state.
- Optimized gateway listener binding by parsing literal socket addresses before falling back to DNS lookup.

## Before/After Evidence

| Metric | Before | After |
| --- | ---: | ---: |
| targeted 10k simple inserts | ~4390 ms | ~34 ms |
| single.insert_one p50 | 0.0489 ms in the first 2026-05-19 run | 0.0035 ms |
| single.delete_by_pk p50 | 0.0924 ms in the first 2026-05-19 run | 0.0052 ms |
| batch.insert_10000 p50 | 4508.50 ms | 34.3215 ms |
| batch.delete_500_by_pk p50 | 843.99 ms | 3.1037 ms |
| mvcc.read_only_transaction p50 | 0.0039 ms | 0.0036 ms |
| mvcc.write_transaction p50 | 0.1705 ms | 0.1264 ms |
| mvcc.concurrent_writer_readers p50 | 1.7629 ms | 0.7846 ms |
| vector.insert p50 | 0.0425 ms | 0.0042 ms |
| release native_sql.simple_insert throughput | 14506.66 ops/s old baseline | 278862.24 ops/s |
| release native_sql.simple_delete throughput | 9835.95 ops/s old baseline | 192249.13 ops/s |

## Remaining Bottlenecks

| Bottleneck | Classification | Blocking? |
| --- | --- | --- |
| OR predicate per-row recursive evaluation | Codebase inefficiency; parsed predicate AST is not reused | No, documented caveat |
| Indexed string equality overhead | Codebase inefficiency in string key conversion/comparison or duplicate-key path | No, documented caveat |
| WAL/checkpoint pressure | Real synchronous persistence cost; still worth optimizing | No for current scoped release, but do not overclaim durability-equivalent throughput |
| Gateway full lifecycle overhead | Runtime/listener/task startup cost | No, gateway steady query path remains healthy |

## Validation

```text
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
PASS: finished in 10.88s after the vector-validation patch

cargo check --manifest-path qm_engine/Cargo.toml
PASS: finished in 2.99s

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
PASS: 400 passed, 15 ignored; 35 bench_new_components tests passed; 23 MVCC integration tests passed; 7 crash-kill tests passed; 7 crash-recovery tests passed; doc tests ok with 2 ignored

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
PASS: 7 passed

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
PASS: 7 passed

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features vector_
PASS: 24 vector-related tests passed across unit/integration targets

python3 -m pytest -q -rxX
PASS: 1113 passed, 12 skipped, 6 xfailed

bash scripts/check_no_space_number_duplicates.sh
PASS

python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
PASS

python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
PASS: full run completed in 4.03s

python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_2026_05_18.json
PASS as a script, PostgreSQL comparison skipped: local role `postgres` does not exist
```

Remaining xfails are unchanged and documented as non-blocking for this NativeSqlEngine release scope: HubEngine SQL passthrough, pgwire float BETWEEN, and VectorQuantizer accounting.

## Final Decision

**Ready with performance caveats**.

The severe insert/delete bottlenecks were code bugs and were fixed. The remaining slower paths are concrete engineering targets, not architecture excuses, and are scoped outside the current release-blocking NativeSqlEngine simple-DML gate.
