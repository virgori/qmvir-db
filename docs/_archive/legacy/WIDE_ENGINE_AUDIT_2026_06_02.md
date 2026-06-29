# Wide Engine Audit - 2026-06-02

## Executive Summary

This pass focused on correctness guardrails and benchmark honesty outside the already-optimized NativeSqlEngine scalar SQL path.

Fixed in this pass:

- Exact vector search now validates dimension, metric, top_k, and non-finite vector values before indexing/search.
- Exact vector search now has deterministic tie-breaking for equal distances.
- Duplicate vector IDs now replace the previous vector instead of silently creating duplicate live entries.
- HNSW wrapper now shares the same validation guardrails, handles empty/top_k=0 queries, keeps metadata for filtering, and exposes remove/size semantics.
- Vector quantization now rejects empty, non-numeric, NaN, and Inf inputs with explicit errors.
- PostgreSQL comparison output now records that QM uses the default in-memory/autocommit NativeSqlEngine path, so current wins are not durability-equivalent WAL/fsync claims.
- Added a deterministic vector/search/quantization benchmark with ANN recall@10 against exact search.

Current status:

- NativeSqlEngine scalar local workloads still show QM faster than PostgreSQL in the current comparison JSON.
- No PostgreSQL-beating claim is approved for durable WAL/fsync equivalence.
- Vector ANN speed claims are only approved together with recall evidence.
- Sharding, HubEngine, planner, text/hybrid search, and Python/Rust bridge have smoke/behavioral coverage but not enough distributed/prod-grade correctness proof.

## Subsystem Inventory

| Subsystem | Files inspected | Current tests | Known gaps | Risk level |
| --- | --- | --- | --- | --- |
| Vector exact search | `vector_platform/ann_index/hnsw.py`, `tests/test_vector_comprehensive.py` | Exact ordering, filters, remove, metric, validation, duplicate replacement | Score semantics for dot-product remain legacy | Medium |
| HNSW / ANN | `vector_platform/ann_index/hnsw.py`, `tests/test_vector_comprehensive.py` | Recall@10 vs exact, empty/top_k=0, validation, size | hnswlib path is approximate; persistence/rebuild not proven here | Medium |
| Quantization | `vector_platform/quantization/quantizer.py`, `tests/test_vector_comprehensive.py` | Byte accounting, aliases, ratio, bounded int8 error, invalid inputs | PQ/SQ are accounting labels only in this module, not full trained quantizers | Medium |
| BM25 / text search | `search_platform/lexical_search/bm25.py`, `tests/test_bm25.py` | Ranking smoke, remove, field weights | No large corpus regression, tokenizer language coverage, or persistence proof | Medium |
| Hybrid search | `search_platform/hybrid_fusion/fusion.py`, `qm_engine/src/executor/hybrid_search.rs` | Present in tree; indirect planner/search tests | Fusion scoring calibration and vector/text consistency not release-proven | High |
| Sharding / routing | `qm_engine/src/cluster/shard.rs`, `tests/test_distributed_sharding.py` | Deterministic routing, distribution smoke, add/remove shard | No rebalancing data movement proof, failure handling, or distributed transaction proof | High |
| WAL / durability | `qm_engine/src/gateway/native_sql.rs`, `qm_engine/src/storage/wal.rs`, crash tests | Crash/recovery release tests and checkpoint tests from prior passes | PostgreSQL comparison does not use QM durable fsync path; page-level checkpoint still future work | Medium |
| Index correctness | `qm_engine/src/index/bplus_tree.rs`, `tests/test_indexing_comprehensive.py`, NativeSqlEngine tests | Duplicate-span fixes from prior passes, Python/Rust index smoke | Full Rust B+Tree split/delete/recovery coverage should remain mandatory | Medium |
| Query planner | `gateway/query_router/planner.py`, `qm_engine/src/hub_engine/planner.rs`, `tests/test_planner.py` | Router smoke by workload type | No cost-model proof or invalidation/fallback matrix for all SQL shapes | High |
| Python/Rust bridge | `qm_engine/src/lib.rs`, gateway/PyO3 modules, Python tests | Full pytest and NativeSqlEngine bridge behavior | Zero-copy bridge not implemented; conversion overhead is measured but still present | Medium |
| Benchmark honesty | `scripts/compare_postgres_native_sql.py`, benchmark JSON/docs | Strict PG script runs locally; metadata now clearer | Current PG wins are not durability-equivalent because QM side is default in-memory/autocommit | High |

