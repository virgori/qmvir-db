# QMvir vs PostgreSQL — Benchmark History

## Latest (v0.4.0, 2026-03-13) — 9 Benchmarks vs PG + DuckDB

See `benchmarks/QMVIR_VS_POSTGRES_DUCKDB.md` for full results.

| Benchmark | QMvir QPS | PostgreSQL QPS | DuckDB QPS | Winner |
|-----------|--------:|--------:|--------:|--------|
| Point Lookup | 17,514 | 6,754 | 3,136 | **QMvir** (2.6x PG) |
| Range Scan | 2,728 | 3,261 | 2,368 | **PostgreSQL** |
| Aggregation | 6,031 | 1,376 | 2,348 | **QMvir** (4.4x PG) |
| GROUP BY | 13,227 | 6,190 | 3,731 | **QMvir** (2.1x PG) |
| JOIN 2-table | 15,607 | 12,782 | 2,645 | **QMvir** (1.2x PG) |
| JOIN 3-table | 14,874 | 5,489 | 1,617 | **QMvir** (2.7x PG) |
| Bulk INSERT | 22,661 | 6,977 | 1,947 | **QMvir** (3.2x PG) |
| UPDATE | 14,153 | 7,690 | 2,993 | **QMvir** (1.8x PG) |
| OLAP Full Scan | 9,529 | 203 | 2,667 | **QMvir** (47x PG) |

**Score: 8/9 wins** (Parquet + Chunk Pipeline v0.4.0)

---

## Historical (v0.3.0, 2026-03-10) — JOIN/SUM only

Date: 2026-03-10 07:36:29 | Profile: `standard`

### Summary

| Metric | PostgreSQL | QMvir | Speedup (QM/PG) |
|---|---:|---:|---:|
| JOIN QPS | 8861 | 19870 | 2.24x |
| JOIN p95 (ms) | 0.136 | 0.068 | 0.50x |
| SUM QPS | 1308 | 679 | 0.52x |
| SUM p95 (ms) | 2.425 | 5.095 | 2.10x |

## Details

- PostgreSQL JOIN avg: 0.113 ms
- QMvir JOIN avg: 0.050 ms
- PostgreSQL SUM avg: 0.764 ms
- QMvir SUM avg: 1.473 ms

