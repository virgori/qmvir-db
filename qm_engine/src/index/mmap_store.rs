/*
 * Disk-Backed Mmap Vector Storage — Big Data HNSW + Inverted Index
 *
 * Provides memory-mapped storage for:
 *   • Vector data (HNSW nodes): mmap'd flat file, O(1) random access
 *   • Graph adjacency lists: separate mmap file
 *   • Inverted index posting lists: block-aligned mmap file
 *
 * Design:
 *   • Vectors stored in a flat file: [header:64][vec0:dim*4][vec1:dim*4]...
 *   • Graph stored separately: [header:64][adj0][adj1]...
 *   • Lazy mmap: file is mmap'd on first access, remapped on grow
 *   • Supports datasets larger than RAM via OS page cache
 *
 * Complexity:
 *   • Vector read:  O(1) — direct mmap pointer
 *   • Vector write: O(1) — direct mmap pointer + msync
 *   • Grow:         O(1) amortized (double file size)
 */

use memmap2::{MmapMut, MmapOptions};
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

/// Memory access pattern hint for madvise.
#[derive(Clone, Copy, Debug)]
pub enum AccessPattern {
    /// Sequential scan — tells OS to aggressively read-ahead pages.
    Sequential,
    /// Random access — disables read-ahead, don't evict recently used pages.
    Random,
    /// Default OS behavior.
    Normal,
    /// Hint that the data will be needed soon (prefetch).
    WillNeed,
}

// ── File header ─────────────────────────────────────────────────────

const HEADER_SIZE: usize = 64;
const MAGIC: [u8; 4] = *b"QMVS"; // QM Vector Store
const VERSION: u32 = 1;

// Header layout (64 bytes):
// [magic:4][version:4][dim:4][count:4][capacity:4][_reserved:44]

// ── Mmap Vector Store ───────────────────────────────────────────────

/// Disk-backed vector storage using memory-mapped files.
///
/// Vectors are stored in a flat binary file and accessed via mmap.
/// Supports datasets larger than RAM — the OS page cache handles
/// transparent paging of hot vectors into memory.
pub struct MmapVectorStore {
    /// Underlying file handle
    file: File,
    /// Memory-mapped region (lazy, remapped on grow)
    mmap: Option<MmapMut>,
    /// Vector dimensionality
    dim: usize,
    /// Number of vectors currently stored
    count: u32,
    /// Maximum vectors before file needs to grow
    capacity: u32,
}

impl MmapVectorStore {
    /// Open or create a vector store at the given path.
    pub fn open(path: &Path, dim: usize) -> io::Result<Self> {
        let exists = path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        let mut store = MmapVectorStore {
            file,
            mmap: None,
            dim,
            count: 0,
            capacity: 0,
        };

        if exists && std::fs::metadata(path)?.len() >= HEADER_SIZE as u64 {
            store.load_header()?;
            store.remap()?;
        } else {
            // Initialize with capacity for 1024 vectors
            store.init(1024)?;
        }

        Ok(store)
    }

    /// Initialize a new file with the given initial capacity.
    fn init(&mut self, initial_capacity: u32) -> io::Result<()> {
        let file_size = self.file_size_for(initial_capacity);
        self.file.set_len(file_size as u64)?;
        self.capacity = initial_capacity;
        self.count = 0;
        self.write_header()?;
        self.remap()
    }

    /// Compute file size needed for `n` vectors.
    fn file_size_for(&self, n: u32) -> usize {
        HEADER_SIZE + (n as usize) * self.dim * std::mem::size_of::<f32>()
    }

    /// Offset of vector `id` in the file.
    fn offset_of(&self, id: u32) -> usize {
        HEADER_SIZE + (id as usize) * self.dim * std::mem::size_of::<f32>()
    }

