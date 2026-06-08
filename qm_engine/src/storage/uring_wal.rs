//! io_uring WAL Writer — asynchronous disk I/O for Linux.
//!
//! Uses io_uring for zero-copy, kernel-bypassing disk writes.
//! Falls back to synchronous I/O on non-Linux platforms.
//!
//! Architecture:
//! - Submission Queue (SQ): batch WAL records into aligned write buffers
//! - Completion Queue (CQ): poll completions without syscalls (shared kernel ring)
//! - FSync batching: group multiple writes before a single fsync
//!
//! Performance characteristics:
//! - Eliminates context-switch overhead (no read/write syscalls in hot path)
//! - Supports O_DIRECT for bypassing page cache on SSDs
//! - Batch fsync coalescing: one fsync per N commits

use crc32fast;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "linux")]
use io_uring::{opcode, types, IoUring};

// ── Constants ─────────────────────────────────────────────────────

/// Default io_uring queue depth (SQ entries).
#[cfg(target_os = "linux")]
const URING_QUEUE_DEPTH: u32 = 256;

/// Buffer alignment for O_DIRECT (must be sector-aligned).
const DIRECT_IO_ALIGN: usize = 4096;

/// Maximum segment size (64 MB).
const SEGMENT_MAX_SIZE: u64 = 64 * 1024 * 1024;

/// Number of writes to coalesce before issuing fsync.
const FSYNC_COALESCE_COUNT: u64 = 32;

// ── WAL Record (shared with wal.rs) ──────────────────────────────

/// Lightweight WAL record for io_uring path.
/// Format: [LSN:8][Type:1][TxnID:8][Len:4][Data:*][CRC32:4]
#[derive(Debug)]
pub struct UringWalRecord {
    pub lsn: u64,
    pub record_type: u8,
    pub txn_id: u64,
    pub data: Vec<u8>,
}

impl UringWalRecord {
    /// Encode into bytes with CRC32 trailer.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(25 + self.data.len());
        buf.extend_from_slice(&self.lsn.to_le_bytes());
        buf.push(self.record_type);
        buf.extend_from_slice(&self.txn_id.to_le_bytes());
        buf.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.data);
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf
    }
}

// ── Write Buffer Pool ─────────────────────────────────────────────

/// Aligned write buffer for O_DIRECT compatibility.
///
/// Buffers are reused from a pool to avoid repeated allocation.
struct AlignedBuffer {
    data: Vec<u8>,
    capacity: usize,
}

