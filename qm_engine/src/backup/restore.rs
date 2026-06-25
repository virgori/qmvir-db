//! `RestoreEngine` — restore data from `.qmvb`, `.qmdiff`, or PG dump.
//!
//! Reads the binary `.qmvb` format produced by `BackupEngine`, deserializes
//! table data, and inserts it back into a `NativeSqlEngine`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::backup::decompress;
use crate::backup::format::*;
use crate::backup::pg_compat::{
    count_insert_rows, create_table_sql, insert_table_name, parse_pgdump,
};
use crate::gateway::native_sql::{Cell, ColType, NativeRow, NativeSqlEngine, NativeTable};

/// Configuration for a restore operation.
pub struct RestoreConfig {
    /// Path to the backup file (.qmvb or .qmdiff).
    pub source: PathBuf,
    /// Optional: restore only specific tables.
    pub tables: Option<Vec<String>>,
    /// Whether to drop existing tables before restoring.
    pub drop_existing: bool,
}

/// Result of a restore operation.
#[derive(Debug, Serialize)]
pub struct RestoreResult {
    pub tables_restored: usize,
    pub total_rows: u64,
    pub duration_ms: u64,
}

pub struct RestoreEngine<'a> {
    engine: &'a mut NativeSqlEngine,
}

impl<'a> RestoreEngine<'a> {
    pub fn new(engine: &'a mut NativeSqlEngine) -> Self {
        Self { engine }
    }

