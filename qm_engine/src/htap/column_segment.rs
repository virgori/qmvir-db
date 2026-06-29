//! Durable column segments — OLAP scans without rebuilding in-memory cache.

use crate::executor::simd_sum_f64;
use memmap2::Mmap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"QMCS";
const HEADER_LEN: usize = 28;

#[derive(Clone, Debug)]
pub struct ColumnSegmentMeta {
    pub table: String,
    pub column: String,
    pub segment_id: u64,
    pub min_lsn: u64,
    pub max_lsn: u64,
    pub row_count: u64,
    pub bytes: u64,
}

pub struct ColumnSegmentStore {
    root: Option<PathBuf>,
    index: Arc<RwLock<HashMap<String, Vec<ColumnSegmentMeta>>>>,
    /// Monotonic segment id per `table\0column` (avoids O(n) read_dir per append).
    next_segment_id: Arc<RwLock<HashMap<String, u64>>>,
}

impl ColumnSegmentStore {
    pub fn new(root: Option<PathBuf>) -> Self {
        let index = Arc::new(RwLock::new(HashMap::new()));
        let next_segment_id = Arc::new(RwLock::new(HashMap::new()));
        let store = Self {
            root: root.clone(),
            index,
            next_segment_id,
        };
        if let Some(ref dir) = root {
            let _ = fs::create_dir_all(dir);
            store.reload_index_from_disk();
        }
        store
    }

    pub fn reload_index_from_disk(&self) {
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let mut fresh: HashMap<String, Vec<ColumnSegmentMeta>> = HashMap::new();
        if let Ok(tables) = fs::read_dir(root) {
            for table_entry in tables.flatten() {
                if !table_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let table_name = table_entry.file_name().to_string_lossy().into_owned();
                let Ok(cols) = fs::read_dir(table_entry.path()) else {
                    continue;
                };
                for col_entry in cols.flatten() {
                    if !col_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let col_name = col_entry.file_name().to_string_lossy().into_owned();
                    let Ok(files) = fs::read_dir(col_entry.path()) else {
                        continue;
                    };
                    for file in files.flatten() {
                        let path = file.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("qmcs") {
                            continue;
                        }
                        if let Some(meta) = Self::read_segment_meta(&table_name, &col_name, &path) {
                            let key = format!("{table_name}\0{col_name}");
                            fresh.entry(key).or_default().push(meta);
                        }
                    }
                }
            }
        }
        for list in fresh.values_mut() {
            list.sort_by_key(|m| m.segment_id);
        }
        let mut next_ids = HashMap::new();
        for (key, list) in &fresh {
            let max_id = list.iter().map(|m| m.segment_id).max().unwrap_or(0);
            next_ids.insert(key.clone(), max_id);
        }
        *self.index.write() = fresh;
        *self.next_segment_id.write() = next_ids;
    }

