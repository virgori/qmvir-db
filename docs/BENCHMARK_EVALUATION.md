# QMvir Benchmark Evaluation Report

**Date:** 2026-03-08  
**System:** macOS, Apple Silicon (8 cores)  
**Test Environment:** Python 3.x, NumPy, Local-mode (no network)

---

## 1. Summary

This report documents comprehensive performance evaluation of QMvir database system across two benchmark categories:

1. **Gateway SQL Benchmark** — PostgreSQL wire-protocol comparison
2. **Local-Mode Vector Search Benchmark** — Native Python engine evaluation

### Key Findings (Updated 2026-03-13, v0.4.0)

| Aspect | Status | Notes |
|--------|--------|-------|
| **Benchmark v0.4.0 (8/9 wins)** | ✅ | Parquet + Chunk Pipeline, vs PG 16 + DuckDB 1.5 |
| Point Lookup (Rust) | ✅ **17,514 QPS** | 2.6x PG, 5.6x DuckDB |
| Aggregation SUM/COUNT | ✅ **6,031 QPS** | 4.4x PG, 2.6x DuckDB |
| GROUP BY | ✅ **13,227 QPS** | 2.1x PG, 3.5x DuckDB |
| JOIN 2-table | ✅ **15,607 QPS** | 1.2x PG, 5.9x DuckDB |
| JOIN 3-table | ✅ **14,874 QPS** | 2.7x PG, 9.2x DuckDB |
| Bulk INSERT | ✅ **22,661 QPS** | 3.2x PG, batch lock optimization |
| UPDATE | ✅ **14,153 QPS** | 1.8x PG |
| OLAP Full Scan | ✅ **9,529 QPS** | 47x PG, chunk-based pipeline |
| Range Scan | ⚠️ 2,728 QPS | PG wins (3,261) — only loss |
| Parquet COPY FROM | ✅ Working | Arrow 53, zstd/snap/lz4 |
| Stress 100K rows | ✅ Stable | Post-VACUUM +24% GROUP BY |
| Vector Search HNSW | ⚠️ 156 QPS (needs optimization) | Multi-threading broken |
| Multi-thread scaling | ❌ Negative (lock contention) | Critical blocker |

> **v0.4.0 changelog**: Parquet Integration + Chunk-based Pipeline (1024-row batches) + Parallel GROUP BY + Batch INSERT lock optimization.

---

## 2. Gateway SQL Benchmark (QMvir vs PostgreSQL)

### 2.1 Test Configuration (Historical — UPDATE Benchmark, 2026-03-08)

| Parameter | Value |
|-----------|-------|
| Dataset | `bench_accounts` table, 10,000 rows |
| Schema | `(id INT, balance INT)` |
| Workload | `UPDATE ... SET balance = balance + 1 WHERE id = ?` |
| Clients | 4 concurrent connections |
| Operations | 2,000 per client (8,000 total) |
| Mode | autocommit, single statement per op |

### 2.2 Results (Historical — 2026-03-08, Python Gateway UPDATE, SUPERSEDED)

| System | Ops | Elapsed (s) | TPS | Avg (ms) | P95 (ms) | P99 (ms) |
|--------|-----|-------------|-----|----------|----------|----------|
| **PostgreSQL 16** | 8,000 | 0.273 | **29,308** | 0.125 | 0.230 | 0.308 |
| **QMvir (Python path)** | 8,000 | 35.46 | **226** | 17.71 | 26.1 | 39.5 |

**Performance Gap:** ~129.9x (PostgreSQL faster)

> **⚠️ SUPERSEDED (2026-03-09):** This benchmark measured UPDATE via the old Python gateway path.
> The Rust NativeSqlEngine (`start_native()`) now bypasses Python entirely.
> **Current JOIN QPS: 5,713 (1.01x PostgreSQL).** See Section 2.5.

### 2.3 Analysis

The large performance gap is expected given the architectural differences:

1. **PostgreSQL**: Native C, decades of optimization, direct disk I/O
2. **QMvir**: Python interpreter, pgwire protocol layer, msgpack serialization

### 2.4 Limitations

- Standard `pgbench` transaction scripts failed on QMvir due to missing `BEGIN/COMMIT` support
- Only write-path (UPDATE) was testable; SELECT result fetching was inconsistent

### 2.5 Updated JOIN/SUM Benchmark (2026-03-09, Rust NativeSqlEngine)

After migrating to Rust `NativeSqlEngine` with B+Tree index fast-path:

**Profile:** `standard` (20K accounts, 2K products, 15K orders, 800 ops)

| Metric | PostgreSQL 16 | QMvir (Rust) | Speedup (QM/PG) |
|--------|--------------|--------------|-----------------|
| **JOIN QPS** | 5,633 | 5,713 | **1.01x** |
| **JOIN avg (ms)** | 0.177 | 0.175 | **1.01x** |
| **JOIN p95 (ms)** | 0.302 | 0.210 | **0.70x** (QM 30% lower) |
| **SUM QPS** | 3,370 | 2,890 | 0.86x |
| **SUM avg (ms)** | 0.297 | 0.346 | 0.86x |
| **SUM p95 (ms)** | 0.965 | 0.425 | **0.44x** (QM 56% lower) |
| **Stress (8 clients, 20s)** | 15,676 QPS | 18,879 QPS | **1.20x** |
| **Error rate** | 0.0000 | 0.0000 | — |

**Key improvements:**
- JOIN QPS: **226 → 5,713** (~25x vs old Python path, 39x vs previous Rust path)
- QM now **matches or exceeds PostgreSQL** on JOIN, and **wins on tail latency and stress throughput**
- Root causes fixed: missing B+Tree indexes, wrong column in SUM, wrong filter in BETWEEN

### 2.6 v0.4.0 Benchmark (2026-03-13, Parquet + Chunk Pipeline)

**Profile:** `quick` (10K accounts, 1K products, 50K orders)  
**New features**: Parquet Integration, Chunk-based Pipeline (1024-row batches), Parallel GROUP BY

| Benchmark | QMvir QPS | PostgreSQL QPS | DuckDB QPS | QM/PG | Winner |
|-----------|--------:|--------:|--------:|------:|--------|
| Point Lookup | 17,514 | 6,754 | 3,136 | 2.59x | **QMvir** |
| Range Scan | 2,728 | 3,261 | 2,368 | 0.84x | PostgreSQL |
| Aggregation | 6,031 | 1,376 | 2,348 | 4.38x | **QMvir** |
| GROUP BY | 13,227 | 6,190 | 3,731 | 2.14x | **QMvir** |
| JOIN 2-table | 15,607 | 12,782 | 2,645 | 1.22x | **QMvir** |
| JOIN 3-table | 14,874 | 5,489 | 1,617 | 2.71x | **QMvir** |
| Bulk INSERT | 22,661 | 6,977 | 1,947 | 3.25x | **QMvir** |
| UPDATE | 14,153 | 7,690 | 2,993 | 1.84x | **QMvir** |
| OLAP Full Scan | 9,529 | 203 | 2,667 | 46.89x | **QMvir** |

**Score: 8/9 wins** (vs v0.3.0: 4/6). Improvement from chunk-based pipeline + batch INSERT lock optimization.

---

## 3. Local-Mode Vector Search Benchmark

### 3.1 Test Configuration

| Parameter | Value |
|-----------|-------|
| Vectors | 10,000 |
| Dimensions | 128 |
| Queries | 1,000 |
| Top-K | 10 |
| ef_search | 50 |
| HNSW M | 16 |
| ef_construction | 200 |
| Metric | Cosine distance |

### 3.2 Results

| Operation | Throughput | Avg Latency | P95 Latency | Notes |
|-----------|------------|-------------|-------------|-------|
| HNSW Build | 59 ops/s | 17.08 ms | 31.84 ms | Sequential insert |
| HNSW Search | 156 QPS | 5.21 ms | 6.22 ms | recall@10=0.6434 |
| Brute-Force | 894 QPS | 1.12 ms | 1.24 ms | Exact baseline |
| XOR-Delta Compress | 16.0 MB/s | 0.032 ms | - | ratio=0.96x |

### 3.3 Multi-Threading Scaling

| Threads | QPS | Speedup vs 1T | Status |
|---------|-----|---------------|--------|
| 1 | 194 | 1.00x | Baseline |
| 2 | 104 | **0.53x** | ❌ Regression |
| 4 | 47 | **0.24x** | ❌ Severe contention |
| 8 | 41 | **0.21x** | ❌ Unusable |

### 3.4 Analysis

#### 3.4.1 HNSW vs Brute-Force

At 10K vectors, brute-force is 5.7x faster than HNSW. This is expected because:
- HNSW has graph traversal overhead
- HNSW benefits appear at 100K+ vectors
- NumPy vectorized operations are highly optimized

#### 3.4.2 Multi-Threading Issue

Root cause: **Global RWLock** in `HNSWIndex.search()`

