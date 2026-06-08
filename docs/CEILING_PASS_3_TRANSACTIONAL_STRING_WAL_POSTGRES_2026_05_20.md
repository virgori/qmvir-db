# NativeSqlEngine / QMvir Ceiling Pass 3 - Transactional Prepared, String Index, WAL, PostgreSQL

Date: 2026-05-20

## Summary

Status: **Ready with performance caveats, not PostgreSQL-claim ready**.

This pass moved prepared execution into persistent autocommit and explicit transaction paths without falling back to SQL parsing for execution. It also added borrowed B+Tree string lookup, materialization/projection decomposition, WAL/checkpoint decomposition, Rust profiling fallback support, and strict PostgreSQL comparison behavior.

No PostgreSQL superiority claim is approved. Strict PostgreSQL comparison was attempted and failed because the configured local role `postgres` does not exist.

## Methodology

- Build mode: release for benchmark artifacts.
- Feature flags: default for Python-facing benchmarks; `--no-default-features` for direct Rust core harness.
- Dataset: 10,000 rows for the full investigation unless workload-specific.
- Iterations: 1,000 unless reduced for expensive checkpoint/gateway loops.
- Cache: warm-cache loops unless workload name says recovery/coldish.
- Durability: in-memory for non-persistent engines; NativeSqlEngine default WAL/checkpoint behavior for persistent engines.
- PostgreSQL mode attempted: `wal_fsync`, strict.

Environment from `docs/native_sql_perf_investigation_last.json`:

| item | value |
| --- | --- |
| OS | macOS-26.5-arm64-arm-64bit-Mach-O |
| CPU count | 8 |
| RAM | 16.00 GiB |
| Python | 3.13.3 |
| Rust | rustc 1.94.0 (4a4ef493e 2026-03-02) |
| Peak RSS | 46.921875 MB |

## Code Paths Changed

| file | change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | Prepared DML now routes through transaction snapshot/WAL bookkeeping for persistent autocommit and active transactions. Added WAL SQL rendering for prepared DML. Added regression tests for persistent restart, transaction rollback, index preservation, and FK violation parity. |
| `qm_engine/src/index/bplus_tree.rs` | Added `IndexLookupKeyRef` and `search_ref` borrowed lookup API. Existing owned `search` now delegates to borrowed lookup. Added unicode/empty/duplicate borrowed lookup tests. |
| `qm_engine/src/index/mod.rs` | Re-exported `IndexLookupKeyRef`. |
| `qm_engine/src/bin/native_sql_core_bench.rs` | Added borrowed string lookup direct-core benchmarks. |
| `scripts/perf_investigate_native_sql.py` | Added prepared persistent/transaction benchmarks, materialization/projection breakdown, borrowed string lookup reporting, and WAL/checkpoint microbenchmarks. |
| `scripts/compare_postgres_native_sql.py` | Added `--strict`, `--durability-mode`, sanitized DSN output, PostgreSQL settings capture, and more matched workloads. |
| `scripts/profile_native_sql_rust.sh` | Added Rust profiling helper with flamegraph/samply/instruments detection and timing-summary fallback. |
| `docs/postgres_benchmark_setup.md` | Added local PostgreSQL setup and DSN instructions. |

## Prepared Transactional / Persistent Status

Prepared DML no longer rejects persistent engines or active transactions. It still uses the prepared plan for execution, while rendering SQL only for the existing SQL-text WAL replay format.

Safety behavior:

- Persistent autocommit: append WAL before prepared DML mutation, then execute prepared primitive.
- Active transaction: materialize rollback snapshot, execute prepared primitive, record WAL SQL only after success.
- Rollback: existing table/tombstone/index snapshot restore path is reused.
- FK/constraint/index/cache paths remain active.

New tests:

- `prepared_persistent_dml_survives_restart_through_wal`
- `prepared_transaction_rollback_restores_insert_update_delete_and_indexes`
- `prepared_fk_violation_matches_sql_path`

Key results:

