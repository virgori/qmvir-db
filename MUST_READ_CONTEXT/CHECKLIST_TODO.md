# QMvir — Checklist Việc Cần Làm

> Dựa trên báo cáo kiểm toán `SECURITY_AUDIT_REPORT.md`  
> Cập nhật: 2026-04-02 — Đã xoá `qm_core/` (84 .py files). Rust-only engine.  
> Tests: 97 pass / 382 fail (cần migration) / 50 collection errors

---

## 🔴 NGAY LẬP TỨC (Blocking — phải fix trước khi deploy bất kỳ đâu)

### Bảo mật
- [x] **[SEC-02]** ✅ Kết nối `PgSession.handle_startup()` với `UserCatalog.authenticate()`
  - Fixed: `handle_startup()` gửi `AuthenticationCleartextPassword`, thêm `handle_password()` gọi `UserCatalog.authenticate()`

- [x] **[SEC-01]** ✅ Xử lý mật khẩu admin mặc định
  - Fixed: `_bootstrap()` raise `RuntimeError` trong production nếu password mặc định; log WARNING trong dev

### C Extension
- [x] **[MEM-01]** ✅ Thêm kiểm tra bounds trong `c_l2_distance()`
  - Fixed: Thêm `PyArray_NDIM` check, size mismatch check, `NPY_FLOAT32` dtype check

### GitHub Workflow
- [x] **[WF-02]** ✅ Sửa `matrix.platform` không tồn tại trong Docker job
  - Fixed: Xóa `build-args: TARGETPLATFORM=${{ matrix.platform }}` (BuildKit tự inject)

---

## 🟠 TUẦN NÀY

### C Extension
- [x] **[MEM-02]** ✅ Thêm kiểm tra bounds trong `c_batch_l2_distances()`
  - Fixed: Thêm ndim check (1D/2D), dimension mismatch check, `NPY_FLOAT32` dtype check

### Bảo mật
- [x] **[SEC-03]** ✅ Sửa SQL Injection trong Extended Query Protocol
  - Fixed: `_decode_param_value()` dùng strict numeric regex, tất cả non-numeric values đều quoted+escaped

### Bảo vệ dữ liệu
- [x] **[DATA-01]** ✅ Atomic checkpoint write
  - Fixed: Write → `.tmp` → `os.fsync()` → `os.rename()` → fsync directory

- [x] **[DATA-03]** ✅ Cleanup indexes khi delete
  - Fixed: `_remove_from_indexes()` helper gọi B+Tree `.delete()` và HNSW `.remove()` trong cả `update()` và `delete()`

### GitHub Workflow
- [x] **[WF-01]** ✅ Xóa global `RUSTFLAGS` ở cấp workflow
  - Fixed: Xóa global `RUSTFLAGS: '-C target-cpu=native'`; mỗi build step dùng `matrix.rustflags`

- [x] **[WF-05]** ✅ Thêm `--manylinux 2014` cho Linux builds
  - Fixed: Cả `qm_engine` và `qm_native` maturin build commands đều có `--manylinux 2014`

---

## 🟡 SPRINT TIẾP THEO

### Bảo mật
- [x] **[SEC-04]** ✅ Tạo server-side session store
  - Fixed: `SessionStore` class với TTL (default 8h), `validate(token)`, `revoke(token)`, `revoke_user()`, `prune_expired()`

### Bảo vệ dữ liệu
- [x] **[DATA-02]** ✅ MVCC rollback khi insert thất bại
  - Fixed: Tất cả DML (`insert`, `update`, `delete`) wrapped trong `txn = begin()` ... `commit()` + `except` → `rollback()` + undo

- [x] **[DATA-04]** ✅ WAL corrupted tail logging
  - Fixed: `open()` recovery loop và `replay()` log WARNING với segment name, position, recovered record count

