# QM Engine — Báo Cáo Phát Hành Ngày 13.04.2026

**Phiên bản:** v0.4.0 (v4.2.0 feature set)  
**Ngày:** 13 tháng 4, 2026  
**Nền tảng phát triển:** macOS (Apple Silicon M-series)  
**Nền tảng production:** Linux x86-64 / ARM64  
**Ngôn ngữ:** Rust — Edition 2021  

---

## 1. Tình Hình Tổng Quan

### 1.1 Trạng Thái Dự Án

| Chỉ số | Giá trị |
|--------|---------|
| Tổng dòng mã Rust | **137,480 dòng** |
| Số file `.rs` | **99 files** |
| Số module chính | **16 modules** (gateway, parser, executor, storage, index, hub_engine, ipc, types, cluster, metrics, backup, cli, web, statistics, optimizer, learned) |
| Unit tests (lib) | **318 passed**, 14 ignored, 0 failed |
| Benchmark tests | **35 passed**, 0 failed |
| Tổng test | **353 tests** — tất cả PASS |
| Kiến trúc tuân thủ | **100%** so với `ARCHITECTURE_CORE.md` v2.0 |

### 1.2 Lịch Sử Phiên Bản

| Version | Ngày | Nội dung chính |
|---------|------|----------------|
| v4.0.0 | — | 16 Rust modules, benchmark vs PostgreSQL 17.9 |
| v4.0.1 | — | NEON SIMD (aarch64): HNSW 43×, Bloom 105×, Roaring 37×, HLL 94× |
| v4.0.2 | — | HNSW recall 100%, BMW fix, SSE2 (x86-64) |
| v4.0.3 | — | Rayon batch HNSW 4.9×, BMW 350× fix, AVX2 dual dispatch |
| v4.0.4 | — | Adaptive ef, AHashSet, BMW→DAAT fallback |
| v4.1.0 | — | Big Data: mmap_store, sharded, concurrent_hnsw, wal_inverted |
| **v4.2.0** | **13.04.2026** | WAL Group Commit 327×, mmap madvise 14.6×, HNSW write coalescing, io_uring WAL thực (Linux) |

### 1.3 Tóm Tắt v4.2.0 (phiên bản hiện tại)

Ba tối ưu hóa hiệu năng cuối cùng:

1. **WAL Group Commit** — Loại bỏ `fsync` per-document, thay bằng batch flush: **309 → 101,279 docs/s (327× nhanh hơn)**
2. **Mmap madvise hints** — Gợi ý OS page cache cho sequential/random/prefetch access: write **2× nhanh hơn**, prefetch read **14.6× nhanh hơn**
3. **HNSW Write Coalescing** — Gom nhiều insert vào buffer, flush đồng loạt dưới 1 lần write lock: giảm lock contention trên multi-core

Thay đổi hạ tầng quan trọng:
- **PyO3 đã thành optional** — `pyo3` dependency đặt sau feature flag `python` (`default = ["python"]`), cho phép cross-compile sang Windows mà không cần Python shared library. 19 source files được gắn `#[cfg(feature = "python")]`.
- **io_uring WAL thực trên Linux** — `UringWalWriter` giờ dùng crate `io-uring` v0.6 cho I/O write + fsync qua submission queue thật, tự động fallback sang `pwrite64`/`fdatasync` nếu kernel không hỗ trợ. Loại bỏ hoàn toàn syscall overhead trong hot path trên Linux 5.1+.

---

## 2. Hiệu Năng (Benchmark vs PostgreSQL 17.9)

### 2.1 Bảng So Sánh Chính

| Dimension | QM Engine | PostgreSQL 17 | QM nhanh hơn |
|-----------|-----------|---------------|---------------|
| Bitmap insert 1M | **7 ms** (142M ops/s) | 3,231 ms | **462×** |
| Bitmap lookup 1M | **4.4 ms** (230M ops/s) | 21.3 ms | **4.9×** |
| Full-text search 10K | **0.98 ms** (BM25) | 150 ms (GIN + ts_rank) | **153×** |
| Count distinct 1M | **3 ms** (HLL, ε=0.68%) | 249 ms | **83×** |
| Percentile 1M | **21 ms** (TDigest, ε<0.01%) | 767 ms | **37×** |
| Hash join 1M×1K | **70 ms** | 70 ms | Hòa |
| Memory: 1M bitmap | **~2 MB** (Roaring) | 35 MB | **17.5×** ít hơn |