    /// Write header to the mmap'd region.
    fn write_header(&mut self) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&MAGIC)?;
        self.file.write_all(&VERSION.to_le_bytes())?;
        self.file.write_all(&(self.dim as u32).to_le_bytes())?;
        self.file.write_all(&self.count.to_le_bytes())?;
        self.file.write_all(&self.capacity.to_le_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    /// Read header from file.
    fn load_header(&mut self) -> io::Result<()> {
        use std::io::Read;
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = [0u8; HEADER_SIZE];
        self.file.read_exact(&mut buf)?;

        if &buf[0..4] != &MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid magic bytes",
            ));
        }
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported version",
            ));
        }
        self.dim = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
        self.count = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        self.capacity = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        Ok(())
    }

    /// (Re)map the file into memory.
    fn remap(&mut self) -> io::Result<()> {
        // Drop old mapping first
        self.mmap = None;
        let file_len = self.file.metadata()?.len();
        if file_len == 0 {
            return Ok(());
        }
        let mmap = unsafe {
            MmapOptions::new()
                .len(file_len as usize)
                .map_mut(&self.file)?
        };
        self.mmap = Some(mmap);
        Ok(())
    }

    /// Grow the file to accommodate more vectors (doubles capacity).
    fn grow(&mut self) -> io::Result<()> {
        let new_capacity = (self.capacity * 2).max(1024);
        let new_size = self.file_size_for(new_capacity);
        // Drop mmap before resizing
        self.mmap = None;
        self.file.set_len(new_size as u64)?;
        self.capacity = new_capacity;
        self.write_header()?;
        self.remap()
    }

    /// Store a vector at the given ID slot.
    /// If `id >= count`, count is updated. If `id >= capacity`, file grows.
    pub fn put(&mut self, id: u32, vector: &[f32]) -> io::Result<()> {
        assert_eq!(vector.len(), self.dim, "Vector dimension mismatch");

        while id >= self.capacity {
            self.grow()?;
        }

        let offset = self.offset_of(id);
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                vector.as_ptr() as *const u8,
                vector.len() * std::mem::size_of::<f32>(),
            )
        };

        if let Some(mmap) = &mut self.mmap {
            mmap[offset..offset + bytes.len()].copy_from_slice(bytes);
        }

        if id >= self.count {
            self.count = id + 1;
            // Update count in header (bytes 12..16)
            if let Some(mmap) = &mut self.mmap {
                mmap[12..16].copy_from_slice(&self.count.to_le_bytes());
            }
        }

        Ok(())
    }

    /// Read a vector by ID. Returns a slice into the mmap region.
    pub fn get(&self, id: u32) -> Option<&[f32]> {
        if id >= self.count {
            return None;
        }
        let mmap = self.mmap.as_ref()?;
        let offset = self.offset_of(id);
        let byte_len = self.dim * std::mem::size_of::<f32>();
        if offset + byte_len > mmap.len() {
            return None;
        }
        let slice = &mmap[offset..offset + byte_len];
        Some(unsafe { std::slice::from_raw_parts(slice.as_ptr() as *const f32, self.dim) })
    }

    /// Flush mmap'd changes to disk.
    pub fn flush(&self) -> io::Result<()> {
        if let Some(mmap) = &self.mmap {
            mmap.flush()?;
        }
        Ok(())
    }

    /// Advise the OS kernel on memory access patterns for this vector store.
    ///
    /// - `Sequential`: Use before bulk sequential writes/reads. Enables aggressive
    ///   read-ahead, which prevents page faults during linear scans.
    /// - `Random`: Use for point queries (e.g., HNSW neighbor lookups). Disables
    ///   read-ahead to avoid polluting the page cache with unused pages.
    /// - `WillNeed`: Prefetch the entire mapping into page cache. Use before a
    ///   known-hot working set needs to be in memory.
    /// - `Normal`: Reset to default OS behavior.
    ///
    /// On datasets larger than physical RAM, correct madvise hints prevent the OS
    /// from evicting hot pages during sequential bulk operations.
    pub fn advise(&self, pattern: AccessPattern) -> io::Result<()> {
        if let Some(mmap) = &self.mmap {
            #[cfg(unix)]
            {
                let advice = match pattern {
                    AccessPattern::Sequential => memmap2::Advice::Sequential,
                    AccessPattern::Random => memmap2::Advice::Random,
                    AccessPattern::Normal => memmap2::Advice::Normal,
                    AccessPattern::WillNeed => memmap2::Advice::WillNeed,
                };
                mmap.advise(advice)?;
            }
            #[cfg(not(unix))]
            {
                let _ = pattern; // madvise is Unix-only
            }
        }
        Ok(())
    }

    /// Advise a specific range of vector IDs.
    /// Useful for prefetching a known working set without touching the entire file.
    pub fn advise_range(
        &self,
        start_id: u32,
        end_id: u32,
        pattern: AccessPattern,
    ) -> io::Result<()> {
        if let Some(mmap) = &self.mmap {
            #[cfg(unix)]
            {
                let start_off = self.offset_of(start_id);
                let end_off = self.offset_of(end_id.min(self.count));
                if end_off > start_off && end_off <= mmap.len() {
                    let advice = match pattern {
                        AccessPattern::Sequential => memmap2::Advice::Sequential,
                        AccessPattern::Random => memmap2::Advice::Random,
                        AccessPattern::Normal => memmap2::Advice::Normal,
                        AccessPattern::WillNeed => memmap2::Advice::WillNeed,
                    };
                    mmap.advise_range(advice, start_off, end_off - start_off)?;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (start_id, end_id, pattern);
            }
        }
        Ok(())
    }

    /// Number of vectors stored.
    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Dimensionality.
    pub fn dim(&self) -> usize {
        self.dim
    }
}

