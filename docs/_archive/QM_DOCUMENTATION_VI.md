# QM — Tài Liệu Kỹ Thuật Chi Tiết

**Phiên bản:** 2.0.0-hub  
**Ngày cập nhật:** 08/03/2026  
**Giấy phép:** Proprietary

---

## Mục Lục

1. [Tổng Quan Hệ Thống](#1-tổng-quan-hệ-thống)
2. [Kiến Trúc Hub-Satellite](#2-kiến-trúc-hub-satellite)
3. [Ring Buffer — Giao Tiếp Liên Tiến Trình](#3-ring-buffer--giao-tiếp-liên-tiến-trình)
4. [Media Slab Allocator — Bộ Cấp Phát Bộ Nhớ Chia Sẻ](#4-media-slab-allocator--bộ-cấp-phát-bộ-nhớ-chia-sẻ)
5. [QM-SQL — Ngôn Ngữ Truy Vấn Mở Rộng AI](#5-qm-sql--ngôn-ngữ-truy-vấn-mở-rộng-ai)
6. [Auto-Checkpoint — Lưu Trữ Trạng Thái Tự Động](#6-auto-checkpoint--lưu-trữ-trạng-thái-tự-động)
7. [QM Daemon — Trình Điều Phối Hệ Thống](#7-qm-daemon--trình-điều-phối-hệ-thống)
8. [Luồng Dữ Liệu End-to-End](#8-luồng-dữ-liệu-end-to-end)
9. [Hướng Dẫn Sử Dụng](#9-hướng-dẫn-sử-dụng)
10. [Kiểm Thử](#10-kiểm-thử)
11. [Phụ Lục — Cấu Trúc Thư Mục](#11-phụ-lục--cấu-trúc-thư-mục)

---

## 1. Tổng Quan Hệ Thống

### 1.1 QM là gì?

QM là một **nền tảng cơ sở dữ liệu đa động cơ** (Multi-Engine Database Platform), tích hợp 5 khả năng trong một hệ thống duy nhất:

| Động cơ | Mô tả | Ứng dụng |
|---------|--------|----------|
| **OLTP** | Lưu trữ theo hàng, MVCC, WAL | Giao dịch thời gian thực |
| **OLAP** | Lưu trữ theo cột | Phân tích dữ liệu lớn |
| **Full-Text Search** | BM25, fuzzy matching, faceted search | Tìm kiếm văn bản |
| **Vector Search** | HNSW, IVF-PQ, ANN | AI/ML embedding search |
| **Smart Cache** | Object cache, query cache, result cache | Tăng tốc truy vấn |

### 1.2 Triết lý thiết kế

- **Zero-Copy IPC**: Dữ liệu không bao giờ bị sao chép khi truyền giữa Hub và Satellite — mọi thứ đều qua `mmap` và `memoryview`.
- **Lock-Free Ring Buffer**: Mô hình LMAX Disruptor — không mutex, không context switch, throughput triệu thao tác/giây.
- **Crash Recovery**: WAL + Auto-Checkpoint đảm bảo không mất dữ liệu khi hệ thống sập.
- **AI-Native SQL**: Mở rộng SQL chuẩn với `SEARCH VECTOR`, `LINK MEDIA`, `SET COMPRESSION` — thiết kế cho ứng dụng AI.

### 1.3 Yêu cầu hệ thống

- **Python** >= 3.11 (khuyến nghị 3.13+)
- **Hệ điều hành**: macOS, Linux (có hỗ trợ `mmap`)
- **RAM tối thiểu**: 512 MB (cho cấu hình mặc định)
- **Phụ thuộc chính**:
  - `msgpack`, `orjson` — Serialization
  - `xxhash`, `lz4`, `zstandard` — Hash và nén
  - `numpy` — Tính toán vector
  - `lark` >= 1.1 — Parser LALR cho QM-SQL
  - `aiohttp`, `uvloop` — Async I/O

---

## 2. Kiến Trúc Hub-Satellite

### 2.1 Sơ đồ tổng thể

```
┌───────────────────────────────────────────────────────────────┐
│                        QM Daemon                              │
│  ┌──────────────────────────────────────────────────────────┐ │
│  │                    QMHubEngine                           │ │
│  │  ┌───────────┐  ┌───────────┐  ┌──────────────────────┐ │ │
│  │  │ QueryParser│  │ Planner   │  │ Adaptive Optimizer   │ │ │
│  │  └─────┬─────┘  └─────┬─────┘  └──────────┬───────────┘ │ │
│  │        └───────────────┴──────────────────┬┘             │ │
│  │                                           ▼              │ │
│  │                 ┌─────────────────────────────┐          │ │
│  │                 │       HubDispatcher         │          │ │
│  │                 │  ┌─────────────────────┐    │          │ │
│  │                 │  │   LSN Sequencer      │    │          │ │
│  │                 │  │ (đánh số thứ tự toàn │    │          │ │
│  │                 │  │  cục cho mọi lệnh)   │    │          │ │
│  │                 │  └─────────────────────┘    │          │ │
│  │                 └──────┬──────┬──────┬────────┘          │ │
│  │                        │      │      │                   │ │
│  │              ┌─────────┤      │      ├──────────┐        │ │
│  │              ▼         ▼      ▼      ▼          ▼        │ │
│  │        ┌──────────┐ ┌──────────┐ ┌──────────┐           │ │
│  │        │Ring Gen  │ │Ring Vec  │ │Ring Proc │           │ │
│  │        │(64KB×1K) │ │(64KB×1K) │ │(64KB×1K) │           │ │
│  │        └────┬─────┘ └────┬─────┘ └────┬─────┘           │ │
│  └─────────────┼────────────┼────────────┼──────────────────┘ │
│                ▼            ▼            ▼                    │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐          │
│  │ General      │ │ Vector       │ │ Procedure    │          │
│  │ Satellite    │ │ Satellite    │ │ Satellite    │          │
│  │ (CRUD, DDL,  │ │ (HNSW, ANN, │ │ (PL/QM,     │          │
│  │  index maint)│ │  vector ops) │ │  stored proc)│          │
│  └──────────────┘ └──────────────┘ └──────────────┘          │
│                                                               │
│  ┌──────────────────┐  ┌──────────────┐  ┌───────────────┐   │
│  │ Media Slab       │  │ Checkpoint   │  │ Merkle        │   │
│  │ Allocator        │  │ Manager      │  │ Auditor       │   │
│  │ (mmap zero-copy) │  │ (RAM→SSD)    │  │ (SHA-256)     │   │
│  └──────────────────┘  └──────────────┘  └───────────────┘   │
│                                                               │
│  ┌──────────────────────────────────────────────────────────┐ │
│  │              PostgreSQL Wire Protocol Gateway             │ │
│  │              (port 5433 — tương thích psql/libpq)         │ │
│  └──────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────┘
```

### 2.2 Các thành phần chính

#### QMHubEngine (`qm_core/hub_engine.py`)

Là **mặt tiền (facade)** của toàn bộ hệ thống. Đóng vai trò Control Plane — tiếp nhận truy vấn, phân tích cú pháp, lập kế hoạch thực thi, rồi phân phối lệnh xuống Satellite qua ring buffer.

```python
class QMHubEngine:
    VERSION = "2.0.0-hub"

    def create_table(table: str, schema: dict) -> None
    def insert(table: str, rows: list[dict]) -> None
    def execute_sql(sql: str) -> Any
    def close() -> None
```

**Thuộc tính nội bộ:**
- `_dispatcher: HubDispatcher` — Bộ phân phối lệnh
- `_gen_sat: GeneralSatellite` — Satellite xử lý CRUD
- `_vec_sat: VectorSatellite` — Satellite tìm kiếm vector
- `_proc_sat: ProcedureSatellite` — Satellite thực thi stored procedure
- `_tables: dict[str, _HubTableMeta]` — Metadata bảng
- `_parser: QueryParser` — Phân tích truy vấn
- `_planner: QueryPlanner` — Lập kế hoạch thực thi
- `_adaptive: AdaptiveExecutor` — Tối ưu hóa bằng ML

#### HubDispatcher (`qm_core/hub/dispatcher.py`)

Bộ điều phối trung tâm — quản lý 3 ring buffer (General, Vector, Procedure), mỗi ring buffer kết nối Hub với một loại Satellite. Tất cả lệnh đều đi qua LSN Sequencer để đảm bảo **thứ tự toàn cục**.

```python
@dataclass
class DispatcherConfig:
    ring_dir: str | None = None
    slot_count: int = 1024          # Số slot trên mỗi ring
    slot_data_size: int = 65536     # 64KB/slot
    wal_path: str | None = None
    timeout_ms: int = 5000

class HubDispatcher:
    def dispatch_sync(cmd_type, table, payload, timeout_ms) -> Any
    def create_table(table: str, schema: dict) -> None

    @property
    def hub_gen -> Hub      # Hub cho General Satellite
    @property
    def hub_vec -> Hub      # Hub cho Vector Satellite
    @property
    def hub_proc -> Hub     # Hub cho Procedure Satellite
```

#### LSN Sequencer (`qm_core/hub/lsn_sequencer.py`)

Bộ đánh số thứ tự đơn điệu (monotonic), đảm bảo mọi lệnh trong hệ thống đều có **số thứ tự duy nhất và tăng dần**. Khi hệ thống phục hồi sau sự cố, epoch tăng lên để phân biệt các giai đoạn.

```python
@dataclass(frozen=True, slots=True)
class LSNStamp:
    lsn: int              # Số thứ tự đơn điệu
    epoch: int            # Epoch phục hồi
    timestamp_ns: int     # Thời gian thực (nanosecond)

    def to_bytes() -> bytes     # 24 bytes
    @classmethod
    def from_bytes(data) -> LSNStamp

class LSNSequencer:
    def next() -> LSNStamp           # Phát hành LSN tiếp theo
    def next_batch(count) -> list    # Phát hành hàng loạt
    def peek() -> int                # Xem trước không tăng
    def bump_epoch() -> int          # Tăng epoch (khi recovery)
    def checkpoint_state() -> bytes  # Serialize (16 bytes)

    @property
    def current_lsn -> int
    @property
    def epoch -> int
```

#### Merkle Auditor (`qm_core/hub/merkle_auditor.py`)

Cây Merkle SHA-256 để **kiểm tra tính toàn vẹn dữ liệu** giữa Hub và Satellite mà không cần truyền dữ liệu thô. Mỗi bảng là một lá (leaf) trong cây.

```python
class MerkleAuditor:
    def update_leaf(key: str, data_hash: bytes) -> None  # Cập nhật lá
    def remove_leaf(key: str) -> None                    # Xóa lá
    def root() -> bytes                                  # Hash gốc 32 bytes
```

#### Satellite Workers

Ba loại Satellite chạy song song, mỗi loại chuyên biệt cho một lĩnh vực:

| Satellite | File | Vai trò |
|-----------|------|---------|
| **GeneralSatellite** | `satellite/general_satellite.py` | CRUD, DDL, bảo trì index |
| **VectorSatellite** | `satellite/vector_satellite.py` | HNSW, DiskANN, tìm kiếm ANN |
| **ProcedureSatellite** | `satellite/procedure_satellite.py` | PL/QM, stored procedure |

Mỗi Satellite:
- Đọc lệnh từ ring buffer riêng của mình
- Xử lý lệnh và ghi kết quả trở lại ring buffer
- Có thể chạy trong cùng tiến trình (thread) hoặc tiến trình riêng (process isolation)

---

## 3. Ring Buffer — Giao Tiếp Liên Tiến Trình

### 3.1 Nguyên lý hoạt động

Ring Buffer trong QM dựa trên mô hình **LMAX Disruptor** — một cấu trúc dữ liệu vòng tròn được ánh xạ vào bộ nhớ chia sẻ (`mmap`), cho phép Hub ghi lệnh và Satellite đọc lệnh **không cần khóa (lock-free)**.

### 3.2 Bố cục bộ nhớ

```
┌─────────────────────────────────────────────────────────────┐
│                   SharedRingBuffer                           │
│                                                              │
│  Slot 0: [state(1B)|lsn(8B)|cmd(1B)|size(4B)|pad(2B)]      │
│          [data ───────────── 65536 bytes ──────────────]     │
│                                                              │
│  Slot 1: [state(1B)|lsn(8B)|cmd(1B)|size(4B)|pad(2B)]      │
│          [data ───────────── 65536 bytes ──────────────]     │
│                                                              │
│  ...                                                         │
│                                                              │
│  Slot 1023: [header 16B] [data 64KB]                         │
│                                                              │
│  Tổng cộng: 1024 × (16 + 65536) = ~64 MB / ring             │
│  Hệ thống có 3 ring → ~192 MB bộ nhớ chia sẻ                │
└─────────────────────────────────────────────────────────────┘
```

### 3.3 Trạng thái Slot

```python
class SlotState(IntEnum):
    EMPTY      = 0   # Slot trống — Hub có thể ghi
    READY      = 1   # Hub đã ghi — Satellite có thể đọc
    PROCESSING = 2   # Satellite đang xử lý
    DONE       = 3   # Satellite hoàn thành — Hub đọc kết quả
    ERROR      = 4   # Satellite gặp lỗi
```

### 3.4 Các loại lệnh

```python
class CommandType(IntEnum):
    NOOP       = 0     # Không làm gì
    INSERT     = 1     # Chèn dữ liệu
    UPDATE     = 2     # Cập nhật
    DELETE     = 3     # Xóa
    QUERY      = 4     # Truy vấn
    DDL        = 5     # Tạo/sửa/xóa bảng
    VECTOR_OP  = 6     # Thao tác vector
    COMPRESS   = 7     # Nén dữ liệu
    CHECKPOINT = 8     # Checkpoint
    SHUTDOWN   = 255   # Tắt Satellite
```

### 3.5 Luồng giao tiếp

```
Hub                          Ring Buffer                    Satellite
──────                       ───────────                    ─────────
1. publish(cmd, payload) ──→ [state=READY, data=payload]
                                                       ←── 2. consume()
                             [state=PROCESSING]
                                                            3. Xử lý lệnh
                             [state=DONE, data=result]  ←── 4. Ghi kết quả
5. ack(slot) ──────────────→ [state=EMPTY]
```

**Đặc điểm:**
- **Zero-Copy**: Payload được ghi trực tiếp vào vùng `mmap`, Satellite đọc tại chỗ.
- **Lock-Free**: Không sử dụng mutex — chỉ dùng atomic state transitions.
- **Bounded**: Tối đa 1024 slot/ring — nếu đầy, Hub phải chờ slot trống.

---

## 4. Media Slab Allocator — Bộ Cấp Phát Bộ Nhớ Chia Sẻ

**File:** `qm_core/ipc/media_allocator.py`

### 4.1 Vấn đề cần giải quyết

Ring buffer mỗi slot chỉ chứa 64 KB — không đủ cho ảnh, video, âm thanh. Media Slab Allocator giải quyết vấn đề này bằng cách:

1. Cấp phát vùng nhớ lớn trên `mmap` (bộ nhớ chia sẻ).
2. Hub ghi media vào slab (zero-copy).
3. Hub gửi **SlabHandle** (chỉ 32 bytes) qua ring buffer.
4. Satellite ánh xạ cùng vùng `mmap` và đọc media tại chỗ — **không sao chép dữ liệu**.

### 4.2 Kiến trúc lớp kích thước (Size Classes)

```
┌──────────┬──────────┬──────────┬──────────────────────────────┐
│ Lớp      │ Kích thước│ Số slab │ Ứng dụng                     │
├──────────┼──────────┼──────────┼──────────────────────────────┤
│ Class 0  │  64 KB   │   256    │ Thumbnail, JSON metadata     │
│ Class 1  │   1 MB   │    64    │ Ảnh nén, âm thanh ngắn       │
│ Class 2  │   8 MB   │    16    │ Ảnh độ phân giải cao, video  │
│ Class 3  │  64 MB   │     4    │ Video frame thô, media chưa nén│
└──────────┴──────────┴──────────┴──────────────────────────────┘

Tổng dung lượng mặc định:
  64KB×256 + 1MB×64 + 8MB×16 + 64MB×4
= 16MB + 64MB + 128MB + 256MB
= 464 MB bộ nhớ chia sẻ dành cho media
```

### 4.3 Bố cục file heap (mỗi lớp kích thước)

```
┌──────────────────────────────────────────────────────┐
│ Magic Header (32 bytes)                               │
│   magic(8B): 0x514D_534C_4142_4846 ("QMSLABHF")     │
│   slab_size(8B): kích thước mỗi slab                 │
│   slab_count(8B): tổng số slab                        │
│   free_count(8B): số slab còn trống                   │
├──────────────────────────────────────────────────────┤
│ Bitmap (ceil(N/8) bytes)                              │
│   Mỗi bit = 1 slab: 0 = trống, 1 = đã cấp phát     │
├───────────────────── Page Aligned (4KB) ─────────────┤
│ Slab[0]  ─── slab_size bytes ───                      │
│ Slab[1]  ─── slab_size bytes ───                      │
│ ...                                                   │
│ Slab[N-1]                                             │
└──────────────────────────────────────────────────────┘
```

### 4.4 API chính

#### SlabHandle — Tay cầm slab (32 bytes, có thể serialize)

```python
@dataclass(frozen=True, slots=True)
class SlabHandle:
    class_idx: int     # Chỉ số lớp kích thước (0-3)
    slab_idx: int      # Chỉ số slab trong lớp
    offset: int        # Offset byte từ đầu file mmap
    size: int          # Kích thước slab khả dụng

    def to_bytes() -> bytes            # Đóng gói 32 bytes
    @classmethod
    def from_bytes(data) -> SlabHandle  # Giải đóng gói
```

#### _SlabPool — Pool đơn lớp kích thước

```python
class _SlabPool:
    def __init__(path, slab_size, slab_count, create=True)

    def allocate() -> int | None   # Cấp phát 1 slab, trả về index hoặc None
    def free(idx: int) -> None     # Giải phóng slab (idempotent)
    def write(idx, data, offset=0) -> int       # Ghi dữ liệu
    def read(idx, length, offset=0) -> bytes    # Đọc dữ liệu
    def memoryview_of(idx) -> memoryview         # Zero-copy view
    def stats() -> dict            # Thống kê sử dụng
```

#### MediaSlabAllocator — Bộ cấp phát đa lớp

```python
class MediaSlabAllocator:
    def __init__(config: MediaAllocatorConfig, create=True)

    def allocate(size: int) -> SlabHandle    # Tự chọn lớp nhỏ nhất phù hợp
    def free(handle: SlabHandle) -> None     # Giải phóng
    def write(handle, data, offset=0) -> int # Ghi media
    def read(handle, length, offset=0) -> bytes  # Đọc media
    def memoryview_of(handle) -> memoryview   # Zero-copy

    def stats() -> list[dict]                # Thống kê từng lớp
    def total_allocated_bytes() -> int       # Tổng đã cấp phát
    def total_capacity_bytes() -> int        # Tổng dung lượng

    @classmethod
    def attach(config) -> MediaSlabAllocator  # Satellite gắn vào heap đã tồn tại
```

### 4.5 Ví dụ sử dụng

```python
from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

# ── Phía Hub (Producer) ──
config = MediaAllocatorConfig(heap_dir="/tmp/qm_media")
alloc = MediaSlabAllocator(config, create=True)

# Cấp phát slab cho ảnh 500KB
image_data = load_image("photo.jpg")  # 500KB
handle = alloc.allocate(len(image_data))  # → Class 1 (1MB)

# Ghi dữ liệu zero-copy
alloc.write(handle, image_data)

# Gửi handle qua ring buffer (chỉ 32 bytes!)
ring.publish(CommandType.VECTOR_OP, "photos", handle.to_bytes())

# ── Phía Satellite (Consumer) ──
alloc_satellite = MediaSlabAllocator.attach(config)

# Nhận handle từ ring buffer
handle = SlabHandle.from_bytes(payload)

# Đọc zero-copy
view = alloc_satellite.memoryview_of(handle)  # memoryview trực tiếp vào mmap
# Hoặc đọc bytes
data = alloc_satellite.read(handle, 500_000)
```

---

## 5. QM-SQL — Ngôn Ngữ Truy Vấn Mở Rộng AI

**File:** `qm_core/execution/qm_sql.py`

### 5.1 Tổng quan

QM-SQL là **phương ngữ SQL mở rộng** (SQL dialect), được thiết kế cho ứng dụng AI. Nó bao gồm:
- **SQL chuẩn đầy đủ**: SELECT, INSERT, UPDATE, DELETE, CREATE TABLE
- **Lệnh AI-Native**: SEARCH VECTOR, SET COMPRESSION, LINK/UNLINK MEDIA, SHOW SLABS, CHECKPOINT

Bộ phân tích cú pháp sử dụng thư viện **Lark** với parser **LALR(1)** — hiệu suất tuyến tính O(n), phù hợp cho workload truy vấn cao tải.

### 5.2 Cú pháp các lệnh mở rộng

#### SEARCH VECTOR — Tìm kiếm vector gần nhất

```sql
-- Tìm 10 ảnh tương tự nhất với vector cho trước
SEARCH VECTOR [0.1, 0.5, -0.3, 0.8] IN photos TOP 10;

-- Có bộ lọc và chỉ định metric
SEARCH VECTOR [0.1, 0.5, -0.3, 0.8] IN photos TOP 5
    WHERE category = 'landscape'
    METRIC l2;
```

**Tham số:**
- `vector`: Mảng float — vector truy vấn
- `table`: Tên bảng chứa vector embedding
- `TOP k`: Trả về k kết quả gần nhất
- `WHERE` (tùy chọn): Bộ lọc trên metadata
- `METRIC` (tùy chọn): `cosine` (mặc định), `l2`, `ip`, `dot`

**AST Node:**
```python
@dataclass
class SearchVectorStmt(ASTNode):
    vector: list[float]
    table: str
    top_k: int
    where: ASTNode | None = None
    metric: str = "cosine"
```

#### SET COMPRESSION — Cấu hình nén dữ liệu

```sql
-- Bật nén LZ4 cho bảng logs
SET COMPRESSION 'lz4' ON logs;

-- Bật nén Zstandard cho bảng archives
SET COMPRESSION 'zstd' ON archives;
```

**AST Node:**
```python
@dataclass
class SetCompressionStmt(ASTNode):
    codec: str     # "lz4", "zstd", "snappy", ...
    table: str
```

#### LINK MEDIA / UNLINK MEDIA — Liên kết media blob

```sql
-- Liên kết ảnh với bản ghi
LINK MEDIA '/data/images/photo_001.jpg' TO photos ROW_ID 42;

-- Hủy liên kết
UNLINK MEDIA FROM photos ROW_ID 42;
```

**AST Nodes:**
```python
@dataclass
class LinkMediaStmt(ASTNode):
    path: str      # Đường dẫn file hoặc slab reference
    table: str
    row_id: int

@dataclass
class UnlinkMediaStmt(ASTNode):
    table: str
    row_id: int
```

#### SHOW SLABS — Xem trạng thái bộ nhớ media

```sql
SHOW SLABS;
```

Trả về thống kê từng lớp kích thước: slab_size, slab_count, free, used.

#### CHECKPOINT — Kích hoạt checkpoint thủ công

```sql
CHECKPOINT;        -- Mặc định: FULL
CHECKPOINT FULL;   -- Snapshot toàn bộ trạng thái
CHECKPOINT DELTA;  -- Chỉ ghi phần thay đổi từ checkpoint trước
```

### 5.3 SQL chuẩn được hỗ trợ

#### SELECT — Truy vấn dữ liệu

```sql
-- Cơ bản
SELECT * FROM users;
SELECT DISTINCT name, age FROM users WHERE age > 25;

-- JOIN
SELECT u.name, o.total
FROM users u
INNER JOIN orders o ON u.id = o.user_id;

-- GROUP BY, HAVING, ORDER BY, LIMIT
SELECT dept, COUNT(id) FROM employees
GROUP BY dept
HAVING COUNT(id) > 5
ORDER BY dept ASC
LIMIT 10 OFFSET 20;

-- CTE (Common Table Expression)
WITH active AS (SELECT * FROM users WHERE status = 'active')
SELECT * FROM active WHERE age > 30;

-- Biểu thức phức hợp
SELECT * FROM products
WHERE price BETWEEN 100 AND 500
  AND category IN ('electronics', 'books')
  AND name LIKE '%phone%'
  AND description IS NOT NULL;

-- CASE
SELECT name,
    CASE
        WHEN age < 18 THEN 'minor'
        WHEN age < 65 THEN 'adult'
        ELSE 'senior'
    END AS age_group
FROM users;
```

#### INSERT, UPDATE, DELETE

```sql
INSERT INTO users (name, age) VALUES ('Alice', 30);
UPDATE users SET age = 31 WHERE name = 'Alice';
DELETE FROM users WHERE age < 18;
```

#### CREATE TABLE

```sql
CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    name VARCHAR NOT NULL,
    price DECIMAL,
    created_at TIMESTAMP
);
```

### 5.4 Kiến trúc Parser

```
Chuỗi SQL
    │
    ▼
┌──────────────┐
│  Lark LALR   │  ← Dùng grammar QM_SQL_GRAMMAR (~200 dòng)
│  Parser      │     Phân tích cú pháp trong O(n)
└──────┬───────┘
       │ Lark Tree
       ▼
┌──────────────┐
│  _QMSQLTrans │  ← Chuyển đổi Lark Tree thành QM AST nodes
│  former      │     Tương thích với AST nodes trong sql_parser.py
└──────┬───────┘
       │ QM AST Node
       ▼
┌──────────────┐
│  QueryPlanner│  ← Lập kế hoạch thực thi
│  Optimizer   │     (cost-based, adaptive)
└──────────────┘
```

**AST Nodes chuẩn (từ `sql_parser.py`):**

| Node | Mô tả |
|------|--------|
| `Literal(value)` | Hằng số: số, chuỗi, null, bool |
| `ColumnRef(table, column)` | Tham chiếu cột |
| `BinaryOp(op, left, right)` | Phép toán hai ngôi |
| `UnaryOp(op, operand)` | Phép toán một ngôi |
| `FunctionCall(name, args, distinct)` | Gọi hàm |
| `StarExpr(table)` | SELECT * |
| `AliasedExpr(expr, alias)` | Biểu thức có alias |
| `InList(expr, values, negate)` | Toán tử IN |
| `BetweenExpr(expr, low, high, negate)` | Toán tử BETWEEN |
| `LikeExpr(expr, pattern, negate)` | Toán tử LIKE |
| `IsNullExpr(expr, negate)` | Kiểm tra NULL |
| `CaseExpr(operand, when_clauses, else_clause)` | Biểu thức CASE |
| `SelectStmt` | SELECT đầy đủ (JOIN, WHERE, GROUP BY, ...) |
| `InsertStmt(table, columns, values)` | INSERT (đa hàng) |
| `UpdateStmt(table, assignments, where)` | UPDATE |
| `DeleteStmt(table, where)` | DELETE |
| `CreateTableStmt(table, columns)` | CREATE TABLE |
| `JoinClause(join_type, table, alias, condition)` | Mệnh đề JOIN |
| `CTEDef(name, query)` | CTE definition |

### 5.5 API sử dụng

```python
from qm_core.execution.qm_sql import QMSQLParser

parser = QMSQLParser()

# Lệnh QM mở rộng
ast = parser.parse("SEARCH VECTOR [0.1, 0.5] IN photos TOP 10;")
print(ast)  # SearchVectorStmt(vector=[0.1, 0.5], table='photos', top_k=10, ...)

# SQL chuẩn
ast = parser.parse("SELECT * FROM users WHERE age > 25 ORDER BY name;")
print(ast)  # SelectStmt(columns=[StarExpr()], from_table='users', ...)

# Lỗi cú pháp → raise SQLSyntaxError
try:
    parser.parse("INVALID QUERY XYZ;")
except SQLSyntaxError as e:
    print(f"Lỗi: {e}")
```

### 5.6 Chi tiết grammar Lark

Grammar được viết theo cú pháp Lark EBNF, sử dụng parser LALR(1). Các đặc điểm:

- **Không phân biệt hoa/thường**: Tất cả keyword đều dùng `"KEYWORD"i`
- **Ưu tiên biểu thức**: Tuân thủ chuẩn SQL — OR < AND < NOT < comparison < addition < multiplication
- **Terminals có tên**: `ORDER_DIR`, `CMP_OP`, `METRIC_NAME`, `CHECKPOINT_MODE` — đảm bảo LALR không bỏ qua token
- **Comment**: Hỗ trợ `-- comment` style

---

## 6. Auto-Checkpoint — Lưu Trữ Trạng Thái Tự Động

**File:** `qm_core/checkpoint.py`

### 6.1 Vấn đề cần giải quyết

QM lưu dữ liệu trong RAM (ring buffer + satellite state). Nếu tiến trình bị crash, mọi dữ liệu sẽ mất. Auto-Checkpoint giải quyết bằng cách:

1. **Định kỳ** (mặc định 30 giây) dump trạng thái Hub ra SSD.
2. **Theo ngưỡng** — sau mỗi 1000 mutation (INSERT/UPDATE/DELETE), checkpoint ngay.
3. **Khi tắt** — ghi checkpoint cuối cùng trước khi shutdown.
4. **Khi phục hồi** — đọc checkpoint mới nhất, chỉ replay WAL tail.

### 6.2 Định dạng file Checkpoint (`.qmck`)

```
┌─────────────────────────────────────────────────────────────┐
│ Header (32 bytes)                                            │
│   magic(8B):      0x514D_434B_5054 ("QMCKPT")              │
│   lsn(8B):        LSN tại thời điểm checkpoint              │
│   epoch(8B):      Epoch hiện tại                            │
│   timestamp_ns(8B): Thời gian wall-clock (nanosecond)       │
├─────────────────────────────────────────────────────────────┤
│ Body Length (4 bytes)                                        │
├─────────────────────────────────────────────────────────────┤
│ Body (msgpack-encoded)                                       │
│   {                                                          │
│     "table_meta": {                                          │
│       "users": {                                             │
│         "schema": {...},                                     │
│         "primary_key": "id",                                 │
│         "vector_dim": null,                                  │
│         "row_count": 1500                                    │
│       },                                                     │
│       ...                                                    │
│     },                                                       │
│     "merkle_root": <32 bytes SHA-256>,                       │
│     "mode": "full" | "delta"                                 │
│   }                                                          │
├─────────────────────────────────────────────────────────────┤
│ SHA-256 Checksum (32 bytes)                                  │
│   = sha256(header + body_len + body)                         │
│   Dùng để xác minh tính toàn vẹn khi đọc lại               │
└─────────────────────────────────────────────────────────────┘

Tên file: ckpt_{lsn:012d}_{timestamp_ns}.qmck
Ví dụ:    ckpt_000000001500_1709856000000000000.qmck
```

### 6.3 Hai chế độ checkpoint

| Chế độ | Mô tả | Khi nào dùng |
|--------|--------|-------------|
| **FULL** | Snapshot toàn bộ metadata Hub + Merkle root | Khi shutdown, theo lịch |
| **DELTA** | Chỉ ghi phần thay đổi từ checkpoint trước | Background auto-checkpoint |

### 6.4 API chính

#### CheckpointRecord — Bản ghi checkpoint bất biến

```python
@dataclass(frozen=True, slots=True)
class CheckpointRecord:
    lsn: int                   # LSN tại checkpoint
    epoch: int                 # Epoch phục hồi
    timestamp_ns: int          # Thời gian wall-clock
    table_meta: dict[str, Any] # Metadata các bảng
    merkle_root: bytes         # Hash gốc Merkle (32B)
    mode: str                  # "full" hoặc "delta"

    def to_bytes() -> bytes                      # Serialize có checksum
    @classmethod
    def from_bytes(data) -> CheckpointRecord      # Deserialize + xác minh
```

#### CheckpointConfig — Cấu hình checkpoint

```python
@dataclass
class CheckpointConfig:
    checkpoint_dir: str | None = None  # Thư mục lưu file .qmck
    interval_seconds: float = 30.0     # Khoảng cách giữa 2 checkpoint
    lsn_threshold: int = 1000          # Số mutation kích hoạt checkpoint
    max_checkpoints: int = 5           # Giữ lại N checkpoint mới nhất
    enabled: bool = True               # Bật/tắt auto-checkpoint
```

#### CheckpointManager — Quản lý checkpoint

```python
class CheckpointManager:
    def __init__(config, state_provider: Callable)

    # Luồng nền
    def start() -> None                # Khởi động thread auto-checkpoint
    def stop() -> None                 # Dừng thread

    # Theo dõi mutation
    def notify_mutation() -> None      # Gọi mỗi khi có INSERT/UPDATE/DELETE
                                       # Nếu vượt ngưỡng → checkpoint ngay

    # Checkpoint thủ công
    def force_checkpoint(mode="full") -> CheckpointRecord

    # Phục hồi
    def latest_checkpoint() -> CheckpointRecord | None
    def recover_state() -> dict[str, Any]  # Trả về {} nếu không có checkpoint

    # Thống kê
    @property
    def is_running -> bool
    @property
    def checkpoint_count -> int
    def stats() -> dict
```

### 6.5 Quy trình hoạt động

```
                     CheckpointManager
                     ─────────────────
Khởi động:
    start() → Tạo daemon thread "qm-checkpoint"

Vòng lặp nền:
    ┌──────────────────────────────────────────────┐
    │  1. sleep(interval_seconds)                   │
    │  2. Nếu mutation_count > 0:                   │
    │     a. Gọi state_provider() → lấy trạng thái │
    │     b. Tạo CheckpointRecord                   │
    │     c. Ghi ra file .qmck (fsync)             │
    │     d. Reset mutation_count = 0               │
    │     e. Xóa checkpoint cũ (giữ lại max 5)     │
    │  3. Lặp lại                                  │
    └──────────────────────────────────────────────┘

Khi mutation vượt ngưỡng (1000):
    notify_mutation() → force_checkpoint("delta") ngay lập tức

Khi tắt hệ thống:
    QMDaemon.stop() → force_checkpoint("full") → stop()

Khi phục hồi:
    recover_state() → Đọc file .qmck mới nhất
                    → Xác minh SHA-256
                    → Trả về {lsn, epoch, table_meta, merkle_root}
```

### 6.6 Bảo toàn tính toàn vẹn

Mỗi file checkpoint có **SHA-256 checksum** ở cuối:
- `checksum = SHA-256(header + body_len + body)`
- Khi đọc, tính lại SHA-256 và so sánh
- Nếu không khớp → `ValueError("Checkpoint integrity check failed")`
- File bị hỏng sẽ bị bỏ qua, tiếp tục đọc file cũ hơn

---

## 7. QM Daemon — Trình Điều Phối Hệ Thống

**File:** `qm_app.py`

### 7.1 Tổng quan

QM Daemon là **điểm vào chính** (entry point) của hệ thống QM. Nó phối hợp mọi thành phần theo một vòng đời rõ ràng:

```
Bootstrap → Recover → Spawn → Checkpoint → Monitor → Gateway → [Chạy] → Shutdown
```

### 7.2 Vòng đời chi tiết

#### Giai đoạn 1: Bootstrap

```
[1/4] Khởi tạo Hub Engine
      → Tạo 3 ring buffer (Gen, Vec, Proc)
      → Tạo 3 satellite (in-process hoặc process isolation)
      → Tạo LSN Sequencer, Merkle Auditor

[2/4] Khởi tạo Media Heap
      → Mmap 4 lớp kích thước (64KB, 1MB, 8MB, 64MB)
      → Tổng ~464 MB bộ nhớ chia sẻ

[3/4] Khởi tạo Checkpoint Manager
      → Cấu hình interval, threshold, max_checkpoints

[4/4] Phục hồi (Recovery)
      → Đọc file .qmck mới nhất
      → Khôi phục metadata bảng, Merkle root, LSN/epoch
      → Nếu không có checkpoint → fresh start
```

#### Giai đoạn 2: Spawn Satellites

```
Nếu process_isolation = False (mặc định):
    → Satellite chạy như thread trong cùng tiến trình
    → Không cần thêm bước

Nếu process_isolation = True:
    → Spawn 3 tiến trình OS (gen-0, vec-0, plqm-0)
    → Mỗi tiến trình attach vào ring buffer riêng
```

#### Giai đoạn 3: Checkpoint & Monitor

```
Checkpoint Manager:
    → Daemon thread "qm-checkpoint"
    → Mỗi 30s hoặc sau 1000 mutation → ghi checkpoint

Health Monitor:
    → Daemon thread "qm-monitor"
    → Mỗi 5s kiểm tra satellite._running
    → Nếu satellite chết → tự động restart (tối đa 3 lần)
    → Nếu vượt 3 lần → log error và bỏ qua
```

#### Giai đoạn 4: Gateway

```
PostgreSQL Wire Protocol Server:
    → Lắng nghe trên port 5433 (mặc định)
    → Tương thích psql, libpq, pg drivers
    → Chuyển truy vấn SQL → QMHubEngine.execute_sql()
```

#### Giai đoạn 5: Shutdown

```
SIGINT hoặc SIGTERM:
    1. Đóng gateway (ngừng nhận kết nối mới)
    2. Ghi checkpoint cuối cùng (FULL)
    3. Dừng checkpoint thread
    4. Dừng satellite workers
    5. Đóng engine
    6. Đóng media heap (unmap)
    7. Xóa PID file
```

### 7.3 Cấu hình

```python
@dataclass
class QMDaemonConfig:
    data_dir: str = "/tmp/qm_data"         # Thư mục dữ liệu
    host: str = "127.0.0.1"                # Địa chỉ bind
    port: int = 5433                       # Cổng lắng nghe

    # Ring buffer
    ring_slot_count: int = 1024            # Slot/ring
    ring_slot_data_size: int = 65536       # 64KB/slot

    # Media heap
    media_size_classes: list[int]          # [64KB, 1MB, 8MB, 64MB]
    media_slab_counts: list[int]           # [256, 64, 16, 4]

    # Checkpoint
    checkpoint_interval: float = 30.0      # Giây
    checkpoint_lsn_threshold: int = 1000   # Mutation
    checkpoint_max_keep: int = 5           # Giữ lại N file

    # Satellite
    vector_dim: int = 128                  # Dimension mặc định
    satellite_poll_us: int = 100           # Polling interval (μs)

    # Monitor
    heartbeat_interval: float = 5.0        # Kiểm tra mỗi 5s
    max_restart_attempts: int = 3          # Tối đa restart/satellite

    # Isolation
    process_isolation: bool = False        # Thread vs Process
```

### 7.4 CLI — Giao diện dòng lệnh

```bash
# Khởi động QM
python qm_app.py --start

# Khởi động với cấu hình tùy chỉnh
python qm_app.py --start \
    --port 5433 \
    --data-dir /ssd/qm_data \
    --checkpoint-interval 60 \
    --vector-dim 256 \
    --log-level DEBUG

# Kiểm tra trạng thái
python qm_app.py --status

# Tắt daemon
python qm_app.py --stop
```

**Tham số CLI:**

| Flag | Mặc định | Mô tả |
|------|----------|--------|
| `--start` | — | Khởi động daemon |
| `--status` | — | Hiển thị trạng thái (đọc state file) |
| `--stop` | — | Gửi SIGTERM đến daemon |
| `--data-dir` | `/tmp/qm_data` | Thư mục dữ liệu |
| `--host` | `127.0.0.1` | Địa chỉ bind |
| `--port` | `5433` | Cổng PostgreSQL |
| `--checkpoint-interval` | `30.0` | Khoảng cách checkpoint (giây) |
| `--vector-dim` | `128` | Dimension vector mặc định |
| `--process-isolation` | `False` | Dùng OS process cho satellite |
| `--log-level` | `INFO` | Mức log: DEBUG/INFO/WARNING/ERROR |

### 7.5 Quản lý tiến trình

- **PID file**: `{data_dir}/qm_daemon.pid` — chứa PID của daemon
- **State file**: `{data_dir}/qm_daemon.state` — JSON chứa trạng thái hiện tại
- **Signal handling**: SIGINT (Ctrl+C) và SIGTERM → graceful shutdown
- **Auto-restart**: Monitor thread theo dõi satellite, tự động khởi động lại nếu chết (tối đa 3 lần)

### 7.6 Ví dụ output khởi động

```
10:30:00 [qm.daemon] INFO === QM Daemon Bootstrap ===
10:30:00 [qm.daemon] INFO Data directory: /ssd/qm_data
10:30:00 [qm.daemon] INFO [1/4] Initializing Hub Engine...
10:30:00 [qm.daemon] INFO   Hub Engine v2.0.0-hub ready
10:30:00 [qm.daemon] INFO [2/4] Initializing Media Heap...
10:30:00 [qm.daemon] INFO   Media Heap: 4 classes, 464.0 MB total capacity
10:30:00 [qm.daemon] INFO [3/4] Initializing Checkpoint Manager...
10:30:00 [qm.daemon] INFO [4/4] Checking for recovery...
10:30:00 [qm.daemon] INFO   No checkpoint found — fresh start
10:30:00 [qm.checkpoint] INFO Auto-checkpoint started (interval=30.0s, threshold=1000)
10:30:00 [qm.daemon] INFO Health monitor started (interval=5.0s)
10:30:00 [qm.daemon] INFO PostgreSQL gateway listening on 127.0.0.1:5433
10:30:00 [qm.daemon] INFO === QM Daemon Ready (0.15s) ===
10:30:00 [qm.daemon] INFO   Engine:     v2.0.0-hub
10:30:00 [qm.daemon] INFO   Gateway:    127.0.0.1:5433 (PostgreSQL wire protocol)
10:30:00 [qm.daemon] INFO   Media Heap: 464.0 MB
10:30:00 [qm.daemon] INFO   Checkpoint: every 30s or 1000 mutations
10:30:00 [qm.daemon] INFO   PID:        12345
```

---

## 8. Luồng Dữ Liệu End-to-End

### 8.1 Truy vấn SQL thông thường

```
psql client
    │
    │  "SELECT * FROM users WHERE age > 25;"
    ▼
┌──────────────────────┐
│ PostgreSQL Gateway    │  Nhận query qua wire protocol
│ (port 5433)          │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ QMHubEngine          │
│   1. QueryParser     │  SQL → AST (QMSQLParser / Lark LALR)
│   2. QueryPlanner    │  AST → Execution Plan
│   3. CostModel       │  Ước tính chi phí
│   4. Adaptive Opt.   │  Tối ưu hóa bằng ML
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ HubDispatcher        │
│   LSN Sequencer      │  Gán số thứ tự toàn cục
│   dispatch_sync()    │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Ring Buffer (Gen)    │  [state=READY, lsn=42, cmd=QUERY]
│                      │  [payload = serialized plan]
│                      │
│  General Satellite   │  Đọc ring → Thực thi truy vấn
│  (scan, filter, agg) │  → Ghi kết quả [state=DONE]
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Hub nhận kết quả     │  Đọc slot DONE → deserialize
│ → Trả về client      │  → Gửi qua PostgreSQL wire protocol
└──────────────────────┘
```

### 8.2 Tìm kiếm vector (SEARCH VECTOR)

```
"SEARCH VECTOR [0.1, 0.5, ...] IN photos TOP 10"
    │
    ▼
QMSQLParser → SearchVectorStmt
    │
    ▼
HubDispatcher → Ring Buffer (Vec) → Vector Satellite
                                     │
                                     ▼
                                   HNSW Index Scan
                                   └→ Top-K nearest neighbors
                                     │
                                     ▼
                                   Kết quả [(id, distance), ...]
                                   → Ring Buffer DONE
    │
    ▼
Hub → Client (qua PostgreSQL wire)
```

### 8.3 Liên kết media (LINK MEDIA)

```
"LINK MEDIA '/data/photo.jpg' TO photos ROW_ID 42"
    │
    ▼
QMSQLParser → LinkMediaStmt
    │
    ▼
QMHubEngine:
    1. alloc = media_allocator.allocate(file_size)
    2. media_allocator.write(alloc, file_bytes)
    3. handle = alloc.to_bytes()  (32 bytes)
    │
    ▼
Ring Buffer → General Satellite
    │  handle gắn vào metadata bản ghi photos[42]
    ▼
Satellite:
    view = media_allocator.memoryview_of(handle)
    # Truy cập zero-copy vào media data
```

---

## 9. Hướng Dẫn Sử Dụng

### 9.1 Cài đặt

```bash
# Clone và cài đặt
cd /path/to/QM
pip install -e ".[vector,analytics]"

# Hoặc chỉ core
pip install -e .
```

### 9.2 Khởi động nhanh

```bash
# Khởi động QM daemon
python qm_app.py --start --data-dir /tmp/qm_demo

# Kết nối bằng psql
psql -h 127.0.0.1 -p 5433
```

```sql
-- Tạo bảng
CREATE TABLE users (id INTEGER, name VARCHAR, age INTEGER);

-- Chèn dữ liệu
INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30);

-- Truy vấn
SELECT * FROM users WHERE age > 25;

-- Tìm kiếm vector
SEARCH VECTOR [0.1, 0.5, -0.3] IN embeddings TOP 5 METRIC cosine;

-- Xem trạng thái media heap
SHOW SLABS;

-- Checkpoint thủ công
CHECKPOINT FULL;
```

### 9.3 Sử dụng từ Python

```python
from qm_core.hub_engine import QMHubEngine
from qm_core.execution.qm_sql import QMSQLParser

# Tạo engine
engine = QMHubEngine(data_dir="/tmp/qm_python")

# Tạo bảng
engine.create_table("users", {
    "id": "INTEGER",
    "name": "VARCHAR",
    "age": "INTEGER",
})

# Chèn dữ liệu
engine.insert("users", [
    {"id": 1, "name": "Alice", "age": 30},
    {"id": 2, "name": "Bob", "age": 25},
])

# Truy vấn SQL
result = engine.execute_sql("SELECT * FROM users WHERE age > 25")
print(result)

# Đóng
engine.close()
```

### 9.4 Sử dụng Media Allocator

```python
from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

config = MediaAllocatorConfig(
    heap_dir="/tmp/qm_media",
    size_classes=[64*1024, 1*1024*1024],   # 64KB, 1MB
    slab_counts=[100, 20],                  # 100 slab 64KB + 20 slab 1MB
)

alloc = MediaSlabAllocator(config)

# Cấp phát slab cho ảnh
image_bytes = open("photo.jpg", "rb").read()
handle = alloc.allocate(len(image_bytes))
alloc.write(handle, image_bytes)

# Đọc lại
data = alloc.read(handle, len(image_bytes))
assert data == image_bytes

# Zero-copy access
view = alloc.memoryview_of(handle)

# Giải phóng
alloc.free(handle)

# Thống kê
print(alloc.stats())
# [{'slab_size': 65536, 'slab_count': 100, 'free': 100, 'used': 0},
#  {'slab_size': 1048576, 'slab_count': 20, 'free': 20, 'used': 0}]

alloc.close()
```

### 9.5 Sử dụng Checkpoint Manager

```python
from qm_core.checkpoint import CheckpointManager, CheckpointConfig

config = CheckpointConfig(
    checkpoint_dir="/tmp/qm_ckpt",
    interval_seconds=60,
    lsn_threshold=500,
    max_checkpoints=3,
)

def get_state():
    return {
        "lsn": 42,
        "epoch": 1,
        "table_meta": {"users": {"row_count": 100}},
        "merkle_root": b"\x00" * 32,
    }

mgr = CheckpointManager(config, state_provider=get_state)
mgr.start()

# Thông báo mỗi mutation
for i in range(500):
    mgr.notify_mutation()  # Khi vượt threshold → auto checkpoint

# Checkpoint thủ công
record = mgr.force_checkpoint("full")
print(f"LSN={record.lsn}, epoch={record.epoch}")

# Phục hồi
state = mgr.recover_state()
print(state)  # {"lsn": 42, "epoch": 1, ...}

mgr.stop()
```

---

## 10. Kiểm Thử

### 10.1 Tổng quan bộ test

```
Tổng số test:     555+
├── Test gốc:     498 (OLTP, OLAP, index, optimizer, ...)
├── Test kiến trúc: 43 (Hub-Satellite, ring buffer, dispatcher)
└── Test Final Sprint: 57 (media, QM-SQL, checkpoint, daemon)
```

### 10.2 Chạy test

```bash
# Chạy toàn bộ
python -m pytest -v

# Chạy chỉ test Final Sprint
python -m pytest tests/test_final_sprint.py -v

# Chạy test cụ thể
python -m pytest tests/test_final_sprint.py::TestQMSQLParser -v
python -m pytest tests/test_final_sprint.py::TestMediaSlabAllocator -v
```

### 10.3 Chi tiết bộ test Final Sprint (`tests/test_final_sprint.py`)

| Lớp test | Số test | Mô tả |
|----------|---------|--------|
| `TestSlabPool` | 7 | Pool đơn lớp: cấp phát, cạn kiệt, đọc/ghi, memoryview, free idempotent, invalid index, attach |
| `TestMediaSlabAllocator` | 8 | Đa lớp: cấp phát tự động, round-trip, zero-copy, handle serialize, stats, exhaustion, oversize, attach |
| `TestQMSQLParser` | 22 | QM extensions (6) + SQL chuẩn (10) + biểu thức (4) + error handling (1) + case-insensitive (1) |
| `TestCheckpointRecord` | 2 | Round-trip serialize, kiểm tra tính toàn vẹn SHA-256 |
| `TestCheckpointManager` | 5 | Force checkpoint, recover, prune, background thread, stats |
| `TestQMDaemon` | 5 | Config defaults, bootstrap, status, format_bytes, state provider |
| `TestMediaIPCIntegration` | 1 | Handle qua ring buffer end-to-end |
| `TestImports` | 4 | Import smoke test cho 4 module mới |
| **Tổng** | **57** | |

### 10.4 Ví dụ test chi tiết

```python
# Test tìm kiếm vector qua QM-SQL
def test_search_vector_basic(self):
    """SEARCH VECTOR [...] IN table TOP k"""
    ast = self.parser.parse(
        "SEARCH VECTOR [0.1, 0.5, -0.3] IN photos TOP 10;"
    )
    assert isinstance(ast, SearchVectorStmt)
    assert ast.vector == [0.1, 0.5, -0.3]
    assert ast.table == "photos"
    assert ast.top_k == 10
    assert ast.metric == "cosine"  # mặc định

# Test zero-copy media
def test_zero_copy_memoryview(self):
    """memoryview trực tiếp vào mmap."""
    handle = self.alloc.allocate(1024)
    data = b"HELLO_MEDIA" * 50
    self.alloc.write(handle, data)

    view = self.alloc.memoryview_of(handle)
    assert bytes(view[:len(data)]) == data  # Đọc không sao chép

# Test checkpoint integrity
def test_integrity_check(self):
    """Corrupt checkpoint → ValueError."""
    record = CheckpointRecord(lsn=1, epoch=0, ...)
    data = bytearray(record.to_bytes())
    data[10] ^= 0xFF  # Corrupt 1 byte

    with pytest.raises(ValueError, match="integrity"):
        CheckpointRecord.from_bytes(bytes(data))
```

---

## 11. Phụ Lục — Cấu Trúc Thư Mục

```
QM/
├── qm_app.py                          # ★ Entry point — QM Daemon
├── pyproject.toml                     # Metadata dự án + dependencies
├── qm_core/
│   ├── hub_engine.py                  # ★ Engine facade chính
│   ├── engine.py                      # Engine cũ (legacy)
│   ├── schema.py                      # Định nghĩa schema
│   ├── checkpoint.py                  # ★ Auto-Checkpoint Manager
│   ├── concurrency.py                 # MVCC, locking
│   ├── hub/
│   │   ├── hub.py                     # Hub orchestrator
│   │   ├── dispatcher.py              # ★ HubDispatcher (3 ring)
│   │   ├── lsn_sequencer.py           # ★ LSN Sequencer
│   │   └── merkle_auditor.py          # ★ Merkle Auditor
│   ├── ipc/
│   │   ├── ring_buffer.py             # ★ Lock-free Ring Buffer
│   │   └── media_allocator.py         # ★ Media Slab Allocator
│   ├── satellite/
│   │   ├── base.py                    # Cấu hình satellite
│   │   ├── general_satellite.py       # CRUD, DDL
│   │   ├── vector_satellite.py        # HNSW, ANN
│   │   ├── procedure_satellite.py     # PL/QM
│   │   └── worker.py                  # Worker thread/process
│   ├── execution/
│   │   ├── sql_parser.py              # SQL parser gốc + AST nodes
│   │   ├── qm_sql.py                  # ★ QM-SQL Lark parser
│   │   ├── parser.py                  # Query parser
│   │   ├── planner.py                 # Query planner
│   │   ├── pipeline.py                # Execution pipeline
│   │   ├── join.py                    # Join operators
│   │   ├── window.py                  # Window functions
│   │   └── vectorized.py             # Vectorized execution
│   ├── storage/                       # Lớp lưu trữ
│   ├── index/                         # B-tree, hash, bitmap, vector
│   ├── statistics/                    # Cost model
│   ├── optimizer/                     # Adaptive optimizer
│   ├── learned/                       # ML-based optimization
│   └── distributed/                   # Sharding
├── gateway/
│   ├── api_postgres/                  # ★ PostgreSQL wire protocol
│   ├── api_http/                      # HTTP REST API
│   ├── api_ws/                        # WebSocket API
│   ├── auth/                          # Authentication/ACL
│   └── query_router/                  # Query routing
├── tests/
│   ├── test_final_sprint.py           # ★ 57 test cho module mới
│   └── ...                            # 498+ test khác
└── docs/
    └── QM_DOCUMENTATION_VI.md         # ★ Tài liệu này
```

**Ghi chú:** Các file đánh dấu ★ là thành phần được tạo hoặc chỉnh sửa trong giai đoạn Final Sprint.

---

## Bảng Thuật Ngữ

| Thuật ngữ | Tiếng Anh | Giải thích |
|-----------|-----------|------------|
| Bộ nhớ chia sẻ | Shared Memory | Vùng nhớ mmap dùng chung giữa các tiến trình |
| Bộ cấp phát slab | Slab Allocator | Cấp phát khối nhớ cố định theo lớp kích thước |
| Bộ đệm vòng | Ring Buffer | Cấu trúc dữ liệu vòng tròn FIFO |
| Cây Merkle | Merkle Tree | Cây băm dùng kiểm tra tính toàn vẹn dữ liệu |
| Điểm kiểm tra | Checkpoint | Snapshot trạng thái ghi ra ổ đĩa |
| Epoch | Epoch | Giai đoạn phục hồi, tăng sau mỗi lần crash |
| LSN | Log Sequence Number | Số thứ tự đơn điệu cho mỗi thao tác |
| Satellite | Satellite | Worker chuyên biệt xử lý một loại tác vụ |
| Hub | Hub | Trung tâm điều phối, phân phối lệnh |
| WAL | Write-Ahead Log | Nhật ký ghi trước để đảm bảo durability |
| Zero-copy | Zero-copy | Truy cập dữ liệu không sao chép qua memoryview/mmap |
| LALR | Look-Ahead LR | Thuật toán phân tích cú pháp tuyến tính |
| HNSW | Hierarchical Navigable Small World | Thuật toán tìm kiếm vector gần nhất |
| ANN | Approximate Nearest Neighbor | Tìm kiếm hàng xóm gần đúng |
| MVCC | Multi-Version Concurrency Control | Điều khiển đồng thời đa phiên bản |
| fsync | File Sync | Đồng bộ dữ liệu ra ổ đĩa vật lý |

---

*Tài liệu được tạo tự động cho dự án QM v2.0.0-hub*
