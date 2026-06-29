//! Durable row segments — mmap-backed pages for scale beyond RAM.

use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use parking_lot::RwLock;
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"QMRS";
const HEADER_LEN: usize = 64;

#[derive(Clone, Debug)]
pub struct RowSegmentMeta {
    pub table: String,
    pub segment_id: u64,
    pub min_lsn: u64,
    pub max_lsn: u64,
    pub row_count: u64,
    pub bytes: u64,
}

pub struct RowSegmentStore {
    root: Option<PathBuf>,
    segments: Arc<RwLock<HashMap<String, Vec<RowSegmentMeta>>>>,
}

impl RowSegmentStore {
    pub fn new(root: Option<PathBuf>) -> Self {
        if let Some(ref dir) = root {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            root,
            segments: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn append_segment(
        &self,
        table: &str,
        min_lsn: u64,
        max_lsn: u64,
        payload: &[u8],
    ) -> Result<RowSegmentMeta, String> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| "row segments require data_dir".to_string())?;
        let table_dir = root.join(sanitize(table));
        std::fs::create_dir_all(&table_dir).map_err(|e| e.to_string())?;
        let segment_id = table_dir
            .read_dir()
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .count() as u64
            + 1;
        let path = table_dir.join(format!("seg_{segment_id:08}.qmrs"));
        let mut file = File::create(&path).map_err(|e| e.to_string())?;
        let mut header = vec![0u8; HEADER_LEN];
        header[0..4].copy_from_slice(MAGIC);
        header[8..16].copy_from_slice(&min_lsn.to_le_bytes());
        header[16..24].copy_from_slice(&max_lsn.to_le_bytes());
        header[24..32].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        file.write_all(&header).map_err(|e| e.to_string())?;
        file.write_all(payload).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let meta = RowSegmentMeta {
            table: table.to_string(),
            segment_id,
            min_lsn,
            max_lsn,
            row_count: 0,
            bytes: (HEADER_LEN + payload.len()) as u64,
        };
        self.segments
            .write()
            .entry(table.to_string())
            .or_default()
            .push(meta.clone());
        Ok(meta)
    }

    pub fn segments_for_table(&self, table: &str) -> Vec<RowSegmentMeta> {
        self.segments
            .read()
            .get(table)
            .cloned()
            .unwrap_or_default()
    }

    pub fn mmap_segment(&self, table: &str, segment_id: u64) -> Result<Mmap, String> {
        let root = self.root.as_ref().ok_or("no data_dir")?;
        let path = root
            .join(sanitize(table))
            .join(format!("seg_{segment_id:08}.qmrs"));
        let file = File::open(&path).map_err(|e| e.to_string())?;
        unsafe { Mmap::map(&file).map_err(|e| e.to_string()) }
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}
