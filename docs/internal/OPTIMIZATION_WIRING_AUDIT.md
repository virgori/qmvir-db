# QMvir Optimization Wiring Audit

Date: 2026-08-27

Scope: static source audit of optimization components that exist in the Rust tree but are not fully connected to the current `NativeSqlEngine` execution path. No benchmark was run for this pass.

## Executive Summary

The gap is mostly integration, not absence of algorithms. The current hot path is concentrated in `qm_engine/src/gateway/native_sql.rs`; many advanced modules are implemented under `executor/`, `index/`, `htap/`, `statistics/`, `learned/`, `optimizer/`, `storage/`, and `hub_engine/`, but several are either exported only, tested only, benchmark-only, or invoked as advisory helpers without driving the final physical path.

Most immediate performance upside should come from wiring existing planning/indexing modules into the SQL path before inventing new algorithms.

## Highest Priority Gaps

### 1. HTAP Planner Is Only Partially Driving SELECT Execution

Status: aggregate fast-path gating wired after this audit; broader SELECT dispatch remains.

Evidence:

- `NativeSqlEngine::htap_plan_sql` delegates to `htap/planner.rs` and is used by durable SUM/COUNT helpers.
- `htap_try_durable_sum`, `htap_try_durable_sum_between`, and `htap_try_durable_count` gate durable column-segment execution on `ScanPath::ColumnScan`.
- SUM, AVG, and GROUP BY fallback branches now use `HtapPhysicalPlan` to decide whether column-cache fast paths are allowed.
- Broader SELECT/projection/join branches still depend on specialized `NativeSqlEngine` code rather than a unified HTAP physical dispatcher.

Impact:

- Some `SUM`, `AVG`, `COUNT`, and simple aggregate paths benefit from durable column segments or column cache.
- Aggregate column-cache execution now follows planner/cache eligibility instead of ignoring `_plan`.
- Broader SELECT, projection, join, and mixed predicate execution still depends on special-case code in `NativeSqlEngine`.
- The architecture's planner is closer to the hot path, but not yet the single source of truth for row vs column vs index path selection.

Wire-in target:

1. Extend the planner-gated dispatch beyond aggregate/range/GROUP BY into projection and join paths.
2. Add actual-path counters/profiling so EXPLAIN/planner choice can be compared with the path that executed.
3. Add tests that assert EXPLAIN/planner choice matches actual execution, not just reported intent.

### 2. Auto Index Manager Records Stats But Does Not Run Autonomous Lifecycle

Status: first SQL-path wiring landed after this audit; latency-baseline refinement remains.

Evidence:

- `NativeSqlEngine` records `record_query_hit`, `record_writes`, and numeric histogram samples.
- `IndexManager::evaluate`, `apply_decisions`, `populate_shadow`, `record_shadow_latency`, `record_shadow_observation`, and `gc_unused_indexes` exist.
- `NativeSqlEngine` now routes repeated filter observations through `record_auto_index_query_hit`.
- A low-frequency auto-index cycle evaluates decisions, creates/populates empty shadow B+Tree indexes, keeps shadow indexes current during DML maintenance, and lets equality/range/COUNT paths use shadow indexes as trial candidates.
- Shadow indexes can promote after repeated beneficial trial observations; full active-vs-shadow latency replay is still not wired.

Impact:

- Repeated selective predicates can now build/populate/promote B+Tree auto indexes from SQL traffic.
- Manual `CREATE INDEX` remains the explicit path, but common equality/range filters no longer depend on manual action only.
- Composite-index and full latency comparison logic still need deeper executor support.

Wire-in target:

1. Replace bootstrap trial observation with real active-vs-shadow latency replay for selected cheap queries.
2. Extend auto-index trial paths beyond simple equality/range/COUNT.
3. Add background population for very large tables so foreground query latency is not affected.
4. Add composite-index population semantics that preserve column tuple ordering instead of treating each column independently.

### 3. JIT Expression Infrastructure Is Not Connected To Native SQL Filters

Status: not wired to SQL hot path.

Evidence:

- `executor/jit.rs` defines `JitExpr`, `JitCache`, interpreter, and `batch_filter`.
- Non-test usage is limited to the `qm` benchmark/demo path.
- `NativeSqlEngine` does not lower parsed WHERE/HAVING expressions into `JitExpr`.
- The file header mentions Cranelift, but the current crate does not expose a real Cranelift backend in the hot path.

Impact:

- Repeated predicates and arithmetic expressions are evaluated by bespoke row/column loops.
- There is no hot-expression cache for common WHERE filters.
- Native machine-code JIT claims should remain experimental until a backend is wired and benchmarked end-to-end.

Wire-in target:

1. Lower a narrow predicate subset first: integer equality, integer BETWEEN, float comparisons, AND/OR.
2. Use `batch_filter` against existing column-cache vectors for supported predicates.
3. Keep fallback behavior exact for unsupported SQL expressions.
4. Only add native-code backend after the interpreted/vectorized path proves the lowering contract.

### 4. Learned Models And Generic Optimizer Are Public But Not Planner Inputs

Status: not wired to SQL hot path.

Evidence:

- `learned/` exports `SelectivityModel`, `CachePredictor`, `FusionWeightTuner`, and `IntentClassifier`.
- `optimizer/` exports adaptive optimizer and rule rewrites.
- `statistics/` exports Bloom, HyperLogLog, Count-Min Sketch, T-Digest, and `CostModel`.
- Search found these mostly in module exports, tests, and benchmarks, not in `NativeSqlEngine` path selection.
- Histogram selectivity inside `IndexManager` is a partial exception for BETWEEN estimates.

