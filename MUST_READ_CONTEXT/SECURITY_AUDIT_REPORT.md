# QMvir Codebase — Báo Cáo Kiểm Toán Bảo Mật & Lỗi

> **Phiên bản codebase**: QMvir v1.0.0  
> **Ngày kiểm toán**: 2026-03-22  
> **Phạm vi**: Toàn bộ codebase tại `/Users/gengyang/Desktop/AI/QM`

---

## Tổng Quan

| Hạng mục | Số lỗi | Critical | High | Medium |
|---|---|---|---|---|
| 🔐 Bảo mật | 6 | 2 | 2 | 2 |
| 🧠 Tràn bộ nhớ | 5 | 2 | 2 | 1 |
| 🛡️ Bảo vệ dữ liệu | 5 | 1 | 3 | 1 |
| ⚙️ GitHub Workflow | 7 | 2 | 3 | 2 |

---

## 1. 🔐 Lỗi Bảo Mật

### SEC-01 — `CRITICAL` — Mật khẩu Admin mặc định cứng trong code

**File**: `qm_core/auth.py:161-162`

```python
_DEFAULT_ADMIN_USER = "admin"
_DEFAULT_ADMIN_PASS = "admin"     # overridden by env QM_ADMIN_PASSWORD
```

**Vấn đề**: Mật khẩu `admin/admin` được hardcode. Nếu biến môi trường `QM_ADMIN_PASSWORD` không được đặt (trường hợp phổ biến trong dev/staging/Docker), server khởi động với tài khoản admin mật khẩu mặc định. Không có log cảnh báo khi dùng mật khẩu mặc định.

**Sửa**: Buộc đặt `QM_ADMIN_PASSWORD` ở production; ném exception nếu env trống trong production mode; in cảnh báo `[WARNING]` khi dùng default.

---

### SEC-02 — `CRITICAL` — Wire Protocol bypass xác thực hoàn toàn

**File**: `qm_core/wire/__init__.py:329-352`

```python
def handle_startup(self, data: bytes) -> bytes:
    self._params = PgProtocol.parse_startup(data)
    response = bytearray()
    response.extend(PgProtocol.build_auth_ok())  # <-- Luôn OK, không hỏi mật khẩu!
```

**Vấn đề**: `PgSession.handle_startup()` luôn trả về `AuthenticationOk` mà **không hề xác thực** username/password. Bất kỳ client nào kết nối đến cổng PostgreSQL wire đều được trao quyền truy cập đầy đủ mà không cần thông tin xác thực. Module `auth.py` tồn tại nhưng không được gọi trong flow wire protocol.

**Sửa**: Gọi `AuthRequest(5)` (MD5) hoặc `AuthRequest(10)` (SCRAM) trong `handle_startup`; kết nối `PgSession` với `UserCatalog.authenticate()`.

---

### SEC-03 — `HIGH` — SQL Injection trong Extended Query Protocol

**File**: `qm_core/wire/__init__.py:465-470`

```python
@staticmethod
def _substitute_params(sql: str, decoded_params: list[str]) -> str:
    out = sql
    for i, val in enumerate(decoded_params, start=1):
        out = out.replace(f"${i}", val)  # String substitution thô
    return out
```

**Vấn đề**: Tham số `$1`, `$2`... được thay thế bằng chuỗi thô vào SQL. Hàm `_decode_param_value` cố gắng escape với `text_val.replace("'", "''")` nhưng không đầy đủ — với numeric injection path (`is_number=True`), không có escape nào được áp dụng.

**Sửa**: Dùng parameterized query thực sự — pass tham số riêng biệt xuống SQL engine, không nhúng vào string SQL.

---

### SEC-04 — `HIGH` — Session token không được xác minh / Không có session store

**File**: `qm_core/auth.py:228-229`

```python
token = secrets.token_hex(16)
return AuthSession(username=rec.username, role=rec.role, token=token)
```

**Vấn đề**: `AuthSession` tạo token nhưng không có server-side session store để xác minh token. Không có cơ chế validate token này trên các request tiếp theo.

**Sửa**: Tạo `SessionStore` dict ánh xạ `token → AuthSession`; implement middleware kiểm tra token trên mỗi request.

