//! Point-in-time recovery — WAL archive index + restore to timestamp.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalArchiveEntry {
    pub lsn: u64,
    pub wall_time_unix: i64,
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalArchiveIndex {
    pub entries: Vec<WalArchiveEntry>,
}

impl WalArchiveIndex {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("wal_archive.json");
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(idx) = serde_json::from_str(&text) {
                return idx;
            }
        }
        Self {
            entries: Vec::new(),
        }
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("wal_archive.json");
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }

    pub fn record(&mut self, lsn: u64, wall_time_unix: i64, wal_path: &Path) {
        let bytes = fs::metadata(wal_path).map(|m| m.len()).unwrap_or(0);
        self.record_with_bytes(lsn, wall_time_unix, wal_path, bytes);
    }

    pub fn record_with_bytes(
        &mut self,
        lsn: u64,
        wall_time_unix: i64,
        wal_path: &Path,
        bytes: u64,
    ) {
        let entry = WalArchiveEntry {
            lsn,
            wall_time_unix,
            path: wal_path.display().to_string(),
            bytes,
        };
        if self.entries.last().is_some_and(|e| e.lsn <= lsn) {
            self.entries.push(entry);
        } else {
            self.entries.push(entry);
            self.entries.sort_by_key(|e| e.lsn);
        }
    }

    pub fn lsn_at_or_before(&self, target_unix: i64) -> Option<u64> {
        self.entries
            .iter()
            .filter(|e| e.wall_time_unix <= target_unix)
            .map(|e| e.lsn)
            .max()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PitrManifest {
    pub backup_id: String,
    pub base_lsn: u64,
    pub target_lsn: u64,
    pub target_time_unix: i64,
    pub snapshot_path: String,
}

/// Collect WAL SQL lines to replay up to `target_lsn` (1-based line index).
pub fn wal_lines_to_replay(data_dir: &Path, target_lsn: u64) -> Result<Vec<String>, String> {
    let wal_path = data_dir.join("native_sql.wal");
    let text = fs::read_to_string(&wal_path).map_err(|e| format!("read WAL: {e}"))?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lsn = idx as u64 + 1;
        if lsn > target_lsn {
            break;
        }
        let sql = line.trim();
        if sql.is_empty() {
            continue;
        }
        // Strip optional CRC prefix: "DEADBEEF\tSQL"
        let sql = if let Some((hex, rest)) = sql.split_once('\t') {
            if hex.len() == 8 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                rest
            } else {
                sql
            }
        } else {
            sql
        };
        out.push(sql.to_string());
    }
    Ok(out)
}

/// Replay WAL into engine until `target_lsn` (requires snapshot already loaded).
pub fn replay_wal_to_lsn(
    data_dir: &Path,
    target_lsn: u64,
    apply: &mut dyn FnMut(&str) -> Result<(), String>,
) -> Result<u64, String> {
    let lines = wal_lines_to_replay(data_dir, target_lsn)?;
    let mut n = 0u64;
    for sql in lines {
        apply(&sql)?;
        n += 1;
    }
    Ok(n)
}

/// Copy `source` data directory into empty `target`, then truncate WAL to `target_lsn`.
pub fn materialize_pitr_data_dir(
    source_dir: &Path,
    target_dir: &Path,
    target_lsn: u64,
) -> Result<PitrManifest, String> {
    if target_dir.exists() {
        let non_empty = fs::read_dir(target_dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some();
        if non_empty {
            return Err(format!(
                "target directory must be empty: {}",
                target_dir.display()
            ));
        }
    } else {
        fs::create_dir_all(target_dir).map_err(|e| e.to_string())?;
    }

    copy_dir_all(source_dir, target_dir)?;

    let lines = wal_lines_to_replay(source_dir, target_lsn)?;
    let wal_body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(target_dir.join("native_sql.wal"), wal_body).map_err(|e| e.to_string())?;

    let index = WalArchiveIndex::load(source_dir);
    let target_time_unix = index
        .entries
        .iter()
        .find(|e| e.lsn == target_lsn)
        .map(|e| e.wall_time_unix)
        .unwrap_or(0);
    let base_lsn = index.entries.first().map(|e| e.lsn).unwrap_or(0);
    let manifest = PitrManifest {
        backup_id: format!("pitr_restore_{target_lsn}"),
        base_lsn,
        target_lsn,
        target_time_unix,
        snapshot_path: target_dir.join("native_sql.snap").display().to_string(),
    };
    fs::write(
        target_dir.join("pitr_manifest.json"),
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_dir() {
        return Err(format!("source is not a directory: {}", src.display()));
    }
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == "native_sql.wal" {
            continue;
        }
        let to = dst.join(&name);
        if ft.is_dir() {
            fs::create_dir_all(&to).map_err(|e| e.to_string())?;
            copy_dir_all(&entry.path(), &to)?;
        } else if ft.is_file() {
            fs::copy(entry.path(), &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Restore to a wall-clock timestamp by replaying WAL up to computed LSN.
pub fn restore_to_timestamp(
    data_dir: &Path,
    target_time_unix: i64,
    snapshot_path: Option<PathBuf>,
) -> Result<PitrManifest, String> {
    let index = WalArchiveIndex::load(data_dir);
    let target_lsn = index
        .lsn_at_or_before(target_time_unix)
        .ok_or_else(|| "no WAL archive entry before target time".to_string())?;
    let base_lsn = index.entries.first().map(|e| e.lsn).unwrap_or(0);
    let snap = snapshot_path
        .unwrap_or_else(|| data_dir.join("snapshot.qmvs"));
    let manifest = PitrManifest {
        backup_id: format!("pitr_{target_time_unix}"),
        base_lsn,
        target_lsn,
        target_time_unix,
        snapshot_path: snap.display().to_string(),
    };
    let out = data_dir.join("pitr_manifest.json");
    fs::write(&out, serde_json::to_string_pretty(&manifest).unwrap()).map_err(|e| e.to_string())?;
    Ok(manifest)
}