    fn read_segment_meta(table: &str, column: &str, path: &Path) -> Option<ColumnSegmentMeta> {
        let data = fs::read(path).ok()?;
        if data.len() < HEADER_LEN || &data[..4] != MAGIC {
            return None;
        }
        let min_lsn = u64::from_le_bytes(data[4..12].try_into().ok()?);
        let max_lsn = u64::from_le_bytes(data[12..20].try_into().ok()?);
        let row_count = u64::from_le_bytes(data[20..28].try_into().ok()?);
        let segment_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("cseg_"))
            .and_then(|s| s.parse().ok())?;
        Some(ColumnSegmentMeta {
            table: table.to_string(),
            column: column.to_string(),
            segment_id,
            min_lsn,
            max_lsn,
            row_count,
            bytes: data.len() as u64,
        })
    }

    fn segment_path(&self, meta: &ColumnSegmentMeta) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        Some(
            root.join(safe(&meta.table))
                .join(safe(&meta.column))
                .join(format!("cseg_{:08}.qmcs", meta.segment_id)),
        )
    }

    pub fn read_f64_segment(&self, meta: &ColumnSegmentMeta) -> Result<Vec<f64>, String> {
        let path = self
            .segment_path(meta)
            .ok_or_else(|| "column segments require data_dir".to_string())?;
        let file = File::open(&path).map_err(|e| e.to_string())?;
        let mmap = unsafe { Mmap::map(&file).map_err(|e| e.to_string())? };
        if mmap.len() < HEADER_LEN || &mmap[..4] != MAGIC {
            return Err(format!("invalid segment {}", path.display()));
        }
        let row_count = u64::from_le_bytes(mmap[20..28].try_into().unwrap()) as usize;
        let mut out = Vec::with_capacity(row_count);
        let mut off = HEADER_LEN;
        for _ in 0..row_count {
            if off + 8 > mmap.len() {
                break;
            }
            out.push(f64::from_le_bytes(mmap[off..off + 8].try_into().unwrap()));
            off += 8;
        }
        Ok(out)
    }

    pub fn append_f64_column(
        &self,
        table: &str,
        column: &str,
        min_lsn: u64,
        max_lsn: u64,
        values: &[f64],
    ) -> Result<ColumnSegmentMeta, String> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| "column segments require data_dir".to_string())?;
        let key = format!("{table}\0{column}");
        let dir = root.join(safe(table)).join(safe(column));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let segment_id = {
            let mut ids = self.next_segment_id.write();
            let next = ids.entry(key.clone()).or_insert(0);
            *next += 1;
            *next
        };
        let path = dir.join(format!("cseg_{segment_id:08}.qmcs"));
        let mut bytes = Vec::with_capacity(HEADER_LEN + values.len() * 8);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&min_lsn.to_le_bytes());
        bytes.extend_from_slice(&max_lsn.to_le_bytes());
        bytes.extend_from_slice(&(values.len() as u64).to_le_bytes());
        for v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        let meta = ColumnSegmentMeta {
            table: table.to_string(),
            column: column.to_string(),
            segment_id,
            min_lsn,
            max_lsn,
            row_count: values.len() as u64,
            bytes: bytes.len() as u64,
        };
        self.index.write().entry(key).or_default().push(meta.clone());
        Ok(meta)
    }

    pub fn segments_at_lsn(&self, table: &str, column: &str, read_lsn: u64) -> Vec<ColumnSegmentMeta> {
        let key = format!("{table}\0{column}");
        self.index
            .read()
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.min_lsn <= read_lsn)
            .collect()
    }

    /// Sum the latest durable segment for `column` visible at `read_lsn`.
    pub fn sum_f64_at_lsn(
        &self,
        table: &str,
        column: &str,
        read_lsn: u64,
    ) -> Option<(f64, u64)> {
        let latest = self
            .segments_at_lsn(table, column, read_lsn)
            .into_iter()
            .max_by_key(|m| m.max_lsn)?;
        let values = self.read_f64_segment(&latest).ok()?;
        let sum = simd_sum_f64(&values);
        Some((sum, values.len() as u64))
    }

    pub fn row_count_at_lsn(&self, table: &str, id_column: &str, read_lsn: u64) -> Option<u64> {
        self.segments_at_lsn(table, id_column, read_lsn)
            .into_iter()
            .max_by_key(|m| m.max_lsn)
            .map(|m| m.row_count)
    }

    /// Sum `agg_col` for rows whose `id_col` is in [lo, hi] (sorted durable segments).
    pub fn sum_f64_between_on_id(
        &self,
        table: &str,
        id_col: &str,
        agg_col: &str,
        lo: i64,
        hi: i64,
        read_lsn: u64,
    ) -> Option<(f64, u64)> {
        let id_meta = self
            .segments_at_lsn(table, id_col, read_lsn)
            .into_iter()
            .max_by_key(|m| m.max_lsn)?;
        let agg_meta = self
            .segments_at_lsn(table, agg_col, read_lsn)
            .into_iter()
            .max_by_key(|m| m.max_lsn)?;
        let ids = self.read_f64_segment(&id_meta).ok()?;
        let vals = self.read_f64_segment(&agg_meta).ok()?;
        if ids.len() != vals.len() || ids.is_empty() {
            return None;
        }
        let lo_f = lo as f64;
        let hi_f = hi as f64;
        let start = ids.partition_point(|&x| x < lo_f);
        let end = ids.partition_point(|&x| x <= hi_f);
        let sum = simd_sum_f64(&vals[start..end]);
        Some((sum, (end - start) as u64))
    }

    pub fn has_any_for_table(&self, table: &str) -> bool {
        let prefix = format!("{table}\0");
        self.index
            .read()
            .keys()
            .any(|k| k.starts_with(prefix.as_str()))
    }
}

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_segment_roundtrip_sum() {
        let dir = std::env::temp_dir().join(format!(
            "qm_cseg_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ColumnSegmentStore::new(Some(dir.clone()));
        store
            .append_f64_column("t", "val", 1, 1, &[10.0, 20.0, 30.0])
            .unwrap();
        let (sum, n) = store.sum_f64_at_lsn("t", "val", 1).unwrap();
        assert!((sum - 60.0).abs() < f64::EPSILON);
        assert_eq!(n, 3);

        let store2 = ColumnSegmentStore::new(Some(dir.clone()));
        let (sum2, _) = store2.sum_f64_at_lsn("t", "val", 1).unwrap();
        assert!((sum2 - 60.0).abs() < f64::EPSILON);
        let _ = fs::remove_dir_all(dir);
    }
}