---

### SEC-05 — `MEDIUM` — scrypt parameters thấp hơn khuyến nghị

**File**: `qm_core/auth.py:75-78`

```python
_SCRYPT_N = 2 ** 14   # CPU/memory cost — OWASP 2023 khuyến nghị 2**17
```

**Sửa**: Tăng `_SCRYPT_N = 2**17`; thêm benchmark để đảm bảo không ảnh hưởng UX.

---

### SEC-06 — `MEDIUM` — `check_permission()` chỉ dựa trên class name AST node

**File**: `qm_core/auth.py:273-298`

**Vấn đề**: `GrantStmt`, `RevokeStmt`, `AlterUserStmt` không có trong bảng ACL → các lệnh quản trị user không được bảo vệ nếu có AST node mới được thêm vào mà không cập nhật bảng.

**Sửa**: Dùng allowlist tường minh; thêm unit test kiểm tra mọi loại AST node.

---

## 2. 🧠 Lỗi Tràn Bộ Nhớ

### MEM-01 — `CRITICAL` — Buffer Overflow trong `c_l2_distance`: thiếu kiểm tra chiều

**File**: `qm_native_c/qm_native_c.c:135-148`

```c
static PyObject *c_l2_distance(PyObject *self, PyObject *args) {
    npy_intp n = PyArray_DIM(arr_a, 0);
    // MISSING: Thiếu kiểm tra PyArray_NDIM và so sánh kích thước!
    const float *b = (const float *)PyArray_DATA(arr_b);
    float dist_sq = l2_distance_sq_simd(a, b, (size_t)n);  // Over-read nếu b ngắn hơn a
```

**Vấn đề**: Hàm `c_l2_distance` **không kiểm tra**: (1) mảng có phải 1D không, (2) hai mảng có cùng kích thước không → **heap buffer over-read**, crash hoặc data leak. Ngược lại `c_cosine_distance` có đủ kiểm tra này.

**Sửa**:
```c
if (PyArray_NDIM(arr_a) != 1 || PyArray_NDIM(arr_b) != 1) { ... }
if (PyArray_DIM(arr_a,0) != PyArray_DIM(arr_b,0)) { ... }
```

---

### MEM-02 — `CRITICAL` — `c_batch_l2_distances`: thiếu kiểm tra chiều mảng 2D

**File**: `qm_native_c/qm_native_c.c:197-224`

```c
// MISSING: Thiếu kiểm tra NDIM và dim == vectors_arr.dim[1]
for (npy_intp i = 0; i < n_vectors; i++) {
    const float *v = vectors + i * dim;  // Sai stride nếu dim != vectors_arr.shape[1]
```

**Vấn đề**: Không kiểm tra `PyArray_NDIM(vectors_arr) == 2` và dimension mismatch. Stride `i * dim` sẽ sai → wild-pointer reads trong vòng lặp SIMD chạy bằng OpenMP parallel.

**Sửa**: Copy pattern từ `c_batch_cosine_distances` — thêm đủ kiểm tra NDIM và dimension mismatch.

---

### MEM-03 — `HIGH` — Toàn bộ table rows được load vào RAM, không có limit

**File**: `qm_core/engine.py:376-378`

```python
if not predicates:
    rows = list(state.rows.values())  # Load TOÀN BỘ bảng vào list — O(n) RAM
```

**Sửa**: Implement cursor/lazy iterator; thêm `max_memory_rows` config; implement on-disk B+Tree thay cho in-memory dict cho large tables.

---

### MEM-04 — `HIGH` — WAL write buffer không có giới hạn cứng

**File**: `qm_core/storage/wal.py:279-295`

**Vấn đề**: Nhiều thread đồng thời ghi large data có thể tích lũy vài GB trong `_write_buffer` (bytearray tự mở rộng) trước khi flush. Với group commit interval 5ms, dữ liệu có thể mất nếu crash.

**Sửa**: Giới hạn cứng `MAX_BUFFER_SIZE`; block writer nếu buffer đầy; serialize bên ngoài lock.

---

### MEM-05 — `MEDIUM` — `_segment_max_lsn()` đọc toàn bộ segment 64MB vào RAM

**File**: `qm_core/storage/wal.py:412-427`