### GitHub Workflow
- [x] **[WF-03]** ✅ Thêm Linux ARM64 build cho `qm_native_c`
  - Fixed: Thêm matrix entry `qm_native_c-linux-arm64` với `gcc-aarch64-linux-gnu` cross-compiler

- [x] **[WF-04]** ✅ Thêm Linux ARM64 vào test matrix
  - Fixed: Thêm `ubuntu-22.04` ARM64 entry với QEMU aarch64 emulation

---

## 🔵 BACKLOG

### Bảo mật
- [x] **[SEC-05]** ✅ Rate limiting cho authenticate — 10 failed attempts / 300s lockout (`qm_core/auth.py`)
  - Lưu ý: scrypt N giữ 2^14 (macOS OpenSSL memory limit); TODO cho production Linux
- [x] **[SEC-06]** ✅ Thêm `GrantStmt`, `RevokeStmt` vào bảng ACL
  - Fixed: Thêm vào `_ADMIN_STMTS` frozenset trong `qm_core/auth.py`

### Tràn bộ nhớ
- [x] **[MEM-03]** ✅ Implement lazy cursor cho full table scan
  - Fixed: `find()` streaming với early termination khi không cần sort
- [x] **[MEM-04]** ✅ Serialize WAL record ngoài lock
  - Fixed: Pre-serialize body bytes trước khi acquire lock; chỉ header+CRC trong lock
- [x] **[MEM-05]** ✅ Cache max LSN per segment
  - Fixed: `_seg_max_lsn` dict cached on write + recovery; evicted on truncate

### Bảo vệ dữ liệu
- [x] **[DATA-05]** ✅ Log `old_data` vào WAL khi update
  - Fixed: Đã có sẵn — `WALOp.UPDATE` gọi `old_data=old_row` từ phiên trước

### GitHub Workflow
- [x] **[WF-06]** ✅ Chuyển `setup.py bdist_wheel` → `python -m build --wheel`
  - Fixed: Dùng `python -m build --wheel --outdir ../dist/` + thêm `build` vào deps
- [x] **[WF-07]** ✅ ARM64 wheels included in release + Docker ARM64 wheels
  - Fixed: `qm_native_c-linux-arm64` wheel đã được thêm vào Docker wheel assembly + release bundles

---

## ✅ Đã hoàn thành (2026-04-01)

| ID | Mô tả | File chính |
|------|-------|------|
| SEC-01 | Hardcoded admin password → RuntimeError in production | `qm_core/auth.py` |
| SEC-02 | Wire auth bypass → cleartext password flow + UserCatalog | `qm_core/wire/__init__.py` |
| SEC-03 | SQL injection → strict numeric regex + escape | `qm_core/wire/__init__.py` |
| SEC-04 | Server-side session store (TTL 8h, revoke, prune) | `qm_core/auth.py` |
| SEC-05 | Auth rate limiting (10/300s lockout) | `qm_core/auth.py` |
| SEC-06 | GrantStmt/RevokeStmt in ACL | `qm_core/auth.py` |
| SEC-09 | MD5 → SHA-256 cho document hashing | `qm_core/engine.py` |
| MEM-01 | c_l2_distance bounds check | `qm_native_c/qm_native_c.c` |
| MEM-02 | c_batch_l2_distances bounds check | `qm_native_c/qm_native_c.c` |
| MEM-03 | Lazy cursor cho full table scan | `qm_core/engine.py` |
| MEM-04 | WAL serialize outside lock | `qm_core/storage/wal.py` |
| MEM-05 | Cache max LSN per segment | `qm_core/storage/wal.py` |
| DATA-01 | Atomic checkpoint (write-tmp-fsync-rename) | `qm_core/checkpoint.py` |
| DATA-02 | MVCC rollback trên tất cả DML | `qm_core/engine.py` |
| DATA-03 | Index cleanup khi delete/update | `qm_core/engine.py` |
| DATA-04 | WAL corrupted tail logging | `qm_core/storage/wal.py` |
| DATA-05 | Log old_data vào WAL khi update | `qm_core/engine.py` |
| WF-01 | Xóa global RUSTFLAGS | `.github/workflows/release.yml` |
| WF-02 | Xóa matrix.platform trong Docker job | `.github/workflows/release.yml` |
| WF-05 | --manylinux 2014 cho Linux builds | `.github/workflows/release.yml` |
| WF-06 | setup.py → python -m build | `.github/workflows/release.yml` |
| FEAT-01 | Transaction control (BEGIN/COMMIT/ROLLBACK) in wire protocol | `qm_core/wire/__init__.py` |
| FEAT-03 | Foreign key enforcement (_check_foreign_key) | `qm_core/schema.py` |
| FEAT-04 | Deadlock prevention (WaitDiePolicy in _reserve_intent) | `qm_core/storage/mvcc.py` |
| FEAT-05 | Query timeout (statement_timeout_ms) | `qm_core/wire/__init__.py` |
| FEAT-13 | Audit logging (AuditLogger + EventBus subscriber) | `qm_core/audit.py` |
| ARCH-04 | HTTP Gateway dispatch (engine adapter + asyncio server) | `gateway/api_http/server.py` |
| WF-03 | Linux ARM64 build for qm_native_c (cross-compile) | `.github/workflows/release.yml` |
| WF-04 | Linux ARM64 test matrix (QEMU) | `.github/workflows/release.yml` |
| WF-07 | ARM64 wheels in Docker + release bundles | `.github/workflows/release.yml` |

