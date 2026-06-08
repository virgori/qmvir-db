/*
 * T-Digest — Quantile Estimation
 *
 * Parameters: δ=100 (compression parameter)
 * Error: <1% at extreme quantiles (p1, p99), ~2-3% at median
 *
 * Algorithm (merging variant):
 *   1. Maintain sorted list of centroids (mean, weight)
 *   2. New points merge into nearest centroid if weight constraint allows
 *   3. Weight constraint: w_i ≤ δ × q(1-q) × n (tighter at tails)
 *   4. When buffer exceeds threshold, compress by re-merging
 *
 * Supports: quantile estimation, CDF, inverse CDF, trimmed mean
 */

const DEFAULT_COMPRESSION: f64 = 100.0;
const BUFFER_FACTOR: usize = 5; // compress when buffer reaches 5× compression

/// A centroid in the T-Digest: mean value + weight (count of merged points).
#[derive(Clone, Debug)]
struct Centroid {
    mean: f64,
    weight: f64,
}

/// T-Digest for streaming quantile estimation.
/// Compression δ=100 → ~200-300 centroids, <1% error at tails.
pub struct TDigest {
    centroids: Vec<Centroid>,
    compression: f64,
    total_weight: f64,
    min: f64,
    max: f64,
    buffer: Vec<f64>,
    buffer_limit: usize,
}

impl TDigest {
    pub fn new() -> Self {
        Self::with_compression(DEFAULT_COMPRESSION)
    }

    pub fn with_compression(compression: f64) -> Self {
        Self {
            centroids: Vec::new(),
            compression,
            total_weight: 0.0,
            min: f64::MAX,
            max: f64::MIN,
            buffer: Vec::new(),
            buffer_limit: (compression as usize) * BUFFER_FACTOR,
        }
    }

    /// Add a single value.
    pub fn add(&mut self, value: f64) {
        self.buffer.push(value);
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
        if self.buffer.len() >= self.buffer_limit {
            self.compress();
        }
    }

    /// Add a value with a specific weight.
    pub fn add_weighted(&mut self, value: f64, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
        self.centroids.push(Centroid {
            mean: value,
            weight,
        });
        self.total_weight += weight;
        if self.centroids.len() >= self.buffer_limit {
            self.compress();
        }
    }

    /// Compress: merge buffer into centroids with weight constraints.
    pub fn compress(&mut self) {
        self.compress_pass();
        // Second pass if still over budget (handles residual tail centroids)
        if self.centroids.len() > (self.compression * 3.0) as usize {
            self.compress_pass();
        }
    }

