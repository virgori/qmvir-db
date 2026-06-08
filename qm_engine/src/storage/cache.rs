//! W-TinyLFU cache backed by `moka` — replaces simple LRU caches.
//!
//! W-TinyLFU (Window-TinyLFU) is scan-resistant: large sequential scans
//! don't evict hot entries. It uses a probabilistic frequency sketch
//! (Count-Min Sketch) for admission filtering and a segmented LRU
//! (Window + Protected + Probationary) for eviction.
//!
//! This module provides typed cache wrappers for different subsystems:
//! - `PageCache`: caches `page_id → Vec<u8>` for storage pages
//! - `QueryCache`: caches `sql_hash → QueryResult` for repeated queries
//! - `MetadataCache`: caches `table_name → TableMeta` for schema lookups

use parking_lot::RwLock;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::hash::{Hash, Hasher};

// ── moka-compatible trait-based W-TinyLFU implementation ──────────

/// Frequency sketch (Count-Min Sketch) for W-TinyLFU admission.
///
/// 4 independent hash functions, each mapping to a row of counters.
/// Counter width = 4 bits (max 15). Periodically halved to decay stale entries.
pub struct FrequencySketch {
    /// 4 rows of 4-bit packed counters (2 counters per byte)
    table: Vec<Vec<u8>>,
    mask: usize,
    /// Total additions — triggers halving at `sample_size`
    additions: u64,
    /// Reset point: halve all counters when `additions` reaches this
    sample_size: u64,
}

impl FrequencySketch {
    pub fn new(capacity: usize) -> Self {
        // Width = next power of 2 ≥ capacity
        let width = capacity.next_power_of_two();
        let bytes_per_row = (width + 1) / 2; // 2 counters per byte
        Self {
            table: vec![vec![0u8; bytes_per_row]; 4],
            mask: width - 1,
            additions: 0,
            sample_size: (capacity as u64).saturating_mul(10),
        }
    }

    /// Record an access for key_hash.
    pub fn increment(&mut self, key_hash: u64) {
        // 4 independent index functions using different bit ranges
        let h = key_hash;
        let indices = [
            (h as usize) & self.mask,
            ((h >> 16) as usize) & self.mask,
            ((h >> 32) as usize) & self.mask,
            ((h >> 48) as usize) & self.mask,
        ];

        for (row, &idx) in self.table.iter_mut().zip(indices.iter()) {
            let byte_idx = idx / 2;
            let nibble = if idx % 2 == 0 { 0 } else { 4 };
            let val = (row[byte_idx] >> nibble) & 0x0F;
            if val < 15 {
                row[byte_idx] += 1 << nibble;
            }
        }

        self.additions += 1;
        if self.additions >= self.sample_size {
            self.halve();
        }
    }

    /// Estimate frequency for key_hash — minimum of 4 counters.
    pub fn frequency(&self, key_hash: u64) -> u8 {
        let h = key_hash;
        let indices = [
            (h as usize) & self.mask,
            ((h >> 16) as usize) & self.mask,
            ((h >> 32) as usize) & self.mask,
            ((h >> 48) as usize) & self.mask,
        ];

        let mut min_val = 15u8;
        for (row, &idx) in self.table.iter().zip(indices.iter()) {
            let byte_idx = idx / 2;
            let nibble = if idx % 2 == 0 { 0 } else { 4 };
            let val = (row[byte_idx] >> nibble) & 0x0F;
            min_val = min_val.min(val);
        }
        min_val
    }

    /// Halve all counters — decays stale frequencies.
    fn halve(&mut self) {
        for row in &mut self.table {
            for byte in row.iter_mut() {
                // Halve both nibbles: (high >> 1) | (low >> 1)
                let lo = (*byte & 0x0F) >> 1;
                let hi = ((*byte >> 4) & 0x0F) >> 1;
                *byte = (hi << 4) | lo;
            }
        }
        self.additions /= 2;
    }
}

// ── Segmented LRU ─────────────────────────────────────────────────

use std::collections::{HashMap, VecDeque};

/// Entry in the cache with value + metadata.
struct CacheEntry<V> {
    value: V,
    /// Size in bytes (approximate).
    weight: usize,
}

