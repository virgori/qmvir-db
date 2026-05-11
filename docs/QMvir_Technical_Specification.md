# QMvir — Đặc tả Kĩ thuật Toàn diện

> **Phiên bản:** 1.0.0 · **Ngày:** 2026-03-08 · **Tác giả:** QMvir Engineering  
> **Quy mô:** 201 file Python · ~40 000 dòng mã nguồn · 143 bài kiểm thử tự động

---

## Mục lục

1. [Tổng quan Kiến trúc](#1-tổng-quan-kiến-trúc)
2. [Control Plane — Hub](#2-control-plane--hub)
3. [Data Plane — Satellite](#3-data-plane--satellite)
4. [IPC — SharedRingBuffer & Media Slab](#4-ipc--shareringbuffer--media-slab)
5. [QM-SQL — Phương ngữ SQL lai](#5-qm-sql--phương-ngữ-sql-lai)
6. [Storage Kernel](#6-storage-kernel)
7. [Index Kernel](#7-index-kernel)
8. [Execution Kernel](#8-execution-kernel)
9. [Checkpoint & Recovery](#9-checkpoint--recovery)
10. [RBAC & Wire Protocol](#10-rbac--wire-protocol)
11. [CLI Ecosystem](#11-cli-ecosystem)
12. [Benchmark & Observability](#12-benchmark--observability)
13. [Đóng gói & Triển khai](#13-đóng-gói--triển-khai)

---

## 1. Tổng quan Kiến trúc

QMvir là hệ quản trị cơ sở dữ liệu **lai AI-native** (Hybrid AI-Native DBMS), tích hợp năm mặt phẳng dữ liệu trong một tiến trình daemon duy nhất:

| Khả năng | Mô tả | Module |
|-----------|--------|--------|
| **OLTP** | Giao dịch ACID trên dữ liệu có cấu trúc | `GeneralSatellite`, WAL, MVCC |
| **OLAP** | Thực thi vector hóa, window function, CTE | `VecFilter`, `VecHashAggregate` |
| **Vector Search** | Tìm kiếm ANN (HNSW + DiskANN) | `VectorSatellite` |
| **Full-text** | Chỉ mục đảo ngược, tokenization | `InvertedIndex` |
| **Media/Blob** | Zero-copy slab heap cho ảnh/video | `MediaSlabAllocator` |

### Sơ đồ kiến trúc tổng thể

```
┌──────────────────────────────────────────────────────────────────┐
│                         CLIENT LAYER                             │
│   psql/psycopg2 ←──── PostgreSQL Wire Protocol v3 ────→ Gateway │
│   qmvir sql     ←──── prompt_toolkit REPL ──────────→ HubEngine │
└────────────────────────────┬─────────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────────┐
│                     CONTROL PLANE (Hub)                           │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────────────┐   │
│  │ QM-SQL      │  │ LSN          │  │ Merkle                │   │
│  │ Parser      │→ │ Sequencer    │→ │ Auditor               │   │
│  │ (Lark LALR) │  │ (monotonic)  │  │ (SHA-256 tree)        │   │
│  └─────────────┘  └──────────────┘  └───────────────────────┘   │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────────────┐   │
│  │ Cost-Based  │  │ Adaptive     │  │ Learned               │   │
│  │ Optimizer   │→ │ Executor     │→ │ Assistants (ML)       │   │
│  └─────────────┘  └──────────────┘  └───────────────────────┘   │
└────────────────────────────┬─────────────────────────────────────┘
                             │ HubDispatcher
         ┌───────────────────┼───────────────────┐
         ▼                   ▼                   ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐
│ SharedRingBuffer │ │ SharedRingBuffer │ │ SharedRingBuffer │
│ gen_ring (67MB)  │ │ vec_ring (67MB)  │ │ proc_ring (67MB) │
└────────┬────────┘ └────────┬────────┘ └────────┬────────┘
         ▼                   ▼                   ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐
│ General          │ │ Vector          │ │ Procedure        │
│ Satellite        │ │ Satellite       │ │ Satellite        │
│ (CRUD + Text)    │ │ (HNSW+DiskANN) │ │ (PL/QM)          │
└─────────────────┘ └─────────────────┘ └─────────────────┘
         │
         ▼
┌──────────────────────────────────────┐
│ MediaSlabAllocator (shared mmap)     │
│ 64KB×256 │ 1MB×64 │ 8MB×16 │ 64MB×4 │
│ ──────── Tổng: 656 MB ──────────── │
└──────────────────────────────────────┘
```

### Vòng đời Daemon

```
Bootstrap → Auth(RBAC) → HubEngine → MediaHeap → Checkpoint(recover)
         → Spawn(Satellites) → Gateway(TCP:5433) → Monitor(heartbeat)
         → [Chạy] → SIGTERM → drain → final checkpoint → cleanup
```

---

## 2. Control Plane — Hub

### 2.1 LSN Sequencer

Bộ đánh số thứ tự giao dịch **đơn điệu tăng** (monotonically increasing), đảm bảo thứ tự toàn cục.

| Thuộc tính | Giá trị |
|------------|---------|
| Định dạng wire | 24 bytes (`lsn:8` + `epoch:8` + `timestamp_ns:8`) |
| Thread-safe | Có (thiết kế cho single Hub thread) |
| Đảm bảo | Monotonic, gap-free, durable, epoch-aware |

```python
stamp = sequencer.next()       # LSNStamp(lsn=1, epoch=0, ts_ns=...)
batch = sequencer.next_batch(100)  # 100 LSN liên tiếp
```

### 2.2 HubDispatcher

Điểm thắt cổ chai duy nhất (single choke-point) cho mọi thao tác dữ liệu:

```
SQL → Parser → Hub.dispatch() → Ring[satellite] → Satellite._execute_command()
                                                  → Ring[result] → Hub.collect()
```

**Bộ đệm vòng (Ring buffers) được phân tuyến:**
- `_ring_gen` → General Satellite (CRUD + text)
- `_ring_vec` → Vector Satellite (HNSW + DiskANN)
- `_ring_proc` → Procedure Satellite (PL/QM)

### 2.3 Merkle Auditor

Cây Merkle SHA-256 tăng dần (incremental) cho kiểm tra tính toàn vẹn dữ liệu:

```
                  root (SHA-256)
                 ╱              ╲
            h(0,1)              h(2,3)
           ╱      ╲           ╱      ╲
     leaf:table_a  leaf:table_b  leaf:table_c  leaf:table_d
```

- **Hash lá:** `SHA-256(0x00 ‖ data)`
- **Hash nội bộ:** `SHA-256(0x01 ‖ left ‖ right)` — domain-separated
- **Chứng minh đường đi (proof path):** danh sách hash anh em → verify O(log N)

---

## 3. Data Plane — Satellite

### 3.1 Kiến trúc Satellite cơ sở

```python
class Satellite(ABC):
    """Lớp trừu tượng cho mọi nốt tính toán/lưu trữ."""
    #   Ring Consumer → Poll loop → _execute_command() → WAL local → complete()
```

| Cấu hình | Mặc định | Mô tả |
|-----------|----------|-------|
| `poll_interval_us` | 100 | Khoảng cách poll ring (µs) |
| `max_batch` | 64 | Lệnh tối đa/chu kỳ |
| `wal_enabled` | True | Ghi WAL nội bộ |

### 3.2 General Satellite

Xử lý dữ liệu có cấu trúc (INSERT/UPDATE/DELETE/QUERY) với nén Zstandard cấp 6:

- **Lưu trữ hàng:** `_tables[table][row_id] = row_dict`
- **Nén:** Zstandard level 6 (tỷ lệ nén ~3-5×)
- **Khử trùng lặp:** Content-Defined Chunking (CDC), `_chunks[content_hash] = data`
- **Chỉ mục văn bản:** Tích hợp InvertedIndex

### 3.3 Vector Satellite

Tìm kiếm vector ANN chuyên dụng:

- **HNSW Index:** Tìm kiếm nhanh trong RAM (ef_construction=200, M=16)
- **DiskANN Index:** Vector trên SSD, re-rank theo yêu cầu
- **Nén lossless:** XOR-Delta (đường chính — luôn bật)
- **Nén lossy:** Product Quantization (PQ) chỉ cho xấp xỉ
- **Metric hỗ trợ:** cosine, l2, inner product (dot)

```sql
LIKEV VEC [0.1, 0.2, 0.3, ...] IN documents TOP 10 METRIC cosine
```

### 3.4 Procedure Satellite

Thực thi Stored Procedure qua PL/QM interpreter:

```sql
-- Đăng ký procedure
CALL register_proc('my_fn', 'function body ...')
-- Gọi procedure
CALL my_fn(arg1, arg2)
```

### 3.5 Process Isolation

Hai chế độ chạy Satellite:

| Chế độ | Flag | Mô tả |
|--------|------|-------|
| In-process | `--process-isolation=false` | Thread trong cùng tiến trình (mặc định) |
| OS Process | `--process-isolation=true` | Tiến trình riêng qua `multiprocessing` |

```
SatelliteCluster.add(spec) → SatelliteWorker.spawn() → subprocess(_satellite_main)
```

---

## 4. IPC — SharedRingBuffer & Media Slab

### 4.1 SharedRingBuffer (mô hình LMAX Disruptor)

Bộ đệm vòng không khóa (lock-free) trên shared memory (`mmap`):

```
Ring Metadata (32 bytes)
├── magic: 0x514D_5249_4E47_4246 ("QMRINGBF")
├── slot_count: 1024
├── slot_total_size: 65552 (header + data)
└── slot_data_size: 65536

Slot[i] (65552 bytes)
├── Header (16 bytes)
│   ├── state   (1B): EMPTY→READY→PROCESSING→DONE/ERROR
│   ├── lsn     (8B): Log Sequence Number
│   ├── cmd     (1B): CommandType enum
│   ├── payload_sz (4B): kích thước payload thực
│   └── pad     (2B): căn chỉnh
└── Data (65536 bytes): payload
```

**Giao thức sản xuất-tiêu thụ:**

```
Hub (Producer)                    Satellite (Consumer)
─────────────                    ─────────────────────
try_publish(lsn, cmd, payload)
  → slot = hub_cursor++ % N
  → write header + payload       consume()
  → state ← READY                  → scan for READY
                                    → state ← PROCESSING
                                    → _execute_command()
                                    → complete(slot, result)
collect_result(slot)                → state ← DONE
  → read result bytes
```

**Dung lượng:** 32 + 1024 × 65 552 = **~67 MB/ring** × 3 ring = **~201 MB IPC tổng**

### 4.2 Media Slab Allocator

Heap bộ nhớ chia sẻ cho blob lớn (ảnh, video, audio):

| Lớp | Kích thước | Số slab | Tổng | Mục đích |
|-----|-----------|---------|------|---------|
| 0 | 64 KB | 256 | 16 MB | Thumbnail, metadata JSON |
| 1 | 1 MB | 64 | 64 MB | Ảnh nén, audio ngắn |
| 2 | 8 MB | 16 | 128 MB | Ảnh gốc, video segment |
| 3 | 64 MB | 4 | 256 MB | Video frame thô |
| | | **Tổng** | **656 MB** | |

**Cấu trúc file (mỗi lớp kích thước):**

```
┌──────────────────────────────────────┐
│ MagicHeader (32B)                     │   0x514D_534C_4142_4846 ("QMSLABHF")
├──────────────────────────────────────┤
│ Spinlock (1B) + Bitmap (ceil(N/8)B)  │   O(1) alloc/free qua bitmap
├──────────────────────────────────────┤
│ Slab[0] │ Slab[1] │ ... │ Slab[N-1] │   mmap zero-copy access
└──────────────────────────────────────┘
```

**API chính:**

```python
handle = allocator.allocate(1)          # Lớp 1 (1MB)
allocator.write(handle, image_bytes)    # Ghi dữ liệu
mv = allocator.memoryview_of(handle)    # Zero-copy read
allocator.free(handle)                  # O(1) giải phóng
```

---

## 5. QM-SQL — Phương ngữ SQL lai

### 5.1 Grammar (Lark LALR)

QMvir mở rộng SQL chuẩn với hai dạng cú pháp song song:

| Tính năng | Cú pháp đầy đủ | Cú pháp rút gọn |
|-----------|----------------|-----------------|
| Tìm kiếm vector | `SEARCH VECTOR [...] IN t TOP k` | `LIKEV VEC [...] IN t TOP k` |
| Checkpoint | `CHECKPOINT FULL` | `CPOINT FULL` |
| Gắn media | `LINK MEDIA '/path' TO t ROW 5` | `LINK '/path' TO t ROW 5` |
| Gỡ media | `UNLINK MEDIA FROM t ROW_ID 5` | `UNLINK FROM t ROW 5` |
| Xem slab | `SHOW SLABS` | `SLABS` |
| Truy vấn media ref | *(không có)* | `MREF table row_id` |
| Cột khoảng cách | *(không có)* | `SELECT DIST, ...` |

### 5.2 Kiểu dữ liệu

| Kiểu | Mô tả | Ví dụ |
|------|--------|-------|
| `INT` / `BIGINT` | Số nguyên 32/64-bit | `42` |
| `FLOAT` / `DOUBLE` | Số thực 32/64-bit | `3.14` |
| `TEXT` / `VARCHAR(n)` | Chuỗi ký tự | `'hello'` |
| `BOOLEAN` | Giá trị logic | `TRUE` / `FALSE` |
| `BLOB` | Dữ liệu nhị phân tùy ý | *(binary)* |
| `TIMESTAMP` | Mốc thời gian | `'2026-03-08T12:00:00'` |
| `JSON` | Dữ liệu JSON | `'{"key": "val"}'` |
| `VECTOR(dim)` | Vector thực dim chiều | `VECTOR(128)` |

### 5.3 Ví dụ QM-SQL

```sql
-- DDL
CREATE TABLE articles (
    id INT,
    title TEXT,
    embedding VECTOR(128)
);

-- DML
INSERT INTO articles (id, title, embedding) VALUES (1, 'AI paper', '[0.1,0.2,...]');

-- Vector search (cú pháp rút gọn)
LIKEV VEC [0.1, 0.2, 0.3] IN articles TOP 5 METRIC cosine;

-- Checkpoint
CPOINT FULL;

-- Media
LINK '/data/photo.jpg' TO articles ROW 1;
MREF articles 1;
```

---

## 6. Storage Kernel

### 6.1 Write-Ahead Log (WAL)

Ghi log tuần tự trước khi áp dụng thay đổi lên dữ liệu. Đảm bảo **durability** trong ACID.

### 6.2 Buffer Pool

Bộ đệm trang với dung lượng mặc định **4 096 trang**. Chính sách thay thế: LRU-K.

### 6.3 MVCC — Snapshot Isolation

Quản lý đa phiên bản (Multi-Version Concurrency Control) cho phép đọc không chặn ghi:

- Mỗi giao dịch thấy snapshot nhất quán
- Không có read-lock → throughput đọc cao

### 6.4 Segment Manager & Compaction

Quản lý phân đoạn dữ liệu và nén chuẩn hóa nền (background compaction) để duy trì hiệu năng đọc.

---

## 7. Index Kernel

| Chỉ mục | Lớp | Độ phức tạp | Mục đích |
|---------|------|-------------|---------|
| **B+ Tree** | `BPlusTree(order=128)` | O(log N) insert/lookup | Primary key, range scan |
| **Roaring Bitmap** | `RoaringBitmap` | O(1) per-bit ops | Bitmap index, set operations |
| **Inverted Index** | `InvertedIndex` | O(1) per-token | Full-text search |
| **HNSW** | `HNSWIndex` | O(log N) approx | In-RAM vector ANN |
| **DiskANN** | `DiskANNIndex` | O(log N) disk | SSD-backed vector ANN |
| **Product Quantizer** | `ProductQuantizer` | O(1) distance | Nén vector lossy |

---

## 8. Execution Kernel

### 8.1 Pipeline

```
SQL Text → QueryParser → AST → CostModel → QueryPlanner → LogicalPlan
         → AdaptiveExecutor → Vectorized Ops → Result
```

### 8.2 Vectorized Execution

Xử lý dữ liệu theo **batch cột (columnar)** thay vì từng hàng:

- `ColumnBatch` — Lô cột kiểu NumPy
- `VecFilter` — Lọc vector hóa (SIMD-friendly)
- `VecSort` — Sắp xếp vector hóa
- `VecHashAggregate` — Hash aggregate vector hóa

### 8.3 JOIN & Window/CTE

- JOIN operators: Nested Loop, Hash Join, Sort-Merge Join
- Window functions: ROW_NUMBER, RANK, SUM/AVG OVER(...)
- Common Table Expressions: `WITH cte AS (...) SELECT ...`

### 8.4 Learned Components (AI-Native)

| Component | Vai trò |
|-----------|---------|
| `LearnedSelectivity` | Ước lượng tỷ lệ lọc bằng ML |
| `LearnedCachePolicy` | Chính sách cache học được |
| `LearnedFusionWeights` | Trọng số RRF fusion tối ưu |
| `QueryIntentClassifier` | Phân loại ý định truy vấn |

---

## 9. Checkpoint & Recovery

### 9.1 Cơ chế

| Cấu hình | Mặc định | Mô tả |
|-----------|----------|-------|
| `interval_seconds` | 30.0 | Khoảng cách checkpoint theo thời gian |
| `lsn_threshold` | 1000 | Số mutation trước khi trigger |
| `max_checkpoints` | 5 | Giữ tối đa N checkpoint trên đĩa |

### 9.2 Chế độ

- **FULL** — Snapshot toàn bộ trạng thái: `table_meta` + `merkle_root` + LSN
- **DELTA** — Chỉ WAL tail kể từ checkpoint cuối

### 9.3 Định dạng file `.qmck`

```
┌──────────────────────────────────────────┐
│ Magic (8B) │ LSN (8B) │ Epoch (8B) │ TS (8B)  │   Header: 32 bytes
├──────────────────────────────────────────┤
│ Body Length (4B)                          │
├──────────────────────────────────────────┤
│ msgpack({table_meta, merkle_root, mode}) │   Body
├──────────────────────────────────────────┤
│ SHA-256 Checksum (32B)                   │   Integrity verification
└──────────────────────────────────────────┘
```

### 9.4 Recovery

Khi khởi động:
1. Quét thư mục `checkpoints/` cho file `.qmck` mới nhất
2. Xác minh SHA-256 checksum
3. Khôi phục `table_meta`, `merkle_root`, LSN
4. Replay WAL tail (nếu có) từ checkpoint đến hiện tại

---

## 10. RBAC & Wire Protocol

### 10.1 Hệ thống vai trò (Role-Based Access Control)

| Vai trò | Giá trị | Quyền |
|---------|---------|-------|
| `READER` | 1 | SELECT, LIKEV, MREF, DIST |
| `WRITER` | 2 | + INSERT, UPDATE, DELETE, LINK/UNLINK |
| `ADMIN` | 3 | + DDL, CPOINT, SLABS, GRANT/REVOKE |

### 10.2 Bảo mật mật khẩu

- **Thuật toán:** scrypt (N=2¹⁴, r=8, p=1, dklen=32)
- **Salt:** 16 byte ngẫu nhiên mỗi người dùng
- **So sánh:** Constant-time để chống timing attack

### 10.3 PostgreSQL Wire Protocol v3

Gateway TCP nói giao thức libpq chuẩn — tương thích `psql`, `psycopg2`, `pgAdmin`:

```
Startup (length-prefixed) → AuthenticationOk → ReadyForQuery
  → SimpleQuery('Q') → RowDescription → DataRow* → CommandComplete → ReadyForQuery
  → Terminate('X')
```

| Mã lỗi PG | Ý nghĩa |
|------------|---------|
| `28P01` | Sai mật khẩu |
| `42501` | Không đủ quyền |

---

## 11. CLI Ecosystem

### 11.1 Subcommands

| Lệnh | Mô tả |
|-------|-------|
| `qmvir start` | Khởi động daemon |
| `qmvir stop` | Dừng daemon an toàn |
| `qmvir status` | Trạng thái hệ thống |
| `qmvir version` | Thông tin phiên bản |
| `qmvir sql` | Shell SQL tương tác |
| `qmvir dash` | Dashboard giám sát thời gian thực |
| `qmvir bench` | Chạy đo kiểm hiệu năng |

### 11.2 SQL Shell (`qmvir sql`)

- **prompt_toolkit 3.0+** với lịch sử lệnh (`~/.qmvir_history`)
- Auto-completion cho ~70 từ khóa QM-SQL
- Nhập đa dòng (kết thúc bằng `;`)
- Lệnh `/d` `/u` `/h` `/q`

### 11.3 Dashboard (`qmvir dash`)

Màn hình ANSI tự cập nhật mỗi 1 giây:
- Trạng thái daemon (PID, uptime, gateway)
- Satellite status (4 nốt)
- Ring Buffer occupancy (cảnh báo hotspot >80%)
- Media Slab fragmentation
- Checkpoint count

---

## 12. Benchmark & Observability

### 12.1 Benchmark tích hợp (`qm_core.bench`)

| Benchmark | Đo lường | Thành phần |
|-----------|----------|-----------|
| `ring` | Throughput (ops/s) | SharedRingBuffer publish+collect |
| `gateway` | TPS | Execute callback (no network) |
| `checkpoint` | Write speed | CPOINT FULL I/O |
| `vector` | p50/p99 latency | LIKEV round-trip |
| `parse` | Parse rate (stmts/s) | Lark LALR parser |

### 12.2 Automated Benchmark (`tools/auto_bench.py`)

Chu kỳ đo kiểm toàn diện:
1. Khởi động daemon
2. Chạy benchmark suite
3. Xuất JSON report
4. Dừng daemon

---

## 13. Đóng gói & Triển khai

### 13.1 Python Package

```toml
[project]
name = "qmvir"
version = "1.0.0"
requires-python = ">=3.11"
```

### 13.2 Docker

```dockerfile
FROM python:3.13-slim
# Multi-stage build, healthcheck on port 5433
# Volume: /data/qm
```

### 13.3 Entry Points

| Script | Target |
|--------|--------|
| `qmvir` | `qm_app:main` |
| `qm-server` | `qm_app:main` (backward compat) |
| `qm-bench` | `qm_core.bench:print_report` |

---

*Kết thúc Đặc tả Kĩ thuật QMvir v1.0.0*