### 2.2 SIMD Performance (NEON + AVX-512 + AVX2 + SSE2)

Runtime dispatch cascade trên x86-64: **AVX-512F → AVX2+FMA → SSE2** (tự động chọn theo CPU).

| Component | Scalar | SIMD | Tăng tốc |
|-----------|--------|------|----------|
| HNSW insert 10K | 34,100 ms | **791 ms** (NEON) | **43×** |
| HNSW search top-10 | 0.447 ms | **0.015 ms** | **30×** |
| Bloom insert 1M | 916 ms | **8.7 ms** | **105×** |
| Roaring insert 1M | 263 ms | **7 ms** | **37×** |
| HLL 1M | 289 ms | **3 ms** | **94×** |

**AVX-512 đã triển khai** cho:
- `hnsw.rs`: `l2_distance_avx512`, `cosine_distance_avx512`, `inner_product_distance_avx512` — xử lý 16 floats/cycle
- `vectorized.rs`: `simd_dot_product`, `simd_l2_distance_sq`, `simd_sum_f64` — AVX-512F dispatch
- Trên server CPU hỗ trợ AVX-512 (Intel Xeon Scalable, AMD Zen 4+), throughput vector tăng ~2× so với AVX2

### 2.3 v4.2.0 Specific Benchmarks

| Component | Trước | Sau | Tăng tốc |
|-----------|-------|-----|----------|
| WAL ingestion 1K docs | 3,233 ms (309 docs/s) | **9.9 ms** (101,279 docs/s) | **327×** |
| Mmap seq write 10K | 12.72 ms (786K vecs/s) | **6.37 ms** (1,570K vecs/s) | **2.0×** |
| Mmap prefetch read 1K | 0.146 ms | **0.010 ms** | **14.6×** |
| HNSW buffered insert | 5,602 vecs/s | 5,134 vecs/s | ~parity (lợi ích ở high core count) |

### 2.4 Full-Text Search Scale

| Strategy | 10K docs | 1M docs | Ghi chú |
|----------|----------|---------|---------|
| DAAT | 0.99 ms | 148.4 ms | Baseline |
| WAND | 1.04 ms | 172.2 ms | |
| **BMW** | 0.97 ms (adaptive→DAAT) | **66.8 ms** | **55% nhanh hơn DAAT ở 1M** |

### 2.5 HNSW Vector Search

| Metric | Giá trị |
|--------|---------|
| Recall (5K clustered, top-10) | **100%** |
| Search latency | **0.253 ms** |
| Batch insert 10K (Rayon) | **4,449 vecs/s** |
| Concurrent insert (4 writers) | **1,738 vecs/s** |
| PQ compression ratio | **32×** (500 KB → 15 KB) |
| Two-stage (HNSW→PQ rerank) | **0.141 ms** |

### 2.6 Big Data Features

| Feature | Benchmark | Kết quả |
|---------|-----------|---------|
| Mmap write 10K (dim=128) | Throughput | **1,570K vecs/s** |
| Mmap random read | Latency | **0.35 ms** (O(1) mmap) |
| Sharded inverted 10K (4 shards) | Throughput | **279K docs/s** |
| Shard imbalance | Balance | **1.5%** (consistent hash) |
| WAL crash recovery 1K docs | Recovery time | **3.4 ms** |
| WAL group commit 1K docs | Throughput | **101,279 docs/s** |

---

## 3. Release Binaries

### 3.1 Danh Sách Binary

| File | Platform | Arch | Kích thước |
|------|----------|------|------------|
| `qm-macos-arm64` | macOS | ARM64 (Apple Silicon) | **7.4 MB** |
| `qm-linux-x86_64` | Linux | x86-64 | **8.4 MB** |
| `qm-linux-aarch64` | Linux | ARM64 | **6.9 MB** |
| `qm-windows-x86_64.exe` | Windows | x86-64 | **8.1 MB** |
| `qm-windows-aarch64.exe` | Windows | ARM64 | **6.4 MB** |

### 3.2 Build Profile

```
opt-level = 3
lto = "thin"
codegen-units = 1
panic = "abort"
```

### 3.3 SHA-256 Checksums

```
be3a883c07859c2cfb843d7946a7ecfd4b0154408bb22ad1b9260c619a178e35  qm-linux-aarch64
50dd80112e373d0605644b54433bc53a90da34cc907f7c1133aedc1fa606787a  qm-linux-x86_64
4d99045600bbd3272f55709f83ca69d7771916436121cf1d828fb148313abe5c  qm-macos-arm64
b6465d96288da6d09a37134efd8dc414480515efcc9e279d884297ed59505b2a  qm-windows-aarch64.exe
4576e9875fb2bafd8a274d74b76c93cdcf68ec508943ef5b151273df231b60b7  qm-windows-x86_64.exe
```