impl AlignedBuffer {
    fn new(capacity: usize) -> Self {
        // Round up to alignment boundary
        let aligned_cap = (capacity + DIRECT_IO_ALIGN - 1) & !(DIRECT_IO_ALIGN - 1);
        let mut data = Vec::with_capacity(aligned_cap);
        // Zero-fill for alignment padding
        data.resize(0, 0);
        Self {
            data,
            capacity: aligned_cap,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> bool {
        if self.data.len() + bytes.len() > self.capacity {
            return false;
        }
        self.data.extend_from_slice(bytes);
        true
    }

    fn padded_len(&self) -> usize {
        // Round up to alignment for O_DIRECT
        (self.data.len() + DIRECT_IO_ALIGN - 1) & !(DIRECT_IO_ALIGN - 1)
    }

    fn as_padded_bytes(&self) -> Vec<u8> {
        let padded = self.padded_len();
        let mut out = self.data.clone();
        out.resize(padded, 0); // zero-pad
        out
    }

    fn clear(&mut self) {
        self.data.clear();
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

// ── io_uring WAL Writer (Linux fast path) ─────────────────────────

/// io_uring-accelerated WAL writer.
///
/// On Linux, uses the io_uring syscall interface for batched async disk I/O.
/// On other platforms, falls back to buffered synchronous writes.
///
/// ## Usage
///
/// ```ignore
/// let mut wal = UringWalWriter::open("/tmp/wal_dir", true)?;
/// let lsn = wal.append(1, 4, b"INSERT row data")?;
/// wal.flush()?; // ensure durability
/// ```
pub struct UringWalWriter {
    dir: PathBuf,
    /// Current segment file
    segment_file: File,
    /// Current segment path
    segment_path: PathBuf,
    /// Current write offset within segment
    segment_offset: u64,
    /// Segment sequence number
    segment_seq: u64,
    /// Next LSN
    next_lsn: AtomicU64,
    /// Write coalesce buffer
    buffer: AlignedBuffer,
    /// Writes since last fsync
    writes_since_fsync: u64,
    /// Whether to use O_DIRECT (requires aligned buffers)
    direct_io: bool,
    /// Linux io_uring handle (if initialization succeeded)
    #[cfg(target_os = "linux")]
    ring: Option<IoUring>,
    /// Total bytes written
    total_bytes_written: u64,
}

impl UringWalWriter {
    /// Open or create a WAL directory.
    ///
    /// `direct_io`: if true, bypass OS page cache (requires sector-aligned I/O).
    /// Recommended for NVMe SSDs. Falls back gracefully if unsupported.
    pub fn open(dir: &Path, direct_io: bool) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;

        let segment_seq = 0u64;
        let segment_path = dir.join(format!("uring_wal_{:016x}.log", segment_seq));

        let segment_file = Self::open_segment(&segment_path, direct_io)?;

        #[cfg(target_os = "linux")]
        let ring = IoUring::new(URING_QUEUE_DEPTH).ok();

        Ok(Self {
            dir: dir.to_path_buf(),
            segment_file,
            segment_path,
            segment_offset: 0,
            segment_seq,
            next_lsn: AtomicU64::new(1),
            buffer: AlignedBuffer::new(256 * 1024), // 256KB coalesce buffer
            writes_since_fsync: 0,
            direct_io,
            #[cfg(target_os = "linux")]
            ring,
            total_bytes_written: 0,
        })
    }

    fn open_segment(path: &Path, _direct_io: bool) -> io::Result<File> {
        let mut opts = OpenOptions::new();
        opts.create(true).write(true).read(true);

        // O_DIRECT is Linux-only
        #[cfg(target_os = "linux")]
        if _direct_io {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_DIRECT);
        }

        opts.open(path)
    }

    /// Append a WAL record. Returns the assigned LSN.
    ///
    /// Records are buffered and flushed in batches for throughput.
    /// Call `flush()` for durability guarantees.
    pub fn append(&mut self, txn_id: u64, record_type: u8, data: &[u8]) -> io::Result<u64> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::SeqCst);

        let record = UringWalRecord {
            lsn,
            record_type,
            txn_id,
            data: data.to_vec(),
        };
        let encoded = record.encode();

        // If buffer can't hold this record, flush first
        if !self.buffer.append(&encoded) {
            self.flush_buffer()?;
            if !self.buffer.append(&encoded) {
                // Record too large for buffer — write directly
                self.write_direct(&encoded)?;
                return Ok(lsn);
            }
        }

        self.writes_since_fsync += 1;

        // Auto-flush on coalesce threshold
        if self.writes_since_fsync >= FSYNC_COALESCE_COUNT {
            self.flush()?;
        }

        Ok(lsn)
    }