```python
buf = seg.read_all()  # Đọc toàn bộ segment (tối đa 64MB) vào RAM chỉ để tìm max LSN
```

**Sửa**: Lưu max LSN trong segment header hoặc sidecar file; hoặc đọc file từ cuối ngược lại.

---

## 3. 🛡️ Lỗi Bảo Vệ Dữ Liệu

### DATA-01 — `CRITICAL` — Checkpoint file không được atomic write

**File**: `qm_core/checkpoint.py:241-247`

```python
with open(filepath, "wb") as f:
    f.write(data)   # Crash ở đây → file corrupt, không có backup!
    f.flush()
    os.fsync(f.fileno())
```

**Sửa**: Write-then-rename: ghi vào `filename.tmp` → fsync → `os.rename(tmp, filepath)` (atomic trên POSIX).

---

### DATA-02 — `HIGH` — MVCC transactions không rollback khi engine crash

**File**: `qm_core/engine.py:265-276`

```python
self._mvcc.commit(txn)       # MVCC committed
# ...
state.rows[doc_id] = doc     # Exception ở đây → MVCC committed nhưng data không có!
```

**Sửa**: Wrap trong try/except với rollback; hoặc dùng MVCC làm single source of truth.

---

### DATA-03 — `HIGH` — Delete không cập nhật MVCC và indexes

**File**: `qm_core/engine.py:337-348`

```python
del state.rows[doc_id]   # Chỉ xóa in-memory dict
# MISSING: Không xóa khỏi B+Tree, HNSW, Inverted Index → ghost records!
```

**Sửa**: Implement proper index cleanup trong mọi index type; gọi `self._mvcc.delete(txn, ...)`.

---

### DATA-04 — `HIGH` — WAL replay không truncate corrupted tail

**File**: `qm_core/storage/wal.py:238-248`

```python
except (ValueError, struct.error):
    break  # Dừng replay, nhưng file vẫn còn corrupt → lần sau vẫn gặp lại
```

**Sửa**: Truncate file tại điểm corrupt sau recovery; log số records bị mất.

---

### DATA-05 — `MEDIUM` — `update()` không log old_data vào WAL

**File**: `qm_core/engine.py:330-335`

```python
self._wal.append(WALOp.UPDATE, ..., data=updates)  # Chỉ log delta, không có old_data
state.rows[doc_id].update(updates)                  # → Không thể rollback!
```

**Sửa**: Snapshot `old_data = state.rows[doc_id].copy()` trước update; pass `old_data=old_data` vào WAL append.

---

## 4. ⚙️ Lỗi GitHub Workflow (`release.yml`)

### WF-01 — `CRITICAL` — `RUSTFLAGS` global bị áp dụng khi cross-compile

**File**: `.github/workflows/release.yml:16-19`

```yaml
env:
  RUSTFLAGS: '-C target-cpu=native'  # detect CPU của runner (x86), không phải target (ARM64)!
```

**Sửa**: Xóa global `RUSTFLAGS`; chỉ set trong từng step `env:`.

---

### WF-02 — `CRITICAL` — Docker build dùng `matrix.platform` không tồn tại

**File**: `.github/workflows/release.yml:307-308`

```yaml
build-args: |
  TARGETPLATFORM=${{ matrix.platform }}  # matrix.platform = "" vì job này không có matrix!
```

**Sửa**: Xóa `build-args:` block; BuildKit tự inject `TARGETPLATFORM` khi build multi-arch.

---

### WF-03 — `HIGH` — `qm_native_c` không có Linux ARM64 build

**File**: `.github/workflows/release.yml:187-223`

**Vấn đề**: Docker ARM64 image cố dùng `qm_native_c-linux-arm64/*.whl` nhưng artifact này không được build → import error hoặc missing SIMD extension trên ARM64.

**Sửa**: Thêm ARM64 cross-compile target dùng `gcc-aarch64-linux-gnu` hoặc QEMU.

---

### WF-04 — `HIGH` — Test matrix thiếu Linux ARM64

**File**: `.github/workflows/release.yml:313-393`

**Vấn đề**: ARM64 wheels không bao giờ được test trước khi release → ARM64-specific bugs có thể ship vào production.

