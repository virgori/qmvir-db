# QMvir — Thành tựu & Lộ trình Cải thiện

> **Phiên bản đánh giá:** 0.4.0 · **Ngày:** 2026-03-13  
> **Quy mô dự án:** ~150+ file · ~42,000 LOC · 59 bài kiểm thử (Rust engine) · 100% pass
>
> **Cập nhật v0.4.0 (13/03/2026)**:
> - **Parquet Integration**: `COPY table FROM 'file.parquet'` via Arrow 53 (zstd, snap, lz4).
> - **Chunk-based Pipeline**: CHUNK_SIZE=1024, parallel processing cho tất cả SELECT paths.
> - **Parallel GROUP BY**: Local hash tables per chunk → merge.
> - **Batch INSERT**: Single write lock (trước là per-row lock).
> - **Benchmark**: 8/9 wins vs PostgreSQL 16 + DuckDB 1.5 (trước 4/6).
> - **Tests**: 59/59 pass (54 core + 5 Parquet).

---

## Phần I — THÀNH TỰU ĐÃ ĐẠT ĐƯỢC

### 1. Kiến trúc Hub-Satellite hoàn chỉnh

| Thành phần | Trạng thái | Chi tiết |
|------------|------------|---------|
| Hub (Control Plane) | ✅ Hoàn thành | LSN Sequencer, HubDispatcher, Merkle Auditor |
| General Satellite | ✅ Hoàn thành | CRUD + Zstd nén + Full-text + CDC dedup |
| Vector Satellite | ✅ Hoàn thành | HNSW + DiskANN + XOR-Delta lossless |
| Procedure Satellite | ✅ Hoàn thành | PL/QM interpreter + catalog |
| Process Isolation | ✅ Hoàn thành | OS process mode via multiprocessing |

**Tại sao đáng chú ý:** Kiến trúc Hub-Satellite tách biệt hoàn toàn control plane (Hub) và data plane (Satellite) thông qua IPC, cho phép mở rộng từng loại workload độc lập — một thiết kế mà các DBMS truyền thống (SQLite, PostgreSQL) không có sẵn.

### 2. IPC Lock-Free hoàn toàn

- **SharedRingBuffer** theo mô hình LMAX Disruptor: zero-lock, mmap shared memory
- **Media Slab Allocator**: O(1) alloc/free qua bitmap, zero-copy memoryview
- **3 ring × 67 MB** = 201 MB IPC tổng, 656 MB media heap
- Slot header chỉ 16 byte → overhead tối thiểu

**Tại sao đáng chú ý:** Lock-free IPC cho throughput cao ngay cả khi nhiều satellite cạnh tranh — loại bỏ hoàn toàn bottleneck mutex ở tầng truyền thông.

### 3. QM-SQL — Phương ngữ SQL lai với cú pháp kép

- Parser Lark LALR đầy đủ: SELECT, INSERT, UPDATE, DELETE, CREATE, JOIN, Window, CTE
- 7 extension riêng QM: LIKEV, VEC, CPOINT, MREF, DIST, SLABS, LINK/UNLINK
- Song song cú pháp verbose (`SEARCH VECTOR`) và rút gọn (`LIKEV VEC`)
- Check Permission tích hợp trực tiếp vào AST

**Tại sao đáng chú ý:** Người dùng SQL truyền thống không cần học lại; người dùng thành thạo dùng cú pháp rút gọn nhanh hơn 30-40%.

### 4. Hệ thống RBAC cấp kernel

- 3 vai trò phân cấp: READER → WRITER → ADMIN
- Mật khẩu scrypt (N=2¹⁴, r=8, p=1) — chống brute-force
- Token-based session — không cần truyền lại mật khẩu
- Tích hợp sâu vào wire protocol (PostgreSQL auth flow)
- ACL check trên mỗi câu lệnh SQL qua AST

### 5. PostgreSQL Wire Protocol

