/*
 * Write-Ahead Logging (WAL) - Durability guarantees
 *
 * WAL record format:
 * [LSN:8][Type:1][TxnID:8][Length:4][Data:*][CRC32:4]
 */

use crc32fast::Hasher;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// WAL record types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WalRecordType {
    Begin = 1,
    Commit = 2,
    Rollback = 3,
    Insert = 4,
    Update = 5,
    Delete = 6,
    Checkpoint = 7,
}

impl TryFrom<u8> for WalRecordType {
    type Error = io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(WalRecordType::Begin),
            2 => Ok(WalRecordType::Commit),
            3 => Ok(WalRecordType::Rollback),
            4 => Ok(WalRecordType::Insert),
            5 => Ok(WalRecordType::Update),
            6 => Ok(WalRecordType::Delete),
            7 => Ok(WalRecordType::Checkpoint),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid WAL record type",
            )),
        }
    }
}

/// WAL record
#[derive(Debug)]
pub struct WalRecord {
    pub lsn: u64,
    pub record_type: WalRecordType,
    pub txn_id: u64,
    pub data: Vec<u8>,
}

impl WalRecord {
    /// Encode record to bytes
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(25 + self.data.len());

        // LSN (8 bytes)
        buf.extend_from_slice(&self.lsn.to_le_bytes());

        // Type (1 byte)
        buf.push(self.record_type as u8);

        // Transaction ID (8 bytes)
        buf.extend_from_slice(&self.txn_id.to_le_bytes());

        // Data length (4 bytes)
        buf.extend_from_slice(&(self.data.len() as u32).to_le_bytes());

        // Data
        buf.extend_from_slice(&self.data);

        // CRC32 (4 bytes)
        let mut hasher = Hasher::new();
        hasher.update(&buf);
        let crc = hasher.finalize();
        buf.extend_from_slice(&crc.to_le_bytes());

        buf
    }

    /// Decode record from bytes
    pub fn decode(buf: &[u8]) -> io::Result<(Self, usize)> {
        if buf.len() < 25 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "WAL record too short",
            ));
        }

        let lsn = u64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]);

        let record_type = WalRecordType::try_from(buf[8])?;

        let txn_id = u64::from_le_bytes([
            buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16],
        ]);

        let data_len = u32::from_le_bytes([buf[17], buf[18], buf[19], buf[20]]) as usize;

        let total_len = 21 + data_len + 4;
        if buf.len() < total_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "WAL record incomplete",
            ));
        }

        let data = buf[21..21 + data_len].to_vec();

        // Verify CRC
        let stored_crc = u32::from_le_bytes([
            buf[21 + data_len],
            buf[22 + data_len],
            buf[23 + data_len],
            buf[24 + data_len],
        ]);

        let mut hasher = Hasher::new();
        hasher.update(&buf[..21 + data_len]);
        let computed_crc = hasher.finalize();

        if stored_crc != computed_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAL record CRC mismatch",
            ));
        }

        Ok((
            WalRecord {
                lsn,
                record_type,
                txn_id,
                data,
            },
            total_len,
        ))
    }
}

/// WAL segment file
pub struct WalSegment {
    file: BufWriter<File>,
    start_lsn: u64,
    current_size: usize,
    max_size: usize,
}

impl WalSegment {
    pub fn create(dir: &PathBuf, segment_id: u64, max_size: usize) -> io::Result<Self> {
        let path = dir.join(format!("wal_{:016x}.log", segment_id));
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(&path)?;

        Ok(Self {
            file: BufWriter::with_capacity(64 * 1024, file),
            start_lsn: segment_id,
            current_size: 0,
            max_size,
        })
    }

    pub fn write(&mut self, record: &WalRecord) -> io::Result<()> {
        let data = record.encode();
        self.file.write_all(&data)?;
        self.current_size += data.len();
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_all()
    }

    pub fn is_full(&self) -> bool {
        self.current_size >= self.max_size
    }
}

/// WAL writer with double buffering
pub struct WalWriter {
    dir: PathBuf,
    current_segment: WalSegment,
    next_lsn: AtomicU64,
    segment_max_size: usize,
    buffer: Vec<u8>,
    buffer_capacity: usize,
    /// M-03: Track segment creation time for time-based rotation.
    segment_created_at: std::time::Instant,
    /// Maximum segment age before rotation.
    segment_max_age: Duration,
}

impl WalWriter {
    pub fn new(dir: PathBuf, buffer_capacity: usize) -> io::Result<Self> {
        let segment_max_size = 64 * 1024 * 1024; // 64MB segments
        let current_segment = WalSegment::create(&dir, 0, segment_max_size)?;

        Ok(Self {
            dir,
            current_segment,
            next_lsn: AtomicU64::new(1),
            segment_max_size,
            buffer: Vec::with_capacity(buffer_capacity),
            buffer_capacity,
            segment_created_at: std::time::Instant::now(),
            segment_max_age: Duration::from_secs(300), // Rotate every 5 minutes
        })
    }

    /// Get the WAL directory path.
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    /// Get next LSN
    fn get_lsn(&self) -> u64 {
        self.next_lsn.fetch_add(1, Ordering::SeqCst)
    }