```python
# Current implementation (qm_core/index/hnsw.py)
def search(self, query, ...):
    with self._rwlock.read():  # ← All readers serialize here
        return self._search_unlocked(query, ...)
```

Even though multiple readers should be able to proceed concurrently with a RWLock, the Python GIL combined with lock acquisition overhead creates severe contention.

#### 3.4.3 Low Recall

Recall@10 = 0.6434 is below acceptable threshold (typically 0.9+). Causes:
- `ef_search=50` may be too low for this graph density
- `M=16` may be insufficient for good graph connectivity
- Need parameter sweep to find optimal values

### 3.5 Recommendations

| Issue | Fix | Priority |
|-------|-----|----------|
| Lock contention | Implement lock-free RCU | P0 |
| Low recall | Increase ef_search to 100-200 | P0 |
| Slow build | Batch insert optimization | P1 |
| Python overhead | Cython/Rust migration | P1 |

---

## 4. Optimization Roadmap

Detailed roadmap is available in [PERFORMANCE_ROADMAP.md](PERFORMANCE_ROADMAP.md).

### Quick Wins (Week 1-2)

1. **Fix RWLock** → Lock-free reads with RCU pattern
2. **Batch distances** → Single NumPy call instead of loop
3. **Tune HNSW params** → ef_search=100, M=32

### Medium-Term (Week 3-6)

1. **Cython hot paths** → 5-10x speedup
2. **FAISS backend** → 100x+ speedup
3. **GPU (MLX)** → Apple Silicon acceleration

### Long-Term (Week 7+)

1. **Rust port** → Eliminate GIL
2. **Distributed search** → Horizontal scaling

---

## 5. Benchmark Scripts

### Gateway SQL Benchmark
```bash
python /tmp/qm_vs_pg_update_bench.py --target both
```

### Local Vector Search Benchmark
```bash
cd /Users/gengyang/Desktop/AI/QM
python benchmarks/local_vector_search_bench.py \
    --num-vectors 10000 \
    --dim 128 \
    --num-queries 1000 \
    --output /tmp/results.md
```

### Micro Runtime + Algorithm Benchmark (Vir vs C vs Python)
```bash
cd /Users/gengyang/Desktop/AI/QM
python benchmarks/microbench_runtime_vir_c_py.py --profile quick
```

Coverage in this suite:
- Large `for` loop
- Large `while` loop
- Integer arithmetic (`+`, `-`, `*`, `/`)
- Recursive Fibonacci
- Iterative Fibonacci
- Simple function call
- Function call with many parameters
- Basic string length + concat
- Array traversal
- Hashmap insert + lookup
- Small and medium file read
- Alloc/free style workload

Outputs:
- `benchmarks/MICROBENCH_PY_VS_C.json`
- `benchmarks/MICROBENCH_PY_VS_C.md` (contains Vir/C/Python table)

---

## 6. Raw Data

### Gateway SQL
```
[postgres]
ops=8000 clients=4 elapsed_s=0.2730
tps=29308.17 avg_ms=0.125 p95_ms=0.230 p99_ms=0.308 failures=0

[qmvir]
ops=8000 clients=4 elapsed_s=35.4607
tps=225.60 avg_ms=17.708 p95_ms=26.105 p99_ms=39.455 failures=0
```

### Local Vector Search
```
[1] HNSW Build: 59 ops/s, avg=17.084ms
[2] HNSW Search: 156 QPS, avg=5.21ms, recall@10=0.6434
[3] Brute-Force: 894 QPS, avg=1.118ms
[4] XOR-Delta: ratio=0.96x, 15.99 MB/s
[5] Multi-Thread 1T: 194 QPS
[5] Multi-Thread 2T: 104 QPS (0.53x)
[5] Multi-Thread 4T: 47 QPS (0.24x)
[5] Multi-Thread 8T: 41 QPS (0.21x)
```

---

## 7. Conclusion

QMvir demonstrates functional correctness but has significant performance gaps compared to production databases. The critical blocker is **multi-threading negative scaling** due to lock contention.

**Immediate Actions:**
1. Fix RWLock → RCU pattern (P0, 1 week)
2. Tune HNSW parameters (P0, 1 day)
3. Add `BEGIN/COMMIT` to pgwire for pgbench compatibility (P1, 1 week)

**Expected Outcome After Fixes:**
- Vector search: 5-10x improvement (800-1500 QPS)
- Multi-thread scaling: Linear up to 4-6 threads
- Recall@10: 0.95+ with proper parameters
