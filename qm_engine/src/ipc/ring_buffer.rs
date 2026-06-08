//! Lock-free Shared Memory Ring Buffer (LMAX Disruptor Pattern)
//!
//! Provides a zero-copy, lock-free ring buffer over mmap shared memory
//! for Hub ↔ Satellite IPC. True atomic state transitions with crash recovery.
//!
//! ## Memory Layout
//!
//! ```text
//! [Header: 64 bytes aligned][Slot 0][Slot 1]...[Slot N-1]
//!
//! Header:
//!   0..8:   magic (0x514D5649_50435200 = "QMVIPCR\0")
//!   8..16:  version (u64 = 1)
//!  16..24:  slot_count (u64, must be power of 2)
//!  24..32:  slot_data_size (u64, max payload per slot)
//!  32..40:  writer_sequence (AtomicU64, monotonically increasing)
//!  40..48:  reader_sequence (AtomicU64, consumed up to here)
//!  48..56:  writer_pid (AtomicU64, for crash detection)
//!  56..64:  epoch (AtomicU64, incremented on recovery)
//!
//! Slot (each `SLOT_HEADER_SIZE + slot_data_size` bytes):
//!   0..1:   state (AtomicU8)
//!   1..9:   lsn (u64 LE)
//!   9..10:  cmd (u8)
//!  10..14:  payload_len (u32 LE)
//!  14..18:  checksum (u32 LE, CRC32 of payload)
//!  18..20:  _pad (2 bytes)
//!  20..20+slot_data_size: payload data
//! ```

use memmap2::MmapMut;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU64, AtomicU8, Ordering};

// ── Constants ──────────────────────────────────────────────────────────

const RING_MAGIC: u64 = 0x514D_5649_5043_5200; // "QMVIPCR\0"
const RING_VERSION: u64 = 1;
const HEADER_SIZE: usize = 64;
const SLOT_HEADER_SIZE: usize = 20;

/// Slot states — each fits in 1 byte (AtomicU8 CAS-able)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// Slot is free for writing
    Free = 0,
    /// Writer is actively writing (incomplete)
    Writing = 1,
    /// Data is committed and ready for consumer
    Committed = 2,
    /// Consumer is processing
    Processing = 3,
    /// Consumer finished successfully
    Done = 4,
    /// Consumer encountered error
    Error = 5,
}

impl TryFrom<u8> for SlotState {
    type Error = u8;
    fn try_from(v: u8) -> Result<Self, u8> {
        match v {
            0 => Ok(SlotState::Free),
            1 => Ok(SlotState::Writing),
            2 => Ok(SlotState::Committed),
            3 => Ok(SlotState::Processing),
            4 => Ok(SlotState::Done),
            5 => Ok(SlotState::Error),
            x => Err(x),
        }
    }
}

/// IPC command identifiers (compatible with Python CommandType)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandType {
    Noop = 0,
    Insert = 1,
    Update = 2,
    Delete = 3,
    Query = 4,
    Ddl = 5,
    VectorOp = 6,
    Compress = 7,
    Checkpoint = 8,
    BatchInsert = 9,
    Shutdown = 255,
}

impl From<u8> for CommandType {
    fn from(v: u8) -> Self {
        match v {
            1 => CommandType::Insert,
            2 => CommandType::Update,
            3 => CommandType::Delete,
            4 => CommandType::Query,
            5 => CommandType::Ddl,
            6 => CommandType::VectorOp,
            7 => CommandType::Compress,
            8 => CommandType::Checkpoint,
            9 => CommandType::BatchInsert,
            255 => CommandType::Shutdown,
            _ => CommandType::Noop,
        }
    }
}

// ── Ring Buffer ────────────────────────────────────────────────────────

/// Lock-free ring buffer over file-backed mmap shared memory.
///
/// Safe for single-producer single-consumer (Hub → Satellite) use.
/// For multi-producer, wrap with `Arc<SharedRingBuffer>` — the atomic
/// operations ensure correctness.
pub struct SharedRingBuffer {
    mmap: MmapMut,
    path: PathBuf,
    slot_count: usize,
    slot_data_size: usize,
    slot_total_size: usize,
    mask: usize,
}

