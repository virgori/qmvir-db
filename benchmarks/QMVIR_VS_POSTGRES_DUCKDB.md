# QMvir vs PostgreSQL vs DuckDB — Benchmark Report

**Date**: 2026-03-13 07:05:55  
**Profile**: `quick`  
**Platform**: macOS arm64 (Apple Silicon)  
**Total runtime**: 44.4s

## Dataset

| Table | Rows |
|-------|-----:|
| accounts | 10,000 |
| products | 1,000 |
| orders | 50,000 |

## Results — QPS (queries per second, higher is better)

| Benchmark | QMvir | PostgreSQL | DuckDB | Winner |
|-----------|--------:|--------:|--------:|--------|
| Point Lookup | 17,514 | 6,754 | 3,136 | **QMvir** |
| Range Scan | 2,728 | 3,261 | 2,368 | **PostgreSQL** |
| Aggregation (SUM/COUNT) | 6,031 | 1,376 | 2,348 | **QMvir** |
| GROUP BY | 13,227 | 6,190 | 3,731 | **QMvir** |
| JOIN 2-table | 15,607 | 12,782 | 2,645 | **QMvir** |
| JOIN 3-table | 14,874 | 5,489 | 1,617 | **QMvir** |
| Bulk INSERT | 22,661 | 6,977 | 1,947 | **QMvir** |
| UPDATE | 14,153 | 7,690 | 2,993 | **QMvir** |
| OLAP Full Scan | 9,529 | 203 | 2,667 | **QMvir** |

## Results — Latency p95 (ms, lower is better)

| Benchmark | QMvir | PostgreSQL | DuckDB | Winner |
|-----------|--------:|--------:|--------:|--------|
| Point Lookup | 0.076 | 0.350 | 0.771 | **QMvir** |
| Range Scan | 0.728 | 0.649 | 0.795 | **PostgreSQL** |
| Aggregation (SUM/COUNT) | 0.274 | 1.213 | 0.576 | **QMvir** |
| GROUP BY | 0.098 | 0.205 | 0.304 | **QMvir** |
| JOIN 2-table | 0.083 | 0.112 | 0.462 | **QMvir** |
| JOIN 3-table | 0.088 | 0.366 | 1.053 | **QMvir** |
| Bulk INSERT | 0.078 | 0.339 | 1.218 | **QMvir** |
| UPDATE | 0.112 | 0.283 | 0.810 | **QMvir** |
| OLAP Full Scan | 0.231 | 9.057 | 0.748 | **QMvir** |

## QMvir Speedup vs PostgreSQL (QPS ratio)

| Benchmark | QMvir/PostgreSQL | QMvir/DuckDB |
|-----------|:----------------:|:------------:|
| Point Lookup | 2.59x | 5.58x |
| Range Scan | 0.84x | 1.15x |
| Aggregation (SUM/COUNT) | 4.38x | 2.57x |
| GROUP BY | 2.14x | 3.54x |
| JOIN 2-table | 1.22x | 5.90x |
| JOIN 3-table | 2.71x | 9.20x |
| Bulk INSERT | 3.25x | 11.64x |
| UPDATE | 1.84x | 4.73x |
| OLAP Full Scan | 46.89x | 3.57x |

## Detailed Statistics

### Point Lookup

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 500 | 500 | 500 |
| qps | 17,514 | 6,754 | 3,136 |
| avg_ms | 0.057 | 0.148 | 0.318 |
| p50_ms | 0.049 | 0.064 | 0.197 |
| p95_ms | 0.076 | 0.350 | 0.771 |
| p99_ms | 0.125 | 2.196 | 1.519 |
| min_ms | 0.022 | 0.022 | 0.143 |
| max_ms | 1.182 | 5.180 | 2.338 |

