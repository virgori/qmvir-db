# QMvir Database Engine — Báo Cáo Tiến Độ

> Cập nhật: 2026-04-10 | Phiên bản: v2.0.0 | Ngôn ngữ: Rust-first, PyO3 compatibility optional
> Lưu ý: bảng số liệu test lịch sử bên dưới chưa được làm mới toàn bộ; các dòng trạng thái module, CLI, backup và web đã được cập nhật theo build hiện tại.

---

## 1. Tổng Quan

| Chỉ Số | Giá Trị |
|---------|---------|
| **Test passed** | 490 |
| **Test xfailed** | 16 |
| **Test failed** | 0 |
| **File test** | 23 |
| **File Rust (.rs)** | 70 |
| **Module Rust** | 12 (gateway, parser, executor, storage, index, hub_engine, ipc, cluster, backup, cli, web, metrics) |
| **PyO3 classes** | 14 |
| **CLI commands** | 11 |

---

## 2. Trạng Thái Từng Module

### 2.1 Core Engine

| Module | Trạng Thái | Mô Tả |
|--------|:----------:|--------|
| `gateway/native_sql` | ✅ Hoàn thành | SQL engine chính — CREATE/INSERT/SELECT/UPDATE/DELETE, WHERE, ORDER BY, LIMIT, JOIN, aggregates, COPY TO PARQUET |
| `gateway` (pgwire) | ✅ Hoàn thành | PostgreSQL wire protocol, async tokio, SO_REUSEADDR, password auth; remote exposure hiện bị khóa ở loopback cho tới khi TLS hoàn tất |
| `parser` | ✅ Hoàn thành | SQL parser + query dispatcher |
| `executor` | ✅ Hoàn thành | Vectorized execution (SIMD), JIT compiler, batch filter/project, hybrid search, JOIN, aggregation |
| `storage` | ✅ Hoàn thành | WAL (io_uring), snapshot, W-TinyLFU cache, transaction, page manager |
| `index` | ✅ Hoàn thành | B+Tree, IndexManager, auto-index |
| `hub_engine` | ✅ Hoàn thành | Rust-native coordinator — catalog, planner, executor, point query, vector gate |
| `ipc` | ✅ Hoàn thành | Ring buffer (shared memory), native dispatcher, LSN sequencer |
| `types` | ✅ Hoàn thành | Kiểu dữ liệu chung |
| `metrics` | ✅ Hoàn thành | MetricsRegistry cho observability |

### 2.2 Backup & Migration Suite

| Tool | Trạng Thái | File | Mô Tả |
|------|:----------:|------|--------|
| **BackupEngine** | ✅ Hoàn thành | `backup/backup.rs` | Logical backup → `.qmvb` format, bincode serialization, chunked (1024 rows), lz4/zstd/none compression, CRC32 + HMAC-SHA256 |
| **RestoreEngine** | ✅ Hoàn thành | `backup/restore.rs` | Restore từ `.qmvb`, `.qmdiff`, và plain `pg_dump` `.sql`; hỗ trợ `--drop-existing` và filter tables |
| **VerifyEngine** | ✅ Hoàn thành | `backup/verify.rs` | Quick verify (header + CRC32 + HMAC), info() trả metadata chi tiết |
| **Format** | ✅ Hoàn thành | `backup/format.rs` | Binary format: Header(64B) + Manifest(JSON) + Data blocks + Footer(40B) |
| **PyO3 Bindings** | ✅ Hoàn thành | `backup/pyo3.rs` | 4 functions: `backup()`, `backup_verify()`, `backup_info()`, `backup_restore()` |
| **DiffEngine** | 🔴 Stub | `backup/snapshot_diff.rs` | Differential/incremental backup — chỉ emit rows thay đổi. Trả `Err(Unsupported)` |
| **PredictEngine** | 🔴 Stub | `backup/predict.rs` | Dry-run estimation — ước lượng size/duration trước khi backup. Trả `Err(Unsupported)` |
| **pg_compat** | ✅ Hoàn thành | `backup/pg_compat.rs` | PostgreSQL plain `pg_dump` import subset: `CREATE TABLE`, `COPY ... FROM stdin`, `INSERT INTO`, type mapping và explicit column rewrite |