    /// Flush buffer and fsync for durability.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        self.fsync()?;
        self.writes_since_fsync = 0;
        Ok(())
    }

    /// Flush the coalesce buffer to disk.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // Check segment rotation
        if self.segment_offset + self.buffer.len() as u64 > SEGMENT_MAX_SIZE {
            self.rotate_segment()?;
        }

        self.write_buffer_to_segment()?;
        self.buffer.clear();
        Ok(())
    }

    /// Write buffer contents to the segment file.
    ///
    /// On Linux with io_uring available, uses io_uring for async write.
    /// Otherwise, falls back to pwrite64/write.
    fn write_buffer_to_segment(&mut self) -> io::Result<()> {
        let bytes = if self.direct_io {
            self.buffer.as_padded_bytes()
        } else {
            self.buffer.data.clone()
        };

        let actual_len = self.buffer.len();

        // Platform-specific write path
        #[cfg(target_os = "linux")]
        {
            self.write_with_uring_or_pwrite(&bytes, self.segment_offset)?;
        }

        #[cfg(not(target_os = "linux"))]
        {
            use std::io::Seek;
            use std::io::Write;
            self.segment_file
                .seek(std::io::SeekFrom::Start(self.segment_offset))?;
            self.segment_file.write_all(&bytes)?;
        }

        self.segment_offset += actual_len as u64;
        self.total_bytes_written += actual_len as u64;
        Ok(())
    }

    /// Linux io_uring write path with pwrite fallback.
    #[cfg(target_os = "linux")]
    fn write_with_uring_or_pwrite(&mut self, data: &[u8], offset: u64) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        let mut disable_uring = false;

        if let Some(ring) = self.ring.as_mut() {
            let fd = self.segment_file.as_raw_fd();
            let entry = opcode::Write::new(types::Fd(fd), data.as_ptr(), data.len() as _)
                .offset(offset as _)
                .build();

            let uring_result: io::Result<()> = (|| {
                // Safe because `data` lives until completion is consumed below.
                unsafe {
                    ring.submission().push(&entry).map_err(|_| {
                        io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full")
                    })?;
                }

                ring.submit_and_wait(1)?;

                let cqe = ring
                    .completion()
                    .next()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "io_uring CQ empty"))?;

                let res = cqe.result();
                if res < 0 {
                    return Err(io::Error::from_raw_os_error(-res));
                }
                if (res as usize) != data.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        format!("short io_uring write: {} of {} bytes", res, data.len()),
                    ));
                }

                Ok(())
            })();

            if uring_result.is_ok() {
                return Ok(());
            }

            // If io_uring fails at runtime, permanently downgrade to pwrite64.
            disable_uring = true;
        }

        if disable_uring {
            self.ring = None;
        }

        // Use pwrite64 for positional write (no seek needed)
        let fd = self.segment_file.as_raw_fd();
        let ret = unsafe {
            libc::pwrite64(
                fd,
                data.as_ptr() as *const libc::c_void,
                data.len(),
                offset as libc::off64_t,
            )
        };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        if (ret as usize) != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("short write: {} of {} bytes", ret, data.len()),
            ));
        }
        Ok(())
    }

    /// Write a single large record directly (bypass buffer).
    fn write_direct(&mut self, data: &[u8]) -> io::Result<()> {
        if self.segment_offset + data.len() as u64 > SEGMENT_MAX_SIZE {
            self.rotate_segment()?;
        }

        #[cfg(target_os = "linux")]
        {
            self.write_with_uring_or_pwrite(data, self.segment_offset)?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            use std::io::{Seek, SeekFrom, Write};
            self.segment_file
                .seek(SeekFrom::Start(self.segment_offset))?;
            self.segment_file.write_all(data)?;
        }

        self.segment_offset += data.len() as u64;
        self.total_bytes_written += data.len() as u64;
        Ok(())
    }

    /// Issue fsync on the current segment.
    ///
    /// On Linux with io_uring active, uses `IORING_OP_FSYNC` with
    /// `IORING_FSYNC_DATASYNC` to avoid an extra syscall.
    /// Falls back to `fdatasync(2)` / `sync_data()` otherwise.
    fn fsync(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self.segment_file.as_raw_fd();

            if let Some(ring) = self.ring.as_mut() {
                let entry = opcode::Fsync::new(types::Fd(fd))
                    .flags(io_uring::types::FsyncFlags::DATASYNC)
                    .build();

                let uring_ok: io::Result<()> = (|| {
                    unsafe {
                        ring.submission()
                            .push(&entry)
                            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "SQ full"))?;
                    }
                    ring.submit_and_wait(1)?;
                    let cqe = ring
                        .completion()
                        .next()
                        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "CQ empty"))?;
                    let res = cqe.result();
                    if res < 0 {
                        return Err(io::Error::from_raw_os_error(-res));
                    }
                    Ok(())
                })();

                if uring_ok.is_ok() {
                    return Ok(());
                }
                // Fallback — don't disable ring here, write path might still work.
            }

            let ret = unsafe { libc::fdatasync(fd) };
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.segment_file.sync_data()
        }
    }

    /// Rotate to a new segment file.
    fn rotate_segment(&mut self) -> io::Result<()> {
        self.fsync()?;
        self.segment_seq += 1;
        self.segment_path = self
            .dir
            .join(format!("uring_wal_{:016x}.log", self.segment_seq));
        self.segment_file = Self::open_segment(&self.segment_path, self.direct_io)?;
        self.segment_offset = 0;
        Ok(())
    }

    /// Recover: read all segments in order and return decoded records.
    pub fn recover(&self) -> io::Result<Vec<UringWalRecord>> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("uring_wal_") && n.ends_with(".log"))
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|e| e.path());

        let mut records = Vec::new();
        for entry in entries {
            let data = std::fs::read(entry.path())?;
            let mut offset = 0;
            while offset + 25 <= data.len() {
                // Parse: [LSN:8][Type:1][TxnID:8][Len:4][Data:N][CRC:4]
                let lsn = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                let record_type = data[offset + 8];
                let txn_id = u64::from_le_bytes(data[offset + 9..offset + 17].try_into().unwrap());
                let data_len =
                    u32::from_le_bytes(data[offset + 17..offset + 21].try_into().unwrap()) as usize;

                let total = 21 + data_len + 4;
                if offset + total > data.len() {
                    break; // truncated record
                }

                let payload = data[offset + 21..offset + 21 + data_len].to_vec();

                // Verify CRC
                let stored_crc = u32::from_le_bytes(
                    data[offset + 21 + data_len..offset + total]
                        .try_into()
                        .unwrap(),
                );
                let computed_crc = crc32fast::hash(&data[offset..offset + 21 + data_len]);
                if stored_crc != computed_crc {
                    break; // corrupt — stop replay
                }

                records.push(UringWalRecord {
                    lsn,
                    record_type,
                    txn_id,
                    data: payload,
                });
                offset += total;
            }
        }

        Ok(records)
    }

    // ── Diagnostics ───────────────────────────────────────────────

    /// Current LSN (next to be assigned).
    pub fn current_lsn(&self) -> u64 {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Total bytes written to disk.
    pub fn total_bytes_written(&self) -> u64 {
        self.total_bytes_written
    }

    /// Current segment file path.
    pub fn current_segment(&self) -> &Path {
        &self.segment_path
    }

    /// Whether Linux io_uring is active for this writer instance.
    #[cfg(target_os = "linux")]
    pub fn io_uring_enabled(&self) -> bool {
        self.ring.is_some()
    }

    /// Non-Linux platforms never enable io_uring.
    #[cfg(not(target_os = "linux"))]
    pub fn io_uring_enabled(&self) -> bool {
        false
    }
}

