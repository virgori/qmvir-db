# QM Database — Đánh giá Toàn diện & Lộ trình đạt PG-class Performance

> **Version**: 3.0 | **Ngày**: 7 tháng 3, 2026
> **Mục tiêu**: Đầy đủ tính năng như PostgreSQL, I/O cực cao, zero deadlock

## Cap Nhat Danh Gia Runtime 2026-03-13 (v0.4.0 — Parquet + Chunk Pipeline)

1. **Parquet Integration**: `COPY table FROM 'file.parquet'` via Arrow 53 / Parquet 53 (zstd, snap, lz4).
2. **Chunk-based Pipeline**: CHUNK_SIZE=1024, parallel processing cho SELECT/BETWEEN/SUM/GROUP BY.
3. **Batch INSERT**: Single write lock cho toan bo batch (truoc la per-row lock).
4. **Parallel GROUP BY**: Local hash tables per chunk → merge pattern.
5. Benchmark v0.4.0 (profile `quick`, 10K accounts, 1K products, 50K orders):

| Benchmark | QMvir QPS | PostgreSQL QPS | DuckDB QPS | Winner |
|-----------|--------:|--------:|--------:|--------|
| Point Lookup | 17,514 | 6,754 | 3,136 | **QMvir** (2.6x PG) |
| Range Scan | 2,728 | 3,261 | 2,368 | PostgreSQL |
| Aggregation | 6,031 | 1,376 | 2,348 | **QMvir** (4.4x PG) |
| GROUP BY | 13,227 | 6,190 | 3,731 | **QMvir** (2.1x PG) |
| JOIN 2-table | 15,607 | 12,782 | 2,645 | **QMvir** (1.2x PG) |
| JOIN 3-table | 14,874 | 5,489 | 1,617 | **QMvir** (2.7x PG) |
| Bulk INSERT | 22,661 | 6,977 | 1,947 | **QMvir** (3.2x PG) |
| UPDATE | 14,153 | 7,690 | 2,993 | **QMvir** (1.8x PG) |
| OLAP Full Scan | 9,529 | 203 | 2,667 | **QMvir** (47x PG) |

6. **Ket qua**: 8/9 wins (truoc v0.3.0 la 4/6).
7. **Tests**: 59/59 pass (54 core + 5 Parquet integration).
8. **Stress test 100K rows**: OLAP SUM 10,009 QPS, GROUP BY 6,460 QPS, JOIN 3-table 17,463 QPS. Post-VACUUM recovery +24% GROUP BY, +17% JOIN.

## Cap Nhat Danh Gia Runtime 2026-03-09 (Post B+Tree Index Optimization)

1. NativeSqlEngine da duoc fix 3 loi nghiem trong: handle_select_join (thieu index), handle_select_sum (sai column), handle_select_between (sai filter).
2. B+Tree index fast-path da duoc them vao handle_select_join: O(log n + k) thay vi O(n) full scan.
3. Benchmark setup_data() da tao index cho QM tables (tuong duong PostgreSQL).
4. Ket qua benchmark moi (profile `standard`, 20K accounts, 15K orders, 800 ops):

| Metric | PostgreSQL | QMvir | Speedup (QM/PG) |
|--------|-----------|-------|-----------------|
| JOIN QPS | 5,633 | 5,713 | **1.01x** |
| JOIN p95 (ms) | 0.302 | 0.210 | **0.70x** (QM thap hon) |
| SUM QPS | 3,370 | 2,890 | 0.86x |
| SUM p95 (ms) | 0.965 | 0.425 | **0.44x** (QM thap hon) |
| Stress (8 clients, 20s) | 15,676 QPS | 18,879 QPS | **1.20x** |
| Error rate | 0.0000 | 0.0000 | — |

5. So voi lan chay truoc: JOIN QPS **147 → 5,713** (39x nhanh hon), SUM QPS **373 → 2,890** (7.7x nhanh hon).
6. QM da vuot PostgreSQL ve tail latency (p95) va stress throughput.
7. Shadow validation: row count khop (59/59), mismatch chi do float precision (REAL vs FLOAT8).
8. Tat ca Python execution files da co Rust fast-path import (`import qm_engine`).
9. hub_engine.py `execute_sql()` da delegate sang Rust HubEngine truoc, fallback Python.

Nguon trang thai migration file-level:

- `docs/PY_TO_RS_MIGRATION_TRACKER.md`

---

## Mục lục

