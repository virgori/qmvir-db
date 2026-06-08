# QMVIR Current Audit - 2026-06-08

## Tóm tắt điều hành

Trạng thái kỹ thuật hiện tại của QMvir là **khá mạnh ở engine core, chưa sạch để release**.

- Phiên bản repo hiện tại: `qmvir`/`qm_engine` `5.4.0`.
- Rust core và Python test suite đều pass trong audit này.
- Native SQL engine đã có coverage tốt cho SQL scalar, index, WAL, checkpoint, UUID/JSON/identity, vector cache, BM25/HNSW và crash recovery.
- Benchmark bền vững mới nhất cho thấy QMvir **không còn nên claim thắng PostgreSQL toàn diện** khi bật persistent WAL per-commit fsync: QM thắng phần lớn read/index workload, nhưng thua rất xa ở write/delete/commit.
- Worktree hiện có 518 dòng thay đổi theo `git status --short`, gồm rất nhiều staged/untracked/generated artifact. Đây là blocker release lớn nhất.
- `qmvir-studio` chưa xác minh build được trong audit này vì thiếu dependency local (`tsc: command not found`).

Kết luận: **engine có nền tảng tốt cho tiếp tục phát triển và test nội bộ; chưa đạt release gate sạch hoặc production claim rộng.**

## Phạm vi audit

Audit này dựa trên workspace tại `/Users/gengyang/QM` ngày 2026-06-08, không dùng internet và không cài thêm dependency.

Các vùng đã kiểm tra:

- Rust engine: `qm_engine`
- Python package/core/tests: `qm_core`, `search_platform`, `vector_platform`, `tests`
- Benchmark artifacts trong `docs/*.json`
- Desktop app metadata/build smoke: `qmvir-studio`
- Release hygiene qua `git status --short`

## Kết quả kiểm tra vừa chạy

| Hạng mục | Lệnh | Kết quả |
| --- | --- | --- |
| Rust compile smoke | `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | Pass |
| Rust full no-default tests | `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | Pass |
| Python full tests | `python3 -m pytest -q -rxX` | `1139 passed, 12 skipped` |
| Python targeted tests | `tests/test_vector_comprehensive.py tests/test_bm25.py tests/test_core_internals.py` | `91 passed` |
| Benchmark script syntax | `python3 -m py_compile ...` | Pass |
| Studio frontend build | `npm run build` trong `qmvir-studio` | Fail: `tsc: command not found` |

Rust test chi tiết theo output:

- lib tests: `444 passed, 15 ignored`
- component/integration style tests: `35 + 23 + 19 + 2 + 18 + 10 passed`
- doc-tests: `2 ignored`
- tổng pass quan sát được trong run Rust: `551 passed`, không có failure.

## Trạng thái tính năng

| Khu vực | Trạng thái | Nhận xét audit |
| --- | --- | --- |
| Native SQL scalar path | Tốt | Test rộng, benchmark local rất nhanh, hỗ trợ nhiều DML/DDL/query shape. |
| Persistent WAL/checkpoint | Khá | Crash/recovery và process-abort tests pass; per-commit fsync có durability claim rõ hơn trước. |
| Write performance bền vững | Yếu | Per-commit fsync thua PostgreSQL mạnh ở insert/update/delete/commit. |
| Read/index performance | Tốt | Persistent WAL run vẫn thắng PostgreSQL ở select/index/count/range nhiều workload. |
| MVCC | Khá | Rust MVCC integration tests pass; vẫn nên tránh claim SERIALIZABLE/đa session production-grade quá rộng. |
| Vector search/HNSW | Khá tốt | Có deterministic tests, persistence/reload, recall fixture và audit benchmark; cần tiếp tục large-scale/cold-cache proof. |
| BM25/text search | Khá | Python/Rust fixtures tốt hơn trước, có ranking/tie/delete/update checks; relevance corpus lớn vẫn thiếu. |
| Hybrid search | Trung bình-rủi ro | Có tests, nhưng scoring calibration và production relevance chưa đủ mạnh để claim rộng. |
| Sharding/distributed | Rủi ro cao | Routing/replica smoke có test; chưa đủ bằng chứng data movement, failure, HA, transaction consistency. |
| Python/Rust bridge | Khá | Full pytest pass; boundary vẫn có object materialization, chưa zero-copy toàn diện. |
| CLI/package/release | Chưa sạch | Worktree quá rộng, có generated artifacts và env file. |
| qmvir-studio | Chưa xác minh | Có `dist/` cũ và `package-lock.json`, nhưng build hiện tại fail vì thiếu dependency local. |

## Benchmark hiện có

### PostgreSQL comparison - persistent WAL per-commit

File: `docs/postgres_comparison_latest.json`

Metadata quan trọng:

- `qm_mode`: `persistent-wal`
- `qm_sync_policy`: `per_commit_sync`
- `acknowledged_before_fsync`: `false`
- claim scope: durable khi COMMIT/autocommit statement trả về
- workloads: 16
- QM thắng: 9
- PostgreSQL thắng: 7

Một số kết quả:

| Workload | Winner | QM p50 ms | PostgreSQL p50 ms | Tỉ lệ ops QM/PG |
| --- | --- | ---: | ---: | ---: |
| insert | PostgreSQL | 2.9913 | 0.1022 | 0.039 |
| select_by_pk | QM | 0.0075 | 0.0392 | 4.229 |
| update_by_pk | PostgreSQL | 2.9987 | 0.0929 | 0.031 |
| delete_by_pk | PostgreSQL | 5.9982 | 0.1794 | 0.032 |
| indexed_integer_equality | QM | 0.0116 | 0.0409 | 3.277 |
| count_indexed_equality | QM | 0.0046 | 0.0433 | 9.144 |
| predicate_range | QM | 0.0065 | 0.0507 | 7.835 |
| transaction_commit | PostgreSQL | 2.9911 | 0.1599 | 0.058 |
| transaction_rollback | QM | 0.0174 | 0.0875 | 4.571 |
| transaction_insert_100_commit | PostgreSQL | 5.5962 | 3.8833 | 0.703 |

