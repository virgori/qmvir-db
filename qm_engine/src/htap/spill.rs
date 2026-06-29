//! External spill arena for hash/sort aggregates beyond RAM.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

pub struct SpillArena {
    root: Option<PathBuf>,
    files: parking_lot::Mutex<Vec<PathBuf>>,
}

impl SpillArena {
    pub fn new(root: Option<PathBuf>) -> Self {
        if let Some(ref dir) = root {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            root,
            files: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub fn spill_bytes(&self, label: &str, data: &[u8]) -> Result<PathBuf, String> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| "spill requires data_dir".to_string())?;
        let path = root.join(format!(
            "spill_{}_{}.bin",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = File::create(&path).map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| e.to_string())?;
        self.files.lock().push(path.clone());
        Ok(path)
    }

    pub fn read_spill(&self, path: &PathBuf) -> Result<Vec<u8>, String> {
        let mut f = File::open(path).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        Ok(buf)
    }

    pub fn cleanup(&self) {
        let mut guard = self.files.lock();
        for path in guard.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}
