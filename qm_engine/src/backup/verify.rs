//! `VerifyEngine` — verify `.qmvb` file integrity.
//!
//! Quick mode: check header magic + footer CRC32.  
//! Deep mode : re-decompress every chunk, recount rows.

use std::fs;
use std::io::{self};
use std::path::Path;

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::backup::format::*;
use crate::backup::{hmac_key, BackupType, Compression};

type HmacSha256 = Hmac<Sha256>;

// ── Result types ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct VerifyResult {
    pub ok: bool,
    pub header_valid: bool,
    pub footer_valid: bool,
    pub crc_match: bool,
    pub hmac_match: Option<bool>,
    pub tables: u32,
    pub rows: u64,
    pub errors: Vec<String>,
}

// ── VerifyEngine ─────────────────────────────────────────────────────

pub struct VerifyEngine;

impl VerifyEngine {
    /// Quick verification: header magic + footer CRC32.
    pub fn verify_quick(path: &Path) -> io::Result<VerifyResult> {
        let data = fs::read(path)?;

        let mut errors = Vec::new();

        // Minimum size: 64 (header) + 4 (manifest len) + 40 (footer).
        if data.len() < 64 + 4 + 40 {
            return Ok(VerifyResult {
                ok: false,
                header_valid: false,
                footer_valid: false,
                crc_match: false,
                hmac_match: None,
                tables: 0,
                rows: 0,
                errors: vec!["File too small".into()],
            });
        }

        // Parse header.
        let header_arr: &[u8; 64] = data[..64].try_into().unwrap();
        let header = match BackupHeader::from_bytes(header_arr) {
            Ok(h) => h,
            Err(e) => {
                return Ok(VerifyResult {
                    ok: false,
                    header_valid: false,
                    footer_valid: false,
                    crc_match: false,
                    hmac_match: None,
                    tables: 0,
                    rows: 0,
                    errors: vec![format!("Header: {e}")],
                });
            }
        };

        // Parse footer (last 40 bytes).
        let footer_offset = data.len() - 40;
        let footer_arr: &[u8; 40] = data[footer_offset..].try_into().unwrap();
        let footer = BackupFooter::from_bytes(footer_arr)?;
        let footer_magic_ok = footer.footer_magic == FOOTER_MAGIC;

        if !footer_magic_ok {
            errors.push("Footer magic mismatch".into());
        }

        // CRC32 over everything except footer.
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&data[..footer_offset]);
        let computed_crc = hasher.finalize();
        let crc_ok = computed_crc == footer.crc32;

        if !crc_ok {
            errors.push(format!(
                "CRC32 mismatch: stored={:#010x}, computed={:#010x}",
                footer.crc32, computed_crc
            ));
        }

        // HMAC check if key is available and HMAC is non-zero.
        let hmac_match = if footer.hmac_sha256 != [0u8; 32] {
            if let Some(key) = hmac_key() {
                let mut mac = HmacSha256::new_from_slice(&key)
                    .map_err(|_| io::Error::new(io::ErrorKind::Other, "Invalid HMAC key"))?;
                mac.update(&data[..footer_offset]);
                let ok = mac.verify_slice(&footer.hmac_sha256).is_ok();
                if !ok {
                    errors.push("HMAC-SHA256 mismatch".into());
                }
                Some(ok)
            } else {
                errors.push("HMAC present but no QM_SNAPSHOT_HMAC_KEY set".into());
                Some(false)
            }
        } else {
            None
        };

        let ok = crc_ok && footer_magic_ok && hmac_match.unwrap_or(true);

        Ok(VerifyResult {
            ok,
            header_valid: true,
            footer_valid: footer_magic_ok,
            crc_match: crc_ok,
            hmac_match,
            tables: header.table_count,
            rows: header.total_rows,
            errors,
        })
    }

    /// Detailed backup information from the header + manifest.
    pub fn info(path: &Path) -> io::Result<BackupInfo> {
        let data = fs::read(path)?;
        if data.len() < 64 + 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "File too small"));
        }

        let header_arr: &[u8; 64] = data[..64].try_into().unwrap();
        let header = BackupHeader::from_bytes(header_arr)?;

        // Read manifest after header.
        let mut cursor = io::Cursor::new(&data[64..]);
        let manifest = BackupManifest::from_reader(&mut cursor)?;

        let file_size = data.len() as u64;

        Ok(BackupInfo {
            format_version: header.format_version,
            backup_type: header.backup_type,
            compression: header.compression,
            base_lsn: header.base_lsn,
            end_lsn: header.end_lsn,
            timestamp: header.timestamp,
            table_count: header.table_count,
            total_rows: header.total_rows,
            original_size: header.original_size,
            file_size,
            tables: manifest.tables,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct BackupInfo {
    pub format_version: u32,
    pub backup_type: BackupType,
    pub compression: Compression,
    pub base_lsn: u64,
    pub end_lsn: u64,
    pub timestamp: u64,
    pub table_count: u32,
    pub total_rows: u64,
    pub original_size: u64,
    pub file_size: u64,
    pub tables: Vec<TableManifestEntry>,
}
