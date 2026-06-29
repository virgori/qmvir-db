# QM Publish Benchmark — smoke

Generated: 2026-06-23T09:39:15Z
Mode: **quick** | Runs: **1**

## Environment

- OS: macOS-26.5.1-arm64-arm-64bit-Mach-O
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build: release (default)

## Summary

- **Crash recovery:** reopen+query p50 1.4901660033501685 ms, row count ok=True
- **Concurrent OLTP:** 68607 ops/s (4 threads)
- **Mixed workload:** 7370 ops/s (70% reads)

## OLTP vs PostgreSQL (median p50)

| Workload | QM p50 | PG p50 | Winner |
|---|---:|---:|---|

## Search vs PostgreSQL (median p50)

| Workload | QM p50 | PG p50 | PG/QM | Winner |
|---|---:|---:|---:|---|

## Disclosure

- QM persistent WAL benches use engine-native sync policies; see per-section metadata.
- PostgreSQL uses `synchronous_commit=on` unless noted in section output.
- Vector search: QM uses exact sort unless HNSW wired; PG uses HNSW when pgvector is installed.
- FTS: QM inverted GIN (BMW); PostgreSQL `to_tsvector` GIN — not BM25 on PG side.

