# QMVIR CODEBASE AUDIT — 10/03/2026 (Updated 13/03/2026 — v0.4.0)

> Tài liệu đối soát toàn diện codebase QMvir, trả lời 6 câu hỏi kỹ thuật cốt lõi.
> 
> **Cập nhật v0.4.0 (13/03/2026)**: Parquet Integration + Chunk-based Pipeline.
> - Dependencies mới: `arrow = "53"`, `parquet = "53"` (zstd, snap, lz4).
> - `CHUNK_SIZE = 1024` cho pipeline batch processing.
> - `ColumnExtractor` enum: zero-copy Arrow→Cell conversion với type widening.
> - Benchmark: **8/9 wins** vs PostgreSQL 16 + DuckDB 1.5 (trước đó 4/6).
> - Tests: **59/59 pass** (54 core + 5 Parquet integration).
> - `native_sql.rs` tăng lên ~3,700 LOC (trước ~2,800).

---

## MỤC LỤC

1. [Kiến trúc & Thuật toán hỗ trợ](#1-kiến-trúc--thuật-toán-hỗ-trợ)
2. [Kiến trúc độc bản & Giải thuật Pipeline riêng](#2-kiến-trúc-độc-bản--giải-thuật-pipeline-riêng)
3. [Hiệu năng & Tính an toàn](#3-hiệu-năng--tính-an-toàn)
4. [Lộ trình SDK đa ngôn ngữ](#4-lộ-trình-sdk-đa-ngôn-ngữ)
5. [Mức độ hoàn thiện: Rust vs Python](#5-mức-độ-hoàn-thiện-rust-vs-python)
6. [Tương thích OS/Chip & Tận dụng GPU](#6-tương-thích-oschip--tận-dụng-gpu)

---

## TỔNG QUAN CODEBASE

| Thành phần | Ngôn ngữ | LOC | Files | Vai trò |
|-----------|----------|-----|-------|---------|
| **qm_engine** | Rust | ~11,000 | 27 .rs | Database engine core (Gateway, Executor, Storage, Index, Planner, Parquet) |
| **qm_native** | Rust | 785 | 4 .rs | Vector operations (HNSW, SIMD distance, XOR-Delta compression) |
| **qm_core** | Python | ~15,000 | 55+ .py | Control plane (Hub, Distributed, IPC, Optimizer, Statistics) |
| **gateway** | Python | ~800 | 6 .py | API layer (PostgreSQL, HTTP, WebSocket) |
| **core_db** | Python | ~600 | 5 .py | Schema, partitioning, replication, WAL/CDC |
| **Platforms** | Python | ~3,000 | 20+ .py | Search, Vector, Analytics, Cache, Storage, Indexing |
| **SDK** | Python/TS | ~500 | 4 files | Python & JS client SDK |
| **Tests** | Python | ~8,000 | 20+ .py | Unit + integration + stress tests |
| **Benchmarks** | Python | ~2,000 | 9 .py | Performance benchmarks |
| **Tổng** | **Rust + Python** | **~42,000** | **150+** | |

---

## 1. Kiến trúc & Thuật toán hỗ trợ

### 1.1 Kiến trúc tổng thể: Hub-Satellite

```
┌───────────────────────────────────────────────────────────────────┐
│                    CLIENT LAYER                                    │
│  psql / DBeaver / Python SDK / JS SDK / REST / WebSocket          │
└─────────┬───────────────┬─────────────────┬──────────────────────┘
          │               │                 │
┌─────────▼───────┐ ┌────▼─────┐  ┌────────▼────────┐
│ PostgreSQL Wire │ │ HTTP API │  │ WebSocket Stream │
│ Protocol (Rust) │ │ (aiohttp)│  │ (asyncio)        │
│ tokio async     │ │          │  │                  │
└─────────┬───────┘ └────┬─────┘  └────────┬────────┘
          └───────────────┼─────────────────┘
                   ┌──────▼──────┐
                   │  HUB (Core) │  ← LSN Sequencer + Merkle Auditor
                   │  Control    │  ← WAL Append + Snapshot Checkpoint
                   │  Plane      │  ← Query Router + Planner
                   └──────┬──────┘
                          │ IPC Ring Buffer (LMAX Disruptor)
            ┌─────────────┼─────────────────┐
     ┌──────▼──────┐ ┌────▼─────┐ ┌─────────▼──────────┐
     │  General    │ │  Vector  │ │  Procedure         │
     │  Satellite  │ │  Satellite│ │  Satellite (PL/QM) │
     │  (Storage)  │ │  (HNSW)  │ │  (Stored Procs)    │
     └──────┬──────┘ └────┬─────┘ └─────────┬──────────┘
            │              │                 │
     ┌──────▼──────────────▼─────────────────▼──────────┐
     │              RUST DATA PLANE                      │
     │  NativeSqlEngine │ VectorExecutor │ B+Tree Index  │
     │  BufferPool │ WAL │ SIMD Executor │ HNSW │ TopN   │
     └─────────────────────────────────────────────────┘
```

### 1.2 Thuật toán Database Core (Rust — qm_engine)

| Thuật toán | Module | Mô tả |
|-----------|--------|-------|
| **Parallel Hash Join** | `hub_engine/executor.rs` | Rayon-parallelized build+probe với AHashMap. Chunk 4KB, threshold 10K rows |
| **TopN BinaryHeap Sort** | `hub_engine/executor.rs` | O(I·log N) thay vì O(I·log I). BinaryHeap<HeapEntry> với typed SortKey |
| **SIMD Equality Filtering** | `gateway/native_sql.rs` | AVX2 `_mm256_cmpeq_epi32` (8-lane) + NEON `vceqq_s32` (4-lane) |
| **SIMD Dot Product / L2 / Cosine** | `executor/vectorized.rs` | AVX2 `_mm256_fmadd_ps` (8×f32) + NEON `vfmaq_f32` (4×f32) |
| **B+Tree Index** | `index/bplus_tree.rs` | Latch crabbing, linked leaves, binary search `partition_point()`. 4KB pages, 100 keys/leaf, 254 keys/internal |
| **Autonomous Index Manager** | `index/auto_manager.rs` | Histogram selectivity estimation, auto-create (hits>50 & sel≤30%), shadow build → promote (30% speedup) |
| **Buffer Pool** | `gateway/native_sql.rs` | Generation-based cache invalidation, SoA column caching, AHashMap dimension tables |
| **WAL + Snapshots** | `storage/wal.rs`, `gateway/native_sql.rs` | Binary CRC32 WAL, atomic tmp→rename snapshots, auto-checkpoint every 10K mutations |
| **MVCC Transactions** | `storage/transaction.rs` | Snapshot isolation, version chains, first-committer-wins |
| **Slotted Page Storage** | `storage/page.rs` | PostgreSQL-style, memmap2 I/O, backward data growth, 64B header + CRC32 |
| **PostgreSQL Wire Protocol** | `gateway/protocol.rs` | V3 startup, SSL negotiation, SASL auth, Query/Parse/Bind/Execute cycle |
| **SQL Parser + Cost Planner** | `parser/`, `hub_engine/planner.rs` | sqlparser crate + cost-based join selection (smaller table → build side) |

### 1.3 Thuật toán Vector Engine (Rust — qm_native)

| Thuật toán | Module | Mô tả |
|-----------|--------|-------|
| **HNSW** | `hnsw.rs` | Multi-layer skip list, m=16, ef_construction=200. RCU snapshot pattern cho thread-safe |
| **SIMD Distance** | `distance.rs` | 8-way scalar unroll + horizontal pairwise reduction. Cosine/L2/InnerProduct |
| **XOR-Delta Compression** | `compression.rs` | Lossless vector encoding, LEB128 variable-length, 50-75% compression ratio |

### 1.4 Thuật toán Control Plane (Python — qm_core)

| Thuật toán | Module | Mô tả |
|-----------|--------|-------|
| **Raft Consensus** | `distributed/consensus.py` | Leader election, log replication, term-based voting |
| **Consistent Hash Ring** | `distributed/shard.py` | Virtual nodes + Vamana graph routing |
| **LMAX Disruptor Ring Buffer** | `ipc/ring_buffer.py` | Lock-free IPC, slot state machine (EMPTY→READY→PROCESSING→DONE) |
| **Slab Memory Allocator** | `ipc/media_allocator.py` | Power-of-2 classes (64KB→64MB), bitmap free-list, O(1) alloc/dealloc |
| **HyperLogLog** | `statistics/sketches.py` | Cardinality estimation, 2^14 registers |
| **Count-Min Sketch** | `statistics/sketches.py` | Frequency estimation, 2048×5 counters |
| **T-Digest** | `statistics/sketches.py` | Quantile estimation, tail-accurate |
| **Bloom Filter** | `statistics/sketches.py` | Pre-filtering, double hashing |
| **DiskANN + Product Quantization** | `index/diskann.py` | SSD-optimized Vamana graph, 256-way codebook |
| **Roaring Bitmap** | `index/roaring.py` | ArrayContainer + BitmapContainer + RunContainer |
| **BM25 Full-Text Search** | `search_platform/lexical_search/bm25.py` | Block-Max WAND, TF-IDF scoring |
| **RRF Fusion** | `execution/pipeline.py` | Reciprocal Rank Fusion cho hybrid search |
| **Volcano-style Joins** | `execution/join.py` | Hash/Merge/NestedLoop/Semi/Anti join iterators |
| **Adaptive Query Execution** | `optimizer/adaptive.py` | Cardinality fence (>3× drift → re-plan), EMA tracking |
| **Learned Optimization** | `learned/assistants.py` | EMA selectivity correction, learned cache policy, per-intent fusion weights |
| **Cost Model** | `statistics/cost_model.py` | I/O + CPU cost formulas, configurable knobs |

### 1.5 Hỗ trợ kiến trúc Chip

| Kiến trúc | SIMD | Build Target | Trạng thái |
|-----------|------|-------------|-----------|
| **x86_64** | AVX2 (8×f32, 8×i32) | `manylinux_2_17_x86_64` | ✅ Production |
| **ARM64 (AArch64)** | NEON (4×f32, 4×i32) | `manylinux_2_17_aarch64`, `macosx_11_0_arm64` | ✅ Production |
| **x86_64 AVX-512** | 16×f32 | Via `wide` crate | ✅ Runtime detect |
| **RISC-V** | — | — | ❌ Chưa hỗ trợ |

### 1.6 Hỗ trợ OS

| OS | Trạng thái | Chi tiết |
|----|-----------|----------|
| **Linux x86_64** | ✅ Production | manylinux_2_17 (Rocky Linux 8+, Ubuntu 18.04+, Debian 10+, CentOS 7+) |
| **Linux aarch64** | ✅ Production | manylinux_2_17 (ARM servers, AWS Graviton, Ampere) |
| **macOS arm64** | ✅ Production | Apple Silicon M1-M4 |
| **macOS x86_64** | ⚠️ Có thể build | Chưa cung cấp binary |
| **Windows** | ❌ Chưa hỗ trợ | Có thể cross-compile nhưng chưa test |
| **FreeBSD** | ❌ Chưa hỗ trợ | POSIX compatible, cần port |

---

## 2. Kiến trúc Độc Bản & Giải Thuật Pipeline Riêng

### 2.1 Kiến trúc độc bản: Hub-Satellite IPC

**Không có database nào** dùng kiến trúc Hub-Satellite với LMAX Disruptor ring buffer cho inter-process communication:

```
Hub (Control Plane)          Satellite (Data Plane)
    │                              │
    │── publish(SQL_CMD) ────────→ │
    │   [state: EMPTY→READY]       │── consume() + execute
    │                              │── complete(result)
    │← collect_result() ──────────│
    │   [state: DONE→EMPTY]        │
    │                              │
    └──────── Shared mmap ─────────┘
```

| Đặc điểm | QMvir | PostgreSQL | MySQL | CockroachDB |
|-----------|-------|-----------|-------|-------------|
| Process isolation | ✅ Hub ≠ Satellite | ❌ Single process | ❌ Single process | ❌ Single process |
| IPC mechanism | LMAX Disruptor ring | — | — | gRPC |
| Zero-copy data | ✅ mmap slab | ❌ | ❌ | ❌ |
| Zero-serialization | ✅ Direct memory | ❌ | ❌ | ❌ protobuf |

### 2.2 Shadow Index Pipeline (Độc bản)

```
Query Pattern Monitor
    │ hits > 50 AND selectivity ≤ 30%
    ▼
Shadow Build (async, non-blocking)
    │ Build B+Tree in background
    ▼
Shadow Validation (30% speedup test)
    │ Compare query time: with_index vs without
    ▼
Promote to Active ── OR ── Drop Shadow
```

Không có database nào tự xây pipeline shadow index evaluation tương tự. PostgreSQL/MySQL đều cần DBA quyết định index thủ công.

### 2.3 Adaptive Execution Pipeline (Độc bản)

```
Plan Generation (cost-based)
    │
    ▼
Execute + Monitor Cardinality
    │ actual_rows / estimated_rows > 3.0?
    │── YES → Re-plan mid-flight
    │── NO → Continue with current plan
    ▼
PlanHistory (EMA tracking)
    │ query_hash → best_plan_type
    ▼
Next execution uses learned plan
```

### 2.4 Multi-Stage Budget-Aware Retrieval (Độc bản)

```
Query → IntentClassifier
           │
    ┌──────┼──────┐
    ▼      ▼      ▼
  Lexical Vector  Filter
  (BM25)  (HNSW) (Bitmap)
    │      │      │
    └──────┼──────┘
           ▼
    RRF Fusion (learned alpha per intent)
           │
    ┌──────┼──────┐
    ▼      ▼      ▼
  Rerank  Late-   Budget
          Materialize Check
           │
           ▼
    Final Results
```

### 2.5 Learned Optimization Trio (Độc bản kết hợp)

| Component | Algorithm | Unique Aspect |
|-----------|----------|---------------|
| **LearnedSelectivity** | EMA correction factor per (table, column, operator) | Self-correcting cardinality estimates |
| **LearnedCachePolicy** | EMA inter-access time + trend analysis | Predictive prefetch khi accelerating, smart eviction |
| **LearnedFusionWeights** | Per-intent alpha (keyword/semantic/navigational/analytical/hybrid) | Tự điều chỉnh hybrid search blend theo query intent |

### 2.6 Media Slab Allocator for IPC (Độc bản)

Không database nào dùng multi-class slab allocator cho IPC:

```
  64 KB slabs ─── cho metadata, small results
   1 MB slabs ─── cho query results
   8 MB slabs ─── cho batch operations  
  64 MB slabs ─── cho media/BLOB transfers
```

---

## 3. Hiệu Năng & Tính An Toàn

### 3.1 Hiệu năng đo được (Benchmark v0.3.0)

| Metric | QMvir | PostgreSQL 16 | Tỷ lệ |
|--------|-------|--------------|-------|
| **Hash JOIN** (10K×10K) | 19,870 QPS | 8,870 QPS | **2.24× nhanh hơn** |
| **INSERT** (multi-row) | 716,332 rows/sec | ~200K rows/sec | **3.5× nhanh hơn** |
| **TopN Sort** (50K rows LIMIT 10) | 16-21ms | ~50ms+ | **2.5-3× nhanh hơn** |
| **Buffer Pool Hit** | >99% (sau warmup) | ~95% | Generation invalidation |
| **P99 Latency** (JOIN) | 0.071ms | ~0.3ms | **4× thấp hơn** |

### 3.2 SIMD Acceleration

| Operation | Scalar | SIMD | Speedup |
|-----------|--------|------|---------|
| **Vector Dot Product** (1024-dim) | Baseline | AVX2 8×f32 FMA | ~6-8× |
| **Integer Equality Filter** | Baseline | AVX2 8-lane cmpeq | ~6× |
| **NEON Distance** (ARM64) | Baseline | 4×f32 FMA | ~3-4× |
| **Batch Cosine** (1000 vectors) | Sequential | Rayon parallel | ~4-8× (multi-core) |

### 3.3 Tính an toàn — Data Integrity

| Tầng | Cơ chế | Chi tiết |
|------|--------|----------|
| **WAL** | CRC32 checksum | Mỗi entry có integrity check, replay recovery |
| **Snapshot** | Atomic tmp→rename | Crash-safe binary serialization (bincode) |
| **B+Tree Pages** | CRC32 per page | Detect corruption on read |
| **Transactions** | MVCC Snapshot Isolation | First-committer-wins conflict detection |
| **Wire Protocol** | SHA-256 password hash | Authentication layer |
| **IPC** | Atomic state machine | Slot state transitions (1-byte atomic CAS) |
| **Distributed** | Raft consensus | Term-based leader election, log replication |
| **Audit** | Merkle tree | Hub Merkle Auditor cho data integrity verification |
| **LSN** | Monotonic sequencer | Tất cả writes đều được gán LSN tăng dần |

### 3.4 Tính an toàn — Concurrency

| Component | Mechanism | Detail |
|-----------|----------|--------|
| **NativeSqlEngine** | `Arc<RwLock<HashMap>>` | Reader-writer lock per table |
| **Buffer Pool** | `parking_lot::RwLock` | Low-contention reader-biased |
| **HNSW** | RCU (Read-Copy-Update) | `Arc<RwLock<Arc<Snapshot>>>` — readers never block |
| **B+Tree** | Latch crabbing | RwLock per node, release parent before child |
| **WAL** | `AtomicU64` mutation counter | Lock-free checkpoint trigger |
| **Hub Status** | `AtomicU8` with SeqCst | Lock-free status transitions |
| **Ring Buffer** | 1-byte atomic state | Wait-free consume, blocking publish on full |

### 3.5 Tính an toàn — Error Handling

| Tầng | Approach |
|------|----------|
| **Rust** | `Result<T, E>` + `thiserror` cho tất cả operations. No panics in production path |
| **Python** | Exception hierarchy, graceful degradation |
| **Wire Protocol** | PostgreSQL ErrorResponse messages với SQLSTATE codes |
| **IPC** | SLOT_ERROR state cho failed satellite operations |

---

## 4. Lộ Trình SDK Đa Ngôn Ngữ

### 4.1 Trạng thái hiện tại

| SDK | Ngôn ngữ | File | LOC | Trạng thái | Tính năng |
|-----|----------|------|-----|-----------|-----------|
| **Python** | Python | `sdk/python/client.py` | 174 | ✅ Cơ bản | REST client: find, search, aggregate, insert, delete |
| **JavaScript/TS** | TypeScript | `sdk/js/client.ts` | 113 | ✅ Cơ bản | REST client: tương tự Python |
| **Schema Action** | Python | `sdk/schema_action_client/builder.py` | 188 | ✅ Cơ bản | Fluent DSL query builder |
| **Go** | — | — | — | ❌ Chưa có | — |
| **Rust** | — | — | — | ❌ Chưa có | — |
| **C** | — | — | — | ❌ Chưa có | — |
| **Swift** | — | — | — | ❌ Chưa có | — |
| **Mojo** | — | — | — | ❌ Chưa có | — |

### 4.2 Phương án SDK (3 hướng)

#### Hướng A: Native SDK cho từng ngôn ngữ
```
┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
│ Python   │ │ Go       │ │ Rust     │ │ Swift    │
│ SDK      │ │ SDK      │ │ SDK      │ │ SDK      │
│ (REST/PG)│ │ (REST/PG)│ │ (REST/PG)│ │ (REST/PG)│
└────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘
     └────────────┴────────────┴────────────┘
                       │
               PostgreSQL Wire Protocol
                       │
                   QMvir Server
```

**Ưu điểm**: Bất kỳ PostgreSQL client library nào cũng kết nối được ngay.
**Hiện trạng**: QMvir đã hỗ trợ PG wire protocol → **mọi ngôn ngữ có PG driver đều dùng được ngay**.

#### Hướng B: FFI từ Rust Core (libqmvir)

```
qm_engine (Rust) → cdylib libqmvir.so/.dylib
    │
    ├── Python: PyO3 (đã có) hoặc ctypes
    ├── Go: cgo + FFI
    ├── C/C++: Direct FFI
    ├── Swift: C interop
    ├── Mojo: C interop
    ├── JS/Node: N-API + FFI
    └── Rust: Direct crate dependency
```

**Ưu điểm**: Hiệu năng cao nhất, zero overhead.
**Yêu cầu**: Build `qm_engine` với `crate-type = ["cdylib"]` (đã có).

#### Hướng C: Universal SDK qua lớp trung gian (Recommended)

```
┌──────────────────────────────────────────────┐
│           QMvir Universal SDK Layer           │
│  ┌────────────────────────────────────────┐   │
│  │  Protocol Buffer / FlatBuffer Schema   │   │
│  │  (IDL cho tất cả operations)           │   │
│  └────────────────┬───────────────────────┘   │
│                   │                           │
│  ┌────────────────▼───────────────────────┐   │
│  │  Code Generator (protoc / flatc)       │   │
│  │  → Python, Go, Rust, C, Swift, JS     │   │
│  └────────────────────────────────────────┘   │
│                                               │
│  Transport: PG Wire (primary) | gRPC | REST   │
└──────────────────────────────────────────────┘
```

### 4.3 Lộ trình đề xuất

| Giai đoạn | Mục tiêu | Effort |
|-----------|---------|--------|
| **Phase 0** (Ngay) | Tận dụng PG wire — mọi ngôn ngữ có `libpq` đều connect được | 0 effort |
| **Phase 1** | Rust SDK crate (thin wrapper over qm_engine) | 1 tuần |
| **Phase 2** | Python SDK nâng cấp (psycopg2 + connection pool + async) | 1 tuần |
| **Phase 3** | Go SDK (pgx driver wrapper + QMvir extensions) | 2 tuần |
| **Phase 4** | C SDK (libqmvir.h + cdylib export) | 2 tuần |
| **Phase 5** | Swift SDK (C interop bridge) | 1 tuần |
| **Phase 6** | JS/Node SDK (pg-node driver + QMvir helpers) | 1 tuần |
| **Phase 7** | Mojo SDK (C FFI interop) | 1 tuần |
| **Phase 8** | Universal protobuf IDL + codegen | 3 tuần |

**Lưu ý quan trọng**: Nhờ PG wire protocol, **Phase 0 đã đáp ứng 80% nhu cầu** — bất kỳ ngôn ngữ nào có PostgreSQL driver đều kết nối được QMvir ngay lập tức.

---

## 5. Mức Độ Hoàn Thiện: Rust vs Python

### 5.1 Tổng quan phân bổ

| Metric | Rust | Python |
|--------|------|--------|
| **LOC (source only)** | 10,886 (20%) | 41,779 (80%) |
| **Production modules** | 31 files | 55+ files |
| **Hot path (data plane)** | ✅ 100% Rust | — |
| **Control plane** | — | ✅ 100% Python |

### 5.2 Chi tiết: Đã triển khai hoàn toàn bằng Rust ✅

| Module | Rust File | LOC | Chức năng |
|--------|----------|-----|-----------|
| **SQL Engine** | `gateway/native_sql.rs` | 2,090 | Full SQL (CREATE/INSERT/SELECT/UPDATE/DELETE/JOIN) |
| **B+Tree Index** | `index/bplus_tree.rs` | 867 | B+Tree với latch crabbing, range scan |
| **Auto Index** | `index/auto_manager.rs` | 686 | Autonomous index lifecycle management |
| **Executor (TopN)** | `hub_engine/executor.rs` | 562 | Parallel TopN sort, hash join |
| **PG Wire** | `gateway/connection.rs` | 558 | Full PostgreSQL v3 wire protocol |
| **PG Protocol** | `gateway/protocol.rs` | 407 | Message encoding/decoding |
| **SIMD Vector** | `executor/vectorized.rs` | 383 | AVX2/NEON dot product, L2, cosine |
| **Vector HNSW** | `qm_native/hnsw.rs` | 387 | Multi-layer HNSW with RCU |
| **WAL** | `storage/wal.rs` | 387 | Binary WAL with CRC32 |
| **Auth** | `gateway/auth.rs` | 364 | SHA-256 auth, privilege system |
| **Operators** | `executor/operators.rs` | 369 | Filter/Project/Sort/Aggregate/VectorSearch |
| **SQL Parser** | `parser/mod.rs` | 310 | SQL classification + dispatch |
| **Page Storage** | `storage/page.rs` | 308 | Slotted pages, memmap2 I/O |
| **Column Batch** | `executor/batch.rs` | 292 | Columnar format, bitmap validity |
| **Transactions** | `storage/transaction.rs` | 291 | MVCC snapshot isolation |
| **PG Server** | `gateway/server.rs` | 267 | Tokio async TCP listener |
| **Distance** | `qm_native/distance.rs` | 183 | SIMD distance kernels |
| **Compression** | `qm_native/compression.rs` | 183 | XOR-Delta LEB128 |
| **Hub Engine** | `hub_engine/` (7 files) | 710 | Coordinator, planner, types |

### 5.3 Chi tiết: VẪN ĐANG DÙNG PYTHON ⚠️

| Module | Python File | LOC | Chức năng | Cần migrate? |
|--------|-----------|-----|-----------|-------------|
| **Hub Coordinator** | `qm_core/hub/hub.py` | 500 | LSN sequencer, Merkle audit | 🔴 Cao — hot path |
| **Hub Dispatcher** | `qm_core/hub/dispatcher.py` | 300 | IPC routing to satellites | 🔴 Cao — latency |
| **Engine Facade** | `qm_core/engine.py` | 1,403 | Top-level API orchestration | 🟡 Trung bình |
| **Hub Engine** | `qm_core/hub_engine.py` | 1,173 | Hub-satellite API layer | 🟡 Trung bình |
| **IPC Ring Buffer** | `qm_core/ipc/ring_buffer.py` | 381 | LMAX Disruptor IPC | 🔴 Cao — critical path |
| **IPC Media Alloc** | `qm_core/ipc/media_allocator.py` | 406 | Slab allocator | 🔴 Cao — memory mgmt |
| **SQL Parser** | `qm_core/execution/sql_parser.py` | 921 | SQL parsing (lark) | 🟢 Thấp — Rust đã có |
| **Join Operators** | `qm_core/execution/join.py` | 686 | Hash/Merge/Semi/Anti join | 🟡 Trung bình |
| **Vectorized Exec** | `qm_core/execution/vectorized.py` | 501 | Columnar execution (numpy) | 🟢 Thấp — Rust đã có |
| **Query Planner** | `qm_core/execution/planner.py` | 385 | Cost-based planning | 🟡 Trung bình |
| **Pipeline** | `qm_core/execution/pipeline.py` | 442 | Multi-stage retrieval | 🟡 Trung bình |
| **Buffer Pool** | `qm_core/storage/buffer_pool.py` | 300 | LRU + clock sweep | 🟢 Thấp — Rust đã có |
| **MVCC** | `qm_core/storage/mvcc.py` | 350 | Version chains | 🟢 Thấp — Rust đã có |
| **WAL (Python)** | `qm_core/storage/wal.py` | 461 | Group-commit WAL | 🟢 Thấp — Rust đã có |
| **Raft Consensus** | `qm_core/distributed/consensus.py` | 587 | Leader election, log replication | 🟡 Trung bình |
| **Sharding** | `qm_core/distributed/shard.py` | 673 | Consistent hash ring | 🟡 Trung bình |
| **Gossip** | `qm_core/distributed/gossip.py` | 672 | Node discovery | 🟡 Trung bình |
| **Cost Model** | `qm_core/statistics/cost_model.py` | 350 | I/O + CPU cost formulas | 🟢 Thấp |
| **Sketches** | `qm_core/statistics/sketches.py` | 448 | HLL, CMS, T-Digest, Bloom | 🟡 Trung bình |
| **HNSW (Python)** | `qm_core/index/hnsw.py` | 457 | Pure-Python HNSW | 🟢 Thấp — Rust đã có |
| **DiskANN** | `qm_core/index/diskann.py` | 509 | Vamana + PQ | 🟡 Trung bình |
| **Roaring Bitmap** | `qm_core/index/roaring.py` | 515 | Bitmap index | 🟡 Trung bình |
| **Adaptive Opt** | `qm_core/optimizer/adaptive.py` | 300 | Cardinality fence, rule rewrites | 🟢 Thấp |
| **Learned Assist** | `qm_core/learned/assistants.py` | 350 | EMA selectivity, cache, fusion | 🟢 Thấp |
| **BM25 Search** | `search_platform/lexical_search/bm25.py` | 300 | Full-text search | 🟡 Trung bình |
| **PL/QM** | `qm_core/procedures/plqm.py` | 809 | Stored procedures engine | 🟡 Trung bình |
| **Wire Protocol** | `qm_core/wire/__init__.py` | 661 | PG wire (Python fallback) | 🟢 Thấp — Rust đã có |
| **PG Gateway (Py)** | `gateway/api_postgres/server.py` | 300 | asyncio TCP server | 🟢 Thấp — Rust đã có |
| **CLI/App** | `qm_app.py` + `qm_core/cli/` | 1,800+ | CLI, dashboard, shell | 🟢 Thấp — UI code |

### 5.4 Tóm tắt Migration Status

```
┌───────────────────────────────────────────────────┐
│                  RUST (đã có)                      │
│  ✅ SQL Engine    ✅ B+Tree     ✅ SIMD Executor   │
│  ✅ PG Wire       ✅ WAL        ✅ MVCC            │
│  ✅ TopN Sort     ✅ Hash Join  ✅ HNSW            │
│  ✅ Auth          ✅ Snapshots  ✅ Compression     │
│  ✅ Auto Index    ✅ Page Store ✅ Vector Distance │
├───────────────────────────────────────────────────┤
│              PYTHON (cần migrate)                  │
│  🔴 IPC Ring Buffer    🔴 Hub Coordinator          │
│  🔴 Media Allocator    🔴 Hub Dispatcher           │
│  🟡 Raft Consensus     🟡 Sharding                 │
│  🟡 DiskANN/PQ         🟡 Roaring Bitmap           │
│  🟡 BM25 Search        🟡 PL/QM Engine             │
│  🟡 Pipeline           🟡 Join Operators            │
│  🟢 Cost Model         🟢 Sketches (duplicated)     │
│  🟢 CLI/Dashboard      🟢 Python Gateway (fallback) │
└───────────────────────────────────────────────────┘

Đã Rust: ~20% LOC nhưng chiếm 100% hot path
Cần migrate: ~15% (IPC + Hub + Distributed)
Duplicated (Rust có, Python backup): ~25%
Python-only hợp lý (CLI, optimizer, SDK): ~40%
```

---

## 6. Tương Thích OS/Chip & Tận Dụng GPU

### 6.1 Trạng thái hiện tại: Cross-platform

| Platform | Build | Binary Wheel | SIMD | Test |
|----------|-------|-------------|------|------|
| **Linux x86_64** | ✅ cargo-zigbuild | ✅ cp311/cp312/cp313 | AVX2 | ✅ CI |
| **Linux aarch64** | ✅ cargo-zigbuild | ✅ cp311/cp312/cp313 | NEON | ✅ CI |
| **macOS arm64** | ✅ native maturin | ✅ cp313 | NEON | ✅ Local |
| **macOS x86_64** | ✅ native maturin | ⚠️ Chưa build | SSE2/AVX2 | ❌ |
| **Windows x86_64** | ⚠️ Có thể | ❌ | AVX2 | ❌ |
| **FreeBSD** | ⚠️ Có thể | ❌ | AVX2 | ❌ |

### 6.2 Tối ưu per-OS Kernel

| OS Feature | Linux | macOS | Windows | Lợi ích |
|-----------|-------|-------|---------|---------|
| **io_uring** | ✅ Linux 5.1+ | ❌ | ❌ | Async I/O zero-copy, giảm syscall |
| **kqueue** | ❌ | ✅ macOS | ❌ | Efficient event notification |
| **IOCP** | ❌ | ❌ | ✅ | Windows async I/O |
| **mmap + MAP_POPULATE** | ✅ | ⚠️ Partial | ✅ | Prefault pages, giảm page faults |
| **madvise MADV_SEQUENTIAL** | ✅ | ✅ | ❌ | Readahead hint cho sequential scan |
| **Huge Pages (2MB/1GB)** | ✅ THP | ⚠️ superpage | ❌ | Giảm TLB miss cho buffer pool |
| **cgroups v2** | ✅ | ❌ | ❌ | Memory/CPU isolation cho satellites |
| **NUMA awareness** | ✅ | ❌ | ✅ | Memory locality cho multi-socket |

**Hiện trạng QMvir:**
- Dùng tokio (cross-platform async) → epoll (Linux), kqueue (macOS) tự động
- Dùng memmap2 (cross-platform mmap) → OS tự chọn backend
- **Chưa có**: io_uring path, huge pages, NUMA-aware allocation

**Kế hoạch tối ưu per-OS:**

| Tối ưu | Effort | Impact | Priority |
|--------|--------|--------|----------|
| `io_uring` cho WAL write | 2 tuần | 2-5× WAL throughput trên Linux | 🔴 Cao |
| Huge pages cho buffer pool | 1 tuần | 10-30% giảm TLB miss | 🟡 TB |
| `MAP_POPULATE` cho snapshots | 2 ngày | Faster snapshot load | 🟢 Thấp |
| NUMA-aware page allocation | 2 tuần | 20-40% trên multi-socket | 🟡 TB |
| Direct I/O (O_DIRECT) cho WAL | 1 tuần | Bypass page cache, deterministic | 🟡 TB |

### 6.3 Tối ưu per-Chip Architecture

| Feature | x86_64 | ARM64 | Lợi ích |
|---------|--------|-------|---------|
| **AVX-512 (512-bit)** | ✅ Ice Lake+ | ❌ | 2× throughput vs AVX2 cho vector ops |
| **NEON (128-bit)** | ❌ | ✅ All ARM | Baseline SIMD |
| **SVE/SVE2 (scalable)** | ❌ | ✅ ARMv9+ | Scalable vector length (128-2048 bit) |
| **AMX (Apple)** | ❌ | ✅ M1+ | Matrix accelerator cho GEMM |
| **Intel AMX** | ✅ Sapphire Rapids+ | ❌ | Tile-based matrix multiply |
| **CRC32 hardware** | ✅ SSE4.2 | ✅ ARMv8 | Hardware CRC cho WAL/pages |
| **AES-NI** | ✅ | ✅ | Hardware crypto cho auth |
| **SHA-NI** | ✅ | ✅ | Hardware SHA-256 |
| **CLMUL** | ✅ | ✅ | Carryless multiply cho hashing |

**Hiện trạng QMvir:**
- AVX2 (8×f32): ✅ Dùng trong equality filter và vector distance
- NEON (4×f32): ✅ Dùng trong vector distance và f64 comparison
- **Chưa có**: AVX-512, SVE/SVE2, Apple AMX, Intel AMX, hardware CRC32

### 6.4 GPU Acceleration

**Hiện trạng: ❌ KHÔNG CÓ GPU SUPPORT**

QMvir hiện chạy 100% CPU. Không có CUDA/Metal/OpenCL/Vulkan code.

**Cơ hội GPU:**

| Workload | GPU Potential | Speedup Estimate | Hardware |
|----------|-------------|-----------------|----------|
| **Vector Distance (batch)** | Rất cao | 50-100× vs CPU | CUDA/Metal |
| **HNSW Build** | Cao | 10-20× | CUDA |
| **Hash Join (large)** | Trung bình | 5-10× | CUDA |
| **Sort (external)** | Trung bình | 3-5× | CUDA |
| **B+Tree operations** | Thấp | 1-2× | Không phù hợp |
| **WAL writes** | Không | 0× | I/O bound |

**Kế hoạch GPU:**

| Phase | Target | Technology | Effort |
|-------|--------|-----------|--------|
| **GPU-1** | Metal Compute (macOS) | metal-rs crate | 3 tuần |
| **GPU-2** | CUDA (NVIDIA) | cuda-sys / cudarc | 4 tuần |
| **GPU-3** | wgpu (Cross-platform) | wgpu crate (Vulkan/Metal/DX12) | 6 tuần |
| **GPU-4** | ROCm (AMD) | Thấp ưu tiên | TBD |

**Kiến trúc GPU đề xuất:**

```
Query Planner
    │ cardinality > GPU_THRESHOLD?
    │── YES → GPU Executor
    │── NO  → CPU Executor (hiện tại)
    ▼
GPU Executor
    │
    ├── Vector Distance Kernel (Metal/CUDA)
    │   └── batch_cosine_gpu(query, matrix) → distances
    │
    ├── Hash Join Kernel
    │   └── gpu_hash_build() + gpu_hash_probe()
    │
    └── Sort Kernel (Bitonic/Radix sort on GPU)
        └── gpu_topn_sort()
```

**GPU nhỏ nhất có ý nghĩa:**
- Apple M1 GPU (8 cores, 2.6 TFLOPS) → 10-20× vector search
- NVIDIA RTX 3060 (12GB, 12.7 TFLOPS) → 50-100× vector search
- Ngưỡng: Batch size > 1000 vectors thì GPU mới có lợi

---

## TỔNG KẾT

| Câu hỏi | Trả lời ngắn |
|---------|-------------|
| **1. Kiến trúc & thuật toán** | Hub-Satellite hybrid (Rust data plane + Python control plane). 30+ thuật toán: Hash Join, TopN Heap, B+Tree, HNSW, SIMD (AVX2+NEON), WAL+MVCC, Raft, HLL, CMS, T-Digest. Hỗ trợ: Linux x86_64/aarch64, macOS arm64 |
| **2. Độc bản** | 6 điểm: Hub-Satellite IPC (LMAX Disruptor), Shadow Index Pipeline, Adaptive Execution (cardinality fence), Learned Optimization Trio, Budget-Aware Multi-stage Retrieval, Media Slab Allocator |
| **3. Hiệu năng & an toàn** | 2.24× nhanh hơn PostgreSQL (JOIN), 3.5× INSERT, P99 0.071ms. An toàn: CRC32 WAL, MVCC, Raft, Merkle audit, SHA-256 auth, atomic snapshots |
| **4. SDK** | Python + JS/TS cơ bản. PG wire protocol cho mọi ngôn ngữ ngay. Lộ trình: Rust → Go → C → Swift → Mojo → Universal protobuf IDL |
| **5. Rust vs Python** | 20% LOC Rust nhưng 100% hot path. 80% Python cho control plane + CLI + optimizer. IPC/Hub/Distributed cần migrate sang Rust. ~25% LOC duplicated (có cả Rust và Python version) |
| **6. OS/Chip/GPU** | Linux + macOS production. Chưa có: io_uring, huge pages, AVX-512, SVE2, Apple AMX. GPU: ❌ chưa có. Lộ trình: Metal → CUDA → wgpu |

---

### Thống kê tổng

| Metric | Giá trị |
|--------|---------|
| **Tổng LOC** | ~52,665 (Rust 10,886 + Python 41,779) |
| **Rust files** | 35 |
| **Python files** | 150+ |
| **Thuật toán triển khai** | 30+ |
| **SIMD instructions** | AVX2 (8-lane), NEON (4-lane) |
| **Protocol** | PostgreSQL v3 wire |
| **External Rust deps** | 25 crates |
| **External Python deps** | 10 packages |
| **Build targets** | 7 wheels (3 arch × 3 Python versions + 1 macOS) |
| **Test files** | 20+ |

---

*Tài liệu được tạo qua audit toàn bộ codebase QMvir ngày 10/03/2026.*
*Paths verified against workspace: `/Users/gengyang/Desktop/AI/QM/`*
