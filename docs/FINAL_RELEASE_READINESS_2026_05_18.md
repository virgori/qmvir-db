# Final Release Readiness - 2026-05-18

Final decision: **Ready**

Scope: Ready for the clean local QM Engine / NativeSqlEngine 5.4.0 rc1 source
snapshot under the documented release claims. This is not a claim of universal
performance, torn-write/power-loss durability, or full storage-wide
PostgreSQL-style MVCC across every storage/cache/index path.

## Snapshot

- Snapshot path: `/Users/gengyang/QM/release_snapshots/qm_engine_5.4.0_rc1`
- Archive path: `/Users/gengyang/QM/release_snapshots/qm_engine_5.4.0_rc1.tar`
- Create result after the 2026-05-19 performance pass: `copied_files: 503`,
  `skipped_entries: 79`, `size_mb: 6.05`
- Archive result: `archive_size_mb: 6.48`
- Final snapshot checker result after doc-only report refresh: pass, `files: 503`,
  `size_mb: 6.05`
- `docs/native_sql_benchmark_last.json` is the one intentional generated file
  kept after snapshot validation.

The snapshot checker confirms no `.env`, `.DS_Store`, build output, Python
cache, Rust target directory, local data/wal/tmp state, duplicate `" 2"` paths,
platform binaries, static `libqm` artifacts, or historical generated regression
reports are included.

## Root-Cause Closure

See `docs/ROOT_CAUSE_CLOSURE_2026_05_18.md` for the full table. Release-blocking
items closed in this pass:

- Python MVCC write-write conflict detection: fixed with commit timestamp
  visibility, first-committer-wins conflict detection, deterministic loser
  abort, rollback WAL entry, and active transaction cleanup.
- WAL/crash safety: strengthened from simulated restart only to subprocess
  process-abort recovery coverage.
- Benchmark baseline: promoted from placeholder to approved local-machine
  baseline.
- Snapshot reproducibility: automated with create/check/validate scripts.
- HNSW recall benchmark nondeterminism: fixed by deterministic level generation
  so clean snapshot validation is reproducible.

Known scoped limitations are documented and non-blocking for this release:

- NativeSqlEngine does not claim full storage-wide MVCC across every
  storage/cache/index path.
- Crash validation covers process abort, not torn-write, disk-full, power-loss,
  or fsync-fault injection.
- Remaining xfails are outside the NativeSqlEngine release claim.

## Files Changed

- `core_db/transaction_engine/mvcc.py`
- `tests/test_core_internals.py`
- `qm_engine/Cargo.toml`
- `qm_engine/src/index/hnsw.rs`
- `qm_engine/tests/release_crash_kill_recovery.rs`
- `README.md`
- `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md`
- `docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md`
- `docs/LOCAL_RELEASE_MANIFEST_2026_05_18.md`
- `docs/ROOT_CAUSE_CLOSURE_2026_05_18.md`
- `docs/BENCHMARK_BASELINE_2026_05_18.md`
- `docs/PERFORMANCE_BASELINE_2026_05_18.md`
- `docs/PERFORMANCE_BOTTLENECKS_2026_05_18.md`
- `docs/FULL_PERFORMANCE_INVESTIGATION_2026_05_18.md`
- `docs/native_sql_benchmark_baseline.json`
- `docs/native_sql_benchmark_last.json`
- `docs/native_sql_perf_investigation_last.json`
- `docs/postgres_comparison_2026_05_18.json`
- `scripts/create_local_release_snapshot.py`
- `scripts/check_local_release_snapshot.py`
- `scripts/validate_local_release_snapshot.sh`

Additional performance-only implementation changes from the 2026-05-19 hot-path
pass:

- `qm_engine/src/gateway/native_sql.rs`
- `qm_engine/src/gateway/server.rs`
- `qm_engine/src/index/auto_manager.rs`

## Tests Added Or Strengthened

- Python MVCC conflict regression tests in `tests/test_core_internals.py`
  covering same-row conflict, loser rollback, conflict after one commit,
  independent-row non-conflict, repeated conflict cleanup, and snapshot
  visibility after begin.
- Rust process-abort crash recovery tests in
  `qm_engine/tests/release_crash_kill_recovery.rs`.
- Deterministic HNSW level generation to keep `bench_hnsw_recall` reproducible
  under clean snapshot validation.

## Xfail Status

Final pytest result:

```text
python3 -m pytest -q -rxX
1113 passed, 12 skipped, 6 xfailed in 38.04s
```

No XPASS remains and no pytest warning summary was emitted.

Remaining xfails:

- `tests/test_distributed_sharding.py::TestHubEngineSQL::test_execute_sql_create_table`
- `tests/test_distributed_sharding.py::TestHubEngineSQL::test_execute_sql_insert`
- `tests/test_distributed_sharding.py::TestHubEngineSQL::test_execute_sql_select`
- `tests/test_full_engine.py::TestSelect::test_select_between_float`
- `tests/test_vector_comprehensive.py::TestVectorQuantizerExtended::test_memory_usage`
- `tests/test_vector_comprehensive.py::TestVectorQuantizerExtended::test_compression_ratio`

These are documented as non-blocking for the scoped NativeSqlEngine local
release.

## Source Validation

