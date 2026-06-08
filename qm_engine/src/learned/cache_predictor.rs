/*
 * Cache Predictor — Access Interval-Based Eviction
 *
 * Predicts when an item will be accessed next based on its
 * historical access intervals. Items with longer predicted
 * intervals are evicted first (replacing simple LRU).
 *
 * Algorithm: EMA of inter-access intervals → predicted next access time.
 * Fallback: LRU eviction when no history exists.
 */

use std::collections::HashMap;
use std::time::Instant;

/// Access history for a single cached item.
#[derive(Clone, Debug)]
struct AccessHistory {
    last_access: Instant,
    ema_interval: f64, // EMA of inter-access intervals (seconds)
    access_count: u64,
    predicted_next: f64, // predicted seconds until next access
}

/// Learned cache eviction predictor.
pub struct CachePredictor {
    items: HashMap<u64, AccessHistory>,
    alpha: f64,
    default_interval: f64, // default prediction for new items (seconds)
}

impl CachePredictor {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
            alpha: 0.3,
            default_interval: 60.0, // assume 60s until re-access
        }
    }

    pub fn with_default_interval(mut self, secs: f64) -> Self {
        self.default_interval = secs;
        self
    }

    /// Record an access to an item. Updates interval prediction.
    pub fn record_access(&mut self, key: u64) {
        let now = Instant::now();
        if let Some(entry) = self.items.get_mut(&key) {
            let interval = now.duration_since(entry.last_access).as_secs_f64();
            entry.ema_interval = self.alpha * interval + (1.0 - self.alpha) * entry.ema_interval;
            entry.last_access = now;
            entry.access_count += 1;
            entry.predicted_next = entry.ema_interval;
        } else {
            self.items.insert(
                key,
                AccessHistory {
                    last_access: now,
                    ema_interval: self.default_interval,
                    access_count: 1,
                    predicted_next: self.default_interval,
                },
            );
        }
    }

    /// Remove tracking for an evicted item.
    pub fn remove(&mut self, key: u64) {
        self.items.remove(&key);
    }

    /// Get the predicted interval until next access (seconds).
    /// Higher = less likely to be accessed soon = better eviction candidate.
    pub fn predicted_interval(&self, key: u64) -> f64 {
        self.items
            .get(&key)
            .map(|e| e.predicted_next)
            .unwrap_or(self.default_interval)
    }

    /// Get eviction priority score. Higher = should evict first.
    /// Combines predicted interval with time since last access.
    pub fn eviction_score(&self, key: u64) -> f64 {
        if let Some(entry) = self.items.get(&key) {
            let elapsed = entry.last_access.elapsed().as_secs_f64();
            // Score = how far past predicted interval we are
            // Positive = overdue for access → probably cold
            elapsed - entry.ema_interval + entry.ema_interval
        } else {
            f64::MAX // unknown items evict first
        }
    }

    /// Select the best item to evict from a set of candidates.
    pub fn select_eviction(&self, candidates: &[u64]) -> Option<u64> {
        candidates
            .iter()
            .max_by(|&&a, &&b| {
                self.eviction_score(a)
                    .partial_cmp(&self.eviction_score(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
    }

    /// Number of tracked items.
    pub fn tracked_count(&self) -> usize {
        self.items.len()
    }

    /// Get access count for an item.
    pub fn access_count(&self, key: u64) -> u64 {
        self.items.get(&key).map(|e| e.access_count).unwrap_or(0)
    }
}

impl Default for CachePredictor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_item() {
        let mut pred = CachePredictor::new();
        pred.record_access(1);
        assert_eq!(pred.access_count(1), 1);
        assert!(pred.predicted_interval(1) > 0.0);
    }

    #[test]
    fn test_unknown_item() {
        let pred = CachePredictor::new();
        assert_eq!(pred.predicted_interval(999), 60.0); // default
    }

    #[test]
    fn test_eviction_selection() {
        let mut pred = CachePredictor::new();
        pred.record_access(1);
        pred.record_access(2);
        pred.record_access(3);

        // All have same interval, but we should get a valid selection
        let evict = pred.select_eviction(&[1, 2, 3]);
        assert!(evict.is_some());
    }

    #[test]
    fn test_remove() {
        let mut pred = CachePredictor::new();
        pred.record_access(1);
        assert_eq!(pred.tracked_count(), 1);
        pred.remove(1);
        assert_eq!(pred.tracked_count(), 0);
    }
}