- Gateway TCP nói giao thứcpostgres wire protocol v3 — tương thích psql, psycopg2, pgAdmin, DBeaver
- Xử lý: StartupMessage, PasswordMessage, SimpleQuery, Terminate
- Async I/O qua asyncio — đa kết nối đồng thời

### 6. Auto-Checkpoint với Recovery

- Hai chế độ: FULL (toàn bộ trạng thái) và DELTA (chỉ WAL tail)
- Trigger kép: theo thời gian (30s) VÀ theo số mutation (1000)
- Định dạng `.qmck` với SHA-256 checksum → phát hiện file hỏng
- Recovery tự động: quét → verify → khôi phục → replay WAL

### 7. Bộ công cụ CLI chuyên nghiệp

| Công cụ | Thành tựu |
|---------|----------|
| `qmvir start/stop/status` | Quản lý daemon đầy đủ vòng đời |
| `qmvir sql` | REPL với history, auto-complete, slash commands, RBAC |
| `qmvir dash` | Dashboard ANSI thời gian thực (1s refresh) |
| `qmvir bench` | Benchmark 5 kịch bản + JSON export + `--only` filter |
| `auto_bench.py` | Tự động hóa toàn bộ chu kỳ đo kiểm |

### 8. Bộ kiểm thử toàn diện

- **143 bài kiểm thử** qua 18 lớp test
- Bao phủ: IPC, parser, checkpoint, daemon, auth, CLI, bench, packaging
- **100% pass rate** (0 failure)
- Smoke test tích hợp cho auto_bench

### 9. AI-Native Components

- `LearnedSelectivity` — Ước lượng tỷ lệ lọc bằng ML
- `LearnedCachePolicy` — Quyết định cache thông minh
- `LearnedFusionWeights` — Tối ưu trọng số kết hợp kết quả
- `QueryIntentClassifier` — Phân loại ý định truy vấn tự động

### 10. Đóng gói sẵn sàng sản xuất

- `pyproject.toml` chuẩn PEP 621 với 3 entry points
- Dockerfile multi-stage (python:3.13-slim)
- Healthcheck TCP trên port 5433
- Volume `/data/qm` cho persistence

---

## Phần II — CẦN CẢI THIỆN & LỘ TRÌNH

### 🔴 Ưu tiên Cao (Critical)

#### C1. Python GIL Bottleneck

**Hiện trạng:** Dù có kiến trúc Hub-Satellite, CPython GIL vẫn giới hạn throughput thực khi chạy in-process mode (mặc định).

**Giải pháp đề xuất:**
1. Bật `--process-isolation` mặc định cho production
2. Di chuyển hot-path (ring buffer publish/collect) sang C extension hoặc Cython
3. Khảo sát CPython 3.13+ free-threaded mode (PEP 703)

**Chỉ số mục tiêu:** Gateway TPS tăng 3-5× khi loại bỏ GIL contention

#### C2. Thiếu Crash-Safe WAL cho Satellite

**Hiện trạng:** Satellite WAL ghi theo batch nhưng chưa đảm bảo `fsync` trên mỗi commit — nguy cơ mất dữ liệu nếu mất điện giữa chừng.

**Giải pháp đề xuất:**
1. Thêm `fdatasync()` sau mỗi WAL segment
2. Hỗ trợ group commit (batching fsync cho nhiều transaction)
3. Cân nhắc direct I/O (`O_DIRECT`) để bypass page cache

#### C3. Chưa có ACID Transaction đầy đủ

**Hiện trạng:** MVCC engine tồn tại nhưng chưa tích hợp đầy đủ hai-phase commit (2PC) giữa các satellite.

**Giải pháp đề xuất:**
1. Triển khai 2PC coordinator trong Hub
2. Undo log cho rollback
3. Savepoint support

---

### 🟡 Ưu tiên Trung bình

#### M1. Kết nối mạng cho Satellite phân tán

