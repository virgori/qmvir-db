# QMvir — Danh mục thuật toán & kiến trúc dữ liệu

Bản kê theo **crate `qm_engine`**, trỏ tới file triển khai chính. Dùng cùng [QMVIR_ARCHITECTURE.md](QMVIR_ARCHITECTURE.md).

---

## 1. Lưu trữ & bền vững

| Chủ đề | Thuật toán / cấu trúc | Vị trí code (tham khảo) |
|--------|---------------------|-------------------------|
| WAL native SQL | Append log **theo dòng**, flush+fsync | `gateway/native_sql.rs` (`wal_append`, `replay_wal`) |
| Snapshot engine | Deserialize snapshot nhị phân + replay delta WAL | `gateway/native_sql.rs` (`load_snapshot`, `replay_wal`) |
| WAL nhị phân + segment | Records + CRC32, rotation | `storage/wal.rs` |
| MVCC | Transactions, snapshots (storage engine path) | `storage/transaction.rs` |
| Trang / heap | Page format, serialization | `storage/page.rs` |
| Buffer / cache | W-TinyLFU & biến thể concurrent | `storage/cache.rs` |
| WAL Linux io_uring | Queue async append (Linux) | `storage/uring_wal.rs` |
| Snapshot incremental | Dirty tracking, CRC | `storage/snapshot.rs` |
| Replication streaming | Wal sender/receiver abstraction | `storage/wal_streaming.rs` |

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
| Dispatch native vs phase2 | `parser/dispatcher.rs` |
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

## 7. Cluster & phân tán

| Chủ đề | Vị trí |
|--------|--------|
| Consistent hashing / shard routing | `cluster/shard.rs` |
| Replica set | `cluster/replica.rs` |
| Two-phase commit | `cluster/two_phase_commit.rs` |
| TCP transport bin | `cluster/transport.rs` |

*(Wiring vào PostgresGateway có thể chưa đầy đủ cho mọi deployment — xem ARCHITECTURE chính.)*

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
| Postgres-compatible pg backup hooks (nếu có) | trong module `backup/` |

---

## 10. Gateway phụ trợ (không đầy đủ liệt kê)

| Miền | File gợi ý |
|------|------------|
| PG protocol & OID | `gateway/protocol.rs`, `gateway/connection.rs` |
| Native SQL FTS gateway | `gateway/fts.rs` |
| SCRAM | `gateway/scram.rs`, `gateway/auth.rs` |
| CDC / tiering / audit | `gateway/cdc.rs`, `gateway/tiering.rs`, `gateway/audit.rs` |
| Reranker | `gateway/reranker.rs` |

---

## 11. Chuẩn dựng (release SIMD)

Rustflags theo triple (ví dụ Linux x86-64-v3, Linux ARM Neoverse, macOS `target-cpu=native`): xem `qm_engine/.cargo/config.toml`.

---

*Cập nhật file này khi thêm index mới hoặc thay WAL/store chính thức.*
