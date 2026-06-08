//! `.qmvb` binary file format — header, manifest, footer.
//!
//! Layout:
//!   [Header 64B] [Manifest (variable)] [Table data blocks] [WAL segment (optional)] [Footer 40B]
//!
//! Footer CRC32 covers everything from byte 0 to just before the footer.

use crate::backup::{BackupType, Compression};
use serde::{Deserialize, Serialize};
use std::io::{self, Cursor, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Magic constants ──────────────────────────────────────────────────

/// File magic: "QMVB\x00\x00\x00\x01" — identifies a .qmvb file.
pub const BACKUP_MAGIC: u64 = 0x514D_5642_0000_0001;
/// Footer magic: "QMEND!" — marks valid EOF.
pub const FOOTER_MAGIC: u32 = 0x514D_454E;
/// Current format version.
pub const FORMAT_VERSION: u32 = 1;

// ── Header ───────────────────────────────────────────────────────────

/// Fixed 64-byte header at the start of every .qmvb file.
#[derive(Debug, Clone)]
pub struct BackupHeader {
    pub magic: u64,
    pub format_version: u32,
    pub backup_type: BackupType,
    pub compression: Compression,
    pub base_lsn: u64,
    pub end_lsn: u64,
    pub timestamp: u64,
    pub table_count: u32,
    pub total_rows: u64,
    pub original_size: u64,
}

impl BackupHeader {
    pub const SIZE: usize = 64;

    /// Create a new header with current timestamp.
    pub fn new(
        backup_type: BackupType,
        compression: Compression,
        base_lsn: u64,
        end_lsn: u64,
        table_count: u32,
        total_rows: u64,
        original_size: u64,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            magic: BACKUP_MAGIC,
            format_version: FORMAT_VERSION,
            backup_type,
            compression,
            base_lsn,
            end_lsn,
            timestamp,
            table_count,
            total_rows,
            original_size,
        }
    }

    /// Serialize header to exactly 64 bytes (little-endian).
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        let mut c = Cursor::new(&mut buf[..]);
        w_u64(&mut c, self.magic);
        w_u32(&mut c, self.format_version);
        w_u8(&mut c, self.backup_type as u8);
        w_u8(&mut c, self.compression as u8);
        w_u8(&mut c, 0); // reserved
        w_u8(&mut c, 0); // reserved
        w_u64(&mut c, self.base_lsn);
        w_u64(&mut c, self.end_lsn);
        w_u64(&mut c, self.timestamp);
        w_u32(&mut c, self.table_count);
        w_u64(&mut c, self.total_rows);
        w_u64(&mut c, self.original_size);
        // 2 bytes padding (already zeroed)
        buf
    }

    /// Deserialize header from exactly 64 bytes.
    pub fn from_bytes(buf: &[u8; Self::SIZE]) -> io::Result<Self> {
        let mut c = Cursor::new(buf);
        let magic = r_u64(&mut c)?;
        if magic != BACKUP_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid backup magic: {:#x}", magic),
            ));
        }
        let format_version = r_u32(&mut c)?;
        let backup_type = BackupType::from_u8(r_u8(&mut c)?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Unknown backup type"))?;
        let compression = Compression::from_u8(r_u8(&mut c)?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Unknown compression"))?;
        let _r1 = r_u8(&mut c)?;
        let _r2 = r_u8(&mut c)?;
        let base_lsn = r_u64(&mut c)?;
        let end_lsn = r_u64(&mut c)?;
        let timestamp = r_u64(&mut c)?;
        let table_count = r_u32(&mut c)?;
        let total_rows = r_u64(&mut c)?;
        let original_size = r_u64(&mut c)?;

        Ok(Self {
            magic,
            format_version,
            backup_type,
            compression,
            base_lsn,
            end_lsn,
            timestamp,
            table_count,
            total_rows,
            original_size,
        })
    }
}

// ── Manifest ─────────────────────────────────────────────────────────

/// JSON manifest describing tables in the backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub tables: Vec<TableManifestEntry>,
}

/// Per-table metadata in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableManifestEntry {
    pub name: String,
    pub columns: Vec<String>,
    pub types: Vec<String>,
    pub row_count: u64,
    pub chunk_count: u32,
    pub data_offset: u64,
    pub data_len: u64,
}