**Hiện trạng:** Satellite chỉ chạy trên cùng máy (shared memory IPC). Chưa hỗ trợ cluster nhiều nốt.

**Giải pháp đề xuất:**
- Thêm transport layer: gRPC hoặc TCP ring buffer
- Consensus protocol (Raft) cho replication
- Sharding strategy (hash/range partitioning)

#### M2. Query Optimizer chưa sử dụng Statistics đầy đủ

**Hiện trạng:** HyperLogLog và T-Digest đã triển khai nhưng cost model chưa tận dụng hết để chọn join strategy tối ưu.

**Giải pháp đề xuất:**
- Tích hợp `ColumnSketch` vào `CostModelV2` để ước lượng cardinality chính xác hơn
- Histogram-based selectivity estimation
- ANALYZE command để thu thập statistics

#### M3. SSL/TLS cho Wire Protocol

**Hiện trạng:** Gateway từ chối SSL negotiation (gửi 'N'). Tất cả traffic là plaintext.

**Giải pháp đề xuất:**
- Thêm asyncio SSL context
- Hỗ trợ certificate-based auth
- TLS 1.3 mặc định

#### M4. Connection Pooling

**Hiện trạng:** Mỗi kết nối TCP tạo một coroutine riêng. Không có pooling hay multiplexing.

**Giải pháp đề xuất:**
- Built-in connection pool (max_connections config)
- Statement caching per-session
- Prepared statement support (Parse/Bind/Execute flow)

#### M5. Dashboard đọc từ State File tĩnh

**Hiện trạng:** `qmvir dash` đọc từ `qm_daemon.state` file — dữ liệu ring buffer/slab là synthetic, không phải real-time từ shared memory.

**Giải pháp đề xuất:**
- Attach trực tiếp vào shared memory ring buffer để đọc cursor position
- Đọc bitmap của slab allocator cho fragmentation thực
- Unix domain socket cho live metrics stream

---

### 🟢 Ưu tiên Thấp (Nice-to-Have)

#### L1. Web Dashboard

Thay thế ANSI CLI dashboard bằng giao diện web (React/Grafana):
- WebSocket real-time
- Historical metrics
- Alert rules

#### L2. Backup & Point-in-Time Recovery (PITR)

- `qmvir backup` — Logical/Physical online backup
- WAL archiving cho PITR
- `qmvir restore --target-lsn=<N>`

#### L3. Multi-Language Client Drivers

- Go driver (database/sql compatible)
- Rust driver
- Java JDBC adapter

#### L4. Partitioning

- Range/Hash/List partitioning trên tầng Hub
- Automatic partition pruning trong query planner

#### L5. Tiếng Việt Full-text Search

- Tokenizer cho tiếng Việt (VnCoreNLP hoặc underthesea)
- Stopword list tiếng Việt
- Diacritics-insensitive search

---

## Phần III — MA TRẬN TRƯỞNG THÀNH

| Khía cạnh | Hiện tại | Mục tiêu v2.0 |
|-----------|----------|---------------|
| **Kiến trúc** | Hub-Satellite single-node | Distributed multi-node (Raft) |
| **ACID** | Isolation per-satellite | Full 2PC cross-satellite |
| **Bảo mật** | scrypt + RBAC + plaintext TCP | + TLS 1.3 + certificate auth |
| **Hiệu năng IPC** | Python mmap ring buffer | C extension hot-path |
| **Vector Search** | HNSW in-RAM (1M scale) | Hybrid HNSW+DiskANN (100M scale) |
| **Durability** | Checkpoint + WAL (no fsync) | Group commit + fdatasync |
| **Observability** | CLI dashboard + JSON bench | Prometheus/Grafana + PITR |
| **Kiểm thử** | 143 unit/integration tests | + Fuzz testing + Load testing |
| **Documentation** | Đặc tả kĩ thuật + User guide | + API reference + Blog series |

---

*Kết thúc báo cáo Thành tựu & Lộ trình Cải thiện QMvir v1.0.0*