    /// Write BEGIN record
    pub fn write_begin(&mut self, txn_id: u64) -> io::Result<u64> {
        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Begin,
            txn_id,
            data: Vec::new(),
        };
        self.write_record(&record)?;
        Ok(record.lsn)
    }

    /// Write COMMIT record
    pub fn write_commit(&mut self, txn_id: u64) -> io::Result<u64> {
        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Commit,
            txn_id,
            data: Vec::new(),
        };
        self.write_record(&record)?;
        self.flush()?; // Ensure commit is durable
        Ok(record.lsn)
    }

    /// Write COMMIT record without flushing (for group commit).
    /// Caller MUST call `flush()` after the batch to ensure durability.
    pub fn write_commit_deferred(&mut self, txn_id: u64) -> io::Result<u64> {
        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Commit,
            txn_id,
            data: Vec::new(),
        };
        self.write_record(&record)?;
        Ok(record.lsn)
    }

    /// Write ROLLBACK record
    pub fn write_rollback(&mut self, txn_id: u64) -> io::Result<u64> {
        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Rollback,
            txn_id,
            data: Vec::new(),
        };
        self.write_record(&record)?;
        Ok(record.lsn)
    }

    /// Write INSERT record
    pub fn write_insert(&mut self, txn_id: u64, table: &str, data: &[u8]) -> io::Result<u64> {
        let mut record_data = Vec::new();
        record_data.extend_from_slice(&(table.len() as u16).to_le_bytes());
        record_data.extend_from_slice(table.as_bytes());
        record_data.extend_from_slice(data);

        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Insert,
            txn_id,
            data: record_data,
        };
        self.write_record(&record)?;
        Ok(record.lsn)
    }

    /// Write UPDATE record
    pub fn write_update(
        &mut self,
        txn_id: u64,
        table: &str,
        key: &[u8],
        old: &[u8],
        new: &[u8],
    ) -> io::Result<u64> {
        let mut record_data = Vec::new();
        record_data.extend_from_slice(&(table.len() as u16).to_le_bytes());
        record_data.extend_from_slice(table.as_bytes());
        record_data.extend_from_slice(&(key.len() as u32).to_le_bytes());
        record_data.extend_from_slice(key);
        record_data.extend_from_slice(&(old.len() as u32).to_le_bytes());
        record_data.extend_from_slice(old);
        record_data.extend_from_slice(&(new.len() as u32).to_le_bytes());
        record_data.extend_from_slice(new);

        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Update,
            txn_id,
            data: record_data,
        };
        self.write_record(&record)?;
        Ok(record.lsn)
    }

    /// Write DELETE record
    pub fn write_delete(&mut self, txn_id: u64, table: &str, key: &[u8]) -> io::Result<u64> {
        let mut record_data = Vec::new();
        record_data.extend_from_slice(&(table.len() as u16).to_le_bytes());
        record_data.extend_from_slice(table.as_bytes());
        record_data.extend_from_slice(&(key.len() as u32).to_le_bytes());
        record_data.extend_from_slice(key);

        let record = WalRecord {
            lsn: self.get_lsn(),
            record_type: WalRecordType::Delete,
            txn_id,
            data: record_data,
        };
        self.write_record(&record)?;
        Ok(record.lsn)
    }

    /// Write record to buffer
    fn write_record(&mut self, record: &WalRecord) -> io::Result<()> {
        let encoded = record.encode();

        // Check if we need to flush buffer
        if self.buffer.len() + encoded.len() > self.buffer_capacity {
            self.flush_buffer()?;
        }

        self.buffer.extend_from_slice(&encoded);
        Ok(())
    }

    /// Flush buffer to segment — write raw bytes directly (no re-decoding)
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // M-03: Check both size-based AND time-based rotation.
        if self.current_segment.is_full()
            || self.segment_created_at.elapsed() > self.segment_max_age
        {
            self.rotate_segment()?;
        }

        // FIX Bug 1.5: Write pre-encoded bytes directly to segment file.
        // Records are already correctly encoded in self.buffer — no need to
        // decode and re-encode them.
        self.current_segment.file.write_all(&self.buffer)?;
        self.current_segment.current_size += self.buffer.len();
        self.current_segment.flush()?;
        self.buffer.clear();
        Ok(())
    }

    /// Rotate to new segment
    fn rotate_segment(&mut self) -> io::Result<()> {
        self.current_segment.sync()?;
        let new_segment_id = self.current_segment.start_lsn + self.segment_max_size as u64;
        self.current_segment =
            WalSegment::create(&self.dir, new_segment_id, self.segment_max_size)?;
        self.segment_created_at = std::time::Instant::now();
        Ok(())
    }

    /// Flush all buffers
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        self.current_segment.sync()
    }

    /// Recover from WAL
    pub fn recover(&self) -> io::Result<()> {
        // Read all WAL segments and replay
        let mut entries: Vec<_> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "log")
                    .unwrap_or(false)
            })
            .collect();

        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let mut file = File::open(entry.path())?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;

            let mut offset = 0;
            while offset < buf.len() {
                match WalRecord::decode(&buf[offset..]) {
                    Ok((_record, len)) => {
                        // Replay record (would update in-memory state)
                        offset += len;
                    }
                    Err(_) => break,
                }
            }
        }

        Ok(())
    }
}