impl BackupManifest {
    /// Serialize manifest to length-prefixed JSON bytes.
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let json = serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut out = Vec::with_capacity(4 + json.len());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        Ok(out)
    }

    /// Deserialize manifest from length-prefixed JSON bytes.
    pub fn from_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > 64 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Manifest too large",
            ));
        }
        let mut json_buf = vec![0u8; len];
        r.read_exact(&mut json_buf)?;
        serde_json::from_slice(&json_buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

// ── Footer ───────────────────────────────────────────────────────────

/// Fixed 40-byte footer at the end of every .qmvb file.
#[derive(Debug, Clone)]
pub struct BackupFooter {
    /// CRC32 of all bytes from header to just before footer.
    pub crc32: u32,
    /// HMAC-SHA256 of all bytes (zeroed if no key set).
    pub hmac_sha256: [u8; 32],
    /// Footer magic for validation.
    pub footer_magic: u32,
}

impl BackupFooter {
    pub const SIZE: usize = 40;

    pub fn new(crc32: u32, hmac_sha256: [u8; 32]) -> Self {
        Self {
            crc32,
            hmac_sha256,
            footer_magic: FOOTER_MAGIC,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.crc32.to_le_bytes());
        buf[4..36].copy_from_slice(&self.hmac_sha256);
        buf[36..40].copy_from_slice(&self.footer_magic.to_le_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8; Self::SIZE]) -> io::Result<Self> {
        let crc32 = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let mut hmac = [0u8; 32];
        hmac.copy_from_slice(&buf[4..36]);
        let magic = u32::from_le_bytes(buf[36..40].try_into().unwrap());
        if magic != FOOTER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid footer magic: {:#x}", magic),
            ));
        }
        Ok(Self {
            crc32,
            hmac_sha256: hmac,
            footer_magic: magic,
        })
    }
}

// ── Little-endian I/O helpers ────────────────────────────────────────

fn w_u8<W: Write>(w: &mut W, v: u8) {
    let _ = w.write_all(&[v]);
}
fn w_u32<W: Write>(w: &mut W, v: u32) {
    let _ = w.write_all(&v.to_le_bytes());
}
fn w_u64<W: Write>(w: &mut W, v: u64) {
    let _ = w.write_all(&v.to_le_bytes());
}

fn r_u8<R: Read>(r: &mut R) -> io::Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}
fn r_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}
fn r_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = BackupHeader::new(
            BackupType::Full,
            Compression::Zstd,
            0,
            1000,
            5,
            50000,
            1024 * 1024,
        );
        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), BackupHeader::SIZE);
        let h2 = BackupHeader::from_bytes(&bytes).unwrap();
        assert_eq!(h2.magic, BACKUP_MAGIC);
        assert_eq!(h2.backup_type as u8, BackupType::Full as u8);
        assert_eq!(h2.compression as u8, Compression::Zstd as u8);
        assert_eq!(h2.end_lsn, 1000);
        assert_eq!(h2.table_count, 5);
        assert_eq!(h2.total_rows, 50000);
    }

    #[test]
    fn footer_roundtrip() {
        let f = BackupFooter::new(0xDEADBEEF, [0xAB; 32]);
        let bytes = f.to_bytes();
        assert_eq!(bytes.len(), BackupFooter::SIZE);
        let f2 = BackupFooter::from_bytes(&bytes).unwrap();
        assert_eq!(f2.crc32, 0xDEADBEEF);
        assert_eq!(f2.hmac_sha256, [0xAB; 32]);
    }

    #[test]
    fn manifest_roundtrip() {
        let m = BackupManifest {
            tables: vec![TableManifestEntry {
                name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                types: vec!["Integer".into(), "Text".into()],
                row_count: 100,
                chunk_count: 1,
                data_offset: 0,
                data_len: 500,
            }],
        };
        let bytes = m.to_bytes().unwrap();
        let mut cursor = Cursor::new(&bytes);
        let m2 = BackupManifest::from_reader(&mut cursor).unwrap();
        assert_eq!(m2.tables.len(), 1);
        assert_eq!(m2.tables[0].name, "users");
        assert_eq!(m2.tables[0].row_count, 100);
    }

    #[test]
    fn invalid_magic_rejected() {
        let mut bytes = [0u8; BackupHeader::SIZE];
        bytes[0..8].copy_from_slice(&0xBAD_CAFE_u64.to_le_bytes());
        assert!(BackupHeader::from_bytes(&bytes).is_err());
    }
}
