# QMvir v2.0.0 — Báo cáo Hiện trạng & Hướng dẫn Sử dụng Chi tiết

> **Phiên bản:** 2.0.0  
> **Ngày:** 02/04/2026  
> **Engine:** Rust-only (PyO3) — ~18,000 LOC, 50 source files  
> **Python:** CPython ≥ 3.11  
> **Nền tảng:** macOS (ARM64 + x86_64), Linux (x86_64 + ARM64), Windows (x64 + ARM64)  
> **Tests:** 118 Python tests + Rust unit tests — ALL PASSING  
> **License:** Proprietary

---

## MỤC LỤC

### Phần I — Hiện trạng Hệ thống
1. [Tổng quan Kiến trúc](#1-tổng-quan-kiến-trúc)
2. [Trạng thái Modules & Components](#2-trạng-thái-modules)
3. [Hiệu năng & Benchmark](#3-hiệu-năng)
4. [Cross-Platform Builds](#4-cross-platform-builds)
5. [Bảo mật](#5-bảo-mật)

### Phần II — Cài đặt
6. [Cài đặt từ Wheel (khuyến nghị)](#6-cài-đặt-từ-wheel)
7. [Cài đặt từ Mã nguồn](#7-cài-đặt-từ-mã-nguồn)
8. [Cài đặt Dependencies](#8-cài-đặt-dependencies)
9. [Xác minh Cài đặt](#9-xác-minh-cài-đặt)

### Phần III — Hướng dẫn Sử dụng
10. [Khởi động Nhanh (Quick Start)](#10-quick-start)
11. [CRUD — Thao tác Dữ liệu Cơ bản](#11-crud)
12. [Truy vấn SQL Nâng cao](#12-truy-vấn-nâng-cao)
13. [Vector Search — Tìm kiếm Ngữ nghĩa](#13-vector-search)
14. [Full-Text Search — BM25](#14-full-text-search)
15. [Analytics — Phân tích Dữ liệu](#15-analytics)
16. [Index — Quản lý Chỉ mục](#16-index)
17. [Cache — Bộ nhớ đệm](#17-cache)
18. [Transaction — Giao dịch ACID](#18-transaction)
19. [IPC Ring Buffer — Giao tiếp Liên tiến trình](#19-ipc-ring-buffer)

### Phần IV — Kết nối Dữ liệu
20. [PostgreSQL Wire Protocol](#20-postgresql-wire-protocol)
21. [HTTP REST API Gateway](#21-http-rest-api)
22. [Schema Action DSL (SDK)](#22-schema-action-dsl)
23. [Kết nối từ Languages khác](#23-kết-nối-languages-khác)
24. [Import/Export Dữ liệu (Parquet, CSV, Arrow)](#24-import-export)

### Phần V — Vận hành
25. [Cấu hình Hệ thống](#25-cấu-hình)
26. [WAL & Crash Recovery](#26-wal-recovery)
27. [Monitoring & Metrics](#27-monitoring)
28. [Docker Production](#28-docker)
29. [Xử lý Sự cố](#29-xử-lý-sự-cố)

### Phụ lục
- [A. Tham chiếu SQL Commands](#phụ-lục-a)
- [B. Tham chiếu Python API](#phụ-lục-b)
- [C. Bảng Data Types](#phụ-lục-c)
- [D. Environment Variables](#phụ-lục-d)

---

# PHẦN I — HIỆN TRẠNG HỆ THỐNG

---

## 1. Tổng quan Kiến trúc {#1-tổng-quan-kiến-trúc}

QMvir là một **multi-engine database platform** kết hợp 5 engine chuyên biệt trong một hệ thống thống nhất:

```
┌─────────────────────────────────────────────────────────────┐
│                    CLIENT LAYER                              │
│  psql / psycopg2 │ HTTP REST │ WebSocket │ Python SDK        │
└────────┬──────────┬──────────┬───────────┬──────────────────┘
         │          │          │           │
┌────────▼──────────▼──────────▼───────────▼──────────────────┐
│                UNIFIED DATA GATEWAY                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────────┐ │
│  │ Auth/ACL │  │ SQL Parse│  │ Query    │  │ W-TinyLFU   │ │
│  │ Argon2id │  │ + Router │  │ Planner  │  │ Cache       │ │
│  └──────────┘  └──────────┘  └──────────┘  └─────────────┘ │
└────────┬──────────┬──────────┬───────────┬──────────────────┘
         │          │          │           │
    ┌────▼───┐ ┌────▼───┐ ┌───▼────┐ ┌───▼─────┐
    │Core DB │ │Search  │ │Vector  │ │Analytics│
    │(OLTP)  │ │Engine  │ │Engine  │ │(OLAP)   │
    │Row-store│ │BM25    │ │HNSW   │ │Columnar │
    │MVCC    │ │Inverted│ │IVF-PQ │ │Arrow    │
    │WAL     │ │Index   │ │DiskANN│ │Parquet  │
    └────┬───┘ └────────┘ └───────┘ └─────────┘
         │
    ┌────▼────────────────────────────────┐
    │        STORAGE LAYER                │
    │  WAL (io_uring) │ Parquet │ mmap    │
    │  Ring Buffer IPC │ Zstd compress    │
    └─────────────────────────────────────┘
```

### Kiến trúc Module

| Layer | Module | Ngôn ngữ | Mô tả |
|-------|--------|-----------|--------|
| **Engine Core** | `qm_engine/` | Rust (PyO3) | 50 files, ~18K LOC — toàn bộ hot path |
| **Gateway** | `gateway/` | Python | HTTP/WS API server, query routing |
| **Core DB** | `core_db/` | Python | MVCC transaction engine, schema management |
| **Search** | `search_platform/` | Python | BM25, inverted index, tokenizer |
| **Vector** | `vector_platform/` | Python+Rust | Embedding store, ANN index, quantization |
| **Analytics** | `analytics_platform/` | Python | Columnar store, aggregation engine |
| **Cache** | `cache_layer/` | Python+Rust | Object cache, query cache, invalidation |
| **Storage** | `storage/` | Python | Row/column store abstraction |
| **Index** | `indexing/` | Python+Rust | B-tree, hash, bitmap, inverted |
| **IPC** | `qm_engine/src/ipc/` | Rust | Lock-free ring buffer, dispatcher |
| **SDK** | `sdk/` | Python | Schema Action client, query builder |

### Rust Engine Components (PyO3)

QMvir expose **14 Python classes** từ Rust core:

| Class Python | Module Rust | Chức năng |
|---|---|---|
| `PostgresGateway` | `gateway/` | PostgreSQL wire protocol v3 server |
| `NativeSqlEngine` | `gateway/native_sql/` | SQL execution engine (CREATE/INSERT/SELECT/UPDATE/DELETE/JOIN) |
| `SqlParser` | `parser/` | SQL parser + query type detection |
| `VectorExecutor` | `executor/` | SIMD-accelerated vector operations |
| `JitCompiler` | `executor/jit.rs` | Adaptive JIT compilation cho WHERE predicates |
| `StorageEngine` | `storage/` | WAL + ACID storage engine |
| `Transaction` | `storage/` | Transaction object (4 isolation levels) |
| `WTinyLfuCache` | `storage/cache.rs` | Sharded W-TinyLFU cache (64 shards) |
| `IndexManager` | `index/` | Autonomous B+Tree index management |
| `HubEngine` | `hub_engine/` | Rust-native SQL coordinator |
| `NativeDispatcher` | `ipc/dispatcher.rs` | Ring buffer IPC dispatcher |
| `RustRingBuffer` | `ipc/ring_buffer.rs` | Shared-memory ring buffer |
| `UringWalWriter` | `storage/uring_wal.rs` | io_uring/pwrite64 WAL writer |
| `MetricsRegistry` | `metrics/` | Performance metrics collector |

---

## 2. Trạng thái Modules & Components {#2-trạng-thái-modules}

### 2.1 Production-Ready (✅ Hoàn tất)

| Component | Trạng thái | Chi tiết |
|---|:-:|---|
| SQL Parser | ✅ | SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, TRUNCATE, JOIN, GROUP BY, ORDER BY, LIMIT |
| Native SQL Engine | ✅ | Full CRUD + JOIN (hash join parallel) + GROUP BY + HAVING + aggregations |
| PostgreSQL Gateway | ✅ | Wire protocol v3, SO_REUSEADDR, optional TLS, prepared statement cache |
| MVCC Transactions | ✅ | 4 isolation levels (Read Uncommitted → Snapshot Isolation) |
| WAL & Recovery | ✅ | io_uring/pwrite64, segment rotation 1GB, CRC32 integrity |
| B+Tree Index | ✅ | Persistent, CRC32 per page, range scan, autonomous management |
| W-TinyLFU Cache | ✅ | 64-shard concurrent, adaptive window/probationary/protected |
| Vector Executor | ✅ | SIMD batch dot product, L2, cosine, flat-buffer zero-copy API |
| IPC Ring Buffer | ✅ | Lock-free, atomic state machine, CRC32, 3 channels |
| Auth & RBAC | ✅ | Argon2id, table-level ACL, GRANT/REVOKE |
| Audit Logging | ✅ | Login, DDL, DML events with timestamps |
| Cross-platform | ✅ | 6 wheels: macOS, Linux, Windows × x86_64 + ARM64 |

### 2.2 Optimized (✅ Đã tối ưu — 2026-04-02)

| Component | Trạng thái | Chi tiết |
|---|:-:|---|
| BM25 Full-text Search | ✅ | BM25Scorer với inverted index, TF-IDF, tokenizer, incremental add/remove |
| Embedding Store | ✅ | SIMD dot product + L2 + cosine similarity, parallel search (Rayon), zero-copy numpy |
| Columnar Store | ✅ | zstd compression trên snapshot pages (~50% giảm dung lượng), CRC32 + HMAC-SHA256 |
| Query Planner | ✅ | Cost-based optimizer: selectivity estimation, point lookup detection, index-aware join |
| JIT Compiler | ✅ | Zero-allocation batch_filter/batch_project — direct columnar indexing thay vì Vec/row |
| Hub Engine | ✅ | Cached tokio Runtime (OnceLock), không tạo mới mỗi PyO3 call |

### 2.3 Roadmap (✅ Đã triển khai)

| Component | Ưu tiên | Trạng thái | Mô tả |
|---|:-:|:-:|---|
| Batch DML | High | ✅ | Multi-row INSERT/UPDATE/DELETE — WHERE col IN (...) support, BatchUpdate/BatchDelete IPC |
| Connection Pooling | High | ✅ | ConnectionPool with borrow/return/evict, pool_hits/pool_misses stats tracking |
| Columnar Storage Format | Medium | ✅ | COPY TO Parquet export (ArrowWriter), CachedColumns columnar cache, COPY FROM import |
| Window Functions | Medium | ✅ | ROW_NUMBER(), RANK(), DENSE_RANK(), LAG(col, n), LEAD(col, n) with PARTITION BY + ORDER BY |
| CTE (WITH clause) | Medium | ✅ | WITH name AS (...) SELECT — materialized temp tables, balanced paren parsing |
| Distributed Sharding | Low | ✅ | PyO3 wrappers: ShardRing, ShardManager — route(), route_batch(), balance_stats() |

---

## 3. Hiệu năng & Benchmark {#3-hiệu-năng}

### 3.1 Kết quả Benchmark (macOS ARM64, Apple M-series)

| Operation | QMvir | vs PostgreSQL | vs DuckDB |
|---|:-:|:-:|:-:|
| **Point Lookup** | 38.7K ops/s | ▲ 2.15x | ▲ 3.58x |
| **Range Scan** | 49.4K ops/s | ▲ 5.69x | ▲ 7.97x |
| **Aggregation** | 52.5K ops/s | ▲ 31.89x | ▲ 7.95x |
| **GROUP BY** | 23.3K ops/s | ▲ 3.73x | ▲ 5.55x |
| **JOIN** | 44.4K ops/s | ▲ 2.43x | ▲ 10.09x |
| **INSERT** | 37.2K ops/s | ▲ 3.58x | ▲ 5.81x |
| **UPDATE** | 37.8K ops/s | ▲ 4.13x | ▲ 4.97x |
| **DELETE** | 41.1K ops/s | ▲ 4.41x | ▲ 5.34x |
| **Ring Buffer IPC** | 1.26M msgs/s | — | — |
| **Cache Hit** | 5.70M ops/s | — | — |
| **Vector L2 (SIMD)** | 7.7K ops/s | — | vs NumPy ▲ 2.41x |
| **SQL Parse** | 105.1K ops/s | — | — |
| **JIT filter_eq 1K** | 74.6K ops/s | — | — |
| **Txn Begin+Commit** | 14.0M ops/s | — | — |

### 3.2 Tài nguyên Hệ thống

| Metric | Giá trị |
|---|---|
| Binary size (wheel) | 3.3 - 3.8 MB (tùy platform) |
| Startup time | < 100ms |
| Memory baseline | ~20 MB (engine only) |
| Memory per 1M rows | ~200-400 MB (tùy schema) |

---

## 4. Cross-Platform Builds {#4-cross-platform-builds}

### 4.1 Wheels Có sẵn (v2.0.0, CPython 3.13)

| Platform | Architecture | File | Size |
|---|---|---|---|
| macOS | ARM64 (Apple Silicon) | `qm_engine-2.0.0-cp313-cp313-macosx_11_0_arm64.whl` | 3.4 MB |
| macOS | x86_64 (Intel) | `qm_engine-2.0.0-cp313-cp313-macosx_10_12_x86_64.whl` | 3.8 MB |
| Linux | x86_64 | `qm_engine-2.0.0-cp313-cp313-manylinux_2_17_x86_64.whl` | 3.7 MB |
| Linux | ARM64 (aarch64) | `qm_engine-2.0.0-cp313-cp313-manylinux_2_17_aarch64.whl` | 3.4 MB |
| Windows | x86_64 | `qm_engine-2.0.0-cp313-cp313-win_amd64.whl` | 3.7 MB |
| Windows | ARM64 | `qm_engine-2.0.0-cp313-cp313-win_arm64.whl` | 3.3 MB |

### 4.2 Build Toolchain

- **Rust:** 1.94.0 stable
- **PyO3:** 0.22 (generate-import-lib cho Windows)
- **Maturin:** 1.12.6
- **Cross-linker:** Zig 0.15.2
- **TLS backend:** `ring` (pure Rust, cross-compile compatible)

---

## 5. Bảo mật {#5-bảo-mật}

### 5.1 Authentication

| Tính năng | Chi tiết |
|---|---|
| Password hashing | **Argon2id** (PHC format) — production-grade KDF |
| Legacy fallback | SHA256 + salt |
| Timing attack protection | Constant-time comparison |
| Default admin | User `admin`, password from `QM_ADMIN_PASSWORD` env |

### 5.2 Authorization (RBAC)

| SQL Command | Mô tả |
|---|---|
| `CREATE USER username PASSWORD 'pass'` | Tạo user mới |
| `DROP USER username` | Xóa user |
| `ALTER USER username PASSWORD 'new_pass'` | Đổi password |
| `GRANT SELECT, INSERT ON table TO user` | Cấp quyền |
| `REVOKE DELETE ON table FROM user` | Thu hồi quyền |
| `GRANT ALL ON * TO user` | Cấp toàn quyền |

**Quyền hạn:** SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, ALL

### 5.3 Encryption

| Tính năng | Chi tiết |
|---|---|
| TLS (optional) | `QM_TLS_CERT` + `QM_TLS_KEY` environment variables |
| Wire protocol | PostgreSQL v3 with optional StartupTLS |
| Data at rest | WAL + CRC32 integrity (encryption-at-rest: roadmap) |

### 5.4 Audit Logging

Ghi log tự động cho: LOGIN, LOGOUT, CREATE TABLE, INSERT, UPDATE, DELETE, GRANT, REVOKE.

---

# PHẦN II — CÀI ĐẶT

---

## 6. Cài đặt từ Wheel (khuyến nghị) {#6-cài-đặt-từ-wheel}

### 6.1 Yêu cầu Hệ thống

| Yêu cầu | Tối thiểu | Khuyến nghị |
|---|---|---|
| Python | 3.11 | 3.13 |
| RAM | 2 GB | 8 GB+ |
| Disk | 100 MB | 10 GB+ (tùy dữ liệu) |
| OS | macOS 10.12+ / Linux glibc 2.17+ / Windows 10+ | macOS 14+ / Ubuntu 22.04+ / Windows 11 |

### 6.2 Cài đặt

**macOS (Apple Silicon — M1/M2/M3/M4):**
```bash
pip install qm_engine-2.0.0-cp313-cp313-macosx_11_0_arm64.whl
```

**macOS (Intel):**
```bash
pip install qm_engine-2.0.0-cp313-cp313-macosx_10_12_x86_64.whl
```

**Linux (x86_64):**
```bash
pip install qm_engine-2.0.0-cp313-cp313-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
```

**Linux (ARM64 — AWS Graviton, Raspberry Pi 4+):**
```bash
pip install qm_engine-2.0.0-cp313-cp313-manylinux_2_17_aarch64.manylinux2014_aarch64.whl
```

**Windows (x64):**
```bash
pip install qm_engine-2.0.0-cp313-cp313-win_amd64.whl
```

**Windows (ARM64 — Surface Pro X, Snapdragon):**
```bash
pip install qm_engine-2.0.0-cp313-cp313-win_arm64.whl
```

### 6.3 Xác minh Cài đặt Nhanh

```bash
python -c "import qm_engine; print('QMvir OK')"
```

---

## 7. Cài đặt từ Mã nguồn {#7-cài-đặt-từ-mã-nguồn}

### 7.1 Yêu cầu Build

| Tool | Phiên bản |
|---|---|
| Rust | ≥ 1.70 (khuyến nghị 1.94+) |
| Maturin | ≥ 1.0 |
| Python | ≥ 3.11 |
| C compiler | Xcode CLI (macOS), gcc (Linux), MSVC (Windows) |

### 7.2 Build Steps

```bash
# Clone repository
git clone <repo-url> QM && cd QM

# Tạo virtual environment
python3 -m venv .venv
source .venv/bin/activate    # Linux/macOS
# .venv\Scripts\activate     # Windows

# Cài đặt maturin
pip install maturin

# Build và install (development mode)
maturin develop --release --manifest-path qm_engine/Cargo.toml

# Hoặc build wheel
maturin build --release --manifest-path qm_engine/Cargo.toml --out dist/

# Cài đặt Python dependencies
pip install -e ".[dev,vector,analytics]"
```

### 7.3 Cross-Compile cho Platform khác

```bash
# Cài zig (cross-linker)
brew install zig     # macOS
# apt install zig    # Linux

# Cài Rust targets
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
rustup target add x86_64-pc-windows-gnu aarch64-pc-windows-gnullvm

# Build cho Linux x86_64
maturin build --release --zig --target x86_64-unknown-linux-gnu -i python3.13

# Build cho Linux ARM64
maturin build --release --zig --target aarch64-unknown-linux-gnu -i python3.13

# Build cho Windows x64
maturin build --release --zig --target x86_64-pc-windows-gnu -i python3.13

# Build cho Windows ARM64
maturin build --release --zig --target aarch64-pc-windows-gnullvm -i python3.13
```

---

## 8. Cài đặt Dependencies {#8-cài-đặt-dependencies}

### 8.1 Dependencies Bắt buộc

```bash
pip install msgpack orjson xxhash lz4 zstandard numpy aiohttp uvloop lark prompt_toolkit
```

### 8.2 Dependencies Tùy chọn

```bash
# Vector search (HNSW, FAISS)
pip install hnswlib faiss-cpu

# Analytics (Arrow, Polars)
pip install pyarrow polars

# Development
pip install pytest pytest-asyncio pytest-benchmark ruff mypy

# Tất cả
pip install -e ".[dev,vector,analytics]"
```

---

## 9. Xác minh Cài đặt {#9-xác-minh-cài-đặt}

```python
import qm_engine
import tempfile, os

# 1. Kiểm tra module
print(f"QMvir version: {qm_engine.__name__}")

# 2. Kiểm tra SQL Parser
parser = qm_engine.SqlParser()
result = parser.parse("SELECT * FROM test")
print(f"SQL Parser: OK — query_type = {result['query_type']}")

# 3. Kiểm tra Vector Executor
vec = qm_engine.VectorExecutor()
scores = vec.batch_dot_product([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], [1.0, 0.0, 0.0])
print(f"Vector Executor: OK — scores = {scores}")

# 4. Kiểm tra Cache
cache = qm_engine.WTinyLfuCache(capacity=1000)
cache.insert("key1", b"value1")
val = cache.get("key1")
print(f"Cache: OK — retrieved = {val}")

# 5. Kiểm tra NativeDispatcher (IPC)
td = tempfile.mkdtemp()
nd = qm_engine.NativeDispatcher(td)
nd.dispatch_ddl(b"CREATE TABLE verify (id INT, name TEXT)")
nd.dispatch_insert(b"INSERT INTO verify VALUES (1, 'hello')")
print(f"NativeDispatcher: OK — ring_dir = {nd.ring_dir()}")

# 6. Kiểm tra Index Manager
idx = qm_engine.IndexManager()
print(f"Index Manager: OK — indexes = {idx.list_indexes()}")

# 7. Kiểm tra JIT Compiler
jit = qm_engine.JitCompiler()
result = jit.filter_eq_i64([10, 20, 30, 20], 0, 20)
print(f"JIT Compiler: OK — matching indices = {result}")

print("\n✅ All QMvir components verified successfully!")
```

---

# PHẦN III — HƯỚNG DẪN SỬ DỤNG

---

## 10. Khởi động Nhanh (Quick Start) {#10-quick-start}

### 10.1 Hello World — Tạo bảng và truy vấn

```python
import qm_engine
import tempfile

# Tạo data directory
data_dir = tempfile.mkdtemp()

# Khởi tạo NativeDispatcher (engine chính)
db = qm_engine.NativeDispatcher(data_dir)

# === DDL: Tạo bảng ===
db.dispatch_ddl(b"CREATE TABLE users (id INT, name TEXT, email TEXT, age INT)")

# === INSERT: Thêm dữ liệu ===
db.dispatch_insert(b"INSERT INTO users VALUES (1, 'Alice', 'alice@example.com', 30)")
db.dispatch_insert(b"INSERT INTO users VALUES (2, 'Bob', 'bob@example.com', 25)")
db.dispatch_insert(b"INSERT INTO users VALUES (3, 'Charlie', 'charlie@example.com', 35)")

# === SELECT: Truy vấn ===
db.dispatch_query(b"SELECT * FROM users")
db.dispatch_query(b"SELECT name, age FROM users WHERE age > 28")
db.dispatch_query(b"SELECT * FROM users ORDER BY age DESC LIMIT 2")

print("✅ Hello World complete!")
```

### 10.2 Sử dụng HubEngine (SQL Coordinator)

```python
import qm_engine
import tempfile, json

# HubEngine = Rust-native SQL coordinator
data_dir = tempfile.mkdtemp()
hub = qm_engine.HubEngine(data_dir)
hub.start()

# Thực thi SQL
result = hub.execute_sql("CREATE TABLE products (id INT, name TEXT, price FLOAT)", "", False)
print(json.loads(result))

result = hub.execute_sql("INSERT INTO products VALUES (1, 'Laptop', 999.99)", "", False)
print(json.loads(result))

result = hub.execute_sql("SELECT * FROM products", "", False)
data = json.loads(result)
print(f"Products: {data}")
```

### 10.3 Kết nối qua PostgreSQL Protocol

```python
import qm_engine
import time

# Khởi động PostgreSQL gateway
gw = qm_engine.PostgresGateway(host='127.0.0.1', port=55433)
gw.start_native()
time.sleep(0.5)
print(f"Gateway running: {gw.is_running}")

# === Kết nối bằng psql (terminal khác) ===
# psql -h 127.0.0.1 -p 55433 -U admin
# Password: (QM_ADMIN_PASSWORD env)

# === Kết nối bằng psycopg2 ===
# import psycopg2
# conn = psycopg2.connect(host='127.0.0.1', port=55433, user='admin', password='...')
# cur = conn.cursor()
# cur.execute("SELECT * FROM users")
# rows = cur.fetchall()

# Dừng gateway
# gw.stop()
```

---

## 11. CRUD — Thao tác Dữ liệu Cơ bản {#11-crud}

### 11.1 CREATE TABLE

```sql
-- Cú pháp
CREATE TABLE table_name (
    column1 TYPE,
    column2 TYPE,
    ...
)

-- Ví dụ
CREATE TABLE employees (
    id INT,
    name TEXT,
    department TEXT,
    salary FLOAT,
    active BOOL
)

CREATE TABLE orders (
    order_id INT,
    customer_id INT,
    product TEXT,
    quantity INT,
    price FLOAT,
    order_date TEXT
)
```

**Data Types:**

| Type | Aliases | Mô tả | Ví dụ |
|---|---|---|---|
| `INT` | `INTEGER`, `INT32` | Số nguyên 32-bit | `42` |
| `INT64` | `BIGINT` | Số nguyên 64-bit | `9999999999` |
| `INT16` | `SMALLINT` | Số nguyên 16-bit | `255` |
| `FLOAT` | `FLOAT8`, `DOUBLE` | Số thực 64-bit | `3.14159` |
| `FLOAT4` | `REAL` | Số thực 32-bit | `3.14` |
| `TEXT` | `VARCHAR` | Chuỗi UTF-8 | `'Hello'` |
| `BOOL` | `BOOLEAN` | Boolean | `true` / `false` |

### 11.2 INSERT

```sql
-- Chèn 1 row
INSERT INTO employees VALUES (1, 'Nguyen Van A', 'Engineering', 85000.0, true)

-- Chèn với tên cột
INSERT INTO employees (id, name, department) VALUES (2, 'Tran Thi B', 'Marketing')

-- Chuỗi có dấu nháy kép
INSERT INTO employees VALUES (3, 'Le Van C', 'Sales', 65000.0, true)
```

**Python batch insert:**

```python
# Insert nhiều row
employees = [
    (1, 'Nguyen Van A', 'Engineering', 85000.0),
    (2, 'Tran Thi B', 'Marketing', 72000.0),
    (3, 'Le Van C', 'Sales', 65000.0),
    (4, 'Pham Thi D', 'Engineering', 92000.0),
    (5, 'Hoang Van E', 'Marketing', 68000.0),
]

for emp in employees:
    sql = f"INSERT INTO employees VALUES ({emp[0]}, '{emp[1]}', '{emp[2]}', {emp[3]}, true)"
    db.dispatch_insert(sql.encode())

# Batch insert (hiệu quả hơn)
payloads = [
    f"INSERT INTO employees VALUES ({e[0]}, '{e[1]}', '{e[2]}', {e[3]}, true)".encode()
    for e in employees
]
db.dispatch_insert_batch(payloads)
```

### 11.3 SELECT

```sql
-- Tất cả columns
SELECT * FROM employees

-- Chọn columns
SELECT name, salary FROM employees

-- WHERE clause
SELECT * FROM employees WHERE department = 'Engineering'
SELECT * FROM employees WHERE salary > 70000
SELECT name, salary FROM employees WHERE salary >= 65000 AND department = 'Sales'

-- ORDER BY
SELECT * FROM employees ORDER BY salary DESC
SELECT * FROM employees ORDER BY name ASC

-- LIMIT + OFFSET (phân trang)
SELECT * FROM employees ORDER BY id LIMIT 10
SELECT * FROM employees ORDER BY id LIMIT 10 OFFSET 20

-- Kết hợp
SELECT name, salary
FROM employees
WHERE department = 'Engineering' AND salary > 80000
ORDER BY salary DESC
LIMIT 5
```

### 11.4 UPDATE

```sql
-- Update với WHERE
UPDATE employees SET salary = 90000.0 WHERE id = 1
UPDATE employees SET department = 'Management', salary = 120000.0 WHERE name = 'Nguyen Van A'

-- Update nhiều rows
UPDATE employees SET active = false WHERE department = 'Sales'
```

### 11.5 DELETE

```sql
-- Delete với WHERE
DELETE FROM employees WHERE id = 5
DELETE FROM employees WHERE active = false
DELETE FROM employees WHERE department = 'Marketing' AND salary < 70000
```

### 11.6 DROP & TRUNCATE

```sql
-- Xóa bảng
DROP TABLE old_data

-- Xóa dữ liệu (giữ schema)
TRUNCATE employees
```

---

## 12. Truy vấn SQL Nâng cao {#12-truy-vấn-nâng-cao}

### 12.1 JOIN

```sql
-- INNER JOIN
SELECT e.name, e.salary, d.department_name
FROM employees e
JOIN departments d ON e.department_id = d.id

-- JOIN với điều kiện
SELECT o.order_id, c.name, o.total
FROM orders o
JOIN customers c ON o.customer_id = c.id
WHERE o.total > 1000
ORDER BY o.total DESC
```

**Python hash join:**

```python
import qm_engine, json

hub = qm_engine.HubEngine(data_dir)
hub.start()

# Hash join giữa 2 bảng (Rayon parallel)
build_json = json.dumps([
    {"id": 1, "dept": "Engineering"},
    {"id": 2, "dept": "Marketing"},
])
probe_json = json.dumps([
    {"emp_id": 1, "name": "Alice", "dept_id": 1},
    {"emp_id": 2, "name": "Bob", "dept_id": 2},
    {"emp_id": 3, "name": "Charlie", "dept_id": 1},
])
result = hub.execute_hash_join_bytes(
    build_json.encode(), probe_json.encode(),
    "id", "dept_id"
)
print(json.loads(result))
```

### 12.2 GROUP BY + Aggregations

```sql
-- COUNT
SELECT department, COUNT(*) as total
FROM employees
GROUP BY department

-- SUM
SELECT department, SUM(salary) as total_salary
FROM employees
GROUP BY department

-- AVG
SELECT department, AVG(salary) as avg_salary
FROM employees
GROUP BY department

-- MIN, MAX
SELECT department, MIN(salary) as min_sal, MAX(salary) as max_sal
FROM employees
GROUP BY department

-- HAVING
SELECT department, AVG(salary) as avg_sal
FROM employees
GROUP BY department
HAVING AVG(salary) > 75000
```

### 12.3 CREATE INDEX

```sql
-- Tạo B+Tree index
CREATE INDEX idx_emp_dept ON employees (department)
CREATE INDEX idx_order_date ON orders (order_date)

-- Xóa index
DROP INDEX idx_emp_dept
```

**Python autonomous index management:**

```python
idx = qm_engine.IndexManager()

# Tạo index thủ công
idx.create_index("idx_emp_dept", "employees", ["department"])

# Ghi nhận query patterns
idx.record_query_hit("employees", "department")
idx.record_query_hit("employees", "department")
idx.record_query_hit("employees", "salary")

# Ghi nhận write operations
idx.record_write("employees", "salary")

# Cập nhật selectivity
idx.update_selectivity("employees", "department", distinct=5, total=10000)
idx.record_numeric_value("employees", "salary", 85000.0)

# Autonomous evaluation — đề xuất tạo/xóa index
decisions = idx.evaluate()
idx.apply_decisions()

# Xem histogram
hist = idx.histogram("employees", "salary")
print(f"Salary histogram: min={hist[0]}, max={hist[1]}, total={hist[2]}")

# Liệt kê indexes
for name, table, cols, state, use_count in idx.list_indexes():
    print(f"  {name}: {table}({cols}) — state={state}, used={use_count}")
```

### 12.4 Transactions

```sql
-- Transaction SQL
BEGIN
INSERT INTO accounts VALUES (1, 'Alice', 10000.0)
UPDATE accounts SET balance = balance - 500 WHERE id = 1
COMMIT

-- Rollback
BEGIN
DELETE FROM accounts WHERE id = 1
ROLLBACK  -- Hủy bỏ, dữ liệu không bị xóa
```

---

## 13. Vector Search — Tìm kiếm Ngữ nghĩa {#13-vector-search}

### 13.1 VectorExecutor — SIMD Operations

```python
import qm_engine
import numpy as np

vec = qm_engine.VectorExecutor()

# === Dot Product (Cosine Similarity) ===
query = [1.0, 0.0, 0.0, 0.0]
vectors = [
    [1.0, 0.0, 0.0, 0.0],   # score = 1.0 (identical)
    [0.7, 0.7, 0.0, 0.0],   # score = 0.7
    [0.0, 1.0, 0.0, 0.0],   # score = 0.0 (orthogonal)
]
scores = vec.batch_dot_product(vectors, query)
print(f"Dot products: {scores}")
# → [1.0, 0.7, 0.0]

# === L2 Distance (Euclidean) ===
distances = vec.batch_l2_distance(vectors, query)
print(f"L2 distances: {distances}")
# → [0.0, ~0.42, 1.0]

# === Top-K Search ===
results = vec.search(vectors, query, k=2)
print(f"Top-2: {results}")
# → [(0, 1.0), (1, 0.7)]  — (vector_index, score)
```

### 13.2 Flat Buffer API (Zero-Copy, hiệu năng cao)

```python
# Flat buffer = mảng 1D liên tục (row-major)
# Phù hợp khi vectors được lưu dạng numpy array hoặc memmap

dim = 128
n_vectors = 100000

# Tạo dữ liệu test
data = np.random.randn(n_vectors * dim).astype(np.float32).tolist()
query = np.random.randn(dim).astype(np.float32).tolist()

# Dot product trên flat buffer
scores = vec.batch_dot_product_flat(data, dim, query)
print(f"100K vectors scored in one call")

# L2 distance trên flat buffer
distances = vec.batch_l2_distance_flat(data, dim, query)

# Top-K search trên flat buffer
results = vec.search_flat(data, dim, query, k=10)
print(f"Top-10 results: {results}")

# Batch search (nhiều queries cùng lúc)
queries_flat = np.random.randn(5 * dim).astype(np.float32).tolist()
batch_results = vec.batch_search_flat(data, dim, queries_flat, dim, k=10)
# → 5 bộ kết quả, mỗi bộ 10 vectors gần nhất
```

### 13.3 Parallel Search (Rayon)

```python
# Parallel dot product — tự động phân chia cho nhiều CPU cores
scores = vec.parallel_batch_dot_product(vectors, query)

# Batch search — xử lý đồng thời nhiều queries
queries = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
]
all_results = vec.batch_search(vectors, queries, k=5)
# → Mỗi query trả về top-5 vectors gần nhất
```

### 13.4 QM-SQL Vector Query

```sql
-- Vector search bằng QM-SQL dialect
SELECT * FROM embeddings
WHERE vector <-> [0.1, 0.2, 0.3, 0.4]
LIMIT 10

-- Với metric cụ thể
LIKEV vector [0.1, 0.2, 0.3] IN documents TOP 10 METRIC cosine
LIKEV vector [0.1, 0.2, 0.3] IN documents TOP 10 METRIC l2
LIKEV vector [0.1, 0.2, 0.3] IN documents TOP 10 METRIC inner_product
```

### 13.5 Embedding Store (Python module)

```python
import numpy as np
from vector_platform.embedding_store.store import EmbeddingStore
from vector_platform.ann_index.hnsw import BruteForceIndex, DistanceMetric

# Tạo store
store = EmbeddingStore(dimension=384)

# Lưu embeddings (từ model như sentence-transformers)
store.insert("doc1", np.array([...]), {"title": "Database Guide", "category": "tech"})
store.insert("doc2", np.array([...]), {"title": "Cooking Recipe", "category": "food"})

# Tìm kiếm ANN
idx = BruteForceIndex(dimension=384, metric=DistanceMetric.COSINE)
idx.add("doc1", store.get("doc1").vector)
idx.add("doc2", store.get("doc2").vector)

query_vector = np.array([...])  # embedding của câu hỏi
results = idx.search(query_vector, k=5)

for r in results.results:
    print(f"  {r.doc_id}: score={r.distance:.4f}")
```

### 13.6 Metadata Filtering

```python
from vector_platform.metadata_filter.filters import MetadataFilterEngine, FilterOp

engine = MetadataFilterEngine()

# Filter theo category
filter_fn = engine.build_filter_fn([
    {"field": "category", "op": FilterOp.EQ, "value": "tech"}
])

# Filter theo range
filter_fn = engine.build_filter_fn([
    {"field": "price", "op": FilterOp.GTE, "value": 10.0},
    {"field": "price", "op": FilterOp.LTE, "value": 100.0},
])

# Filter IN list
filter_fn = engine.build_filter_fn([
    {"field": "status", "op": FilterOp.IN, "value": ["active", "pending"]}
])
```

### 13.7 Vector Quantization

```python
from vector_platform.quantization.quantizer import VectorQuantizer
import numpy as np

q = VectorQuantizer()
v = np.array([1.0, 2.0, 3.0, 4.0], dtype=np.float32)

# FP16 — tiết kiệm 50% memory, mất precision rất nhỏ
encoded = q.to_fp16(v)
decoded = q.from_fp16(encoded)

# Int8 — tiết kiệm 75% memory, mất precision nhỏ
encoded, params = q.to_int8(v)
decoded = q.from_int8(encoded, params)
```

---

## 14. Full-Text Search — BM25 {#14-full-text-search}

```python
from search_platform.lexical_search.bm25 import BM25Scorer

# Khởi tạo với field weights
scorer = BM25Scorer(field_weights={"title": 3.0, "body": 1.0, "tags": 2.0})

# Thêm tài liệu
scorer.add_document("article_1", {
    "title": "Hướng dẫn Python cho người mới bắt đầu",
    "body": "Python là ngôn ngữ lập trình phổ biến...",
    "tags": "python programming tutorial"
})

scorer.add_document("article_2", {
    "title": "Database Design Best Practices",
    "body": "Relational database design requires normalization...",
    "tags": "database sql design"
})

scorer.add_document("article_3", {
    "title": "Machine Learning với Python",
    "body": "Machine learning sử dụng Python và scikit-learn...",
    "tags": "python ml ai"
})

# Tìm kiếm
results = scorer.search("python", limit=10)
for r in results:
    print(f"  {r.doc_id}: score={r.score:.4f}")
# article_1 và article_3 sẽ được rank cao (title weight = 3.0)

# Tìm kiếm multi-term
results = scorer.search("database design", limit=5)
# article_2 sẽ rank #1

# Xóa tài liệu
scorer.remove_document("article_1")
```

---

## 15. Analytics — Phân tích Dữ liệu {#15-analytics}

### 15.1 Columnar Store

```python
from analytics_platform.columnar_store.columnar import ColumnarStore

store = ColumnarStore()

# Insert dữ liệu events
events = [
    {"event": "pageview", "page": "/home", "user_id": 1, "duration": 5.2},
    {"event": "click", "page": "/pricing", "user_id": 2, "duration": 1.1},
    {"event": "pageview", "page": "/docs", "user_id": 1, "duration": 12.5},
    {"event": "click", "page": "/signup", "user_id": 3, "duration": 0.8},
    {"event": "pageview", "page": "/home", "user_id": 2, "duration": 3.7},
]
for e in events:
    store.insert("events", e)

# Column pruning — chỉ đọc columns cần thiết
rows = store.scan("events", columns=["event", "duration"])

# Predicate pushdown — lọc trước khi scan
rows = store.scan("events", predicates={"event": "pageview"})

# Aggregations
result = store.aggregate("events",
    group_by=["event"],
    metrics=[{"count": "*"}, {"avg": "duration"}, {"sum": "duration"}]
)
for r in result:
    print(f"  {r['event']}: count={r['count']}, avg={r['avg_duration']:.1f}s")
```

### 15.2 Arrow/Parquet Integration

```python
# Đọc Parquet files trực tiếp vào SQL engine
# QMvir tự động convert Arrow types → Cell types

# Supported Arrow types:
# - Int64, Int32 → Cell::Int
# - Float64, Float32 → Cell::Float
# - Utf8, LargeUtf8 → Cell::Text
```

---

## 16. Index — Quản lý Chỉ mục {#16-index}

### 16.1 B-Tree Index

```python
from indexing.btree.btree import BTree

# Tạo B-tree với order = 4
tree = BTree(order=4)

# Insert
tree.insert(42, "row_42")
tree.insert(10, "row_10")
tree.insert(99, "row_99")

# Point lookup — O(log n)
value = tree.get(42)  # → "row_42"

# Range scan
results = tree.range_scan(10, 50)  # → [(10, "row_10"), (42, "row_42")]

# Scan all (sorted order)
all_items = tree.scan_all()  # → [(10, ...), (42, ...), (99, ...)]

# Delete
tree.delete(42)
assert tree.get(42) is None

# Unique constraint
tree = BTree(order=4, unique=True)
tree.insert(1, "first")
tree.insert(1, "duplicate")  # → raises ValueError
```

### 16.2 Rust Native Index Manager

```python
import qm_engine

mgr = qm_engine.IndexManager()

# Tạo index
mgr.create_index("idx_users_email", "users", ["email"])

# Track query patterns (hệ thống tự học)
for _ in range(100):
    mgr.record_query_hit("users", "email")
mgr.record_query_hit("users", "name")

# Track writes
mgr.record_write("users", "email")

# Update column statistics
mgr.update_selectivity("users", "email", distinct=9500, total=10000)
mgr.update_selectivity("users", "name", distinct=8000, total=10000)

# Autonomous index recommendation
decisions = mgr.evaluate()
mgr.apply_decisions()

# Histogram cho numeric columns
mgr.record_numeric_value("orders", "amount", 150.0)
mgr.record_numeric_value("orders", "amount", 250.0)
mgr.record_numeric_value("orders", "amount", 50.0)
hist = mgr.histogram("orders", "amount")
# → (min, max, total, buckets)

# Range selectivity estimation
sel = mgr.update_selectivity_between("orders", "amount", 100.0, 200.0)

# List all indexes
for name, table, cols, state, use_count in mgr.list_indexes():
    print(f"  {name}: {table}({', '.join(cols)}) state={state} uses={use_count}")
```

---

## 17. Cache — Bộ nhớ đệm {#17-cache}

### 17.1 Rust W-TinyLFU Cache

```python
import qm_engine

cache = qm_engine.WTinyLfuCache(capacity=100000)

# Insert
cache.insert("user:42", b'{"name": "Alice", "age": 30}')
cache.insert("user:43", b'{"name": "Bob", "age": 25}')

# Get
data = cache.get("user:42")
if data:
    import json
    user = json.loads(data)
    print(f"User: {user['name']}")
else:
    print("Cache miss — load from database")

# Remove
cache.remove("user:42")

# Statistics
hit_rate = cache.hit_rate()
weight = cache.weight()
print(f"Hit rate: {hit_rate:.1%}, Weight: {weight}")
```

### 17.2 Python Object Cache (LRU)

```python
from cache_layer.object_cache.lru_cache import ObjectCache

cache = ObjectCache(max_size=1000, default_ttl=300)  # TTL = 5 phút

# Put với tags (để invalidate theo nhóm)
cache.put("article:1", {"title": "Hello"}, version=1, tags=["articles"])
cache.put("article:2", {"title": "World"}, version=1, tags=["articles"])
cache.put("user:1", {"name": "Alice"}, version=1, tags=["users"])

# Get
article = cache.get("article:1")

# Invalidate tất cả articles khi có write
cache.invalidate_by_tag("articles")

# Version-based invalidation
cache.put("config:db", {"host": "localhost"}, version=1, tags=["config"])
cache.invalidate_if_stale("config:db", current_version=2)  # → True (xóa entry cũ)

# Statistics
stats = cache.stats()
print(f"Hits: {stats['hits']}, Misses: {stats['misses']}")
```

### 17.3 Query Cache

```python
from cache_layer.query_cache.query_cache import QueryCache

qc = QueryCache(max_size=5000)

# Cache kết quả query
query = {"action": "find", "entity": "articles", "filter": {"status": "published"}}
results = [{"id": "a1", "title": "Hello"}, {"id": "a2", "title": "World"}]
qc.put("articles", query, results)

# Lấy từ cache
cached = qc.get("articles", query)
if cached:
    print(f"Cache hit: {len(cached)} results")

# Invalidate khi collection thay đổi
qc.invalidate_collection("articles")
```

---

## 18. Transaction — Giao dịch ACID {#18-transaction}

### 18.1 Rust Transaction Engine

```python
import qm_engine
import tempfile

# StorageEngine + Transaction
storage = qm_engine.StorageEngine(
    data_dir=tempfile.mkdtemp(),
    wal_dir=tempfile.mkdtemp()
)

# Bắt đầu transaction
txn = storage.begin_transaction()
print(f"Transaction ID: {txn.txn_id}, Isolation: {txn.isolation}")

# Commit
storage.commit(txn)

# Hoặc rollback
txn2 = storage.begin_transaction()
storage.rollback(txn2)

# Flush WAL to disk
storage.flush()
```

### 18.2 Python MVCC Engine

```python
from core_db.transaction_engine.mvcc import TransactionEngine, IsolationLevel

engine = TransactionEngine()

# === Read Committed ===
txn = engine.begin(isolation=IsolationLevel.READ_COMMITTED)
engine.insert(txn, "accounts", "acc1", {"balance": 10000})
engine.commit(txn)

# === Snapshot Isolation ===
# Transaction 1: read snapshot tại thời điểm BEGIN
txn1 = engine.begin(isolation=IsolationLevel.SNAPSHOT)
row = engine.read(txn1, "accounts", "acc1")
print(f"Balance: {row['balance']}")

# Transaction 2: update và commit
txn2 = engine.begin()
engine.update(txn2, "accounts", "acc1", {"balance": 9500})
engine.commit(txn2)

# Transaction 1: vẫn thấy giá trị cũ (snapshot)
row = engine.read(txn1, "accounts", "acc1")
assert row["balance"] == 10000  # Snapshot isolation!
engine.commit(txn1)

# === WAL Entries ===
txn3 = engine.begin()
engine.insert(txn3, "logs", "l1", {"msg": "test"})
engine.commit(txn3)
wal = engine.get_wal_entries(txn3)
print(f"WAL entries: {len(wal)}")
```

**Isolation Levels:**

| Level | Dirty Read | Non-repeatable Read | Phantom Read |
|---|:-:|:-:|:-:|
| `READ_UNCOMMITTED` | Có thể | Có thể | Có thể |
| `READ_COMMITTED` | Không | Có thể | Có thể |
| `REPEATABLE_READ` | Không | Không | Có thể |
| `SNAPSHOT` | Không | Không | Không |

---

## 19. IPC Ring Buffer — Giao tiếp Liên tiến trình {#19-ipc-ring-buffer}

### 19.1 NativeDispatcher

NativeDispatcher sử dụng **3 ring buffers** (shared memory, mmap) để giao tiếp giữa Hub và Satellites:

| Ring | Kích thước | Chức năng |
|---|---|---|
| `_ring_gen` | 67 MB | General Satellite — CRUD + text indexing |
| `_ring_vec` | 67 MB | Vector Satellite — ANN search |
| `_ring_proc` | 67 MB | Procedure Satellite — stored procedures |

```python
import qm_engine
import tempfile

# Khởi tạo
ring_dir = tempfile.mkdtemp()
dispatcher = qm_engine.NativeDispatcher(ring_dir)

# DDL operations
dispatcher.dispatch_ddl(b"CREATE TABLE events (id INT, type TEXT, value FLOAT)")

# Insert operations
dispatcher.dispatch_insert(b"INSERT INTO events VALUES (1, 'click', 1.5)")

# Batch insert (hiệu quả cho lượng lớn)
payloads = [
    f"INSERT INTO events VALUES ({i}, 'view', {i * 0.1})".encode()
    for i in range(1000)
]
dispatcher.dispatch_insert_batch(payloads)

# Query / Select
dispatcher.dispatch_query(b"SELECT * FROM events WHERE type = 'click'")
dispatcher.dispatch_select(b"SELECT COUNT(*) FROM events")

# Diagnostics
lsn = dispatcher.current_lsn()
print(f"Current LSN: {lsn}")
print(f"Ring directory: {dispatcher.ring_dir()}")

# Recovery (sau crash)
reports = dispatcher.recover_all()

# Drain tất cả pending messages
drained = dispatcher.drain_all()
print(f"Drained: gen={drained[0]}, vec={drained[1]}, proc={drained[2]}")
```

### 19.2 Raw Ring Buffer API

```python
import qm_engine
import tempfile, os

# Tạo ring buffer trực tiếp
ring_path = os.path.join(tempfile.mkdtemp(), "my_ring.bin")
ring = qm_engine.RustRingBuffer(ring_path, slot_count=1024, slot_data_size=65536)

# Publish message
ring.publish(lsn=1, cmd_type=0, payload=b"SELECT * FROM users")

# Consume message (consumer side)
msg = ring.consume()
if msg:
    slot_idx, lsn, cmd_type, payload = msg
    print(f"Received: LSN={lsn}, cmd={cmd_type}, data={payload}")

    # Complete processing
    ring.complete(slot_idx, b"OK: 5 rows returned")

    # Or fail
    # ring.fail(slot_idx, b"Error: table not found")
```

**Slot State Machine:**

```
Free(0) ──publish──► Writing(1) ──commit──► Committed(2)
                                               │
                                          consume
                                               │
                                               ▼
                                         Processing(3)
                                          │         │
                                     complete      fail
                                          │         │
                                          ▼         ▼
                                       Done(4)  Error(5)
                                          │         │
                                          └──free──►Free(0)
```

---

# PHẦN IV — KẾT NỐI DỮ LIỆU

---

## 20. PostgreSQL Wire Protocol {#20-postgresql-wire-protocol}

### 20.1 Khởi động Gateway

```python
import qm_engine
import time, os

# Cấu hình
os.environ["QM_ADMIN_PASSWORD"] = "my_secure_password"

# Khởi tạo gateway
gw = qm_engine.PostgresGateway(
    host="127.0.0.1",       # Bind address
    port=55433,              # Port (tránh conflict với PostgreSQL 5432)
    max_connections=1000,    # Max concurrent connections
    unix_socket_path=None    # Optional Unix socket
)

# Start
gw.start_native()
time.sleep(0.5)

print(f"Running: {gw.is_running}")
print(f"Config: {gw.get_config()}")
print(f"Connections: {gw.connection_count}")
```

### 20.2 Kết nối từ psql

```bash
# Kết nối
psql -h 127.0.0.1 -p 55433 -U admin

# Sau khi nhập password
CREATE TABLE users (id INT, name TEXT, email TEXT);
INSERT INTO users VALUES (1, 'Alice', 'alice@example.com');
INSERT INTO users VALUES (2, 'Bob', 'bob@example.com');
SELECT * FROM users;
SELECT * FROM users WHERE id = 1;
```

### 20.3 Kết nối từ psycopg2 (Python)

```python
import psycopg2

# Kết nối
conn = psycopg2.connect(
    host="127.0.0.1",
    port=55433,
    user="admin",
    password="my_secure_password",
    dbname="qmvir"        # Bất kỳ tên nào (QMvir single-db)
)
conn.autocommit = True
cur = conn.cursor()

# DDL
cur.execute("CREATE TABLE products (id INT, name TEXT, price FLOAT)")

# Insert
cur.execute("INSERT INTO products VALUES (1, 'Widget', 19.99)")
cur.execute("INSERT INTO products VALUES (2, 'Gadget', 49.99)")

# Select
cur.execute("SELECT * FROM products WHERE price > 20")
rows = cur.fetchall()
for row in rows:
    print(f"  Product: id={row[0]}, name={row[1]}, price={row[2]}")

# Transaction
conn.autocommit = False
try:
    cur.execute("UPDATE products SET price = 24.99 WHERE id = 1")
    cur.execute("INSERT INTO products VALUES (3, 'Doohickey', 9.99)")
    conn.commit()
except Exception as e:
    conn.rollback()
    print(f"Transaction failed: {e}")

cur.close()
conn.close()
```

### 20.4 Kết nối từ SQLAlchemy

```python
from sqlalchemy import create_engine, text

# Connection string — dùng psycopg2 driver
engine = create_engine("postgresql+psycopg2://admin:password@127.0.0.1:55433/qmvir")

with engine.connect() as conn:
    conn.execute(text("CREATE TABLE logs (id INT, msg TEXT)"))
    conn.execute(text("INSERT INTO logs VALUES (1, 'start')"))
    result = conn.execute(text("SELECT * FROM logs"))
    for row in result:
        print(row)
    conn.commit()
```

### 20.5 TLS Encryption

```bash
# Tạo self-signed certificate
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem -days 365 -nodes

# Set environment variables
export QM_TLS_CERT=/path/to/cert.pem
export QM_TLS_KEY=/path/to/key.pem

# Gateway sẽ tự động enable TLS
```

```python
# Kết nối với TLS
conn = psycopg2.connect(
    host="127.0.0.1",
    port=55433,
    user="admin",
    password="password",
    sslmode="require"  # hoặc "verify-full"
)
```

---

## 21. HTTP REST API Gateway {#21-http-rest-api}

### 21.1 Khởi động HTTP Server

```bash
# Start HTTP gateway (port 8400)
python -m gateway.api_http.server

# Hoặc
QM_HTTP_PORT=8400 python -m gateway.api_http.server
```

### 21.2 API Endpoints

**Find / Query:**

```bash
# GET — tìm tài liệu
curl -X POST http://localhost:8400/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "action": "find",
    "entity": "articles",
    "filters": {"status": "published"},
    "select": ["id", "title", "score"],
    "order_by": {"score": "desc"},
    "limit": 10
  }'
```

**Insert:**

```bash
curl -X POST http://localhost:8400/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "action": "insert",
    "entity": "articles",
    "data": {
      "title": "New Article",
      "body": "Content here...",
      "status": "draft"
    }
  }'
```

**Update:**

```bash
curl -X POST http://localhost:8400/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "action": "update",
    "entity": "articles",
    "id": "article_123",
    "data": {"status": "published"}
  }'
```

**Delete:**

```bash
curl -X POST http://localhost:8400/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "action": "delete",
    "entity": "articles",
    "id": "article_123"
  }'
```

**Search (Full-text + Vector):**

```bash
# Lexical search
curl -X POST http://localhost:8400/api/v1/query \
  -d '{
    "action": "search",
    "collection": "articles",
    "text": "database performance",
    "strategy": {"lexical": true},
    "limit": 10
  }'

# Vector search
curl -X POST http://localhost:8400/api/v1/query \
  -d '{
    "action": "search",
    "collection": "articles",
    "text": "database performance",
    "strategy": {"vector": true},
    "limit": 10
  }'

# Hybrid search (lexical + vector)
curl -X POST http://localhost:8400/api/v1/query \
  -d '{
    "action": "search",
    "collection": "articles",
    "text": "database performance",
    "strategy": {"lexical": true, "vector": true},
    "limit": 10
  }'
```

**Aggregate:**

```bash
curl -X POST http://localhost:8400/api/v1/query \
  -d '{
    "action": "aggregate",
    "dataset": "events",
    "group_by": ["event_type"],
    "metrics": [{"count": "*"}, {"sum": "value"}, {"avg": "value"}]
  }'
```

### 21.3 Response Format

```json
{
  "ok": true,
  "data": [...],
  "meta": {
    "total": 42,
    "latency_ms": 12.5,
    "cache_hit": false
  },
  "error": null
}
```

---

## 22. Schema Action DSL (SDK) {#22-schema-action-dsl}

SDK cung cấp **fluent API** để xây dựng queries mà không cần viết SQL/JSON thủ công:

```python
from sdk.schema_action_client.builder import SchemaAction

# === Find với filters ===
query = (SchemaAction("articles")
    .find()
    .where(status="published", category__in=["tech", "ai"])
    .select("id", "title", "score")
    .order_by("score", "desc")
    .limit(10)
    .build())

result = query.to_dict()
# → {"action": "find", "entity": "articles", "filters": {...}, ...}

# === Search (hybrid) ===
query = (SchemaAction("articles")
    .search("machine learning database")
    .strategy(lexical=True, vector=True)
    .limit(20)
    .build())

# === Insert ===
query = (SchemaAction("users")
    .insert({"name": "Alice", "email": "alice@example.com"})
    .build())

# === Update ===
query = (SchemaAction("users")
    .update("user_123")
    .data({"name": "Alice Updated"})
    .build())

# === Delete ===
query = (SchemaAction("users")
    .delete("user_123")
    .build())

# === Aggregate ===
query = (SchemaAction("events")
    .aggregate()
    .group_by("event_type")
    .metrics({"count": "*"}, {"sum": "value"}, {"avg": "value"})
    .build())
```

---

## 23. Kết nối từ Languages khác {#23-kết-nối-languages-khác}

Nhờ PostgreSQL wire protocol, QMvir tương thích với **bất kỳ ngôn ngữ nào** có PostgreSQL driver:

### 23.1 Node.js (pg)

```javascript
const { Client } = require('pg');

const client = new Client({
    host: '127.0.0.1',
    port: 55433,
    user: 'admin',
    password: 'my_password',
    database: 'qmvir'
});

await client.connect();
await client.query('CREATE TABLE items (id INT, name TEXT)');
await client.query("INSERT INTO items VALUES (1, 'Widget')");
const res = await client.query('SELECT * FROM items');
console.log(res.rows);
await client.end();
```

### 23.2 Go (pgx)

```go
package main

import (
    "context"
    "fmt"
    "github.com/jackc/pgx/v5"
)

func main() {
    conn, _ := pgx.Connect(context.Background(),
        "postgres://admin:password@127.0.0.1:55433/qmvir")
    defer conn.Close(context.Background())

    conn.Exec(context.Background(),
        "CREATE TABLE items (id INT, name TEXT)")
    conn.Exec(context.Background(),
        "INSERT INTO items VALUES (1, 'Widget')")

    rows, _ := conn.Query(context.Background(),
        "SELECT * FROM items")
    for rows.Next() {
        var id int
        var name string
        rows.Scan(&id, &name)
        fmt.Printf("id=%d, name=%s\n", id, name)
    }
}
```

### 23.3 Java (JDBC)

```java
import java.sql.*;

public class QMvirExample {
    public static void main(String[] args) throws Exception {
        String url = "jdbc:postgresql://127.0.0.1:55433/qmvir";
        Connection conn = DriverManager.getConnection(url, "admin", "password");

        Statement stmt = conn.createStatement();
        stmt.execute("CREATE TABLE items (id INT, name TEXT)");
        stmt.execute("INSERT INTO items VALUES (1, 'Widget')");

        ResultSet rs = stmt.executeQuery("SELECT * FROM items");
        while (rs.next()) {
            System.out.printf("id=%d, name=%s%n",
                rs.getInt(1), rs.getString(2));
        }
        conn.close();
    }
}
```

### 23.4 Rust (tokio-postgres)

```rust
use tokio_postgres::NoTls;

#[tokio::main]
async fn main() {
    let (client, connection) = tokio_postgres::connect(
        "host=127.0.0.1 port=55433 user=admin password=password dbname=qmvir",
        NoTls,
    ).await.unwrap();

    tokio::spawn(async move { connection.await.unwrap() });

    client.execute("CREATE TABLE items (id INT, name TEXT)", &[]).await.unwrap();
    client.execute("INSERT INTO items VALUES (1, 'Widget')", &[]).await.unwrap();

    let rows = client.query("SELECT * FROM items", &[]).await.unwrap();
    for row in rows {
        let id: i64 = row.get(0);
        let name: &str = row.get(1);
        println!("id={}, name={}", id, name);
    }
}
```

### 23.5 C# (.NET / Npgsql)

```csharp
using Npgsql;

var connStr = "Host=127.0.0.1;Port=55433;Username=admin;Password=password;Database=qmvir";
await using var conn = new NpgsqlConnection(connStr);
await conn.OpenAsync();

await using var cmd = new NpgsqlCommand("SELECT * FROM items", conn);
await using var reader = await cmd.ExecuteReaderAsync();
while (await reader.ReadAsync())
{
    Console.WriteLine($"id={reader.GetInt64(0)}, name={reader.GetString(1)}");
}
```

---

## 24. Import/Export Dữ liệu {#24-import-export}

### 24.1 Import từ CSV

```python
import csv

def import_csv(dispatcher, table_name, csv_path, columns):
    """Import CSV file vào QMvir table."""
    with open(csv_path, 'r') as f:
        reader = csv.DictReader(f)
        batch = []
        for row in reader:
            vals = []
            for col in columns:
                v = row[col]
                # Auto-detect type
                try:
                    vals.append(str(int(v)))
                except ValueError:
                    try:
                        vals.append(str(float(v)))
                    except ValueError:
                        vals.append(f"'{v}'")
            sql = f"INSERT INTO {table_name} VALUES ({', '.join(vals)})"
            batch.append(sql.encode())

            if len(batch) >= 1000:
                dispatcher.dispatch_insert_batch(batch)
                batch = []

        if batch:
            dispatcher.dispatch_insert_batch(batch)

# Sử dụng
import_csv(db, "sales", "data/sales_2025.csv", ["id", "product", "amount", "date"])
```

### 24.2 Import từ Parquet

```python
import pyarrow.parquet as pq

def import_parquet(dispatcher, table_name, parquet_path):
    """Import Parquet file vào QMvir."""
    table = pq.read_table(parquet_path)

    # Auto-create table from schema
    cols = []
    for field in table.schema:
        if field.type in (pa.int64(), pa.int32()):
            cols.append(f"{field.name} INT")
        elif field.type in (pa.float64(), pa.float32()):
            cols.append(f"{field.name} FLOAT")
        else:
            cols.append(f"{field.name} TEXT")
    ddl = f"CREATE TABLE {table_name} ({', '.join(cols)})"
    dispatcher.dispatch_ddl(ddl.encode())

    # Insert rows
    batch = []
    for row_idx in range(table.num_rows):
        vals = []
        for col_idx in range(table.num_columns):
            v = table.column(col_idx)[row_idx].as_py()
            if isinstance(v, str):
                vals.append(f"'{v}'")
            elif v is None:
                vals.append("NULL")
            else:
                vals.append(str(v))
        sql = f"INSERT INTO {table_name} VALUES ({', '.join(vals)})"
        batch.append(sql.encode())

        if len(batch) >= 1000:
            dispatcher.dispatch_insert_batch(batch)
            batch = []

    if batch:
        dispatcher.dispatch_insert_batch(batch)

# Sử dụng
import_parquet(db, "analytics_data", "data/events.parquet")
```

### 24.3 Export ra JSON

```python
import json

def export_json(hub, table_name, output_path):
    """Export table ra JSON file."""
    result = hub.execute_sql(f"SELECT * FROM {table_name}", "", False)
    data = json.loads(result)

    with open(output_path, 'w') as f:
        json.dump(data, f, indent=2, ensure_ascii=False)

# Sử dụng
export_json(hub, "users", "exports/users.json")
```

---

# PHẦN V — VẬN HÀNH

---

## 25. Cấu hình Hệ thống {#25-cấu-hình}

### 25.1 Environment Variables

| Variable | Mặc định | Mô tả |
|---|---|---|
| `QM_DATA_DIR` | `/tmp/qm_data` | Thư mục dữ liệu chính |
| `QM_HOST` | `127.0.0.1` | Địa chỉ bind gateway |
| `QM_PORT` | `55433` | Port PostgreSQL wire protocol |
| `QM_HTTP_PORT` | `8400` | Port HTTP REST API |
| `QM_TLS_CERT` | (trống) | Đường dẫn TLS certificate PEM |
| `QM_TLS_KEY` | (trống) | Đường dẫn TLS private key PEM |
| `QM_ADMIN_PASSWORD` | (random) | Password admin user |
| `QM_MAX_CONNECTIONS` | `1000` | Max kết nối đồng thời |
| `QM_READ_TIMEOUT_MS` | `30000` | Read timeout (ms) |
| `QM_WRITE_TIMEOUT_MS` | `30000` | Write timeout (ms) |
| `QM_WAL_DIR` | `{QM_DATA_DIR}/wal` | Thư mục WAL files |
| `QM_FSYNC_MODE` | `Periodic` | WAL fsync: `Always` / `Periodic` / `None` |
| `QM_BUFFER_POOL_SIZE` | `128MB` | Kích thước cache |
| `QM_COMPILE_THRESHOLD` | `1000` | Ngưỡng JIT compilation |

### 25.2 Ví dụ Cấu hình Production

```bash
# Production environment
export QM_DATA_DIR=/var/lib/qmvir/data
export QM_WAL_DIR=/var/lib/qmvir/wal
export QM_HOST=0.0.0.0
export QM_PORT=55433
export QM_HTTP_PORT=8400
export QM_ADMIN_PASSWORD="$(openssl rand -base64 32)"
export QM_MAX_CONNECTIONS=5000
export QM_FSYNC_MODE=Always
export QM_TLS_CERT=/etc/qmvir/tls/cert.pem
export QM_TLS_KEY=/etc/qmvir/tls/key.pem
export QM_BUFFER_POOL_SIZE=4096MB

# Start
python -m gateway.api_http.server &
```

---

## 26. WAL & Crash Recovery {#26-wal-recovery}

### 26.1 WAL Architecture

```
WAL Segment Files:
  wal/uring_wal_000001.log  (max 1GB mỗi segment)
  wal/uring_wal_000002.log
  ...

Record Format:
  [LSN:8 bytes][RecordType:1][TxnID:8][DataLen:4][Data:N][CRC32:4]
```

### 26.2 WAL Writer

```python
import qm_engine
import tempfile

wal = qm_engine.UringWalWriter(tempfile.mkdtemp())

# Ghi transaction
wal.write_begin(txn_id=1)
wal.write_record("insert", txn_id=1, data=b'{"table":"users","row":{"id":1}}')
wal.write_record("insert", txn_id=1, data=b'{"table":"users","row":{"id":2}}')
wal.flush()  # fsync to disk

# Recovery sau crash
recovered = wal.recover()
print(f"Recovered records: {recovered}")
```

### 26.3 Fsync Modes

| Mode | Mô tả | Durability | Performance |
|---|---|:-:|:-:|
| `Always` | fsync sau mỗi commit | Cao nhất | Chậm nhất |
| `Periodic` | fsync theo batch (mặc định) | Tốt | Trung bình |
| `None` | Để OS quản lý | Thấp nhất | Nhanh nhất |

---

## 27. Monitoring & Metrics {#27-monitoring}

### 27.1 Cache Metrics

```python
cache = qm_engine.WTinyLfuCache(capacity=100000)

# Sau khi sử dụng
hit_rate = cache.hit_rate()
total_weight = cache.weight()
print(f"Cache hit rate: {hit_rate:.1%}")
print(f"Cache weight: {total_weight}")
```

### 27.2 Index Statistics

```python
mgr = qm_engine.IndexManager()

# Liệt kê và check health
for name, table, cols, state, use_count in mgr.list_indexes():
    print(f"Index {name}: table={table}, cols={cols}, state={state}, uses={use_count}")

# Histogram analysis
hist = mgr.histogram("orders", "amount")
if hist:
    min_val, max_val, total, buckets = hist
    print(f"Range: [{min_val}, {max_val}], Total: {total}")
    for bucket in buckets:
        print(f"  Bucket: {bucket}")
```

### 27.3 Ring Buffer Diagnostics

```python
dispatcher = qm_engine.NativeDispatcher(ring_dir)

# Current write position
lsn = dispatcher.current_lsn()
print(f"Current LSN: {lsn}")

# Drain pending messages
gen, vec, proc = dispatcher.drain_all()
print(f"Drained: general={gen}, vector={vec}, procedure={proc}")

# Recovery report
reports = dispatcher.recover_all()
```

---

## 28. Docker Production {#28-docker}

### 28.1 Dockerfile

```dockerfile
FROM python:3.13-slim

WORKDIR /app

# Install QMvir wheel
COPY dist/qm_engine-2.0.0-cp313-cp313-manylinux_2_17_x86_64.manylinux2014_x86_64.whl /tmp/
RUN pip install /tmp/qmvir-*.whl

# Install Python dependencies
COPY requirements.txt .
RUN pip install -r requirements.txt

# Copy application code
COPY . .

# Data directory
RUN mkdir -p /var/lib/qmvir/data /var/lib/qmvir/wal
VOLUME ["/var/lib/qmvir"]

# Environment
ENV QM_DATA_DIR=/var/lib/qmvir/data
ENV QM_WAL_DIR=/var/lib/qmvir/wal
ENV QM_HOST=0.0.0.0
ENV QM_PORT=55433
ENV QM_HTTP_PORT=8400
ENV QM_FSYNC_MODE=Periodic
ENV QM_MAX_CONNECTIONS=1000

# Expose ports
EXPOSE 55433 8400

# Health check
HEALTHCHECK --interval=30s --timeout=5s \
    CMD python -c "import qm_engine; print('OK')" || exit 1

CMD ["python", "-m", "gateway.api_http.server"]
```

### 28.2 Docker Compose

```yaml
version: '3.8'

services:
  qmvir:
    build: .
    ports:
      - "55433:55433"   # PostgreSQL wire protocol
      - "8400:8400"     # HTTP REST API
    volumes:
      - qmvir-data:/var/lib/qmvir
    environment:
      - QM_ADMIN_PASSWORD=${QM_ADMIN_PASSWORD:-secure_password}
      - QM_FSYNC_MODE=Always
      - QM_MAX_CONNECTIONS=2000
      - QM_BUFFER_POOL_SIZE=2048MB
    restart: unless-stopped
    deploy:
      resources:
        limits:
          memory: 4G
          cpus: '4'

volumes:
  qmvir-data:
```

### 28.3 Docker Commands

```bash
# Build
docker build -t qmvir:2.0.0 .

# Run
docker run -d \
  --name qmvir \
  -p 55433:55433 \
  -p 8400:8400 \
  -v qmvir-data:/var/lib/qmvir \
  -e QM_ADMIN_PASSWORD=secure_pass \
  qmvir:2.0.0

# Logs
docker logs -f qmvir

# Shell
docker exec -it qmvir bash

# Connect
psql -h localhost -p 55433 -U admin
```

---

## 29. Xử lý Sự cố {#29-xử-lý-sự-cố}

### 29.1 Lỗi thường gặp

| Lỗi | Nguyên nhân | Giải pháp |
|---|---|---|
| `ModuleNotFoundError: qm_engine` | Chưa install wheel | `pip install qmvir-*.whl` |
| `Address already in use` | Port đã bị chiếm | Đổi `QM_PORT` hoặc kill process cũ |
| `Permission denied (ring buffer)` | Không có quyền ghi | Kiểm tra permissions thư mục data |
| `CRC32 mismatch` | Data corruption | Chạy `dispatcher.recover_all()` |
| `TLS certificate not found` | Sai đường dẫn cert | Kiểm tra `QM_TLS_CERT` / `QM_TLS_KEY` |
| `Connection refused` | Gateway chưa start | Chạy `gw.start_native()` trước |
| `Authentication failed` | Sai password | Kiểm tra `QM_ADMIN_PASSWORD` |

### 29.2 Debug Commands

```python
import qm_engine

# Check module load
print(dir(qm_engine))

# Check gateway status
gw = qm_engine.PostgresGateway()
print(f"Running: {gw.is_running}")
print(f"Config: {gw.get_config()}")

# Check ring buffer health
nd = qm_engine.NativeDispatcher("/path/to/data")
print(f"LSN: {nd.current_lsn()}")
drained = nd.drain_all()
print(f"Drained: {drained}")
reports = nd.recover_all()
print(f"Recovery: {reports}")

# Check cache
cache = qm_engine.WTinyLfuCache(1000)
print(f"Hit rate: {cache.hit_rate()}")
```

### 29.3 Performance Tuning

| Vấn đề | Giải pháp |
|---|---|
| Insert chậm | Dùng `dispatch_insert_batch()` thay vì insert từng row |
| Query chậm trên large table | Tạo index: `CREATE INDEX idx ON table(column)` |
| Memory cao | Giảm `QM_BUFFER_POOL_SIZE`, kiểm tra cache size |
| Disk I/O cao | Đổi `QM_FSYNC_MODE=Periodic` (thay vì Always) |
| Connection timeout | Tăng `QM_READ_TIMEOUT_MS` / `QM_WRITE_TIMEOUT_MS` |

---

# PHỤ LỤC

---

## Phụ lục A. Tham chiếu SQL Commands {#phụ-lục-a}

| Lệnh | Cú pháp | Ví dụ |
|---|---|---|
| `CREATE TABLE` | `CREATE TABLE t (col TYPE, ...)` | `CREATE TABLE users (id INT, name TEXT)` |
| `DROP TABLE` | `DROP TABLE t` | `DROP TABLE old_data` |
| `TRUNCATE` | `TRUNCATE t` | `TRUNCATE logs` |
| `CREATE INDEX` | `CREATE INDEX idx ON t(col)` | `CREATE INDEX idx_name ON users(name)` |
| `DROP INDEX` | `DROP INDEX idx` | `DROP INDEX idx_name` |
| `INSERT` | `INSERT INTO t VALUES (...)` | `INSERT INTO users VALUES (1, 'Alice')` |
| `SELECT` | `SELECT cols FROM t WHERE ... ORDER BY ... LIMIT n` | `SELECT * FROM users WHERE id > 5 LIMIT 10` |
| `UPDATE` | `UPDATE t SET col=val WHERE ...` | `UPDATE users SET name='Bob' WHERE id=1` |
| `DELETE` | `DELETE FROM t WHERE ...` | `DELETE FROM users WHERE id=1` |
| `JOIN` | `SELECT ... FROM a JOIN b ON a.x=b.y` | `SELECT * FROM orders o JOIN users u ON o.uid=u.id` |
| `GROUP BY` | `SELECT col, AGG() FROM t GROUP BY col` | `SELECT dept, COUNT(*) FROM emp GROUP BY dept` |
| `HAVING` | `... GROUP BY col HAVING AGG() > val` | `... HAVING COUNT(*) > 5` |
| `BEGIN` | `BEGIN` hoặc `START TRANSACTION` | `BEGIN` |
| `COMMIT` | `COMMIT` | `COMMIT` |
| `ROLLBACK` | `ROLLBACK` hoặc `ABORT` | `ROLLBACK` |
| `CREATE USER` | `CREATE USER name PASSWORD 'pass'` | `CREATE USER alice PASSWORD 'secret'` |
| `GRANT` | `GRANT privs ON table TO user` | `GRANT SELECT, INSERT ON users TO alice` |
| `REVOKE` | `REVOKE privs ON table FROM user` | `REVOKE DELETE ON users FROM alice` |
| `SET` | `SET variable = value` | `SET search_path = public` |
| `SHOW` | `SHOW variable` | `SHOW max_connections` |
| `EXPLAIN` | `EXPLAIN SELECT ...` | `EXPLAIN SELECT * FROM users` |

**Aggregation Functions:** `COUNT(*)`, `SUM(col)`, `AVG(col)`, `MIN(col)`, `MAX(col)`

**WHERE Operators:** `=`, `!=`, `<>`, `>`, `<`, `>=`, `<=`, `AND`, `OR`, `NOT`, `BETWEEN`, `IN`, `LIKE`, `IS NULL`, `IS NOT NULL`

---

## Phụ lục B. Tham chiếu Python API {#phụ-lục-b}

### Core Classes

```python
import qm_engine

# SQL Parser
parser = qm_engine.SqlParser()
  .parse(sql: str) → dict
  .get_query_type(sql: str) → str

# PostgreSQL Gateway
gw = qm_engine.PostgresGateway(host, port, max_connections, unix_socket_path)
  .start_native()
  .stop()
  .is_running → bool
  .get_config() → dict
  .connection_count → int

# NativeSqlEngine
engine = qm_engine.NativeSqlEngine()
  # Trả về (col_names, col_oids, rows)

# Vector Executor
vec = qm_engine.VectorExecutor(batch_size=1024)
  .batch_dot_product(vectors, query) → list[float]
  .batch_l2_distance(vectors, query) → list[float]
  .search(vectors, query, k) → list[(index, score)]
  .parallel_batch_dot_product(vectors, query) → list[float]
  .batch_search(vectors, queries, k) → list[list[(index, score)]]
  .batch_dot_product_flat(data, dim, query) → list[float]
  .batch_l2_distance_flat(data, dim, query) → list[float]
  .search_flat(data, dim, query, k) → list[(index, score)]
  .batch_search_flat(data, dim, queries_flat, query_dim, k) → list[...]

# JIT Compiler
jit = qm_engine.JitCompiler()
  .filter_eq_i64(data, col_idx, value) → list[int]
  .filter_between_f64(data, col_idx, lo, hi) → list[int]
  .project_f64(col_a, col_b, bias) → list[float]
  .register(hash, expr)
  .record_execution(hash)
  .get_expr(hash) → expr
  .execution_count(hash) → int

# Storage Engine
storage = qm_engine.StorageEngine(data_dir, wal_dir)
  .begin_transaction() → Transaction
  .commit(txn)
  .rollback(txn)
  .flush()

# Transaction
txn = qm_engine.Transaction(txn_id, isolation_level)
  .txn_id → int
  .isolation → str
  .state → str

# W-TinyLFU Cache
cache = qm_engine.WTinyLfuCache(capacity)
  .insert(key, value: bytes)
  .get(key) → bytes | None
  .remove(key)
  .hit_rate() → float
  .weight() → int

# Index Manager
idx = qm_engine.IndexManager()
  .create_index(name, table, columns) → int
  .drop_index(name) → bool
  .list_indexes() → list[(name, table, cols, state, use_count)]
  .evaluate() → decisions
  .apply_decisions()
  .record_query_hit(table, column)
  .record_write(table, column)
  .update_selectivity(table, column, distinct, total)
  .record_numeric_value(table, column, value)
  .update_selectivity_between(table, column, lo, hi)
  .histogram(table, column) → (min, max, total, buckets)

# Hub Engine
hub = qm_engine.HubEngine(data_dir)
  .start()
  .execute_sql(sql, purpose, allow_vector_join) → str (JSON)
  .execute_hash_join_bytes(build, probe, build_key, probe_key) → str

# Native Dispatcher (IPC)
nd = qm_engine.NativeDispatcher(ring_dir)
  .dispatch_ddl(payload: bytes)
  .dispatch_insert(payload: bytes)
  .dispatch_insert_batch(payloads: list[bytes])
  .dispatch_select(sql: str)
  .dispatch_query(sql: bytes)
  .dispatch_query_batch(payloads: list[bytes])
  .collect_result(target, slot_idx) → (status, data)
  .current_lsn() → int
  .ring_dir() → str
  .recover_all() → tuple
  .drain_all() → (int, int, int)

# Ring Buffer
ring = qm_engine.RustRingBuffer(path, slot_count, slot_data_size)
  .publish(lsn, cmd_type, payload)
  .consume() → (slot_idx, lsn, cmd_type, payload) | None
  .complete(slot_idx, result)
  .fail(slot_idx, error)

# WAL Writer
wal = qm_engine.UringWalWriter(dir)
  .write_begin(txn_id)
  .write_record(record_type, txn_id, data)
  .flush()
  .recover() → recovered_records
```

---

## Phụ lục C. Bảng Data Types {#phụ-lục-c}

### SQL Types

| Type | Aliases | Rust Internal | Size | Range |
|---|---|---|---|---|
| `INT` | `INTEGER`, `INT32` | `Cell::Int(i64)` | 8 bytes | ±9.2×10¹⁸ |
| `INT64` | `BIGINT` | `Cell::Int(i64)` | 8 bytes | ±9.2×10¹⁸ |
| `INT16` | `SMALLINT` | `Cell::Int(i64)` | 8 bytes | Stored as i64 |
| `FLOAT` | `FLOAT8`, `DOUBLE` | `Cell::Float(f64)` | 8 bytes | IEEE 754 |
| `FLOAT4` | `REAL` | `Cell::Float(f64)` | 8 bytes | Stored as f64 |
| `TEXT` | `VARCHAR` | `Cell::Text(String)` | Variable | UTF-8 |
| `BOOL` | `BOOLEAN` | `Cell::Int(0\|1)` | 8 bytes | 0 or 1 |
| `NULL` | — | `Cell::Null` | 0 bytes | — |

### Arrow Type Mapping

| Arrow Type | QMvir Type |
|---|---|
| `Int64`, `Int32` | `Cell::Int` |
| `Float64`, `Float32` | `Cell::Float` |
| `Utf8`, `LargeUtf8` | `Cell::Text` |
| Other | `Cell::Text` (fallback) |

### PostgreSQL OID Mapping

| QMvir Type | PostgreSQL OID | pgtype |
|---|---|---|
| `INT` | 20 | `int8` |
| `FLOAT` | 701 | `float8` |
| `TEXT` | 25 | `text` |
| `BOOL` | 16 | `bool` |

---

## Phụ lục D. Environment Variables {#phụ-lục-d}

| Variable | Type | Default | Mô tả |
|---|---|---|---|
| `QM_DATA_DIR` | Path | `/tmp/qm_data` | Thư mục chứa database files |
| `QM_WAL_DIR` | Path | `$QM_DATA_DIR/wal` | Thư mục WAL segments |
| `QM_HOST` | String | `127.0.0.1` | Gateway bind address |
| `QM_PORT` | Integer | `55433` | PostgreSQL wire protocol port |
| `QM_HTTP_PORT` | Integer | `8400` | HTTP REST API port |
| `QM_TLS_CERT` | Path | (none) | TLS certificate file (PEM) |
| `QM_TLS_KEY` | Path | (none) | TLS private key file (PEM) |
| `QM_ADMIN_PASSWORD` | String | (random) | Superuser password |
| `QM_MAX_CONNECTIONS` | Integer | `1000` | Maximum concurrent connections |
| `QM_READ_TIMEOUT_MS` | Integer | `30000` | Client read timeout (ms) |
| `QM_WRITE_TIMEOUT_MS` | Integer | `30000` | Client write timeout (ms) |
| `QM_FSYNC_MODE` | Enum | `Periodic` | `Always` / `Periodic` / `None` |
| `QM_BUFFER_POOL_SIZE` | Size | `128MB` | Buffer pool / cache size |
| `QM_COMPILE_THRESHOLD` | Integer | `1000` | JIT compilation trigger threshold |

---

**Tài liệu này được tạo tự động từ codebase QMvir v2.0.0**  
**Ngày cập nhật:** 02/04/2026  
**Tổng cộng:** 118 Python tests PASSING, 6 cross-platform wheels, ~18K LOC Rust engine  