## Bugs Fixed

| Area | Root cause | Fix |
| --- | --- | --- |
| Vector search validation | Dimension, metric, NaN/Inf, and top_k errors could fail late or silently produce invalid search state | Added metric normalization and explicit vector/top_k validation |
| Exact search determinism | Equal-distance rows used NumPy argsort without deterministic ID tie-break | Sort now uses `(distance, vector_id)` |
| Duplicate vector IDs | Re-adding an ID appended another live entry | Add now replaces vector and metadata for the same ID |
| HNSW wrapper guardrails | HNSW path did not consistently validate, handle empty queries, or preserve metadata | Shared validation, empty/top_k=0 handling, metadata storage, remove support |
| Quantization invalid input | Empty arrays and non-finite data produced NumPy errors or invalid params | Added explicit validation and finite param checks |
| Benchmark metadata | `durability_mode=wal_fsync` could be misread as applying equally to QM and PostgreSQL | Added `qm_durability_mode=native_sql_default_in_memory_autocommit` and explicit fairness rule |

## Benchmark Results

Vector/search/quantization audit command:

```bash
python3 scripts/vector_search_audit_benchmark.py --iterations 100 --output docs/vector_search_audit_latest.json
```

Selected results:

| Workload | p50 ms | p95 ms | Throughput ops/s | Quality |
| --- | ---: | ---: | ---: | --- |
| `vector.exact_search_n100_d32_cosine` | 0.054125 | 0.067166 | 17771.6 | recall@10 1.0 |
| `vector.exact_search_n1000_d32_cosine` | 0.477792 | 0.579792 | 2042.2 | recall@10 1.0 |
| `vector.hnsw_search_n1000_d32_cosine` | 0.017209 | 0.022917 | 54365.0 | recall@10 1.0 |
| `vector.hnsw_search_n1000_d128_euclidean` | 0.055458 | 0.087916 | 16723.7 | recall@10 1.0 |
| `quantization.to_int8_1000x128` | 0.182667 | 0.211083 | 5242.4 | mean abs error 0.009161 |
| `quantization.from_int8_1000x128` | 0.061042 | 0.074208 | 15884.4 | max abs error 0.018356 |

Current PostgreSQL comparison from `docs/postgres_comparison_latest.json`:

| Workload | QM p50 ms | PostgreSQL p50 ms | Winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 0.004917 | 0.102792 | QM | 25.82 |
| select_by_pk | 0.003500 | 0.036708 | QM | 11.01 |
| update_by_pk | 0.002917 | 0.086375 | QM | 33.48 |
| delete_by_pk | 0.008041 | 0.184291 | QM | 25.59 |
| indexed_integer_equality | 0.004958 | 0.039541 | QM | 8.15 |
| indexed_string_equality_duplicate_heavy | 0.016750 | 0.051500 | QM | 3.06 |
| count_indexed_equality | 0.003458 | 0.042084 | QM | 12.14 |
| predicate_range | 0.004625 | 0.049208 | QM | 10.87 |
| transaction_commit | 0.003667 | 0.141041 | QM | 41.12 |
| transaction_rollback | 0.005625 | 0.097000 | QM | 17.24 |
| indexed_string_equality_unique | 0.004250 | 0.037667 | QM | 9.03 |

PostgreSQL settings in that run: PostgreSQL 17.9, `fsync=on`, `synchronous_commit=on`.

Important caveat: the QM side of this comparison uses `qm_durability_mode=native_sql_default_in_memory_autocommit`. These are valid local scalar-path wins for the measured script, not durable WAL/fsync equivalence claims.

Latest NativeSqlEngine release benchmark highlights:

| Workload | p50 ms | p95 ms | Throughput ops/s |
| --- | ---: | ---: | ---: |
| `native_sql.simple_insert` | 0.003416 | 0.003709 | 273938.2 |
| `native_sql.simple_select` | 0.003125 | 0.003167 | 314679.9 |
| `native_sql.simple_update` | 0.002625 | 0.002875 | 369458.2 |
| `native_sql.simple_delete` | 0.005583 | 0.006042 | 161173.3 |
| `native_sql.mvcc_read_write` | 0.007042 | 0.009458 | 132048.1 |
| `native_sql.vector_cache_hot_path` | 0.004208 | 0.004416 | 235128.1 |

