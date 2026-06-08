/*
 * Count-Min Sketch — Frequency Estimation
 *
 * Parameters: width=2048, depth=5 → ε ≈ e/2048 ≈ 0.0013, δ = e^(-5) ≈ 0.0067
 * Memory: 2048 × 5 × 8 = 80 KB
 *
 * Operations:
 *   • add(item, count): increment counters at depth hash positions
 *   • estimate(item): return minimum counter across all depths
 *   • merge(other): element-wise max of counters
 */

use std::hash::{Hash, Hasher};

const DEFAULT_WIDTH: usize = 2048;
const DEFAULT_DEPTH: usize = 5;

/// Count-Min Sketch for frequency estimation.
/// Error bound: ε ≈ e/width (~0.0013 for width=2048)
/// Failure probability: δ = e^(-depth) (~0.007 for depth=5)
pub struct CountMinSketch {
    counters: Vec<Vec<u64>>,
    width: usize,
    depth: usize,
    total_count: u64,
}

impl CountMinSketch {
    pub fn new() -> Self {
        Self::with_dimensions(DEFAULT_WIDTH, DEFAULT_DEPTH)
    }

    pub fn with_dimensions(width: usize, depth: usize) -> Self {
        Self {
            counters: vec![vec![0u64; width]; depth],
            width,
            depth,
            total_count: 0,
        }
    }

    /// Create from desired error bounds.
    /// ε = e/width, δ = e^(-depth)
    pub fn from_error_bounds(epsilon: f64, delta: f64) -> Self {
        let width = (std::f64::consts::E / epsilon).ceil() as usize;
        let depth = (-delta.ln()).ceil() as usize;
        Self::with_dimensions(width.max(16), depth.max(2))
    }

    /// Hash item for a given depth row.
    fn hash_at<T: Hash>(item: &T, seed: usize) -> u64 {
        let mut hasher = ahash::AHasher::default();
        seed.hash(&mut hasher);
        item.hash(&mut hasher);
        hasher.finish()
    }

    /// Add an item with count.
    pub fn add<T: Hash>(&mut self, item: &T, count: u64) {
        self.total_count += count;
        for d in 0..self.depth {
            let h = Self::hash_at(item, d) as usize % self.width;
            self.counters[d][h] += count;
        }
    }

    /// Add an item once.
    pub fn increment<T: Hash>(&mut self, item: &T) {
        self.add(item, 1);
    }

    /// Estimate frequency of an item (may over-count, never under-count).
    pub fn estimate<T: Hash>(&self, item: &T) -> u64 {
        let mut min = u64::MAX;
        for d in 0..self.depth {
            let h = Self::hash_at(item, d) as usize % self.width;
            min = min.min(self.counters[d][h]);
        }
        min
    }

    /// Merge another CMS into this one.
    pub fn merge(&mut self, other: &CountMinSketch) {
        assert_eq!(self.width, other.width);
        assert_eq!(self.depth, other.depth);
        for d in 0..self.depth {
            for w in 0..self.width {
                self.counters[d][w] += other.counters[d][w];
            }
        }
        self.total_count += other.total_count;
    }

    /// Total items added.
    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    /// Memory usage in bytes.
    pub fn size_bytes(&self) -> usize {
        self.width * self.depth * 8
    }
}

impl Default for CountMinSketch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_frequency() {
        let mut cms = CountMinSketch::new();
        cms.add(&"hello", 5);
        cms.add(&"world", 3);
        cms.increment(&"hello");

        assert!(cms.estimate(&"hello") >= 6);
        assert!(cms.estimate(&"world") >= 3);
    }

    #[test]
    fn test_never_undercount() {
        let mut cms = CountMinSketch::new();
        for i in 0..1000u64 {
            cms.add(&i, i + 1);
        }
        // Every item's estimate should be >= actual count
        for i in 0..1000u64 {
            assert!(cms.estimate(&i) >= i + 1, "Undercount at {}", i);
        }
    }

    #[test]
    fn test_merge() {
        let mut a = CountMinSketch::new();
        let mut b = CountMinSketch::new();
        a.add(&"x", 10);
        b.add(&"x", 5);
        a.merge(&b);
        assert!(a.estimate(&"x") >= 15);
    }

    #[test]
    fn test_unknown_item() {
        let cms = CountMinSketch::new();
        // Unknown items should return 0 (or very small due to hash collisions)
        let est = cms.estimate(&"never_added");
        assert_eq!(est, 0);
    }

    #[test]
    fn test_from_error_bounds() {
        let cms = CountMinSketch::from_error_bounds(0.001, 0.01);
        assert!(cms.width >= 2000);
        assert!(cms.depth >= 4);
    }
}