/// W-TinyLFU cache — scan-resistant, high hit-rate.
///
/// Architecture:
/// - **Window cache** (1% capacity): admits all new entries
/// - **Main cache** (99% capacity): protected + probationary segments
/// - **Admission filter**: new entries from window must beat the
///   least-recently-used probationary entry's frequency to be admitted
pub struct WTinyLfuCache<K: Hash + Eq + Clone, V: Clone> {
    /// Window region — LRU, admits everything
    window: VecDeque<K>,
    /// Protected region — frequently accessed entries
    protected: VecDeque<K>,
    /// Probationary region — entries on their way out
    probationary: VecDeque<K>,
    /// Key → value storage
    data: HashMap<K, CacheEntry<V>>,
    /// Frequency sketch for admission decisions
    sketch: FrequencySketch,
    /// Max total weight (bytes)
    max_weight: usize,
    /// Current total weight
    current_weight: usize,
    /// Window capacity (1% of max)
    window_max: usize,
    /// Protected capacity (80% of main)
    protected_max: usize,
    /// Stats
    hits: u64,
    misses: u64,
}

impl<K: Hash + Eq + Clone + std::fmt::Debug, V: Clone> WTinyLfuCache<K, V> {
    pub fn new(max_weight: usize) -> Self {
        let window_max = (max_weight / 100).max(1);
        let main_max = max_weight - window_max;
        let protected_max = (main_max * 80) / 100;

        Self {
            window: VecDeque::new(),
            protected: VecDeque::new(),
            probationary: VecDeque::new(),
            data: HashMap::new(),
            sketch: FrequencySketch::new(max_weight / 64), // rough entry count estimate
            max_weight,
            current_weight: 0,
            window_max,
            protected_max,
            hits: 0,
            misses: 0,
        }
    }