Impact:

- Planner decisions cannot yet self-correct from observed selectivity/cardinality.
- Cache decisions do not use learned access interval prediction.
- Hybrid search ranking does not appear to be tuned from `FusionWeightTuner` in the main SQL path.

Wire-in target:

1. Feed runtime observed row counts into `SelectivityModel`.
2. Feed table/index stats into `CostModel` and HTAP planner.
3. Use `IntentClassifier` only for choosing search/analytics/vector routing after deterministic SQL parsing succeeds.
4. Add guardrails so learned feedback can bias but not override correctness constraints.

## Important Medium Priority Gaps

### 5. Vector HNSW SQL Path Uses In-Memory HNSW, Not Concurrent/Sharded/Mmap Backends

Status: partially wired.

Evidence:

- `VectorHnswCatalog` is wired into `NativeSqlEngine`.
- `ManagedVectorHnswIndex` stores `RwLock<HnswIndex>` plus pending vectors.
- `ConcurrentHnswIndex`, `ShardedHnswIndex`, and `MmapVectorStore` are exported and tested but not used by `VectorHnswCatalog`.

Impact:

- Current vector SQL path benefits from HNSW and lazy pending flush.
- It does not yet reach the intended large-dataset mmap storage, sharded graph, or concurrent graph mutation design.
- Memory pressure and concurrent insert/search behavior will plateau before the architecture target.

Wire-in target:

1. Introduce a backend trait for vector index storage/search.
2. Keep current `HnswIndex` as default backend.
3. Add mmap storage for vector payloads first, then concurrent or sharded graph backend behind a feature/config gate.

### 6. WAL-Backed Inverted Index Module Is Not Used By SQL FTS Catalog

Status: not wired to SQL hot path.

Evidence:

- `InvertedIndexCatalog` is wired into `NativeSqlEngine` for `@@` / `MATCH` style search.
- Search checkpoint encode/load is wired for index persistence.
- `WalInvertedIndex` appears in its own module/tests and public re-export, but not in `NativeSqlEngine`.

Impact:

- Full-text search benefits from the managed inverted catalog.
- It does not use the WAL-backed inverted-index recovery/batching module for high-frequency index mutation workloads.

Wire-in target:

1. Decide whether `WalInvertedIndex` replaces or backs `ManagedInvertedIndex`.
2. Route FTS insert/update/delete maintenance through a WAL-backed adapter.
3. Keep existing search checkpoint as snapshot acceleration, not the only recovery mechanism.

### 7. Binary StorageEngine Optimizations Are Parallel To Native SQL Persistence

Status: secondary path.

Evidence:

- `storage/` implements binary WAL/pages/cache/snapshot/io_uring pieces.
- `NativeSqlEngine` persists through its own text SQL WAL and table snapshots, with optional `WalBackend::Uring`.
- `StorageEngine` is used by the hub-engine/Python-facing surfaces, not as the main SQL persistence layer.

Impact:

- Native SQL does receive batching, group commit, and optional `io_uring` WAL.
- Other binary storage optimizations do not automatically improve SQL recovery/write path unless adapted.
- There is duplicated persistence architecture.

Wire-in target:

1. Keep the text SQL WAL for logical replay compatibility in the short term.
2. Add a binary mutation-log adapter for high-volume DML.
3. Make checkpoint/recovery choose the fastest complete source without weakening logical correctness.

### 8. Hub/Satellite Planner And IPC Are Not The Default Runtime

Status: experimental/opt-in architecture surface.

Evidence:

- `hub_engine/` has physical planning/executor pieces.
- `ipc/` has mmap ring/dispatcher support.
- `NativeSqlEngine` currently runs in-process and does not route normal SQL through hub/satellite IPC.

Impact:

- The default build does not benefit from process isolation, satellite scheduling, or IPC-parallel execution.
- This is acceptable as an R&D boundary, but should not be counted in current performance claims.

Wire-in target:

1. Define one explicit dispatch use case, for example vector search worker or analytical scan worker.
2. Add config-gated routing and latency accounting.
3. Only expand after a benchmark proves IPC overhead is amortized.

## Already Wired Or Usable With Explicit Configuration

- Transaction commit uses WAL batching and can use `group_commit_sync`, `per_commit_sync`, `per_commit_sync_data`, or relaxed policies.
- `WalBackend::Uring` is connected through `QMVIR_URING_WAL=1` on Linux, but it is not default and is unavailable on macOS.
- Search, trigram, JSON path, and vector HNSW catalogs are maintained on DML when indexes exist.
- HTAP MVCC dirty tracking, commit publishing, and column-segment helpers are connected to `NativeSqlEngine`.
- PyO3 bindings are behind the `python` feature and no longer part of the default Rust build.

## Suggested Execution Order

1. Wire HTAP planner output into aggregate/range/GROUP BY execution and add actual-path assertions.
2. Wire auto-index evaluation, shadow population, latency observation, and promotion.
3. Add JIT lowering for a narrow predicate subset against existing column cache.
4. Move vector payloads to mmap storage, then evaluate concurrent/sharded graph backend.
5. Decide whether WAL-backed inverted index becomes the FTS catalog backend.
6. Feed `CostModel`, `SelectivityModel`, and observed cardinality into planner decisions.
7. Consolidate binary WAL/storage pieces into Native SQL only after the logical WAL contract is preserved.
8. Keep hub/satellite IPC experimental until one dispatch path beats in-process execution under benchmark.
