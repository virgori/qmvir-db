# QMvir / QM Engine — Kiến trúc hiện tại (Rust `qm_engine`)

**Phạm vi:** crate `qm_engine/` (PostgreSQL wire protocol + `NativeSqlEngine`)  
**Đồng bộ code:** nhắm tới **`Cargo.toml` v4.8.x** và cấu trúc module như trong `qm_engine/src/lib.rs`  
**Đọc kèm:** [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md) (danh mục thuật toán & chỉ dẫn file).

---

## 1. Vai trò hệ thống

QMvir là một **Rust core** phục vụ:

- Gateway **PostgreSQL wire protocol v3** (tương thích `psql`, JDBC, v.v.).
- Thực thi SQL qua **`NativeSqlEngine`** — engine đơn khối, tối ưu scan/agg/join/index trong process.
- **FTS + vector (HNSW)** + **OLAP-style** truy vấn trên cùng luồng dữ liệu native.
- **Ảnh chụp (snapshot) + WAL dạng văn bản** cho `data_dir` của native engine; module **`storage/`** chứa thêm WAL nhị phân/MVCC dùng cho API Python / lộ trình tích hợp.

Điều QMvir **không** cam kết trong hot path hiện tại (xem chi tiết §6):

- Giao dịch đa câu lệnh kiểu **Serializable / snapshot isolation** trên gateway native (BEGIN/COMMIT được xác nhận nhưng không gom delta).

---

## 2. Biểu đồ luồng đang chạy (production-shaped)

```
                    ┌─────────────────────────────────────┐
  psql / app        │  PostgresGateway (Tokio TCP/Unix)    │
 ─────────────────► │  Codec PG v3, loop xử lý message     │
                    └──────────────┬──────────────────────┘
                                   │ auth (SCRAM, ACL)
                                   ▼
                    ┌─────────────────────────────────────┐
                    │  NativeSqlEngine (native_sql.rs)     │
                    │  parse/dispatch • columnar/cache     │
                    │  SIMD scan • rayon • index hooks     │
                    └───────┬─────────────────────────────┘
                            │
         ┌──────────────────┼──────────────────┐
         ▼                  ▼                  ▼
  ┌─────────────┐   ┌───────────────┐   ┌─────────────────┐
  │ engines::    │   │ index::       │   │ executor::      │
  │ analytics /  │   │ B+Tree,       │   │ SIMD kernels,   │
  │ vector /     │   │ Inverted,HNSW │   │ JIT (optional), │
  │ compactor    │   │ Roaring, …    │   │ hybrid search   │
  └─────────────┘   └───────────────┘   └─────────────────┘

  Persistence (native_sql data_dir):  snapshot nhị phân + native_sql.wal (dòng SQL)
  Thư viện song song:                  storage::{wal, MVCC snapshot, io_uring, streaming}
```

---

## 3. Cây module Rust (`qm_engine/src`)

| Thư mục | Trách nhiệm trong code thực tế |
|--------|---------------------------------|
| `gateway/` | TCP server PG, protocol, SCRAM; **`native_sql`** = hot path SQL; connection, fts, dsl, CDC hooks, rerank… |
| `parser/` | `sqlparser` + dispatcher chọn nhánh native / hybrid. |
| `executor/` | Toán tử vector hóa, batch, JIT, txn executor phụ trợ, hybrid lexical+vector. |
| `storage/` | **StorageEngine** nhị phân + WAL có CRC + MVCC transaction + snapshot + buffer cache + **`io_uring`** (Linux) + WAL streaming — chủ yếu cho binding / tương lai; **không** thay thế hoàn toàn WAL văn bản của `NativeSqlEngine`. |
| `index/` | B+Tree (mmap page), auto index manager, Roaring, inverted (WAND/BMW), HNSW+PQ, concurrent HNSW, WAL inverted, mmap vector/graph. |
| `engines/` | **v4.8+** tách pool: `AnalyticsEngine` (Rayon), `VectorEngine` (Tokio worker), `CompactorEngine` — singleton process-wide, bật/tắt env. |
| `hub_engine/` | Coordinator / planner / point query / vector gate cho mô hình hub–satellite (Rust). |
| `cluster/` | Shard ring, replica, 2PC, transport TCP — **thư viện**; wiring gateway đầy đủ phụ thuộc lộ trình. |
| `ipc/` | Ring buffer mmap + dispatcher (hub↔satellite); ít dùng khi mọi thứ in-process. |
| `backup/` | Backup/restore/encrypt/snapshot diff/verify. |
| `web/` | Axum: API, studio, WebSocket, dashboard. |
| `cli/` | Binary `qm`: start, sql, backup, bench, … |
| `statistics/` | Bloom, HLL, Count-Min, T-Digest, cost model. |
| `optimizer/` | Adaptive rewrite / cost. |
| `learned/` | Selectivity, cache predictor, fusion weights, intent. |
| `metrics.rs` | Registry metric. |
| `types.rs` | Kiểu dùng chung. |

**Feature `python`:** `pyo3` expose gateway, storage, index, hub, IPC, cache, uring WAL, JIT, `PyNativeSqlEngine`. Build CLI/npm thường dùng **`--no-default-features`** để tránh link Python.

---

## 4. Gateway & thực thi SQL

### 4.1 PostgresGateway

