# Ceiling Pass 5: Materialization Ceiling + Segment Checkpoint

Date: 2026-05-20

## 1. Summary

This pass focused on duplicate-heavy indexed result materialization and on keeping the checkpoint claims honest after Pass 4.

Implemented:

- Added specialized indexed projection materialization for `id`, one-column, and full-row projections.
- Avoided row fetch for `SELECT id ... WHERE indexed_col = ?` when the row id is known and the row still exists.
- Avoided full-row materialization for one-column indexed projections.
- Added direct `Cell` to query-byte conversion to avoid the broader `as_text()` conversion path for common scalar types.
- Added duplicate-heavy materialization decomposition benchmarks for Python consumption of already-produced row-id, one-column, and full-row results.
- Improved PostgreSQL comparison ergonomics with `--postgres-dsn`, `--explain-env`, and `--setup-sql-output`.
- Re-ran full 1000-iteration release/perf benchmarks from the rebuilt local wheel.

Not implemented:

- Segment/page-level checkpoint is not implemented in this pass. The current checkpoint remains table-level dirty tracking with table-level checkpoint files.
- Internal Rust-only materialization timing is still not exposed through a crate-level benchmark hook.
- PostgreSQL strict comparison still did not run because the local default role `postgres` does not exist.

## 2. Root Cause Analysis

| bottleneck | finding | status |
| --- | --- | --- |
| duplicate-heavy row-id projection | Pass 4 still fetched/materialized through the generic projection path for indexed result rows; row-id-only output was paying avoidable row access and conversion overhead | Improved, but not 2x |
| duplicate-heavy one-column projection | the index seek is cheap; most latency is row fetch plus scalar result conversion for many duplicate rows | Slightly improved |
| duplicate-heavy full-row projection | unavoidable cardinality dominates; full-row projection still clones/converts every returned cell at the Python-visible result boundary | Still bottleneck |
| prepared indexed string equality | direct string lookup is microsecond/sub-microsecond, but prepared duplicate-heavy output is still materialization-bound | Still bottleneck |
| checkpoint dirty path | Pass 4 table-level tracking removed the severe full-snapshot bug; remaining 11-14 ms cost is table-level serialization/fsync, not WAL record construction | Still table-level |
| PostgreSQL comparison | script was usable only through default role/DSN behavior; strict benchmark still requires a valid local benchmark role/database | Tooling improved, comparison unavailable |

Profiling evidence:

- `scripts/profile_native_sql_rust.sh` found no flamegraph/samply/Instruments profiler and wrote timing fallback files under `docs/profiles/`.
- The timing fallback shows direct index lookup is not the dominant duplicate-heavy cost:
  - `core.index_lookup_numeric_direct` p50 `0.000167 ms`
  - `core.index_lookup_string_direct` p50 `0.000375 ms`
  - `core.index_lookup_string_borrowed_direct` p50 `0.000334 ms`
  - `string_index.lookup_duplicate_heavy` p50 `0.007667 ms`
- End-to-end duplicate-heavy materialization remains much higher than raw lookup:
  - `materialization.row_id_only_duplicate_heavy` p50 `0.033833 ms`
  - `materialization.one_col_projection_duplicate_heavy` p50 `0.039708 ms`
  - `materialization.full_row_projection_duplicate_heavy` p50 `0.113417 ms`

## 3. Changed Code Paths

| file | change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | added scalar `Cell` byte conversion, indexed projection row materialization helper, row-id-only fast path, one-column projection fast path, and routed prepared/SQL indexed equality result paths through the helper |
| `scripts/perf_investigate_native_sql.py` | added duplicate-heavy materialization decomposition benchmarks for consuming already-produced Python-visible results; documented skipped internal/projection-only timings |
| `scripts/compare_postgres_native_sql.py` | added `--postgres-dsn`, `--explain-env`, and `--setup-sql-output`; strict mode now reports a clearer DSN/setup failure |
| `docs/native_sql_benchmark_last.json` | updated 1000-iteration release benchmark output |
| `docs/native_sql_perf_investigation_last.json` | updated 1000-iteration full perf investigation output |
| `docs/postgres_comparison_latest.json` | updated strict PostgreSQL comparison attempt and failure reason |
| `docs/profiles/native_sql_core_bench_profile_summary.txt` | updated Rust profiling fallback summary |
| `docs/profiles/native_sql_core_bench_timing_summary.json` | updated Rust timing fallback output |

## 4. Materialization Before/After

Before values are from Pass 4. After values are from:

```bash
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
```