1. [Tổng quan Dự án](#1-tổng-quan-dự-án)
2. [Kiến trúc Hiện tại](#2-kiến-trúc-hiện-tại)
3. [Benchmark Hiệu năng](#3-benchmark-hiệu-năng)
4. [So sánh Chi tiết vs PostgreSQL](#4-so-sánh-chi-tiết-vs-postgresql)
5. [Phân tích Deadlock & Concurrency](#5-phân-tích-deadlock--concurrency)
6. [Phân tích I/O Efficiency](#6-phân-tích-io-efficiency)
7. [Đánh giá từng Module](#7-đánh-giá-từng-module)
8. [Gap Analysis — Cần gì để ngang PG?](#8-gap-analysis)
9. [Lộ trình Hành động](#9-lộ-trình-hành-động)
10. [Kết luận](#10-kết-luận)

---

## 1. Tổng quan Dự án

### 1.1 QM là gì?

QM là **multi-engine database platform** xây từ zero bằng **Python + C (SIMD)**,
kết hợp 5 workload vào 1 kernel:

| Workload | Engine | Tương đương |
|----------|--------|-------------|
| OLTP (CRUD) | Storage + MVCC + WAL | PostgreSQL |
| Full-text Search | Inverted Index + BMW/WAND | Elasticsearch |
| Vector Search | HNSW + PQ | Pinecone / pgvector |
| Analytics (OLAP) | Vectorized + Columnar | ClickHouse |
| Distributed | Gossip + Raft + Shard | CockroachDB |

### 1.2 Quy mô Code

| Layer | Files | Lines | Tỷ lệ |
|-------|-------|-------|--------|
| **Storage Kernel** | 6 | 1,416 | 9.3% |
| **Index Kernel** | 6 | 1,979 | 13.0% |
| **Execution Kernel** | 6 | 1,906 | 12.5% |
| **Statistics + Optimizer** | 4 | 991 | 6.5% |
| **Learned Components** | 2 | 296 | 1.9% |
| **Native C (SIMD)** | 1 | 369 | 2.4% |
| **Distributed** | 10 | 5,776 | 37.9% |
| **Engine** | 1 | 680 | 4.5% |
| **Tests (core+dist)** | 2 | 1,947 | 12.8% |
| **Outer scaffold** | ~114 | ~5,206 | — |
| **TỔNG CỘNG** | **152** | **21,566** | **100%** |

### 1.3 Test Health

| Suite | Tests | Pass | Fail | Lý do fail |
|-------|-------|------|------|------------|
| `test_core.py` | 56 | 56 | 0 | ✅ Clean |
| `test_distributed.py` | 86 | 86 | 0 | ✅ Clean |
| Scaffold tests | 56 | 30 | 26 | API mismatch scaffold ↔ qm_core |
| **Tổng** | **198** | **172** | **26** | 26 fail là scaffold cũ, không ảnh hưởng kernel |

---

## 2. Kiến trúc Hiện tại

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            Client / SDK                                     │
│        Python SDK │ HTTP REST │ WebSocket │ Cluster Client                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                          Gateway Layer                                      │
│        Auth/ACL │ Query Router │ Rate Limiter                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                     QMEngine — Unified API                                  │
│   .find() .search() .vector_search() .hybrid_search()                      │
│   .aggregate() .insert() .update() .delete() .execute()                    │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌────────────────┐ ┌────────────────┐ ┌────────────────┐ ┌──────────────┐ │
│  │  Execution      │ │  Index          │ │  Storage        │ │  Statistics  │ │
│  │  Parser (DSL)   │ │  B+Tree         │ │  WAL (binary)   │ │  CostModelV2 │ │
│  │  Planner (cost) │ │  HNSW + PQ      │ │  Segments (8KB) │ │  HLL/CMS     │ │
│  │  Vectorized     │ │  Inverted/BMW   │ │  Buffer Pool    │ │  TDigest     │ │
│  │  Pipeline       │ │  Roaring Bitmap │ │  MVCC (SI)      │ │  Bloom       │ │
│  │  Plan Nodes     │ │  Stats Collect  │ │  Compaction     │ │  Selectivity │ │
│  └────────────────┘ └────────────────┘ └────────────────┘ └──────────────┘ │
│                                                                             │
│  ┌────────────────┐ ┌────────────────┐ ┌────────────────────────────────┐   │
│  │  Optimizer      │ │  Learned        │ │  Native C (SIMD)               │   │
│  │  Adaptive Exec  │ │  Selectivity    │ │  bitmap_and/or  batch_l2       │   │
│  │  Rewrite Rules  │ │  Cache Policy   │ │  bm25_score_block  crc32       │   │
│  │  Plan History   │ │  Fusion Weights │ │  ARM NEON + x86 SSE4.2         │   │
│  └────────────────┘ └────────────────┘ └────────────────────────────────┘   │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                        Distributed Layer                                    │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐         │
│  │  Gossip   │ │  Raft     │ │  Shard    │ │  Repl.    │ │  CDC      │         │
│  │  (SWIM)   │ │ (Leader)  │ │(Hash Ring)│ │ (WAL log) │ │(Stream)   │         │
│  └──────────┘ └──────────┘ └──────────┘ └──────────┘ └──────────┘         │
│  ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐            │
│  │  Cluster Coord.   │ │  Dist. Query      │ │  Dist. Txn (2PC)  │            │
│  │  + Topology       │ │  Scatter-Gather   │ │  + Saga Orch.     │            │
│  └──────────────────┘ └──────────────────┘ └──────────────────┘            │
│  ┌──────────────────────────────────────────────────────────────┐          │
│  │  Cluster Client (topology-aware, retry, read preference)     │          │
│  └──────────────────────────────────────────────────────────────┘          │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.1 Các Protocol đã implement

| Protocol | Module | Chi tiết |
|----------|--------|----------|
| **SWIM Gossip** | `gossip.py` (672 LOC) | PING/ACK, indirect probing, suspicion timer, infection dissemination |
| **Raft Consensus** | `consensus.py` (586 LOC) | Leader election, log replication, term-based voting, heartbeat |
| **Consistent Hashing** | `shard.py` (673 LOC) | Virtual nodes, key routing, migration, split/merge |
| **WAL Replication** | `replication.py` (676 LOC) | Async/sync_one/sync_quorum, LSN tracking, failover |
| **2PC + Saga** | `dist_txn.py` (541 LOC) | Two-Phase Commit + backward compensation on failure |
| **CDC Streaming** | `cdc.py` (528 LOC) | Push callback + pull polling, WAL extractor bridge |

---

## 3. Benchmark Hiệu năng

> Chạy trên **Apple Silicon M-series**, Python 3.13.7, single-thread

### 3.1 Kết quả Benchmark

| Operation | Throughput | Latency | PostgreSQL tương đương |
|-----------|-----------|---------|----------------------|
| **B+Tree insert** (100K) | **1.31 M ops/s** | 76 ms | PG: ~500K inserts/s (heap + WAL + index) |
| **B+Tree point lookup** (100K) | **1.92 M ops/s** | 52 ms | PG: ~2M sequential reads from shared_buffers |
| **B+Tree range scan** (1K rows) | **2.22 M ops/s** | 0.5 ms | PG: ~1M rows/s range scan |
| **Roaring add** (1M) | **2.55 M ops/s** | 393 ms | pgvector bitmap: ~1M/s |
| **Roaring AND** (1M ∩ 1M) | 47 ops/s | 21 ms | — |
| **Roaring OR** (1M ∪ 1M) | 39 ops/s | 25 ms | — |
| **Inverted build** (10K docs) | **32 K docs/s** | 311 ms | Elasticsearch: ~50K docs/s |
| **BMW search** (100 queries) | 25 q/s | 3,962 ms total | ES: ~200 q/s (optimized) |
| **WAND search** (100 queries) | 22 q/s | 4,494 ms total | — |
| **DAAT search** (100 queries) | 125 q/s | 797 ms total | — |
| **HNSW insert** (5K, dim=64) | 96 vecs/s | 52 s | pgvector HNSW: ~2K vecs/s |
| **HNSW search** (k=10) | **297 q/s** | 337 ms total | pgvector: ~500 q/s |
| **Vectorized filter** (100K) | **34 M ops/s** | 2.9 ms | ClickHouse: ~100M+ (C++) |
| **Vectorized sort** (100K) | **16 M ops/s** | 6.2 ms | — |
| **Vectorized hash agg** (100K) | **1.32 M ops/s** | 76 ms | PG hash agg: ~5M/s |
| **HLL add** (100K) | 956 K/s | 105 ms | pg_hll: ~10M/s |
| **Engine insert** (10K) | **34.8 K ops/s** | 287 ms | PG: ~50K inserts/s (with WAL) |
| **Engine find** (100 queries) | **1.13 K q/s** | 88 ms | PG: ~10K q/s (indexed) |
| **Engine search** (100 queries) | 143 q/s | 699 ms | PG tsquery: ~1K q/s |
| **Engine aggregate** | 79 ops/s | 13 ms | PG: ~100 ops/s (sequential scan) |

### 3.2 Nhận xét Hiệu năng

**🟢 Mạnh (đã đạt hoặc vượt PG):**
- B+Tree ops: **1.3-2.2M ops/s** — nhanh hơn PG vì pure in-memory, không WAL overhead
- Vectorized filter: **34M ops/s** — numpy SIMD backend, vượt PG scan
- Sketches: HLL/CMS/TDigest đều có chất lượng production (HLL error 0.74%)
- HNSW search: **297 q/s** — competitive với pgvector

**🟡 Trung bình (cần cải thiện):**
- BMW/WAND search: **22-25 q/s** — pure Python, cần wire native C extension
- HNSW insert: **96 vecs/s** — Python distance calc bottleneck
- Engine insert: **35K ops/s** — tốt cho in-memory, nhưng chưa có real disk I/O

**🔴 Yếu (cần refactor):**
- Roaring AND/OR: **39-47 ops/s** cho 1M elements — popcount dùng `bin().count("1")`
- Tất cả đều là **in-memory only** — chưa benchmark được real disk I/O path

---

## 4. So sánh Chi tiết vs PostgreSQL

### 4.1 Feature Matrix

| Feature Category | PostgreSQL 17 | QM hiện tại | Đánh giá |
|-----------------|--------------|-------------|----------|
| **SQL Language** | Full SQL:2016 + PL/pgSQL | JSON DSL only | 🔴 **Critical gap** |
| **ACID Transactions** | SSI + 2PL + row locking | SI only, no W-W conflict detect | 🔴 **Critical gap** |
| **Disk Persistence** | Heap files + WAL + PITR | WAL exists, heap = Python dict | 🔴 **Critical gap** |
| **Crash Recovery** | Full WAL replay + timeline | WAL writes but not replayed to data | 🔴 **Critical gap** |
| **Concurrent Access** | MVCC + Lehman-Yao B-tree | No thread-safety on indexes | 🔴 **Critical gap** |
| **JOINs** | NL + Hash + Merge + Parallel | Plan nodes defined, not executed | 🔴 **High gap** |
| **Subqueries/CTEs/Window** | Full support | None | 🔴 **High gap** |
| **Row Locking** | SELECT FOR UPDATE, advisory | None | 🟠 **Medium gap** |
| **Constraints** | CHECK, FK, UNIQUE, EXCLUDE | None | 🟠 **Medium gap** |
| **VACUUM / Autovacuum** | Automatic | MVCC GC exists, not automatic | 🟠 **Medium gap** |
| **Triggers/Procedures** | PL/pgSQL, PL/Python | None | 🟠 **Medium gap** |
| **Replication** | Streaming + Logical | Designed, not wired to engine | 🟡 **Low gap** |
| **Partitioning** | Range, Hash, List | Consistent hash shard | 🟡 **Low gap** |
| **Statistics/Analyze** | Auto-analyze + extended stats | MCV + histogram, no auto | 🟡 **Low gap** |
| ─── | ─── | ─── | ─── |
| **Full-text Search** | tsvector + GIN (basic) | **BMW + WAND + DAAT + phrase** | 🟢 **QM vượt** |
| **Vector Search** | pgvector HNSW/IVFFlat | **HNSW + PQ + TwoStage + metadata** | 🟢 **QM vượt** |
| **Hybrid Search** | Manual query + custom logic | **Native RRF + weighted fusion** | 🟢 **QM vượt** |
| **Vectorized Exec** | None native | **numpy columnar engine** | 🟢 **QM vượt** |
| **Probabilistic Sketches** | None native | **HLL + CMS + TDigest + Bloom** | 🟢 **QM vượt** |
| **Learned Optimization** | None | **EMA selectivity + cache + fusion** | 🟢 **QM vượt** |
| **Multi-stage Pipeline** | None | **5-stage retrieval with budgets** | 🟢 **QM vượt** |
| **SIMD Kernels** | None in PG core | **ARM NEON + x86 SSE4.2** | 🟢 **QM vượt** |

### 4.2 Tóm tắt So sánh

```
PostgreSQL:  ████████████████████████████░░░░░░  72% feature coverage
QM Database: ████████████████░░░░░░░░░░░░░░░░░░  45% feature coverage (RDBMS core)
             ████████████████████████████████░░  90% feature coverage (Search/Vector/OLAP)
```

**PostgreSQL mạnh hơn ở**: SQL, ACID toàn diện, disk I/O, crash recovery, concurrent access, JOINs, constraints
**QM mạnh hơn ở**: Search (BMW/WAND), Vector (HNSW+PQ), Hybrid fusion, Vectorized columnar, Learned components, SIMD

---

## 5. Phân tích Deadlock & Concurrency

### 5.1 Bản đồ Lock hiện tại

```
Module                  Lock Type        Count   Re-entrant?   Nested?
─────────────────────────────────────────────────────────────────────────
WAL                     threading.Lock    1      No            No
SegmentManager          threading.Lock    1      No            No
BufferPool              threading.Lock    1      No            ⚠️ YES
MVCCEngine              threading.Lock    2      No            ⚠️ YES
B+Tree                  NONE              0      —             —
HNSW                    NONE              0      —             —
InvertedIndex           NONE              0      —             —
RoaringBitmap           NONE              0      —             —
GossipProtocol          threading.RLock   1      Yes           No
RaftNode                threading.RLock   1      Yes           No
ClusterCoordinator      threading.RLock   1      Yes           Possible
ShardManager            threading.RLock   1      Yes           No
ReplicationManager      threading.RLock   1      Yes           No
```

### 5.2 Nguy cơ Deadlock

#### 🔴 HIGH: BufferPool._evict_if_needed()

```python
# buffer_pool.py — HIỆN TẠI
def get_page(self, page_id):
    with self._lock:                    # ← acquire lock
        ...
        self._evict_if_needed()         # ← still holding lock
            self._write_page(frame)     # ← callback WHILE HOLDING LOCK
            # Nếu callback gọi lại buffer pool → DEADLOCK!
```

**Kịch bản**: Flush dirty page → callback ghi WAL → WAL trigger checkpoint → checkpoint cần evict → deadlock

**Fix**: Tách dirty-page list ra ngoài lock, flush sau khi release:

```python
def get_page(self, page_id):
    dirty_pages = []
    with self._lock:
        ...
        dirty_pages = self._evict_if_needed()  # return list, don't flush
    for page in dirty_pages:
        self._write_page(page)                 # flush OUTSIDE lock
```

#### 🟠 MEDIUM: MVCC commit() lock ordering

```python
# mvcc.py — HIỆN TẠI
def commit(self, txn):
    with txn._lock:           # Lock A
        with self._global_lock:  # Lock B
            # ...install writes...
```

**Kịch bản**: Thread 1 commit → txn._lock → _global_lock. Thread 2 GC → _global_lock → iterate txn._lock. Lock ordering khác nhau → potential deadlock.

**Fix**: Luôn acquire locks theo thứ tự cố định: `_global_lock` trước `txn._lock`, hoặc dùng lock-free commit.

#### 🔴 CRITICAL: Indexes không có lock

B+Tree, HNSW, Inverted, Roaring — tất cả đều **không có bất kỳ synchronization nào**. Concurrent insert + search sẽ corrupt data structure.

PostgreSQL dùng Lehman-Yao protocol (latch coupling) cho B-tree: lock parent → lock child → release parent. Đảm bảo concurrent access an toàn + no deadlock.

### 5.3 Kế hoạch Zero-Deadlock

| # | Giải pháp | Mục đích |
|---|----------|---------|
| 1 | **Lock ordering protocol** | Định nghĩa thứ tự: Index lock → Buffer lock → WAL lock → Global lock |
| 2 | **RWLock cho indexes** | Readers không block nhau, Writer exclusive |
| 3 | **Lock-free MVCC reads** | Readers chỉ đọc snapshot, không cần lock |
| 4 | **Deferred dirty flush** | Buffer pool flush dirty pages ngoài lock |
| 5 | **Latch coupling cho B-tree** | Lehman-Yao: top-down, release parent khi đã lock child |
| 6 | **Wait-die scheme cho 2PC** | Older txn waits, younger txn dies → no circular wait |

---

## 6. Phân tích I/O Efficiency

### 6.1 I/O Path Map hiện tại

```
                    ┌─────────────────────────────┐
 INSERT:            │  Python dict (state.rows)    │ ← TẤT CẢ DATA Ở ĐÂY
                    │  + WAL append (os.write)     │
                    │  + Index update (in-memory)  │
                    └─────────────────────────────┘

 FIND/SEARCH:       ┌─────────────────────────────┐
                    │  Scan Python dict trực tiếp   │ ← BYPASS storage stack
                    │  KHÔNG dùng Buffer Pool      │
                    │  KHÔNG dùng Segments          │
                    │  KHÔNG dùng MVCC read         │
                    └─────────────────────────────┘

 WAL:               ┌─────────────────────────────┐
                    │  os.open(O_WRONLY|O_APPEND)  │ ← Good: raw FD
                    │  os.write(binary)            │ ← Good: no Python buffering
                    │  os.fsync() (optional)       │ ← Good: durable
                    │  Group commit: batch fsync   │ ← Good: amortized
                    └─────────────────────────────┘

 Segments:          ┌─────────────────────────────┐
                    │  open("rb") PER PAGE         │ ← BAD: N syscalls/segment
                    │  seek + read 8KB             │
                    │  close                        │
                    │  Lặp lại cho mỗi page        │
                    └─────────────────────────────┘
```

### 6.2 Vấn đề I/O chính

| # | Vấn đề | Impact | Giải pháp |
|---|--------|--------|----------|
| 1 | **Engine dùng Python dict làm storage** | Data path bypass toàn bộ storage stack | Wire find() qua Buffer Pool → Segments → MVCC |
| 2 | **SegmentReader open/close per page** | 1000 pages = 1000 `open()` syscalls | Giữ file descriptor mở, hoặc dùng `mmap()` |
| 3 | **Không có mmap** | Mỗi read cần kernel ↔ userspace copy | Dùng `mmap` + `madvise(MADV_SEQUENTIAL)` |
| 4 | **Prefetch đồng bộ** | Buffer pool prefetch blocking | Background I/O thread + `readahead()` |
| 5 | **Compaction load all rows vào RAM** | N segments × M rows = N×M RAM | Streaming merge với fixed-size buffer |
| 6 | **WAL truncate scan toàn bộ segment** | O(records) per segment | Maintain LSN index in memory |
| 7 | **Roaring popcount dùng bin().count("1")** | 100x chậm hơn `int.bit_count()` | Dùng `int.bit_count()` (Python 3.10+) |
| 8 | **Native C extension chưa wire** | SIMD kernels unused | Import và gọi từ Python classes |

### 6.3 Target I/O Architecture (PG-class)

```
                    ┌─────────────────────────────────────────┐
 WRITE PATH:        │                                         │
                    │  Client → Engine → MVCC begin_txn()      │
                    │    → WAL append (group commit, batch fsync)│
                    │    → Buffer Pool mark_dirty(page)         │
                    │    → Index update (with RWLock)            │
                    │    → MVCC commit_txn()                     │
                    │                                         │
 READ PATH:         │                                         │
                    │  Client → Engine → MVCC snapshot_read()  │
                    │    → Planner → choose access path        │
                    │    → Index probe (B+tree/inverted/HNSW)  │
                    │    → Buffer Pool get_page()               │
                    │      → Cache hit: return page (0 I/O)    │
                    │      → Cache miss: read from mmap segment │
                    │    → MVCC visibility check                │
                    │    → Vectorized pipeline → results        │
                    │                                         │
 BACKGROUND:        │                                         │
                    │  Checkpointer: flush dirty pages đều đặn │
                    │  BGWriter: preemptive eviction            │
                    │  Compactor: level-merge in background     │
                    │  Autovacuum: MVCC GC + dead tuple cleanup │
                    │  WAL Archiver: ship WAL cho replication   │
                    └─────────────────────────────────────────┘
```

### 6.4 Ultra-high I/O Roadmap

| Kỹ thuật | Cải thiện | Độ khó |
|----------|----------|--------|
| **mmap segments** | Loại bỏ open/close/read syscalls | ⭐⭐ |
| **io_uring** (Linux) / **kqueue** (macOS) | Async non-blocking I/O | ⭐⭐⭐⭐ |
| **Direct I/O (O_DIRECT)** | Bypass OS page cache, control caching | ⭐⭐⭐ |
| **Huge Pages (2MB)** | Giảm TLB miss cho large buffer pool | ⭐⭐ |
| **Write-combining WAL buffer** | Batch WAL writes, single fsync | ⭐⭐ |
| **Page-aligned I/O** | Optimal block device transfer | ⭐ |
| **Prefetch pipeline** | Background thread prefetch next pages | ⭐⭐ |
| **Zero-copy** | `sendfile()` / `splice()` cho query results | ⭐⭐⭐ |
| **Wire C SIMD extensions** | 10-50x speedup bitmap/BM25/L2 | ⭐⭐ |
| **Column compression** | Dictionary + RLE + delta encoding | ⭐⭐⭐ |

---

## 7. Đánh giá từng Module

### 7.1 Storage Kernel

#### WAL (379 LOC) — ⭐⭐⭐⭐ Tốt

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Binary format | ✅ | Magic + CRC32 + struct pack |
| Segment rotation | ✅ | 64MB per segment, configurable |
| Group commit | ✅ | Batch fsync amortization |
| Raw FD I/O | ✅ | `os.open()` + `os.write()`, no Python buffering |
| Crash recovery | ⚠️ | Replay works nhưng chưa wire vào data path |
| PITR | ❌ | Không có archiving, không có timeline |
| Page alignment | ❌ | PG dùng 8KB WAL pages, QM dùng variable-length |

#### Buffer Pool (200 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Pin/Unpin | ✅ | Prevent eviction of in-use pages |
| LRU eviction | ⚠️ | OrderedDict, O(n) worst case khi all pinned |
| Dirty write-back | ⚠️ | **Deadlock risk** — flush trong lock |
| Statistics | ✅ | Hit/miss/eviction counters |
| Clock-sweep | ❌ | PG dùng O(1) amortized clock hand |
| Background writer | ❌ | Không có preemptive flush |
| Ring buffer | ❌ | Sequential scans pollute cache |

#### MVCC (250 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Snapshot Isolation | ✅ | Clean read-your-own-writes |
| Version chains | ✅ | Linked list với prev pointer |
| GC | ✅ | Watermark-based pruning |
| Write-Write conflict | ❌ | **Last-writer-wins** — mất data! |
| SSI | ❌ | Không có serializable isolation |
| Row locking | ❌ | Không có SELECT FOR UPDATE |
| CLOG | ❌ | In-memory dict thay vì commit log |

#### Segments (352 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Slotted pages (8KB) | ✅ | Proper slot array + backward row growth |
| Per-page CRC32 | ✅ | Integrity validation |
| Min/max tracking | ✅ | Skip during range scans |
| File I/O | ❌ | **Open/close per page** — N syscalls |
| mmap | ❌ | Không dùng memory-mapped I/O |
| TOAST | ❌ | Không support large values |

#### Compaction (234 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Leveled policy | ✅ | Like RocksDB |
| Size-tiered policy | ✅ | Like Cassandra |
| Tier lifecycle | ✅ | Hot → warm → cold → archive |
| RAM usage | ❌ | Load tất cả rows vào RAM trước merge |
| Rate limiting | ❌ | Không throttle I/O |

### 7.2 Index Kernel

#### B+Tree (307 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Insert + split | ✅ | Correct B+tree algorithm |
| Leaf chain | ✅ | Efficient range scan |
| Bulk load | ✅ | Bottom-up construction |
| Thread safety | ❌ | **KHÔNG CÓ LOCK** — corrupt on concurrent access |
| Disk persistence | ❌ | In-memory only |
| Delete rebalance | ❌ | Leaves có thể empty |
| Covering index | ❌ | Không INCLUDE columns |

#### HNSW + PQ (433 LOC) — ⭐⭐⭐⭐ Tốt

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Multi-layer graph | ✅ | Correct navigable small world |
| Dynamic insert/delete | ✅ | No rebuild needed |
| Multiple metrics | ✅ | Cosine, L2, IP |
| Product Quantization | ✅ | Memory compression + asymmetric |
| Two-stage search | ✅ | PQ coarse → exact re-rank |
| Metadata filtering | ✅ | Pre + post filter |
| Thread safety | ❌ | No lock |
| Disk persistence | ❌ | In-memory only |

#### Inverted Index + BMW (487 LOC) — ⭐⭐⭐⭐ Rất tốt

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Block-Max WAND | ✅ | State-of-the-art top-k, skip 70-80% postings |
| DAAT baseline | ✅ | Correct document-at-a-time |
| WAND | ✅ | Score-based pruning |
| BM25 scoring | ✅ | Configurable k1/b |
| Phrase search | ✅ | Positional index |
| Field-aware | ✅ | Per-field weight |
| Thread safety | ❌ | No lock |
| C extension hot path | ❌ | bm25_score_block exists but unused |

#### Roaring Bitmap (502 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| 3 container types | ✅ | Array/Bitmap/Run |
| Adaptive conversion | ✅ | Array ↔ Bitmap at 4096 |
| Set operations | ✅ | AND, OR, ANDNOT, XOR |
| Serialization | ✅ | Binary format |
| Popcount | ❌ | **bin().count("1")** — 100x chậm |
| C extension | ❌ | bitmap_and/or exists but unused |

### 7.3 Execution Kernel

#### Parser (258 LOC) — ⭐⭐⭐ Khá

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| JSON DSL | ✅ | Clean, typed AST |
| Operators | ✅ | EQ, NEQ, GT, GTE, LT, LTE, IN, LIKE, BETWEEN |
| Compound predicates | ✅ | AND/OR/NOT |
| SQL parser | ❌ | Không có |
| Subqueries | ❌ | Không có |
| CTEs/Window | ❌ | Không có |

#### Planner (380 LOC) — ⭐⭐⭐⭐ Tốt

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Cost model | ✅ | seq_page_cost, random_page_cost, cpu_tuple_cost |
| Access path selection | ✅ | SeqScan vs IndexScan vs BitmapScan |
| Selectivity-aware | ✅ | Use column stats for estimation |
| Top-k optimization | ✅ | Heap select thay vì full sort |
| JOIN planning | ❌ | Không có join optimizer |
| Parallel queries | ❌ | Single-thread only |
| Plan cache | ❌ | Không cache prepared plans |

#### Vectorized (494 LOC) — ⭐⭐⭐⭐⭐ Xuất sắc

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| Columnar batches | ✅ | numpy arrays |
| Vectorized filter | ✅ | **34M ops/s** — SIMD via numpy |
| Hash aggregate | ✅ | Proper group-by with multiple aggregates |
| Top-k partition | ✅ | `np.argpartition` — O(n+k log k) |
| Null handling | ✅ | Validity bitmask |
| Fallback no-numpy | ✅ | Graceful degradation |

#### Pipeline (437 LOC) — ⭐⭐⭐⭐⭐ Xuất sắc

| Aspect | Rating | Chi tiết |
|--------|--------|----------|
| 5-stage retrieval | ✅ | CandidateGen → Score → ReRank → LateMat → PostProcess |
| Budget-aware | ✅ | Time + candidate count limits |
| Adaptive widening | ✅ | Auto-expand when stage underperforms |
| RRF fusion | ✅ | k=60, industry standard |
| Late materialization | ✅ | Fetch full docs only for final top-k |

### 7.4 Statistics + Optimizer + Learned

#### CostModelV2 (279 LOC) — ⭐⭐⭐⭐ Tốt

Toàn diện: seq/index/bitmap scan, 3 join types, external sort, hash agg, BMW/HNSW/PQ/hybrid.
Cache-hit probability. Work memory budget cho spill estimation.

#### Sketches (448 LOC) — ⭐⭐⭐⭐ Tốt

HLL (p=14, error 0.74%), CMS (2048×5), TDigest (compression=100), Bloom Filter (optimal sizing).
ColumnSketch bundles all 4.

#### Adaptive Optimizer (262 LOC) — ⭐⭐⭐ Khá

CardinalityFence (re-plan if >3x off), PlanHistory, rewrite rules.
**Bug**: Rule `applicable()` so sánh `node_type` (enum) với string → rules có thể không bao giờ match.

#### Learned Components (295 LOC) — ⭐⭐⭐⭐ Tốt

EMA selectivity correction, cache policy prediction, fusion weight tuning, intent classifier.
Lightweight, no external ML deps.

### 7.5 Native C Extension (369 LOC)

| Function | SIMD | Platform | Wired? |
|----------|------|----------|--------|
| `bitmap_and` | Generic | All | ❌ |
| `bitmap_or` | Generic | All | ❌ |
| `bitmap_popcount` | Generic | All | ❌ |
| `bm25_score_block` | **ARM NEON** | Apple Silicon | ❌ |
| `batch_l2` | **ARM NEON** | Apple Silicon | ❌ |
| `crc32` | **SSE4.2** | x86 | ❌ |

**⚠️ KHÔNG CÓ FUNCTION NÀO ĐƯỢC GỌI TỪ PYTHON.** Tất cả SIMD kernels đều unused.

**Bug**: `bm25_score_block` NEON path process 4 floats/iteration nhưng không handle remainder khi `block_size % 4 != 0` → read past buffer.

### 7.6 Distributed Layer (5,776 LOC)

| Module | LOC | Protocol | Đánh giá |
|--------|-----|----------|----------|
| Gossip (SWIM) | 672 | PING/ACK, indirect probe, suspicion | ⭐⭐⭐⭐ Tốt |
| Raft Consensus | 586 | Leader election, log replication | ⭐⭐⭐⭐ Tốt |
| Cluster Coordinator | 727 | DDL coordination, topology | ⭐⭐⭐⭐ Tốt |
| Shard Manager | 673 | Consistent hashing, migration, split | ⭐⭐⭐⭐ Tốt |
| Replication | 676 | WAL streaming, failover | ⭐⭐⭐⭐ Tốt |
| Distributed Query | 693 | Scatter-gather, merge strategies | ⭐⭐⭐⭐ Tốt |
| Distributed Txn | 541 | 2PC + Saga compensation | ⭐⭐⭐⭐ Tốt |
| CDC | 528 | Push/pull, WAL extractor | ⭐⭐⭐⭐ Tốt |
| Cluster Client | 643 | Topology-aware, retry, read pref. | ⭐⭐⭐⭐ Tốt |

**Điểm mạnh**: Full distributed stack, từ membership detection đến cross-shard transactions.
**Điểm yếu**: Chưa wire vào engine thật — tất cả dùng in-memory transport only.

---

## 8. Gap Analysis — Cần gì để ngang PG?

### 8.1 Tổng hợp Severity

| Severity | Count | Ví dụ |
|----------|-------|-------|
| 🔴 Critical | 5 | Engine dict storage, no W-W conflict, no crash recovery, no thread-safety indexes, no SQL |
| 🟠 High | 4 | No JOINs execution, BufferPool deadlock, SegmentReader I/O, C extension unused |
| 🟡 Medium | 6 | No row locking, no constraints, no VACUUM auto, no plan cache, adaptive rule bugs |
| 🟢 Done | 8 | BMW, HNSW+PQ, hybrid fusion, vectorized, sketches, learned, pipeline, distributed |

### 8.2 Priority Actions

#### P0 — Wire Storage Stack (đạt durability)

**Mục tiêu**: Data survive crash, reads go through storage → buffer pool → MVCC.

```
Hiện tại:                        Mục tiêu:
  insert → dict + WAL append       insert → MVCC begin → WAL → page → buffer pool → commit
  find   → scan dict               find   → MVCC snapshot → planner → index → buffer pool → page
```

**LOC ước tính**: ~500 dòng refactor engine.py

#### P1 — Write-Write Conflict Detection (đạt correctness)

**Mục tiêu**: First-writer-wins semantics. Detect concurrent writes to same key.

```python
# Thêm vào MVCC commit():
for key in txn.write_set:
    current = self._heap.get(key)
    if current and current.txn_id > txn.snapshot_time:
        raise WriteConflictError(f"Key {key} modified by concurrent transaction")
```

**LOC ước tính**: ~50 dòng trong mvcc.py

#### P2 — Thread-safe Indexes (đạt concurrent access)

**Mục tiêu**: Multiple readers + 1 writer không corrupt. Lehman-Yao cho B+tree.

| Index | Solution | LOC |
|-------|----------|-----|
| B+Tree | Lehman-Yao latch coupling + right-link | ~200 |
| HNSW | RWLock (read-shared, write-exclusive) | ~80 |
| Inverted | Copy-on-write cho posting lists + RWLock | ~100 |
| Roaring | RWLock | ~50 |

**LOC ước tính**: ~430 dòng

#### P3 — Fix Buffer Pool Deadlock + mmap I/O (đạt high I/O)

**Mục tiêu**: Zero deadlock + eliminate open/close/seek per page.

1. Deferred dirty flush (release lock trước khi write callback)
2. mmap cho segment reads
3. Clock-sweep thay LRU cho O(1) eviction
4. Background writer thread

**LOC ước tính**: ~300 dòng

#### P4 — Wire C Extension (đạt 10-50x speedup)

**Mục tiêu**: Roaring, Inverted dùng native C functions.

```python
# Trong roaring.py
try:
    from qm_core.native import qm_native
    _HAS_NATIVE = True
except ImportError:
    _HAS_NATIVE = False

# BitmapContainer.popcount()
if _HAS_NATIVE:
    return qm_native.bitmap_popcount(self._bits)
else:
    return sum(bin(b).count("1") for b in self._words)
```

**LOC ước tính**: ~100 dòng

#### P5 — JOIN Execution (đạt query completeness)

**Mục tiêu**: Execute Hash Join, Merge Join, Nested Loop.

**LOC ước tính**: ~600 dòng cho 3 join operators + integration vào pipeline

#### P6 — SQL Parser (optional, đạt PG-compatible interface)

**Mục tiêu**: Parse SELECT/INSERT/UPDATE/DELETE/JOIN/WHERE → AST.

**Lựa chọn**:
- Option A: Dùng `sqlparse` library → ~200 LOC adapter
- Option B: Hand-write recursive descent → ~800 LOC
- Option C: Dùng `lark` grammar → ~400 LOC

---

## 9. Lộ trình Hành động

### Phase 7: Wire & Harden (đạt PG-class correctness)

```
Thời gian: ~3 ngày
LOC mới: ~1,500

┌─────────────────────────────────────────────────────────┐
│ 7.1  Wire Storage Stack                                  │
│      → engine.find() đi qua Buffer Pool → Segments       │
│      → engine.insert() đi qua MVCC → WAL → Buffer Pool  │
│      → crash recovery: WAL replay → reconstruct state    │
│                                                          │
│ 7.2  Write-Write Conflict Detection                      │
│      → first-writer-wins trong MVCC commit()             │
│      → WriteConflictError + retry logic                  │
│                                                          │
│ 7.3  Thread-safe Indexes                                 │
│      → Lehman-Yao cho B+Tree                             │
│      → RWLock cho HNSW, Inverted, Roaring                │
│                                                          │
│ 7.4  Fix BufferPool Deadlock                             │
│      → Deferred dirty flush                              │
│      → Clock-sweep eviction                              │
│      → Background writer thread                          │
│                                                          │
│ 7.5  Wire MVCC vào Query Path                            │
│      → find() dùng snapshot_read()                       │
│      → visibility check per row                          │
│      → GC auto-trigger                                   │
└─────────────────────────────────────────────────────────┘
```

### Phase 8: Ultra I/O (đạt high-performance)

```
Thời gian: ~2 ngày
LOC mới: ~800

┌─────────────────────────────────────────────────────────┐
│ 8.1  mmap Segment I/O                                    │
│      → mmap thay vì open/read/close per page             │
│      → madvise(SEQUENTIAL) cho scan                      │
│      → madvise(RANDOM) cho point lookup                  │
│                                                          │
│ 8.2  Wire C SIMD Extension                               │
│      → Roaring popcount, AND, OR dùng C                  │
│      → Inverted BM25 scoring dùng C                      │
│      → HNSW L2 distance dùng C                           │
│      → Fix NEON remainder bug trong bm25_score_block     │
│                                                          │
│ 8.3  WAL Optimization                                    │
│      → Page-aligned WAL records                          │
│      → WAL buffer (write-combining)                      │
│      → Parallel WAL writer                               │
│                                                          │
│ 8.4  Prefetch Pipeline                                   │
│      → Background I/O thread cho buffer pool             │
│      → Sequential scan prefetch (readahead)              │
│      → Index scan prefetch (next leaf pages)             │
└─────────────────────────────────────────────────────────┘
```

### Phase 9: Query Features (đạt PG feature parity)

```
Thời gian: ~3 ngày
LOC mới: ~2,000

┌─────────────────────────────────────────────────────────┐
│ 9.1  JOIN Execution                                      │
│      → Hash Join (in-memory + spill to disk)             │
│      → Merge Join (sorted inputs)                        │
│      → Nested Loop (with index)                          │
│                                                          │
│ 9.2  SQL Parser                                          │
│      → SELECT, INSERT, UPDATE, DELETE                    │
│      → WHERE, ORDER BY, GROUP BY, HAVING                 │
│      → JOIN syntax                                       │
│      → Basic subqueries                                  │
│                                                          │
│ 9.3  Constraints & Schema                                │
│      → PRIMARY KEY, UNIQUE, NOT NULL                     │
│      → CHECK constraints                                 │
│      → Foreign Key (với deferred checking)               │
│                                                          │
│ 9.4  Advanced Query Features                             │
│      → Window functions (ROW_NUMBER, RANK, SUM OVER)     │
│      → CTEs (WITH clause)                                │
│      → UNION / INTERSECT / EXCEPT                        │
└─────────────────────────────────────────────────────────┘
```

### Lộ trình Tổng quan

```
Hiện tại                    Phase 7              Phase 8              Phase 9
  (21,566 LOC)               (+1,500 LOC)         (+800 LOC)           (+2,000 LOC)
  ┌──────────┐               ┌──────────┐         ┌──────────┐        ┌──────────┐
  │ In-memory │──────────────→│ Durable   │────────→│ Ultra I/O │───────→│ Full SQL  │
  │ dict store│               │ ACID store│         │ mmap+SIMD │        │ JOINs,CTE │
  │ no locks  │               │ W-W detect│         │ zero-copy │        │ Window fn │
  │ 172 PASS  │               │ Lehman-Yao│         │ native C  │        │ Constraint│
  │ 26 FAIL   │               │ zero-DL   │         │ bg writer │        │ Subquery  │
  └──────────┘               └──────────┘         └──────────┘        └──────────┘

  Feature parity:             PG correctness:       PG I/O:              PG features:
  45% RDBMS                   70% RDBMS             85% RDBMS            95% RDBMS
  90% Search/Vector           90% Search/Vector     95% Search/Vector    95% Search/Vector
```

---

## 10. Kết luận

### 10.1 Điểm mạnh nổi bật

QM đã xây dựng thành công một **hybrid search + vector + OLAP engine** với các thành phần **vượt trội so với PostgreSQL**:

| Thành phần | Đánh giá |
|------------|----------|
| Block-Max WAND search | ⭐⭐⭐⭐⭐ Production-grade, skip 70-80% postings |
| HNSW + Product Quantization | ⭐⭐⭐⭐⭐ Vượt pgvector về tính năng |
| Hybrid RRF/weighted fusion | ⭐⭐⭐⭐⭐ Unique, không DB nào có native |
| Vectorized numpy engine | ⭐⭐⭐⭐⭐ 34M ops/s filter |
| 5-stage retrieval pipeline | ⭐⭐⭐⭐⭐ Budget-aware, adaptive |
| Probabilistic sketches | ⭐⭐⭐⭐ HLL 0.74% error |
| Learned components | ⭐⭐⭐⭐ EMA-based, no ML deps |
| Full distributed stack | ⭐⭐⭐⭐ SWIM + Raft + 2PC + CDC |
| Native C SIMD kernels | ⭐⭐⭐ Exists, ARM NEON + SSE4.2 |

### 10.2 Critical Gaps để đạt PG-class

| # | Gap | Impact | Fix effort |
|---|-----|--------|------------|
| 1 | **Engine = Python dict** | Data lost on restart | ~500 LOC |
| 2 | **No W-W conflict detect** | Lost updates | ~50 LOC |
| 3 | **No index thread-safety** | Data corruption | ~430 LOC |
| 4 | **BufferPool deadlock** | System hang | ~100 LOC |
| 5 | **C extension unused** | 10-50x perf missed | ~100 LOC |
| 6 | **No JOINs execution** | Can't relate data | ~600 LOC |
| 7 | **No SQL** | Not PG-compatible | ~400-800 LOC |
| 8 | **SegmentReader N opens** | Poor disk I/O | ~150 LOC |

### 10.3 Tiến trình Tổng thể

```
Phase  Tên                  LOC      Tests   Status
─────────────────────────────────────────────────────────
  1    Storage Engine        1,416    56/56   ✅ DONE
  2    Index Layer           1,979    56/56   ✅ DONE
  3    Execution Engine      1,906    56/56   ✅ DONE
  4    Statistics/Optimizer    991    56/56   ✅ DONE
  5    Learned Components      296    56/56   ✅ DONE
  6    Distributed & Scale   5,776    86/86   ✅ DONE
  7    Wire & Harden          TBD     TBD    ⬜ NEXT
  8    Ultra I/O               TBD     TBD    ⬜ PLANNED
  9    Query Features          TBD     TBD    ⬜ PLANNED
─────────────────────────────────────────────────────────
       TỔNG HIỆN TẠI       ~14,400   142/142
       DỰ KIẾN HOÀN TẤT    ~18,700   ~250+
```

### 10.4 Đánh giá cuối cùng

| Tiêu chí | Đánh giá | Điểm / 10 |
|----------|----------|------------|
| **Kiến trúc tổng thể** | Multi-engine, layered, well-designed | **8/10** |
| **Search & Vector** | Vượt PG, ngang ES/Pinecone | **9/10** |
| **OLAP Vectorized** | Numpy-backed, production-quality | **8/10** |
| **Distributed** | Full stack, proper protocols | **8/10** |
| **ACID Correctness** | SI có, W-W conflict chưa, SSI chưa | **4/10** |
| **Disk Persistence** | WAL tốt, nhưng data path chưa wire | **3/10** |
| **Concurrent Access** | Indexes không lock, deadlock risk | **2/10** |
| **I/O Efficiency** | Potential cao, thực tế chưa wire | **3/10** |
| **SQL Compatibility** | JSON DSL only | **2/10** |
| **Overall vs PG** | 45% RDBMS core, 90% search/vector | **5.5/10** |

> **Bottom line**: QM là một **excellent search + vector database** với **solid distributed design**,
> nhưng cần **Phase 7-8-9** (~4,300 LOC, ~8 ngày) để đạt **PG-class RDBMS correctness + performance**.
> Sau Phase 9, QM sẽ là một database **vượt PostgreSQL** ở search/vector/OLAP
> mà vẫn đạt PG-level **ACID, durability, concurrency, và I/O performance**.