    /// Restore from a `.qmvb` backup.
    ///
    /// 1. Read and validate header + footer (CRC32).
    /// 2. Parse manifest to get table metadata.
    /// 3. For each table: read chunks → decompress → deserialize rows.
    /// 4. Insert tables into engine (optionally dropping existing ones).
    pub fn restore_qmvb(&mut self, config: &RestoreConfig) -> io::Result<RestoreResult> {
        let start = Instant::now();

        // Read entire file into memory for CRC32 validation.
        let data = fs::read(&config.source)?;
        if data.len() < BackupHeader::SIZE + BackupFooter::SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File too small to be a valid .qmvb backup",
            ));
        }

        // Validate footer.
        let footer_start = data.len() - BackupFooter::SIZE;
        let footer_bytes: [u8; BackupFooter::SIZE] = data[footer_start..]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid footer size"))?;
        let footer = BackupFooter::from_bytes(&footer_bytes)?;

        // CRC32 check over everything before the footer.
        let mut crc_hasher = crc32fast::Hasher::new();
        crc_hasher.update(&data[..footer_start]);
        let computed_crc = crc_hasher.finalize();
        if computed_crc != footer.crc32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CRC32 mismatch: expected {:#010x}, computed {:#010x}",
                    footer.crc32, computed_crc
                ),
            ));
        }

        // Parse header.
        let header_bytes: [u8; BackupHeader::SIZE] = data[..BackupHeader::SIZE]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid header size"))?;
        let header = BackupHeader::from_bytes(&header_bytes)?;
        validate_backup_format_version(header.format_version)?;
        let compression = header.compression;

        // Parse manifest.
        let mut cursor = Cursor::new(&data[BackupHeader::SIZE..footer_start]);
        let manifest = BackupManifest::from_reader(&mut cursor)?;

        // Build a set of tables to restore (None = all).
        let filter: Option<std::collections::HashSet<&str>> = config
            .tables
            .as_ref()
            .map(|names| names.iter().map(|s| s.as_str()).collect());

        // Read table data blocks.
        let mut tables_restored = 0usize;
        let mut total_rows = 0u64;

        for entry in &manifest.tables {
            // Read table name from data block.
            let name_len = read_u16_le(&mut cursor)?;
            let mut name_buf = vec![0u8; name_len as usize];
            cursor.read_exact(&mut name_buf)?;
            let table_name = String::from_utf8(name_buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            let chunk_count = read_u32_le(&mut cursor)?;

            // Check if this table should be restored.
            let should_restore = filter
                .as_ref()
                .map(|f| f.contains(table_name.as_str()))
                .unwrap_or(true);

            if !should_restore {
                // Skip chunks for this table.
                for _ in 0..chunk_count {
                    let comp_len = read_u32_le(&mut cursor)? as usize;
                    let pos = cursor.position() as usize;
                    cursor.set_position((pos + comp_len) as u64);
                }
                continue;
            }

            // Parse column types from manifest entry.
            let column_types: Vec<ColType> = entry
                .types
                .iter()
                .map(|t| col_type_from_manifest(t))
                .collect();

            // Deserialize all chunks for this table.
            let mut rows: HashMap<i64, NativeRow> = HashMap::new();
            let mut row_id_counter = 1i64;

            for _ in 0..chunk_count {
                let comp_len = read_u32_le(&mut cursor)? as usize;
                let pos = cursor.position() as usize;
                let compressed = &cursor.get_ref()[pos..pos + comp_len];
                cursor.set_position((pos + comp_len) as u64);

                let decompressed = decompress(compressed, compression)?;
                let chunk_rows: Vec<NativeRow> = bincode::deserialize(&decompressed)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                for row in chunk_rows {
                    // Try to extract an "id" column for the row key, otherwise use counter.
                    let key = row
                        .cols
                        .get("id")
                        .and_then(|c| match c {
                            &Cell::Int(v) => Some(v),
                            _ => None,
                        })
                        .unwrap_or_else(|| {
                            let k = row_id_counter;
                            row_id_counter += 1;
                            k
                        });
                    // Ensure counter stays ahead.
                    if key >= row_id_counter {
                        row_id_counter = key + 1;
                    }
                    rows.insert(key, row);
                }
            }

            total_rows += rows.len() as u64;

            // Insert table into engine.
            let table = NativeTable {
                columns: entry.columns.clone(),
                column_types,
                rows,
                next_auto_id: row_id_counter.max(1),
                foreign_keys: Vec::new(),
                constraints: Vec::new(),
                table_checks: Vec::new(),
                sequences: HashMap::new(),
            };

            let mut tables_guard = self.engine.tables.write().unwrap();
            if config.drop_existing {
                tables_guard.remove(&table_name);
            }
            tables_guard.insert(table_name, table);
            drop(tables_guard);

            tables_restored += 1;
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(RestoreResult {
            tables_restored,
            total_rows,
            duration_ms,
        })
    }

    /// Restore from a `.qmdiff` differential backup.
    ///
    /// Applies changed rows (upsert) onto the existing engine state.
    /// Rows present in the diff overwrite existing rows; rows not mentioned
    /// are left unchanged.  Does not drop/recreate tables.
    pub fn restore_diff(&mut self, config: &RestoreConfig) -> io::Result<RestoreResult> {
        let start = Instant::now();

        // Read entire file into memory for CRC32 validation.
        let data = fs::read(&config.source)?;
        if data.len() < BackupHeader::SIZE + BackupFooter::SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File too small to be a valid .qmdiff backup",
            ));
        }

        // Validate footer.
        let footer_start = data.len() - BackupFooter::SIZE;
        let footer_bytes: [u8; BackupFooter::SIZE] = data[footer_start..]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid footer size"))?;
        let footer = BackupFooter::from_bytes(&footer_bytes)?;

        // CRC32 check.
        let mut crc_hasher = crc32fast::Hasher::new();
        crc_hasher.update(&data[..footer_start]);
        let computed_crc = crc_hasher.finalize();
        if computed_crc != footer.crc32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CRC32 mismatch: expected {:#010x}, computed {:#010x}",
                    footer.crc32, computed_crc
                ),
            ));
        }

        // Parse header and verify it is a differential backup.
        let header_bytes: [u8; BackupHeader::SIZE] = data[..BackupHeader::SIZE]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid header size"))?;
        let header = BackupHeader::from_bytes(&header_bytes)?;
        validate_backup_format_version(header.format_version)?;

        if header.backup_type != crate::backup::BackupType::Differential {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Expected differential backup (type=1), got type={:?}",
                    header.backup_type
                ),
            ));
        }

        let compression = header.compression;

        // Parse manifest.
        let mut cursor = Cursor::new(&data[BackupHeader::SIZE..footer_start]);
        let manifest = BackupManifest::from_reader(&mut cursor)?;

        // Build table filter.
        let filter: Option<std::collections::HashSet<&str>> = config
            .tables
            .as_ref()
            .map(|names| names.iter().map(|s| s.as_str()).collect());

        let mut tables_restored = 0usize;
        let mut total_rows = 0u64;

        for entry in &manifest.tables {
            // Read table name from data block.
            let name_len = read_u16_le(&mut cursor)?;
            let mut name_buf = vec![0u8; name_len as usize];
            cursor.read_exact(&mut name_buf)?;
            let table_name = String::from_utf8(name_buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            let chunk_count = read_u32_le(&mut cursor)?;

            let should_restore = filter
                .as_ref()
                .map(|f| f.contains(table_name.as_str()))
                .unwrap_or(true);

            if !should_restore {
                for _ in 0..chunk_count {
                    let comp_len = read_u32_le(&mut cursor)? as usize;
                    let pos = cursor.position() as usize;
                    cursor.set_position((pos + comp_len) as u64);
                }
                continue;
            }

            // Parse column types from manifest.
            let column_types: Vec<ColType> = entry
                .types
                .iter()
                .map(|t| col_type_from_manifest(t))
                .collect();

            // Deserialize changed rows.
            let mut changed_rows: HashMap<i64, NativeRow> = HashMap::new();
            let mut row_id_counter = 1i64;

            for _ in 0..chunk_count {
                let comp_len = read_u32_le(&mut cursor)? as usize;
                let pos = cursor.position() as usize;
                let compressed = &cursor.get_ref()[pos..pos + comp_len];
                cursor.set_position((pos + comp_len) as u64);

                let decompressed = decompress(compressed, compression)?;
                let chunk_rows: Vec<NativeRow> = bincode::deserialize(&decompressed)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                for row in chunk_rows {
                    let key = row
                        .cols
                        .get("id")
                        .and_then(|c| match c {
                            &Cell::Int(v) => Some(v),
                            _ => None,
                        })
                        .unwrap_or_else(|| {
                            let k = row_id_counter;
                            row_id_counter += 1;
                            k
                        });
                    if key >= row_id_counter {
                        row_id_counter = key + 1;
                    }
                    changed_rows.insert(key, row);
                }
            }

            total_rows += changed_rows.len() as u64;

            // Upsert into existing table (or create if it doesn't exist yet).
            let mut tables_guard = self.engine.tables.write().unwrap();

            let t = tables_guard
                .entry(table_name.clone())
                .or_insert_with(|| NativeTable {
                    columns: entry.columns.clone(),
                    column_types,
                    rows: HashMap::new(),
                    next_auto_id: 1,
                    foreign_keys: Vec::new(),
                    constraints: Vec::new(),
                    table_checks: Vec::new(),
                    sequences: HashMap::new(),
                });

            for (id, row) in changed_rows {
                if id >= t.next_auto_id {
                    t.next_auto_id = id.saturating_add(1);
                }
                t.rows.insert(id, row);
            }
            drop(tables_guard);

            tables_restored += 1;
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(RestoreResult {
            tables_restored,
            total_rows,
            duration_ms,
        })
    }

    /// Restore from a PostgreSQL pg_dump file.
    pub fn restore_pgdump(&mut self, path: &Path) -> io::Result<RestoreResult> {
        let config = RestoreConfig {
            source: path.to_path_buf(),
            tables: None,
            drop_existing: false,
        };
        self.restore_pgdump_with_config(&config)
    }

    pub fn restore_pgdump_with_config(
        &mut self,
        config: &RestoreConfig,
    ) -> io::Result<RestoreResult> {
        let start = Instant::now();
        let dump = parse_pgdump(&config.source)?;
        let filter: Option<HashSet<&str>> = config
            .tables
            .as_ref()
            .map(|names| names.iter().map(|name| name.as_str()).collect());

        if config.drop_existing {
            let mut tables = self.engine.tables.write().unwrap();
            for table in &dump.tables {
                if filter
                    .as_ref()
                    .map(|set| set.contains(table.name.as_str()))
                    .unwrap_or(true)
                {
                    tables.remove(&table.name);
                }
            }
        }

        let mut tables_restored = 0usize;
        for table in &dump.tables {
            let should_restore = filter
                .as_ref()
                .map(|set| set.contains(table.name.as_str()))
                .unwrap_or(true);
            if !should_restore {
                continue;
            }

            self.engine
                .execute(&create_table_sql(table))
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            tables_restored += 1;
        }

        let mut total_rows = 0u64;
        for insert in &dump.inserts {
            let table_name = insert_table_name(insert).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "translated INSERT is missing a table name",
                )
            })?;
            let should_restore = filter
                .as_ref()
                .map(|set| set.contains(table_name.as_str()))
                .unwrap_or(true);
            if !should_restore {
                continue;
            }

            self.engine
                .execute(insert)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            total_rows += count_insert_rows(insert) as u64;
        }

        Ok(RestoreResult {
            tables_restored,
            total_rows,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

// ── Little-endian read helpers ───────────────────────────────────────

fn read_u16_le<R: Read>(r: &mut R) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32_le<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::{col_type_from_manifest, col_type_to_manifest, validate_backup_format_version, *};
    use crate::backup::backup::{BackupConfig, BackupEngine};
    use crate::backup::Compression;
    use crate::gateway::native_sql::{Cell, ColType};
    use crate::gateway::NativeSqlEngine;
    use tempfile::tempdir;

    #[test]
    fn col_type_manifest_roundtrip() {
        let samples = [
            (ColType::Integer, "INTEGER"),
            (ColType::Vector(128), "VECTOR:128"),
            (ColType::Jsonb, "JSONB"),
        ];
        for (ct, expected) in samples {
            assert_eq!(col_type_to_manifest(&ct), expected);
            assert_eq!(col_type_from_manifest(expected), ct);
        }
        assert_eq!(col_type_from_manifest("Vector(32)"), ColType::Vector(32));
    }

    #[test]
    fn rejects_newer_backup_format_version() {
        assert!(validate_backup_format_version(0).is_err());
        assert!(validate_backup_format_version(FORMAT_VERSION + 1).is_err());
        assert!(validate_backup_format_version(FORMAT_VERSION).is_ok());
    }

    #[test]
    fn qmvb_vector_columns_roundtrip() {
        let engine = NativeSqlEngine::new();
        engine
            .execute("CREATE TABLE docs (id INTEGER, embedding VECTOR(3))")
            .unwrap();
        engine
            .execute("INSERT INTO docs (id, embedding) VALUES (1, '[0.1,0.2,0.3]')")
            .unwrap();

        let dir = tempdir().unwrap();
        let path = dir.path().join("vec.qmvb");
        let config = BackupConfig {
            tables: None,
            compression: Compression::Lz4,
            include_wal: false,
            output: path.clone(),
        };
        BackupEngine::new(&engine).run(&config).unwrap();

        let mut restored = NativeSqlEngine::new();
        let restore_cfg = RestoreConfig {
            source: path,
            tables: None,
            drop_existing: true,
        };
        let result = RestoreEngine::new(&mut restored)
            .restore_qmvb(&restore_cfg)
            .unwrap();
        assert_eq!(result.tables_restored, 1);
        assert_eq!(result.total_rows, 1);

        let tables = restored.tables.read().unwrap();
        let docs = tables.get("docs").unwrap();
        assert_eq!(docs.column_types[1], ColType::Vector(3));
        let row = docs.rows.get(&1).unwrap();
        assert!(matches!(
            row.cols.get("embedding"),
            Some(Cell::Vector { .. })
        ));
    }

    #[test]
    fn restore_pgdump_imports_copy_and_insert_rows() {
        let dir = tempdir().unwrap();
        let dump_path = dir.path().join("sample.sql");
        std::fs::write(
            &dump_path,
            "CREATE TABLE public.users (\n\
                 id integer,\n\
                 name text,\n\
                 active boolean,\n\
                 score numeric\n\
             );\n\
             COPY public.users (id, name, active, score) FROM stdin;\n\
             1\talice\tt\t10.5\n\
             2\tbob\tf\t7.25\n\
             \\.\n\
             INSERT INTO public.users VALUES (3, 'carol', true, 9.0);\n",
        )
        .unwrap();

        let mut engine = NativeSqlEngine::new();
        let config = RestoreConfig {
            source: dump_path,
            tables: None,
            drop_existing: true,
        };

        let result = RestoreEngine::new(&mut engine)
            .restore_pgdump_with_config(&config)
            .unwrap();

        assert_eq!(result.tables_restored, 1);
        assert_eq!(result.total_rows, 3);

        let tables = engine.tables.read().unwrap();
        let users = tables.get("users").unwrap();
        assert_eq!(users.rows.len(), 3);
        assert!(matches!(
            users.rows.get(&1).unwrap().cols.get("active"),
            Some(Cell::Int(1))
        ));
        assert!(matches!(
            users.rows.get(&2).unwrap().cols.get("active"),
            Some(Cell::Int(0))
        ));
    }
}
