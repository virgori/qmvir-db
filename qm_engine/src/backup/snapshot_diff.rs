//! `DiffEngine` — compute differential / incremental change sets.
//!
//! Compares current engine state against a `.qmvb` baseline using
//! `last_modified_lsn` on rows to emit only changed data since the
//! base backup's `end_lsn`.

use serde::Serialize;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::backup::format::*;
use crate::backup::{compress, hmac_key, BackupType, Compression, CHUNK_SIZE};
use crate::gateway::native_sql::NativeSqlEngine;

/// Configuration for a differential backup.
pub struct DiffConfig {
    /// Path to the base `.qmvb` file to diff against.
    pub base_backup: PathBuf,
    /// Compression algorithm for the diff output.
    pub compression: Compression,
    /// Output path for the diff `.qmvb` file.
    pub output: PathBuf,
}

/// Result of a diff operation.
#[derive(Debug, Serialize)]
pub struct DiffResult {
    pub path: String,
    pub tables_changed: usize,
    pub rows_added: u64,
    pub rows_modified: u64,
    pub rows_deleted: u64,
    pub diff_size: u64,
    pub duration_ms: u64,
    pub base_lsn: u64,
    pub end_lsn: u64,
}

pub struct DiffEngine<'a> {
    engine: &'a NativeSqlEngine,
}

impl<'a> DiffEngine<'a> {
    pub fn new(engine: &'a NativeSqlEngine) -> Self {
        Self { engine }
    }

    /// Create a differential backup containing only rows changed since
    /// the base backup's `end_lsn`.
    ///
    /// Strategy:
    /// 1. Read base backup header → extract `end_lsn`.
    /// 2. For each table, collect rows where `last_modified_lsn > base_lsn`.
    /// 3. Collect tombstone entries (deleted rows) since `base_lsn`.
    /// 4. Write a standard `.qmvb` file with `backup_type = Differential`.
    pub fn create_diff(&self, config: &DiffConfig) -> io::Result<DiffResult> {
        let start = Instant::now();

        // 1. Read base backup header to get the end_lsn.
        let base_data = fs::read(&config.base_backup)?;
        if base_data.len() < BackupHeader::SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "base backup too small to contain a header",
            ));
        }
        let header_bytes: [u8; BackupHeader::SIZE] =
            base_data[..BackupHeader::SIZE].try_into().unwrap();
        let base_header = BackupHeader::from_bytes(&header_bytes)?;
        let base_lsn = base_header.end_lsn;

        // 2. Scan tables for changed rows.
        let tables_guard = self.engine.tables.to_native_map();
        let current_lsn = self.engine.row_lsn_counter.load(Ordering::SeqCst);

        let mut tables_changed = 0usize;
        let rows_added = 0u64;
        let mut rows_modified = 0u64;

        let mut table_entries = Vec::new();
        let mut all_chunks: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
        let mut total_rows = 0u64;
        let mut original_size = 0u64;

        for (name, table) in tables_guard.iter() {
            // Collect rows with LSN > base_lsn.
            let changed_rows: Vec<_> = table
                .rows
                .values()
                .filter(|r| r.last_modified_lsn > base_lsn)
                .collect();

            if changed_rows.is_empty() {
                continue;
            }

            tables_changed += 1;
            let row_count = changed_rows.len() as u64;
            total_rows += row_count;

            // Count added vs modified (heuristic: all are "modified" in diff context).
            rows_modified += row_count;

            // Serialize in chunks.
            let mut compressed_chunks = Vec::new();
            let mut table_data_len = 0u64;

            for chunk in changed_rows.chunks(CHUNK_SIZE) {
                let raw = bincode::serialize(&chunk)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                original_size += raw.len() as u64;
                let compressed = compress(&raw, config.compression)?;
                table_data_len += 4 + compressed.len() as u64;
                compressed_chunks.push(compressed);
            }

            let types: Vec<String> = table
                .column_types
                .iter()
                .map(|ct| format!("{:?}", ct))
                .collect();

            table_entries.push(TableManifestEntry {
                name: name.clone(),
                columns: table.columns.clone(),
                types,
                row_count,
                chunk_count: compressed_chunks.len() as u32,
                data_offset: 0,
                data_len: table_data_len,
            });

            all_chunks.push((name.clone(), compressed_chunks));
        }

        // 3. Collect tombstone entries (deleted since base_lsn).
        let tombstone_guard = self.engine.tombstone_log.read().unwrap();
        let deleted: Vec<_> = tombstone_guard
            .iter()
            .filter(|(_table, _row_id, lsn)| *lsn > base_lsn)
            .collect();
        let rows_deleted = deleted.len() as u64;
        drop(tombstone_guard);
        drop(tables_guard);

        // 4. Write diff file in standard .qmvb format with Differential type.
        let header = BackupHeader::new(
            BackupType::Differential,
            config.compression,
            base_lsn,
            current_lsn,
            tables_changed as u32,
            total_rows,
            original_size,
        );

        let manifest = BackupManifest {
            tables: table_entries,
        };

        let tmp_path = config.output.with_extension("qmvb.tmp");
        let file = fs::File::create(&tmp_path)?;
        let mut w = BufWriter::new(file);
        let mut crc_hasher = crc32fast::Hasher::new();

        // Header.
        let header_bytes = header.to_bytes();
        w.write_all(&header_bytes)?;
        crc_hasher.update(&header_bytes);

        // Manifest.
        let manifest_bytes = manifest.to_bytes()?;
        w.write_all(&manifest_bytes)?;
        crc_hasher.update(&manifest_bytes);

        // Data blocks.
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
                let comp_len = (chunk_data.len() as u32).to_le_bytes();
                w.write_all(&comp_len)?;
                crc_hasher.update(&comp_len);
                w.write_all(chunk_data)?;
                crc_hasher.update(chunk_data);
            }
        }

        // No WAL segment for diff backups.
        let absent = [0u8];
        w.write_all(&absent)?;
        crc_hasher.update(&absent);

        // Footer.
        let crc32 = crc_hasher.finalize();
        let hmac_bytes = if let Some(key) = hmac_key() {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            w.flush()?;
            let file_data = fs::read(&tmp_path)?;
            let mut mac = <Hmac<Sha256>>::new_from_slice(&key)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "Invalid HMAC key"))?;
            mac.update(&file_data);
            let result = mac.finalize();
            let mut hmac = [0u8; 32];
            hmac.copy_from_slice(&result.into_bytes());
            hmac
        } else {
            [0u8; 32]
        };

        let footer = BackupFooter::new(crc32, hmac_bytes);
        let footer_bytes = footer.to_bytes();
        w.write_all(&footer_bytes)?;
        w.flush()?;
        drop(w);

        // Atomic rename.
        fs::rename(&tmp_path, &config.output)?;

        let diff_size = fs::metadata(&config.output)?.len();
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(DiffResult {
            path: config.output.to_string_lossy().into_owned(),
            tables_changed,
            rows_added,
            rows_modified,
            rows_deleted,
            diff_size,
            duration_ms,
            base_lsn,
            end_lsn: current_lsn,
        })
    }
}