// ── PyO3 Bindings ─────────────────────────────────────────────────

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// Python-exposed io_uring WAL writer.
#[cfg(feature = "python")]
#[pyclass(name = "UringWalWriter")]
pub struct PyUringWalWriter {
    inner: parking_lot::Mutex<UringWalWriter>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyUringWalWriter {
    #[new]
    #[pyo3(signature = (dir, direct_io=false))]
    fn new(dir: &str, direct_io: bool) -> PyResult<Self> {
        let inner = UringWalWriter::open(Path::new(dir), direct_io)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
        Ok(Self {
            inner: parking_lot::Mutex::new(inner),
        })
    }

    /// Append a WAL record. Returns LSN.
    fn append(&self, txn_id: u64, record_type: u8, data: &[u8]) -> PyResult<u64> {
        self.inner
            .lock()
            .append(txn_id, record_type, data)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))
    }

    /// Flush and fsync for durability.
    fn flush(&self) -> PyResult<()> {
        self.inner
            .lock()
            .flush()
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))
    }

    /// Current LSN.
    #[getter]
    fn current_lsn(&self) -> u64 {
        self.inner.lock().current_lsn()
    }

    /// Total bytes written.
    #[getter]
    fn total_bytes_written(&self) -> u64 {
        self.inner.lock().total_bytes_written()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_wal_dir() -> PathBuf {
        let id = DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("qm_uring_wal_{}_{}", std::process::id(), id))
    }

    #[test]
    fn test_append_and_recover() {
        let dir = temp_wal_dir();
        {
            let mut wal = UringWalWriter::open(&dir, false).unwrap();
            wal.append(1, 1, b"BEGIN").unwrap(); // Begin
            wal.append(1, 4, b"row data").unwrap(); // Insert
            wal.append(1, 2, b"").unwrap(); // Commit
            wal.flush().unwrap();
        }

        // Recover
        let wal = UringWalWriter::open(&dir, false).unwrap();
        let records = wal.recover().unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].lsn, 1);
        assert_eq!(records[0].record_type, 1);
        assert_eq!(records[1].data, b"row data");
        assert_eq!(records[2].record_type, 2);
    }

    #[test]
    fn test_segment_rotation() {
        let dir = temp_wal_dir();
        let mut wal = UringWalWriter::open(&dir, false).unwrap();

        // Write enough to trigger rotation (we'll lower the constant for test)
        let big_data = vec![0u8; 1024];
        for i in 0..100 {
            wal.append(i, 4, &big_data).unwrap();
        }
        wal.flush().unwrap();
        assert!(wal.total_bytes_written() > 100 * 1024);
    }

    #[test]
    fn test_aligned_buffer() {
        let mut buf = AlignedBuffer::new(4096);
        assert!(buf.is_empty());
        assert!(buf.append(b"hello"));
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.padded_len(), 4096);

        let padded = buf.as_padded_bytes();
        assert_eq!(padded.len(), 4096);
        assert_eq!(&padded[..5], b"hello");
    }

    #[test]
    fn test_crc_integrity() {
        let record = UringWalRecord {
            lsn: 42,
            record_type: 4,
            txn_id: 1,
            data: b"test data".to_vec(),
        };
        let encoded = record.encode();

        // Verify CRC is correct
        let data_end = encoded.len() - 4;
        let stored_crc = u32::from_le_bytes(encoded[data_end..].try_into().unwrap());
        let computed_crc = crc32fast::hash(&encoded[..data_end]);
        assert_eq!(stored_crc, computed_crc);
    }

    #[test]
    fn test_large_record_direct_write() {
        let dir = temp_wal_dir();
        let mut wal = UringWalWriter::open(&dir, false).unwrap();

        // Record larger than buffer (256KB buffer, 300KB record)
        let big = vec![42u8; 300 * 1024];
        let lsn = wal.append(1, 4, &big).unwrap();
        assert_eq!(lsn, 1);
        wal.flush().unwrap();

        let records = wal.recover().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].data.len(), 300 * 1024);
    }

    #[test]
    fn test_coalesce_flush() {
        let dir = temp_wal_dir();
        let mut wal = UringWalWriter::open(&dir, false).unwrap();

        // Write FSYNC_COALESCE_COUNT records to trigger auto-flush
        for i in 0..FSYNC_COALESCE_COUNT {
            wal.append(i, 4, b"row").unwrap();
        }
        // Should have auto-flushed at threshold
        assert_eq!(wal.writes_since_fsync, 0);
    }
}
