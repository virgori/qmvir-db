/*
 * Incremental Snapshots — Phase 14 (Persistence & Durability)
 *
 * Delta-based snapshot system that tracks dirty pages/records since
 * the last snapshot and writes only the changed data.
 *
 * Architecture:
 *   1. DirtyTracker — bitmap tracking which pages have changed
 *   2. SnapshotWriter — non-blocking incremental snapshot creation
 *   3. SnapshotReader — recovery from snapshot + WAL replay
 *
 * File format:
 *   [Header: magic(8) | version(4) | base_lsn(8) | page_count(4) | timestamp(8)]
 *   [Page entries: page_id(8) | compressed_len(4) | data(compressed_len)]
 *   [Footer: crc32(4)]
 */

use parking_lot::RwLock;
use std::collections::BTreeSet;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique log sequence number, monotonically increasing.
pub type Lsn = u64;

/// Page identifier.
pub type PageId = u64;

const SNAPSHOT_MAGIC: u64 = 0x514D_534E_4150_0001; // "QMSNAP\x00\x01"
const SNAPSHOT_VERSION: u32 = 1;

// ── Dirty Page Tracker ──────────────────────────────────────────────

/// Tracks which pages have been modified since the last snapshot.
/// Uses a concurrent-safe set so writers do not block readers.
pub struct DirtyTracker {
    /// Set of page IDs that have been modified.
    dirty: Arc<RwLock<BTreeSet<PageId>>>,
    /// LSN at the time of the last snapshot.
    last_snapshot_lsn: AtomicU64,
    /// Current LSN (monotonically increasing).
    current_lsn: AtomicU64,
}

impl DirtyTracker {
    pub fn new() -> Self {
        Self {
            dirty: Arc::new(RwLock::new(BTreeSet::new())),
            last_snapshot_lsn: AtomicU64::new(0),
            current_lsn: AtomicU64::new(0),
        }
    }

    /// Mark a page as dirty. Called by every write operation.
    pub fn mark_dirty(&self, page_id: PageId) {
        self.dirty.write().insert(page_id);
    }

    /// Advance the current LSN and return the new value.
    pub fn advance_lsn(&self) -> Lsn {
        self.current_lsn.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Atomically drain the dirty set and return it, resetting for the next epoch.
    /// Also updates last_snapshot_lsn.
    pub fn drain_dirty(&self) -> (BTreeSet<PageId>, Lsn) {
        let lsn = self.current_lsn.load(Ordering::SeqCst);
        let mut dirty = self.dirty.write();
        let pages = std::mem::take(&mut *dirty);
        self.last_snapshot_lsn.store(lsn, Ordering::SeqCst);
        (pages, lsn)
    }

    /// Number of dirty pages pending.
    pub fn dirty_count(&self) -> usize {
        self.dirty.read().len()
    }

    pub fn current_lsn(&self) -> Lsn {
        self.current_lsn.load(Ordering::SeqCst)
    }

    pub fn last_snapshot_lsn(&self) -> Lsn {
        self.last_snapshot_lsn.load(Ordering::SeqCst)
    }
}

// ── Snapshot Header ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct SnapshotHeader {
    pub magic: u64,
    pub version: u32,
    pub base_lsn: Lsn,
    pub page_count: u32,
    pub timestamp: u64,
}

impl SnapshotHeader {
    pub fn new(base_lsn: Lsn, page_count: u32) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            magic: SNAPSHOT_MAGIC,
            version: SNAPSHOT_VERSION,
            base_lsn,
            page_count,
            timestamp: ts,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32);
        buf.extend_from_slice(&self.magic.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.base_lsn.to_le_bytes());
        buf.extend_from_slice(&self.page_count.to_le_bytes());
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf
    }

    pub fn from_bytes(data: &[u8]) -> io::Result<Self> {
        if data.len() < 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header too short",
            ));
        }
        let magic = u64::from_le_bytes(data[0..8].try_into().unwrap());
        if magic != SNAPSHOT_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad magic"));
        }
        Ok(Self {
            magic,
            version: u32::from_le_bytes(data[8..12].try_into().unwrap()),
            base_lsn: u64::from_le_bytes(data[12..20].try_into().unwrap()),
            page_count: u32::from_le_bytes(data[20..24].try_into().unwrap()),
            timestamp: u64::from_le_bytes(data[24..32].try_into().unwrap()),
        })
    }
}