// ── Graph Adjacency Store ───────────────────────────────────────────

/// Disk-backed HNSW graph adjacency storage.
///
/// Layout: flat file with fixed-size neighbor slots per node per level.
/// [header:64][node0_L0:M0*4][node0_L1:M*4]...[node1_L0:M0*4]...
///
/// Each neighbor slot stores a u32 node ID (0xFFFFFFFF = empty).
const EMPTY_NEIGHBOR: u32 = u32::MAX;

pub struct MmapGraphStore {
    file: File,
    mmap: Option<MmapMut>,
    /// Max neighbors at level 0
    m0: usize,
    /// Max neighbors at other levels
    m: usize,
    /// Max levels
    max_levels: usize,
    /// Bytes per node (all levels combined)
    node_stride: usize,
    /// Number of nodes with allocated slots
    capacity: u32,
    /// Actual node count
    count: u32,
}

impl MmapGraphStore {
    /// Open or create a graph store.
    pub fn open(path: &Path, m0: usize, m: usize, max_levels: usize) -> io::Result<Self> {
        let exists = path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        // Each node: level0 has m0 slots, levels 1..max_levels have m slots each
        let node_stride = (m0 + m * max_levels) * std::mem::size_of::<u32>();

        let mut store = MmapGraphStore {
            file,
            mmap: None,
            m0,
            m,
            max_levels,
            node_stride,
            capacity: 0,
            count: 0,
        };

        if exists && std::fs::metadata(path)?.len() >= HEADER_SIZE as u64 {
            store.load_header()?;
            store.remap()?;
        } else {
            store.init(1024)?;
        }

        Ok(store)
    }

    fn init(&mut self, initial_capacity: u32) -> io::Result<()> {
        let file_size = HEADER_SIZE + (initial_capacity as usize) * self.node_stride;
        self.file.set_len(file_size as u64)?;
        self.capacity = initial_capacity;
        self.count = 0;

        // Write header
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&MAGIC)?;
        self.file.write_all(&VERSION.to_le_bytes())?;
        self.file.write_all(&(self.m0 as u32).to_le_bytes())?;
        self.file.write_all(&(self.m as u32).to_le_bytes())?;
        self.file
            .write_all(&(self.max_levels as u32).to_le_bytes())?;
        self.file.write_all(&self.count.to_le_bytes())?;
        self.file.write_all(&self.capacity.to_le_bytes())?;
        self.file.flush()?;