**Ngoài checklist ban đầu** (từ audit report CODEBASE_AUDIT_2026_04_01.md):

| ID | Mô tả | File chính |
|------|-------|------|
| ARCH-02 | Wire observability (METRICS + SlowQueryLog) vào engine | `qm_core/engine.py` |
| ARCH-03 | Wire triggers (BEFORE/AFTER) + events (EventBus) vào DML | `qm_core/engine.py` |
| ARCH-04 | HTTP Gateway dispatch (engine adapter + asyncio) | `gateway/api_http/server.py` |
| FEAT-01 | Transaction control (BEGIN/COMMIT/ROLLBACK) | `qm_core/wire/__init__.py` |
| FEAT-02 | Wire ConstraintChecker vào insert/update/delete | `qm_core/engine.py` |
| FEAT-03 | Foreign key enforcement | `qm_core/schema.py` |
| FEAT-04 | Deadlock prevention (WaitDiePolicy) | `qm_core/storage/mvcc.py` |
| FEAT-05 | Query timeout (statement_timeout_ms) | `qm_core/wire/__init__.py` |
| FEAT-06 | Auto MVCC vacuum (gc() mỗi 500 DML ops) | `qm_core/engine.py` |
| FEAT-13 | Audit logging (AuditLogger + EventBus) | `qm_core/audit.py` |

*Tổng cộng 658 tests pass (pytest), 0 failed.*

---

## Ghi chú

- ~~**Ưu tiên tuyệt đối**: SEC-02 (auth bypass)~~ ✅ ĐÃ FIX
- ~~**Nguy hiểm nhất cho dữ liệu**: DATA-01 (checkpoint corrupt)~~ ✅ ĐÃ FIX
- ~~**Dễ test nhất**: MEM-01, MEM-02~~ ✅ ĐÃ FIX
- ~~**CI vẫn chạy được**: WF-01, WF-02~~ ✅ ĐÃ FIX
- **Còn lại**: SEC-08 (scrypt N trên production Linux)
- **Tất cả 28 mục Python đã fix** (qm_core/ đã bị xoá — Rust-only)
- Tests sau khi xoá qm_core/: **97 pass**, 382 fail (cần rewrite sang Rust), 50 collection errors