| workload | p50 ms | p95 ms | p99 ms | ops/s | interpretation |
| --- | ---: | ---: | ---: | ---: | --- |
| prepared.select_by_pk | 0.000917 | 0.000959 | 0.001000 | 996926 | prepared in-memory read path remains fast |
| prepared.update_by_pk | 0.001416 | 0.001459 | 0.001542 | 678637 | prepared in-memory update remains faster than SQL path |
| prepared.indexed_string_equality | 0.017125 | 0.018000 | 0.022583 | 57687 | faster than SQL string equality, still materialization-bound |
| prepared.persistent_autocommit_insert | 0.005208 | 0.007166 | 0.013750 | 176401 | now WAL-safe; slower than in-memory prepared insert as expected |
| prepared.persistent_reopen_count | 2.280916 | 2.280916 | 2.280916 | 438 | restart/recovery smoke for prepared persistent path |
| prepared.transaction_rollback_roundtrip | 0.006208 | 0.006500 | 0.008833 | 155966 | prepared active transaction rollback path now covered |

Remaining limitation: prepared vector exact search is still not implemented.

## String Index Ceiling Status

Borrowed lookup exists and is correctness-tested, but this run does not prove a major speedup. It removes an allocation-capability blocker; the remaining unique string lookup gap is mostly compare/tree path and surrounding executor materialization.

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| core.index_lookup_numeric_direct | 0.000291 | 0.000334 | 0.000375 | 2942977 |
| core.index_lookup_string_direct | 0.000583 | 0.000584 | 0.000667 | 1575113 |
| core.index_lookup_string_borrowed_direct | 0.000583 | 0.000584 | 0.000709 | 1496538 |
| string_index.lookup_borrowed_key | 0.000542 | 0.000584 | 0.000666 | 1640689 |
| string_index.lookup_duplicate_heavy | 0.010917 | 0.012375 | 0.015625 | 86177 |
| string_index.compare_numeric_baseline | 0.000250 | 0.000250 | 0.000250 | 3564529 |

Interpretation:

- Borrowed lookup is semantically correct and avoids requiring an owned lookup key.
- Unique string lookup remains about 2x numeric direct lookup in this run.
- Duplicate-heavy lookup remains orders slower because it returns many row IDs.
- Interned/symbol string keys were not implemented.

## Materialization / Projection Status

Materialization is now separated from lookup.

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| materialization.count_no_projection_duplicate_heavy | 0.006250 | 0.006334 | 0.008417 | 156581 |
| materialization.row_id_only_duplicate_heavy | 0.042833 | 0.057084 | 0.088542 | 21542 |
| materialization.one_col_projection_duplicate_heavy | 0.046625 | 0.064375 | 0.092542 | 19895 |
| materialization.full_row_projection_duplicate_heavy | 0.133708 | 0.154042 | 0.166167 | 7303 |

Root cause: duplicate-heavy result materialization dominates over tree seek. COUNT avoids most projection work and is much faster.

Next optimization target: late materialization, direct row-id count path, and narrow projection without full row/result cloning.

## WAL / Checkpoint Decomposition

WAL append itself is not the bottleneck. Checkpoint is.

| workload | p50 ms | p95 ms | p99 ms | ops/s | interpretation |
| --- | ---: | ---: | ---: | ---: | --- |
| wal.record_construction | 0.000125 | 0.000125 | 0.000166 | 5699369 | negligible |
| wal.file_write | 0.001459 | 0.002125 | 0.006750 | 541260 | small |
| wal.file_fsync | 0.000542 | 0.000583 | 0.000625 | 1548346 | this microbench is not the bottleneck on this filesystem/run |
| wal.commit_latency | 0.049792 | 0.081625 | 0.120917 | 18825 | ordinary WAL transaction cost |
| checkpoint.no_dirty_tables | 148.343417 | 266.346834 | 403.180417 | 7.41 | severe bug/inefficiency: clean checkpoint still expensive |
| checkpoint.dirty_small_table | 334.327417 | 660.976667 | 796.116042 | 3.03 | severe |
| wal.commit_with_checkpoint_pressure | 341.888458 | 861.753917 | 1084.255167 | 2.64 | severe |
| checkpoint.dirty_large_table | 426.527250 | 784.262916 | 784.262916 | 2.16 | severe |
| recovery.load_from_checkpoint | 3.449458 | 3.449458 | 3.449458 | 289.90 | recovery load is not the main issue |

This is a real bottleneck, not architecture-proof. The next pass should focus on dirty-table/page tracking, no-op checkpoint elision, avoiding full table snapshot clone/serialization when clean, and incremental checkpoint output.

## Rust Profiler Evidence

Command:

```bash
ITERATIONS=1000 OUT_DIR=docs/profiles scripts/profile_native_sql_rust.sh
```

Result:

- `docs/profiles/native_sql_core_bench_timing_summary.json`
- `docs/profiles/native_sql_core_bench_profile_summary.txt`

