/*
 * Selectivity Model — Learned Cardinality Correction
 *
 * Uses Exponential Moving Average (EMA) to correct selectivity estimates
 * based on observed query results. Each table+column pair maintains
 * independent correction factors.
 *
 * Fallback: traditional 1/ndistinct estimation when no history exists.
 */

use std::collections::HashMap;

/// Per-column correction entry.
#[derive(Clone, Debug)]
struct CorrectionEntry {
    factor: f64,     // EMA-smoothed actual/estimated ratio
    confidence: f64, // 0..1, increases with samples
    samples: u64,
}

/// Learned selectivity estimator with EMA correction.
pub struct SelectivityModel {
    /// (table, column) → correction entry
    corrections: HashMap<(String, String), CorrectionEntry>,
    /// EMA smoothing factor
    alpha: f64,
    /// Samples needed for full confidence
    confidence_threshold: u64,
}

impl SelectivityModel {
    pub fn new() -> Self {
        Self {
            corrections: HashMap::new(),
            alpha: 0.3,
            confidence_threshold: 10,
        }
    }

    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.max(0.01).min(0.99);
        self
    }

    /// Record an observation: estimated vs actual selectivity.
    pub fn observe(&mut self, table: &str, column: &str, estimated: f64, actual: f64) {
        if estimated <= 0.0 {
            return;
        }
        let ratio = actual / estimated;

        let key = (table.to_string(), column.to_string());
        let entry = self.corrections.entry(key).or_insert(CorrectionEntry {
            factor: 1.0,
            confidence: 0.0,
            samples: 0,
        });

        entry.factor = self.alpha * ratio + (1.0 - self.alpha) * entry.factor;
        entry.samples += 1;
        entry.confidence = (entry.samples as f64 / self.confidence_threshold as f64).min(1.0);
    }

    /// Get corrected selectivity estimate.
    /// Blends traditional estimate with learned correction based on confidence.
    pub fn correct(&self, table: &str, column: &str, base_estimate: f64) -> f64 {
        let key = (table.to_string(), column.to_string());
        if let Some(entry) = self.corrections.get(&key) {
            let corrected = base_estimate * entry.factor;
            // Blend: confidence × corrected + (1 - confidence) × base
            let blended = entry.confidence * corrected + (1.0 - entry.confidence) * base_estimate;
            blended.max(0.0).min(1.0)
        } else {
            base_estimate
        }
    }

    /// Get raw correction factor.
    pub fn get_factor(&self, table: &str, column: &str) -> Option<f64> {
        let key = (table.to_string(), column.to_string());
        self.corrections.get(&key).map(|e| e.factor)
    }

    /// Get confidence level (0..1).
    pub fn confidence(&self, table: &str, column: &str) -> f64 {
        let key = (table.to_string(), column.to_string());
        self.corrections
            .get(&key)
            .map(|e| e.confidence)
            .unwrap_or(0.0)
    }

    /// Number of tracked column pairs.
    pub fn tracked_count(&self) -> usize {
        self.corrections.len()
    }

    /// Reset all learned corrections.
    pub fn reset(&mut self) {
        self.corrections.clear();
    }
}

impl Default for SelectivityModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_correction() {
        let model = SelectivityModel::new();
        assert_eq!(model.correct("t", "c", 0.5), 0.5);
    }

    #[test]
    fn test_learn_correction() {
        let mut model = SelectivityModel::new();

        // Observed: actual is 2× base estimate consistently
        for _ in 0..20 {
            model.observe("users", "status", 0.1, 0.2);
        }

        let corrected = model.correct("users", "status", 0.1);
        // Should be close to 0.2 with high confidence
        assert!((corrected - 0.2).abs() < 0.05, "corrected = {}", corrected);
    }

    #[test]
    fn test_confidence_ramp() {
        let mut model = SelectivityModel::new();

        // Low confidence: 1 sample
        model.observe("t", "c", 0.1, 0.5);
        assert!(model.confidence("t", "c") < 0.5);

        // High confidence: many samples
        for _ in 0..20 {
            model.observe("t", "c", 0.1, 0.5);
        }
        assert!((model.confidence("t", "c") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_reset() {
        let mut model = SelectivityModel::new();
        model.observe("t", "c", 0.1, 0.2);
        assert_eq!(model.tracked_count(), 1);
        model.reset();
        assert_eq!(model.tracked_count(), 0);
    }
}
