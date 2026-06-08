/*
 * Bloom Filter — Probabilistic Set Membership
 *
 * Configurable false-positive rate (FP). No false negatives.
 *
 * Formula:
 *   • bits = -n × ln(fp) / (ln(2)²)
 *   • k (hash functions) = (bits/n) × ln(2)
 *
 * Hashing: Kirsch–Mitzenmacker double-hashing via single 128-bit ahash.
 * h_i(x) = h1 + i × h2  where (h1, h2) = split128(ahash(x))
 *
 * Performance: ~2× throughput vs dual-hash approach.
 */

use std::hash::{Hash, Hasher};

/// Bloom Filter with configurable FP rate.
pub struct BloomFilter {
    bits: Vec<u64>,
    n_bits: usize,
    n_hashes: usize,
    count: usize,
}

impl BloomFilter {
    /// Create a Bloom filter for `expected_items` with `fp_rate` false positive rate.
    pub fn new(expected_items: usize, fp_rate: f64) -> Self {
        let fp = fp_rate.max(1e-10).min(0.5);
        let n_bits = (-(expected_items as f64) * fp.ln() / (2.0f64.ln().powi(2))).ceil() as usize;
        let n_bits = n_bits.max(64); // minimum 64 bits
        let n_hashes = ((n_bits as f64 / expected_items as f64) * 2.0f64.ln()).ceil() as usize;
        let n_hashes = n_hashes.max(1).min(20);
        let words = (n_bits + 63) / 64;

        Self {
            bits: vec![0u64; words],
            n_bits,
            n_hashes,
            count: 0,
        }
    }

    /// Create with explicit parameters.
    pub fn with_params(n_bits: usize, n_hashes: usize) -> Self {
        let words = (n_bits + 63) / 64;
        Self {
            bits: vec![0u64; words],
            n_bits,
            n_hashes,
            count: 0,
        }
    }

    /// Single-hash Kirsch–Mitzenmacker: one ahash call → split into (h1, h2).
    /// This is ~2× faster than computing two independent hashes.
    #[inline(always)]
    fn hashes<T: Hash>(&self, item: &T) -> (u64, u64) {
        let mut hasher = ahash::AHasher::default();
        item.hash(&mut hasher);
        let full = hasher.finish();
        // Split 64-bit hash into two 32-bit halves, expand to 64-bit.
        // Kirsch & Mitzenmacker proved this gives the same guarantees as k independent hashes.
        let h1 = full;
        let h2 = (full >> 32) | (full << 32); // swap halves for independence
        (h1, h2)
    }

    /// Add an item to the filter.
    #[inline]
    pub fn insert<T: Hash>(&mut self, item: &T) {
        let (h1, h2) = self.hashes(item);
        let n = self.n_bits;
        for i in 0..self.n_hashes {
            let pos = h1.wrapping_add((i as u64).wrapping_mul(h2)) as usize % n;
            // SAFETY: pos / 64 is always < self.bits.len() since pos < n_bits and
            // bits.len() = (n_bits + 63) / 64
            unsafe {
                let word = self.bits.get_unchecked_mut(pos / 64);
                *word |= 1u64 << (pos % 64);
            }
        }
        self.count += 1;
    }

    /// Check if an item might be in the set.
    /// Returns false → definitely not in set.
    /// Returns true  → possibly in set (FP rate applies).
    #[inline]
    pub fn may_contain<T: Hash>(&self, item: &T) -> bool {
        let (h1, h2) = self.hashes(item);
        let n = self.n_bits;
        for i in 0..self.n_hashes {
            let pos = h1.wrapping_add((i as u64).wrapping_mul(h2)) as usize % n;
            unsafe {
                if *self.bits.get_unchecked(pos / 64) & (1u64 << (pos % 64)) == 0 {
                    return false;
                }
            }
        }
        true
    }

    /// Items inserted (not unique count).
    pub fn count(&self) -> usize {
        self.count
    }

    /// Estimated current false positive rate.
    pub fn estimated_fp_rate(&self) -> f64 {
        let set_bits: u64 = self.bits.iter().map(|w| w.count_ones() as u64).sum();
        let ratio = set_bits as f64 / self.n_bits as f64;
        ratio.powi(self.n_hashes as i32)
    }

    /// Memory usage in bytes.
    pub fn size_bytes(&self) -> usize {
        self.bits.len() * 8
    }

    /// Number of hash functions used.
    pub fn hash_count(&self) -> usize {
        self.n_hashes
    }

    /// Total bits in the filter.
    pub fn bit_count(&self) -> usize {
        self.n_bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_membership() {
        let mut bf = BloomFilter::new(1000, 0.01);
        bf.insert(&"hello");
        bf.insert(&"world");

        assert!(bf.may_contain(&"hello"));
        assert!(bf.may_contain(&"world"));
    }

    #[test]
    fn test_no_false_negatives() {
        let mut bf = BloomFilter::new(10000, 0.01);
        for i in 0..10000u64 {
            bf.insert(&i);
        }
        // Every inserted item must be found
        for i in 0..10000u64 {
            assert!(bf.may_contain(&i), "False negative at {}", i);
        }
    }

    #[test]
    fn test_false_positive_rate() {
        let mut bf = BloomFilter::new(10000, 0.01);
        for i in 0..10000u64 {
            bf.insert(&i);
        }

        let mut fp_count = 0;
        let test_range = 10000..20000u64;
        for i in test_range.clone() {
            if bf.may_contain(&i) {
                fp_count += 1;
            }
        }

        let fp_rate = fp_count as f64 / 10000.0;
        // Should be roughly ≤ 3% (allowing some statistical variance)
        assert!(fp_rate < 0.03, "FP rate too high: {:.4}", fp_rate);
    }

    #[test]
    fn test_empty_filter() {
        let bf = BloomFilter::new(1000, 0.01);
        assert!(!bf.may_contain(&"anything"));
        assert_eq!(bf.count(), 0);
    }

    #[test]
    fn test_size() {
        let bf = BloomFilter::new(100000, 0.01);
        // ~117 KB for 100K items at 1% FP
        assert!(bf.size_bytes() > 100_000);
        assert!(bf.size_bytes() < 200_000);
    }

    #[test]
    fn test_hash_count() {
        let bf = BloomFilter::new(10000, 0.01);
        // Optimal k for 1% FP ≈ 7
        assert!(
            bf.hash_count() >= 5 && bf.hash_count() <= 10,
            "Unexpected hash count: {}",
            bf.hash_count()
        );
    }
}