- Async **Tokio** accept, state machine PG (Startup, Auth, Simple/Extended query).
- **SCRAM-SHA-256** + catalog user (xem `gateway/auth.rs`, `scram.rs`).
- Sau auth, câu lệnh SQL đi vào **`NativeSqlEngine`** (không qua Python).

### 4.2 NativeSqlEngine (`gateway/native_sql.rs`)

- **SoA / columnar** cache, dictionary encoding, SIMD equality scan (NEON / AVX2+), **Rayon** chunk song song.
- **JOIN** nhiều dạng (hash/broadcast tùy benchmark path), **GROUP BY** parallel merge, **COPY** Parquet/CSV/binary (theo lộ trình code).
- **Fast path** có thể dispatch sớm cho macro-benchmark / protocol nội bộ (ví dụ token `__QM_FAST_*` nếu có trong tree).
- **IndexManager** gắn B+Tree / FTS / HNSW theo DDL.

### 4.3 Engine con (`engines/`)

- **`QM_ANALYTICS_ENGINE`**, **`QM_VECTOR_ENGINE`**, **`QM_COMPACTOR_ENGINE`**: `0` để tắt, mặc định bật lazy singleton.
- Mục tiêu: giảm tranh chấp Rayon/Tokio giữa OLTP gateway và workload nặng.

---

## 5. Bền vững & phục hồi (native `data_dir`)

| Thành phần | Hành vi (source of truth: `NativeSqlEngine::with_data_dir`) |
|------------|-------------------------------------------------------------|
| Snapshot | Load **bincode** snapshot nếu có (fast path). |
| `native_sql.wal` | **Append-only, mỗi dòng một câu SQL** (text); flush + **fsync** sau mutation. |
| Replay | Đọc lần lượt dòng, parse/execute để tái dựng state in-memory. |
| Checkpoint | Theo ngưỡng mutation (tuning trong code) — ghi snapshot, cắt WAL tùy policy. |

**Lưu ý:** `storage/wal.rs` mô tả WAL **nhị phân có CRC theo record** — đây là **lớp lưu trữ thứ hai** trong crate, dùng cho `StorageEngine` và tương lai hợp nhất, **không** mâu thuẫn có chủ đích với WAL văn bản của native SQL; tài liệu audit cũ từng ghi “text vs binary” — **cả hai tồn tại song song** với vai trò khác nhau.

---

## 6. Giao dịch & MVCC (độ trung thực)

- Trên **`NativeSqlEngine`**, `BEGIN` / `COMMIT` / `ROLLBACK` (và nhiều biến thể savepoint) **được trả lời tương thích wire** nhưng **không** mở transaction đa câu lệnh có isolate snapshot như PostgreSQL.
- Mỗi DML/DDL thành công: cập nhật bảng in-memory + ghi WAL tương ứng (auto-commit per statement).
- **`storage/transaction.rs`** triển khai MVCC & conflict detection cho **mô hình storage engine** — chưa phải bảo đảm toàn bộ đường đi gateway native.

Khi document cho user/operator: nêu rõ **giới hạn transaction** nếu workload cần multi-statement atomicity.

---

## 7. An ninh (tóm tắt)

- SCRAM + ACL (GRANT/REVOKE) trên engine native.
- Rate limit / lockout (xem code auth).
- Backup **AES-GCM**; mật khẩu admin qua CLI/env.

Chi tiết lịch sử rà soát: [CODEBASE_AUDIT_2026_04_01.md](reference/CODEBASE_AUDIT_2026_04_01.md) (Python + Rust lẫn context cũ — đối chiếu với Rust path hiện tại).

---

## 8. Đóng gói & vận hành

| Artifact | Mục đích |
|----------|----------|
| `qm` (CLI) | `cargo build -p qm_engine --release --no-default-features --bin qm` |
| `qm_web` | HTTP dashboard / API |
| `libqm_engine` | rlib + cdylib (Python) |
| `npm/qmvir` | Phân phối binary đa nền + `postinstall` |
| `qm_engine/scripts/build_release.sh` | Ma trận cross-compile; cố định `CARGO_TARGET_DIR` trong script |

---

## 9. Lộ trình / phần “có code, wiring tùy ngữ cảnh”

- **Cluster** (`shard`, `replica`, `two_phase_commit`, `transport`): sẵn sàng ở mức thư viện; cần cấu hình gateway & ops để bật cluster thật.
- **IPC** ring + dispatcher: phục vụ kiến trúc multi-process; mặc định single-process dùng `NativeSqlEngine` trực tiếp.
- **`native_sql_v2_wip.rs`**: biến thể / thử nghiệm — không thay thế file production trừ khi merge có chủ đích.

---

## 10. Liên hệ tài liệu khác

| Doc | Nội dung |
|-----|----------|
| [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md) | Bảng thuật toán & module |
| [ARCHITECTURE.md](reference/ARCHITECTURE.md) | Lịch sử dài (timeline, audit cũ, hub join — **đối chiếu ngày**) |
| [ENGINE_SPLIT_v4.8.0.md](reference/ENGINE_SPLIT_v4.8.0.md) | Engine pool split design |
| [CODEBASE_AUDIT_2026_04_01.md](reference/CODEBASE_AUDIT_2026_04_01.md) | Audit toàn repo (Python + Rust) |

---

*Tài liệu này ưu tiên **trạng thái code** hơn tài liệu marketing. Khi merge thay đổi lớn (WAL thống nhất, MVCC trên gateway), cập nhật §5–§6 và [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md).*