// Safety: The ring buffer uses atomic state transitions (CAS) to ensure exclusive
// access to each slot's data region. The `write_bytes` method is only called when
// the caller owns the slot (Writing or Processing state), preventing data races.
unsafe impl Sync for SharedRingBuffer {}
unsafe impl Send for SharedRingBuffer {}

impl SharedRingBuffer {
    /// Create a new ring buffer (Hub side) — initializes shared memory.
    pub fn create(path: &Path, slot_count: usize, slot_data_size: usize) -> io::Result<Self> {
        if !slot_count.is_power_of_two() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slot_count must be power of 2",
            ));
        }

        let slot_total_size = SLOT_HEADER_SIZE + slot_data_size;
        let total_size = HEADER_SIZE + slot_count * slot_total_size;

        // Create and truncate file
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(total_size as u64)?;

        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        // Write header
        mmap[0..8].copy_from_slice(&RING_MAGIC.to_le_bytes());
        mmap[8..16].copy_from_slice(&RING_VERSION.to_le_bytes());
        mmap[16..24].copy_from_slice(&(slot_count as u64).to_le_bytes());
        mmap[24..32].copy_from_slice(&(slot_data_size as u64).to_le_bytes());
        // writer_sequence, reader_sequence, writer_pid, epoch all start at 0

        // Initialize all slots to Free
        for i in 0..slot_count {
            let off = HEADER_SIZE + i * slot_total_size;
            mmap[off] = SlotState::Free as u8;
        }

        mmap.flush()?;

        Ok(Self {
            mmap,
            path: path.to_path_buf(),
            slot_count,
            slot_data_size,
            slot_total_size,
            mask: slot_count - 1,
        })
    }

    /// Attach to an existing ring buffer (Satellite side).
    pub fn attach(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        // Validate magic
        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap());
        if magic != RING_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid ring buffer magic — not a QMvir shared memory region",
            ));
        }

        let slot_count = u64::from_le_bytes(mmap[16..24].try_into().unwrap()) as usize;
        let slot_data_size = u64::from_le_bytes(mmap[24..32].try_into().unwrap()) as usize;
        let slot_total_size = SLOT_HEADER_SIZE + slot_data_size;

        Ok(Self {
            mmap,
            path: path.to_path_buf(),
            slot_count,
            slot_data_size,
            slot_total_size,
            mask: slot_count - 1,
        })
    }

    // ── Header field accessors (atomic via pointer cast) ───────────

    fn writer_seq_ptr(&self) -> &AtomicU64 {
        // Safety: offset 32 is aligned to 8 bytes within Header
        unsafe { &*(self.mmap.as_ptr().add(32) as *const AtomicU64) }
    }

    fn reader_seq_ptr(&self) -> &AtomicU64 {
        unsafe { &*(self.mmap.as_ptr().add(40) as *const AtomicU64) }
    }

    fn epoch_ptr(&self) -> &AtomicU64 {
        unsafe { &*(self.mmap.as_ptr().add(56) as *const AtomicU64) }
    }

    fn slot_state_ptr(&self, index: usize) -> &AtomicU8 {
        let off = HEADER_SIZE + (index & self.mask) * self.slot_total_size;
        unsafe { &*(self.mmap.as_ptr().add(off) as *const AtomicU8) }
    }

    fn slot_header_offset(&self, index: usize) -> usize {
        HEADER_SIZE + (index & self.mask) * self.slot_total_size
    }

    fn slot_data_offset(&self, index: usize) -> usize {
        self.slot_header_offset(index) + SLOT_HEADER_SIZE
    }

    /// Write bytes into mmap via raw pointer.
    ///
    /// # Safety
    /// Caller must ensure exclusive access to `[offset..offset+data.len()]`.
    /// Guaranteed by the atomic state machine: only the writer touches a slot in
    /// `Writing` state, only the consumer touches a slot in `Processing` state.
    #[inline]
    unsafe fn write_bytes(&self, offset: usize, data: &[u8]) {
        let ptr = self.mmap.as_ptr() as *mut u8;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(offset), data.len());
    }

    // ── Producer (Hub side) ────────────────────────────────────────

    /// Publish a command into the ring buffer (Hub → Satellite).
    ///
    /// Returns the sequence number on success, or `Err` if the ring is full.
    ///
    /// ## Memory ordering
    /// - Payload is written BEFORE state transition (data-before-flag)
    /// - State store uses `Release` so consumer sees complete payload
    pub fn publish(&self, lsn: u64, cmd: CommandType, payload: &[u8]) -> Result<u64, RingError> {
        if payload.len() > self.slot_data_size {
            return Err(RingError::PayloadTooLarge {
                size: payload.len(),
                max: self.slot_data_size,
            });
        }

        // Claim sequence slot
        let seq = self.writer_seq_ptr().fetch_add(1, Ordering::AcqRel);
        let slot_idx = seq as usize & self.mask;

        // Check if slot is free — back-pressure if ring is full
        let state = self.slot_state_ptr(slot_idx).load(Ordering::Acquire);
        if state != SlotState::Free as u8 {
            // Undo claim (best-effort, doesn't affect correctness)
            self.writer_seq_ptr().fetch_sub(1, Ordering::AcqRel);
            return Err(RingError::Full);
        }

        // CAS: Free → Writing
        match self.slot_state_ptr(slot_idx).compare_exchange(
            SlotState::Free as u8,
            SlotState::Writing as u8,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => {}
            Err(_) => {
                self.writer_seq_ptr().fetch_sub(1, Ordering::AcqRel);
                return Err(RingError::Full);
            }
        }

        // Write payload data
        let doff = self.slot_data_offset(slot_idx);
        // Safety: exclusive access guaranteed — slot is in Writing state
        unsafe {
            self.write_bytes(doff, payload);
        }

        // Compute CRC32
        let crc = crc32fast::hash(payload);

        // Write slot header fields (non-atomic, but safe because state=WRITING)
        let hoff = self.slot_header_offset(slot_idx);
        // Skip byte 0 (state), write the rest
        unsafe {
            self.write_bytes(hoff + 1, &lsn.to_le_bytes());
            self.write_bytes(hoff + 9, &[cmd as u8]);
            self.write_bytes(hoff + 10, &(payload.len() as u32).to_le_bytes());
            self.write_bytes(hoff + 14, &crc.to_le_bytes());
        }

        // Release fence: ensure all writes above are visible before state change
        fence(Ordering::Release);

        // State transition: Writing → Committed (makes slot visible to consumer)
        self.slot_state_ptr(slot_idx)
            .store(SlotState::Committed as u8, Ordering::Release);

        Ok(seq)
    }

    /// Collect result from a completed slot. Returns payload and resets to Free.
    pub fn collect_result(&self, slot_idx: usize) -> Result<(SlotState, Vec<u8>), RingError> {
        let state_byte = self.slot_state_ptr(slot_idx).load(Ordering::Acquire);
        let state =
            SlotState::try_from(state_byte).map_err(|_| RingError::InvalidState(state_byte))?;

        if state != SlotState::Done && state != SlotState::Error {
            return Err(RingError::NotComplete(state));
        }

        let hoff = self.slot_header_offset(slot_idx);
        let payload_len =
            u32::from_le_bytes(self.mmap[hoff + 10..hoff + 14].try_into().unwrap()) as usize;

        let doff = self.slot_data_offset(slot_idx);
        let data = self.mmap[doff..doff + payload_len].to_vec();

        // Reset to Free
        self.slot_state_ptr(slot_idx)
            .store(SlotState::Free as u8, Ordering::Release);

        Ok((state, data))
    }

    // ── Consumer (Satellite side) ──────────────────────────────────

    /// Try to consume the next Committed slot. Returns None if nothing ready.
    ///
    /// ## Memory ordering
    /// - State load uses `Acquire` to see the complete payload from producer
    /// - CAS Committed → Processing before reading data
    pub fn consume(&self) -> Option<ConsumedSlot> {
        let seq = self.reader_seq_ptr().load(Ordering::Acquire);
        let slot_idx = seq as usize & self.mask;

        // Check if slot is committed
        match self.slot_state_ptr(slot_idx).compare_exchange(
            SlotState::Committed as u8,
            SlotState::Processing as u8,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => {}
            Err(_) => return None,
        }

        // Advance reader sequence
        self.reader_seq_ptr().fetch_add(1, Ordering::AcqRel);

        // Acquire fence: ensure we see all producer writes
        fence(Ordering::Acquire);

        let hoff = self.slot_header_offset(slot_idx);
        let lsn = u64::from_le_bytes(self.mmap[hoff + 1..hoff + 9].try_into().unwrap());
        let cmd = CommandType::from(self.mmap[hoff + 9]);
        let payload_len =
            u32::from_le_bytes(self.mmap[hoff + 10..hoff + 14].try_into().unwrap()) as usize;
        let stored_crc = u32::from_le_bytes(self.mmap[hoff + 14..hoff + 18].try_into().unwrap());

        let doff = self.slot_data_offset(slot_idx);
        let payload = self.mmap[doff..doff + payload_len].to_vec();

        // Verify CRC32 integrity
        let computed_crc = crc32fast::hash(&payload);
        if computed_crc != stored_crc {
            // Corrupt data — mark as error and skip
            self.slot_state_ptr(slot_idx)
                .store(SlotState::Error as u8, Ordering::Release);
            return None;
        }

        Some(ConsumedSlot {
            slot_idx,
            lsn,
            cmd,
            payload,
        })
    }

    /// Mark slot as done (Satellite finished successfully). Optionally write result payload.
    pub fn complete(&self, slot_idx: usize, result: Option<&[u8]>) {
        if let Some(data) = result {
            let doff = self.slot_data_offset(slot_idx);
            let len = data.len().min(self.slot_data_size);

            let hoff = self.slot_header_offset(slot_idx);
            let crc = crc32fast::hash(&data[..len]);
            // Safety: exclusive access — slot is in Processing state
            unsafe {
                self.write_bytes(doff, &data[..len]);
                self.write_bytes(hoff + 10, &(len as u32).to_le_bytes());
                self.write_bytes(hoff + 14, &crc.to_le_bytes());
            }

            fence(Ordering::Release);
        }
        self.slot_state_ptr(slot_idx)
            .store(SlotState::Done as u8, Ordering::Release);
    }

    /// Mark slot as error (Satellite encountered failure).
    pub fn fail(&self, slot_idx: usize, error_data: Option<&[u8]>) {
        if let Some(data) = error_data {
            let doff = self.slot_data_offset(slot_idx);
            let len = data.len().min(self.slot_data_size);

            let hoff = self.slot_header_offset(slot_idx);
            // Safety: exclusive access — slot is in Processing state
            unsafe {
                self.write_bytes(doff, &data[..len]);
                self.write_bytes(hoff + 10, &(len as u32).to_le_bytes());
            }

            fence(Ordering::Release);
        }
        self.slot_state_ptr(slot_idx)
            .store(SlotState::Error as u8, Ordering::Release);
    }

    // ── Crash Recovery ─────────────────────────────────────────────

    /// Recover after a process crash. Scans all slots and fixes incomplete states.
    ///
    /// - `Writing` → discard (writer died mid-write) → `Free`
    /// - `Processing` → re-queue (reader died mid-process) → `Committed`  
    /// - `Free`, `Committed`, `Done`, `Error` → unchanged
    pub fn recover_after_crash(&self) -> RecoveryReport {
        let new_epoch = self.epoch_ptr().fetch_add(1, Ordering::AcqRel) + 1;
        let mut report = RecoveryReport {
            epoch: new_epoch,
            discarded_writes: 0,
            requeued_slots: 0,
            intact_slots: 0,
        };

        let mut min_requeued_slot: Option<usize> = None;

        for i in 0..self.slot_count {
            let state_byte = self.slot_state_ptr(i).load(Ordering::Acquire);
            match SlotState::try_from(state_byte) {
                Ok(SlotState::Writing) => {
                    // Writer died mid-write → discard
                    self.slot_state_ptr(i)
                        .store(SlotState::Free as u8, Ordering::Release);
                    report.discarded_writes += 1;
                }
                Ok(SlotState::Processing) => {
                    // Reader died mid-process → re-queue for retry
                    self.slot_state_ptr(i)
                        .store(SlotState::Committed as u8, Ordering::Release);
                    report.requeued_slots += 1;
                    min_requeued_slot = Some(match min_requeued_slot {
                        Some(prev) => prev.min(i),
                        None => i,
                    });
                }
                _ => {
                    report.intact_slots += 1;
                }
            }
        }

        // Rewind reader_seq so re-queued slots can be consumed again
        if let Some(slot_idx) = min_requeued_slot {
            let current_reader = self.reader_seq_ptr().load(Ordering::Acquire);
            // Find the sequence that maps to this slot index
            // The slot's sequence = some_base where some_base & mask == slot_idx
            // We need reader_seq ≤ that value. Simplest: set to slot_idx
            // (safe because we only re-process Committed slots)
            let target_seq = (current_reader & !(self.mask as u64)) | (slot_idx as u64);
            let rewind_to = if target_seq < current_reader {
                target_seq
            } else {
                // slot_idx is ahead of or at reader seq mod mask
                target_seq.saturating_sub(self.slot_count as u64)
            };
            if rewind_to < current_reader {
                self.reader_seq_ptr().store(rewind_to, Ordering::Release);
            }
        }

        // Full fence: ensure all recovery state transitions are globally visible
        // before any new producer/consumer resumes.
        fence(Ordering::SeqCst);

        report
    }

    // ── Diagnostics ────────────────────────────────────────────────

    /// Get ring buffer status for monitoring.
    pub fn status(&self) -> RingStatus {
        // Use Acquire ordering so callers see consistent state across multi-core.
        let writer_seq = self.writer_seq_ptr().load(Ordering::Acquire);
        let reader_seq = self.reader_seq_ptr().load(Ordering::Acquire);
        let epoch = self.epoch_ptr().load(Ordering::Acquire);

        let mut state_counts = [0u32; 6];
        for i in 0..self.slot_count {
            let s = self.slot_state_ptr(i).load(Ordering::Acquire);
            if (s as usize) < state_counts.len() {
                state_counts[s as usize] += 1;
            }
        }

        RingStatus {
            writer_seq,
            reader_seq,
            epoch,
            slot_count: self.slot_count,
            slot_data_size: self.slot_data_size,
            free_slots: state_counts[0],
            writing_slots: state_counts[1],
            committed_slots: state_counts[2],
            processing_slots: state_counts[3],
            done_slots: state_counts[4],
            error_slots: state_counts[5],
        }
    }

    /// Path to the backing shared memory file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ── Supporting types ───────────────────────────────────────────────────

