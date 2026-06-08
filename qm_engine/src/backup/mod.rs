//! QMvir Backup & Migration Suite
//!
//! Five backup/migration tools operating directly on `NativeSqlEngine` internals:
//! - `BackupEngine`  — logical + physical backup to `.qmvb`
//! - `DiffEngine`    — LSN-based differential/delta backup
//! - `RestoreEngine` — restore from `.qmvb`/`.qmdiff`/pg_dump
//! - `PredictEngine` — dry-run estimation
//! - `VerifyEngine`  — integrity + safety checks
//!
//! All tools work in-process (no TCP/IPC), serializing data via bincode + optional
//! compression (Zstd or LZ4). File integrity is ensured by CRC32 footer and optional
//! HMAC-SHA256 (key via `QM_SNAPSHOT_HMAC_KEY` env var).

pub mod backup;
pub mod encrypt;
pub mod format;
pub mod pg_compat;
pub mod predict;
#[cfg(feature = "python")]
pub mod pyo3;
pub mod restore;
pub mod snapshot_diff;
pub mod verify;

use std::io;

// ── Compression ──────────────────────────────────────────────────────

/// Compression algorithm for backup data chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum Compression {
    None = 0,
    Lz4 = 1,
    Zstd = 2,
}

impl Compression {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Compression::None),
            1 => Some(Compression::Lz4),
            2 => Some(Compression::Zstd),
            _ => None,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "none" => Some(Compression::None),
            "lz4" => Some(Compression::Lz4),
            "zstd" | "zstandard" => Some(Compression::Zstd),
            _ => None,
        }
    }
}

/// Compress a byte slice using the specified algorithm.
pub fn compress(data: &[u8], algo: Compression) -> io::Result<Vec<u8>> {
    match algo {
        Compression::None => Ok(data.to_vec()),
        Compression::Lz4 => Ok(lz4_flex::compress_prepend_size(data)),
        Compression::Zstd => {
            zstd::bulk::compress(data, 3).map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
    }
}

/// Decompress a byte slice using the specified algorithm.
pub fn decompress(data: &[u8], algo: Compression) -> io::Result<Vec<u8>> {
    match algo {
        Compression::None => Ok(data.to_vec()),
        Compression::Lz4 => lz4_flex::decompress_size_prepended(data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        Compression::Zstd => zstd::stream::decode_all(std::io::Cursor::new(data))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

// ── Backup Type ──────────────────────────────────────────────────────

/// Type of backup stored in .qmvb header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum BackupType {
    Full = 0,
    Differential = 1,
    Table = 2,
    Incremental = 3,
}

impl BackupType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(BackupType::Full),
            1 => Some(BackupType::Differential),
            2 => Some(BackupType::Table),
            3 => Some(BackupType::Incremental),
            _ => None,
        }
    }
}

// ── Chunk size ───────────────────────────────────────────────────────

/// Number of rows per chunk in backup data blocks.
/// Chosen for good parallelism with rayon + reasonable compression ratio.
pub const CHUNK_SIZE: usize = 1024;

// ── HMAC key ─────────────────────────────────────────────────────────

/// Read the HMAC key from `QM_SNAPSHOT_HMAC_KEY` env var (same as snapshot.rs).
pub fn hmac_key() -> Option<Vec<u8>> {
    std::env::var("QM_SNAPSHOT_HMAC_KEY")
        .ok()
        .map(|k| k.into_bytes())
}