### 2.3 Snapshot & Checkpoint

| Component | Trạng Thái | Mô Tả |
|-----------|:----------:|--------|
| `NativeSqlEngine.checkpoint()` | ✅ Hoàn thành | Bincode snapshot + WAL truncate, auto-checkpoint mỗi 10K mutations |
| `PyNativeSqlEngine.checkpoint()` | ✅ Hoàn thành | PyO3 binding, lỗi nếu engine in-memory |
| `PyNativeSqlEngine.snapshot_info()` | ✅ Hoàn thành | Trả dict: persistent status, snapshot file, WAL file, table/row counts |
| `storage/snapshot.rs` | ✅ Hoàn thành | Incremental page-level snapshots: DirtyTracker, SnapshotWriter, SnapshotReader, SnapshotManager (zstd, CRC32, HMAC) |

### 2.4 CLI (`qm` binary)

| Command | Trạng Thái | Mô Tả |
|---------|:----------:|--------|
| `qm backup` | ✅ | Tạo backup `.qmvb`, chọn compression/tables/PITR |
| `qm restore` | ✅ | Khôi phục từ `.qmvb`, `.qmdiff`, plain `pg_dump` `.sql`, `--drop-existing`, filter tables |
| `qm verify` | ✅ | Kiểm tra integrity, `--info` hiện metadata |
| `qm checkpoint` | ✅ | Force snapshot + truncate WAL |
| `qm inspect` | ✅ | Xem table metadata + sample rows |
| `qm stat` | ✅ | Engine statistics (JSON hoặc text) |
| `qm check` | ✅ | Integrity check (all tables hoặc specific) |
| `qm dump` | ✅ | Export SQL/CSV/JSONL/Parquet |
| `qm schema` | ✅ | Schema diff + migration SQL generation |
| `qm sql` | ✅ | Interactive SQL REPL |
| `qm version` | ✅ | Hiện phiên bản |

### 2.5 Web Dashboard

| Component | Trạng Thái | Mô Tả |
|-----------|:----------:|--------|
| HTTP Server | ✅ Hoàn thành | Axum + HTTP Basic Auth, loopback-only bind, không còn open CORS |
| Dashboard HTML | ✅ Hoàn thành | `static/dashboard.html` |
| 9 API endpoints | ✅ Hoàn thành | health, stats, tables, table detail, WAL status, metrics, query, backup; toàn bộ dashboard nằm sau auth |

### 2.6 Cluster (Distributed)

| Component | Trạng Thái | Mô Tả |
|-----------|:----------:|--------|
| ConsistentHashRing | ✅ Logic done | Virtual-node consistent hashing, add/remove, distribution stats |
| ShardManager | ✅ Logic done | Routing, insert tracking, load balance, scale-out/in |
| ReplicaSet | ✅ Logic done | Primary-secondary, 3 consistency levels, heartbeat, failover, epoch fencing |
| Network transport | 🔴 Chưa có | Chưa có TCP/gRPC inter-node communication |

---

## 3. Test Coverage Chi Tiết

| File Test | Số Test | Lĩnh Vực |
|-----------|--------:|-----------|
| `test_full_engine.py` | 94 | SQL engine E2E qua pgwire |
| `test_rust_engine.py` | 60 | Tất cả 12 PyO3 classes |
| `test_gateway_auth.py` | 47 | Authentication, SCRAM, audit |
| `test_distributed_sharding.py` | 35 | Consistent hashing, shard routing |
| `test_core_internals.py` | 33 | MVCC, WAL, CDC, SchemaRegistry |
| `test_indexing_comprehensive.py` | 33 | B+Tree, auto-index |
| `test_wal_concurrency.py` | 32 | WAL writer, StorageEngine txn |
| `test_storage_compression.py` | 31 | Compression algorithms |
| `test_vector_comprehensive.py` | 31 | Vector search, embeddings |
| `test_search_comprehensive.py` | 23 | BM25, full-text search |
| `test_backup_suite.py` | 18 | Backup + Verify + Info + Restore |
| `test_snapshot_checkpoint.py` | 11 | Checkpoint, persistence, snapshot_info |
| `test_schema_action.py` | 9 | Schema operations |
| `test_cache.py` | 8 | W-TinyLFU cache |
| `test_vector.py` | 8 | Vector basic |
| `test_btree.py` | 7 | B+Tree basic |
| `test_planner.py` | 7 | Query planner |
| `test_columnar.py` | 6 | Columnar analytics |
| `test_mvcc.py` | 6 | MVCC isolation |
| `test_bm25.py` | 5 | BM25 ranking |
| `test_restart.py` | 2 | SO_REUSEADDR rapid restart |
| `test_parquet.py` | 0 | (script, không phải pytest) |
| `test_cli_e2e.py` | 0 | (script, không phải pytest) |
| **TỔNG** | **490** | |

