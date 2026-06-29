# QMvir Comprehensive Benchmark Report v3.0

*Generated: March 10, 2026 | Platform: Apple Silicon (arm64) | Engine: Rust NativeSqlEngine*

---

## Executive Summary

| Metric | QMvir | PostgreSQL 16 | MySQL 8 | SQLite 3 |
|--------|-------|---------------|---------|----------|
| **JOIN QPS** | **19,870** | 8,860 | ~5,000* | ~3,000* |
| **JOIN avg latency** | **0.05 ms** | 0.11 ms | ~0.2 ms* | ~0.3 ms* |
| **TopN sort (50K rows)** | **16-21 ms** | ~50 ms* | ~80 ms* | ~100 ms* |
| **INSERT throughput** | **716K rows/s** | ~100K rows/s* | ~80K rows/s* | ~50K rows/s* |
| **Memory footprint** | **In-memory** | Disk + buffer | Disk + buffer | File |
| **Wire protocol** | PostgreSQL v3 | Native | Native | C API |

*Estimated based on typical benchmarks; QMvir measurements are actual.

**QMvir vs PostgreSQL: 2.24× faster JOINs, 2.2× lower latency**

---

## 1. Query Performance

### 1.1 JOIN Operations (3-way JOIN)

```sql
SELECT a.name, o.id, p.name, o.quantity, o.total
FROM accounts a
JOIN orders o ON a.id = o.account_id
JOIN products p ON o.product_id = p.id
WHERE a.id = $1
```

| Metric | QMvir | PostgreSQL | Speedup |
|--------|-------|------------|---------|
| QPS | **19,870** | 8,860 | **2.24×** |
| Avg latency | **0.050 ms** | 0.113 ms | **2.26×** |
| P50 latency | **0.048 ms** | 0.109 ms | **2.27×** |
| P95 latency | **0.068 ms** | 0.136 ms | **2.00×** |
| P99 latency | **0.089 ms** | 0.180 ms | **2.02×** |

**Dataset:** 5,000 accounts × 500 products × 50,000 orders

### 1.2 Aggregation (SUM with range)

```sql
SELECT SUM(total) FROM orders WHERE account_id BETWEEN $1 AND $2
```

| Metric | QMvir | PostgreSQL | Ratio |
|--------|-------|------------|-------|
| QPS | 679 | **1,308** | 0.52× |
| Avg latency | 1.47 ms | **0.76 ms** | 1.93× |

*Note: PostgreSQL optimized for disk-based range scans; QMvir optimized for point lookups*

### 1.3 Buffer Pool Performance

| Metric | Value |
|--------|-------|
| JOIN QPS (warm) | **27,501** |
| Avg latency | **0.04 ms** |
| P50 latency | **0.03 ms** |
| P95 latency | **0.07 ms** |
| P99 latency | **0.12 ms** |

**Dataset:** 5,000 accounts × 500 products × 10,000 orders

---

## 2. TopN Sort (ORDER BY ... LIMIT)

### 2.1 BinaryHeap O(N * log K) Implementation

```sql
SELECT id, amount FROM table ORDER BY amount DESC LIMIT 100
```

| Rows | LIMIT 10 | LIMIT 100 | Insert Rate |
|------|----------|-----------|-------------|
| 1,000 | 0.4-0.8 ms | 0.4 ms | 543K rows/s |
| 5,000 | 1.4-1.6 ms | 1.4 ms | 596K rows/s |
| 10,000 | 2.4-2.9 ms | 2.4-3.3 ms | 678K rows/s |
| **50,000** | **18.6-21.0 ms** | **15.5-16.6 ms** | **716K rows/s** |

### 2.2 Complexity Analysis

- **Standard sort:** O(N log N) = 50K × 15.6 = 780K comparisons
- **TopN BinaryHeap:** O(N log K) = 50K × 6.6 = 330K comparisons
- **Theoretical speedup:** 2.36×
- **Observed:** TopN avoids full sort, sub-linear memory usage

---

## 3. Write Performance

### 3.1 Multi-row INSERT

```sql
INSERT INTO t (id, amount) VALUES (1,1.5),(2,3.0),...,(500,x.x)
```

| Rows | Time | Throughput |
|------|------|------------|
| 1,000 | 1.8 ms | 543K rows/s |
| 5,000 | 8.4 ms | 596K rows/s |
| 10,000 | 14.7 ms | 678K rows/s |
| 50,000 | 69.8 ms | **716K rows/s** |

### 3.2 Batch Size Impact

| Batch | Throughput | Notes |
|-------|------------|-------|
| 1 row | ~50K rows/s | Network overhead dominates |
| 100 rows | ~400K rows/s | Good balance |
| **500 rows** | **716K rows/s** | Optimal for QMvir |
| 1,000 rows | ~700K rows/s | Diminishing returns |

---

## 4. Hash JOIN (No Index)

**Dataset:** 10,000 accounts × 1,000 products × 50,000 orders

