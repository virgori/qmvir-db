# qmvir-db Stabilization Report - 2026-05-16

## Summary

This pass focused on correctness, deterministic test behavior, warning cleanup, Python binding parity, duplicate hygiene, and a truthful benchmark/release-gate status for version 5.4.0.

Release readiness: **Not Ready**. Core Rust and Python suites are green, but the repository still has a very large pre-existing dirty/staged worktree that must be reviewed, split, and committed or reset by the owner before a release can be cut.

## MVCC Root Cause

### `tests::test_concurrent_read_write`

Root cause: the MVCC transaction manager used a `next_commit_ts` atomic as if it were the latest committed timestamp. `current_commit_ts()` returned the next timestamp to allocate, so a new snapshot could get `read_ts = commit_ts + 1`. That broke deterministic snapshot semantics and made committed-version visibility inconsistent.

Files:

- `qm_engine/src/mvcc/tx_manager.rs`
- `qm_engine/src/mvcc/visibility.rs`
- `qm_engine/tests/mvcc_integration.rs`

Invariant restored:

- `current_commit_ts()` means latest committed epoch, not next allocatable epoch.
- Commit timestamp allocation is monotonic and starts at 1.
- New snapshots read at the latest committed timestamp.
- Active transaction IDs in a snapshot are authoritative for uncommitted visibility.

### `tests::test_write_set_tracking`

Root cause: `add_write()` appended to `write_set`, but did not add the table to `touched_tables`. Transactions that wrote rows without calling `touch_table()` lost table-level write-set metadata.

Files:

- `qm_engine/src/mvcc/tx_manager.rs`
- `qm_engine/tests/mvcc_integration.rs`

Invariant restored:

- Every insert/update/delete write record also marks its table as touched.
- Commit and rollback clear the session active transaction.
- Commit and rollback release transaction locks.

## MVCC Regression Tests Added

Added/covered in `qm_engine/tests/mvcc_integration.rs`:

- own uncommitted write is visible to the writer
- other transaction's uncommitted write is invisible
- snapshot isolation does not see commits after snapshot creation
- READ COMMITTED sees a later commit on a later statement snapshot
- rollback create leaves no visible row
- delete rollback restores visibility
- update keeps old version visible to old snapshot
- concurrent write conflict is detected
- write-set tracks insert/update/delete tables
- commit and rollback release locks
- new transaction can begin after commit or abort

Additional SQL regression:

- `gateway::native_sql::tests::delete_where_in_list_deletes_only_matching_rows`

## Other Correctness Fixes

- Gateway lifecycle now waits for TCP bind success and reports startup errors instead of returning success while the listener may have failed.
- Gateway `stop()` now notifies and joins the server thread, making rapid restart deterministic.
- `DELETE ... WHERE id IN (...)` now uses the shared predicate evaluator instead of equality-only parsing.
- `find_keyword_top_level()` now handles keyword patterns that include leading/trailing spaces, restoring `IN`, `AND`, and similar predicate matching.
- Python `NativeSqlEngine` now exposes `snapshot_info()`, `execute_timeout()`, `checkpoint()`, and useful `EXPLAIN` rows.
- Python binding exports backup functions, `ShardRing`, `ShardManager`, and dispatcher batch update/delete/drain APIs.
- Python `Transaction("snapshot_isolation")` is accepted; `serializable` remains an alias and reports as `snapshot_isolation` for compatibility.
- Python W-TinyLFU wrapper now uses a single cache for small capacities so entries are not immediately evicted by per-shard byte slicing.
- Added a minimal `gateway.api_postgres` package for Python wire-protocol integration tests.

## Repo Hygiene

Duplicate cleanup:

- Audited duplicate files matching `* [0-9].*` excluding generated Tauri target output.
- Found 186 duplicates: 96 byte-identical and 90 stale/divergent copies with canonical counterparts.
- Removed the duplicate copies.
- Added `scripts/check_no_space_number_duplicates.sh`.
- Added `.github/workflows/repo-hygiene.yml`.
- Current duplicate check: pass, no uncontrolled duplicate files found.

Metadata:

- Version is standardized at 5.4.0 across `Cargo.toml`, `pyproject.toml`, `npm/package.json`, and `qm_app.py`.
- License metadata is standardized as proprietary / `SEE LICENSE IN LICENSE`; README no longer advertises MIT.
- Python dev dependencies now include `pytest`, `pytest-asyncio`, `pytest-benchmark`, `msgpack`, `lark`, `lz4`, and `zstandard`.

Remaining hygiene risk:

- `git status --short` still shows a large dirty worktree with many pre-existing `A`, `AM`, and `??` files. This pass did not revert unrelated user/workspace changes.

## Warning Cleanup