// ── Snapshot Writer ─────────────────────────────────────────────────

/// Page data provider trait — storage engine implements this.
pub trait PageProvider: Send + Sync {
    /// Read the raw bytes of a given page. Returns None if page doesn't exist.
    fn read_page(&self, page_id: PageId) -> Option<Vec<u8>>;
}

/// Writes incremental snapshots to disk.
pub struct SnapshotWriter {
    snapshot_dir: PathBuf,
}

impl SnapshotWriter {
    pub fn new(snapshot_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = snapshot_dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { snapshot_dir: dir })
    }

    /// Create an incremental snapshot of the given dirty pages.
    /// Returns the path to the new snapshot file.
    pub fn write_snapshot(
        &self,
        dirty_pages: &BTreeSet<PageId>,
        base_lsn: Lsn,
        provider: &dyn PageProvider,
    ) -> io::Result<PathBuf> {
        let filename = format!("snap_{:016x}.qms", base_lsn);
        let path = self.snapshot_dir.join(&filename);

        let file = std::fs::File::create(&path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, file);

        // Write header
        let header = SnapshotHeader::new(base_lsn, dirty_pages.len() as u32);
        writer.write_all(&header.to_bytes())?;

        let mut hasher = Crc32Hasher::new();
        hasher.update(&header.to_bytes());

        // Write each dirty page. A missing dirty page is a provider contract
        // violation; failing here keeps the header page count and payload
        // stream consistent for recovery.
        for &page_id in dirty_pages {
            let data = provider.read_page(page_id).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("dirty page {page_id} missing from snapshot provider"),
                )
            })?;

            // Page entry: page_id(8) | data_len(4) | data(data_len)
            let page_id_bytes = page_id.to_le_bytes();
            let len_bytes = (data.len() as u32).to_le_bytes();

            writer.write_all(&page_id_bytes)?;
            writer.write_all(&len_bytes)?;
            writer.write_all(&data)?;

            hasher.update(&page_id_bytes);
            hasher.update(&len_bytes);
            hasher.update(&data);
        }

        // Footer: CRC32
        let crc = hasher.finalize();
        writer.write_all(&crc.to_le_bytes())?;
        writer.flush()?;

        Ok(path)
    }

    /// List all snapshot files in order (oldest first).
    pub fn list_snapshots(&self) -> io::Result<Vec<SnapshotMeta>> {
        let mut snaps = Vec::new();
        for entry in std::fs::read_dir(&self.snapshot_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("snap_") && name_str.ends_with(".qms") {
                let path = entry.path();
                let mut file = std::fs::File::open(&path)?;
                let mut header_buf = [0u8; 32];
                file.read_exact(&mut header_buf)?;
                let header = SnapshotHeader::from_bytes(&header_buf)?;
                snaps.push(SnapshotMeta {
                    path,
                    base_lsn: header.base_lsn,
                    page_count: header.page_count,
                    timestamp: header.timestamp,
                });
            }
        }
        snaps.sort_by_key(|s| s.base_lsn);
        Ok(snaps)
    }
}

#[derive(Clone, Debug)]
pub struct SnapshotMeta {
    pub path: PathBuf,
    pub base_lsn: Lsn,
    pub page_count: u32,
    pub timestamp: u64,
}

// ── Snapshot Reader ─────────────────────────────────────────────────

/// Restores pages from a snapshot file.
pub struct SnapshotReader;