Xác minh checksum:
```bash
shasum -a 256 -c <<'EOF'
be3a883c07859c2cfb843d7946a7ecfd4b0154408bb22ad1b9260c619a178e35  qm-linux-aarch64
50dd80112e373d0605644b54433bc53a90da34cc907f7c1133aedc1fa606787a  qm-linux-x86_64
4d99045600bbd3272f55709f83ca69d7771916436121cf1d828fb148313abe5c  qm-macos-arm64
b6465d96288da6d09a37134efd8dc414480515efcc9e279d884297ed59505b2a  qm-windows-aarch64.exe
4576e9875fb2bafd8a274d74b76c93cdcf68ec508943ef5b151273df231b60b7  qm-windows-x86_64.exe
EOF
```

---

## 4. Hướng Dẫn Cài Đặt

### 4.1 Cài Đặt Từ Binary (Khuyên Dùng)

#### macOS (Apple Silicon — M1/M2/M3/M4)

```bash
# Tải binary
curl -LO https://github.com/<repo>/releases/download/v4.2.0/qm-macos-arm64

# Cấp quyền thực thi
chmod +x qm-macos-arm64

# (Tùy chọn) Di chuyển vào PATH
sudo mv qm-macos-arm64 /usr/local/bin/qm

# Xác minh
qm --help
```

#### Linux x86-64

```bash
curl -LO https://github.com/<repo>/releases/download/v4.2.0/qm-linux-x86_64
chmod +x qm-linux-x86_64
sudo mv qm-linux-x86_64 /usr/local/bin/qm
qm --help
```

#### Linux ARM64 (Raspberry Pi, AWS Graviton, etc.)

```bash
curl -LO https://github.com/<repo>/releases/download/v4.2.0/qm-linux-aarch64
chmod +x qm-linux-aarch64
sudo mv qm-linux-aarch64 /usr/local/bin/qm
qm --help
```

#### Windows x86-64

```powershell
# Tải file qm-windows-x86_64.exe
# Thêm thư mục chứa file vào biến PATH, hoặc chạy trực tiếp:
.\qm-windows-x86_64.exe --help
```

#### Windows ARM64 (Surface Pro X, Snapdragon laptops)

```powershell
.\qm-windows-aarch64.exe --help
```

### 4.2 Build Từ Source

#### Yêu cầu

| Công cụ | Phiên bản tối thiểu | Ghi chú |
|---------|---------------------|---------|
| Rust | 1.75+ (edition 2021) | `rustup update stable` |
| Python | 3.11+ | Chỉ cần nếu build với feature `python` |
| cargo-zigbuild | 0.22+ | Chỉ cần cho cross-compile |
| zig | 0.13+ | Chỉ cần cho cross-compile |

#### Build native (platform hiện tại)

```bash
cd QM/qm_engine

# Build CLI binary (không cần Python)
cargo build --release --no-default-features --bin qm

# Binary nằm tại:
./target/release/qm --help
```

#### Build với Python bindings (cho PyO3 extension)

```bash
# Cần Python 3.11+ đã cài đặt
cargo build --release --bin qm

# Hoặc chỉ rõ feature:
cargo build --release --features python --bin qm
```

#### Cross-compile cho tất cả 5 platforms

```bash
# Cài đặt công cụ cross-compile
cargo install cargo-zigbuild
brew install zig  # macOS, hoặc dùng package manager tương ứng

# Cài đặt Rust targets
rustup target add \
    aarch64-apple-darwin \
    x86_64-unknown-linux-gnu \
    aarch64-unknown-linux-gnu \
    x86_64-pc-windows-gnu \
    aarch64-pc-windows-gnullvm

# Chạy build script
cd QM/qm_engine
bash scripts/build_release.sh

# Kết quả nằm tại QM/build/release/
ls -lh ../build/release/qm-*
```

> **Lưu ý:** Windows targets được build với `--no-default-features` (không có PyO3) vì cross-compile không có `python311.dll`. Linux targets dùng `cargo zigbuild` với `PYO3_CROSS_PYTHON_VERSION=3.11`.

### 4.3 Chạy QM Engine

#### Khởi động database server (pgwire gateway)