Commands run from `/Users/gengyang/QM`:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
python3 -m pytest -q -rxX
bash scripts/check_no_space_number_duplicates.sh
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json
```

Results:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
- `cargo check --manifest-path qm_engine/Cargo.toml`: pass
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
  - lib unit tests: 397 passed, 15 ignored
  - `bench_engine`: 15 ignored
  - `bench_new_components`: 35 passed
  - `mvcc_integration`: 23 passed
  - `release_crash_kill_recovery`: 7 passed
  - `release_crash_recovery`: 7 passed
  - doctests: 2 ignored
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery`: pass, 7 passed
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery`: pass, 7 passed
- `python3 -m pytest -q -rxX`: pass, 1113 passed, 12 skipped, 6 xfailed
- `bash scripts/check_no_space_number_duplicates.sh`: pass
- benchmark smoke: pass

Additional 2026-05-19 performance validation:

- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features vector_`:
  pass, 24 vector-related tests across unit/integration targets
- `python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default`:
  pass, full run completed in 4.03s
- `python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default`:
  pass, release baseline refreshed
- `python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_2026_05_18.json`:
  script pass, PostgreSQL comparison skipped because local role `postgres` does
  not exist

The Python suite and benchmark smoke require localhost permission because they
start the local gateway server.

## Benchmark Baseline

Baseline file: `docs/native_sql_benchmark_baseline.json`

Approved: **yes**, as a local-machine baseline only.

Full baseline command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

Environment:

- OS: macOS-26.5-arm64-arm-64bit-Mach-O
- CPU count: 8
- Machine: arm64
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build mode metadata: release
- Feature flags: default
- Peak RSS: 26.72 MB

Baseline summary:

| Benchmark | p50 ms | p95 ms | p99 ms | throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| `native_sql.simple_insert` | 0.0033 | 0.0038 | 0.0056 | 278862.24 |
| `native_sql.simple_select` | 0.0028 | 0.0030 | 0.0031 | 343736.16 |
| `native_sql.simple_update` | 0.0025 | 0.0026 | 0.0027 | 392631.72 |
| `native_sql.simple_delete` | 0.0051 | 0.0053 | 0.0054 | 192249.13 |
| `native_sql.predicate_path` | 0.0023 | 0.0025 | 0.0080 | 379356.77 |
| `native_sql.mvcc_read_write` | 0.0326 | 0.0563 | 0.0702 | 29606.53 |
| `native_sql.concurrent_read_write_smoke` | n/a | n/a | n/a | 238547.55 |
| `native_sql.vector_cache_hot_path` | 0.0040 | 0.0042 | 0.0055 | 236910.69 |
| `python_gateway.startup_shutdown` | 0.2652 | 0.5115 | 0.7172 | 3425.32 |

Final source-tree benchmark smoke also passed.

## Snapshot Validation

Commands run:

```bash
python3 scripts/create_local_release_snapshot.py --version 5.4.0 --rc rc1 --clean --archive
python3 scripts/check_local_release_snapshot.py release_snapshots/qm_engine_5.4.0_rc1
bash scripts/validate_local_release_snapshot.sh release_snapshots/qm_engine_5.4.0_rc1
python3 scripts/check_local_release_snapshot.py release_snapshots/qm_engine_5.4.0_rc1
```

Validation result from inside the snapshot:

- cargo check no default features: pass
- cargo check default features: pass
- cargo test no default features: pass
  - lib unit tests: 397 passed, 15 ignored
  - `bench_engine`: 15 ignored
  - `bench_new_components`: 35 passed
  - `mvcc_integration`: 23 passed
  - `release_crash_kill_recovery`: 7 passed
  - `release_crash_recovery`: 7 passed
  - doctests: 2 ignored
- `release_crash_recovery`: pass, 7 passed
- `release_crash_kill_recovery`: pass, 7 passed
- `python3 -m pytest -q`: pass, 1113 passed, 12 skipped, 6 xfailed in 41.85s
- duplicate hygiene check: pass
- benchmark smoke: pass
- final snapshot checker after validation: pass, 504 files, 6.05 MB
- final recreated snapshot checker after doc-only report refresh: pass, 503 files,
  6.05 MB

One non-escalated snapshot validation attempt failed at Python collection because
the sandbox blocked localhost server startup. The same snapshot validation
passed with localhost permission.

An earlier snapshot run exposed the nondeterministic HNSW recall test; that was
fixed before the final source and snapshot validation runs.

## Remaining Risks

- Full storage-wide MVCC is not claimed for NativeSqlEngine in this release.
- Crash validation covers restart and process abort, but not torn writes,
  disk-full, or fsync-fault injection.
- The benchmark baseline is local to this machine and must not be used as a
  public cross-hardware performance claim.
- The 2026-05-19 PostgreSQL comparison was not available locally because the
  default PostgreSQL role `postgres` does not exist; do not claim a current
  PostgreSQL comparison table from this pass.
- HubEngine SQL passthrough, pgwire float `BETWEEN`, and VectorQuantizer
  accounting xfails remain as documented non-blocking gaps.

## Readiness Decision

Status: **Ready**

Reason:

- release-blocking Python MVCC write-write conflict detection is fixed and
  covered by regression tests
- no XPASS or avoidable pytest warning remains
- remaining xfails are documented and outside the scoped NativeSqlEngine release
- simulated restart and process-abort crash/recovery tests pass
- benchmark baseline is approved as a local release baseline
- deterministic snapshot create/check/validate scripts exist
- source-tree validation passes
- clean snapshot validation passes from inside the snapshot
- snapshot safety checker passes and excludes secrets, local junk, caches, build
  outputs, platform binaries, duplicate-copy paths, and generated historical
  artifacts
- release claims have been narrowed to match the implementation boundary