### ⚠️ `qm_core/` ĐÃ XOÁ (2026-04-02)
- 84 file .py đã xoá + build/lib/qm_core/
- Tất cả Python implementation nay chỉ còn Rust `qm_engine/`
- 10 test files cần rewrite để dùng Rust API thay vì Python qm_core imports
- Files bị ảnh hưởng: `test_core.py`, `test_distributed.py`, `test_final_sprint.py`, `test_hub_satellite_arch.py`, `test_phase10.py`, `test_phase11.py`, `test_phases789.py`, `test_sql_shell_gateway.py`, `test_task3_integration.py`, `test_vector_gate_policy.py`

---

## 🔧 RUST ENGINE (`qm_engine/src/`) — 13/22 đã fix

### ĐÃ FIX
- [x] **[RS-SEC-01]** ✅ SHA-256 → Argon2id (`auth.rs`)
- [x] **[RS-SEC-02]** ✅ Hardcoded admin/admin → env var `QM_ADMIN_PASSWORD` (`auth.rs`)
- [x] **[RS-SEC-05]** ✅ SQL injection trong `substitute_params()` → reverse-order replace (`connection.rs`)
- [x] **[RS-SEC-06]** ✅ Unsafe `ptr::read` → `read_unaligned` (`page.rs`)
- [x] **[RS-SEC-07]** ✅ Unsafe `transmute` → safe `ptr::read` (`page.rs`)
- [x] **[RS-SEC-08]** ✅ Unaligned `from_raw_parts` → runtime alignment check (`types.rs`)
- [x] **[RS-SEC-10]** ✅ i32 → usize wraparound → bounds check (`protocol.rs`)
- [x] **[RS-DATA-01]** ✅ MVCC conflict detection sai → bỏ `created_by > txn_id` (`txn.rs`)
- [x] **[RS-DATA-02]** ✅ WAL commit durability → xác nhận đúng (`wal.rs`)
- [x] **[RS-DATA-03]** ✅ WAL recovery no-op → trả committed records (`wal.rs`)
- [x] **[RS-DATA-05]** ✅ Snapshot non-atomic → tmp + fsync + rename (`snapshot.rs`)
- [x] **[RS-DATA-06]** ✅ GC aggressive → check deleted_by threshold (`txn.rs`)
- [x] **[RS-FEAT-02]** ✅ Query timeout → `tokio::time::timeout()` 300s default (`connection.rs`)

### CHƯA FIX (cần ticket riêng)
- [x] **[RS-SEC-03]** ✅ TLS support — rustls + tokio-rustls integration (`server.rs`, `connection.rs`)
- [x] **[RS-SEC-04]** ✅ SCRAM-SHA-256 auth — full RFC 5802 server impl (`scram.rs`, `protocol.rs`, `connection.rs`, `auth.rs`)
- [x] **[RS-SEC-09]** ✅ No-auth fallback — `QM_ALLOW_NO_AUTH` env + `require_auth` flag (`connection.rs`)
- [x] **[RS-SEC-11]** ✅ CancelRequest validation — `CANCEL_REGISTRY` DashMap + token verification (`connection.rs`)
- [x] **[RS-FEAT-01]** ✅ Audit logging — structured JSON audit trail (`audit.rs`, `connection.rs`)
- [x] **[RS-FEAT-05]** ✅ Deadlock detection — wait-for graph + DFS cycle detection (`txn.rs`)
- [x] **[RS-FEAT-06]** ✅ UNIQUE constraint enforcement — `is_unique` in IndexMeta + check on INSERT (`auto_manager.rs`, `native_sql/mod.rs`)
- [x] **[RS-ARCH-01]** ✅ Hợp nhất 2 txn systems — IsolationLevel + WAL in MvccStore, thin adapter in storage (`txn.rs`, `transaction.rs`)
- [x] **[RS-ARCH-02]** ✅ Refactor native_sql.rs monolith → directory module `native_sql/mod.rs` (`native_sql/mod.rs`)
