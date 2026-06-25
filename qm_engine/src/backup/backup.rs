//! `BackupEngine` — logical + physical backup to `.qmvb` format.
//!
//! Serializes table data directly from `NativeSqlEngine` internals via
//! bincode (same format as checkpoint), compressed in chunks, with CRC32
//! + optional HMAC-SHA256 footer for integrity verification.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::backup::format::*;
use crate::backup::{compress, hmac_key, BackupType, Compression, CHUNK_SIZE};
use crate::gateway::native_sql::NativeSqlEngine;

type HmacSha256 = Hmac<Sha256>;

// ── Config & Result ──────────────────────────────────────────────────

/// Configuration for a backup operation.
pub struct BackupConfig {
    /// Specific tables to back up (None = all tables).
    pub tables: Option<Vec<String>>,
    /// Compression algorithm.
    pub compression: Compression,
    /// Whether to include WAL data for physical / PITR backup.
    pub include_wal: bool,
    /// Output file path (.qmvb).
    pub output: PathBuf,
}

/// Result of a completed backup.
#[derive(Debug, Serialize)]
pub struct BackupResult {
    pub path: String,
    pub tables_backed_up: usize,
    pub total_rows: u64,
    pub original_size: u64,
    pub compressed_size: u64,
    pub duration_ms: u64,
    pub crc32: u32,
}

// ── BackupEngine ─────────────────────────────────────────────────────

pub struct BackupEngine<'a> {
    engine: &'a NativeSqlEngine,
}

impl<'a> BackupEngine<'a> {
    pub fn new(engine: &'a NativeSqlEngine) -> Self {
        Self { engine }
    }