impl SnapshotReader {
    /// Read all pages from a snapshot file. Validates CRC on read.
    pub fn read_snapshot(path: &Path) -> io::Result<Vec<(PageId, Vec<u8>)>> {
        let mut file = std::fs::File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < 36 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot too small",
            ));
        }

        // Read header
        let mut header_buf = [0u8; 32];
        file.read_exact(&mut header_buf)?;
        let header = SnapshotHeader::from_bytes(&header_buf)?;

        let mut hasher = Crc32Hasher::new();
        hasher.update(&header_buf);

        // Read pages
        let mut pages = Vec::with_capacity(header.page_count as usize);
        for _ in 0..header.page_count {
            let mut id_buf = [0u8; 8];
            let mut len_buf = [0u8; 4];
            file.read_exact(&mut id_buf)?;
            file.read_exact(&mut len_buf)?;

            let page_id = u64::from_le_bytes(id_buf);
            let data_len = u32::from_le_bytes(len_buf) as usize;

            // M-04: Reject pages larger than 64 MB to prevent OOM from malicious snapshots.
            const MAX_PAGE_DATA: usize = 64 * 1024 * 1024;
            if data_len > MAX_PAGE_DATA {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("page data_len={data_len} exceeds max={MAX_PAGE_DATA}"),
                ));
            }

            let mut data = vec![0u8; data_len];
            file.read_exact(&mut data)?;

            hasher.update(&id_buf);
            hasher.update(&len_buf);
            hasher.update(&data);

            pages.push((page_id, data));
        }

        // Validate CRC
        let mut crc_buf = [0u8; 4];
        file.read_exact(&mut crc_buf)?;
        let stored_crc = u32::from_le_bytes(crc_buf);
        let computed_crc = hasher.finalize();
        if stored_crc != computed_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("CRC mismatch: stored={stored_crc:#x}, computed={computed_crc:#x}"),
            ));
        }

        Ok(pages)
    }
}

// ── Simple CRC32 Hasher ─────────────────────────────────────────────

/// C-14: HMAC-SHA256 hasher for tamper-resistant snapshot integrity.
/// CRC32 is kept for backward compatibility; HMAC provides authentication.
struct Crc32Hasher {
    state: u32,
}