No `cargo-flamegraph`, `samply`, or usable Instruments CLI profiler was found, so the script produced timing-summary fallback evidence. This is useful but not a symbol-level flamegraph.

## PostgreSQL Comparison Status

Command:

```bash
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json --strict
```

Result: failed as intended in strict mode.

```text
PostgreSQL connection failed: connection to server on socket "/tmp/.s.PGSQL.5432" failed: FATAL: role "postgres" does not exist
```

The script now writes:

- sanitized DSN
- durability mode
- PostgreSQL availability
- skip reason
- psql version
- QM-only results

No PostgreSQL performance claim is approved.

Setup instructions were added in `docs/postgres_benchmark_setup.md`.

## Release Benchmark

Command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

Result: passed.

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.003500 | 0.004459 | 0.006541 | 261983 |
| native_sql.simple_select | 0.002917 | 0.003125 | 0.003875 | 326340 |
| native_sql.simple_update | 0.002666 | 0.002750 | 0.005584 | 357170 |
| native_sql.simple_delete | 0.005250 | 0.005500 | 0.005792 | 185845 |
| native_sql.predicate_path | 0.038708 | 0.050125 | 0.078959 | 23019 |
| native_sql.mvcc_read_write | 0.032250 | 0.060500 | 0.132083 | 26785 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 195205 |
| native_sql.vector_cache_hot_path | 0.004166 | 0.005625 | 0.006791 | 210128 |
| python_gateway.startup_shutdown | 0.251750 | 0.728167 | 0.956959 | 2889 |

## Correctness Risks

| risk | status |
| --- | --- |
| Prepared path bypassing WAL in persistent mode | Fixed for supported prepared DML by WAL SQL rendering plus existing WAL replay format. |
| Prepared path bypassing rollback snapshot in active transaction | Fixed for supported prepared DML by calling `ensure_transaction_snapshot`. |
| Prepared path skipping FK/constraints/index/cache | Covered by new tests and reused checks. |
| Borrowed string lookup equality drift | Covered for empty string, unicode, and duplicate-heavy values. |
| Checkpoint cost | Still severe and performance-blocking for durability-heavy claims. |
| PostgreSQL comparison | Still unavailable; no PG claim. |

## Validation

Passed:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml
python3 -m pytest -q -rxX
bash scripts/check_no_space_number_duplicates.sh
python3 -m py_compile scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/compare_postgres_native_sql.py
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
ITERATIONS=1000 OUT_DIR=docs/profiles scripts/profile_native_sql_rust.sh
```

Observed results:

- Rust no-default: 406 passed, 15 ignored; integration suites passed.
- Crash recovery: 7 passed.
- Crash-kill recovery: 7 passed.
- Rust default with PyO3 flags: 406 passed, 15 ignored; integration suites passed.
- Pytest: 1113 passed, 12 skipped, 6 xfailed.
- Duplicate hygiene: passed.
- Release benchmark: passed.
- Full performance investigation: passed.

Failed by environment:

```bash
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json --strict
```

Reason: local PostgreSQL role `postgres` does not exist.

## Approved Claims

- Supported prepared DML now executes in persistent autocommit and active transaction modes with WAL/rollback bookkeeping.
- Borrowed B+Tree string lookup exists and is tested.
- Duplicate-heavy string equality is materialization/cardinality-bound more than key-construction-bound.
- COUNT duplicate-heavy path is much faster than materialized row projection.
- WAL append/write is not the main bottleneck in this run; checkpoint is.
- Strict PostgreSQL comparison now fails loudly instead of silently allowing claims.

## Rejected Claims

- QMvir beats PostgreSQL.
- String index has reached numeric index ceiling.
- WAL/checkpoint performance is acceptable for durability-heavy PostgreSQL-equivalent claims.
- Prepared path covers every SQL form.
- Rust profiler has symbol-level flamegraph evidence in this environment.

## Next Targets

1. Fix checkpoint no-op path: clean checkpoint must not take ~148 ms p50.
2. Add dirty table/page tracking and incremental checkpoint serialization.
3. Optimize materialization: row-id only, narrow projection, direct scalar count, and late row fetch.
4. Add interned/symbol string key option and compare memory cost.
5. Add true Rust flamegraph/symbol profiler on a machine with `cargo-flamegraph`, `samply`, Instruments, or Linux `perf`.
6. Configure a valid `POSTGRES_DSN` and rerun strict comparison before any PostgreSQL claim.
