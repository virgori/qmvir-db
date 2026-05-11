# QMvir — Giáo trình Hướng dẫn Sử dụng
## Từ Cơ bản đến Nâng cao — Tận dụng Tối đa Hiệu năng

> **Phiên bản:** 1.0.0 · **Ngày:** 2026-03-08  
> **Yêu cầu:** Python ≥ 3.11 · macOS / Linux · RAM ≥ 2 GB

---

## Mục lục

**Phần A — Cơ bản**
1. [Cài đặt & Khởi động](#chương-1-cài-đặt--khởi-động)
2. [Lệnh đầu tiên — CRUD cơ bản](#chương-2-lệnh-đầu-tiên--crud-cơ-bản)
3. [Quản lý người dùng & Phân quyền](#chương-3-quản-lý-người-dùng--phân-quyền)

**Phần B — Trung cấp**
4. [Vector Search — Tìm kiếm ngữ nghĩa](#chương-4-vector-search--tìm-kiếm-ngữ-nghĩa)
5. [Media Management — Quản lý Blob](#chương-5-media-management--quản-lý-blob)
6. [Checkpoint & Recovery](#chương-6-checkpoint--recovery)
7. [Giám sát hệ thống](#chương-7-giám-sát-hệ-thống)

**Phần C — Nâng cao**
8. [Tối ưu hiệu năng SQL](#chương-8-tối-ưu-hiệu-năng-sql)
9. [IPC Tuning — Điều chỉnh Ring Buffer & Slab](#chương-9-ipc-tuning--điều-chỉnh-ring-buffer--slab)
10. [Process Isolation & Đa Satellite](#chương-10-process-isolation--đa-satellite)
11. [Benchmark & Phân tích Bottleneck](#chương-11-benchmark--phân-tích-bottleneck)
12. [Triển khai Docker Production](#chương-12-triển-khai-docker-production)
13. [Kết nối từ ứng dụng bên ngoài](#chương-13-kết-nối-từ-ứng-dụng-bên-ngoài)

**Phụ lục**
- [A. Bảng tham chiếu QM-SQL](#phụ-lục-a-bảng-tham-chiếu-qm-sql)
- [B. Bảng cấu hình & Flag](#phụ-lục-b-bảng-cấu-hình--flag)
- [C. Xử lý sự cố](#phụ-lục-c-xử-lý-sự-cố)

---

# PHẦN A — CƠ BẢN

---

## Chương 1. Cài đặt & Khởi động

### 1.1 Cài đặt từ mã nguồn

```bash
# Clone và vào thư mục dự án
cd QM

# Tạo virtual environment
python3 -m venv .venv
source .venv/bin/activate

# Cài đặt ở chế độ editable
pip install -e ".[dev]"
```

Sau khi cài đặt thành công, bạn có 3 lệnh:

| Lệnh | Mô tả |
|-------|-------|
| `qmvir` | Lệnh chính — quản lý daemon và mọi tính năng |
| `qm-server` | Bí danh tương thích ngược cho `qmvir` |
| `qm-bench` | Chạy benchmark nhanh |

### 1.2 Kiểm tra cài đặt

```bash
qmvir version
```

Kết quả mong đợi:

```
QM Database v1.0.0
  Engine:  v2.0.0-hub
  Kernel:  v1.0.0
  Python:  3.13.7
```

### 1.3 Khởi động Daemon

```bash
# Khởi động với cấu hình mặc định
qmvir start

# Hoặc tùy chỉnh
qmvir start --port 5433 --data-dir /ssd/qm_data
```

Daemon sẽ in log bootstrap:

```
08:30:01 [qm.daemon] INFO === QM Daemon Bootstrap ===
08:30:01 [qm.daemon] INFO Data directory: /tmp/qm_data
08:30:01 [qm.daemon] INFO [0/4] RBAC User Catalog initialized (1 users)
08:30:01 [qm.daemon] INFO [1/4] Initializing Hub Engine...
08:30:01 [qm.daemon] INFO [2/4] Initializing Media Heap...
08:30:01 [qm.daemon] INFO [3/4] Initializing Checkpoint Manager...
08:30:01 [qm.daemon] INFO [4/4] Scanning for .qmck recovery files...
08:30:01 [qm.daemon] INFO === QM Daemon Ready (0.42s) ===
```

### 1.4 Kiểm tra trạng thái

```bash
qmvir status
```

```
QM Daemon Status
  PID:      12345 (running)
  Data Dir: /tmp/qm_data
  Gateway:  127.0.0.1:5433
  Engine:   2.0.0-hub
  Media:    656.0 MB
```

### 1.5 Dừng Daemon

```bash
qmvir stop
```

Daemon sẽ:
1. Đóng gateway (từ chối kết nối mới)
2. Ghi checkpoint cuối cùng (final CPOINT FULL)
3. Dừng satellite
4. Dọn dẹp file PID

---

## Chương 2. Lệnh đầu tiên — CRUD cơ bản

### 2.1 Mở SQL Shell

```bash
# Khuyến nghị: client mode qua daemon (mặc định)
qmvir --data-dir /tmp/qm_data sql -u admin -p admin

# Chế độ tự dò daemon (hữu ích khi đổi data-dir liên tục)
qmvir --data-dir /tmp/qm_data sql --daemon auto -u admin -p admin

# Ép kết nối daemon cụ thể
qmvir sql --daemon on --host 127.0.0.1 --port 55433 -u admin -p admin

# Local engine (offline/debug, dữ liệu không tương đương server persistent)
qmvir sql --daemon off -u admin -p admin
```

> **Quan trọng:** `qmvir sql` hiện mặc định dùng **gateway client mode** để dữ liệu bền như server thật.
> 
> Bạn có thể chạy lệnh này ở **bất kì thư mục nào** bằng đường dẫn tuyệt đối:

```bash
/Users/gengyang/Desktop/AI/.venv/bin/qmvir --data-dir /tmp/qm_data sql -u admin -p admin
```

```
QMvir SQL Shell v1.0.0
Type /h for help, /q to quit.
Connected as: admin (ADMIN)

qmvir>
```

### 2.2 Tạo bảng

```sql
qmvir> CREATE TABLE users (
   ...     id INT,
   ...     name TEXT,
   ...     email TEXT
   ... );
```

> **Mẹo:** Câu lệnh đa dòng kết thúc bằng `;`. Khi chưa nhập `;`, shell hiển thị `...` để tiếp tục.

> **Mẹo nhanh:** Trong gateway mode, bạn có thể query trực tiếp bảng mà **không cần `USE`**.
> 
> `USE`/`CREATE DATABASE` chủ yếu dành cho local compatibility mode (`--daemon off`).

### 2.3 Thêm dữ liệu

```sql
qmvir> INSERT INTO users (id, name, email) VALUES (1, 'Nguyễn Văn A', 'a@email.com');
qmvir> INSERT INTO users (id, name, email) VALUES (2, 'Trần Thị B', 'b@email.com');
qmvir> INSERT INTO users (id, name, email) VALUES (3, 'Lê Văn C', 'c@email.com');
```

### 2.4 Truy vấn

```sql
-- Lấy tất cả
qmvir> SELECT * FROM users;
id | name          | email
---+---------------+-------------
1  | Nguyễn Văn A  | a@email.com
2  | Trần Thị B    | b@email.com
3  | Lê Văn C      | c@email.com
(3 rows)

-- Lọc theo điều kiện
qmvir> SELECT name, email FROM users WHERE id = 2;
name        | email
------------+-------------
Trần Thị B  | b@email.com
(1 row)
```

### 2.5 Cập nhật & Xóa

```sql
-- Cập nhật
qmvir> UPDATE users SET email = 'new_a@email.com' WHERE id = 1;

-- Xóa
qmvir> DELETE FROM users WHERE id = 3;
```

### 2.6 Slash Commands

| Lệnh | Tác dụng |
|-------|---------|
| `/d` | Liệt kê tất cả bảng |
| `/u` | Liệt kê người dùng |
| `/h` | Trợ giúp |
| `/q` | Thoát shell |

```
qmvir> /d
Tables:
  users                 (2 rows)
```

---

## Chương 3. Quản lý người dùng & Phân quyền

### 3.1 Hệ thống vai trò (RBAC)

QMvir có 3 cấp quyền phân tầng:

```
ADMIN (cấp 3)   ← Toàn quyền
  ↑ bao gồm
WRITER (cấp 2)  ← Đọc + Ghi dữ liệu
  ↑ bao gồm
READER (cấp 1)  ← Chỉ đọc
```

| Vai trò | Lệnh được phép |
|---------|----------------|
| **READER** | SELECT, LIKEV (tìm kiếm vector), MREF, DIST |
| **WRITER** | + INSERT, UPDATE, DELETE, LINK/UNLINK media |
| **ADMIN** | + CREATE TABLE, DROP, CPOINT, SLABS, GRANT/REVOKE |

### 3.2 Bảo mật mật khẩu

Mật khẩu được hash bằng **scrypt** (N=16384, r=8, p=1) với salt ngẫu nhiên 16 byte — một trong những thuật toán hash mạnh nhất hiện tại, chống brute-force và GPU attack.

### 3.3 Đăng nhập với vai trò khác nhau

```bash
# Admin — toàn quyền
qmvir sql -u admin -p admin_password

# Reader — chỉ đọc (sẽ bị từ chối nếu INSERT)
qmvir sql -u reader_user -p reader_password
```

Nếu reader cố gắng INSERT:

```
qmvir> INSERT INTO users (id, name) VALUES (4, 'Test');
ERROR: Permission denied: role READER cannot execute INSERT
       (PostgreSQL error code: 42501)
```

---

# PHẦN B — TRUNG CẤP

---

## Chương 4. Vector Search — Tìm kiếm ngữ nghĩa

> **Cập nhật gateway:** `LIKEV`/`SEARCH VECTOR` đã hỗ trợ qua daemon mode (`--daemon auto|on`).
> Nếu kết quả rỗng, hãy kiểm tra dữ liệu vector đã được nạp cho bảng đích.

### 4.1 Khái niệm

Vector search cho phép tìm dữ liệu **tương tự về ngữ nghĩa** thay vì so khớp chính xác. Mỗi hàng dữ liệu mang một vector embedding (mảng số thực) biểu diễn ý nghĩa của nó.

### 4.2 Tạo bảng với cột Vector

```sql
qmvir> CREATE TABLE documents (
   ...     id INT,
   ...     title TEXT,
   ...     content TEXT,
   ...     embedding VECTOR(128)
   ... );
```

`VECTOR(128)` nghĩa là mỗi embedding có 128 chiều (dimension).

### 4.3 Chèn dữ liệu với Embedding

```sql
qmvir> INSERT INTO documents (id, title, embedding)
   ...     VALUES (1, 'AI là gì?', '[0.12, -0.45, 0.78, ...]');
```

> **Thực tế:** Embedding thường được sinh bởi mô hình AI (OpenAI, Sentence-BERT, v.v.) rồi chèn vào QMvir.

### 4.4 Tìm kiếm Vector — Cú pháp rút gọn

```sql
-- Tìm 5 tài liệu gần nhất với vector truy vấn
qmvir> LIKEV VEC [0.10, -0.42, 0.80, ...] IN documents TOP 5;
```

**Các metric khoảng cách hỗ trợ:**

| Metric | Ý nghĩa | Khi nào dùng |
|--------|---------|-------------|
| `cosine` | Độ tương tự cosin | Văn bản, NLP (mặc định) |
| `l2` | Khoảng cách Euclid | Ảnh, tọa độ |
| `dot` / `ip` | Tích vô hướng | Recommendation |

```sql
-- Chỉ định metric
qmvir> LIKEV VEC [0.1, 0.2, ...] IN documents TOP 10 METRIC l2;
```

### 4.5 Cú pháp đầy đủ (tương đương)

```sql
qmvir> SEARCH VECTOR [0.1, 0.2, ...] IN documents TOP 5 METRIC cosine;
```

### 4.6 Cột khoảng cách (DIST)

```sql
-- Trả về khoảng cách cùng với kết quả
qmvir> SELECT DIST, title FROM documents
   ...     WHERE embedding LIKEV [0.1, 0.2, ...] TOP 5;
```

### 4.7 Hiệu năng Vector Search

| Quy mô | HNSW (RAM) | DiskANN (SSD) | Ghi chú |
|--------|------------|---------------|---------|
| 1 000 | ~200 µs p99 | — | Chỉ HNSW |
| 10 000 | ~500 µs p99 | ~2 ms p99 | Hybrid |
| 100 000 | ~1 ms p99 | ~5 ms p99 | DiskANN chính |
| 1 000 000 | — | ~10 ms p99 | DiskANN + PQ |

> **Nén vector:** QMvir tự động áp dụng **XOR-Delta lossless compression** — giảm dung lượng 30-50% mà không mất độ chính xác.

---

## Chương 5. Media Management — Quản lý Blob

> **Cập nhật gateway:** `LINK/UNLINK/MREF/SLABS` đã hỗ trợ qua daemon mode.
> Điều này cho phép truyền tải media chính thức qua gateway mà không cần chuyển sang local mode.

### 5.1 Gắn Media vào Hàng dữ liệu

```sql
-- Gắn ảnh vào hàng id=1
qmvir> LINK '/data/photos/avatar.jpg' TO users ROW 1;

-- Hoặc cú pháp đầy đủ
qmvir> LINK MEDIA '/data/photos/avatar.jpg' TO users ROW 1;
```

### 5.2 Truy vấn Media Reference

```sql
-- Lấy SlabHandle pointer
qmvir> MREF users 1;
```

### 5.3 Gỡ Media

```sql
qmvir> UNLINK FROM users ROW 1;
```

### 5.4 Xem trạng thái Slab Heap

```sql
qmvir> SLABS;
```

Hoặc cú pháp đầy đủ:

```sql
qmvir> SHOW SLABS;
```

### 5.5 Các lớp kích thước Media

| Lớp | Kích thước | Mục đích | Lưu ý |
|-----|-----------|---------|-------|
| 0 | 64 KB | Thumbnail, JSON config | Nhanh nhất, nhiều slab nhất |
| 1 | 1 MB | Ảnh nén, audio ngắn | Cân bằng |
| 2 | 8 MB | Ảnh gốc, video segment | Cho multimedia |
| 3 | 64 MB | Video frame thô | Ít slab, dùng tiết kiệm |

> **Zero-copy:** Media được truy cập qua `memoryview` trực tiếp trên mmap — không có copy buffer trung gian.

---

## Chương 6. Checkpoint & Recovery

### 6.1 Checkpoint thống

```sql
-- Checkpoint toàn bộ trạng thái
qmvir> CPOINT FULL;

-- Checkpoint chỉ phần thay đổi
qmvir> CPOINT DELTA;
```

### 6.2 Checkpoint tự động

QMvir tự động ghi checkpoint khi:
- **30 giây** kể từ checkpoint trước, HOẶC
- **1 000 mutation** (INSERT/UPDATE/DELETE) đã xảy ra

### 6.3 Recovery tự động

Khi daemon khởi động:
1. Quét thư mục `checkpoints/` cho file `.qmck` mới nhất
2. Verify SHA-256 integrity
3. Khôi phục metadata
4. Replay WAL tail

Bạn không cần làm gì — recovery hoàn toàn tự động.

### 6.4 Tùy chỉnh checkpoint

```bash
# Checkpoint mỗi 60 giây
qmvir start --checkpoint-interval 60

# Dữ liệu trên SSD riêng
qmvir start --data-dir /ssd/qm_data
```

---

## Chương 7. Giám sát hệ thống

### 7.1 Dashboard thời gian thực

```bash
qmvir dash
```

Giao diện tự cập nhật mỗi 1 giây:

```
  ╔═══════════════════ QMvir Dashboard ═══════════════════╗
  ║  Status : ● ONLINE     PID : 12345     Uptime : 01:23:45  ║
  ║  Gateway: 127.0.0.1:5433    Engine : v2.0.0-hub           ║
  ╠═══════════════════════════════════════════════════════╣
  ║  SATELLITES                                            ║
  ║    gen-0      ● running   (general)                    ║
  ║    vec-0      ● running   (vector)                     ║
  ║    plqm-0     ● running   (procedure)                  ║
  ║    media-0    ● ready     (media)                      ║
  ╠═══════════════════════════════════════════════════════╣
  ║  RING BUFFERS                                          ║
  ║    gen    [██░░░░░░░░░░░░░░░░░░]  10.0%               ║
  ║    vec    [░░░░░░░░░░░░░░░░░░░░]   0.0%               ║
  ║    proc   [░░░░░░░░░░░░░░░░░░░░]   0.0%               ║
  ╠═══════════════════════════════════════════════════════╣
  ║  MEDIA SLAB HEAP                                       ║
  ║    64K  : 240/256 free   (frag  6.3%)                  ║
  ║    1M   :  62/64  free   (frag  3.1%)                  ║
  ║    8M   :  15/16  free   (frag  6.3%)                  ║
  ║    64M  :   4/4   free   (frag  0.0%)                  ║
  ╚═══════════════════════════════════════════════════════╝
```

Nhấn **Ctrl+C** để thoát.

### 7.2 Đọc Dashboard

| Chỉ số | Ý nghĩa | Cảnh báo |
|--------|---------|---------|
| Ring Buffer % | Bao nhiêu % slot đang dùng | > 80% → **hotspot!** Hub nhanh hơn Satellite |
| Slab frag % | Tỷ lệ slab đã cấp phát | > 90% → cần giải phóng media hoặc tăng slab |
| Satellite ● | Trạng thái nốt | ○ vàng = chưa sẵn sàng |

### 7.3 Tùy chỉnh tốc độ refresh

```bash
# Cập nhật mỗi 2.5 giây (tiết kiệm CPU)
qmvir dash --refresh 2.5
```

### 7.4 Kiểm tra khả năng gateway (modern protocol introspection)

Trong `qmvir sql` daemon mode, bạn có thể hỏi gateway đang bật những tính năng nào:

```sql
SHOW QM FEATURES;
```

Ví dụ kết quả:

```text
feature       | status  | mode
--------------+---------+--------
vector_search | enabled | gateway
media_link    | enabled | gateway
mref          | enabled | gateway
slabs         | enabled | gateway
checkpoint    | enabled | gateway
```

---

# PHẦN C — NÂNG CAO

---

## Chương 8. Tối ưu hiệu năng SQL

### 8.1 Dùng cú pháp rút gọn

Cú pháp rút gọn QM-SQL nhanh hơn vì token ngắn hơn:

```sql
-- ❌ Chậm hơn (nhiều token để parse)
SEARCH VECTOR [0.1, 0.2, ...] IN documents TOP 5 METRIC cosine;
CHECKPOINT FULL;

-- ✅ Nhanh hơn (ít token hơn)
LIKEV VEC [0.1, 0.2, ...] IN documents TOP 5;
CPOINT FULL;
```

### 8.2 Batch INSERT

Thay vì INSERT từng hàng một, gom lại:

```sql
-- ❌ Chậm: 1000 round-trip Hub→Ring→Satellite
INSERT INTO t (id, val) VALUES (1, 'a');
INSERT INTO t (id, val) VALUES (2, 'b');
-- ... 998 lệnh nữa

-- ✅ Nhanh: dùng script Python để bulk insert
```

```python
from qm_core.hub_engine import QMHubEngine

engine = QMHubEngine(data_dir="/tmp/qm_data")
engine.execute_sql("CREATE TABLE t (id INT, val TEXT)")
for i in range(1000):
    engine.insert("t", {"id": i, "val": f"row_{i}"})
```

### 8.3 Vector Search — Chọn đúng TOP k

```sql
-- ❌ TOP lớn = chậm (quét nhiều nốt HNSW hơn)
LIKEV VEC [...] IN docs TOP 1000;

-- ✅ TOP nhỏ = nhanh
LIKEV VEC [...] IN docs TOP 10;
```

**Quy tắc ngón tay cái:**
- TOP 10-50: phù hợp cho 99% use case
- TOP > 100: cân nhắc dùng pre-filter trước

### 8.4 Predicate Pushdown

Đặt điều kiện WHERE càng sớm càng tốt để giảm dữ liệu qua ring buffer:

```sql
-- ✅ Tốt: lọc trước ở Hub
SELECT * FROM orders WHERE status = 'active' AND amount > 100;

-- ❌ Tránh: lấy tất cả rồi lọc ở client
SELECT * FROM orders;  -- rồi lọc ở Python
```

---

## Chương 9. IPC Tuning — Điều chỉnh Ring Buffer & Slab

### 9.1 Ring Buffer Geometry

Hai tham số quan trọng nhất:

| Tham số | Mặc định | Ảnh hưởng |
|---------|----------|----------|
| `ring_slot_count` | 1024 | Số lệnh đệm tối đa. Tăng → throughput cao hơn, RAM nhiều hơn |
| `ring_slot_data_size` | 65536 (64KB) | Kích thước payload tối đa/slot. Tăng cho blob lớn |

**Công thức tính dung lượng:**

$$\text{Ring Size} = 32 + \text{slot\_count} \times (16 + \text{slot\_data\_size})$$

Ví dụ mặc định: $32 + 1024 \times 65\,552 = 67\,137\,568$ bytes ≈ **67 MB**

### 9.2 Khi nào cần tăng slot_count

Xem dashboard:
- Ring occupancy thường xuyên > 60% → tăng `slot_count` lên 2048 hoặc 4096
- Ring occupancy < 10% → có thể giảm để tiết kiệm RAM

> **Lưu ý:** `slot_count` phải là **lũy thừa 2** (256, 512, 1024, 2048, 4096...)

### 9.3 Khi nào cần tăng slot_data_size

- INSERT payload > 64KB → tăng lên 128KB hoặc 256KB
- Chỉ dùng metadata nhỏ → giảm xuống 16KB để đệm nhiều lệnh hơn

### 9.4 Media Slab Tuning

Cấu hình mặc định phù hợp cho hầu hết use case. Điều chỉnh khi:

| Tình huống | Hướng điều chỉnh |
|------------|-----------------|
| Lưu nhiều thumbnail nhỏ | Tăng slab_count lớp 0 (64KB): 256 → 512 |
| Lưu video thô lớn | Tăng slab_count lớp 3 (64MB): 4 → 16 |
| RAM hạn chế | Giảm slab_count hoặc bỏ lớp 3 |

### 9.5 Ví dụ cấu hình tùy chỉnh (Python API)

```python
from qm_app import QMDaemonConfig

config = QMDaemonConfig(
    ring_slot_count=2048,           # Tăng gấp đôi throughput IPC
    ring_slot_data_size=32768,      # Giảm slot size → nhiều slot hơn
    media_size_classes=[64*1024, 1*1024*1024],  # Chỉ 2 lớp
    media_slab_counts=[512, 128],               # Nhiều slab hơn
)
```

---

## Chương 10. Process Isolation & Đa Satellite

### 10.1 Chế độ In-Process (mặc định)

```bash
qmvir start  # Satellite chạy như thread trong cùng tiến trình
```

- **Ưu điểm:** Khởi động nhanh, ít overhead IPC
- **Nhược điểm:** Chịu giới hạn GIL, crash satellite = crash daemon

### 10.2 Chế độ OS Process

```bash
qmvir start --process-isolation
```

- **Ưu điểm:** Mỗi satellite là tiến trình riêng → bypass GIL, fault isolation
- **Nhược điểm:** Overhead IPC qua mmap, khởi động chậm hơn

### 10.3 Khi nào dùng Process Isolation

| Kịch bản | Đề xuất |
|-----------|---------|
| Development, testing | In-process (mặc định) |
| Production, heavy workload | `--process-isolation` |
| Vector search nặng (>100K vectors) | `--process-isolation` |
| Docker container | In-process (đủ cho hầu hết) |

### 10.4 Health Monitor & Auto-Restart

Khi bật process isolation, daemon tự động:
- **Heartbeat** mỗi 5 giây kiểm tra satellite sống hay chết
- **Auto-restart** satellite chết (tối đa 3 lần/satellite)
- Log cảnh báo khi satellite cần restart

```
08:35:15 [qm.daemon] WARNING Satellite vec-0 not running — restarting (1/3)
```

---

## Chương 11. Benchmark & Phân tích Bottleneck

### 11.1 Chạy Benchmark nhanh

```bash
# Chạy tất cả 5 benchmark
qmvir bench

# Chỉ chạy benchmark cụ thể
qmvir bench --only ring
qmvir bench --only vector
qmvir bench --only gateway
qmvir bench --only checkpoint
qmvir bench --only parse

# Xuất JSON report
qmvir bench --json bench_report.json
```

### 11.2 Kết quả Benchmark mẫu

```
==========================================================================================
  QM Database — Performance Benchmark Report
==========================================================================================
  ring_buffer_publish_collect                 245.31   K ops/s       203.8ms  p50=3.2µs  p99=12.1µs
  gateway_select_tps                           18.72   K ops/s       534.2ms  p50=48.5µs p99=125.3µs
  checkpoint_full_write                         0.89   K ops/s        22.5ms  p50=1050µs p99=1380µs
  vector_search_1000                            1.23   K ops/s        40.7ms  p50=750µs  p99=1200µs
  sql_parse_lalr                               35.41   K ops/s       282.4ms  p50=25.1µs p99=45.8µs
==========================================================================================
```

### 11.3 Đọc kết quả — Xác định Bottleneck

| Benchmark | Chỉ số tốt | Chỉ số cần cải thiện |
|-----------|-----------|---------------------|
| Ring Buffer | > 200K ops/s | < 50K ops/s → tăng slot_count |
| Gateway TPS | > 15K TPS | < 5K TPS → CPU bottleneck |
| Checkpoint | p99 < 2ms | p99 > 10ms → SSD chậm |
| Vector Search (1K) | p99 < 2ms | p99 > 5ms → tăng RAM cho HNSW |
| SQL Parse | > 30K stmts/s | < 10K → grammar phức tạp |

### 11.4 Automated Benchmark (Chu kỳ đầy đủ)

```bash
# Tự động: start server → benchmark → stop server → JSON report
python tools/auto_bench.py

# Không cần server (benchmark tạo engine tạm)
python tools/auto_bench.py --no-server

# Chỉ đo vector
python tools/auto_bench.py --only vector --json vector_results.json
```

### 11.5 Phân tích Bottleneck qua Dashboard

Mở hai terminal:

**Terminal 1** — Chạy benchmark:
```bash
python tools/auto_bench.py --no-server --only ring
```

**Terminal 2** — Quan sát dashboard:
```bash
qmvir dash
```

Theo dõi:
- **Ring Buffer > 80%** → Hub gửi nhanh hơn Satellite xử lý → tăng `slot_count` hoặc bật process isolation
- **Slab frag > 50%** → Nhiều blob đã cấp phát → kiểm tra memory leak hoặc tăng slab


---

## Chương 12. Triển khai Docker Production

### 12.1 Build Image

```bash
docker build -t qmvir:1.0.0 .
```

### 12.2 Chạy Container

```bash
docker run -d \
  --name qmvir-prod \
  -p 5433:5433 \
  -v qmvir_data:/data/qm \
  -e QM_ADMIN_PASSWORD=your_secure_password \
  qmvir:1.0.0
```

### 12.3 Kiểm tra Health

```bash
docker inspect --format='{{.State.Health.Status}}' qmvir-prod
# → healthy
```

### 12.4 Benchmark trong Docker (cô lập)

```bash
docker exec qmvir-prod qmvir bench --json /data/qm/bench_report.json

# Lấy report ra host
docker cp qmvir-prod:/data/qm/bench_report.json ./bench_report.json
```

### 12.5 Cấu hình qua biến môi trường

| Biến | Mặc định | Mô tả |
|------|----------|-------|
| `QM_DATA_DIR` | `/data/qm` | Thư mục dữ liệu |
| `QM_HOST` | `0.0.0.0` | Địa chỉ bind |
| `QM_PORT` | `5433` | Port gateway |
| `QM_ADMIN_PASSWORD` | `changeme` | Mật khẩu admin ban đầu |

---

## Chương 13. Kết nối từ ứng dụng bên ngoài

### 13.1 Python — psycopg2

```python
import psycopg2

conn = psycopg2.connect(
    host="127.0.0.1",
    port=5433,
    user="admin",
    password="changeme",
    dbname="qm",
)
cur = conn.cursor()
cur.execute("SELECT * FROM users WHERE id = 1")
rows = cur.fetchall()
print(rows)
conn.close()
```

### 13.2 Python — API trực tiếp (nhanh nhất)

```python
from qm_core.hub_engine import QMHubEngine

engine = QMHubEngine(data_dir="/tmp/qm_data", wal_enabled=True)

# DDL
engine.execute_sql("CREATE TABLE items (id INT, name TEXT)")

# DML
engine.execute_sql("INSERT INTO items (id, name) VALUES (1, 'widget')")

# Query
result = engine.execute_sql("SELECT * FROM items")
print(result)  # {'columns': ['id', 'name'], 'rows': [[1, 'widget']]}

engine.close()
```

> **Ghi chú hiệu năng:** API trực tiếp nhanh hơn wire protocol ~2-5× vì không có TCP overhead.

### 13.3 CLI — psql

```bash
psql -h 127.0.0.1 -p 5433 -U admin -d qm
```

---

# PHỤ LỤC

---

## Phụ lục A. Bảng tham chiếu QM-SQL

### DDL

```sql
CREATE TABLE name (col1 TYPE, col2 TYPE, ...);
```

### DML

```sql
INSERT INTO table (cols...) VALUES (vals...);
UPDATE table SET col = val WHERE condition;
DELETE FROM table WHERE condition;
```

### Query

```sql
SELECT [DIST,] cols FROM table [WHERE cond] [ORDER BY col] [LIMIT n];
```

### Vector Search

```sql
-- Rút gọn
LIKEV VEC [v1,v2,...] IN table TOP k [METRIC cosine|l2|dot];

-- Đầy đủ
SEARCH VECTOR [v1,v2,...] IN table TOP k [METRIC cosine|l2|dot];
```

### Media

```sql
LINK '/path' TO table ROW id;
UNLINK FROM table ROW id;
MREF table id;
SLABS;
```

### Checkpoint

```sql
CPOINT FULL;    -- hoặc CHECKPOINT FULL
CPOINT DELTA;   -- hoặc CHECKPOINT DELTA
```

---

## Phụ lục B. Bảng cấu hình & Flag

### qmvir start

| Flag | Mặc định | Mô tả |
|------|----------|-------|
| `--host` | `127.0.0.1` | Địa chỉ bind |
| `--port` | `5433` | Port gateway |
| `--data-dir` | `/tmp/qm_data` | Thư mục dữ liệu |
| `--checkpoint-interval` | `30.0` | Auto-checkpoint (giây) |
| `--vector-dim` | `128` | Chiều vector mặc định |
| `--process-isolation` | `false` | Chạy satellite dạng OS process |
| `--log-level` | `INFO` | DEBUG, INFO, WARNING, ERROR |

### qmvir sql

| Flag | Mặc định | Mô tả |
|------|----------|-------|
| `-u` / `--user` | `admin` | Tên người dùng |
| `-p` / `--password` | *(trống)* | Mật khẩu |
| `--daemon` | `auto` | `auto`=tự dò daemon, `on`=ép daemon, `off`=local mode |
| `--host` | *(auto)* | Host gateway khi chạy daemon mode |
| `--port` | *(auto)* | Port gateway khi chạy daemon mode |
| `--local` | `false` | Alias nhanh cho `--daemon off` |

### qmvir bench

| Flag | Mặc định | Mô tả |
|------|----------|-------|
| `--only` | *(tất cả)* | ring, gateway, checkpoint, vector, parse |
| `--json` | *(không)* | Xuất JSON report |

### qmvir dash

| Flag | Mặc định | Mô tả |
|------|----------|-------|
| `--refresh` | `1.0` | Tốc độ cập nhật (giây) |

---

## Phụ lục C. Xử lý sự cố

### C.1 "QM daemon is not running"

```bash
# Kiểm tra xem PID file có tồn tại không
ls /tmp/qm_data/qm_daemon.pid

# Nếu file tồn tại nhưng daemon đã chết, xóa file PID
rm /tmp/qm_data/qm_daemon.pid

# Khởi động lại
qmvir start
```

### C.1b "Unknown command: /Users/.../.venv/bin/qmvir"

Bạn đã dán lệnh terminal vào bên trong SQL REPL (`qmvir>`).  
Trong REPL, chuỗi bắt đầu bằng `/` được hiểu là slash command (`/q`, `/h`, `/d`...).

Cách đúng:

1. Gõ `/q` để thoát REPL.
2. Chạy lại lệnh `qmvir ...` ở terminal bình thường.

### C.2 "Permission denied" khi INSERT

Vai trò của bạn là READER. Liên hệ admin để nâng cấp lên WRITER:

```sql
-- Admin chạy:
GRANT WRITER TO username;
```

### C.3 Ring Buffer đầy (hotspot!)

Dashboard hiển thị ring > 80%:

1. **Ngắn hạn:** Giảm tải query/insert
2. **Trung hạn:** Tăng `ring_slot_count` lên 2048+
3. **Dài hạn:** Bật `--process-isolation` để satellite xử lý song song thực sự

### C.4 Checkpoint chậm

p99 checkpoint > 10ms:

1. Kiểm tra ổ đĩa: dùng SSD thay vì HDD
2. Giảm `checkpoint_lsn_threshold` để checkpoint thường xuyên hơn với payload nhỏ hơn
3. Dùng DELTA mode thay vì FULL cho checkpoint tự động

### C.5 Vector search chậm

p99 > 5ms cho 1000 vectors:

1. Đảm bảo đủ RAM cho HNSW index
2. Giảm TOP k (10 thay vì 100)
3. Kiểm tra dimension — dim=128 nhanh hơn dim=768 nhiều

### C.6 `SHOW DATABASES` / `USE` báo lỗi trong daemon mode

Các lệnh `CREATE DATABASE`, `USE`, `SHOW DATABASES` chỉ là lớp tương thích cho **local mode**.

- Nếu bạn chạy `qmvir sql --daemon auto|on`: không dùng các lệnh này, query bảng trực tiếp.
- Nếu cần mô phỏng nhiều DB trong shell local: chạy `qmvir sql --daemon off`.

Ví dụ daemon mode chuẩn:

```sql
SELECT * FROM persist_users;
```

### C.7 Import error "No module named 'prompt_toolkit'"

```bash
pip install prompt_toolkit>=3.0
```

### C.8 Port 5433 đã bị chiếm

```bash
# Kiểm tra ai đang dùng port
lsof -i :5433

# Dùng port khác
qmvir start --port 5434
```

---

*Kết thúc Giáo trình QMvir v1.0.0 — Từ Cơ bản đến Nâng cao*