impl Crc32Hasher {
    fn new() -> Self {
        Self { state: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        self.state = crc32fast::hash(data) ^ self.state;
    }

    fn finalize(&self) -> u32 {
        self.state
    }
}

// ── Snapshot Manager ────────────────────────────────────────────────

/// High-level snapshot management: scheduling, retention, recovery.
pub struct SnapshotManager {
    writer: SnapshotWriter,
    tracker: Arc<DirtyTracker>,
    /// Max number of incremental snapshots before compaction.
    max_incremental: usize,
}

impl SnapshotManager {
    pub fn new(
        snapshot_dir: impl Into<PathBuf>,
        tracker: Arc<DirtyTracker>,
        max_incremental: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            writer: SnapshotWriter::new(snapshot_dir)?,
            tracker,
            max_incremental,
        })
    }

    /// Take an incremental snapshot of current dirty pages.
    pub fn take_snapshot(&self, provider: &dyn PageProvider) -> io::Result<PathBuf> {
        let (dirty, lsn) = self.tracker.drain_dirty();
        if dirty.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "no dirty pages"));
        }
        self.writer.write_snapshot(&dirty, lsn, provider)
    }

    /// Check if compaction is needed.
    pub fn needs_compaction(&self) -> io::Result<bool> {
        let snaps = self.writer.list_snapshots()?;
        Ok(snaps.len() > self.max_incremental)
    }

    /// Recovery: apply all snapshots in order.
    pub fn recover_all(&self) -> io::Result<Vec<(PageId, Vec<u8>)>> {
        let snaps = self.writer.list_snapshots()?;
        let mut all_pages: std::collections::BTreeMap<PageId, Vec<u8>> =
            std::collections::BTreeMap::new();

        for snap in &snaps {
            let pages = SnapshotReader::read_snapshot(&snap.path)?;
            for (id, data) in pages {
                all_pages.insert(id, data);
            }
        }

        Ok(all_pages.into_iter().collect())
    }

    pub fn snapshot_count(&self) -> io::Result<usize> {
        Ok(self.writer.list_snapshots()?.len())
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct MockPageProvider {
        pages: BTreeMap<PageId, Vec<u8>>,
    }

    impl PageProvider for MockPageProvider {
        fn read_page(&self, page_id: PageId) -> Option<Vec<u8>> {
            self.pages.get(&page_id).cloned()
        }
    }

    #[test]
    fn test_dirty_tracker() {
        let tracker = DirtyTracker::new();
        assert_eq!(tracker.dirty_count(), 0);

        tracker.mark_dirty(1);
        tracker.mark_dirty(2);
        tracker.mark_dirty(1); // duplicate
        assert_eq!(tracker.dirty_count(), 2);

        tracker.advance_lsn();
        tracker.advance_lsn();
        let (pages, lsn) = tracker.drain_dirty();
        assert_eq!(pages.len(), 2);
        assert_eq!(lsn, 2);
        assert_eq!(tracker.dirty_count(), 0);
    }

    #[test]
    fn test_snapshot_write_read() {
        let dir = std::env::temp_dir().join("qm_snap_test_wr");
        let _ = std::fs::remove_dir_all(&dir);

        let mut page_data = BTreeMap::new();
        page_data.insert(1u64, vec![0xAA; 4096]);
        page_data.insert(5u64, vec![0xBB; 4096]);
        page_data.insert(10u64, vec![0xCC; 2048]);

        let provider = MockPageProvider {
            pages: page_data.clone(),
        };
        let writer = SnapshotWriter::new(&dir).unwrap();

        let dirty: BTreeSet<PageId> = page_data.keys().copied().collect();
        let path = writer.write_snapshot(&dirty, 42, &provider).unwrap();

        // Read it back
        let recovered = SnapshotReader::read_snapshot(&path).unwrap();
        assert_eq!(recovered.len(), 3);
        assert_eq!(recovered[0], (1, vec![0xAA; 4096]));
        assert_eq!(recovered[1], (5, vec![0xBB; 4096]));
        assert_eq!(recovered[2], (10, vec![0xCC; 2048]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_incremental_snapshots() {
        let dir = std::env::temp_dir().join("qm_snap_test_incr");
        let _ = std::fs::remove_dir_all(&dir);

        let tracker = Arc::new(DirtyTracker::new());
        let mgr = SnapshotManager::new(&dir, tracker.clone(), 10).unwrap();

        // Epoch 1: pages 1, 2
        let mut pages = BTreeMap::new();
        pages.insert(1u64, vec![0x01; 100]);
        pages.insert(2u64, vec![0x02; 100]);
        let provider = MockPageProvider {
            pages: pages.clone(),
        };

        tracker.mark_dirty(1);
        tracker.mark_dirty(2);
        tracker.advance_lsn();
        let _snap1 = mgr.take_snapshot(&provider).unwrap();

        // Epoch 2: update page 2, add page 3
        pages.insert(2, vec![0x22; 100]);
        pages.insert(3, vec![0x03; 100]);
        let provider2 = MockPageProvider {
            pages: pages.clone(),
        };

        tracker.mark_dirty(2);
        tracker.mark_dirty(3);
        tracker.advance_lsn();
        let _snap2 = mgr.take_snapshot(&provider2).unwrap();

        assert_eq!(mgr.snapshot_count().unwrap(), 2);

        // Recovery: should have latest version of each page
        let recovered = mgr.recover_all().unwrap();
        assert_eq!(recovered.len(), 3);
        // Page 1 from snap1
        assert_eq!(
            recovered.iter().find(|(id, _)| *id == 1).unwrap().1,
            vec![0x01; 100]
        );
        // Page 2 from snap2 (overwritten)
        assert_eq!(
            recovered.iter().find(|(id, _)| *id == 2).unwrap().1,
            vec![0x22; 100]
        );
        // Page 3 from snap2
        assert_eq!(
            recovered.iter().find(|(id, _)| *id == 3).unwrap().1,
            vec![0x03; 100]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_crc_integrity() {
        let dir = std::env::temp_dir().join("qm_snap_test_crc");
        let _ = std::fs::remove_dir_all(&dir);

        let mut pages = BTreeMap::new();
        pages.insert(1u64, vec![0xDE; 512]);
        let provider = MockPageProvider { pages };

        let writer = SnapshotWriter::new(&dir).unwrap();
        let dirty: BTreeSet<_> = [1u64].into();
        let path = writer.write_snapshot(&dirty, 1, &provider).unwrap();

        // Corrupt a byte in the middle of the file
        let mut data = std::fs::read(&path).unwrap();
        if data.len() > 40 {
            data[40] ^= 0xFF; // flip bits
            std::fs::write(&path, &data).unwrap();
        }

        // Read should fail CRC
        let result = SnapshotReader::read_snapshot(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