---

## 4. Remaining Stubs — Cần Implement

### 4.1 Phase 3: DiffEngine (Differential Backup)
- **File**: `backup/snapshot_diff.rs`
- **Mục tiêu**: So sánh state hiện tại vs `.qmvb` baseline, chỉ export rows thay đổi (dựa trên LSN)
- **Output**: `.qmdiff` file
- **Ước lượng**: Cần thêm `last_modified_lsn` vào `NativeRow` + diff algorithm + writer
- **Ưu tiên**: Trung bình — hữu ích cho backup tăng dần khi data lớn

### 4.2 Phase 4: pg_compat (PostgreSQL Import)
- **Trạng thái**: Hoàn thành cho plain-text `pg_dump` (`.sql`)
- **Hỗ trợ hiện tại**: `CREATE TABLE`, `COPY ... FROM stdin`, `INSERT INTO`, type mapping sang `INTEGER` / `REAL` / `TEXT`
- **Giới hạn hiện tại**: chưa hỗ trợ custom/binary dump format, TLS cho remote import không liên quan đến tool này

### 4.3 Phase 5: PredictEngine (Dry-Run Estimation)
- **File**: `backup/predict.rs`
- **Mục tiêu**: Scan tables → ước lượng backup size + duration mà không ghi file
- **Ưu tiên**: Thấp — nice-to-have cho UX

### 4.4 Cluster Network Transport
- **Mục tiêu**: Thêm TCP/gRPC layer để ShardManager và ReplicaSet hoạt động cross-node
- **Ưu tiên**: Cao nếu cần multi-node deployment

---

## 5. Kế Hoạch Tiếp Theo (Đề Xuất)

### Đợt A — Consolidation (Ưu tiên cao)
1. **DiffEngine**: Implement differential backup dựa trên LSN tracking
2. **COPY FROM**: Hỗ trợ import CSV/JSONL (ngược lại với `qm dump`)
3. **Test cho web dashboard**: API endpoint tests
4. **Benchmark documentation**: Ghi lại benchmark results (bench_engine.rs)

### Đợt B — Production Hardening
5. **TLS cho pgwire**: Encrypted connections
6. **Connection pooling**: Giới hạn concurrent connections
7. **Query timeout**: Tự động kill slow queries
8. **EXPLAIN plan**: Query execution plan visualization

### Đợt C — Distributed Features
9. **Inter-node transport**: gRPC/TCP cho ShardManager
10. **Distributed transactions**: 2PC hoặc Raft consensus
11. **Replication streaming**: WAL shipping giữa primary-secondary

### Đợt D — Advanced Features
12. **PredictEngine**: Backup size estimation
13. **Materialized views**: Cached query results
14. **Triggers & stored procedures**: Server-side logic

---

## 6. Build & Run

```bash
# Build Rust binaries
cargo build --manifest-path QM/qm_engine/Cargo.toml \
	--no-default-features --features auto-initialize \
	--bin qm --bin qm_web

# Run targeted Rust tests
cargo test --manifest-path QM/qm_engine/Cargo.toml --lib

# CLI help
cargo run --manifest-path QM/qm_engine/Cargo.toml --bin qm -- --help

# Authenticated dashboard (loopback only)
cargo run --manifest-path QM/qm_engine/Cargo.toml --bin qm_web -- \
	--data-dir ./data --host 127.0.0.1 --admin-password change-me
```

---

*Current release posture: local-only build is hardened; remote pgwire release still needs transport security before non-loopback exposure is acceptable.*