Latest deeper investigation highlights:

| Workload | p50 ms | p95 ms | Throughput ops/s |
| --- | ---: | ---: | ---: |
| `prepared.select_by_pk` | 0.000958 | 0.001042 | 956289.8 |
| `prepared.indexed_string_equality` | 0.016875 | 0.018417 | 58312.4 |
| `checkpoint.no_dirty_tables` | 0.000125 | 0.000167 | 4898457.2 |
| `checkpoint.dirty_small_table` | 15.971583 | 17.282709 | 65.4 |
| `wal.commit_with_checkpoint_pressure` | 16.154334 | 20.016375 | 61.2 |
| `gateway.reused_connection_select_by_pk` | 0.022875 | 0.028416 | 42020.5 |
| `vector.cache_hot_path` | 0.031792 | 0.035041 | 30884.1 |

## Validation Commands Run So Far

```bash
python3 -m pytest tests/test_vector_comprehensive.py -q
python3 -m py_compile scripts/vector_search_audit_benchmark.py
python3 scripts/vector_search_audit_benchmark.py --iterations 100 --output docs/vector_search_audit_latest.json
cargo fmt --manifest-path qm_engine/Cargo.toml
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml
python3 -m pytest -q -rxX
bash scripts/check_no_space_number_duplicates.sh
python3 -m py_compile scripts/compare_postgres_native_sql.py scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/vector_search_audit_benchmark.py
python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels
python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json --strict
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
```

Results:

- `tests/test_vector_comprehensive.py`: 43 passed.
- `scripts/vector_search_audit_benchmark.py`: compiled and wrote `docs/vector_search_audit_latest.json`.
- `cargo fmt`: passed.
- `cargo check --no-default-features`: passed.
- `cargo check`: passed.
- `cargo test --no-default-features`: 412 passed, 15 ignored in lib tests; integration/crash targets passed, including release crash kill/recovery.
- `cargo test` with PyO3 link flags: 412 passed, 15 ignored in lib tests; integration/crash targets passed.
- `python3 -m pytest -q -rxX`: 1131 passed, 12 skipped.
- duplicate hygiene check: passed.
- script compile check: passed.
- maturin release wheel build/install: passed.
- strict PostgreSQL comparison: passed with PostgreSQL available.
- NativeSqlEngine release benchmark and perf investigation: passed and updated JSON outputs.

## Approved Claims

- Exact vector search now fails fast for invalid metric, dimension mismatch, negative top_k, and non-finite vectors.
- Exact vector search ordering is deterministic under equal distance.
- HNSW benchmark rows are not accepted without recall@10 against exact search.
- Quantization byte accounting and dtype aliases are covered by tests.
- Current scalar NativeSqlEngine local comparison beats PostgreSQL for the listed workloads in `docs/postgres_comparison_latest.json`.

## Rejected Claims

- Do not claim durable PostgreSQL-equivalent performance from `compare_postgres_native_sql.py`; QM is not using a matched WAL/fsync persistent path there.
- Do not claim production-ready sharding/distributed semantics; tests currently prove routing behavior, not failure/rebalance/data movement correctness.
- Do not claim full hybrid search quality; scoring calibration and persistence are not proven.
- Do not claim all vector search is production ANN; HNSW behavior depends on `hnswlib` availability and recall validation.
- Do not claim query planner maturity beyond the supported routing and NativeSqlEngine prepared/scalar subset.

## Remaining Risks

- HNSW persistence/reload and delete compaction need dedicated tests before production claim.
- PQ/SQ labels in `VectorQuantizer` do not represent full product/scalar quantizer training pipelines.
- BM25 and hybrid search need larger relevance fixtures and persistence/reload tests.
- Sharding needs data movement, replication, failure, and transaction consistency tests.
- Python/Rust bridge still materializes Python objects at the boundary; zero-copy bridge remains future work.
- Benchmark comparison needs a separate persistent NativeSqlEngine WAL/fsync mode before durability claims.

## Next Targets

1. Add persistent/reload tests for HNSW/vector indexes and quantized vectors.
2. Add BM25/hybrid relevance regression fixtures with stable expected ordering.
3. Add sharding rebalance/failure-mode tests.
4. Add a persistent NativeSqlEngine mode to `compare_postgres_native_sql.py` so durable PostgreSQL comparison is fair.
5. Extend vector audit benchmark to large datasets and cold-cache reload once persistence is implemented.