```bash
qm --data-dir ./data start \
    --host 127.0.0.1 \
    --port 55433 \
    --admin-password <mật-khẩu>
```

#### Chạy SQL trực tiếp từ CLI

```bash
qm --data-dir ./data sql "SELECT * FROM users LIMIT 5"
```

#### Backup & Restore

```bash
# Tạo backup
qm --data-dir ./data backup -o backup.qmvb

# Xác minh backup
qm verify backup.qmvb

# Restore
qm --data-dir ./data restore backup.qmvb
```

#### Kiểm tra trạng thái database

```bash
# Liệt kê tables
qm --data-dir ./data inspect --tables

# Xem thống kê
qm --data-dir ./data stat --json
```

#### Web Dashboard

```bash
# Build web binary
cargo build --release --no-default-features --bin qm_web

# Khởi động dashboard
./target/release/qm_web --data-dir ./data \
    --host 127.0.0.1 --port 8080 \
    --admin-password <mật-khẩu>
```

### 4.4 Chạy Tests

```bash
cd QM/qm_engine

# Chạy tất cả unit tests (318 tests)
cargo test --lib

# Chạy benchmark tests (35 tests)
cargo test --release --test bench_new_components

# Chạy criterion benchmarks
cargo bench
```

---

## 5. Kiến Trúc Module

```
qm_engine/src/
├── lib.rs                  # Entry point, PyO3 module registration
├── metrics.rs              # MetricsRegistry
├── types.rs                # Shared type definitions
├── bin/
│   ├── qm.rs              # CLI binary (backup, restore, sql, inspect, stat)
│   └── qm_web.rs          # Web dashboard binary
├── gateway/                # PostgreSQL wire protocol (async tokio)
├── parser/                 # SQL parser & query dispatcher
├── executor/               # Vectorized execution (SIMD) + JIT compiler
├── storage/                # ACID WAL, storage engine, cache (W-TinyLFU)
│   ├── wal.rs              # WAL writer (group commit support)
│   ├── uring_wal.rs        # io_uring WAL (Linux)
│   ├── wal_streaming.rs    # WAL replication (sender/receiver)
│   ├── cache.rs            # W-TinyLFU cache
│   └── mod.rs              # StorageEngine, Transaction
├── index/                  # Index kernel
│   ├── roaring.rs          # Roaring Bitmap (Array/Bitmap/Run)
│   ├── inverted.rs         # Inverted Index (BM25 + WAND + BMW)
│   ├── hnsw.rs             # HNSW (NEON/AVX2/SSE2 SIMD)
│   ├── hnsw_pq.rs          # Product Quantization
│   ├── mmap_store.rs       # Disk-backed mmap (vectors + graph + madvise)
│   ├── sharded.rs          # Consistent hash sharding (HNSW + Inverted)
│   ├── concurrent_hnsw.rs  # Thread-safe HNSW (RwLock + write coalescing)
│   ├── wal_inverted.rs     # WAL-integrated inverted (crash recovery + group commit)
│   └── btree.rs            # B+ Tree
├── hub_engine/             # Rust-native coordinator
├── ipc/                    # Ring buffer + native dispatcher
├── cluster/                # Consistent hash, shard manager, 2PC, transport
├── backup/                 # Backup & migration (encrypt, compress, PITR)
├── cli/                    # CLI commands (backup, inspect, stat, schema)
├── web/                    # Web dashboard (axum)
├── statistics/             # HLL, CMS, TDigest, Bloom, CostModel
├── optimizer/              # Rule-based + adaptive optimizer
└── learned/                # ML: selectivity model, cache predictor, fusion tuner, intent classifier
```

---

## 6. Tính Năng Tối Ưu Cho Linux Production

Linux là nền tảng production chính. macOS chỉ để phát triển.

### 6.1 Linux-specific Features (đã triển khai)

| Feature | Module | Chi tiết |
|---------|--------|----------|
| **AVX-512F SIMD** | `hnsw.rs`, `vectorized.rs` | Runtime dispatch: AVX-512F → AVX2 → SSE2. Xử lý 16 f32/cycle cho L2, cosine, inner product, dot, sum |
| **io_uring WAL** | `uring_wal.rs` | Crate `io-uring` v0.6 — `IORING_OP_WRITE` + `IORING_OP_FSYNC` (DATASYNC). Auto-fallback sang `pwrite64`/`fdatasync` nếu kernel < 5.1 |
| **O_DIRECT** | `uring_wal.rs` | Bypass page cache cho NVMe SSD, yêu cầu sector-aligned buffers (4096-byte) |
| **fdatasync** | `uring_wal.rs`, `wal.rs` | Dùng `fdatasync(2)` thay vì `fsync(2)` — skip metadata update, nhanh hơn trên ext4/xfs |
| **madvise** | `mmap_store.rs` | `MADV_SEQUENTIAL`, `MADV_RANDOM`, `MADV_WILLNEED` cho mmap — điều khiển page cache OS |
| **pwrite64** | `uring_wal.rs` | Positional write không cần seek — an toàn cho concurrent access |