**Sửa**: Thêm test job dùng `ubuntu-22.04` + QEMU ARM64 emulation.

---

### WF-05 — `HIGH` — `maturin build` không dùng `--manylinux` flag

**File**: `.github/workflows/release.yml:104-106` và `176-177`

**Vấn đề**: Wheel build trên ubuntu-22.04 link đến glibc 2.35+. RHEL8/Debian Bullseye dùng glibc 2.28 → `ImportError: GLIBC_2.33 not found`.

**Sửa**:
```yaml
maturin build --release --target x86_64-unknown-linux-gnu \
  --manylinux 2014 --out ../dist/
```

---

### WF-06 — `MEDIUM` — `setup.py bdist_wheel` deprecated

**File**: `.github/workflows/release.yml:214-218`

**Sửa**: Chuyển sang `python -m build --wheel`; explicit compiler flags `-march=armv8-a` (ARM64), `-msse4.2` (x86).

---

### WF-07 — `MEDIUM` — Release không test Linux ARM64 trước khi publish

**File**: `.github/workflows/release.yml:398-401`

**Vấn đề**: `release` phụ thuộc `test` nhưng `test` không có ARM64 matrix → ARM64 wheels được publish mà không qua test nào.

**Sửa**: Thêm ARM64 smoke test; hoặc thêm warning trong release notes.

---

## Tóm Tắt Ưu Tiên Sửa

| Độ ưu tiên | Mã | Mô tả ngắn |
|---|---|---|
| 🔴 **NGAY LẬP TỨC** | SEC-02 | Wire protocol không xác thực — anyone can connect |
| 🔴 **NGAY LẬP TỨC** | SEC-01 | Mật khẩu admin mặc định `admin/admin` |
| 🔴 **NGAY LẬP TỨC** | MEM-01 | C buffer over-read trong `c_l2_distance` |
| 🔴 **NGAY LẬP TỨC** | WF-02 | Docker `matrix.platform` rỗng — build broken |
| 🟠 **TUẦN NÀY** | MEM-02 | C wild-pointer trong `c_batch_l2_distances` |
| 🟠 **TUẦN NÀY** | SEC-03 | SQL Injection numeric path |
| 🟠 **TUẦN NÀY** | DATA-01 | Checkpoint không atomic write |
| 🟠 **TUẦN NÀY** | DATA-03 | Delete không cleanup indexes → ghost records |
| 🟠 **TUẦN NÀY** | WF-01 | `RUSTFLAGS=native` sai khi cross-compile ARM64 |
| 🟠 **TUẦN NÀY** | WF-05 | Thiếu `--manylinux 2014` flag |
| 🟡 **SPRINT TIẾP** | SEC-04 | Session store không tồn tại |
| 🟡 **SPRINT TIẾP** | DATA-02 | MVCC không rollback được |
| 🟡 **SPRINT TIẾP** | MEM-03 | Load toàn bộ table vào RAM |
| 🟡 **SPRINT TIẾP** | WF-03 | Thiếu `qm_native_c` Linux ARM64 build |
| 🟡 **SPRINT TIẾP** | WF-04 | Thiếu test Linux ARM64 |
| 🔵 **BACKLOG** | SEC-05 | scrypt N=2^14 thấp hơn OWASP |
| 🔵 **BACKLOG** | SEC-06 | ACL thiếu GrantStmt/RevokeStmt |
| 🔵 **BACKLOG** | MEM-04 | WAL buffer không có hard limit |
| 🔵 **BACKLOG** | MEM-05 | `_segment_max_lsn` đọc 64MB vào RAM |
| 🔵 **BACKLOG** | DATA-04 | WAL không truncate corrupted tail |
| 🔵 **BACKLOG** | DATA-05 | WAL update không log old_data |
| 🔵 **BACKLOG** | WF-06 | `setup.py bdist_wheel` deprecated |
| 🔵 **BACKLOG** | WF-07 | Release không test ARM64 trước |

---

*Báo cáo này dựa trên phân tích tĩnh (static analysis) của codebase. Một số lỗi có thể được giảm nhẹ bởi các lớp bảo vệ ở tầng network/infrastructure không được kiểm tra trong scope này.*