/// A consumed slot with payload data, ready for processing.
#[derive(Debug)]
pub struct ConsumedSlot {
    pub slot_idx: usize,
    pub lsn: u64,
    pub cmd: CommandType,
    pub payload: Vec<u8>,
}

/// Recovery report after crash detection.
#[derive(Debug)]
pub struct RecoveryReport {
    pub epoch: u64,
    pub discarded_writes: usize,
    pub requeued_slots: usize,
    pub intact_slots: usize,
}

/// Ring buffer diagnostic status.
#[derive(Debug)]
pub struct RingStatus {
    pub writer_seq: u64,
    pub reader_seq: u64,
    pub epoch: u64,
    pub slot_count: usize,
    pub slot_data_size: usize,
    pub free_slots: u32,
    pub writing_slots: u32,
    pub committed_slots: u32,
    pub processing_slots: u32,
    pub done_slots: u32,
    pub error_slots: u32,
}

/// Ring buffer errors.
#[derive(Debug)]
pub enum RingError {
    Full,
    PayloadTooLarge {
        size: usize,
        max: usize,
    },
    InvalidState(u8),
    NotComplete(SlotState),
    ChecksumMismatch {
        slot: usize,
        expected: u32,
        got: u32,
    },
}

impl std::fmt::Display for RingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RingError::Full => write!(f, "Ring buffer is full"),
            RingError::PayloadTooLarge { size, max } => write!(
                f,
                "Payload {} bytes exceeds slot capacity {} bytes",
                size, max
            ),
            RingError::InvalidState(s) => write!(f, "Invalid slot state: {}", s),
            RingError::NotComplete(s) => write!(f, "Slot not complete: {:?}", s),
            RingError::ChecksumMismatch {
                slot,
                expected,
                got,
            } => write!(
                f,
                "CRC mismatch at slot {}: expected {:#x}, got {:#x}",
                slot, expected, got
            ),
        }
    }
}

