# QMvir / QM Engine — Kiến trúc hiện tại (Rust `qm_engine`)

**Phạm vi:** crate `qm_engine/` (PostgreSQL wire protocol + `NativeSqlEngine`)  
**Phiên bản:** **6.2.8** - Rust-first release surface  
**Đồng bộ code:** `qm_engine/Cargo.toml`, module tree trong `qm_engine/src/lib.rs`  
**Đọc kèm:** [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md) · [Basic Usage](../en/BASIC_USAGE.md) · [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md)

---

## 1. Vai trò hệ thống

QMvir là một **Rust core** phục vụ:

- Gateway **PostgreSQL wire protocol v3** (tương thích `psql`, JDBC, v.v.).
- Thực thi SQL qua **`NativeSqlEngine`** — engine đơn khối, tối ưu scan/agg/join/index trong process.
- **FTS + vector (HNSW)** + **OLAP-style** truy vấn trên cùng luồng dữ liệu native.
- **Ảnh chụp (snapshot) + WAL dạng văn bản** cho `data_dir` của native engine; module **`storage/`** chứa thêm WAL nhị phân/MVCC cho lớp lưu trữ phụ trợ và hướng hợp nhất dài hạn.
- **HTAP + MVCC READ COMMITTED** gắn trực tiếp vào `NativeSqlEngine`: transaction rõ ràng, autocommit DML qua transaction ngầm, row visibility và column segments.

Điều QMvir **không** cam kết trong hot path hiện tại (xem chi tiết §6):

- Giao dịch kiểu **Serializable / snapshot isolation cố định tại BEGIN** như PostgreSQL.
- `SAVEPOINT` / `ROLLBACK TO SAVEPOINT` / `RELEASE SAVEPOINT` trên gateway native.

---

## 2. Biểu đồ luồng chính

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
         ┌──────────────────┼──────────────────┬──────────────────┐
         ▼                  ▼                  ▼                  ▼
  ┌─────────────┐   ┌───────────────┐   ┌─────────────────┐  ┌──────────────┐
  │ htap::      │   │ index::       │   │ executor::      │  │ storage::    │
  │ MVCC,       │   │ B+Tree,       │   │ SIMD kernels,   │  │ binary WAL,  │
  │ column seg, │   │ Inverted,HNSW │   │ JIT (optional), │  │ snapshot,    │
  │ PITR        │   │ Roaring, …    │   │ hybrid search   │  │ io_uring     │
  └─────────────┘   └───────────────┘   └─────────────────┘  └──────────────┘

  Persistence (native_sql data_dir):  per-table bincode snapshots + native_sql.wal
  HTAP durability side data:           row_segments/, column_segments/, spill/, WAL archive/PITR metadata
