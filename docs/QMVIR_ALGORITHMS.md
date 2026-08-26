# QMvir — Danh mục thuật toán & kiến trúc dữ liệu

**Phiên bản:** 6.2.8
Bản kê theo **crate `qm_engine`**, trỏ tới file triển khai chính. Đọc cùng [QMVIR_ARCHITECTURE.md](QMVIR_ARCHITECTURE.md) · [BASIC_USAGE.md](BASIC_USAGE.md) · [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md).

---

## 1. Lưu trữ & bền vững

| Chủ đề | Thuật toán / cấu trúc | Vị trí code (tham khảo) |
|--------|---------------------|-------------------------|
| WAL native SQL | Append log **theo dòng**, đồng bộ theo `WalSyncPolicy` (gateway mặc định `group_commit_sync`) | `gateway/native_sql.rs` (`WalSyncPolicy`, `wal_append`, `replay_wal`) |
| Snapshot engine | Ưu tiên `native_sql.tables.manifest` + snapshot bincode từng bảng; fallback `native_sql.snap` legacy rồi replay delta WAL | `gateway/native_sql.rs` (`load_snapshot`, `replay_wal`) |
| WAL nhị phân + segment | Records + CRC32, rotation | `storage/wal.rs` |
| MVCC | Transactions, snapshots (storage engine path) | `storage/transaction.rs` |
| MVCC table store | MVCC store dùng trực tiếp bởi native gateway | `htap/mod.rs`, `htap/mvcc_store.rs` |
| MVCC transaction manager | Quản lý transaction, commit timestamp, session context | `mvcc/tx_manager.rs` |
| MVCC visibility | Snapshot và kiểm tra visibility theo isolation | `mvcc/visibility.rs` |
| Trang / heap | Page format, serialization | `storage/page.rs` |
| Buffer / cache | W-TinyLFU & biến thể concurrent | `storage/cache.rs` |
| WAL Linux io_uring | Queue async append (Linux) | `storage/uring_wal.rs` |
| Snapshot incremental | Dirty tracking, CRC | `storage/snapshot.rs` |
| Replication streaming | Wal sender/receiver abstraction | `storage/wal_streaming.rs` |

## 1.1. HTAP

| Chủ đề | Thuật toán / cấu trúc | Vị trí code |
|--------|-----------------------|-------------|
| Column segment | Lưu trữ và quét segment dạng cột | `htap/column_segment.rs` |
| Row segment | Lưu trữ segment dạng hàng cho OLTP | `htap/row_segment.rs` |
| Columnizer | Chuyển dữ liệu hàng sang segment cột | `htap/columnizer.rs` |
| HTAP planner | Chọn đường quét và physical plan | `htap/planner.rs` |
| Spill | Spill dữ liệu trung gian ra disk | `htap/spill.rs` |
| PITR | Archive/replay WAL và khôi phục theo thời điểm | `htap/pitr.rs` |
| Certification | Kiểm tra tính đúng đắn của HTAP engine | `htap/certify.rs` |

---

## 2. Index & tìm kiếm

| Chủ đề | Thuật toán / cấu trúc | Vị trí code |
|--------|---------------------|-------------|
| B+Tree | Cây B+ chỉ mục, trang mmap, CRC trang | `index/bplus_tree.rs` |
| Auto indexing | Theo dõi selectivity / cost, quyết định build/drop shadow | `index/auto_manager.rs` |
| Roaring | Nén bitmap (array / bitmap / run) | `index/roaring.rs` |
| Inverted FTS | Postings, BM25, **WAND / Block-Max WAND**, chiến lược truy vết | `index/inverted.rs` |
| HNSW | Đồ thị layered ANN, beam search configurable | `index/hnsw.rs` |
| Product quantization | PQ cho vector nén | `index/hnsw.rs` (PQ types) |
| Concurrent HNSW | Đọc song song / graph có lock | `index/concurrent_hnsw.rs` |
| Sharded HNSW / inverted | Shard theo khóa | `index/sharded.rs` |
| Mmap vector store | Vector cố định chiều trên mmap | `index/mmap_store.rs` |
| WAL inverted | Bảo đảm FTS index có log | `index/wal_inverted.rs` |
| Inverted catalog | Catalog cho inverted index | `index/inverted_catalog.rs` |
| JSON path catalog | Catalog chỉ mục JSON path | `index/json_path_catalog.rs` |
| Trigram catalog | Catalog chỉ mục trigram | `index/trigram_catalog.rs` |
| Vector HNSW catalog | Catalog metadata cho vector HNSW | `index/vector_hnsw_catalog.rs` |
| Search checkpoint | Checkpoint tiến trình search/index | `index/search_checkpoint.rs` |

---

## 3. Thực thi truy vấn & SIMD

| Chủ đề | Thuật toán / kỹ thuật | Vị trí code |
|--------|----------------------|-------------|
| SIMD dot / L2 / sum | NEON (aarch64), AVX2/AVX-512 (x86 runtime detect) | `executor/vectorized.rs` |
| Morsel parallel aggregate | Rayon chunks + SIMD inner (predicated sum, v.v.) | `executor/vectorized.rs`, `gateway/native_sql.rs` |
| Hash join / batch | Parallel build-probe patterns | `executor/join.rs`, `hub_engine/executor.rs` |
| Physical plan hash join | Build/probe keys từ planner | `hub_engine/` |
| JIT / expression cache | Cache biên dịch (optional path) | `executor/jit.rs` |
| Hybrid search | Kết hợp lexical + vector | `executor/hybrid_search.rs` |
| Vectorized operators | Batch toán tử | `executor/operators.rs`, `executor/batch.rs` |