### Range Scan

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 300 | 300 | 300 |
| qps | 2,728 | 3,261 | 2,368 |
| avg_ms | 0.366 | 0.306 | 0.422 |
| p50_ms | 0.330 | 0.263 | 0.376 |
| p95_ms | 0.728 | 0.649 | 0.795 |
| p99_ms | 1.386 | 1.231 | 1.428 |
| min_ms | 0.110 | 0.071 | 0.229 |
| max_ms | 1.803 | 2.264 | 2.128 |

### Aggregation (SUM/COUNT)

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 300 | 300 | 300 |
| qps | 6,031 | 1,376 | 2,348 |
| avg_ms | 0.166 | 0.726 | 0.426 |
| p50_ms | 0.140 | 0.676 | 0.367 |
| p95_ms | 0.274 | 1.213 | 0.576 |
| p99_ms | 0.738 | 2.257 | 1.694 |
| min_ms | 0.077 | 0.328 | 0.319 |
| max_ms | 1.066 | 3.298 | 2.743 |

### GROUP BY

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 200 | 200 | 200 |
| qps | 13,227 | 6,190 | 3,731 |
| avg_ms | 0.075 | 0.161 | 0.268 |
| p50_ms | 0.070 | 0.146 | 0.250 |
| p95_ms | 0.098 | 0.205 | 0.304 |
| p99_ms | 0.125 | 0.425 | 0.684 |
| min_ms | 0.051 | 0.134 | 0.215 |
| max_ms | 0.677 | 1.067 | 1.695 |

### JOIN 2-table

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 300 | 300 | 300 |
| qps | 15,607 | 12,782 | 2,645 |
| avg_ms | 0.064 | 0.078 | 0.378 |
| p50_ms | 0.059 | 0.048 | 0.336 |
| p95_ms | 0.083 | 0.112 | 0.462 |
| p99_ms | 0.099 | 0.157 | 1.445 |
| min_ms | 0.038 | 0.040 | 0.299 |
| max_ms | 0.891 | 4.486 | 2.095 |

### JOIN 3-table

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 300 | 300 | 300 |
| qps | 14,874 | 5,489 | 1,617 |
| avg_ms | 0.067 | 0.182 | 0.618 |
| p50_ms | 0.058 | 0.148 | 0.550 |
| p95_ms | 0.088 | 0.366 | 1.053 |
| p99_ms | 0.261 | 0.811 | 1.966 |
| min_ms | 0.042 | 0.133 | 0.472 |
| max_ms | 0.999 | 1.060 | 2.545 |

### Bulk INSERT

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 2,000 | 2,000 | 2,000 |
| qps | 22,661 | 6,977 | 1,947 |
| avg_ms | 0.044 | 0.143 | 0.513 |
| p50_ms | 0.035 | 0.086 | 0.375 |
| p95_ms | 0.078 | 0.339 | 1.218 |
| p99_ms | 0.162 | 1.343 | 2.602 |
| min_ms | 0.023 | 0.048 | 0.279 |
| max_ms | 0.835 | 6.201 | 6.679 |

### UPDATE

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 500 | 500 | 500 |
| qps | 14,153 | 7,690 | 2,993 |
| avg_ms | 0.070 | 0.130 | 0.334 |
| p50_ms | 0.056 | 0.085 | 0.245 |
| p95_ms | 0.112 | 0.283 | 0.810 |
| p99_ms | 0.195 | 0.670 | 1.648 |
| min_ms | 0.034 | 0.055 | 0.190 |
| max_ms | 1.595 | 2.556 | 4.024 |

### OLAP Full Scan

| Metric | QMvir | PostgreSQL | DuckDB |
|--------|------:|------:|------:|
| ops | 200 | 200 | 200 |
| qps | 9,529 | 203 | 2,667 |
| avg_ms | 0.105 | 4.919 | 0.375 |
| p50_ms | 0.080 | 3.885 | 0.302 |
| p95_ms | 0.231 | 9.057 | 0.748 |
| p99_ms | 0.723 | 13.263 | 1.697 |
| min_ms | 0.057 | 2.902 | 0.249 |
| max_ms | 1.190 | 17.460 | 2.462 |
