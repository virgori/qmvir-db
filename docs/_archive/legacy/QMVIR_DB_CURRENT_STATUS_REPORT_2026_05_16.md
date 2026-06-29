# Báo cáo tình hình hiện tại của qmvir-db

**Ngày lập báo cáo:** 16/05/2026  
**Workspace:** `/Users/gengyang/QM`  
**Phạm vi:** đánh giá nhanh hiện trạng mã nguồn, tài liệu, cấu hình đóng gói và kết quả kiểm thử cục bộ.

## 1. Tóm tắt điều hành

qmvir-db hiện là một hệ cơ sở dữ liệu hybrid tập trung vào Rust core, kết hợp OLTP, OLAP, full-text search, vector search, cache, backup, CLI, web dashboard và PostgreSQL-compatible gateway. Phiên bản hiện tại trong các manifest chính là **5.4.0**.

Trạng thái kỹ thuật nhìn chung là **đã có nền tảng chức năng rộng và test coverage Rust lớn**, nhưng **chưa ở trạng thái repository sạch để phát hành ngay**. Lý do chính là integration test MVCC đang fail, workspace có rất nhiều thay đổi chưa commit, nhiều file duplicate dạng ` 2.*`, warning compile còn nhiều, và Python test suite chưa chạy được do thiếu `pytest` trong môi trường hiện tại.

## 2. Phiên bản và cấu hình đóng gói

| Khu vực | File | Trạng thái |
|---|---|---|
| Rust engine | `qm_engine/Cargo.toml` | `version = "5.4.0"`, Rust 2021, crate type `cdylib` + `rlib` |
| Python package | `pyproject.toml` | `name = "qmvir"`, `version = "5.4.0"`, Python `>=3.11`, build bằng `maturin` |
| NPM package | `npm/package.json` | `name = "qmvir"`, `version = "5.4.0"`, binary `qm`/`qmvir` |
| Studio UI | `qmvir-studio/package.json` | Solid/Vite/Tauri app, version `1.0.0` |

Ghi chú: metadata license chưa thống nhất. `pyproject.toml` dùng `Proprietary`, trong khi `npm/package.json` và README chính ghi `MIT`.

## 3. Kiến trúc hiện tại

Rust core nằm tại `qm_engine/`, với các module chính:

- `gateway`: PostgreSQL wire protocol, auth, SCRAM, SQL gateway.
- `parser`: phân loại và parse query.
- `executor`: aggregate, join, vectorized execution, JIT, hybrid search.
- `storage`: WAL, page storage, snapshot, cache, transaction, uring WAL.
- `index`: B+Tree, HNSW, concurrent HNSW, inverted index, roaring, mmap store, sharded index, WAL inverted.
- `mvcc`: transaction manager, visibility, lock manager, row version.
- `backup`: backup, restore, verify, encryption, pg compatibility, snapshot diff.
- `cluster`: shard, replica, transport, two-phase commit.
- `statistics`: Bloom, HLL, TDigest, Count-Min, cost model.
- `optimizer`, `learned`, `procedures`, `search`, `web`, `cli`, `ipc`.

Số liệu cục bộ:

| Chỉ số | Giá trị |
|---|---:|
| Rust source chính trong `qm_engine/src` | 111 file `.rs` |
| Dòng Rust chính trong `qm_engine/src` | ~57,729 dòng |
| Python test file chính trong `tests/` | 34 file |
| Markdown docs cấp `docs/` | 44 file |
| File duplicate dạng `* 2.*` sau khi loại trừ Tauri target | 186 file |

## 4. Kết quả kiểm thử cục bộ