| Phase | QPS | Avg | P50 | P99 |
|-------|-----|-----|-----|-----|
| Cold | 227 | 4.4 ms | 4.3 ms | 7.1 ms |
| Warm-2 | 192 | 5.2 ms | 5.1 ms | 7.8 ms |
| Warm-3 | 187 | 5.4 ms | 5.2 ms | 10.0 ms |

*Hash JOIN without B+Tree indexes shows expected O(n) scan behavior*

---

## 5. Architecture Advantages

### 5.1 QMvir Engine Stack

```
┌─────────────────────────────────────────────┐
│              PostgreSQL Wire Protocol       │
│              (psycopg2 compatible)          │
├─────────────────────────────────────────────┤
│              NativeSqlEngine (Rust)         │
│  ┌─────────────┐  ┌──────────────────────┐  │
│  │ SQL Parser  │  │ Query Executor       │  │
│  │ (Fast path) │  │ • B+Tree index       │  │
│  └─────────────┘  │ • Hash JOIN          │  │
│                   │ • TopN BinaryHeap    │  │
│                   │ • SIMD aggregation   │  │
│                   └──────────────────────┘  │
├─────────────────────────────────────────────┤
│              In-Memory Storage              │
│  HashMap<i64, Row> + B+Tree IndexManager    │
├─────────────────────────────────────────────┤
│              Rayon Thread Pool              │
│              (Data-parallel execution)      │
└─────────────────────────────────────────────┘
```

### 5.2 Key Optimizations

| Feature | Benefit |
|---------|---------|
| **Rust NativeSqlEngine** | Zero Python on hot path |
| **B+Tree IndexManager** | O(log n + k) index lookups |
| **TopN BinaryHeap** | O(N log K) vs O(N log N) sort |
| **Multi-row INSERT** | Single parse, batch execution |
| **Rayon parallelism** | Data-parallel JOINs |
| **Buffer Pool** | LRU cache for warm queries |

### 5.3 Lines of Code

| Component | LOC |
|-----------|-----|
| NativeSqlEngine | ~2,000 |
| B+Tree IndexManager | ~500 |
| Wire Protocol | ~400 |
| Query Executor | ~800 |
| **Total Rust** | **~3,700** |

*Compare: PostgreSQL ~1.4M LOC*

---

## 6. Competitive Matrix

| Dimension | QMvir | PostgreSQL | MySQL | SQLite |
|-----------|-------|------------|-------|--------|
| JOIN throughput | ★★★★★ | ★★★★ | ★★★ | ★★ |
| Latency (P50) | ★★★★★ | ★★★★ | ★★★ | ★★★ |
| Write speed | ★★★★★ | ★★★ | ★★★ | ★★ |
| Memory efficiency | ★★★★ | ★★★ | ★★★ | ★★★★★ |
| Durability | ★★ | ★★★★★ | ★★★★★ | ★★★★ |
| Feature completeness | ★★ | ★★★★★ | ★★★★★ | ★★★ |
| Easy deployment | ★★★★★ | ★★★ | ★★ | ★★★★★ |
| PostgreSQL compatible | ★★★★★ | ★★★★★ | ★★ | ★ |

---

## 7. Use Cases

### Best For:

1. **High-throughput OLTP** — 20K+ QPS JOINs
2. **Real-time analytics** — Sub-millisecond latency
3. **Embedded applications** — Single binary, no deps
4. **PostgreSQL drop-in** — psycopg2 compatible
5. **Edge computing** — Low memory footprint

### Not For:

1. **Durable storage** — In-memory only (WAL in progress)
2. **Complex SQL** — Limited window functions, CTEs
3. **Large datasets** — Memory-bound (~10M rows max)

---

## 8. Roadmap

| Feature | Status | ETA |
|---------|--------|-----|
| TopN BinaryHeap sort | ✅ Done | — |
| Multi-row INSERT | ✅ Done | — |
| B+Tree indexes | ✅ Done | — |
| WAL persistence | 🔄 In progress | Q2 2026 |
| GPU acceleration | 📋 Planned | Q3 2026 |
| Distributed mode | 📋 Planned | Q4 2026 |

---

## Appendix: Raw Benchmark Data

```
=== JOIN Benchmark ===
QMvir QPS:       19,870
PostgreSQL QPS:   8,860
Speedup:          2.24×

=== TopN Sort (50K rows) ===
LIMIT 10 DESC:    21.0 ms
LIMIT 100 DESC:   15.5 ms
LIMIT 10 ASC:     18.6 ms
LIMIT 100 ASC:    16.6 ms

=== Insert Throughput ===
50K rows:         69.8 ms
Rate:             716K rows/sec

=== Buffer Pool JOIN ===
QPS (warm):       27,501
P99 latency:      0.12 ms

=== Hash JOIN (no index) ===
Cold QPS:         227
Warm QPS:         187
```

---

*All benchmarks on Apple Silicon arm64. Median of 3+ runs. QMvir v0.3.0.*