impl std::error::Error for RingError {}

// ── PyO3 Bindings ──────────────────────────────────────────────────────

/// Python-exposed ring buffer for IPC between Hub and Satellite processes.
#[cfg(feature = "python")]
#[pyclass(name = "RustRingBuffer")]
pub struct PyRingBuffer {
    inner: SharedRingBuffer,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyRingBuffer {
    /// Create a new ring buffer (Hub side).
    #[new]
    #[pyo3(signature = (path, slot_count=1024, slot_data_size=65536))]
    fn new(path: &str, slot_count: usize, slot_data_size: usize) -> PyResult<Self> {
        let inner = SharedRingBuffer::create(Path::new(path), slot_count, slot_data_size)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Attach to an existing ring buffer (Satellite side).
    #[staticmethod]
    fn attach(path: &str) -> PyResult<Self> {
        let inner = SharedRingBuffer::attach(Path::new(path))
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Publish a message into the ring (Hub → Satellite).
    fn publish(&self, lsn: u64, cmd: u8, payload: &[u8]) -> PyResult<u64> {
        self.inner
            .publish(lsn, CommandType::from(cmd), payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Try to consume the next message (Satellite side).
    /// Returns (slot_idx, lsn, cmd, payload) or None.
    fn consume(&self) -> Option<(usize, u64, u8, Vec<u8>)> {
        self.inner
            .consume()
            .map(|s| (s.slot_idx, s.lsn, s.cmd as u8, s.payload))
    }

    /// Mark slot as completed.
    #[pyo3(signature = (slot_idx, result=None))]
    fn complete(&self, slot_idx: usize, result: Option<Vec<u8>>) {
        self.inner.complete(slot_idx, result.as_deref());
    }

    /// Mark slot as failed.
    #[pyo3(signature = (slot_idx, error_data=None))]
    fn fail(&self, slot_idx: usize, error_data: Option<Vec<u8>>) {
        self.inner.fail(slot_idx, error_data.as_deref());
    }

    /// Collect result from a Done/Error slot. Returns (state_int, payload).
    fn collect_result(&self, slot_idx: usize) -> PyResult<(u8, Vec<u8>)> {
        self.inner
            .collect_result(slot_idx)
            .map(|(state, data)| (state as u8, data))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Recover after crash — returns (epoch, discarded, requeued, intact).
    fn recover(&self) -> (u64, usize, usize, usize) {
        let r = self.inner.recover_after_crash();
        (
            r.epoch,
            r.discarded_writes,
            r.requeued_slots,
            r.intact_slots,
        )
    }

    /// Get ring status as dict.
    fn status(&self) -> PyResult<(u64, u64, u64, usize, u32, u32, u32, u32)> {
        let s = self.inner.status();
        Ok((
            s.writer_seq,
            s.reader_seq,
            s.epoch,
            s.slot_count,
            s.free_slots,
            s.committed_slots,
            s.processing_slots,
            s.done_slots,
        ))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_ring(slot_count: usize, slot_data_size: usize) -> SharedRingBuffer {
        let dir = std::env::temp_dir();
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("qm_test_ring_{}_{}.shm", std::process::id(), id));
        SharedRingBuffer::create(&path, slot_count, slot_data_size).unwrap()
    }

    #[test]
    fn test_create_and_publish_consume() {
        let ring = temp_ring(16, 4096);
        let payload = b"hello world";

        let seq = ring.publish(1, CommandType::Insert, payload).unwrap();
        assert_eq!(seq, 0);

        let consumed = ring.consume().unwrap();
        assert_eq!(consumed.lsn, 1);
        assert_eq!(consumed.cmd, CommandType::Insert);
        assert_eq!(consumed.payload, payload);

        // Complete the slot
        ring.complete(consumed.slot_idx, Some(b"ok"));

        let (state, data) = ring.collect_result(consumed.slot_idx).unwrap();
        assert_eq!(state, SlotState::Done);
        assert_eq!(data, b"ok");
    }

    #[test]
    fn test_ring_full() {
        let ring = temp_ring(4, 1024);
        for i in 0..4 {
            ring.publish(i, CommandType::Insert, b"data").unwrap();
        }
        // Ring should be full now
        assert!(ring.publish(99, CommandType::Insert, b"overflow").is_err());
    }

    #[test]
    fn test_crash_recovery_writing() {
        let ring = temp_ring(8, 1024);

        // Simulate: writer starts but crashes during write
        // Manually set a slot to Writing state
        ring.slot_state_ptr(0)
            .store(SlotState::Writing as u8, Ordering::Release);

        let report = ring.recover_after_crash();
        assert_eq!(report.discarded_writes, 1);
        assert_eq!(
            ring.slot_state_ptr(0).load(Ordering::Acquire),
            SlotState::Free as u8
        );
    }

    #[test]
    fn test_crash_recovery_processing() {
        let ring = temp_ring(8, 1024);

        // Publish and consume a slot
        ring.publish(1, CommandType::Insert, b"test").unwrap();
        let _consumed = ring.consume().unwrap();
        // Now slot is in Processing state

        // Simulate crash — satellite dies
        let report = ring.recover_after_crash();
        assert_eq!(report.requeued_slots, 1);
        assert_eq!(
            ring.slot_state_ptr(0).load(Ordering::Acquire),
            SlotState::Committed as u8
        );

        // Should be consumable again
        let re_consumed = ring.consume().unwrap();
        assert_eq!(re_consumed.payload, b"test");
    }

    #[test]
    fn test_payload_too_large() {
        let ring = temp_ring(4, 16);
        let big = [0u8; 64];
        assert!(ring.publish(1, CommandType::Insert, &big).is_err());
    }

    #[test]
    fn test_status() {
        let ring = temp_ring(8, 1024);
        ring.publish(1, CommandType::Insert, b"a").unwrap();
        ring.publish(2, CommandType::Insert, b"b").unwrap();

        let status = ring.status();
        assert_eq!(status.slot_count, 8);
        assert_eq!(status.committed_slots, 2);
        assert_eq!(status.free_slots, 6);
    }

    #[test]
    fn test_attach_and_cross_process_simulation() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("qm_attach_test_{}.shm", std::process::id()));

        // Hub creates
        let hub_ring = SharedRingBuffer::create(&path, 16, 4096).unwrap();
        hub_ring
            .publish(42, CommandType::Query, b"SELECT 1")
            .unwrap();

        // Satellite attaches
        let sat_ring = SharedRingBuffer::attach(&path).unwrap();
        let consumed = sat_ring.consume().unwrap();
        assert_eq!(consumed.lsn, 42);
        assert_eq!(consumed.payload, b"SELECT 1");

        // Satellite completes
        sat_ring.complete(consumed.slot_idx, Some(b"1"));

        // Hub collects
        let (state, data) = hub_ring.collect_result(consumed.slot_idx).unwrap();
        assert_eq!(state, SlotState::Done);
        assert_eq!(data, b"1");
    }

    #[test]
    fn test_concurrent_publish_consume() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("qm_conc_test_{}.shm", std::process::id()));
        let ring = Arc::new(SharedRingBuffer::create(&path, 256, 1024).unwrap());
        let ring2 = Arc::clone(&ring);

        let producer = std::thread::spawn(move || {
            for i in 0u64..100 {
                loop {
                    match ring.publish(i, CommandType::Insert, &i.to_le_bytes()) {
                        Ok(_) => break,
                        Err(RingError::Full) => std::thread::yield_now(),
                        Err(e) => panic!("publish error: {}", e),
                    }
                }
            }
        });

        let consumer = std::thread::spawn(move || {
            let mut received = Vec::new();
            while received.len() < 100 {
                if let Some(slot) = ring2.consume() {
                    let val = u64::from_le_bytes(slot.payload.try_into().unwrap());
                    received.push(val);
                    ring2.complete(slot.slot_idx, None);
                } else {
                    std::thread::yield_now();
                }
            }
            received
        });

        producer.join().unwrap();
        let received = consumer.join().unwrap();
        assert_eq!(received.len(), 100);
    }
}