---

## 4. Parser & tối ưu hoá

| Chủ đề | Vị trí code |
|--------|-------------|
| SQL AST / visitor | `parser/query.rs`, `sqlparser` |
| Engine dispatch | Dispatcher chọn `NativeEngine`, `VectorEngine`, `HybridEngine`, hoặc `StorageEngine` | `parser/dispatcher.rs` |
| Adaptive optimizer | `optimizer/adaptive.rs`, `optimizer/rules.rs` |

---

## 5. Thống kê xác suất (sketches)

| Cấu trúc | Ứng dụng | Vị trí |
|----------|----------|--------|
| Bloom | Membership approximate | `statistics/bloom.rs` |
| HyperLogLog | Cardinality | `statistics/hll.rs` |
| Count-Min Sketch | Frequency | `statistics/count_min.rs` |
| T-Digest | Quantile | `statistics/tdigest.rs` |
| Cost model | Ước lượng cost | `statistics/cost_model.rs` |

---

## 6. Learned components

| Thành phần | Vị trí |
|-------------|--------|
| Selectivity model | `learned/selectivity.rs` |
| Cache predictor | `learned/cache_predictor.rs` |
| Fusion weights (hybrid) | `learned/fusion_weights.rs` |
| Intent classifier | `learned/intent.rs` |

---

## 7. Cluster & phân tán (v6.2.x)

| Chủ đề | Thuật toán / kỹ thuật | Vị trí code |
|--------|----------------------|-------------|
| Consistent hashing | Shard ring routing | `cluster/shard.rs` |
| Sync WAL replication | RPO≈0, fsync + peer ack | `cluster/wal_replication.rs` |
| Write quorum | W = ⌊N/2⌋+1 acks | `cluster/wal_replication.rs` |
| Failover | Health probe + promotion | `cluster/failover.rs` |
| Epoch fencing | Stale writer reject | `cluster/fencing.rs` |
| Meta Raft | Leader election, log replicate | `cluster/meta_raft_network.rs` |
| Two-phase commit | Cross-shard atomicity | `cluster/two_phase_commit.rs` |
| PG distributed txn | BEGIN/COMMIT wire | `cluster/pg_distributed.rs` |
| STONITH lease | Primary lease fencing | `cluster/stonith.rs` |
| Witness voter | 2-DC tie-break | `cluster/witness.rs` |
| WAL catch-up | Segment replay on standby | `cluster/wal_catchup.rs` |
| LSN dedupe | Exactly-once apply | `cluster/wal_apply.rs` |
| Chaos battery | Partition, duplicate WAL, RPO/RTO | `cluster/chaos_battery.rs` |

Certification CLI: `qm cluster certify` · chaos tier: `qm cluster certify --chaos`.  
See [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md).

---

## 8. IPC

| Chủ đề | Vị trí |
|--------|--------|
| Mmap ring buffer (producer/consumer) | `ipc/ring_buffer.rs`, `ipc/lsn.rs` |
| Native dispatcher hub↔satellite | `ipc/dispatcher.rs` |

---

## 9. Backup & bảo vệ dữ liệu

| Chủ đề | Vị trí |
|--------|--------|
| Snapshot backup, ZSTD/LZ4, manifest | `backup/backup.rs`, `backup/format.rs` |
| Restore | `backup/restore.rs` |
| Encrypt (AES-GCM) | `backup/encrypt.rs` |
| Verify / diff snapshot | `backup/verify.rs`, `backup/snapshot_diff.rs` |
| Backup prediction / Python bridge | Ước lượng backup và tích hợp PyO3 | `backup/predict.rs`, `backup/pyo3.rs` |
| PostgreSQL compatibility | Hooks tương thích PostgreSQL | `backup/pg_compat.rs` |
| Postgres-compatible pg backup hooks (nếu có) | trong module `backup/` |

---

## 10. Gateway phụ trợ (không đầy đủ liệt kê)

| Miền | File gợi ý |
|------|------------|
| PG protocol & OID | `gateway/protocol.rs`, `gateway/connection.rs` |
| SCRAM | `gateway/scram.rs`, `gateway/auth.rs` |
| Gateway modules hiện có | `gateway/auth.rs`, `gateway/cancel_registry.rs`, `gateway/connection.rs`, `gateway/native_sql.rs`, `gateway/pg_tls.rs`, `gateway/protocol.rs`, `gateway/scram.rs`, `gateway/server.rs`, `gateway/session_pool.rs`, `gateway/stream.rs`, `gateway/table_store.rs` |

---

## 11. Chuẩn dựng (release SIMD)

Rustflags theo triple (ví dụ Linux x86-64-v3, Linux ARM Neoverse, macOS `target-cpu=native`): xem `.cargo/config.toml.example`.

---

*Cập nhật file này khi thêm index mới hoặc thay WAL/store chính thức.*
