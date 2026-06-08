//! `qm backup`, `qm verify`, and `qm restore` command implementations.

use crate::backup::backup::{BackupConfig, BackupEngine};
use crate::backup::restore::{RestoreConfig, RestoreEngine};
use crate::backup::verify::VerifyEngine;
use crate::backup::Compression;
use crate::gateway::native_sql::NativeSqlEngine;
use std::path::Path;

fn is_pgdump_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "sql" | "pgsql"))
        .unwrap_or(false)
}

/// Execute `qm backup`.
pub fn run_backup(
    engine: &NativeSqlEngine,
    output: &Path,
    compress: &str,
    tables: Option<Vec<String>>,
    pitr: bool,
) {
    let comp = match Compression::from_str(compress) {
        Some(c) => c,
        None => {
            eprintln!("error: unknown compression '{compress}'. Use: none, lz4, zstd");
            std::process::exit(1);
        }
    };

    let config = BackupConfig {
        tables,
        compression: comp,
        include_wal: pitr,
        output: output.to_path_buf(),
    };

    let be = BackupEngine::new(engine);
    match be.run(&config) {
        Ok(result) => {
            println!("Backup created: {}", result.path);
            println!("  Tables:      {}", result.tables_backed_up);
            println!("  Rows:        {}", result.total_rows);
            println!("  Original:    {} bytes", result.original_size);
            println!("  Compressed:  {} bytes", result.compressed_size);
            let ratio = if result.original_size > 0 {
                (result.compressed_size as f64 / result.original_size as f64) * 100.0
            } else {
                100.0
            };
            println!("  Ratio:       {:.1}%", ratio);
            println!("  CRC32:       {:#010x}", result.crc32);
            println!("  Duration:    {} ms", result.duration_ms);
        }
        Err(e) => {
            eprintln!("error: backup failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Execute `qm verify`.
pub fn run_verify(path: &Path, show_info: bool) {
    match VerifyEngine::verify_quick(path) {
        Ok(result) => {
            if result.ok {
                println!("✓ Backup OK: {}", path.display());
            } else {
                println!("✗ Backup FAILED: {}", path.display());
            }
            println!("  Header valid: {}", result.header_valid);
            println!("  Footer valid: {}", result.footer_valid);
            println!("  CRC match:    {}", result.crc_match);
            if let Some(hmac) = result.hmac_match {
                println!("  HMAC match:   {}", hmac);
            }
            println!("  Tables:       {}", result.tables);
            println!("  Rows:         {}", result.rows);
            if !result.errors.is_empty() {
                println!("  Errors:");
                for e in &result.errors {
                    println!("    - {e}");
                }
            }
            if !result.ok {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error: verify failed: {e}");
            std::process::exit(1);
        }
    }

    if show_info {
        println!();
        run_info(path);
    }
}

/// Show backup metadata.
pub fn run_info(path: &Path) {
    match VerifyEngine::info(path) {
        Ok(info) => {
            println!("Backup info: {}", path.display());
            println!("  Format:      v{}", info.format_version);
            println!("  Type:        {:?}", info.backup_type);
            println!("  Compression: {:?}", info.compression);
            println!("  Base LSN:    {}", info.base_lsn);
            println!("  End LSN:     {}", info.end_lsn);
            println!("  Timestamp:   {}", info.timestamp);
            println!("  Tables:      {}", info.table_count);
            println!("  Total rows:  {}", info.total_rows);
            println!("  Orig size:   {} bytes", info.original_size);
            println!("  File size:   {} bytes", info.file_size);
            println!();
            for t in &info.tables {
                println!(
                    "  Table '{}': {} rows, {} chunks, columns: {:?}",
                    t.name, t.row_count, t.chunk_count, t.columns
                );
            }
        }
        Err(e) => {
            eprintln!("error: info failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Execute `qm restore`.
pub fn run_restore(
    engine: &mut NativeSqlEngine,
    input: &Path,
    drop_existing: bool,
    tables: Option<Vec<String>>,
) {
    let config = RestoreConfig {
        source: input.to_path_buf(),
        tables,
        drop_existing,
    };

    let mut re = RestoreEngine::new(engine);
    let result = if input
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("qmdiff"))
        .unwrap_or(false)
    {
        re.restore_diff(&config)
    } else if is_pgdump_path(input) {
        re.restore_pgdump_with_config(&config)
    } else {
        re.restore_qmvb(&config)
    };

    match result {
        Ok(result) => {
            println!("Restore completed: {}", input.display());
            println!("  Tables:   {}", result.tables_restored);
            println!("  Rows:     {}", result.total_rows);
            println!("  Duration: {} ms", result.duration_ms);
        }
        Err(e) => {
            eprintln!("error: restore failed: {e}");
            std::process::exit(1);
        }
    }
}
