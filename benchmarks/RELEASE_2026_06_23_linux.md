# QM Publish Benchmark — 2026_06_23

Generated: 2026-06-23T09:41:02Z
Mode: **full** | Runs: **5**

## Environment

- OS: Linux-6.12.90+deb13.1-arm64-aarch64-with-glibc2.41
- Python: 3.13.5
- Rust: rustc 1.96.0 (ac68faa20 2026-05-25)
- Build: release (default)

## Summary

- **OLTP vs PostgreSQL:** QM 16/16 wins (median of 5 runs)
- **Search vs PostgreSQL:** QM 8/8 wins
- **Search @ 10000 rows:** QM 6/8 wins
- **Search @ 100000 rows:** QM 5/8 wins
- **Crash recovery:** reopen+query p50 36.57524008303881 ms, row count ok=True
- **Concurrent OLTP:** 36305 ops/s (8 threads)
- **Mixed workload:** 906 ops/s (70% reads)

## OLTP vs PostgreSQL (median p50)

| Workload | QM p50 | PG p50 | Winner |
|---|---:|---:|---|
| count_indexed_equality | 0.005 ms | 0.337 ms | QM |
| delete_by_pk | 0.008 ms | 1.591 ms | QM |
| indexed_integer_equality | 0.007 ms | 0.307 ms | QM |
| indexed_string_equality_duplicate_heavy | 0.033 ms | 0.359 ms | QM |
| indexed_string_equality_unique | 0.006 ms | 0.321 ms | QM |
| insert | 0.009 ms | 1.532 ms | QM |
| predicate_range | 0.007 ms | 0.386 ms | QM |
| select_by_pk | 0.004 ms | 0.282 ms | QM |
| transaction_commit | 0.009 ms | 2.408 ms | QM |
| transaction_insert_1000_commit | 21.816 ms | 327.100 ms | QM |
| transaction_insert_100_commit | 1.650 ms | 37.704 ms | QM |
| transaction_insert_10_commit | 0.137 ms | 5.749 ms | QM |
| transaction_mixed_dml_100_commit | 1.439 ms | 39.319 ms | QM |
| transaction_rollback | 0.011 ms | 0.747 ms | QM |
| transaction_rollback_100 | 10.509 ms | 33.874 ms | QM |
| update_by_pk | 0.005 ms | 1.970 ms | QM |

## Search vs PostgreSQL (median p50)

| Workload | QM p50 | PG p50 | PG/QM | Winner |
|---|---:|---:|---:|---|
| json.insert_autocommit | 0.139 ms | 2.526 ms | 18.14x | QM |
| json.path_filter | 0.048 ms | 0.845 ms | 17.69x | QM |
| text.bm25_persistent_vs_fts | 0.169 ms | 1.239 ms | 7.34x | QM |
| text.fts_plainto_tsquery | 0.173 ms | 1.699 ms | 9.80x | QM |
| text.indexed_equality | 0.116 ms | 0.809 ms | 6.95x | QM |
| text.like_contains | 0.997 ms | 1.516 ms | 1.52x | QM |
| vector.cosine_top10_hnsw | 0.219 ms | 1.084 ms | 4.95x | QM |
| vector.l2_top10_hnsw | 0.194 ms | 0.693 ms | 3.56x | QM |

## Disclosure

- QM persistent WAL benches use engine-native sync policies; see per-section metadata.
- PostgreSQL uses `synchronous_commit=on` unless noted in section output.
- Vector search: QM uses exact sort unless HNSW wired; PG uses HNSW when pgvector is installed.
- FTS: QM inverted GIN (BMW); PostgreSQL `to_tsvector` GIN — not BM25 on PG side.

