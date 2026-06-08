/*
 * Concurrent HNSW Index — Thread-safe wrapper with RwLock
 *
 * Provides:
 *   • Multiple concurrent readers (search) via parking_lot::RwLock
 *   • Serialized writers (insert) — graph mutations must be sequential
 *   • Arc-based shared ownership for multi-thread access
 *   • Streaming insert: background thread can insert while foreground searches
 *
 * Design rationale:
 *   HNSW graph mutation (bidirectional edge creation + shrink) is inherently
 *   mutating. Per-node fine-grained locking adds complexity with marginal gain
 *   since single insert is already fast (~1ms). The RwLock approach is simple
 *   and correct: N concurrent searches + serialized inserts.
 *
 *   For high-throughput writes, use batch_insert() which parallelizes
 *   distance computation internally via Rayon.
 */

use crate::index::hnsw::{HnswConfig, HnswIndex};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;

/// Thread-safe concurrent HNSW index.
///
/// Wraps `HnswIndex` with `Arc<RwLock>` for safe multi-thread access.
/// Multiple searches can run in parallel. Inserts acquire an exclusive lock.
///
/// # Example
/// ```ignore
/// let idx = ConcurrentHnswIndex::new(128, HnswConfig::default());
/// let idx_clone = idx.clone();
///
/// // Writer thread
/// std::thread::spawn(move || {
///     for i in 0..10000 {
///         idx_clone.insert(i, random_vector(128));
///     }
/// });
///
/// // Reader threads can search concurrently
/// let results = idx.search(&query, 10);
/// ```
#[derive(Clone)]
pub struct ConcurrentHnswIndex {
    inner: Arc<RwLock<HnswIndex>>,
    /// Pending write buffer for coalesced inserts.
    /// Multiple threads push to this buffer; when it reaches flush_threshold
    /// or flush_writes() is called, all pending vectors are inserted under
    /// a single write lock acquisition.
    pending: Arc<Mutex<Vec<(u32, Vec<f32>)>>>,
    /// Number of pending vectors before auto-flush triggers.
    flush_threshold: usize,
}

/// Default auto-flush threshold — flush pending writes every 256 vectors.
const DEFAULT_FLUSH_THRESHOLD: usize = 256;

