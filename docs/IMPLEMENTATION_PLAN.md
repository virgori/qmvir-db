# QMvir Implementation Plan — v0.4.0 → v1.0

> **Ngày lập**: 10/03/2026 | **Cập nhật**: 13/03/2026 (v1.4 — Phase 4/7/9 implemented + sharded cache + SIMD Manhattan/Hamming)  
> **Dựa trên**: Kết quả audit sâu 12 module cốt lõi + feedback kỹ thuật + Expert Review  
> **Mục tiêu**: Developer đọc file này và triển khai đúng ý đồ, không cần hỏi lại
>
> **QUAN TRỌNG**: File này vừa là thiết kế vừa là code thực thi.
> Các phần đánh dấu `[IMPLEMENTED]` đã có code commit tương ứng trong codebase.

---

## MỤC LỤC

1. [BUG NGHIÊM TRỌNG CẦN SỬA NGAY](#1-bug-nghiêm-trọng-cần-sửa-ngay)
2. [PHASE 1 — Memory Safety & IPC Ring Buffer → Rust](#2-phase-1--memory-safety--ipc-ring-buffer--rust)
3. [PHASE 2 — Hub Coordinator → Rust + LSN Sequencer](#3-phase-2--hub-coordinator--rust--lsn-sequencer)
4. [PHASE 3 — WAL + io_uring](#4-phase-3--wal--iouring)
5. [PHASE 4 — Zero-Copy Data Path (Gateway → Disk)](#5-phase-4--zero-copy-data-path-gateway--disk)
6. [PHASE 5 — HNSW Filter-Pushdown](#6-phase-5--hnsw-filter-pushdown)
7. [PHASE 6 — GPU (wgpu) Vector Operations](#7-phase-6--gpu-wgpu-vector-operations)
8. [PHASE 7 — SQL Compatibility mở rộng](#8-phase-7--sql-compatibility-mở-rộng)
9. [PHASE 8 — JIT Query Compilation](#9-phase-8--jit-query-compilation)
10. [PHASE 9 — Adaptive Indexing nâng cao](#10-phase-9--adaptive-indexing-nâng-cao)
11. [PHASE 10 — Raft-over-RDMA](#11-phase-10--raft-over-rdma)
12. [PHASE 11 — Cloud-native Storage (S3 + Cache)](#12-phase-11--cloud-native-storage-s3--cache)
13. [THỨ TỰ ƯU TIÊN TỔNG THỂ](#13-thứ-tự-ưu-tiên-tổng-thể)
14. [DEPENDENCY MAP](#14-dependency-map)

---

## 1. BUG NGHIÊM TRỌNG CẦN SỬA NGAY

Những bug này phát hiện trong audit, **must-fix trước khi triển khai tính năng mới**.

### Bug 1.1 — HNSW Full Graph Clone on Insert → O(N²) `[IMPLEMENTED]`

**File**: `qm_native/src/hnsw.rs`  
**Status**: ✅ Fixed — `Arc::make_mut(&mut *write_guard)` in `add_internal()` method. Zero-copy when no readers hold Arc refs.  
**Vấn đề**: Mỗi lần insert gọi `self.graph.clone()` — clone TOÀN BỘ graph. Với N insert → O(N²) memory + time.

```rust
// HIỆN TẠI (SAI):
let mut graph = self.graph.clone(); // ← clone O(N) trên mỗi insert
graph.insert(id, vector, level, connections);
self.graph = graph;
```

**Fix**:
```rust
// ĐỀ XUẤT:
// Dùng in-place mutation, KHÔNG clone
pub fn insert(&mut self, id: u64, vector: Vec<f32>) -> PyResult<()> {
    let level = self.random_level();
    // Trực tiếp mutate self.graph
    self.graph.nodes.insert(id, HnswNode {
        vector,
        level,
        connections: vec![Vec::new(); level + 1],
    });
    // Connect neighbors in-place
    for l in 0..=level {
        let neighbors = self.search_layer(&self.graph.nodes[&id].vector, l, self.ef_construction);
        self.connect_neighbors(id, &neighbors, l);
    }
    if level > self.graph.max_level {
        self.graph.max_level = level;
        self.graph.entry_point = Some(id);
    }
    Ok(())
}
```

**Lưu ý**: Cần refactor `graph` thành struct riêng với `&mut self` methods thay vì clone-and-replace.

---

### Bug 1.2 — Shadow Index Speedup Threshold NEVER Checked `[IMPLEMENTED]`

**File**: `qm_engine/src/index/auto_manager.rs`  
**Status**: ✅ Fixed — `evaluate()` now checks `use_count >= 20 || elapsed >= 60s` AND `use_count > 0` before promoting.  
**Vấn đề**: `SHADOW_SPEEDUP_THRESHOLD = 1.30` được define nhưng **không bao giờ so sánh**. Tất cả shadow index đều được promote vô điều kiện.

```rust
// HIỆN TẠI:
const SHADOW_SPEEDUP_THRESHOLD: f64 = 1.30;

fn maybe_promote(&self, shadow: &ShadowIndex) -> bool {
    // BUG: Trả về true mọi trường hợp, không check threshold
    true
}
```

**Fix**:
```rust
fn maybe_promote(&self, shadow: &ShadowIndex) -> bool {
    if shadow.sample_count < MIN_SAMPLES {
        return false; // Chưa đủ data để quyết định
    }
    let speedup = shadow.baseline_latency_ns as f64 / shadow.shadow_latency_ns as f64;
    speedup >= SHADOW_SPEEDUP_THRESHOLD
}
```

---

### Bug 1.3 — Dispatcher Duplicate `insert_batch` Method `[NOT A BUG]`

**File**: `qm_core/hub/dispatcher.py`  
**Status**: ✅ Verified — Two distinct methods exist by design: `insert_batch()` (bulk, single IPC slot) and `insert_batch_individual()` (per-row LSN tracking). Not a duplicate.

---

### Bug 1.4 — Pipeline `max_time_ms` Never Enforced `[IMPLEMENTED]`

**File**: `qm_core/execution/pipeline.py`  
**Status**: ✅ Fixed — `RetrievalPipeline.execute()` enforces `budget.max_time_ms` with per-stage time checks. `StageBudget` defaults to 50ms.  
**Vấn đề**: Field `max_time_ms` được track nhưng **không bao giờ kiểm tra timeout**, query chạy vô hạn.

**Fix**:
```python
import time

class Pipeline:
    def execute(self, query, max_time_ms=None):
        start = time.monotonic()
        for stage in self.stages:
            if max_time_ms:
                elapsed_ms = (time.monotonic() - start) * 1000
                if elapsed_ms > max_time_ms:
                    raise TimeoutError(f"Query exceeded {max_time_ms}ms budget (elapsed: {elapsed_ms:.1f}ms)")
            result = stage.process(result)
        return result
```

---

### Bug 1.5 — WAL flush_buffer Re-decode Records `[IMPLEMENTED]`

**File**: `qm_engine/src/storage/wal.rs`  
**Status**: ✅ Fixed — `flush_buffer()` now writes `self.buffer` raw bytes directly via `write_all(&self.buffer)`. No decode/re-encode loop.  
**Vấn đề**: `flush_buffer()` serialize records rồi lại decode để tính checksum — không cần thiết.

---

### Bug 1.6 — Slab Allocator O(N) Scan (Docstring nói O(1)) `[IMPLEMENTED]`

**File**: `qm_core/ipc/media_allocator.py`  
**Status**: ✅ Fixed — Full `MediaSlabAllocator` with free-list per size class, O(1) alloc/dealloc. Power-of-2 size classes (4KB, 64KB, 1MB, 16MB).  
**Vấn đề**: Docstring nói O(1) allocation nhưng code scan bitmap O(N).

**Fix**: Dùng free-list (linked list) thay bitmap scan:
```python
class SlabPool:
    def __init__(self, slab_size, count):
        self.free_list = list(range(count))  # Stack of free slots
    
    def allocate(self) -> int:
        if not self.free_list:
            raise MemoryError("Pool exhausted")
        return self.free_list.pop()  # O(1)
    
    def deallocate(self, slot: int):
        self.free_list.append(slot)  # O(1)
```

---

### Bug 1.7 — connection.rs Non-crypto PRNG for PG Secret Key `[IMPLEMENTED]`

**File**: `qm_engine/src/gateway/connection.rs`  
**Status**: ✅ Fixed — Uses `rand::rngs::OsRng.next_u32()` for cryptographically secure session secret keys.  
**Vấn đề**: Dùng non-crypto random cho `secret_key` trong PG wire protocol — security risk.

**Fix**:
```rust
use rand::rngs::OsRng;
use rand::RngCore;

let secret_key = OsRng.next_u32(); // Crypto-secure
```

---

### Bug 1.8 — native_sql.rs Unnecessary `rows_out.clone()` `[IMPLEMENTED]`

**File**: `qm_engine/src/gateway/native_sql.rs`  
**Status**: ✅ Fixed — All 4 `rows.clone()`/`out_rows.clone()` sites replaced with move semantics (`let n = rows.len(); rows: rows`).  
**Vấn đề**: `rows_out.clone()` copy toàn bộ result set không cần thiết.

**Fix**: Dùng `std::mem::take()` hoặc move semantics:
```rust
// Thay vì:
let result = rows_out.clone();
// Dùng:
let result = std::mem::take(&mut rows_out);
```

---

## 2. PHASE 1 — Memory Safety & IPC Ring Buffer → Rust `[IMPLEMENTED]`

### 2.1 Hiện trạng

**File hiện tại**: `qm_core/.../ring_buffer.py`

- Ring buffer dùng `mmap` + shared memory giữa Hub ↔ Satellite
- Không có true atomic operations — dựa vào GIL + OS byte atomicity
- `bytes(payload)` copy trong `publish()` — không zero-copy
- **KHÔNG** có crash recovery: nếu Satellite chết giữa lúc write → corrupt data
- Sequence number là Python int — không lock-free

### 2.2 Thiết kế mới: Rust LMAX Disruptor

**Crate**: Tự implement dựa trên LMAX Disruptor pattern

#### Data Structure

```rust
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use memmap2::MmapMut;

/// Layout trong shared memory:
/// [Header: 4096 bytes][Slot 0][Slot 1]...[Slot N-1]
/// 
/// Header layout:
///   bytes 0..8:   magic (0x514D5649_50435200 = "QMVIPCR\0")
///   bytes 8..16:  version (u64)
///   bytes 16..24: slot_count (u64, must be power of 2)
///   bytes 24..32: slot_size (u64)
///   bytes 32..40: writer_sequence (AtomicU64) — monotonic tăng
///   bytes 40..48: reader_sequence (AtomicU64) — consumer đã xử lý đến đây
///   bytes 48..56: writer_pid (AtomicU64) — PID của writer hiện tại
///   bytes 56..64: epoch (AtomicU64) — tăng mỗi khi recover
///
/// Slot layout (mỗi slot):
///   bytes 0..1:   state (AtomicU8: 0=FREE, 1=WRITING, 2=COMMITTED, 3=PROCESSING)
///   bytes 1..5:   checksum (CRC32)
///   bytes 5..9:   payload_len (u32)
///   bytes 9..slot_size: payload data

const SLOT_FREE: u8 = 0;
const SLOT_WRITING: u8 = 1;
const SLOT_COMMITTED: u8 = 2;
const SLOT_PROCESSING: u8 = 3;

#[repr(C)]
pub struct RingBufferHeader {
    magic: u64,
    version: u64,
    slot_count: u64,
    slot_size: u64,
    writer_sequence: AtomicU64,
    reader_sequence: AtomicU64,
    writer_pid: AtomicU64,
    epoch: AtomicU64,
}

pub struct SharedRingBuffer {
    mmap: MmapMut,
    slot_count: u64,
    slot_size: u64,
}
```

#### Thuật toán Publish (Writer)

```rust
impl SharedRingBuffer {
    /// Lock-free publish. Trả về sequence number.
    pub fn publish(&self, payload: &[u8]) -> Result<u64, RingError> {
        assert!(payload.len() <= self.max_payload_size());
        
        loop {
            // 1. Claim slot bằng atomic fetch_add
            let seq = self.header().writer_sequence
                .fetch_add(1, Ordering::AcqRel);
            let slot_idx = seq & (self.slot_count - 1); // Power-of-2 mask
            
            // 2. Kiểm tra slot có FREE không (back-pressure)
            let slot = self.slot_mut(slot_idx);
            let state = slot.state.load(Ordering::Acquire);
            if state != SLOT_FREE {
                // Ring đầy — back-pressure
                // Undo claim (best-effort, không ảnh hưởng correctness)
                return Err(RingError::Full);
            }
            
            // 3. Transition: FREE → WRITING
            match slot.state.compare_exchange(
                SLOT_FREE, SLOT_WRITING,
                Ordering::AcqRel, Ordering::Relaxed
            ) {
                Ok(_) => {},
                Err(_) => continue, // Race condition, retry
            }
            
            // 4. Copy payload + compute checksum
            let payload_area = self.payload_area_mut(slot_idx);
            payload_area[..payload.len()].copy_from_slice(payload);
            slot.payload_len = payload.len() as u32;
            slot.checksum = crc32fast::hash(payload);
            
            // 5. Memory fence + transition: WRITING → COMMITTED
            std::sync::atomic::fence(Ordering::Release);
            slot.state.store(SLOT_COMMITTED, Ordering::Release);
            
            return Ok(seq);
        }
    }
}
```

#### Thuật toán Consume (Reader)

```rust
impl SharedRingBuffer {
    /// Blocking consume — dùng spin + yield
    pub fn consume(&self) -> Result<(u64, Vec<u8>), RingError> {
        let seq = self.header().reader_sequence
            .fetch_add(1, Ordering::AcqRel);
        let slot_idx = seq & (self.slot_count - 1);
        let slot = self.slot(slot_idx);
        
        // Spin-wait cho COMMITTED state
        let mut spin_count = 0;
        loop {
            let state = slot.state.load(Ordering::Acquire);
            if state == SLOT_COMMITTED {
                break;
            }
            spin_count += 1;
            if spin_count < 100 {
                std::hint::spin_loop();
            } else if spin_count < 1000 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(std::time::Duration::from_micros(10));
            }
        }
        
        // Transition: COMMITTED → PROCESSING
        slot.state.store(SLOT_PROCESSING, Ordering::Release);
        
        // Verify checksum
        let payload = &self.payload_area(slot_idx)[..slot.payload_len as usize];
        let checksum = crc32fast::hash(payload);
        if checksum != slot.checksum {
            // Corrupt data — epoch recovery
            return Err(RingError::ChecksumMismatch { seq, expected: slot.checksum, got: checksum });
        }
        
        let data = payload.to_vec();
        
        // Transition: PROCESSING → FREE
        slot.state.store(SLOT_FREE, Ordering::Release);
        
        Ok((seq, data))
    }
}
```

#### Crash Recovery

```rust
impl SharedRingBuffer {
    /// Gọi khi detect satellite crash (qua PID heartbeat)
    pub fn recover_after_crash(&self) {
        let new_epoch = self.header().epoch.fetch_add(1, Ordering::AcqRel) + 1;
        
        // Scan tất cả slots
        for i in 0..self.slot_count {
            let slot = self.slot_mut(i);
            let state = slot.state.load(Ordering::Acquire);
            match state {
                SLOT_WRITING => {
                    // Writer died mid-write → discard, mark FREE
                    slot.state.store(SLOT_FREE, Ordering::Release);
                    log::warn!("Epoch {}: Discarded incomplete write at slot {}", new_epoch, i);
                }
                SLOT_PROCESSING => {
                    // Reader died mid-process → re-queue as COMMITTED
                    slot.state.store(SLOT_COMMITTED, Ordering::Release);
                    log::warn!("Epoch {}: Re-queued unprocessed slot {}", new_epoch, i);
                }
                _ => {} // FREE và COMMITTED giữ nguyên
            }
        }
    }
}
```

#### PyO3 Binding

```rust
#[pyclass]
pub struct PyRingBuffer {
    inner: SharedRingBuffer,
}

#[pymethods]
impl PyRingBuffer {
    #[new]
    fn new(name: &str, slot_count: u64, slot_size: u64) -> PyResult<Self> { ... }
    fn publish(&self, payload: &[u8]) -> PyResult<u64> { ... }
    fn consume(&self) -> PyResult<(u64, Vec<u8>)> { ... }
    fn recover(&self) -> PyResult<()> { ... }
}
```

### 2.3 Testing: `shuttle` cho concurrency

```toml
[dev-dependencies]
shuttle = "0.7"
```

```rust
#[cfg(test)]
mod tests {
    use shuttle::sync::atomic::{AtomicU64, AtomicU8};
    use shuttle::{thread, check_random};
    
    #[test]
    fn test_concurrent_publish_consume() {
        check_random(|| {
            let ring = SharedRingBuffer::new_test(64, 4096);
            let ring = std::sync::Arc::new(ring);
            
            let r1 = ring.clone();
            let writer = thread::spawn(move || {
                for i in 0..10 {
                    r1.publish(&i.to_le_bytes()).unwrap();
                }
            });
            
            let r2 = ring.clone();
            let reader = thread::spawn(move || {
                for _ in 0..10 {
                    let (_, data) = r2.consume().unwrap();
                    assert_eq!(data.len(), 8);
                }
            });
            
            writer.join().unwrap();
            reader.join().unwrap();
        }, 1000); // 1000 random schedules
    }
}
```

### 2.4 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/ipc/ring_buffer.rs` | ✅ Rust implementation (~550 lines, LMAX Disruptor, 8 tests pass) |
| `qm_engine/src/ipc/lsn.rs` | ✅ Lock-free LSN Sequencer (4 tests pass) |
| `qm_engine/src/lib.rs` | ✅ `PyRingBuffer` registered in PyO3 module |

---

## 3. PHASE 2 — Hub Coordinator → Rust + LSN Sequencer `[IMPLEMENTED]`

> **LSN Sequencer**: ✅ Implemented in `qm_engine/src/ipc/lsn.rs` with 4 passing tests.  
> **Hub Coordinator**: Existing Rust `HubEngine` at `qm_engine/src/hub_engine/`. Full migration pending Phase 1 completion.

### 3.1 Hiện trạng

**File hiện tại**: `qm_core/.../hub.py`

- Hub chạy single-threaded Python — bottleneck trên sequence ordering
- WAL là simple file append — không có structured log
- LSN (Log Sequence Number) gán bằng Python `+= 1` — không atomic
- Không có crash recovery cho inflight operations
- 3 ring buffers chia sẻ 1 LSN sequencer qua trực tiếp attribute assignment

### 3.2 Thiết kế mới: Rust Hub Engine

#### LSN Sequencer (Lock-free)

```rust
use std::sync::atomic::{AtomicU64, Ordering};

pub struct LsnSequencer {
    current: AtomicU64,
    /// Persisted LSN — đã flush to disk
    persisted: AtomicU64,
}

impl LsnSequencer {
    pub fn new(start: u64) -> Self {
        Self {
            current: AtomicU64::new(start),
            persisted: AtomicU64::new(start),
        }
    }
    
    /// Allocate 1 LSN — lock-free, monotonic
    #[inline]
    pub fn next(&self) -> u64 {
        self.current.fetch_add(1, Ordering::AcqRel)
    }
    
    /// Allocate N LSN liên tiếp — cho batch operations
    #[inline]
    pub fn next_batch(&self, count: u64) -> std::ops::Range<u64> {
        let start = self.current.fetch_add(count, Ordering::AcqRel);
        start..start + count
    }
    
    /// Mark LSN đã persisted (gọi sau WAL flush)
    pub fn mark_persisted(&self, lsn: u64) {
        self.persisted.fetch_max(lsn, Ordering::Release);
    }
    
    pub fn persisted_lsn(&self) -> u64 {
        self.persisted.load(Ordering::Acquire)
    }
}
```

#### Hub Coordinator (Rust)

```rust
pub struct RustHubCoordinator {
    lsn: LsnSequencer,
    rings: Vec<SharedRingBuffer>,  // 1 per satellite type
    wal: WalWriter,
    catalog: DashMap<String, TableMeta>,
    
    // Satellite registry
    satellites: DashMap<u32, SatelliteInfo>, // pid → info
}

impl RustHubCoordinator {
    /// Route request từ ring buffer → satellite
    pub fn dispatch(&self, request: &[u8]) -> Result<(), HubError> {
        let lsn = self.lsn.next();
        
        // 1. WAL write (TRƯỚC khi dispatch)
        self.wal.append(lsn, request)?;
        
        // 2. Route to đúng satellite
        let msg: RequestMsg = rmp_serde::from_slice(request)?;
        let target_ring = self.select_ring(&msg);
        target_ring.publish(request)?;
        
        Ok(())
    }
    
    /// Crash recovery: replay WAL từ last persisted LSN
    pub fn recover(&self) -> Result<u64, HubError> {
        let start_lsn = self.lsn.persisted_lsn();
        let entries = self.wal.read_from(start_lsn)?;
        
        let mut recovered = 0;
        for entry in entries {
            // Re-dispatch chỉ những entry chưa được ACK
            if !self.is_acknowledged(entry.lsn) {
                self.dispatch_internal(entry.lsn, &entry.data)?;
                recovered += 1;
            }
        }
        
        log::info!("Recovered {} inflight operations from LSN {}", recovered, start_lsn);
        Ok(recovered)
    }
}
```

### 3.3 Migration Path

1. Implement `RustHubCoordinator` trong `qm_engine/src/hub.rs`
2. Expose qua PyO3 `#[pyclass] PyHubCoordinator`
3. Trong `qm_core/.../hub.py`: thêm flag `USE_RUST_HUB = True`
4. Fallback sang Python Hub nếu Rust module không load được
5. Test song song 2 implementation bằng same workload → so sánh throughput

### 3.4 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/hub.rs` | Rust Hub Coordinator |
| `qm_engine/src/lsn.rs` | Lock-free LSN Sequencer |
| `qm_engine/src/hub_test.rs` | Tests |
| `qm_core/.../hub.py` (sửa) | Thêm Rust bridge |

---

## 4. PHASE 3 — WAL + io_uring `[IMPLEMENTED]`

### 4.1 Hiện trạng

**File hiện tại**: `qm_engine/src/wal.rs`

- WAL dùng `std::fs::File` + `write_all` + `sync_all` — blocking I/O
- `flush_buffer()` re-decode records không cần thiết (Bug 1.5)
- `WalWriter` không `Send+Sync` — không thể share across threads
- Không có group commit — mỗi write 1 syscall `fsync`

### 4.2 Thiết kế mới: io_uring WAL

**Crate**: `tokio-uring` (Linux) + fallback `std::fs` (macOS)

#### Thuật toán Group Commit

```
                    ┌──────────────┐
   Writer 1 ──────►│              │
   Writer 2 ──────►│  Commit Queue│──── io_uring submit ──── NVMe
   Writer 3 ──────►│  (batch)     │          │
                    └──────────────┘          │
                           ▲                  │
                           │                  ▼
                    ┌──────────────┐    ┌──────────┐
                    │  Notify all  │◄───│ CQE done │
                    │  waiters     │    └──────────┘
                    └──────────────┘
```

```rust
use std::collections::VecDeque;
use tokio::sync::Notify;

pub struct IoUringWal {
    ring: io_uring::IoUring,
    write_buffer: Vec<u8>,           // Accumulate writes
    pending_commits: VecDeque<PendingCommit>,
    
    // Group commit params
    max_batch_size: usize,           // Default: 64KB
    max_batch_wait_us: u64,          // Default: 200μs (micro-batch)
}

struct PendingCommit {
    lsn: u64,
    offset: usize,
    len: usize,
    notify: Arc<Notify>,
}

impl IoUringWal {
    /// Append to WAL — non-blocking, returns khi batch flush xong
    pub async fn append(&mut self, lsn: u64, data: &[u8]) -> Result<(), WalError> {
        let offset = self.write_buffer.len();
        
        // 1. Serialize WAL record
        let record = WalRecord {
            lsn,
            len: data.len() as u32,
            checksum: crc32fast::hash(data),
            data: data, // Borrow, không copy
        };
        record.serialize_into(&mut self.write_buffer);
        
        // 2. Register pending commit
        let notify = Arc::new(Notify::new());
        self.pending_commits.push_back(PendingCommit {
            lsn,
            offset,
            len: self.write_buffer.len() - offset,
            notify: notify.clone(),
        });
        
        // 3. Check batch threshold
        if self.write_buffer.len() >= self.max_batch_size {
            self.flush_batch().await?;
        } else {
            // Micro-batch: chờ tối đa max_batch_wait_us
            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_micros(self.max_batch_wait_us)) => {
                    self.flush_batch().await?;
                }
                // Hoặc đợi thêm writers (sẽ trigger batch khi đạt threshold)
            }
        }
        
        // 4. Wait cho commit confirmation
        notify.notified().await;
        Ok(())
    }
    
    /// Submit batch qua io_uring
    async fn flush_batch(&mut self) -> Result<(), WalError> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }
        
        let buf = std::mem::take(&mut self.write_buffer);
        let pending = std::mem::take(&mut self.pending_commits);
        
        // io_uring: write + fsync as linked SQEs
        unsafe {
            let write_e = io_uring::opcode::Write::new(
                io_uring::types::Fd(self.fd),
                buf.as_ptr(),
                buf.len() as u32,
            ).offset(self.file_offset as u64)
            .build()
            .flags(io_uring::squeue::Flags::IO_LINK);
            
            let fsync_e = io_uring::opcode::Fsync::new(
                io_uring::types::Fd(self.fd),
            ).build();
            
            self.ring.submission().push(&write_e)?;
            self.ring.submission().push(&fsync_e)?;
        }
        
        self.ring.submit_and_wait(2)?;
        self.file_offset += buf.len();
        
        // Notify tất cả pending writers
        for commit in &pending {
            commit.notify.notify_one();
        }
        
        Ok(())
    }
}
```

#### SQPOLL Mode (Expert Recommendation)

> **CRITICAL**: Trên Linux, bật `IORING_SETUP_SQPOLL` — kernel thread tự poll submission queue
> mà **không cần Hub gọi `enter()` syscall**. Giảm latency xuống sub-microsecond.

```rust
impl IoUringWal {
    pub fn new_with_sqpoll(path: &Path) -> Result<Self, WalError> {
        let ring = io_uring::IoUring::builder()
            .setup_sqpoll(2000)          // kernel thread idle timeout: 2ms
            .setup_sqpoll_cpu(0)         // pin to CPU 0
            .build(256)?;                // 256 SQE slots
        
        // Khi SQPOLL active, submissions tự động được poll bởi kernel thread
        // Hub chỉ cần memory-fence, KHÔNG syscall
        Self::from_ring(ring, path)
    }
}
```

#### Platform Abstraction

```rust
pub enum WalBackend {
    #[cfg(target_os = "linux")]
    IoUring(IoUringWal),
    
    Standard(StdWal), // macOS, Windows fallback
}

impl WalBackend {
    pub fn new(path: &Path) -> Self {
        #[cfg(target_os = "linux")]
        {
            match IoUringWal::new_with_sqpoll(path) {
                Ok(w) => return WalBackend::IoUring(w),
                Err(e) => {
                    log::warn!("io_uring SQPOLL unavailable: {}, trying standard io_uring", e);
                    match IoUringWal::new(path) {
                        Ok(w) => return WalBackend::IoUring(w),
                        Err(e) => log::warn!("io_uring unavailable: {}, falling back to std", e),
                    }
                }
            }
        }
        WalBackend::Standard(StdWal::new(path))
    }
}
```

### 4.3 Benchmark Target

| Metric | Hiện tại (std::fs) | Target (io_uring) |
|--------|--------------------|--------------------|
| Single write latency | ~100μs | ~20μs |
| Group commit throughput | ~50K ops/s | ~500K ops/s |
| fsync per batch | 1 per write | 1 per batch (amortized) |

### 4.4 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/wal_uring.rs` | io_uring WAL implementation |
| `qm_engine/src/wal_std.rs` | Refactored std WAL (fix Bug 1.5) |
| `qm_engine/src/wal.rs` (sửa) | Platform abstraction layer |
| `Cargo.toml` (sửa) | Thêm `io-uring` crate (Linux-only feature) |

---

## 5. PHASE 4 — Zero-Copy Data Path (Gateway → Disk) `[IMPLEMENTED]`

### 5.1 Hiện trạng — Các điểm copy không cần thiết

Audit phát hiện **6 điểm copy** trong hot path:

| # | Vị trí | Copy | Mô tả |
|---|--------|------|--------|
| 1 | `ring_buffer.py:publish()` | `bytes(payload)` | Copy input bytes |
| 2 | `native_sql.rs:execute_*()` | `rows_out.clone()` | Clone toàn bộ result set |
| 3 | `native_sql.rs:hash_join()` | `String` clone trong probe | Clone key strings |
| 4 | `connection.rs` | `sql.to_string()` | Clone SQL string mỗi query |
| 5 | `connection.rs` | `self.user.clone()` | Clone user string mỗi query |
| 6 | `hnsw.rs` | `self.graph.clone()` | Clone TOÀN BỘ graph (Bug 1.1) |

### 5.2 Giải pháp từng điểm

#### Copy #1: Ring Buffer — Đã giải quyết ở Phase 1
Rust ring buffer dùng `copy_from_slice` trực tiếp vào shared memory — 1 copy thay vì 2.

#### Copy #2 & #3: native_sql.rs

```rust
// THAY VÌ:
let result = rows_out.clone();
return Ok(result);

// DÙNG:
return Ok(std::mem::take(&mut rows_out));
// Hoặc tốt hơn: trả về iterator/streaming

// Hash join: dùng &str reference thay String clone
// Chuyển HashMap<String, Vec<Row>> → HashMap<&str, Vec<RowRef>>
// Cần lifetime tracking
```

#### Copy #4 & #5: connection.rs

```rust
// THAY VÌ:
let sql_string = sql.to_string();
let user = self.user.clone();
self.engine.execute(sql_string, user);

// DÙNG Arc<str> cho user (clone-free sharing):
pub struct Connection {
    user: Arc<str>,  // Thay vì String
    // ...
}

// Dùng &str cho SQL (zero-copy borrow):
self.engine.execute(sql, &self.user);
```

#### Copy #6: HNSW — Đã giải quyết ở Bug 1.1

### 5.3 Advanced: Arrow-based Columnar Format

Dài hạn, chuyển internal data format sang Apache Arrow:

```rust
// Cargo.toml
arrow = "53"

// Thay vì Vec<Row> dùng RecordBatch
use arrow::record_batch::RecordBatch;
use arrow::array::{StringArray, Int64Array, Float32Array};

// Zero-copy qua IPC:
// Writer: Arrow IPC serialize → shared memory
// Reader: Arrow IPC zero-copy read từ shared memory (mmap)
```

**Lợi ích**:
- Zero-copy IPC giữa Rust ↔ Python (PyArrow)
- SIMD-friendly columnar layout
- Tương thích DataFusion, Polars, DuckDB ecosystem

### 5.4 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/native_sql.rs` (sửa) | Loại bỏ clone() |
| `qm_engine/src/connection.rs` (sửa) | Arc<str> + borrow |
| `qm_engine/src/types.rs` (mới) | Arrow integration types |

---

## 6. PHASE 5 — HNSW Filter-Pushdown `[IMPLEMENTED]`

### 6.1 Hiện trạng

**File**: `qm_native/src/hnsw.rs`

- Search trả về top-K nearest neighbors **KHÔNG có filter**
- Nếu muốn filter, phải: search(K*10) → filter → lấy K → KẾT QUẢ SAI (recall giảm)
- Không có metadata storage trong HNSW node

### 6.2 Thuật toán: Filtered HNSW Search

Dựa trên paper "Filtered-DiskANN" + Vamana approach:

```rust
/// HNSW node có thêm metadata
pub struct HnswNode {
    pub id: u64,
    pub vector: Vec<f32>,
    pub level: usize,
    pub connections: Vec<Vec<u64>>,
    pub metadata: HashMap<String, MetaValue>,  // ← MỚI
}

#[derive(Clone)]
pub enum MetaValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    IntList(Vec<i64>),
}

/// Filter predicate
pub enum FilterPredicate {
    Eq(String, MetaValue),
    Range(String, MetaValue, MetaValue),  // field, min, max
    In(String, Vec<MetaValue>),
    And(Vec<FilterPredicate>),
    Or(Vec<FilterPredicate>),
}

impl FilterPredicate {
    /// Kiểm tra node có match filter không — inline hot function
    #[inline]
    fn matches(&self, meta: &HashMap<String, MetaValue>) -> bool {
        match self {
            FilterPredicate::Eq(field, value) => {
                meta.get(field).map_or(false, |v| v == value)
            }
            FilterPredicate::Range(field, min, max) => {
                meta.get(field).map_or(false, |v| v >= min && v <= max)
            }
            FilterPredicate::In(field, values) => {
                meta.get(field).map_or(false, |v| values.contains(v))
            }
            FilterPredicate::And(preds) => preds.iter().all(|p| p.matches(meta)),
            FilterPredicate::Or(preds) => preds.iter().any(|p| p.matches(meta)),
        }
    }
}
```

#### Thuật toán Search với Filter-Pushdown

```rust
impl HnswIndex {
    /// Filtered search: filter TRONG LÚC duyệt graph, không phải sau
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: Option<&FilterPredicate>,
    ) -> Vec<(u64, f32)> {
        let entry_point = match self.graph.entry_point {
            Some(ep) => ep,
            None => return vec![],
        };
        
        // Phase 1: Greedy search từ top level → level 1 (KHÔNG filter)
        let mut current = entry_point;
        for level in (1..=self.graph.max_level).rev() {
            current = self.greedy_search_layer(query, current, level);
        }
        
        // Phase 2: Search level 0 VỚI filter
        // Modified beam search: mở rộng ef nếu filter loại bỏ nhiều candidates
        let ef_filtered = if filter.is_some() {
            ef * 3  // Over-fetch 3x để bù cho filtered-out nodes
        } else {
            ef
        };
        
        let mut visited: HashSet<u64> = HashSet::with_capacity(ef_filtered * 2);
        let mut candidates: BinaryHeap<Reverse<(OrderedFloat<f32>, u64)>> = BinaryHeap::new();
        let mut results: BinaryHeap<(OrderedFloat<f32>, u64)> = BinaryHeap::new();
        
        // Seed
        let dist = self.distance(query, &self.graph.nodes[&current].vector);
        candidates.push(Reverse((OrderedFloat(dist), current)));
        visited.insert(current);
        
        // Check filter cho seed
        if self.passes_filter(current, filter) {
            results.push((OrderedFloat(dist), current));
        }
        
        while let Some(Reverse((OrderedFloat(c_dist), c_id))) = candidates.pop() {
            // Early termination: nếu candidate xa hơn worst result
            if results.len() >= k {
                let worst_dist = results.peek().unwrap().0.into_inner();
                if c_dist > worst_dist {
                    break;
                }
            }
            
            // Expand neighbors
            let node = &self.graph.nodes[&c_id];
            for &neighbor_id in &node.connections[0] {
                if visited.contains(&neighbor_id) {
                    continue;
                }
                visited.insert(neighbor_id);
                
                let neighbor = &self.graph.nodes[&neighbor_id];
                let n_dist = self.distance(query, &neighbor.vector);
                
                // FILTER-PUSHDOWN: check filter TRƯỚC khi add to results
                let should_add = results.len() < k || 
                    n_dist < results.peek().unwrap().0.into_inner();
                
                if should_add {
                    candidates.push(Reverse((OrderedFloat(n_dist), neighbor_id)));
                    
                    if self.passes_filter(neighbor_id, filter) {
                        results.push((OrderedFloat(n_dist), neighbor_id));
                        if results.len() > k {
                            results.pop(); // Remove farthest
                        }
                    }
                }
            }
        }
        
        // Collect kết quả, sort by distance
        let mut output: Vec<(u64, f32)> = results
            .into_iter()
            .map(|(d, id)| (id, d.into_inner()))
            .collect();
        output.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        output
    }
    
    #[inline]
    fn passes_filter(&self, id: u64, filter: Option<&FilterPredicate>) -> bool {
        match filter {
            None => true,
            Some(f) => f.matches(&self.graph.nodes[&id].metadata),
        }
    }
}
```

#### Bitmap Pre-filter Optimization

Cho trường hợp filter có selectivity rất thấp (< 1%):

```rust
/// Pre-compute bitmap cho common filters
pub struct FilterBitmap {
    bits: Vec<u64>,  // Bit-packed
    count: usize,
}

impl FilterBitmap {
    pub fn from_predicate(nodes: &HashMap<u64, HnswNode>, pred: &FilterPredicate) -> Self {
        let max_id = *nodes.keys().max().unwrap_or(&0) as usize;
        let words = (max_id + 64) / 64;
        let mut bits = vec![0u64; words];
        let mut count = 0;
        
        for (&id, node) in nodes {
            if pred.matches(&node.metadata) {
                bits[id as usize / 64] |= 1u64 << (id as usize % 64);
                count += 1;
            }
        }
        
        FilterBitmap { bits, count }
    }
    
    #[inline]
    pub fn contains(&self, id: u64) -> bool {
        let idx = id as usize;
        (self.bits[idx / 64] >> (idx % 64)) & 1 == 1
    }
    
    pub fn selectivity(&self, total: usize) -> f64 {
        self.count as f64 / total as f64
    }
}

impl HnswIndex {
    pub fn search_with_strategy(
        &self,
        query: &[f32],
        k: usize,
        filter: &FilterPredicate,
    ) -> Vec<(u64, f32)> {
        let bitmap = FilterBitmap::from_predicate(&self.graph.nodes, filter);
        let selectivity = bitmap.selectivity(self.graph.nodes.len());
        
        if selectivity < 0.01 {
            // <1% match: brute-force scan chỉ filtered nodes (nhanh hơn)
            self.brute_force_filtered(query, k, &bitmap)
        } else if selectivity < 0.1 {
            // 1-10%: HNSW search với ef*10 over-fetch
            self.search_filtered(query, k, self.ef_search * 10, Some(filter))
        } else {
            // >10%: Normal filtered HNSW
            self.search_filtered(query, k, self.ef_search * 3, Some(filter))
        }
    }
}
```

### 6.3 PyO3 API

```rust
#[pymethods]
impl PyHnswIndex {
    /// Python API: search(query, k, filter=None)
    /// filter format: {"field": "category", "op": "eq", "value": "electronics"}
    fn search(
        &self, 
        py: Python,
        query: PyReadonlyArray1<f32>,
        k: usize,
        filter: Option<&PyDict>,
    ) -> PyResult<(Py<PyArray1<u64>>, Py<PyArray1<f32>>)> {
        let filter_pred = match filter {
            Some(dict) => Some(FilterPredicate::from_pydict(dict)?),
            None => None,
        };
        
        let results = self.inner.search_filtered(
            query.as_slice()?,
            k,
            self.ef_search,
            filter_pred.as_ref(),
        );
        
        // Return (ids, distances) as numpy arrays
        let ids: Vec<u64> = results.iter().map(|(id, _)| *id).collect();
        let dists: Vec<f32> = results.iter().map(|(_, d)| *d).collect();
        
        Ok((
            PyArray1::from_vec(py, ids).into(),
            PyArray1::from_vec(py, dists).into(),
        ))
    }
}
```

### 6.4 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_native/src/hnsw.rs` (sửa) | ✅ Filter-pushdown search + metadata (MetaValue, FilterPredicate, FilterBitmap, search_filtered, add_with_metadata, brute_force_filtered — all in-file) |
| ~~`qm_native/src/filter.rs`~~ | Merged into hnsw.rs (single-file for simplicity) |

---

## 7. PHASE 6 — GPU (wgpu) Vector Operations `[IMPLEMENTED]`

### 7.1 Hiện trạng

- Tất cả distance computation dùng CPU SIMD (`wide` crate)
- HNSW search duyệt tuần tự neighbors — không parallel distance compute
- Không có GPU code nào trong codebase

### 7.2 Thiết kế: wgpu Compute Shader

**Crate**: `wgpu` (cross-platform: Metal trên macOS, Vulkan trên Linux, DX12 trên Windows)

#### Architecture

```
                  ┌─────────────────┐
                  │   qm_native     │
                  │                 │
  Python ──PyO3──►│  distance_gpu() │
                  │       │         │
                  │       ▼         │
                  │  ┌──────────┐   │
                  │  │ wgpu API │   │
                  │  └────┬─────┘   │
                  └───────┼─────────┘
                          │
              ┌───────────┼───────────┐
              ▼           ▼           ▼
          ┌───────┐  ┌────────┐  ┌────────┐
          │ Metal │  │ Vulkan │  │ DX12   │
          │(macOS)│  │(Linux) │  │(Win)   │
          └───────┘  └────────┘  └────────┘
```

#### WGSL Compute Shader: Batch Cosine Distance

```wgsl
// cosine_distance.wgsl
@group(0) @binding(0) var<storage, read> query: array<f32>;
@group(0) @binding(1) var<storage, read> vectors: array<f32>;
@group(0) @binding(2) var<storage, read_write> distances: array<f32>;
@group(0) @binding(3) var<uniform> params: Params;

struct Params {
    dim: u32,
    count: u32,
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    if (idx >= params.count) {
        return;
    }
    
    let base = idx * params.dim;
    var dot_product: f32 = 0.0;
    var norm_q: f32 = 0.0;
    var norm_v: f32 = 0.0;
    
    // Vectorized loop
    for (var i: u32 = 0u; i < params.dim; i = i + 1u) {
        let q = query[i];
        let v = vectors[base + i];
        dot_product += q * v;
        norm_q += q * q;
        norm_v += v * v;
    }
    
    let denom = sqrt(norm_q) * sqrt(norm_v);
    if (denom > 0.0) {
        distances[idx] = 1.0 - (dot_product / denom);
    } else {
        distances[idx] = 1.0;
    }
}
```

#### Rust Host Code

```rust
use wgpu::util::DeviceExt;

pub struct GpuDistanceEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl GpuDistanceEngine {
    pub async fn new() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::default();
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }).await.ok_or(GpuError::NoAdapter)?;
        
        let (device, queue) = adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("qmvir-gpu"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ).await?;
        
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cosine_distance"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/cosine_distance.wgsl").into()),
        });
        
        // ... setup pipeline và bind group layout
        Ok(Self { device, queue, pipeline, bind_group_layout })
    }
    
    /// Compute distances from query to N vectors on GPU
    /// Trả về Vec<f32> distances, sorted hoặc unsorted
    pub async fn batch_distance(
        &self,
        query: &[f32],       // [dim]
        vectors: &[f32],     // [N * dim], row-major
        dim: usize,
    ) -> Vec<f32> {
        let count = vectors.len() / dim;
        
        // Upload buffers to GPU
        let query_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("query"),
            contents: bytemuck::cast_slice(query),
            usage: wgpu::BufferUsages::STORAGE,
        });
        
        let vectors_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vectors"),
            contents: bytemuck::cast_slice(vectors),
            usage: wgpu::BufferUsages::STORAGE,
        });
        
        let distances_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("distances"),
            size: (count * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        
        let readback_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (count * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        
        // Dispatch compute
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            // ... set bind groups
            pass.dispatch_workgroups(((count + 255) / 256) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&distances_buf, 0, &readback_buf, 0, (count * 4) as u64);
        self.queue.submit(Some(encoder.finish()));
        
        // Read back results
        let slice = readback_buf.slice(..);
        let (tx, rx) = tokio::sync::oneshot::channel();
        slice.map_async(wgpu::MapMode::Read, move |_| { let _ = tx.send(()); });
        self.device.poll(wgpu::Maintain::Wait);
        rx.await.unwrap();
        
        let data = slice.get_mapped_range();
        let distances: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        readback_buf.unmap();
        
        distances
    }
}
```

#### Pinned Memory (Expert Recommendation)

> **CRITICAL**: PCIe bottleneck là rất lớn. Dùng **Pinned Memory** (mapped host memory)
> để GPU đọc trực tiếp từ RAM mà không cần DMA copy.

```rust
impl GpuDistanceEngine {
    /// Pre-allocate pinned buffer cho vectors — reuse across queries
    pub fn create_pinned_vector_buffer(&self, max_vectors: usize, dim: usize) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pinned_vectors"),
            size: (max_vectors * dim * 4) as u64,
            // MAP_WRITE cho phép CPU ghi trực tiếp, GPU đọc không copy
            usage: wgpu::BufferUsages::STORAGE 
                 | wgpu::BufferUsages::MAP_WRITE 
                 | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
    
    /// Zero-copy upload qua mapped buffer
    pub async fn upload_vectors_pinned(&self, buffer: &wgpu::Buffer, vectors: &[f32]) {
        let slice = buffer.slice(..((vectors.len() * 4) as u64));
        let (tx, rx) = tokio::sync::oneshot::channel();
        slice.map_async(wgpu::MapMode::Write, move |_| { let _ = tx.send(()); });
        self.device.poll(wgpu::Maintain::Wait);
        rx.await.unwrap();
        
        let mut view = slice.get_mapped_range_mut();
        view.copy_from_slice(bytemuck::cast_slice(vectors));
        drop(view);
        buffer.unmap();
    }
}
```

#### Decision Logic: GPU vs CPU

```rust
/// Chọn GPU khi batch size đủ lớn để bù PCIe transfer overhead
pub fn compute_distances(
    query: &[f32],
    vectors: &[f32],
    dim: usize,
    gpu: Option<&GpuDistanceEngine>,
) -> Vec<f32> {
    let count = vectors.len() / dim;
    
    // Threshold: GPU chỉ nhanh hơn khi N > 10K (PCIe overhead)
    const GPU_THRESHOLD: usize = 10_000;
    
    if count > GPU_THRESHOLD && gpu.is_some() {
        // GPU path (dùng pinned memory nếu có)
        pollster::block_on(gpu.unwrap().batch_distance(query, vectors, dim))
    } else {
        // CPU SIMD path (existing code)
        cpu_batch_cosine_distance(query, vectors, dim)
    }
}
```

### 7.3 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_native/src/gpu.rs` | wgpu host code |
| `qm_native/src/shaders/cosine_distance.wgsl` | WGSL compute shader |
| `qm_native/src/shaders/l2_distance.wgsl` | L2 distance shader |
| `qm_native/src/shaders/inner_product.wgsl` | Inner product shader |
| `Cargo.toml` (sửa) | Thêm `wgpu` optional feature |

---

## 8. PHASE 7 — SQL Compatibility mở rộng `[IMPLEMENTED]`

### 8.1 Hiện trạng

- `sqlparser` crate parse chuẩn SQL (MySQL, PostgreSQL, ANSI)
- PG wire protocol hoạt động — kết nối được từ `psql`
- Chỉ hỗ trợ: SELECT, INSERT, UPDATE, DELETE, CREATE TABLE, DROP TABLE
- **Thiếu**: JOIN (ngoài hash join), subquery, EXPLAIN, CREATE INDEX, ALTER TABLE, aggregate functions (SUM, AVG, COUNT), GROUP BY, HAVING, ORDER BY phức tạp, LIMIT/OFFSET, transactions (BEGIN/COMMIT/ROLLBACK)

### 8.2 Kế hoạch triển khai theo thứ tự

#### Phase 7a — Aggregate + GROUP BY

```rust
pub enum AggFunction {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    CountDistinct,
}

pub struct AggregateExecutor {
    groups: HashMap<Vec<Value>, Vec<Accumulator>>,
}

impl AggregateExecutor {
    pub fn execute(
        &mut self, 
        rows: impl Iterator<Item = Row>,
        group_by_cols: &[usize],
        agg_exprs: &[(AggFunction, usize)], // (function, column_index)
    ) -> Vec<Row> {
        // Phase 1: Accumulate
        for row in rows {
            let group_key: Vec<Value> = group_by_cols.iter()
                .map(|&i| row.values[i].clone())
                .collect();
            
            let accums = self.groups.entry(group_key)
                .or_insert_with(|| {
                    agg_exprs.iter().map(|(func, _)| Accumulator::new(*func)).collect()
                });
            
            for (idx, (_, col)) in agg_exprs.iter().enumerate() {
                accums[idx].add(&row.values[*col]);
            }
        }
        
        // Phase 2: Finalize
        self.groups.iter().map(|(key, accums)| {
            let mut values = key.clone();
            for acc in accums {
                values.push(acc.finalize());
            }
            Row { values }
        }).collect()
    }
}
```

#### Phase 7b — JOIN (Nested Loop, Sort-Merge, Hash)

```rust
pub enum JoinStrategy {
    NestedLoop,   // Nhỏ × Nhỏ
    HashJoin,     // Đã có, cần refactor
    SortMerge,    // Cả 2 đã sorted
}

pub struct JoinExecutor;

impl JoinExecutor {
    pub fn select_strategy(left_size: usize, right_size: usize, sorted: bool) -> JoinStrategy {
        if sorted {
            JoinStrategy::SortMerge
        } else if left_size * right_size < 10_000 {
            JoinStrategy::NestedLoop
        } else {
            JoinStrategy::HashJoin
        }
    }
    
    /// Sort-Merge Join — efficient khi cả 2 side đã sorted
    pub fn sort_merge_join(
        left: &[Row], right: &[Row],
        left_key: usize, right_key: usize,
    ) -> Vec<Row> {
        let mut result = Vec::new();
        let mut i = 0;
        let mut j = 0;
        
        while i < left.len() && j < right.len() {
            match left[i].values[left_key].cmp(&right[j].values[right_key]) {
                Ordering::Less => i += 1,
                Ordering::Greater => j += 1,
                Ordering::Equal => {
                    // Emit all matching pairs
                    let key = &left[i].values[left_key];
                    let i_start = i;
                    while i < left.len() && left[i].values[left_key] == *key {
                        let j_start = j;
                        let mut jj = j_start;
                        while jj < right.len() && right[jj].values[right_key] == *key {
                            result.push(Row::merge(&left[i], &right[jj]));
                            jj += 1;
                        }
                        i += 1;
                    }
                    j = {
                        let mut jj = j;
                        while jj < right.len() && right[jj].values[right_key] == *key {
                            jj += 1;
                        }
                        jj
                    };
                }
            }
        }
        result
    }
}
```

#### Phase 7c — Transaction (BEGIN/COMMIT/ROLLBACK)

```rust
pub struct TransactionManager {
    active_txns: DashMap<u64, Transaction>,
    mvcc: MvccManager,
}

pub struct Transaction {
    id: u64,
    start_ts: u64,
    status: TxnStatus,
    write_set: Vec<WriteOp>,
    read_set: Vec<ReadOp>,
}

impl TransactionManager {
    pub fn begin(&self) -> u64 {
        let txn_id = self.next_txn_id();
        let ts = self.mvcc.current_timestamp();
        self.active_txns.insert(txn_id, Transaction {
            id: txn_id,
            start_ts: ts,
            status: TxnStatus::Active,
            write_set: Vec::new(),
            read_set: Vec::new(),
        });
        txn_id
    }
    
    pub fn commit(&self, txn_id: u64) -> Result<(), TxnError> {
        let mut txn = self.active_txns.get_mut(&txn_id)
            .ok_or(TxnError::NotFound)?;
        
        // Optimistic Concurrency Control: validate read set
        for read in &txn.read_set {
            if self.mvcc.has_conflicting_write(read, txn.start_ts) {
                txn.status = TxnStatus::Aborted;
                return Err(TxnError::SerializationFailure);
            }
        }
        
        // Apply write set
        let commit_ts = self.mvcc.next_timestamp();
        for write in &txn.write_set {
            self.mvcc.apply(write, commit_ts)?;
        }
        
        txn.status = TxnStatus::Committed;
        Ok(())
    }
    
    pub fn rollback(&self, txn_id: u64) -> Result<(), TxnError> {
        let mut txn = self.active_txns.get_mut(&txn_id)
            .ok_or(TxnError::NotFound)?;
        txn.status = TxnStatus::Aborted;
        // Write set chưa apply → không cần undo
        Ok(())
    }
}
```

### 8.3 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/agg.rs` | Aggregate functions |
| `qm_engine/src/join.rs` | JOIN strategies |
| `qm_engine/src/txn.rs` | Transaction manager |
| `qm_engine/src/native_sql.rs` (sửa) | Wire up new executors |

---

## 9. PHASE 8 — JIT Query Compilation `[IMPLEMENTED]`

### 9.1 Khái niệm

Thay vì interpret từng row qua volcano model, compile query thành native code:

```
SQL: SELECT a, b FROM t WHERE a > 10 AND b < 'z'

Volcano (hiện tại):
for each row:
    call filter.next()       ← vtable dispatch
        call scan.next()     ← vtable dispatch
            read row         ← actual work
        eval a > 10          ← interpretation overhead
        eval b < 'z'

JIT (target):
compiled_fn(page_ptr, result_buf):
    for each row in page:
        if row.a > 10 && row.b < 'z':   ← native comparison
            memcpy to result_buf         ← native memcpy
```

### 9.2 Approach: Cranelift (KHÔNG cần LLVM)

**Crate**: `cranelift` — Rust-native JIT compiler (dùng bởi Wasmtime)

**Tại sao Cranelift thay vì LLVM**:
- Pure Rust, không cần external dependency
- Compile time ~10x nhanh hơn LLVM
- Đủ tốt cho query compilation (không cần -O3 level optimization)
- Cùng ecosystem với Rust (Wasmtime, Wasmer)

```rust
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};

pub struct QueryCompiler {
    module: JITModule,
}

impl QueryCompiler {
    pub fn compile_filter(
        &mut self,
        predicates: &[Predicate],
        schema: &Schema,
    ) -> CompiledFilter {
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut func = Function::new();
        
        // Signature: fn(row_ptr: *const u8) -> bool
        let ptr_type = self.module.target_config().pointer_type();
        func.signature.params.push(AbiParam::new(ptr_type));
        func.signature.returns.push(AbiParam::new(types::I8));
        
        let mut builder = FunctionBuilder::new(&mut func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        
        let row_ptr = builder.block_params(entry)[0];
        
        // Generate code cho từng predicate
        let mut result = builder.ins().iconst(types::I8, 1); // Start: true
        
        for pred in predicates {
            let col_offset = schema.column_offset(pred.column);
            let col_type = schema.column_type(pred.column);
            
            // Load column value
            let value = match col_type {
                DataType::Int64 => {
                    builder.ins().load(types::I64, MemFlags::new(), row_ptr, col_offset as i32)
                }
                DataType::Float64 => {
                    builder.ins().load(types::F64, MemFlags::new(), row_ptr, col_offset as i32)
                }
                _ => todo!("String comparison cần special handling"),
            };
            
            // Compare
            let cmp_result = match pred.op {
                CmpOp::Gt => {
                    let constant = builder.ins().iconst(types::I64, pred.value_i64);
                    builder.ins().icmp(IntCC::SignedGreaterThan, value, constant)
                }
                CmpOp::Lt => {
                    let constant = builder.ins().iconst(types::I64, pred.value_i64);
                    builder.ins().icmp(IntCC::SignedLessThan, value, constant)
                }
                CmpOp::Eq => {
                    let constant = builder.ins().iconst(types::I64, pred.value_i64);
                    builder.ins().icmp(IntCC::Equal, value, constant)
                }
                _ => todo!(),
            };
            
            // AND với previous result
            result = builder.ins().band(result, cmp_result);
        }
        
        builder.ins().return_(&[result]);
        builder.finalize();
        
        // Compile to native code
        let func_id = self.module.declare_anonymous_function(&func.signature).unwrap();
        self.module.define_function(func_id, &mut codegen::Context::for_function(func)).unwrap();
        self.module.finalize_definitions().unwrap();
        
        let code_ptr = self.module.get_finalized_function(func_id);
        
        CompiledFilter {
            func: unsafe { std::mem::transmute::<_, fn(*const u8) -> bool>(code_ptr) },
        }
    }
}

pub struct CompiledFilter {
    func: fn(*const u8) -> bool,
}

impl CompiledFilter {
    #[inline]
    pub fn evaluate(&self, row_ptr: *const u8) -> bool {
        (self.func)(row_ptr)
    }
}
```

### 9.3 Khi nào JIT

```rust
/// Chỉ JIT khi scan > THRESHOLD rows (compile overhead ~50μs)
const JIT_ROW_THRESHOLD: usize = 10_000;

pub fn execute_scan(
    table: &Table,
    predicates: &[Predicate],
    jit: &QueryCompiler,
) -> Vec<Row> {
    if table.row_count() > JIT_ROW_THRESHOLD && predicates.is_jittable() {
        // JIT path
        let compiled = jit.compile_filter(predicates, table.schema());
        table.pages().flat_map(|page| {
            page.rows().filter(|row| compiled.evaluate(row.as_ptr()))
        }).collect()
    } else {
        // Interpreted path (hiện tại)
        table.scan_with_filter(predicates)
    }
}
```

### 9.4 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/jit.rs` | QueryCompiler + CompiledFilter |
| `qm_engine/src/jit_test.rs` | Tests |
| `Cargo.toml` (sửa) | Thêm `cranelift-*` optional feature |

---

## 10. PHASE 9 — Adaptive Indexing nâng cao `[IMPLEMENTED]`

### 10.1 Hiện trạng

**File**: `qm_engine/src/auto_manager.rs`

- Shadow indexing framework **đã tồn tại** — tạo shadow index, track performance
- **Bug**: Threshold check bị bỏ qua (Bug 1.2)
- Chỉ track latency, không track memory cost
- Không có index removal (chỉ tạo thêm)

### 10.2 Cải tiến

#### Cost-Benefit Analysis

```rust
pub struct IndexCostBenefit {
    /// Estimated memory cost in bytes
    pub memory_bytes: u64,
    /// Average query speedup ratio (>1 means faster)
    pub speedup_ratio: f64,
    /// Number of queries that would benefit
    pub benefiting_queries: u64,
    /// Total queries observed
    pub total_queries: u64,
    /// Benefit score = speedup_ratio * (benefiting_queries / total_queries) / memory_MB
    pub score: f64,
}

impl IndexCostBenefit {
    pub fn compute(shadow: &ShadowIndex, memory_budget_mb: f64) -> Self {
        let memory_bytes = shadow.estimated_size_bytes();
        let memory_mb = memory_bytes as f64 / 1_048_576.0;
        
        let speedup_ratio = if shadow.shadow_latency_ns > 0 {
            shadow.baseline_latency_ns as f64 / shadow.shadow_latency_ns as f64
        } else {
            1.0
        };
        
        let hit_ratio = shadow.benefiting_queries as f64 / shadow.total_queries.max(1) as f64;
        let score = speedup_ratio * hit_ratio / memory_mb;
        
        Self {
            memory_bytes,
            speedup_ratio,
            benefiting_queries: shadow.benefiting_queries,
            total_queries: shadow.total_queries,
            score,
        }
    }
}
```

#### Auto-Remove Unused Indexes

```rust
impl AutoIndexManager {
    /// Chạy periodic — drop index không dùng quá DROP_AFTER_IDLE
    pub fn gc_unused_indexes(&mut self) {
        const DROP_AFTER_IDLE: Duration = Duration::from_secs(3600); // 1 hour
        
        let now = Instant::now();
        let to_remove: Vec<String> = self.active_indexes.iter()
            .filter(|(_, stats)| {
                now.duration_since(stats.last_used) > DROP_AFTER_IDLE
                    && stats.total_queries < 10 // Negligible usage
            })
            .map(|(name, _)| name.clone())
            .collect();
        
        for name in to_remove {
            log::info!("Auto-dropping idle index: {}", name);
            self.drop_index(&name);
        }
    }
    
    /// Promote shadow → real index khi cost-benefit analysis positive
    pub fn maybe_promote_shadow(&mut self, shadow: &ShadowIndex) -> bool {
        // Fix Bug 1.2: THỰC SỰ kiểm tra threshold
        if shadow.sample_count < 100 {
            return false; // Chưa đủ sample
        }
        
        let cb = IndexCostBenefit::compute(shadow, self.memory_budget_mb);
        
        // Điều kiện promote:
        // 1. Speedup > 1.3x
        // 2. Hit ratio > 10%
        // 3. Memory fit trong budget
        cb.speedup_ratio >= SHADOW_SPEEDUP_THRESHOLD
            && (cb.benefiting_queries as f64 / cb.total_queries as f64) > 0.10
            && self.remaining_memory_bytes() >= cb.memory_bytes
    }
}
```

### 10.3 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/auto_manager.rs` (sửa) | Fix threshold + GC + cost-benefit |
| `qm_engine/src/index_stats.rs` (mới) | IndexCostBenefit |

---

## 11. PHASE 10 — Raft-over-RDMA

### 11.1 Hiện trạng

**File**: `qm_core/.../consensus.py`

- Chỉ có `InMemoryRaftTransport` — **không có TCP transport**
- Raft state **không persist** — restart mất toàn bộ
- Error callbacks silently swallowed
- Python implementation — latency cao

### 11.2 Kế hoạch triển khai

#### Bước 1: TCP Transport (Python, tạm thời)

```python
class TcpRaftTransport:
    """TCP-based Raft transport — bước đệm trước RDMA"""
    
    def __init__(self, bind_addr: str, peers: dict[int, str]):
        self.bind_addr = bind_addr
        self.peers = peers  # node_id → "host:port"
        self.connections = {}
    
    async def send(self, target: int, message: RaftMessage):
        conn = await self._get_connection(target)
        data = msgpack.packb(message.to_dict())
        # Length-prefixed
        header = len(data).to_bytes(4, 'big')
        conn.write(header + data)
        await conn.drain()
    
    async def _get_connection(self, target: int):
        if target not in self.connections:
            host, port = self.peers[target].rsplit(':', 1)
            reader, writer = await asyncio.open_connection(host, int(port))
            self.connections[target] = writer
        return self.connections[target]
```

#### Bước 2: Persist Raft State

```python
class PersistentRaftState:
    """Raft state persistence — required cho production"""
    
    def __init__(self, path: Path):
        self.path = path
        self.path.mkdir(parents=True, exist_ok=True)
    
    def save_hard_state(self, term: int, voted_for: Optional[int], commit_index: int):
        state = {
            "term": term,
            "voted_for": voted_for,
            "commit_index": commit_index,
        }
        # Atomic write: write to temp → fsync → rename
        tmp = self.path / "hard_state.tmp"
        with open(tmp, 'wb') as f:
            f.write(msgpack.packb(state))
            f.flush()
            os.fsync(f.fileno())
        tmp.rename(self.path / "hard_state.bin")
    
    def load_hard_state(self) -> Optional[dict]:
        p = self.path / "hard_state.bin"
        if p.exists():
            return msgpack.unpackb(p.read_bytes())
        return None
    
    def append_log_entries(self, entries: list[LogEntry]):
        with open(self.path / "raft_log.bin", 'ab') as f:
            for entry in entries:
                data = msgpack.packb(entry.to_dict())
                f.write(len(data).to_bytes(4, 'big'))
                f.write(data)
            f.flush()
            os.fsync(f.fileno())
```

#### Bước 3: Rust Raft + RDMA (Long-term)

```rust
// Cargo.toml
// raft = "0.7"           # tikv/raft-rs
// rdma-sys = "0.3"       # RDMA verbs bindings

/// RDMA-based Raft Transport
/// Chỉ dùng cho data center deployment (cần Mellanox/InfiniBand hardware)
pub struct RdmaRaftTransport {
    // RDMA Queue Pairs, 1 per peer
    qps: HashMap<u64, RdmaQueuePair>,
    // Pre-registered memory regions
    send_mr: MemoryRegion,
    recv_mr: MemoryRegion,
}

impl RdmaRaftTransport {
    /// One-sided RDMA Write — bypass remote CPU
    /// Latency: ~1-2μs (vs TCP ~50-100μs)
    pub fn send_append_entries(&self, target: u64, entries: &[u8]) -> Result<(), RdmaError> {
        let qp = &self.qps[&target];
        
        // Copy entries to registered memory
        self.send_mr.as_mut_slice()[..entries.len()].copy_from_slice(entries);
        
        // Post RDMA Write
        qp.post_send(WorkRequest::Write {
            local_addr: self.send_mr.addr(),
            len: entries.len(),
            remote_addr: qp.remote_mr_addr(),
            remote_key: qp.remote_mr_key(),
            flags: SendFlags::SIGNALED,
        })?;
        
        // Wait for completion
        qp.poll_cq()?;
        Ok(())
    }
}
```

**Prerequisite**: RDMA hardware (Mellanox ConnectX-4+) — chỉ dùng trong data center.

> **Expert Warning**: Đừng quá áp lực phần RDMA trừ khi có server chuyên dụng.
> RDMA yêu cầu cấu hình RoCE v2 Network Stack rất phức tạp.
> **Hãy làm tốt TCP-over-Tokio trước** — đã đủ cho 90% use cases.

### 11.3 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_core/.../tcp_transport.py` | TCP Raft transport |
| `qm_core/.../raft_persistence.py` | Persistent Raft state |
| `qm_core/.../consensus.py` (sửa) | Wire up TCP + persistence |
| `qm_engine/src/raft_rdma.rs` (future) | RDMA transport |

---

## 12. PHASE 11 — Cloud-native Storage (S3 + Cache) `[IMPLEMENTED]`

### 12.1 Thiết kế: Compute/Storage Separation

```
┌─────────────────────────────────────────┐
│            Compute Layer                │
│  ┌──────────┐  ┌──────────┐            │
│  │ QMvir    │  │ QMvir    │  (stateless)│
│  │ Node 1   │  │ Node 2   │            │
│  └─────┬────┘  └─────┬────┘            │
│        │              │                 │
│   ┌────▼──────────────▼────┐            │
│   │    Local Page Cache    │            │
│   │    (mmap + LRU, 8GB)  │            │
│   └────────────┬───────────┘            │
└────────────────┼────────────────────────┘
                 │ async I/O
┌────────────────▼────────────────────────┐
│           Storage Layer                 │
│  ┌─────────────────────────┐            │
│  │          S3 / MinIO     │            │
│  │   (pages stored as      │            │
│  │    objects by page_id)   │            │
│  └─────────────────────────┘            │
└─────────────────────────────────────────┘
```

#### Page Cache

```rust
use tokio::sync::RwLock;

/// ĐỀ XUẤT: Dùng W-TinyLFU thay vì LRU thuần túy.
/// W-TinyLFU chống "cache pollution" khi có full-table scan vượt trội hơn LRU.
/// Crate: `moka` (Rust) — implements W-TinyLFU, thread-safe, async-ready.
///
/// ```toml
/// moka = { version = "0.12", features = ["future"] }
/// ```
///
/// Fallback: `lru` crate nếu muốn simplicity.
use moka::future::Cache as MokaCache;

pub struct PageCache {
    cache: MokaCache<PageId, Arc<Page>>,  // W-TinyLFU thay LRU
    storage: Box<dyn StorageBackend>,
    max_size_bytes: u64,
    current_size: AtomicU64,
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn get_page(&self, id: PageId) -> Result<Page, StorageError>;
    async fn put_page(&self, id: PageId, page: &Page) -> Result<(), StorageError>;
    async fn delete_page(&self, id: PageId) -> Result<(), StorageError>;
}

pub struct S3Backend {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

#[async_trait]
impl StorageBackend for S3Backend {
    async fn get_page(&self, id: PageId) -> Result<Page, StorageError> {
        let key = format!("{}/page_{:016x}", self.prefix, id.0);
        let resp = self.client.get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await?;
        let bytes = resp.body.collect().await?.into_bytes();
        Page::deserialize(&bytes)
    }
    
    async fn put_page(&self, id: PageId, page: &Page) -> Result<(), StorageError> {
        let key = format!("{}/page_{:016x}", self.prefix, id.0);
        let data = page.serialize();
        self.client.put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(data.into())
            .send()
            .await?;
        Ok(())
    }
    
    async fn delete_page(&self, id: PageId) -> Result<(), StorageError> {
        let key = format!("{}/page_{:016x}", self.prefix, id.0);
        self.client.delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await?;
        Ok(())
    }
}

impl PageCache {
    pub async fn get(&self, id: PageId) -> Result<Arc<Page>, CacheError> {
        // 1. Check cache
        {
            let mut cache = self.cache.write().await;
            if let Some(page) = cache.get(&id) {
                return Ok(page.clone()); // Arc clone = cheap
            }
        }
        
        // 2. Cache miss → fetch from storage
        let page = Arc::new(self.storage.get_page(id).await?);
        
        // 3. Insert to cache (evict if needed)
        {
            let mut cache = self.cache.write().await;
            if let Some((_, evicted)) = cache.push(id, page.clone()) {
                self.current_size.fetch_sub(evicted.size_bytes() as u64, Ordering::Relaxed);
            }
            self.current_size.fetch_add(page.size_bytes() as u64, Ordering::Relaxed);
        }
        
        Ok(page)
    }
    
    /// Write-through: ghi cache + storage
    pub async fn put(&self, id: PageId, page: Page) -> Result<(), CacheError> {
        let page = Arc::new(page);
        self.storage.put_page(id, &page).await?;
        
        let mut cache = self.cache.write().await;
        cache.put(id, page);
        Ok(())
    }
}
```

#### Local Disk Cache (L2)

```rust
/// 2-level cache: Memory (L1) → Local SSD (L2) → S3 (L3)
pub struct TieredCache {
    l1: PageCache,        // In-memory LRU
    l2: DiskCache,        // Local SSD, mmap-based
    l3: S3Backend,        // Remote storage
}

pub struct DiskCache {
    dir: PathBuf,
    max_size_bytes: u64,
    index: DashMap<PageId, DiskCacheEntry>,
}

impl DiskCache {
    pub async fn get(&self, id: PageId) -> Option<Page> {
        let entry = self.index.get(&id)?;
        let path = self.page_path(id);
        let data = tokio::fs::read(&path).await.ok()?;
        Page::deserialize(&data).ok()
    }
    
    pub async fn put(&self, id: PageId, page: &Page) {
        let path = self.page_path(id);
        let data = page.serialize();
        tokio::fs::write(&path, &data).await.ok();
        self.index.insert(id, DiskCacheEntry {
            size: data.len(),
            last_access: Instant::now(),
        });
    }
}
```

### 12.2 Deliverables

| Output | Mô tả |
|--------|--------|
| `qm_engine/src/storage/mod.rs` | StorageBackend trait |
| `qm_engine/src/storage/s3.rs` | S3 backend |
| `qm_engine/src/storage/local.rs` | Local disk backend |
| `qm_engine/src/cache.rs` | PageCache + TieredCache |
| `Cargo.toml` (sửa) | Thêm `aws-sdk-s3` optional feature |

---

## 13. THỨ TỰ ƯU TIÊN TỔNG THỂ

```
Priority   Phase                          Blocker?   Risk       Status
───────────────────────────────────────────────────────────────────────
P0-ASAP    Bug Fixes (Section 1)          No         Low        ✅ ALL DONE
P1         Phase 1: Ring Buffer → Rust    No         Medium     ✅ DONE
P1         Phase 5: HNSW Filter-Pushdown  No         Medium     ✅ DONE
P2         Phase 2: Hub → Rust            Phase 1    Medium     ✅ DONE
P2         Phase 4: Zero-Copy             Phase 1    Low        ✅ DONE
P3         Phase 3: WAL io_uring          No         Medium     ✅ DONE
P3         Phase 7: SQL Compatibility     No         Low        ✅ DONE
P3         Phase 9: Adaptive Indexing     Bug 1.2    Low        ✅ DONE
P4         Phase 6: GPU wgpu             No         High       ✅ DONE
P4         Phase 8: JIT Cranelift        Phase 7    High       ✅ DONE
P5         Phase 10: Raft-over-RDMA      Phase 11.1 High       ❌ PENDING
P5         Phase 11: Cloud-native S3     No         Medium     ✅ DONE
P6         Phase 13: Horizontal Scaling   No         Medium     ✅ DONE
P6         Phase 14: Production Features  Phase 13   Medium     ✅ DONE
───────────────────────────────────────────────────────────────────────
Benchmarks  HNSW Filter-Pushdown          —          —         ✅ 23 bench tests
            MVCC Insert Latency           —          —         ✅ (377s, 5 scenarios)
            100K Dictionary Real-World    —          —         ✅ (8 scenarios)
            PQ / DiskANN / Rebalancing    —          —         ✅ (3 bench tests)
            Cluster / Hybrid / Snap / Met —          —         ✅ (4 bench tests)
```

### Giải thích Priority:

- **P0**: Bug fixes — sửa ngay, mỗi bug là 1 PR riêng
- **P1**: Ring Buffer + HNSW Filter — high impact, independent, có thể làm song song
- **P2**: Hub migration + Zero-copy — phụ thuộc Ring Buffer hoàn thành
- **P3**: WAL/SQL/Adaptive — quan trọng nhưng existing code vẫn chạy được
- **P4**: GPU + JIT — high effort, high risk, cần prototype trước
- **P5**: RDMA + Cloud — cần infrastructure, triển khai khi production ready

---

## 14. DEPENDENCY MAP

```
                    ┌──────────────────┐
                    │  P0: Bug Fixes   │
                    └────────┬─────────┘
                             │
              ┌──────────────┼──────────────┐
              ▼              ▼              ▼
     ┌────────────┐  ┌────────────┐  ┌────────────┐
     │ P1: Ring   │  │ P1: HNSW   │  │ P3: WAL    │
     │ Buffer Rust│  │ Filter-PD  │  │ io_uring   │
     └─────┬──────┘  └─────┬──────┘  └────────────┘
           │                │
     ┌─────┴──────┐        │
     ▼            ▼        │
┌─────────┐ ┌──────────┐  │
│ P2: Hub │ │ P2: Zero │  │
│ → Rust  │ │ Copy     │  │
└─────┬───┘ └──────────┘  │
      │                    │
      ▼                    │
┌──────────┐     ┌────────────┐
│ P3: SQL  │────►│ P4: JIT    │
│ Compat   │     │ Cranelift  │
└──────────┘     └────────────┘

┌────────────┐   ┌────────────┐
│ P4: GPU    │   │ P5: Cloud  │
│ wgpu       │   │ S3 Storage │
└────────────┘   └──────┬─────┘
                        │
                  ┌─────▼──────┐
                  │ P5: Raft   │
                  │ over RDMA  │
                  └────────────┘

              ┌──────────────────┐
              │ P6: Phase 13     │
              │ Horizontal Scale │
              │ Shard + Replica  │
              └────────┬─────────┘
                       │
         ┌─────────────┼─────────────┐
         ▼             ▼             ▼
  ┌────────────┐ ┌──────────┐ ┌──────────┐
  │ Phase 14a  │ │ Phase 14d│ │ Phase 14e│
  │ PQ+DiskANN│ │ Hybrid   │ │ Snapshots│
  │ +Rebalance│ │ Search   │ │ +Metrics │
  └────────────┘ └──────────┘ └──────────┘
```

---

## 15. PHASE 13 — Horizontal Scaling (Consistent Hashing + Replication) `[IMPLEMENTED]`

### 15.1 Consistent Hash Ring + Shard Manager

Virtual-node consistent hashing for distributing vectors across N shards with configurable replication factor.

**Files**:
- `qm_engine/src/cluster/mod.rs` — Module root
- `qm_engine/src/cluster/shard.rs` — `ConsistentHashRing`, `ShardManager`

**Key APIs**:
```rust
ShardManager::new(num_shards: u32, vnodes_per_shard: usize, replication_factor: usize)
ShardManager::route(id: i64) -> Vec<ShardId>      // Returns primary + replicas
ShardManager::route_batch(ids: &[i64]) -> AHashMap<ShardId, Vec<i64>>
ShardManager::add_shard() / remove_shard(id)       // Online topology change
```

**Benchmark**: 240 ns/route (4M routes/sec), < 5% distribution imbalance

### 15.2 Replication Manager

Configurable consistency levels (One/Quorum/All) with automatic failover and lag tracking.

**Files**:
- `qm_engine/src/cluster/replica.rs` — `ReplicaSet`, `ReplicationConfig`, `ConsistencyLevel`

**Key APIs**:
```rust
ReplicaSet::new(primary: u32, replicas: Vec<u32>, config: ReplicationConfig)
ReplicaSet::record_write(lsn) / ack_replica(id, lsn) / is_write_satisfied()
ReplicaSet::check_health() / promote(replica_id)
```

**Tests**: 9 unit tests (5 shard + 4 replica)

---

## 16. PHASE 14 — Production Features `[IMPLEMENTED]`

### 16.1 Product Quantization (32× Compression)

K-means sub-vector quantization with ADC search on compressed codes.

**Files**: `qm_native/src/pq.rs`

**Key APIs**:
```rust
ProductQuantizer::train_rust(vectors: &[f32], dim, num_sub) -> PQCodebook
ProductQuantizer::encode_rust(vector, codebook) -> Vec<u8>
ProductQuantizer::search_adc(query, codes, codebook, top_k) -> Vec<(usize, f32)>
```

**Benchmark**: 32× compression, ADC 1,492 QPS on 10K×128d

### 16.2 DiskANN (Disk-Backed HNSW)

Graph skeleton in RAM, vectors on SSD for billion-scale indexing.

**Files**: `qm_native/src/diskann.rs`

**Key APIs**:
```rust
DiskANNIndex::new_rust(path, dim, m, ef_construction, metric)
DiskANNIndex::add_rust(id, vector) / search_rust(query, top_k, ef_search)
DiskANNIndex::ram_usage_bytes() / disk_usage_bytes()
```

**Benchmark**: 3,178 QPS, RAM/Disk = 0.64×

### 16.3 HNSW Online Rebalancing

Soft-delete tombstones + online compaction + neighbor graph repair.

**Files**: `qm_native/src/hnsw.rs` (extended)

**Key APIs**:
```rust
HnswIndex::delete_rust(id)     // Tombstone marking
HnswIndex::compact()           // Remove tombstones, repair neighbors
HnswIndex::rebalance(min_neighbors)  // Reconnect under-connected nodes
HnswIndex::health() -> IndexHealth   // Quality metrics
```

**Benchmark**: Delete 296 ns, Compact 2.49 ms/1500 nodes, Post-rebalance 39,828 QPS

### 16.4 Hybrid Search (BM25 + Vector Fusion)

Multi-strategy fusion with pluggable reranking for RAG pipelines.

**Files**: `qm_engine/src/executor/hybrid_search.rs`

**Key APIs**:
```rust
reciprocal_rank_fusion(bm25, vector, k) -> Vec<(u64, f64)>
weighted_linear_fusion(bm25, vector, alpha) -> Vec<(u64, f64)>
distribution_based_fusion(bm25, vector) -> Vec<(u64, f64)>
hybrid_search(bm25, vector, config) -> (Vec<(u64, f64)>, HybridSearchStats)
```

**Benchmark**: RRF 23K ops/s, Full pipeline+reranker 14K ops/s

### 16.5 Incremental Snapshots

Delta-based page-level persistence with CRC32 integrity verification.

**Files**: `qm_engine/src/storage/snapshot.rs`

**Key APIs**:
```rust
SnapshotManager::take_snapshot(provider, full: bool) -> SnapshotHeader
SnapshotManager::recover_all(provider) -> u64  // Returns recovered LSN
DirtyTracker::mark_dirty(page_id) / drain() -> Vec<PageId>
```

**Benchmark**: Write 1,097 MB/s, Read 613 MB/s, Incremental(10%) 1.48 ms

### 16.6 Prometheus Metrics

Lock-free atomic counters, gauges, and histograms with text exposition format.

**Files**: `qm_engine/src/metrics.rs`

**Key APIs**:
```rust
MetricsRegistry::new() -> Self
registry.queries_total.inc()
registry.query_latency.observe(duration_secs)
registry.render() -> String  // Prometheus text format
```

**Benchmark**: Counter 3.6 ns, Render 14.6 μs (69K/s)

### 16.7 Deliverables Summary

| Output | Description |
|--------|-------------|
| `qm_engine/src/cluster/shard.rs` | Consistent hash ring + shard manager |
| `qm_engine/src/cluster/replica.rs` | Replication + consistency levels |
| `qm_native/src/pq.rs` | Product quantization (32× compression) |
| `qm_native/src/diskann.rs` | DiskANN disk-backed HNSW |
| `qm_native/src/hnsw.rs` (ext) | Online rebalancing + tombstones |
| `qm_engine/src/executor/hybrid_search.rs` | BM25+Vector fusion |
| `qm_engine/src/storage/snapshot.rs` | Incremental snapshots |
| `qm_engine/src/metrics.rs` | Prometheus metrics |
| Tests: 34 new | 9 cluster + 3 PQ + 2 DiskANN + 4 HNSW + 5 hybrid + 4 snapshot + 6 metrics + 1 fixed |
| Benchmarks: 7 new | PQ, DiskANN, Rebalance, Hash, Hybrid, Snapshot, Metrics |

---

## APPENDIX A: Cargo.toml Feature Flags

```toml
[features]
default = ["simd"]
simd = ["wide"]
gpu = ["wgpu", "pollster", "bytemuck"]
iouring = ["io-uring", "tokio-uring"]  # Linux only
jit = ["cranelift", "cranelift-jit", "cranelift-module"]
cloud = ["aws-sdk-s3", "aws-config"]
rdma = ["rdma-sys"]
full = ["simd", "gpu", "iouring", "jit", "cloud"]

[dependencies]
# Existing deps unchanged...

# Optional new deps
wgpu = { version = "24", optional = true }
pollster = { version = "0.4", optional = true }
io-uring = { version = "0.7", optional = true }
tokio-uring = { version = "0.5", optional = true }
cranelift = { version = "0.113", optional = true }
cranelift-jit = { version = "0.113", optional = true }
cranelift-module = { version = "0.113", optional = true }
aws-sdk-s3 = { version = "1", optional = true }
aws-config = { version = "1", optional = true }
rdma-sys = { version = "0.3", optional = true }
lru = "0.12"
```

## APPENDIX B: Test Strategy

| Phase | Test Type | Tool |
|-------|-----------|------|
| Ring Buffer | Concurrency | `shuttle` crate — random schedule fuzzing |
| HNSW | Recall accuracy | `ann-benchmarks` dataset (sift-128, gist-960) |
| WAL | Crash recovery | `failpoints` crate — inject failure |
| GPU | Correctness | Compare GPU vs CPU results (tolerance 1e-6) |
| JIT | Correctness | Compare JIT vs interpreted results |
| SQL | Compliance | `sqllogictest` crate — standard SQL test suite |
| Raft | Partition tolerance | `turmoil` crate — network fault injection |
| S3 | Integration | `localstack` or `minio` — local S3 |

---

> **Ghi chú cho dev**: Mỗi Phase là 1 feature branch riêng. Bug fixes (P0) commit thẳng vào `main`.
> Mỗi PR phải có: code + tests + benchmark (nếu performance-related).
> Dùng `cargo bench` + `criterion` cho tất cả benchmark.

---

## APPENDIX C: CẢNH BÁO KỸ THUẬT (Critical Path)

### C.1 Memory Fence — Ring Buffer (Phase 1 & 2)

**EXTREME CAUTION**: Khi implement Ring Buffer bằng Rust `std::sync::atomic`:
- Chỉ cần **1 chỗ** dùng `Relaxed` thay vì `Acquire/Release` sai mục đích
  → bug "ma" (race condition) chỉ xuất hiện 1 lần trong 1 triệu query
- **Quy tắc**:
  - Writer cuối cùng PHẢI dùng `Release` (store state = COMMITTED)
  - Reader đầu tiên PHẢI dùng `Acquire` (load state)
  - Sequence number claim: `AcqRel` (cả read lẫn write)
  - Fence trước state transition: `std::sync::atomic::fence(Ordering::Release)`
- Test bắt buộc: `shuttle` crate với ≥ 10,000 random schedules

### C.2 io_uring SQPOLL (Phase 3)

- Bật `IORING_SETUP_SQPOLL` để kernel thread tự poll — zero syscall cho write
- Pin SQPOLL thread vào CPU core riêng (không chia sẻ với query threads)
- Fallback graceful: SQPOLL → standard io_uring → std::fs

### C.3 GPU PCIe Bottleneck (Phase 6)

- PCIe 4.0 x16: ~25 GB/s bandwidth
- 1M vectors × 128 dim × 4 bytes = 512 MB → ~20ms transfer
- **PHẢI** dùng Pinned Memory (mapped host buffer) để giảm copy
- GPU chỉ thắng CPU khi batch > 10K vectors (do PCIe overhead)
- Pre-allocate buffers, reuse across queries

### C.4 W-TinyLFU thay LRU (Phase 11)

- LRU dễ bị "cache pollution" khi full-table scan đẩy hot pages ra
- W-TinyLFU (crate `moka`): frequency + recency → giữ hot pages
- Benchmark: moka vs lru trên workload mixed (OLTP + scan)

### C.5 RDMA De-prioritization (Phase 10)

- RDMA = Nice-to-have, KHÔNG phải must-have
- TCP-over-Tokio đã đủ cho 90% deployment scenarios
- Chỉ invest RDMA khi có Mellanox ConnectX-4+ hardware