    /// Create a backup according to the given config.
    ///
    /// 1. Acquire read lock on tables (non-blocking for writers already committed).
    /// 2. Build manifest from table metadata.
    /// 3. For each table: serialize rows in chunks, compress, write.
    /// 4. Optionally append WAL file contents.
    /// 5. Compute CRC32 + HMAC-SHA256 footer.
    /// 6. Atomic write: .qmvb.tmp → rename → .qmvb
    pub fn run(&self, config: &BackupConfig) -> io::Result<BackupResult> {
        let start = Instant::now();
        let tmp_path = config.output.with_extension("qmvb.tmp");

        // M-09: Create an incomplete marker file to detect interrupted backups.
        let marker_path = config.output.with_extension("qmvb.incomplete");
        fs::write(&marker_path, format!("backup started at {:?}", start))?;

        let tables_guard = self.engine.tables.read().unwrap();

        // Filter tables if specific ones requested.
        let table_names: Vec<&String> = if let Some(ref names) = config.tables {
            names
                .iter()
                .filter(|n| tables_guard.contains_key(n.as_str()))
                .collect()
        } else {
            tables_guard.keys().collect()
        };

        // Serialize all table data into chunks and build manifest entries.
        let mut table_entries = Vec::new();
        let mut all_chunks: Vec<(String, Vec<Vec<u8>>)> = Vec::new(); // (table_name, compressed_chunks)
        let mut total_rows = 0u64;
        let mut original_size = 0u64;

        for table_name in &table_names {
            let table = &tables_guard[table_name.as_str()];
            let row_count = table.rows.len() as u64;
            total_rows += row_count;

            // Serialize rows in chunks of CHUNK_SIZE.
            let rows_vec: Vec<_> = table.rows.values().collect();
            let mut compressed_chunks = Vec::new();
            let mut table_data_len = 0u64;

            for chunk in rows_vec.chunks(CHUNK_SIZE) {
                let raw = bincode::serialize(&chunk)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                original_size += raw.len() as u64;
                let compressed = compress(&raw, config.compression)?;
                table_data_len += 8 + compressed.len() as u64; // original_len(4) + compressed_len(4) + data
                compressed_chunks.push(compressed);
            }

            let types: Vec<String> = table
                .column_types
                .iter()
                .map(crate::backup::format::col_type_to_manifest)
                .collect();

            table_entries.push(TableManifestEntry {
                name: table_name.to_string(),
                columns: table.columns.clone(),
                types,
                row_count,
                chunk_count: compressed_chunks.len() as u32,
                data_offset: 0, // will be computed during write
                data_len: table_data_len,
            });

            all_chunks.push((table_name.to_string(), compressed_chunks));
        }

        // Get current LSN for header.
        let end_lsn = self.engine.row_lsn_counter.load(Ordering::SeqCst);

        // Build header.
        let header = BackupHeader::new(
            BackupType::Full,
            config.compression,
            0, // base_lsn for full backup
            end_lsn,
            table_names.len() as u32,
            total_rows,
            original_size,
        );

        // Build manifest.
        let manifest = BackupManifest {
            tables: table_entries,
        };

        // Drop read lock — all data has been serialized.
        drop(tables_guard);

        // Write to tmp file.
        let file = fs::File::create(&tmp_path)?;
        let mut w = BufWriter::new(file);
        let mut crc_hasher = crc32fast::Hasher::new();

        // Write header.
        let header_bytes = header.to_bytes();
        w.write_all(&header_bytes)?;
        crc_hasher.update(&header_bytes);

        // Write manifest.
        let manifest_bytes = manifest.to_bytes()?;
        w.write_all(&manifest_bytes)?;
        crc_hasher.update(&manifest_bytes);

        // Write table data blocks.
        for (table_name, chunks) in &all_chunks {
            let name_bytes = table_name.as_bytes();
            let name_len = (name_bytes.len() as u16).to_le_bytes();
            w.write_all(&name_len)?;
            crc_hasher.update(&name_len);
            w.write_all(name_bytes)?;
            crc_hasher.update(name_bytes);

            let chunk_count = (chunks.len() as u32).to_le_bytes();
            w.write_all(&chunk_count)?;
            crc_hasher.update(&chunk_count);

            for chunk_data in chunks {
                // We don't store original_len separately here since lz4_flex prepends it.
                // For format compat, we store compressed_len as u32.
                let comp_len = (chunk_data.len() as u32).to_le_bytes();
                w.write_all(&comp_len)?;
                crc_hasher.update(&comp_len);
                w.write_all(chunk_data)?;
                crc_hasher.update(chunk_data);
            }
        }

        // Write WAL segment (optional).
        if config.include_wal {
            if let Some(ref data_dir) = self.engine.data_dir {
                let wal_path = data_dir.join("native_sql.wal");
                if wal_path.exists() {
                    let wal_data = fs::read(&wal_path)?;
                    let present = [1u8];
                    w.write_all(&present)?;
                    crc_hasher.update(&present);
                    let wal_len = (wal_data.len() as u64).to_le_bytes();
                    w.write_all(&wal_len)?;
                    crc_hasher.update(&wal_len);
                    w.write_all(&wal_data)?;
                    crc_hasher.update(&wal_data);
                } else {
                    let absent = [0u8];
                    w.write_all(&absent)?;
                    crc_hasher.update(&absent);
                }
            } else {
                let absent = [0u8];
                w.write_all(&absent)?;
                crc_hasher.update(&absent);
            }
        } else {
            let absent = [0u8];
            w.write_all(&absent)?;
            crc_hasher.update(&absent);
        }

        // Compute footer.
        let crc32 = crc_hasher.finalize();
        let hmac_bytes = if let Some(key) = hmac_key() {
            // Recompute HMAC over the same data (header + manifest + chunks + WAL marker).
            // For simplicity, re-read the tmp file for HMAC computation.
            // In production, we'd maintain a streaming HMAC alongside CRC.
            let mut mac = HmacSha256::new_from_slice(&key)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "Invalid HMAC key"))?;
            w.flush()?;
            // Read tmp file for HMAC (before footer is written).
            let tmp_data = fs::read(&tmp_path)?;
            mac.update(&tmp_data);
            let result = mac.finalize();
            let mut hmac = [0u8; 32];
            hmac.copy_from_slice(&result.into_bytes());
            hmac
        } else {
            [0u8; 32]
        };

        let footer = BackupFooter::new(crc32, hmac_bytes);
        w.write_all(&footer.to_bytes())?;
        w.flush()?;
        drop(w);

        // Atomic rename.
        fs::rename(&tmp_path, &config.output)?;

        // M-09: Remove incomplete marker on successful completion.
        let _ = fs::remove_file(&marker_path);

        let compressed_size = fs::metadata(&config.output)?.len();
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(BackupResult {
            path: config.output.display().to_string(),
            tables_backed_up: all_chunks.len(),
            total_rows,
            original_size,
            compressed_size,
            duration_ms,
            crc32,
        })
    }
}