        self.remap()?;
        // Initialize all neighbor slots to EMPTY
        if let Some(mmap) = &mut self.mmap {
            for i in HEADER_SIZE..mmap.len() {
                mmap[i] = 0xFF;
            }
        }
        Ok(())
    }

    fn load_header(&mut self) -> io::Result<()> {
        use std::io::Read;
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = [0u8; HEADER_SIZE];
        self.file.read_exact(&mut buf)?;
        if &buf[0..4] != &MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Bad magic"));
        }
        self.m0 = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
        self.m = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
        self.max_levels = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as usize;
        self.count = u32::from_le_bytes(buf[20..24].try_into().unwrap());
        self.capacity = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        self.node_stride = (self.m0 + self.m * self.max_levels) * std::mem::size_of::<u32>();
        Ok(())
    }

    fn remap(&mut self) -> io::Result<()> {
        self.mmap = None;
        let file_len = self.file.metadata()?.len();
        if file_len == 0 {
            return Ok(());
        }
        let mmap = unsafe {
            MmapOptions::new()
                .len(file_len as usize)
                .map_mut(&self.file)?
        };
        self.mmap = Some(mmap);
        Ok(())
    }

    fn grow(&mut self) -> io::Result<()> {
        let new_capacity = (self.capacity * 2).max(1024);
        let new_size = HEADER_SIZE + (new_capacity as usize) * self.node_stride;
        self.mmap = None;
        self.file.set_len(new_size as u64)?;
        self.capacity = new_capacity;
        self.remap()?;
        // Initialize new slots to EMPTY
        let old_end = HEADER_SIZE + (self.count as usize) * self.node_stride;
        if let Some(mmap) = &mut self.mmap {
            for i in old_end..mmap.len() {
                mmap[i] = 0xFF;
            }
        }
        Ok(())
    }

    /// Offset of node `id`'s neighbor data in the file.
    fn node_offset(&self, id: u32) -> usize {
        HEADER_SIZE + (id as usize) * self.node_stride
    }

    /// Offset of level `level`'s neighbor slot within a node's region.
    fn level_offset(&self, level: usize) -> usize {
        if level == 0 {
            0
        } else {
            self.m0 * 4 + (level - 1) * self.m * 4
        }
    }

    /// Max neighbors at a given level.
    fn max_neighbors(&self, level: usize) -> usize {
        if level == 0 {
            self.m0
        } else {
            self.m
        }
    }

    /// Set neighbors for a node at a given level.
    pub fn set_neighbors(&mut self, id: u32, level: usize, neighbors: &[u32]) -> io::Result<()> {
        while id >= self.capacity {
            self.grow()?;
        }
        if id >= self.count {
            self.count = id + 1;
            // Update count in header (bytes 20..24 in graph store layout)
            if let Some(mmap) = &mut self.mmap {
                mmap[20..24].copy_from_slice(&self.count.to_le_bytes());
            }
        }

        let max_n = self.max_neighbors(level);
        let base = self.node_offset(id) + self.level_offset(level);

        if let Some(mmap) = &mut self.mmap {
            for i in 0..max_n {
                let val = if i < neighbors.len() {
                    neighbors[i]
                } else {
                    EMPTY_NEIGHBOR
                };
                let off = base + i * 4;
                if off + 4 <= mmap.len() {
                    mmap[off..off + 4].copy_from_slice(&val.to_le_bytes());
                }
            }
        }
        Ok(())
    }

    /// Get neighbors for a node at a given level.
    pub fn get_neighbors(&self, id: u32, level: usize) -> Vec<u32> {
        if id >= self.count {
            return Vec::new();
        }
        let max_n = self.max_neighbors(level);
        let base = self.node_offset(id) + self.level_offset(level);
        let mut result = Vec::with_capacity(max_n);

        if let Some(mmap) = &self.mmap {
            for i in 0..max_n {
                let off = base + i * 4;
                if off + 4 > mmap.len() {
                    break;
                }
                let val = u32::from_le_bytes(mmap[off..off + 4].try_into().unwrap());
                if val != EMPTY_NEIGHBOR {
                    result.push(val);
                }
            }
        }
        result
    }

    /// Flush mmap changes to disk.
    pub fn flush(&self) -> io::Result<()> {
        if let Some(mmap) = &self.mmap {
            mmap.flush()?;
        }
        Ok(())
    }

    /// Advise the OS kernel on access patterns for the graph adjacency store.
    pub fn advise(&self, pattern: AccessPattern) -> io::Result<()> {
        if let Some(mmap) = &self.mmap {
            #[cfg(unix)]
            {
                let advice = match pattern {
                    AccessPattern::Sequential => memmap2::Advice::Sequential,
                    AccessPattern::Random => memmap2::Advice::Random,
                    AccessPattern::Normal => memmap2::Advice::Normal,
                    AccessPattern::WillNeed => memmap2::Advice::WillNeed,
                };
                mmap.advise(advice)?;
            }
            #[cfg(not(unix))]
            {
                let _ = pattern;
            }
        }
        Ok(())
    }

    pub fn len(&self) -> u32 {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_mmap_vector_store_basic() {
        let dir = std::env::temp_dir().join("qm_mmap_test_vec");
        let _ = fs::remove_file(&dir);
        let mut store = MmapVectorStore::open(&dir, 4).unwrap();

        let v1 = vec![1.0f32, 2.0, 3.0, 4.0];
        let v2 = vec![5.0f32, 6.0, 7.0, 8.0];
        store.put(0, &v1).unwrap();
        store.put(1, &v2).unwrap();

        assert_eq!(store.len(), 2);
        assert_eq!(store.get(0).unwrap(), &v1[..]);
        assert_eq!(store.get(1).unwrap(), &v2[..]);
        assert!(store.get(2).is_none());

        store.flush().unwrap();
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn test_mmap_vector_store_grow() {
        let dir = std::env::temp_dir().join("qm_mmap_test_grow");
        let _ = fs::remove_file(&dir);
        let mut store = MmapVectorStore::open(&dir, 8).unwrap();

        // Insert more than initial capacity (1024)
        for i in 0..2048u32 {
            let v: Vec<f32> = (0..8).map(|j| (i * 8 + j) as f32).collect();
            store.put(i, &v).unwrap();
        }
        assert_eq!(store.len(), 2048);

        // Verify first and last
        let first = store.get(0).unwrap();
        assert_eq!(first[0], 0.0);
        let last = store.get(2047).unwrap();
        assert_eq!(last[0], (2047 * 8) as f32);

        store.flush().unwrap();
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn test_mmap_vector_store_reopen() {
        let dir = std::env::temp_dir().join("qm_mmap_test_reopen");
        let _ = fs::remove_file(&dir);

        // Write
        {
            let mut store = MmapVectorStore::open(&dir, 4).unwrap();
            store.put(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
            store.put(1, &[5.0, 6.0, 7.0, 8.0]).unwrap();
            store.flush().unwrap();
        }

        // Reopen and verify
        {
            let store = MmapVectorStore::open(&dir, 4).unwrap();
            assert_eq!(store.len(), 2);
            assert_eq!(store.get(0).unwrap(), &[1.0, 2.0, 3.0, 4.0]);
            assert_eq!(store.get(1).unwrap(), &[5.0, 6.0, 7.0, 8.0]);
        }

        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn test_mmap_graph_store_basic() {
        let dir = std::env::temp_dir().join("qm_mmap_test_graph");
        let _ = fs::remove_file(&dir);

        let mut store = MmapGraphStore::open(&dir, 32, 16, 4).unwrap();
        store.set_neighbors(0, 0, &[1, 2, 3]).unwrap();
        store.set_neighbors(0, 1, &[4, 5]).unwrap();
        store.set_neighbors(1, 0, &[0, 2]).unwrap();

        assert_eq!(store.get_neighbors(0, 0), vec![1, 2, 3]);
        assert_eq!(store.get_neighbors(0, 1), vec![4, 5]);
        assert_eq!(store.get_neighbors(1, 0), vec![0, 2]);
        assert!(store.get_neighbors(2, 0).is_empty());

        store.flush().unwrap();
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn test_mmap_graph_store_reopen() {
        let dir = std::env::temp_dir().join("qm_mmap_test_graph_reopen");
        let _ = fs::remove_file(&dir);

        {
            let mut store = MmapGraphStore::open(&dir, 32, 16, 4).unwrap();
            store.set_neighbors(0, 0, &[10, 20, 30]).unwrap();
            store.flush().unwrap();
        }

        {
            let store = MmapGraphStore::open(&dir, 32, 16, 4).unwrap();
            assert_eq!(store.get_neighbors(0, 0), vec![10, 20, 30]);
        }

        let _ = fs::remove_file(&dir);
    }
}