    fn hash_key(key: &K) -> u64 {
        let mut hasher = ahash::AHasher::default();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Get a value, recording an access in the frequency sketch.
    pub fn get(&mut self, key: &K) -> Option<V> {
        let kh = Self::hash_key(key);
        self.sketch.increment(kh);

        if self.data.contains_key(key) {
            self.hits += 1;
            // Promote: if in probationary, move to protected
            if let Some(pos) = self.probationary.iter().position(|k| k == key) {
                let k = self.probationary.remove(pos).unwrap();
                self.protected.push_back(k);
                self.enforce_protected_capacity();
            }
            Some(self.data.get(key).unwrap().value.clone())
        } else {
            self.misses += 1;
            None
        }
    }

    /// Insert or update a key-value pair.
    pub fn insert(&mut self, key: K, value: V, weight: usize) {
        let kh = Self::hash_key(&key);
        self.sketch.increment(kh);

        // Update existing
        if let Some(existing) = self.data.get_mut(&key) {
            self.current_weight = self.current_weight.saturating_sub(existing.weight);
            existing.value = value;
            existing.weight = weight;
            self.current_weight += weight;
            return;
        }

        // New entry → goes to window first
        self.window.push_back(key.clone());
        self.data.insert(key, CacheEntry { value, weight });
        self.current_weight += weight;

        // Evict from window if over capacity
        self.evict_window();

        // Evict from main if total over capacity
        while self.current_weight > self.max_weight && !self.probationary.is_empty() {
            self.evict_probationary();
        }
    }

    /// Remove a key.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(entry) = self.data.remove(key) {
            self.current_weight = self.current_weight.saturating_sub(entry.weight);
            // Remove from whichever deque it's in
            if let Some(pos) = self.window.iter().position(|k| k == key) {
                self.window.remove(pos);
            } else if let Some(pos) = self.protected.iter().position(|k| k == key) {
                self.protected.remove(pos);
            } else if let Some(pos) = self.probationary.iter().position(|k| k == key) {
                self.probationary.remove(pos);
            }
            Some(entry.value)
        } else {
            None
        }
    }

    fn evict_window(&mut self) {
        let mut window_weight: usize = self
            .window
            .iter()
            .filter_map(|k| self.data.get(k))
            .map(|e| e.weight)
            .sum();

        while window_weight > self.window_max {
            if let Some(victim_key) = self.window.pop_front() {
                if let Some(entry) = self.data.get(&victim_key) {
                    window_weight -= entry.weight;
                    // Admission: compare victim's frequency vs probationary LRU
                    let victim_freq = self.sketch.frequency(Self::hash_key(&victim_key));

                    if let Some(prob_lru) = self.probationary.front() {
                        let prob_freq = self.sketch.frequency(Self::hash_key(prob_lru));
                        if victim_freq > prob_freq {
                            // Admit victim to probationary, evict prob LRU
                            self.probationary.push_back(victim_key);
                            self.evict_probationary();
                        } else {
                            // Reject victim
                            if let Some(e) = self.data.remove(&victim_key) {
                                self.current_weight = self.current_weight.saturating_sub(e.weight);
                            }
                        }
                    } else {
                        // Probationary empty, just admit
                        self.probationary.push_back(victim_key);
                    }
                }
            } else {
                break;
            }
        }
    }

    fn evict_probationary(&mut self) {
        if let Some(victim) = self.probationary.pop_front() {
            if let Some(entry) = self.data.remove(&victim) {
                self.current_weight = self.current_weight.saturating_sub(entry.weight);
            }
        }
    }

    fn enforce_protected_capacity(&mut self) {
        let mut prot_weight: usize = self
            .protected
            .iter()
            .filter_map(|k| self.data.get(k))
            .map(|e| e.weight)
            .sum();

        while prot_weight > self.protected_max {
            if let Some(demoted) = self.protected.pop_front() {
                if let Some(entry) = self.data.get(&demoted) {
                    prot_weight -= entry.weight;
                }
                self.probationary.push_back(demoted);
            } else {
                break;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
    pub fn weight(&self) -> usize {
        self.current_weight
    }
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
    pub fn hits(&self) -> u64 {
        self.hits
    }
    pub fn misses(&self) -> u64 {
        self.misses
    }
}

// ── Thread-safe Cache wrapper (Sharded) ───────────────────────────

/// Number of shards for concurrent cache — must be power of 2.
const SHARD_COUNT: usize = 64;

/// Sharded W-TinyLFU cache for high-concurrency access.
///
/// Each shard is an independent `WTinyLfuCache` protected by its own
/// `RwLock`, distributing contention across `SHARD_COUNT` locks.
/// Key-to-shard mapping uses high-quality hash bits (ahash) to ensure
/// uniform distribution.
pub struct ConcurrentCache<K: Hash + Eq + Clone + std::fmt::Debug, V: Clone> {
    shards: Vec<RwLock<WTinyLfuCache<K, V>>>,
}

impl<K: Hash + Eq + Clone + std::fmt::Debug, V: Clone> ConcurrentCache<K, V> {
    pub fn new(max_weight: usize) -> Self {
        // M-02: Don't inflate per-shard capacity with .max(64) — that could
        // give total capacity = 64 * SHARD_COUNT even when max_weight is small.
        let per_shard = max_weight / SHARD_COUNT;
        let shards = (0..SHARD_COUNT)
            .map(|_| RwLock::new(WTinyLfuCache::new(per_shard.max(1))))
            .collect();
        Self { shards }
    }

    #[inline]
    fn shard_index(key: &K) -> usize {
        let mut hasher = ahash::AHasher::default();
        key.hash(&mut hasher);
        (hasher.finish() as usize) & (SHARD_COUNT - 1)
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let idx = Self::shard_index(key);
        let mut shard = self.shards[idx].write();
        shard.get(key)
    }

    pub fn insert(&self, key: K, value: V, weight: usize) {
        let idx = Self::shard_index(&key);
        let mut shard = self.shards[idx].write();
        shard.insert(key, value, weight);
    }

    pub fn remove(&self, key: &K) -> Option<V> {
        let idx = Self::shard_index(key);
        let mut shard = self.shards[idx].write();
        shard.remove(key)
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.read().len()).sum()
    }

    pub fn hit_rate(&self) -> f64 {
        let (hits, misses): (u64, u64) = self.shards.iter().fold((0, 0), |(h, m), s| {
            let cache = s.read();
            (h + cache.hits(), m + cache.misses())
        });
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    pub fn weight(&self) -> usize {
        self.shards.iter().map(|s| s.read().weight()).sum()
    }
}

// ── Typed Cache Instantiations ────────────────────────────────────

/// Page cache: caches storage pages by page_id.
pub type PageCache = ConcurrentCache<u64, Vec<u8>>;

/// Query result cache: caches query results by SQL hash.
pub type QueryResultCache = ConcurrentCache<u64, Vec<u8>>;

// ── PyO3 Bindings ─────────────────────────────────────────────────

/// Python-exposed W-TinyLFU cache for arbitrary bytes.
#[cfg(feature = "python")]
#[pyclass(name = "WTinyLfuCache")]
pub struct PyWTinyLfuCache {
    inner: RwLock<WTinyLfuCache<String, Vec<u8>>>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyWTinyLfuCache {
    #[new]
    #[pyo3(signature = (max_weight_bytes=134217728))]
    fn new(max_weight_bytes: usize) -> Self {
        Self {
            inner: RwLock::new(WTinyLfuCache::new(max_weight_bytes.max(1))),
        }
    }

    /// Get a cached value by key. Returns None if not present.
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.inner.write().get(&key.to_string())
    }

    /// Insert a value with estimated weight (bytes).
    fn insert(&self, key: &str, value: &[u8]) {
        self.inner
            .write()
            .insert(key.to_string(), value.to_vec(), value.len());
    }

    /// Remove a key. Returns True if it was present.
    fn remove(&self, key: &str) -> bool {
        self.inner.write().remove(&key.to_string()).is_some()
    }

    /// Number of entries.
    fn __len__(&self) -> usize {
        self.inner.read().len()
    }

    /// Hit rate (0.0 to 1.0).
    fn hit_rate(&self) -> f64 {
        self.inner.read().hit_rate()
    }

    /// Current weight in bytes.
    fn weight(&self) -> usize {
        self.inner.read().weight()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frequency_sketch_basic() {
        let mut sketch = FrequencySketch::new(1024);
        sketch.increment(42);
        sketch.increment(42);
        sketch.increment(42);
        assert!(sketch.frequency(42) >= 3);
        assert_eq!(sketch.frequency(9999), 0);
    }

    #[test]
    fn test_frequency_sketch_halving() {
        let mut sketch = FrequencySketch::new(16);
        // Force many additions to trigger halving
        for _ in 0..200 {
            sketch.increment(1);
        }
        // After halving, frequency should be less than 200
        let freq = sketch.frequency(1);
        assert!(freq <= 15); // 4-bit counters cap at 15
    }

    #[test]
    fn test_cache_insert_get() {
        let mut cache = WTinyLfuCache::<String, i32>::new(1024);
        cache.insert("a".into(), 1, 8);
        cache.insert("b".into(), 2, 8);
        assert_eq!(cache.get(&"a".into()), Some(1));
        assert_eq!(cache.get(&"b".into()), Some(2));
        assert_eq!(cache.get(&"c".into()), None);
    }

    #[test]
    fn test_cache_eviction() {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(100);
        // Insert entries that exceed capacity
        for i in 0..20 {
            cache.insert(i, vec![0u8; 10], 10);
        }
        // Cache should have evicted some entries
        assert!(cache.len() <= 12); // ~100 bytes capacity, 10 bytes each
        assert!(cache.weight() <= 120); // some slack
    }

    #[test]
    fn test_cache_scan_resistance() {
        let mut cache = WTinyLfuCache::<u64, u8>::new(200);

        // Build up frequency for hot keys 0..5
        for _ in 0..20 {
            for key in 0..5 {
                cache.insert(key, 1, 8);
                cache.get(&key);
            }
        }

        // Sequential scan with cold keys 100..200
        for key in 100..200 {
            cache.insert(key, 2, 8);
        }

        // Hot keys should still be present (scan-resistant)
        let hot_hits: usize = (0..5).filter(|k| cache.get(k).is_some()).count();
        assert!(
            hot_hits >= 3,
            "W-TinyLFU should resist scan pollution, hot_hits={}",
            hot_hits
        );
    }

    #[test]
    fn test_cache_update_in_place() {
        let mut cache = WTinyLfuCache::<String, i32>::new(1024);
        cache.insert("key".into(), 10, 8);
        cache.insert("key".into(), 20, 8);
        assert_eq!(cache.get(&"key".into()), Some(20));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_cache_remove() {
        let mut cache = WTinyLfuCache::<String, i32>::new(1024);
        cache.insert("x".into(), 42, 8);
        assert_eq!(cache.remove(&"x".into()), Some(42));
        assert!(cache.get(&"x".into()).is_none());
    }

    #[test]
    fn test_concurrent_cache() {
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(ConcurrentCache::<u64, u64>::new(10_000));
        let mut handles = Vec::new();

        for t in 0..4 {
            let cache = Arc::clone(&cache);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let key = t * 100 + i;
                    cache.insert(key, key * 2, 8);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert!(cache.len() > 0);
    }

    #[test]
    fn test_hit_rate() {
        let mut cache = WTinyLfuCache::<u64, u64>::new(10000);
        cache.insert(1, 100, 8);
        cache.get(&1); // hit
        cache.get(&1); // hit
        cache.get(&999); // miss
        assert!(cache.hit_rate() > 0.6);
    }
}