Rust:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`: pass, no warnings.
- `cargo check --manifest-path qm_engine/Cargo.toml`: pass, no warnings.

Notable warning/hot-path cleanup:

- removed dead fields/imports in gateway, index, storage, statistics, parser, and executor modules
- tightened snapshot write behavior so missing dirty pages are errors
- cleaned PyO3 signatures to avoid deprecation warnings
- removed no-op HNSW buffered-insert greedy descent work
- clarified vectorized prefix-sum tail handling

Python:

- Full pytest has one remaining warning: `tests/test_task3_integration.py::TestQMPostgresServer::test_import` is marked `asyncio` but is not async.

## Test Results

Before:

- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- Rust lib unit tests: 396 passed, 15 ignored
- `bench_new_components`: 35 passed
- `mvcc_integration`: 12 passed, 2 failed
- Python `pytest`: initially blocked by missing dependencies and stale installed binding

After:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
- `cargo check --manifest-path qm_engine/Cargo.toml`: pass
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
- Rust lib unit tests: 397 passed, 15 ignored
- `bench_engine`: 15 ignored
- `bench_new_components`: 35 passed
- `mvcc_integration`: 23 passed
- Rust doc tests: 2 ignored
- `python3 -m pytest -q`: 1098 passed, 12 skipped, 7 xfailed, 9 xpassed, 1 warning

## Python Test Layer

Actions:

- Installed missing dev dependencies.
- Built a current Python wheel with `python3 -m maturin build`.
- Installed the wheel with pip so pytest imports the updated 5.4.0 binding.
- Ran full pytest outside the sandbox because gateway tests need localhost TCP bind permission.

Status: pass.

## Performance Hardening Completed

- MVCC commit timestamp hot path now uses a single monotonic latest-commit atomic with correct semantics.
- MVCC visibility checks now use snapshot active transaction IDs deterministically.
- Transaction commit/abort avoid session cleanup while holding the transaction registry lock.
- Gateway startup avoids spin/sleep races and deterministic stop joins the listener thread.
- SQL delete uses the existing predicate evaluator, avoiding duplicated equality-only parsing.
- W-TinyLFU Python wrapper avoids sharding overhead and pathological eviction for small test/cache capacities.
- HNSW and vectorized executor warning cleanup removed no-op or misleading hot-path work.
- Snapshot writing now fails rather than silently producing inconsistent page-count metadata.

## Benchmark Status

No speedup claims are made in this report.

Commands actually run:

- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m pytest -q`

Machine/environment captured:

- commit: `cc30dc3`
- OS: macOS 26.5, Darwin arm64
- CPU arch: arm64
- logical CPUs from Python: 8
- build flags: Rust dev/test profile, `--no-default-features` for release gate test; Python wheel built by maturin dev profile

Existing benchmark-style coverage exercised as tests:

- MVCC insert latency smoke
- WAL group commit and inverted-WAL smoke
- HNSW/PQ/search/index smoke
- cache/sketch/statistics smoke
- hybrid search and executor smoke

Benchmark gaps before performance release:

- add Criterion or equivalent report with p50/p95/p99 for OLTP insert/lookup/update/delete/mixed RW
- add MVCC visibility throughput by version-chain length and active snapshot count
- add WAL durable TPS, batch/group append, fsync mode, and recovery-time benchmarks
- add executor scan/filter/projection/aggregation/join vectorized vs non-vectorized benchmarks
- add B+Tree, inverted, HNSW recall/latency, and concurrent insert/search benchmarks
- record memory usage, dataset size, warmup, run count, machine info, build flags, and commit hash for every result
- keep historical benchmark docs archived or marked historical to avoid current-performance claims without baseline

## Files Changed In This Pass

Primary Rust:

- `qm_engine/src/mvcc/tx_manager.rs`
- `qm_engine/src/mvcc/visibility.rs`
- `qm_engine/src/mvcc/executor.rs`
- `qm_engine/src/mvcc/row_version.rs`
- `qm_engine/tests/mvcc_integration.rs`
- `qm_engine/src/gateway/mod.rs`
- `qm_engine/src/gateway/server.rs`
- `qm_engine/src/gateway/native_sql.rs`
- `qm_engine/src/gateway/connection.rs`
- `qm_engine/src/cluster/shard.rs`
- `qm_engine/src/ipc/dispatcher.rs`
- `qm_engine/src/storage/cache.rs`
- `qm_engine/src/storage/mod.rs`
- `qm_engine/src/storage/snapshot.rs`
- `qm_engine/src/storage/transaction.rs`
- `qm_engine/src/storage/uring_wal.rs`
- `qm_engine/src/storage/wal.rs`
- `qm_engine/src/executor/vectorized.rs`
- `qm_engine/src/index/*`
- `qm_engine/src/statistics/*`
- `qm_engine/src/parser/dispatcher.rs`
- `qm_engine/src/lib.rs`

Python/docs/hygiene:

- `pyproject.toml`
- `qm_app.py`
- `qm_core/hub/hub.py`
- `gateway/__init__.py`
- `gateway/query_router/*`
- `gateway/auth/*`
- `gateway/api_postgres/*`
- `scripts/check_no_space_number_duplicates.sh`
- `.github/workflows/repo-hygiene.yml`
- `README.md`
- `npm/package.json`
- `tests/test_final_sprint.py`
- `docs/STABILIZATION_REPORT_2026_05_16.md`

## Remaining Risks

- Worktree is still very dirty from pre-existing staged/untracked files; release requires owner review and cleanup.
- MVCC metadata is now correct for the integration layer, but native SQL still has areas labelled phase-1 MVCC metadata and should not be advertised as fully serializable storage MVCC without deeper integration.
- WAL/crash recovery has smoke and unit coverage, but needs crash-injection and fsync-mode validation before a durability release claim.
- Benchmark harness is not yet a complete p50/p95/p99 release benchmark suite.
- Python test suite still has one warning and several xpass tests, which should be triaged.

## Release Gate

Status: **Not Ready**.

Reasons:

- Rust release-gate tests pass.
- Python tests pass.
- Duplicate guard passes.
- Warning cleanup is materially improved and current Rust checks are warning-free.
- However, the repository is not clean and the benchmark suite is not yet a complete release-grade performance report with p50/p95/p99, memory, and baseline data.