```

---

## 3. Cây module Rust (`qm_engine/src`)

| Thư mục | Trách nhiệm trong code thực tế |
|--------|---------------------------------|
| `gateway/` | TCP server PG, protocol, SCRAM; **`native_sql`** = hot path SQL; connection, FTS/DSL hooks, CDC-oriented plumbing. |
| `parser/` | `sqlparser` + dispatcher chọn nhánh native / hybrid. |
| `executor/` | Toán tử vector hóa, batch, JIT-expression infrastructure, txn executor phụ trợ, hybrid lexical+vector. |
| `storage/` | **StorageEngine** nhị phân + WAL có CRC + MVCC transaction + snapshot + buffer cache + **`io_uring`** (Linux) + WAL streaming - lớp lưu trữ song song; **không** thay thế WAL văn bản của `NativeSqlEngine`. |
| `index/` | B+Tree (mmap page), auto index manager, Roaring, inverted (WAND/BMW), HNSW+PQ, concurrent HNSW, WAL inverted, mmap vector/graph. |
| `htap/` | Runtime HTAP gắn vào `NativeSqlEngine`: `TransactionManager`, `TableMvccStore`, row/column segments, columnizer, planner, spill, PITR, isolation/certify. |
| `mvcc/` | Transaction manager READ COMMITTED, visibility, row versions, lock manager, executor helper. |
| `hub_engine/` | Coordinator / planner / point query / vector gate cho mô hình hub–satellite (Rust). |
| `cluster/` | Shard ring, sync WAL replication, failover, fencing, meta Raft, 2PC, STONITH, witness, chaos battery — opt-in via `QM_CLUSTER_*`; phải certify trong môi trường deploy thật trước khi claim production. |
| `ipc/` | Ring buffer mmap + dispatcher (hub↔satellite); ít dùng khi mọi thứ in-process. |
| `backup/` | Backup/restore/encrypt/snapshot diff/verify. |
| `web/` | Axum: local dashboard/API loopback (`qm_web`). |
| `cli/` | Binary `qm`: start, sql, backup, bench, … |
| `search/` | Synonym/search helpers. |
| `procedures/` | PL/QM procedure catalog/runtime phụ trợ. |
| `statistics/` | Bloom, HLL, Count-Min, T-Digest, cost model. |
| `optimizer/` | Adaptive rewrite / cost. |
| `learned/` | Selectivity, cache predictor, fusion weights, intent. |
| `metrics.rs` | Registry metric. |
| `types.rs` | Kiểu dùng chung. |

**Feature `python`:** `pyo3` expose một số API cho test/bridge. Build CLI/release mặc định dùng **`--no-default-features`** để giữ surface Rust-only.

---

## 4. Gateway & thực thi SQL

### 4.1 PostgresGateway

- Async **Tokio** accept, state machine PG (Startup, Auth, Simple/Extended query).
- **SCRAM-SHA-256** + catalog user (xem `gateway/auth.rs`, `scram.rs`).
- Sau auth, câu lệnh SQL đi vào **`NativeSqlEngine`** (không qua Python).

### 4.2 NativeSqlEngine (`gateway/native_sql.rs`)

- **SoA / columnar** cache, dictionary encoding, SIMD equality scan (NEON / AVX2+), **Rayon** chunk song song.
- **JOIN** nhiều dạng (hash/broadcast tùy path), **GROUP BY** parallel merge, **COPY** Parquet import và `COPY FROM STDIN` qua wire protocol. CLI dump hỗ trợ SQL/CSV/JSONL/Parquet.
- **Fast path** có thể dispatch sớm cho macro-benchmark / protocol nội bộ (ví dụ token `__QM_FAST_*` nếu có trong tree).
- **IndexManager** gắn B+Tree / FTS / HNSW theo DDL.

### 4.3 HTAP runtime (`htap/` + `mvcc/`)

- `NativeSqlEngine::with_data_dir` tạo `HtapRuntime` và đăng ký session cho mỗi engine/session PG.
- `BEGIN` mở transaction trong `TransactionManager`; autocommit DML dùng transaction ngầm.
- `COMMIT` publish MVCC versions, đánh dấu column segments dirty, cập nhật WAL archive/PITR metadata.
- `ROLLBACK` abort MVCC transaction và phục hồi heap/index bằng undo/snapshot nội bộ.
- Planner HTAP chọn row scan, column scan, index point, HNSW vector scan hoặc FTS/inverted scan theo ngữ cảnh.

---

## 5. Bền vững & phục hồi (native `data_dir`)

| Thành phần | Hành vi (source of truth: `NativeSqlEngine::with_data_dir`) |
|------------|-------------------------------------------------------------|
| Snapshot | Ưu tiên load **per-table bincode snapshot** qua `native_sql.tables.manifest`; fallback `native_sql.snap` legacy nếu manifest chưa có. |
| `native_sql.wal` | **Append-only text WAL**, mỗi record là SQL mutation; COMMIT transaction có thể ghi batch nhiều dòng. |
| Sync policy | Không nói cứng "fsync sau mỗi mutation": engine hỗ trợ `append_only_profile`, `per_mutation_sync`, `per_commit_sync`, `per_commit_sync_data`, `relaxed_os_buffered`, `group_commit_sync`; gateway CLI mặc định set `group_commit_sync`, có thể override bằng `QMVIR_WAL_SYNC_POLICY`. |
| Replay | Load snapshot rồi replay WAL delta theo thứ tự để tái dựng state in-memory. |
| Checkpoint | Theo ngưỡng mutation: ghi snapshot per-table, manifest, index/search catalogs; có background checkpoint và compatibility marker `native_sql.snap`. |

**Lưu ý:** `storage/wal.rs` mô tả WAL **nhị phân có CRC theo record** — đây là **lớp lưu trữ thứ hai** trong crate, dùng cho `StorageEngine` và tương lai hợp nhất, **không** mâu thuẫn có chủ đích với WAL văn bản của native SQL; tài liệu audit cũ từng ghi “text vs binary” — **cả hai tồn tại song song** với vai trò khác nhau.

---

## 6. Giao dịch & MVCC (độ trung thực)

- Trên **`NativeSqlEngine`**, `BEGIN` / `COMMIT` / `ROLLBACK` là transaction thật ở mức gateway native: có `TransactionState`, undo rows, inserted rows, deferred index work, staged WAL và `mvcc_tx_id`.
- Isolation hiện là **READ COMMITTED**: mỗi statement lấy snapshot mới; own writes visible; in-flight writes của transaction khác bị lọc.
- Autocommit DML dùng transaction ngầm: BEGIN → mutation → COMMIT.
- `COMMIT` publish MVCC versions, flush deferred index work, ghi WAL batch theo policy, đánh dấu HTAP column dirty.
- `ROLLBACK` phục hồi heap/index/tombstone state bằng undo rows hoặc snapshot nội bộ, rồi abort MVCC transaction.
- `SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, `RELEASE SAVEPOINT` **không được hỗ trợ**.
- `storage/transaction.rs` vẫn là mô hình storage engine nhị phân riêng; đường gateway native dùng thêm `htap/` + `mvcc/` và `TransactionState` trong `gateway/native_sql.rs`.