impl ConcurrentHnswIndex {
    /// Create a new concurrent index.
    pub fn new(dim: usize, config: HnswConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HnswIndex::new(dim, config))),
            pending: Arc::new(Mutex::new(Vec::new())),
            flush_threshold: DEFAULT_FLUSH_THRESHOLD,
        }
    }

    /// Create with default config.
    pub fn with_default_config(dim: usize) -> Self {
        Self::new(dim, HnswConfig::default())
    }

    /// Create with a custom flush threshold for write coalescing.
    pub fn with_flush_threshold(dim: usize, config: HnswConfig, flush_threshold: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HnswIndex::new(dim, config))),
            pending: Arc::new(Mutex::new(Vec::new())),
            flush_threshold,
        }
    }

    /// Insert a single vector. Acquires write lock.
    pub fn insert(&self, id: u32, vector: Vec<f32>) {
        self.inner.write().insert(id, vector);
    }

    /// Buffered insert — pushes to a pending buffer instead of acquiring
    /// write lock immediately. When the buffer exceeds `flush_threshold`,
    /// all pending vectors are flushed as a single batch_insert under one
    /// write lock, minimizing lock contention across many writer threads.
    ///
    /// Returns true if an auto-flush was triggered.
    pub fn insert_buffered(&self, id: u32, vector: Vec<f32>) -> bool {
        let should_flush;
        {
            let mut buf = self.pending.lock();
            buf.push((id, vector));
            should_flush = buf.len() >= self.flush_threshold;
        }
        if should_flush {
            self.flush_writes();
            true
        } else {
            false
        }
    }

    /// Flush all pending buffered writes under a single write lock.
    /// This is the "group commit" for HNSW: one lock acquisition for N inserts.
    pub fn flush_writes(&self) {
        let batch: Vec<(u32, Vec<f32>)> = {
            let mut buf = self.pending.lock();
            std::mem::take(&mut *buf)
        };
        if !batch.is_empty() {
            self.inner.write().batch_insert(batch);
        }
    }

    /// Batch insert vectors. Acquires write lock for the duration.
    /// Distance computation is parallelized internally via Rayon.
    pub fn batch_insert(&self, vectors: Vec<(u32, Vec<f32>)>) {
        self.inner.write().batch_insert(vectors);
    }

    /// Search for nearest neighbors. Acquires read lock (concurrent with other reads).
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(u32, f32)> {
        self.inner.read().search(query, top_k)
    }

    /// Number of vectors. Acquires read lock.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Check if empty. Acquires read lock.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Dimensionality. Acquires read lock.
    pub fn dim(&self) -> usize {
        self.inner.read().dim()
    }

    /// Current max level in the graph. Acquires read lock.
    pub fn levels(&self) -> usize {
        self.inner.read().levels()
    }

    /// Get a reference to the underlying RwLock for advanced usage.
    pub fn inner(&self) -> &Arc<RwLock<HnswIndex>> {
        &self.inner
    }

    /// Number of vectors currently in the pending buffer (not yet flushed).
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;
    use std::thread;

    fn random_vector(dim: usize) -> Vec<f32> {
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen::<f32>()).collect()
    }

    #[test]
    fn test_concurrent_insert_and_search() {
        let dim = 16;
        let idx = ConcurrentHnswIndex::with_default_config(dim);

        // Insert sequentially first (need some data for search)
        for i in 0..100u32 {
            idx.insert(i, random_vector(dim));
        }
        assert_eq!(idx.len(), 100);

        // Start concurrent readers + writers
        let idx_write = idx.clone();
        let writer = thread::spawn(move || {
            for i in 100..200u32 {
                idx_write.insert(i, random_vector(dim));
            }
        });

        let idx_read = idx.clone();
        let reader = thread::spawn(move || {
            let mut count = 0;
            for _ in 0..50 {
                let results = idx_read.search(&random_vector(dim), 5);
                count += results.len();
            }
            count
        });

        writer.join().unwrap();
        let search_count = reader.join().unwrap();

        // All inserts completed
        assert_eq!(idx.len(), 200);
        // Searches returned results
        assert!(search_count > 0);
    }

    #[test]
    fn test_concurrent_multiple_readers() {
        let dim = 8;
        let idx = ConcurrentHnswIndex::with_default_config(dim);

        for i in 0..100u32 {
            idx.insert(i, random_vector(dim));
        }

        // Spawn 8 concurrent reader threads
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let idx_clone = idx.clone();
                thread::spawn(move || {
                    let mut total = 0;
                    for _ in 0..100 {
                        let results = idx_clone.search(&random_vector(dim), 5);
                        total += results.len();
                    }
                    total
                })
            })
            .collect();

        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        // 8 threads × 100 searches × 5 results each
        assert_eq!(total, 8 * 100 * 5);
    }

    #[test]
    fn test_concurrent_batch_insert() {
        let dim = 16;
        let idx = ConcurrentHnswIndex::with_default_config(dim);

        let vecs: Vec<(u32, Vec<f32>)> = (0..500).map(|i| (i, random_vector(dim))).collect();
        idx.batch_insert(vecs);

        assert_eq!(idx.len(), 500);
        let results = idx.search(&random_vector(dim), 10);
        assert_eq!(results.len(), 10);
    }

    #[test]
    fn test_concurrent_clone_shared() {
        let dim = 8;
        let idx = ConcurrentHnswIndex::with_default_config(dim);

        // Clone shares the same underlying data
        let idx2 = idx.clone();
        idx.insert(0, random_vector(dim));

        assert_eq!(idx2.len(), 1); // Both see the insert
    }

    #[test]
    fn test_concurrent_buffered_insert() {
        let dim = 16;
        let idx = ConcurrentHnswIndex::with_flush_threshold(dim, HnswConfig::default(), 64);

        // Buffered insert under threshold — no auto-flush
        for i in 0..63u32 {
            let flushed = idx.insert_buffered(i, random_vector(dim));
            assert!(!flushed);
        }
        // Index is still empty (pending buffer not flushed)
        assert_eq!(idx.len(), 0);
        assert_eq!(idx.pending_count(), 63);

        // This triggers auto-flush (64th vector hits threshold)
        let flushed = idx.insert_buffered(63, random_vector(dim));
        assert!(flushed);
        assert_eq!(idx.len(), 64);
        assert_eq!(idx.pending_count(), 0);
    }

    #[test]
    fn test_concurrent_buffered_multi_thread() {
        let dim = 8;
        let idx = ConcurrentHnswIndex::with_flush_threshold(dim, HnswConfig::default(), 100);

        // 4 threads each insert 50 vectors via buffered path
        let handles: Vec<_> = (0..4u32)
            .map(|t| {
                let idx_clone = idx.clone();
                thread::spawn(move || {
                    for i in 0..50u32 {
                        idx_clone.insert_buffered(t * 1000 + i, random_vector(dim));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Flush remaining
        idx.flush_writes();

        assert_eq!(idx.len(), 200);
        let results = idx.search(&random_vector(dim), 5);
        assert_eq!(results.len(), 5);
    }
}
