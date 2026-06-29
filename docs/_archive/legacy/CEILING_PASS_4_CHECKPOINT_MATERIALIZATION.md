# Ceiling Pass 4: Checkpoint + Materialization

Date: 2026-05-20

## 1. Summary

This pass targeted the two largest remaining non-PostgreSQL blockers from Ceiling Pass 3: checkpoint cost and duplicate-heavy result materialization.

Implemented:

- Added persistent-engine dirty generation tracking.
- Added table-level checkpoint files plus a manifest.
- Added clean checkpoint no-op elision.
- Moved auto-checkpoint execution to after successful DML, avoiding WAL truncation before the mutation applies.
- Kept `native_sql.snap` as a compatibility marker while loading table-level checkpoint manifests first.
- Reduced SQL indexed result materialization clones by borrowing `Cell` values in hot projection paths.
- Added regression tests for checkpoint dirty tracking, no-op behavior, checkpoint failure retry state, table-level recovery, and duplicate-heavy projection correctness.
- Rebuilt and installed the local PyO3 wheel before Python benchmarks so results use the current source.

PostgreSQL strict comparison: not available. Local PostgreSQL exists, but the default role `postgres` does not exist, so no PostgreSQL performance claim is approved.

## 2. Root Cause

| bottleneck | root cause | status |
| --- | --- | --- |
| clean checkpoint | `checkpoint()` always synced WAL, serialized the full `tables` map, wrote/fsynced snapshot and index files, and truncated WAL even when no state changed | Fixed |
| dirty checkpoint | single full-snapshot file forced full-table serialization; index catalog was also rewritten broadly | Improved with table-level dirty checkpoint; not page-level incremental |
| checkpoint hot path correctness | auto-checkpoint could run after WAL append but before mutation execution | Fixed by moving mutation count/checkpoint after successful DML |
| duplicate-heavy materialization | index seek was fast; row-id collection, row fetch, projection conversion, and Python/result materialization dominated | Partially improved; still a bottleneck |
| string index ceiling | borrowed lookup helps lookup cost, but duplicate-heavy latency is dominated by result cardinality/materialization | Still a ceiling target |

## 3. Code Paths Changed

| file | change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | dirty tracker, table-level checkpoint manifest/files, clean no-op checkpoint, safer auto-checkpoint ordering, borrowed projection bytes, checkpoint/materialization tests |
| `scripts/perf_investigate_native_sql.py` | fixed `checkpoint.no_dirty_tables` methodology by checkpointing the baseline before measuring no-dirty calls |
| `docs/native_sql_benchmark_last.json` | release benchmark output from current rebuilt wheel |
| `docs/native_sql_perf_investigation_last.json` | full perf investigation output from current rebuilt wheel |
| `docs/postgres_comparison_latest.json` | strict comparison attempt and failure reason |
| `docs/profiles/native_sql_core_bench_*` | Rust core timing profile fallback output |

## 4. Benchmark Before/After

Checkpoint before values are from Ceiling Pass 3. After values are from:

```bash
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
```

| workload | before p50 ms | after p50 ms | after p95 ms | after p99 ms | result |
| --- | ---: | ---: | ---: | ---: | --- |
| `checkpoint.no_dirty_tables` | ~148 | 0.000125 | 0.000167 | 0.000833 | fixed; no-op elided |
| `checkpoint.dirty_small_table` | ~334 | 10.932 | 12.051 | 13.004 | >30x faster |
| `checkpoint.dirty_large_table` | ~426 | 13.501 | 17.699 | 17.699 | >30x faster |
| `wal.commit_with_checkpoint_pressure` | ~341 | 12.801 | 17.398 | 35.751 | >25x faster |
| `wal.bulk_insert_1000_then_checkpoint` | not recorded in Pass 3 summary | 25.597 | 25.597 | 25.597 | measured |

Materialization:

| workload | before p50 ms | after p50 ms | after p95 ms | after p99 ms | result |
| --- | ---: | ---: | ---: | ---: | --- |
| `materialization.count_no_projection_duplicate_heavy` | ~0.006 | 0.006292 | 0.006542 | 0.006750 | held fast |
| `materialization.row_id_only_duplicate_heavy` | ~0.043 | 0.035416 | 0.083750 | 0.133375 | improved, less than 2x target |
| `materialization.one_col_projection_duplicate_heavy` | ~0.047 | 0.042792 | 0.055917 | 0.147459 | slight improvement only |
| `materialization.full_row_projection_duplicate_heavy` | ~0.134 | 0.121083 | 0.185584 | 0.357500 | ~10% improvement |
| `prepared.indexed_string_equality` | Pass 3 varied by run | 0.015917 | 0.020333 | 0.044875 | still materialization-bound |

Release benchmark:

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| `native_sql.simple_insert` | 0.003375 | 0.004000 | 0.005417 | 274630.96 |
| `native_sql.simple_select` | 0.002959 | 0.003125 | 0.003250 | 325936.32 |
| `native_sql.simple_update` | 0.002666 | 0.005541 | 0.021875 | 283252.66 |
| `native_sql.simple_delete` | 0.005334 | 0.005750 | 0.006084 | 180453.82 |
| `native_sql.predicate_path` | 0.039667 | 0.045417 | 0.078792 | 24308.65 |
| `python_gateway.startup_shutdown` | 0.306500 | 0.644333 | 0.984125 | 2849.35 |

## 5. Correctness Validation

Commands run:

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
python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels
python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
ITERATIONS=1000 OUT_DIR=docs/profiles scripts/profile_native_sql_rust.sh
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json --strict
```

Results:

- `cargo check --no-default-features`: pass.
- `cargo check` default features: pass.
- `cargo test --no-default-features`: pass, 410 passed, 15 ignored; release crash tests also passed in-suite.
- `release_crash_recovery`: pass, 7 passed.
- `release_crash_kill_recovery`: pass, 7 passed.
- default-feature cargo test with PyO3 flags: pass, 410 passed, 15 ignored.
- `pytest -q -rxX`: pass, 1113 passed, 12 skipped, 6 xfailed, no XPASS.
- duplicate hygiene check: pass.
- py_compile: pass.
- local PyO3 wheel rebuild/install: pass.
- release benchmark: pass.
- full perf investigation: pass.
- Rust profiler script: pass using timing-summary fallback; no flamegraph/samply/instruments profiler was available.
- PostgreSQL strict comparison: failed by environment, `role "postgres" does not exist`.

## 6. Risks

- Checkpoint is table-level incremental, not page-level incremental.
- Dirty checkpoint still pays per-dirty-table serialization and fsync; p50 is now ~11-14 ms rather than sub-ms.
- Rollback conservatively leaves dirty state when a transaction touched persistent tables; this may cause an extra checkpoint of restored state, but not incorrect state.
- Duplicate-heavy one-column/full-row materialization did not hit the 2x / 20-40% target.
- String index unique lookup is close to numeric in direct core, but SQL/prepared duplicate-heavy paths remain materialization-bound.
- PostgreSQL strict comparison remains blocked by local role/DSN setup; no PostgreSQL-beating claim is valid.

## 7. Approved Claims

- Clean checkpoint no-op is elided and measured at p50 ~0.000125 ms.
- Checkpoint now uses table-level dirty tracking and table-level checkpoint files.
- Dirty checkpoint pressure fell from hundreds of milliseconds to low tens of milliseconds on this machine.
- COUNT duplicate-heavy indexed path remains fast by avoiding full row materialization.
- Narrow projection paths avoid `Cell` clone in hot SQL indexed projection loops.
- Crash/recovery tests still pass after the checkpoint format change.

## 8. Rejected Claims

- No claim that QMvir beats PostgreSQL: strict PostgreSQL comparison did not run successfully.
- No claim that durability performance is PostgreSQL-equivalent.
- No claim that checkpoint is page-level incremental.
- No claim that duplicate-heavy materialization is fully solved.
- No claim that string index has reached numeric-index ceiling.

## 9. Next Targets

- Page-level or segment-level incremental checkpoint.
- Async/background checkpoint or group checkpoint scheduling.
- Arena or reusable batch result buffers for duplicate-heavy materialization.
- Zero-copy/borrowed internal result batches before Python/pgwire conversion.
- Interned/symbol string keys for repeated indexed strings after result materialization is reduced.
- Strict PostgreSQL benchmark with a valid `POSTGRES_DSN` and benchmark role/database.
