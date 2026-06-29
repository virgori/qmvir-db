//! `qm pitr` — point-in-time recovery planning and restore.

use crate::gateway::NativeSqlEngine;
use crate::htap::{materialize_pitr_data_dir, restore_to_timestamp, wal_lines_to_replay};
use std::path::{Path, PathBuf};

/// `qm pitr plan --timestamp <unix>`
pub fn run_pitr_plan(data_dir: &Path, timestamp: i64) {
    match restore_to_timestamp(data_dir, timestamp, None) {
        Ok(manifest) => {
            let lines = wal_lines_to_replay(data_dir, manifest.target_lsn).unwrap_or_default();
            println!("PITR plan:");
            println!("  Target time:  {timestamp}");
            println!("  Base LSN:     {}", manifest.base_lsn);
            println!("  Target LSN:   {}", manifest.target_lsn);
            println!("  WAL lines:    {}", lines.len());
            println!("  Snapshot:     {}", manifest.snapshot_path);
            println!("  Manifest:     {}", data_dir.join("pitr_manifest.json").display());
        }
        Err(e) => {
            eprintln!("error: pitr plan failed: {e}");
            std::process::exit(1);
        }
    }
}

/// `qm pitr restore --timestamp <unix> --output <dir>` or `--lsn <n>`.
pub fn run_pitr_restore(
    source_dir: &Path,
    output_dir: &PathBuf,
    timestamp: Option<i64>,
    lsn: Option<u64>,
) {
    let target_lsn = if let Some(l) = lsn {
        l
    } else {
        let ts = timestamp.unwrap_or_else(|| {
            eprintln!("error: specify --timestamp or --lsn");
            std::process::exit(1);
        });
        match restore_to_timestamp(source_dir, ts, None) {
            Ok(m) => m.target_lsn,
            Err(e) => {
                eprintln!("error: pitr restore failed: {e}");
                std::process::exit(1);
            }
        }
    };

    match materialize_pitr_data_dir(source_dir, output_dir, target_lsn) {
        Ok(manifest) => {
            let _engine = NativeSqlEngine::with_data_dir(output_dir.clone());
            println!("PITR restore completed:");
            println!("  Output:     {}", output_dir.display());
            println!("  Target LSN: {}", manifest.target_lsn);
            println!("  Target time:{}", manifest.target_time_unix);
        }
        Err(e) => {
            eprintln!("error: pitr restore failed: {e}");
            std::process::exit(1);
        }
    }
}
