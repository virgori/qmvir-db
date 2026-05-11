# QM Py -> Rust Migration Tracker

Updated: 2026-03-13 (v0.4.0 — Parquet Integration + Chunk-based Pipeline)
Owner: Core Runtime Team
Scope: Remove Python from query and data hot-path, keep Python only for optional tooling/fallback.

## Status Legend

- `DONE`: migrated to Rust runtime path
- `IN_PROGRESS`: partially migrated or shadow path active (Rust import + fast-path added)
- `TODO`: not migrated yet

## P0 - Query Hot-Path (must finish first)

1. `qm_core/hub_engine.py` - `IN_PROGRESS` (Rust fast-path in execute_sql, fallback Python)
2. `qm_core/execution/sql_parser.py` - `IN_PROGRESS` (Rust import added)
3. `qm_core/execution/join.py` - `IN_PROGRESS` (Rust fast-path for HashJoin)
4. `qm_core/execution/window.py` - `IN_PROGRESS` (Rust import added)
5. `qm_core/execution/vectorized.py` - `IN_PROGRESS` (Rust import added)
6. `qm_core/execution/planner.py` - `IN_PROGRESS` (Rust import added)
7. `qm_core/execution/pipeline.py` - `IN_PROGRESS` (Rust import added)
8. `gateway/api_postgres/hub_executor.py` - `DONE` (NativeSqlEngine handles all SQL in Rust)

## P1 - Data Plane / IPC / Storage Integration

1. `qm_core/hub/dispatcher.py` - `IN_PROGRESS` (Rust import added)
2. `qm_core/hub/hub.py` - `IN_PROGRESS` (Rust import added)
3. `qm_core/hub/lsn_sequencer.py` - `IN_PROGRESS` (Rust import added)
4. `qm_core/satellite/general_satellite.py` - `IN_PROGRESS` (Rust import added)
5. `qm_core/satellite/vector_satellite.py` - `TODO`
6. `qm_core/satellite/procedure_satellite.py` - `TODO`
7. `qm_core/ipc/ring_buffer.py` - `IN_PROGRESS` (Rust import added)
8. `qm_core/ipc/media_allocator.py` - `IN_PROGRESS` (Rust import added)

## P2 - Control Plane / Ops

1. `qm_app.py` - `IN_PROGRESS`
2. `gateway/api_postgres/server.py` - `IN_PROGRESS`
3. `qm_core/checkpoint.py` - `TODO`
4. `qm_core/satellite/worker.py` - `TODO`
5. `qm_core/auth.py` - `TODO`

## Rust Target Mapping

1. Hub runtime: `qm_engine/src/hub_engine/mod.rs`
2. Planner: `qm_engine/src/hub_engine/planner.rs`
3. Executor: `qm_engine/src/hub_engine/executor.rs`
4. Parser/dispatch: `qm_engine/src/parser/*`
5. Storage: `qm_engine/src/storage/*`
6. Gateway: `qm_engine/src/gateway/*` (NativeSqlEngine = 1200+ LOC in-memory SQL engine)

## Completed Milestones (Current)

1. Rust gateway native mode enabled (`PostgresGateway.start_native`).
2. Rust hub planner carries explicit hash-join keys (`build_key`, `probe_key`).
3. Rust hub executor performs native disk scan for `.qmr` and `.qmb` row files.
4. Rust hash join uses Rayon parallel probe path.
5. Benchmark path includes `SHADOW_MODE=1` compare hook.
6. Python `qm_core/execution/join.py` now has Rust fast-path for INNER HashJoin (`QM_RUST_HASH_JOIN=1`, fallback-safe).
7. Daemon gateway now supports strict Rust-only mode (`QM_RUST_ONLY=1`) and refuses Python gateway fallback.
8. Benchmark launcher now auto-detects PostgreSQL endpoint/auth mode and uses venv Python for Rust binding availability.
9. **B+Tree index fast-path** added to `NativeSqlEngine::handle_select_join()`: O(log n + k) instead of O(n).
10. **handle_select_sum()** fixed: parses actual column name, handles WHERE BETWEEN with B+Tree range scan.
11. **handle_select_between()** fixed: proper column projection and correct column-based filtering.
12. **Benchmark indexes**: `setup_data()` now creates B+Tree indexes on QM tables for parity with PostgreSQL.
13. **Shadow compare**: rewritten to use wire protocol (NativeSqlEngine) instead of HubEngine disk reads.
14. **All Python execution files** now have `import qm_engine` Rust fast-path imports.
15. **hub_engine.py execute_sql()** delegates to Rust HubEngine first, falls back to Python.
16. **Benchmark result**: JOIN QPS 1.01x PostgreSQL (was 0.07x), Stress 1.20x PostgreSQL.
17. **Tail latency**: QM p95 JOIN 30% lower than PostgreSQL, p95 SUM 56% lower.
18. **Parquet Integration (v0.4.0)**: `COPY table FROM 'file.parquet'` via Arrow 53 + Parquet 53 (zstd, snap, lz4). ColumnExtractor for zero-copy Arrow→Cell.
19. **Chunk-based Pipeline (v0.4.0)**: `CHUNK_SIZE=1024` for SELECT/BETWEEN/SUM/GROUP BY via `rayon::par_chunks`. Parallel local hash tables for GROUP BY.
20. **Batch INSERT (v0.4.0)**: Single write lock for entire batch insert (was per-row lock). Pre-build all rows first.
21. **Benchmark (v0.4.0)**: 8/9 wins vs PostgreSQL 16 + DuckDB 1.5. Point Lookup 17,514 QPS (2.6x PG), OLAP Full Scan 9,529 QPS (47x PG).
22. **Tests (v0.4.0)**: 59/59 pass (54 core + 5 Parquet integration tests).

## Documentation Update Rule

After every migration milestone, update these files in the same commit:

1. `docs/PY_TO_RS_MIGRATION_TRACKER.md` (status source of truth)
2. `docs/ARCHITECTURE.md` (runtime architecture delta)
3. `docs/PERFORMANCE_ROADMAP.md` (impact + next optimization targets)
4. `docs/QM_TECHNICAL_REPORT.md` (engineering state)
5. `docs/QM_EVALUATION_REPORT.md` (benchmark/validation state)