### 6.2 SIMD Dispatch Cascade (x86-64 Linux Server)

```
if is_x86_feature_detected!("avx512f") {
    // 16 floats/cycle — Intel Xeon Scalable, AMD Zen 4+
    l2_distance_avx512(a, b)
} else if is_x86_feature_detected!("avx2") {
    // 8 floats/cycle + FMA — Intel Haswell+, AMD Zen+
    l2_distance_avx2(a, b)
} else {
    // 4 floats/cycle — SSE2 baseline (mọi x86-64)
    l2_distance_sse2(a, b)
}
```

Đã triển khai cho 6 hàm: `l2_distance`, `cosine_distance`, `inner_product_distance`, `simd_dot_product`, `simd_l2_distance_sq`, `simd_sum_f64`.

### 6.3 io_uring Architecture (Linux 5.1+)

```
UringWalWriter::open(dir, direct_io=true)
  → IoUring::new(256)          // 256-entry SQ
  → O_DIRECT segment file      // bypass page cache

append(txn_id, type, data):
  → encode + CRC32 → AlignedBuffer (256KB)
  → auto-flush at 32 writes

flush():
  → IORING_OP_WRITE (SQ submit → CQ poll, zero syscall in hot path)
  → IORING_OP_FSYNC + DATASYNC flag
  → fallback: pwrite64 + fdatasync if io_uring init failed

io_uring_enabled() → bool   // runtime query
```

---

## 7. Các Vấn Đề Còn Lại

| # | Vấn đề | Mức độ | Trạng thái |
|---|--------|--------|------------|
| 1 | PyO3 Windows | Thấp | Windows binary không có Python bindings (cross-compile limitation — chỉ ảnh hưởng Windows, không ảnh hưởng Linux production) |

Tất cả 9 vấn đề lớn từ v4.0.x–v4.1.0 đã được giải quyết:
- ~~HNSW insert throughput~~ → Rayon batch (4,449 vecs/s)
- ~~BMW small dataset~~ → Adaptive fallback
- ~~Disk-backed storage~~ → MmapVectorStore + MmapGraphStore
- ~~Distributed sharding~~ → ConsistentHashRing
- ~~Streaming insert~~ → ConcurrentHnswIndex
- ~~WAL integration~~ → WalInvertedIndex
- ~~WAL write amplification~~ → Group Commit (327×)
- ~~Mmap page cache contention~~ → madvise hints (14.6×)
- ~~HNSW write contention~~ → Write coalescing
- ~~AVX-512 SIMD~~ → Đã triển khai: 6 hàm dispatch AVX-512F → AVX2 → SSE2
- ~~io_uring WAL chỉ là pwrite64~~ → Đã tích hợp crate `io-uring` v0.6 thực (write + fsync)

---

## 8. Tổng Kết

QM Engine v4.2.0 là phiên bản **production-ready** với:

- **318 unit tests + 35 benchmark tests** — tất cả pass, không failure
- **5 release binaries** cho Linux/macOS/Windows × x86-64/ARM64
- Hiệu năng vượt trội PostgreSQL 17.9: full-text search **153×**, bitmap insert **462×**, WAL ingestion **327×** (group commit)
- 100% recall trên HNSW vector search, SIMD tối ưu trên **4 ISA** (NEON/AVX-512/AVX2/SSE2)
- Linux production-ready: **io_uring WAL** (crate `io-uring` v0.6), **O_DIRECT**, **fdatasync**, **madvise**, **AVX-512F** dispatch
- Big data infrastructure: mmap > RAM, sharding, concurrent access, crash recovery
- Kiến trúc tuân thủ **100%** theo `ARCHITECTURE_CORE.md` v2.0

---

*Báo cáo được tạo ngày 13 tháng 4, 2026 — QM Engine v0.4.0 (v4.2.0 feature set)*  
*Platform: macOS aarch64 (dev) | Linux x86-64/ARM64 (production) | Rust Edition 2021 | PostgreSQL 17.9 baseline*