| workload | Pass 4 p50 ms | Pass 5 p50 ms | Pass 5 p95 ms | Pass 5 p99 ms | result |
| --- | ---: | ---: | ---: | ---: | --- |
| `materialization.count_no_projection_duplicate_heavy` | ~0.006292 | 0.006291 | 0.006334 | 0.006458 | unchanged; still fast |
| `materialization.row_id_only_duplicate_heavy` | ~0.035416 | 0.033833 | 0.043792 | 0.065000 | small improvement, target not met |
| `materialization.one_col_projection_duplicate_heavy` | ~0.042792 | 0.039708 | 0.050375 | 0.068417 | small improvement, target not met |
| `materialization.full_row_projection_duplicate_heavy` | ~0.121083 | 0.113417 | 0.140541 | 0.210334 | ~6% improvement, target not met |
| `prepared.indexed_string_equality` | ~0.015917 | 0.017416 | 0.018416 | 0.018959 | no stable improvement in full run |

Additional decomposition added in Pass 5:

| workload | p50 ms | p95 ms | p99 ms | interpretation |
| --- | ---: | ---: | ---: | --- |
| `materialization.python_consume_existing_row_id_duplicate_heavy` | 0.008917 | 0.009834 | 0.018333 | Python-side consumption of an already produced row-id result |
| `materialization.python_consume_existing_one_col_duplicate_heavy` | 0.009167 | 0.009792 | 0.019833 | Python-side consumption of an already produced one-column result |
| `materialization.python_consume_existing_full_row_duplicate_heavy` | 0.030542 | 0.035042 | 0.050583 | Python-side consumption of an already produced full-row result |
| `materialization.internal_execution_only_duplicate_heavy` | skipped | skipped | skipped | PyO3 API exposes only owned boundary results; needs Rust bench hook |
| `materialization.projection_only_duplicate_heavy` | skipped | skipped | skipped | projection-only timing is not separately exposed through `NativeSqlEngine::execute` |

Conclusion: the simple scalar conversion helper and row-id/narrow-projection fast paths help only marginally. The remaining cost is still the owned result batch/Python-visible materialization pipeline. The next real optimization needs an internal result batch representation or crate-level materialization benchmark hook, not more SQL-path string lookup tuning.

## 5. Checkpoint Before/After

Checkpoint remained stable after Pass 5; no segment/page-level implementation was added.

| workload | Pass 4 p50 ms | Pass 5 p50 ms | Pass 5 p95 ms | Pass 5 p99 ms | result |
| --- | ---: | ---: | ---: | ---: | --- |
| `checkpoint.no_dirty_tables` | 0.000125 | 0.000125 | 0.000250 | 0.000542 | clean no-op still elided |
| `checkpoint.dirty_small_table` | 10.932 | 10.923 | 12.959 | 13.005 | table-level cost stable |
| `checkpoint.dirty_large_table` | 13.501 | 12.040 | 14.121 | 14.121 | table-level cost stable |
| `wal.commit_with_checkpoint_pressure` | 12.801 | 13.116 | 17.919 | 19.527 | stable, still table-level checkpoint bound |
| `wal.bulk_insert_1000_then_checkpoint` | 25.597 | 24.344 | 24.344 | 24.344 | stable |
| `wal.update_delete_then_checkpoint` | not recorded | 14.266 | 14.266 | 14.266 | measured |

Segment/page-level checkpoint design status:

- Current manifest/table-file checkpoint format is a reasonable base for segment manifests, but this pass did not split table files into row-range/page segments.
- A correct segment implementation must add segment identity, per-segment dirty generations, atomic manifest replacement, checksums, and backward-compatible recovery from existing table-level checkpoint files.
- Until that exists, the approved claim remains table-level dirty checkpoint only.

## 6. Release Benchmark

Command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| `native_sql.simple_insert` | 0.003208 | 0.003875 | 0.005334 | 289107.83 |
| `native_sql.simple_select` | 0.002916 | 0.003208 | 0.014792 | 300259.00 |
| `native_sql.simple_update` | 0.002417 | 0.002708 | 0.003333 | 390332.86 |
| `native_sql.simple_delete` | 0.005167 | 0.007083 | 0.007500 | 182091.32 |
| `native_sql.predicate_path` | 0.038500 | 0.050125 | 0.089708 | 24647.84 |
| `native_sql.mvcc_read_write` | 0.033083 | 0.055541 | 0.080541 | 29208.10 |
| `native_sql.concurrent_read_write_smoke` | n/a | n/a | n/a | 209337.91 |
| `native_sql.vector_cache_hot_path` | 0.003959 | 0.004041 | 0.004084 | 246629.44 |
| `python_gateway.startup_shutdown` | 0.258833 | 0.357458 | 0.368291 | 3778.72 |

## 7. PostgreSQL Comparison Status

Command:

```bash
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json --strict
```

Result: failed by environment.

```text
strict PostgreSQL comparison unavailable: PostgreSQL connection failed: connection to server on socket "/tmp/.s.PGSQL.5432" failed: FATAL: role "postgres" does not exist
```