Đánh giá: đây là benchmark trung thực hơn cho durability. QMvir hiện rất nhanh ở read path, nhưng write path bền vững đang bị fsync cost chi phối.

### Group commit comparison

File: `docs/postgres_comparison_persistent_wal_group_commit_latest.json`

- `qm_sync_policy`: `group_commit`
- `acknowledged_before_fsync`: `true`
- QM thắng 14/16 workloads.

Đánh giá: hữu ích để hiểu trần hiệu năng khi batching, nhưng **không tương đương PostgreSQL `synchronous_commit=on`**. Không nên dùng làm claim durability chính.

### In-memory comparison

File: `docs/postgres_comparison_memory_latest.json`

- `qm_mode`: `memory`
- QM thắng 16/16 workloads.

Đánh giá: hợp lệ cho embedded/in-memory/local scalar path; không được dùng để claim PostgreSQL-equivalent durability.

### Native SQL benchmark

File: `docs/native_sql_benchmark_last.json`

Kết quả nổi bật:

| Benchmark | p50 ms | p95 ms | ops/s |
| --- | ---: | ---: | ---: |
| `native_sql.simple_insert` | 0.00429 | 0.00571 | 204525 |
| `native_sql.simple_select` | 0.00350 | 0.00367 | 276145 |
| `native_sql.simple_update` | 0.00296 | 0.00308 | 331108 |
| `native_sql.simple_delete` | 0.00625 | 0.00642 | 157414 |
| `native_sql.mvcc_read_write` | 0.00804 | 0.01088 | 116246 |
| `native_sql.vector_cache_hot_path` | 0.00458 | 0.00583 | 201850 |

Đánh giá: engine local path rất nhanh. Cần tách rõ local/in-memory benchmark với persistent-WAL benchmark trong docs/marketing.

## Release hygiene

`git status --short` hiện có 518 dòng. Các nhóm đáng chú ý:

- Nhiều file staged mới ở Python packages, Rust engine, docs, npm, SDK, Studio.
- `.DS_Store` đang staged dù là artifact không nên commit.
- `npm/.env` đang untracked, cần kiểm tra secret và không commit.
- Binary/static artifacts đang staged hoặc untracked: `lib/libqm_*.a`, `npm/qm-*`, benchmark JSON/report files.
- `qmvir-studio/dist/` tồn tại, nhưng build từ source không xác minh được vì thiếu dependency.

Đánh giá: **không nên cắt release từ worktree này** trước khi split commit, loại artifact không cần thiết và xác minh lại CI từ clean checkout.

## Rủi ro chính

1. **Write durability bottleneck**: per-commit fsync làm insert/update/delete/commit thua PostgreSQL rất mạnh. Cần tối ưu WAL/checkpoint/sync policy hoặc thiết kế batching có semantics rõ.
2. **Release surface quá rộng**: nhiều subsystem mới đang staged cùng lúc, làm review khó và tăng rủi ro regression.
3. **Distributed/HA chưa production-grade**: routing, replica và sharding tests chưa đủ để claim HA/rebalance/failure correctness.
4. **Studio chưa reproducible build trong workspace hiện tại**: thiếu `node_modules` hoặc local TypeScript binary.
5. **Benchmark claim dễ bị hiểu sai**: cần tài liệu phân biệt `memory`, `persistent-wal per_commit_sync`, và `group_commit`.
6. **Generated/secret hygiene**: `.DS_Store`, `npm/.env`, binary blobs và report outputs cần owner review trước commit.

## Khuyến nghị ưu tiên

1. Dọn release hygiene trước: unstage/remove artifact, kiểm tra `npm/.env`, tách commit theo subsystem.
2. Đặt benchmark claim policy trong README/docs:
   - in-memory: dùng cho embedded/local speed
   - persistent per-commit: dùng cho durability-equivalent claim
   - group commit: throughput mode, không claim strict synchronous durability
3. Tối ưu durable write path:
   - giảm số fsync trên autocommit nếu semantics cho phép
   - group commit có wait policy rõ
   - checkpoint materialization tránh chặn mutation hot path
4. Chạy lại release gate từ clean checkout:
   - `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
   - PyO3/default feature Rust tests nếu môi trường Python link sẵn sàng
   - `python3 -m pytest -q -rxX`
   - persistent WAL PostgreSQL comparison
   - `npm ci && npm run build` trong `qmvir-studio`
5. Bổ sung proof cho distributed:
   - rebalance data movement
   - replica failover with writes
   - network partition tests
   - transaction consistency across shards

## Kết luận

QMvir hiện ở trạng thái **engine core pass mạnh, benchmark đã trung thực hơn, nhưng release chưa sẵn sàng**.

Điểm kỹ thuật nội bộ: **7.5/10**.

Điểm release readiness: **5/10** vì worktree bẩn, frontend chưa build reproducibly, và durable write performance còn là nút thắt lớn.

Claim an toàn hiện tại:

- QMvir có read/index/local scalar path rất nhanh.
- Persistent WAL per-commit đã có test và benchmark rõ hơn, nhưng write path không cạnh tranh với PostgreSQL ở chế độ durability nghiêm ngặt.
- Chưa nên claim production replacement cho PostgreSQL trong web/SaaS/HA workloads.