Lệnh đã chạy:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
```

Kết quả:

| Nhóm test | Kết quả |
|---|---|
| Rust lib unit tests | 396 passed, 15 ignored, 0 failed |
| Rust bin tests | 0 tests, pass |
| `bench_engine` test target | 15 ignored, pass |
| `bench_new_components` | 35 passed |
| `mvcc_integration` | 12 passed, 2 failed |

Hai test fail:

- `tests::test_concurrent_read_write`
- `tests::test_write_set_tracking`

Ý nghĩa: phần Rust core có nền test rất lớn và nhiều module đang pass, nhưng trạng thái hiện tại **không đạt green test suite** vì integration test MVCC fail. Đây là blocker nếu chuẩn bị release hoặc merge.

Python test chưa chạy được:

```bash
pytest -q
```

Kết quả:

```text
zsh:1: command not found: pytest
```

Vì vậy chưa thể xác nhận tình trạng Python integration/legacy layer trong môi trường này.

## 5. Điểm mạnh hiện tại

- Engine Rust đã bao phủ nhiều capability quan trọng: SQL CRUD, transaction, MVCC, WAL, snapshot, index, vector search, full-text search, statistics, backup, CLI và web API.
- PyO3 được đặt sau feature flag `python`, giúp build Rust core không cần Python khi dùng `--no-default-features`.
- Test Rust unit và component có độ phủ rộng: lib pass 396 test, `bench_new_components` pass 35 test.
- Đã có nhiều tài liệu trạng thái, kiến trúc, benchmark, release report và usage guide.
- Có packaging đa kênh: Rust binary, Python/maturin, NPM binary wrapper, Tauri studio.

## 6. Rủi ro và vấn đề cần xử lý

| Mức độ | Vấn đề | Tác động |
|---|---|---|
| Cao | `mvcc_integration` fail 2 test | Rủi ro correctness ở concurrency/write-set tracking |
| Cao | Workspace rất bẩn, nhiều file `A/AM/??` và diff lớn | Khó review, khó release, khó xác định nguồn thay đổi |
| Cao | 186 file duplicate dạng `* 2.*` | Dễ build nhầm, khó bảo trì, tăng nhiễu trong review |
| Trung bình | Nhiều warning compile Rust unused/dead_code | Làm giảm tín hiệu CI, che warning quan trọng hơn |
| Trung bình | Python test chưa chạy do thiếu `pytest` | Chưa xác nhận lớp Python/legacy/SDK |
| Trung bình | License metadata không thống nhất | Rủi ro pháp lý/đóng gói khi public package |
| Thấp-Trung bình | Tài liệu có nhiều phiên bản cũ và số liệu benchmark lịch sử | Dễ gây nhầm giữa trạng thái đã công bố và trạng thái repo hiện tại |

## 7. Đánh giá mức sẵn sàng

| Hạng mục | Đánh giá |
|---|---|
| Rust core functionality | Mạnh, nhiều module pass test |
| MVCC/integration stability | Chưa đạt, đang có regression hoặc test expectation lệch |
| Python layer | Chưa xác minh được trong môi trường hiện tại |
| Packaging metadata | Có đủ nền tảng nhưng cần chuẩn hóa |
| Release readiness | Chưa sẵn sàng |
| Internal development readiness | Có thể tiếp tục phát triển, nhưng nên dọn repo và sửa test fail trước |

## 8. Khuyến nghị ưu tiên

1. Sửa hai failure trong `qm_engine/tests/mvcc_integration.rs` trước khi mở rộng tính năng mới.
2. Tách rõ source chính và file duplicate `* 2.*`; xóa hoặc đưa ra khỏi tree build/review nếu đó là bản sao ngoài ý muốn.
3. Làm sạch compile warning Rust, ít nhất ở các module hot path: `gateway/native_sql.rs`, `executor`, `index`, `mvcc`, `storage`.
4. Cài môi trường Python dev rồi chạy `pytest -q` để xác nhận test suite ngoài Rust.
5. Chuẩn hóa license và repository metadata giữa README, `pyproject.toml`, `npm/package.json`.
6. Chốt một tài liệu trạng thái chính cho version 5.4.0, tránh để report cũ v2/v4 gây hiểu nhầm là trạng thái mới nhất.

## 9. Kết luận

qmvir-db hiện đã vượt giai đoạn prototype đơn giản: codebase có Rust engine lớn, nhiều module database chuyên sâu và test Rust đáng kể. Tuy nhiên, tình hình hiện tại là **mạnh về năng lực kỹ thuật nhưng chưa sạch về release discipline**. Việc cần làm ngay không phải là thêm feature, mà là ổn định MVCC integration, làm sạch repo, chạy lại full CI và chuẩn hóa tài liệu/metadata cho version 5.4.0.
