/*
 * HyperLogLog — Cardinality Estimation
 *
 * Parameters: p=14 → 2^14 = 16384 registers → ~0.81% standard error
 * Memory: 16 KB (16384 × 1 byte)
 *
 * Algorithm:
 *   1. Hash item to 64-bit value
 *   2. Use top p bits as register index
 *   3. Count leading zeros in remaining bits + 1 = rank
 *   4. Store max(rank) per register
 *   5. Harmonic mean of 2^(-register) estimates cardinality
 */

use std::hash::{Hash, Hasher};

const P: u32 = 14;
const M: usize = 1 << P; // 16384 registers
const ALPHA_M: f64 = 0.7213 / (1.0 + 1.079 / M as f64);

/// HyperLogLog cardinality estimator with p=14 (~0.81% error).
pub struct HyperLogLog {
    registers: Vec<u8>,
}

impl HyperLogLog {
    pub fn new() -> Self {
        Self {
            registers: vec![0u8; M],
        }
    }

    /// Hash a value using a simple but effective algorithm.
    fn hash_value<T: Hash>(item: &T) -> u64 {
        let mut hasher = ahash::AHasher::default();
        item.hash(&mut hasher);
        hasher.finish()
    }

    /// Add an item to the sketch.
    pub fn add<T: Hash>(&mut self, item: &T) {
        let hash = Self::hash_value(item);
        let idx = (hash >> (64 - P)) as usize;
        let remaining = (hash << P) | (1u64 << (P - 1)); // ensure at least 1 bit
        let rank = remaining.leading_zeros() as u8 + 1;
        if rank > self.registers[idx] {
            self.registers[idx] = rank;
        }
    }

    /// Add a pre-hashed u64 value.
    pub fn add_hash(&mut self, hash: u64) {
        let idx = (hash >> (64 - P)) as usize;
        let remaining = (hash << P) | (1u64 << (P - 1));
        let rank = remaining.leading_zeros() as u8 + 1;
        if rank > self.registers[idx] {
            self.registers[idx] = rank;
        }
    }

    /// Estimate the cardinality.
    pub fn estimate(&self) -> u64 {
        // Raw harmonic mean estimate
        let sum: f64 = self
            .registers
            .iter()
            .map(|&r| 2.0f64.powi(-(r as i32)))
            .sum();
        let raw = ALPHA_M * (M as f64) * (M as f64) / sum;

        // Small range correction (linear counting)
        if raw <= 2.5 * M as f64 {
            let zeros = self.registers.iter().filter(|&&r| r == 0).count();
            if zeros > 0 {
                return (M as f64 * (M as f64 / zeros as f64).ln()) as u64;
            }
        }

        // Large range correction (not needed for 64-bit hashes in practice)
        raw as u64
    }

    /// Merge another HLL into this one (union).
    pub fn merge(&mut self, other: &HyperLogLog) {
        for (i, &r) in other.registers.iter().enumerate() {
            if r > self.registers[i] {
                self.registers[i] = r;
            }
        }
    }

    /// Memory usage in bytes.
    pub fn size_bytes(&self) -> usize {
        self.registers.len()
    }

    /// Relative standard error.
    pub fn standard_error() -> f64 {
        1.04 / (M as f64).sqrt() // ~0.0081 for p=14
    }
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_cardinality() {
        let mut hll = HyperLogLog::new();
        for i in 0..10000u64 {
            hll.add(&i);
        }
        let est = hll.estimate();
        // Should be within ~5% for 10k items
        assert!(
            est > 9000 && est < 11000,
            "Estimate {} not in range [9000, 11000]",
            est
        );
    }

    #[test]
    fn test_duplicates() {
        let mut hll = HyperLogLog::new();
        for _ in 0..1000 {
            hll.add(&42u64);
        }
        let est = hll.estimate();
        assert!(
            est <= 5,
            "Duplicate-only estimate should be ~1, got {}",
            est
        );
    }

    #[test]
    fn test_merge() {
        let mut a = HyperLogLog::new();
        let mut b = HyperLogLog::new();
        for i in 0..5000u64 {
            a.add(&i);
        }
        for i in 5000..10000u64 {
            b.add(&i);
        }
        a.merge(&b);
        let est = a.estimate();
        assert!(
            est > 9000 && est < 11000,
            "Merged estimate {} not in range",
            est
        );
    }

    #[test]
    fn test_empty() {
        let hll = HyperLogLog::new();
        assert_eq!(hll.estimate(), 0);
    }

    #[test]
    fn test_size() {
        let hll = HyperLogLog::new();
        assert_eq!(hll.size_bytes(), 16384);
    }
}