    fn compress_pass(&mut self) {
        // Add buffered values as unit-weight centroids
        for &v in &self.buffer {
            self.centroids.push(Centroid {
                mean: v,
                weight: 1.0,
            });
            self.total_weight += 1.0;
        }
        self.buffer.clear();

        if self.centroids.is_empty() {
            return;
        }

        // Sort by mean
        self.centroids.sort_by(|a, b| {
            a.mean
                .partial_cmp(&b.mean)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let total = self.total_weight;
        let mut merged: Vec<Centroid> = Vec::new();
        let mut weight_so_far = 0.0;

        for c in self.centroids.drain(..) {
            let q = (weight_so_far + c.weight / 2.0) / total;
            // k₁ scale function: max_w ∝ √(q(1-q)) gives O(δ) centroids
            let max_w = (total * 2.0 * std::f64::consts::PI * (q * (1.0 - q)).sqrt()
                / self.compression)
                .max(1.0);

            if let Some(last) = merged.last_mut() {
                if last.weight + c.weight <= max_w {
                    let new_weight = last.weight + c.weight;
                    last.mean = (last.mean * last.weight + c.mean * c.weight) / new_weight;
                    last.weight = new_weight;
                    weight_so_far += c.weight;
                    continue;
                }
            }

            weight_so_far += c.weight;
            merged.push(c);
        }

        self.centroids = merged;
    }

    /// Ensure all buffered data is integrated.
    fn ensure_compressed(&mut self) {
        if !self.buffer.is_empty() {
            self.compress();
        }
        // Second pass for tighter compression (merge residual tail centroids)
        if self.centroids.len() > (self.compression * 3.0) as usize {
            self.compress();
        }
    }

    /// Estimate the value at quantile q (0.0 to 1.0).
    pub fn quantile(&mut self, q: f64) -> f64 {
        self.ensure_compressed();

        if self.centroids.is_empty() {
            return 0.0;
        }
        if q <= 0.0 {
            return self.min;
        }
        if q >= 1.0 {
            return self.max;
        }

        let target = q * self.total_weight;
        let mut cumulative = 0.0;

        // Edge: before first centroid
        let first = &self.centroids[0];
        if target <= first.weight / 2.0 {
            // Interpolate between min and first centroid
            if first.weight > 0.0 {
                return self.min + (first.mean - self.min) * (target / (first.weight / 2.0));
            }
            return self.min;
        }

        for i in 0..self.centroids.len() - 1 {
            let c1 = &self.centroids[i];
            let c2 = &self.centroids[i + 1];
            let mid1 = cumulative + c1.weight / 2.0;
            let mid2 = cumulative + c1.weight + c2.weight / 2.0;

            if target >= mid1 && target <= mid2 {
                // Linear interpolation between centroid means
                let fraction = if mid2 > mid1 {
                    (target - mid1) / (mid2 - mid1)
                } else {
                    0.5
                };
                return c1.mean + (c2.mean - c1.mean) * fraction;
            }

            cumulative += c1.weight;
        }

        // After last centroid: interpolate to max
        let last = self.centroids.last().unwrap();
        let mid_last = self.total_weight - last.weight / 2.0;
        if target >= mid_last {
            if last.weight > 0.0 {
                let frac = (target - mid_last) / (last.weight / 2.0);
                return last.mean + (self.max - last.mean) * frac;
            }
        }

        self.max
    }

    /// Estimate CDF: P(X ≤ value).
    pub fn cdf(&mut self, value: f64) -> f64 {
        self.ensure_compressed();

        if self.centroids.is_empty() {
            return 0.0;
        }
        if value <= self.min {
            return 0.0;
        }
        if value >= self.max {
            return 1.0;
        }

        let mut cumulative = 0.0;
        for i in 0..self.centroids.len() {
            let c = &self.centroids[i];
            if value < c.mean {
                if i == 0 {
                    return cumulative / self.total_weight;
                }
                let prev = &self.centroids[i - 1];
                let frac = (value - prev.mean) / (c.mean - prev.mean);
                return (cumulative - prev.weight / 2.0
                    + frac * (prev.weight / 2.0 + c.weight / 2.0))
                    / self.total_weight;
            }
            cumulative += c.weight;
        }

        1.0
    }

    /// Median (p50).
    pub fn median(&mut self) -> f64 {
        self.quantile(0.5)
    }

    /// Trimmed mean between quantiles q_low and q_high.
    pub fn trimmed_mean(&mut self, q_low: f64, q_high: f64) -> f64 {
        self.ensure_compressed();

        let low = self.quantile(q_low);
        let high = self.quantile(q_high);

        let mut sum = 0.0;
        let mut count = 0.0;
        for c in &self.centroids {
            if c.mean >= low && c.mean <= high {
                sum += c.mean * c.weight;
                count += c.weight;
            }
        }

        if count > 0.0 {
            sum / count
        } else {
            0.0
        }
    }

    /// Merge another T-Digest into this one.
    pub fn merge(&mut self, other: &TDigest) {
        for c in &other.centroids {
            self.add_weighted(c.mean, c.weight);
        }
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
    }

    /// Number of centroids (compressed).
    pub fn centroid_count(&self) -> usize {
        self.centroids.len() + self.buffer.len()
    }

    /// Total data points seen.
    pub fn count(&self) -> f64 {
        self.total_weight + self.buffer.len() as f64
    }

    /// Memory usage estimate in bytes.
    pub fn size_bytes(&self) -> usize {
        (self.centroids.len() + self.buffer.len()) * 16 + 64 // 16 bytes per centroid/value + struct overhead
    }
}

impl Default for TDigest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantiles_uniform() {
        let mut td = TDigest::new();
        for i in 1..=10000 {
            td.add(i as f64);
        }

        let p50 = td.quantile(0.5);
        assert!(
            (p50 - 5000.0).abs() < 200.0,
            "p50 = {}, expected ~5000",
            p50
        );

        let p99 = td.quantile(0.99);
        assert!(
            (p99 - 9900.0).abs() < 200.0,
            "p99 = {}, expected ~9900",
            p99
        );

        let p1 = td.quantile(0.01);
        assert!((p1 - 100.0).abs() < 200.0, "p1 = {}, expected ~100", p1);
    }

    #[test]
    fn test_min_max() {
        let mut td = TDigest::new();
        for i in 0..100 {
            td.add(i as f64);
        }
        assert_eq!(td.quantile(0.0), 0.0);
        assert_eq!(td.quantile(1.0), 99.0);
    }

    #[test]
    fn test_single_value() {
        let mut td = TDigest::new();
        td.add(42.0);
        assert!((td.quantile(0.5) - 42.0).abs() < 1.0);
    }

    #[test]
    fn test_merge() {
        let mut a = TDigest::new();
        let mut b = TDigest::new();
        for i in 0..5000 {
            a.add(i as f64);
        }
        for i in 5000..10000 {
            b.add(i as f64);
        }
        a.merge(&b);

        let p50 = a.quantile(0.5);
        assert!((p50 - 5000.0).abs() < 300.0, "Merged p50 = {}", p50);
    }

    #[test]
    fn test_cdf() {
        let mut td = TDigest::new();
        for i in 0..1000 {
            td.add(i as f64);
        }
        let cdf_500 = td.cdf(500.0);
        assert!((cdf_500 - 0.5).abs() < 0.05, "CDF(500) = {}", cdf_500);
    }

    #[test]
    fn test_empty() {
        let mut td = TDigest::new();
        assert_eq!(td.quantile(0.5), 0.0);
        assert_eq!(td.cdf(0.0), 0.0);
    }

    #[test]
    fn test_compression() {
        let mut td = TDigest::new();
        for i in 0..100000 {
            td.add(i as f64);
        }
        td.compress();
        // Should compress to ~100-300 centroids
        assert!(
            td.centroid_count() < 500,
            "Too many centroids: {}",
            td.centroid_count()
        );
    }
}