The script wrote `docs/postgres_comparison_latest.json` with `postgresql_available=false` and kept NativeSqlEngine local measurements for reference only. No PostgreSQL performance claim is approved.

New setup helpers:

```bash
python3 scripts/compare_postgres_native_sql.py --explain-env
python3 scripts/compare_postgres_native_sql.py --setup-sql-output /tmp/qm_bench_setup.sql
POSTGRES_DSN="postgresql://qm_bench:change-me@localhost:5432/qm_bench" \
  python3 scripts/compare_postgres_native_sql.py --iterations 1000 --strict
```

## 8. Correctness Validation

Commands run and results:

| command | result |
| --- | --- |
| `cargo fmt --manifest-path qm_engine/Cargo.toml` | pass |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `cargo check --manifest-path qm_engine/Cargo.toml` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | pass; 410 passed, 15 ignored; integration/crash tests also passed in-suite |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery` | pass; 7 passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery` | pass; 7 passed |
| `RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml` | pass; 410 passed, 15 ignored |
| `python3 -m pytest -q -rxX` | pass outside sandbox; 1113 passed, 12 skipped, 6 xfailed |
| `bash scripts/check_no_space_number_duplicates.sh` | pass |
| `python3 -m py_compile scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/compare_postgres_native_sql.py` | pass |
| `python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels` | pass |
| `python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl` | pass |
| `python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default` | pass |
| `python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default` | pass |
| `ITERATIONS=1000 OUT_DIR=docs/profiles scripts/profile_native_sql_rust.sh` | pass outside sandbox; timing fallback only |
| `python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json --strict` | failed by missing PostgreSQL role/DSN |

Note: the sandboxed `pytest` run failed at collection with `Operation not permitted` when the gateway tried to bind/start. The same command passed outside the sandbox, so this is an environment permission issue, not a test failure.

## 9. Allocation / Clone Reduction Evidence

- Row-id-only indexed projection now emits the row id directly after an existence check instead of fetching and projecting the row.
- One-column indexed projection fetches only the requested `Cell` and converts that scalar directly.
- Common scalar `Cell` conversion now avoids the broader `as_text()` path for `Int`, `Float`, `Text`, `Bool`, `Timestamp`, `Json`, `Uuid`, `Bytes`, `Interval`, and `Date`.
- The full 1000-iteration benchmark shows only marginal end-to-end gains, which means most remaining cost is outside those specific clones: owned result batch construction and Python-visible conversion still dominate duplicate-heavy output.

## 10. Approved Claims

- Clean checkpoint no-op remains effectively free at p50 `0.000125 ms`.
- Dirty checkpoint remains table-level and stable at roughly `10.9-13.1 ms` p50 for the measured dirty/checkpoint-pressure cases.
- Indexed row-id and one-column projection paths now avoid some avoidable row/full-row materialization work.
- COUNT duplicate-heavy indexed path remains fast because it avoids row materialization.
- PostgreSQL comparison tooling now gives clearer DSN/setup guidance.
- Correctness and crash/recovery suites pass after the materialization changes.

## 11. Rejected Claims

- No claim that QMvir beats PostgreSQL: strict PostgreSQL comparison did not run.
- No claim that checkpoint is segment/page-level incremental.
- No claim that duplicate-heavy materialization is solved.
- No claim that prepared indexed string equality improved in a stable full run.
- No claim that the result pipeline is zero-copy; Python-visible results are still owned/materialized.
- No claim that string index equality has reached numeric-index ceiling.

## 12. Remaining Risks

- The materialization benchmark still cannot isolate internal Rust execution/projection from PyO3 result ownership.
- Full-row duplicate-heavy output remains cardinality/materialization-bound.
- Table-level checkpoint still serializes/fsyncs the dirty table broadly; single-row dirty updates do not yet checkpoint as single segments/pages.
- Rollback and recovery are covered by tests, but a future segment checkpoint needs new crash tests for partial segment writes and manifest swaps.
- PostgreSQL strict comparison requires owner/environment setup with a valid benchmark DSN.

## 13. Next Targets

- Add a Rust crate-level materialization benchmark hook that can time index seek, row-id scan, visibility, projection, and owned/Python conversion separately.
- Introduce an internal result batch representation with row-id-only, borrowed one-column, borrowed full-row, and late owned-conversion variants.
- Add reusable/arena-backed result buffers for duplicate-heavy output.
- Implement segment-level checkpoint manifest and per-segment dirty generations with backward-compatible table-level recovery.
- Add crash tests for partial segment write, partial manifest write, and WAL replay across mixed table-level/segment-level checkpoints.
- Run strict PostgreSQL comparison with a valid `POSTGRES_DSN`.