Khi document cho user/operator: nêu rõ **giới hạn isolation** nếu workload cần Serializable hoặc snapshot cố định toàn transaction.

---

## 7. An ninh (tóm tắt)

- SCRAM + ACL (GRANT/REVOKE) trên engine native.
- Rate limit / lockout (xem code auth).
- Backup **AES-GCM**; mật khẩu admin qua CLI/env.

Các audit lịch sử Python-era đã bị loại khỏi active tree; tài liệu này ưu tiên trạng thái Rust hiện tại.

---

## 8. Đóng gói & vận hành

| Artifact | Mục đích |
|----------|----------|
| `qm` (CLI) | `cargo build -p qm_engine --release --no-default-features --bin qm` |
| `qm_web` | Local loopback HTTP dashboard / API |
| `libqm_engine` | rlib + cdylib, Python bridge khi bật feature `python` |
| `.github/workflows/release-binaries.yml` | Build Linux/macOS/Windows binaries trên GitHub Actions, tránh Mac local quá tải |
| `scripts/sync_and_build_release_quizzman.sh` | Helper tùy chọn: sync qua SSH và build trên server `quizzman` |

---

## 9. Enterprise cluster (v6.2.x)

Cluster mode is **opt-in** (`QM_CLUSTER_ENABLE=1`). Single-node remains the default.

```
┌─────────────┐     sync WAL      ┌─────────────┐
│  Node A     │◄─────────────────►│  Node B     │
│  (primary)  │   TLS transport   │  (standby)  │
└──────┬──────┘                   └──────┬──────┘
       │         meta Raft / 2PC          │
       └──────────────┬───────────────────┘
                      ▼
              qm cluster certify
```

| Layer | Modules | Purpose |
|-------|---------|---------|
| Transport | `cluster/transport.rs` | Inter-node TCP + TLS |
| WAL replication | `cluster/wal_replication.rs`, `wal_buffer.rs` | Sync replication / write quorum; RPO claim cần validate bằng certify/soak |
| Failover | `cluster/failover.rs`, `fencing.rs` | Auto promotion, epoch fencing |
| Meta catalog | `cluster/meta_raft_network.rs` | Networked Raft quorum |
| Distributed txn | `cluster/two_phase_commit.rs`, `pg_distributed.rs` | Cross-shard atomic batches |
| STONITH | `cluster/stonith.rs` | Primary lease fencing |
| Witness | `cluster/witness.rs` | 2-DC tie-break voter |
| Catch-up | `cluster/wal_catchup.rs` | Durable segment catch-up on standby |
| Certification | `cluster/certify.rs`, `readiness.rs` | Tier scoring + CLI gates |
| Chaos | `cluster/chaos_battery.rs` | In-process jepsen-style scenarios |

**Certification tiers:** `community` → `enterprise-certified` → `production-multi-dc-full` → `jepsen-certified` (`qm cluster certify --chaos`). Đây là framework chứng nhận trong code; mỗi deployment cần chạy gate/soak thật trước khi dùng trong claim thương mại.

Full env vars, topology examples, and validation scripts: **[ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md)**.

---

## 10. Công nghệ đột phá / chưa claim rộng

- **Cluster** (`cluster/*`): có code và certification gates; enable via `QM_CLUSTER_*`. Chỉ claim production cho môi trường đã pass certify/soak tương ứng.
- **IPC** ring + dispatcher: phục vụ kiến trúc multi-process; mặc định single-process dùng `NativeSqlEngine` trực tiếp.
- **`native_sql_v2_wip.rs`**: biến thể / thử nghiệm — không thay thế file production trừ khi merge có chủ đích.
- **JIT native machine code:** hiện có JIT expression IR/cache và vectorized interpretation; phần native-code backend cần wiring/benchmark end-to-end trước khi claim.
- **Adaptive indexing:** có observer/manager và hooks; policy tự động bật/tắt index là hướng tối ưu theo workload, chưa nên trình bày như tính năng production mặc định.
- **CDC/streaming platform:** có hooks/WAL streaming modules; chưa claim như ingestion platform hoàn chỉnh nếu chưa có guide vận hành và test production riêng.

---

## 11. Liên hệ tài liệu khác

| Doc | Nội dung |
|-----|----------|
| [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md) | Bảng thuật toán & module |
| [BASIC_USAGE.md](../en/BASIC_USAGE.md) | Hướng dẫn cơ bản |
| [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md) | HA enterprise / multi-DC |
| [HTAP_GUIDE.md](HTAP_GUIDE.md) | HTAP, MVCC, planner, PITR |

---

*Tài liệu này ưu tiên **trạng thái code** hơn tài liệu marketing. Khi merge thay đổi lớn (WAL thống nhất, isolation Serializable/snapshot-isolation, savepoint), cập nhật §5–§6 và [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md).*
