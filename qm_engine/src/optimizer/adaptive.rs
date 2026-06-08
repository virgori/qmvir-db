/*
 * Adaptive Optimizer — Runtime Re-optimization
 *
 * Monitors actual vs estimated cardinalities during execution.
 * When estimates are significantly off (cardinality fence breach),
 * triggers re-planning with corrected statistics.
 *
 * Features:
 *   • Cardinality fences: detect 2× over/under-estimation
 *   • Plan history: cache previously optimized plans
 *   • Feedback loop: update statistics with observed values
 *   • Adaptive join strategy: switch hash↔merge at runtime
 */

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Threshold for cardinality fence: re-optimize when actual/estimated ratio
/// exceeds this factor (or its inverse).
const FENCE_FACTOR: f64 = 2.0;

/// A record of a plan execution for learning.
#[derive(Clone, Debug)]
pub struct PlanRecord {
    pub query_hash: u64,
    pub estimated_rows: f64,
    pub actual_rows: u64,
    pub execution_time: Duration,
    pub plan_description: String,
    pub timestamp: Instant,
}

/// Correction factor for a specific table+column selectivity.
#[derive(Clone, Debug)]
struct SelectivityCorrection {
    column: String,
    correction_factor: f64, // actual/estimated ratio (EMA-smoothed)
    sample_count: u64,
}

/// Adaptive optimizer with runtime feedback.
pub struct AdaptiveOptimizer {
    /// Plan execution history keyed by query hash.
    plan_history: HashMap<u64, Vec<PlanRecord>>,
    /// Selectivity correction factors per table.
    corrections: HashMap<String, Vec<SelectivityCorrection>>,
    /// Maximum history entries per query.
    max_history: usize,
    /// EMA smoothing factor (0..1, higher = more recent weight).
    ema_alpha: f64,
}

impl AdaptiveOptimizer {
    pub fn new() -> Self {
        Self {
            plan_history: HashMap::new(),
            corrections: HashMap::new(),
            max_history: 20,
            ema_alpha: 0.3,
        }
    }

    /// Record an execution result for future optimization.
    pub fn record_execution(&mut self, record: PlanRecord) {
        let hash = record.query_hash;
        let history = self.plan_history.entry(hash).or_insert_with(Vec::new);
        history.push(record);
        if history.len() > self.max_history {
            history.remove(0);
        }
    }

    /// Check if a cardinality fence is breached.
    /// Returns true if actual/estimated ratio exceeds FENCE_FACTOR.
    pub fn fence_breached(estimated: f64, actual: u64) -> bool {
        if estimated <= 0.0 {
            return actual > 0;
        }
        let ratio = actual as f64 / estimated;
        ratio > FENCE_FACTOR || ratio < 1.0 / FENCE_FACTOR
    }

    /// Update selectivity correction for a table+column based on observed data.
    pub fn update_correction(&mut self, table: &str, column: &str, estimated: f64, actual: u64) {
        if estimated <= 0.0 {
            return;
        }
        let ratio = actual as f64 / estimated;

        let corrections = self
            .corrections
            .entry(table.to_string())
            .or_insert_with(Vec::new);

        if let Some(corr) = corrections.iter_mut().find(|c| c.column == column) {
            // EMA update
            corr.correction_factor =
                self.ema_alpha * ratio + (1.0 - self.ema_alpha) * corr.correction_factor;
            corr.sample_count += 1;
        } else {
            corrections.push(SelectivityCorrection {
                column: column.to_string(),
                correction_factor: ratio,
                sample_count: 1,
            });
        }
    }

    /// Get the corrected selectivity for a table+column.
    /// Returns the correction factor (multiply with base estimate).
    pub fn get_correction(&self, table: &str, column: &str) -> f64 {
        self.corrections
            .get(table)
            .and_then(|cs| cs.iter().find(|c| c.column == column))
            .map(|c| c.correction_factor)
            .unwrap_or(1.0) // no correction (1.0 = unchanged)
    }

    /// Apply correction to an estimated row count.
    pub fn correct_estimate(&self, table: &str, column: &str, estimate: f64) -> f64 {
        let factor = self.get_correction(table, column);
        (estimate * factor).max(1.0)
    }

    /// Recommend whether to re-optimize a query based on history.
    pub fn should_reoptimize(&self, query_hash: u64) -> bool {
        if let Some(history) = self.plan_history.get(&query_hash) {
            if history.len() < 2 {
                return false;
            }

            // Check if last execution had a significant fence breach
            if let Some(last) = history.last() {
                return Self::fence_breached(last.estimated_rows, last.actual_rows);
            }
        }
        false
    }

    /// Suggest join strategy based on observed cardinalities.
    pub fn suggest_join_strategy(
        &self,
        left_table: &str,
        right_table: &str,
        left_estimated: u64,
        right_estimated: u64,
    ) -> &'static str {
        let left_corrected = self.correct_estimate(left_table, "*", left_estimated as f64);
        let right_corrected = self.correct_estimate(right_table, "*", right_estimated as f64);

        let smaller = left_corrected.min(right_corrected);
        let larger = left_corrected.max(right_corrected);

        if smaller < 100.0 {
            "nested_loop" // small inner → NL is fine
        } else if larger / smaller > 100.0 {
            "hash_join" // skewed sizes → hash join
        } else {
            "hash_join" // default: hash join is generally safe
        }
    }

    /// Get execution statistics for a query.
    pub fn get_stats(&self, query_hash: u64) -> Option<ExecutionStats> {
        self.plan_history.get(&query_hash).map(|history| {
            let count = history.len();
            let avg_time = history
                .iter()
                .map(|r| r.execution_time.as_secs_f64())
                .sum::<f64>()
                / count as f64;
            let avg_rows = history.iter().map(|r| r.actual_rows as f64).sum::<f64>() / count as f64;
            let fence_breaches = history
                .iter()
                .filter(|r| Self::fence_breached(r.estimated_rows, r.actual_rows))
                .count();

            ExecutionStats {
                execution_count: count,
                avg_execution_time: Duration::from_secs_f64(avg_time),
                avg_actual_rows: avg_rows,
                fence_breach_count: fence_breaches,
            }
        })
    }

    /// Clear all history and corrections.
    pub fn reset(&mut self) {
        self.plan_history.clear();
        self.corrections.clear();
    }

    /// Number of tracked queries.
    pub fn tracked_queries(&self) -> usize {
        self.plan_history.len()
    }

    /// Number of table corrections.
    pub fn correction_count(&self) -> usize {
        self.corrections.values().map(|v| v.len()).sum()
    }
}

impl Default for AdaptiveOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of execution history for a query.
#[derive(Clone, Debug)]
pub struct ExecutionStats {
    pub execution_count: usize,
    pub avg_execution_time: Duration,
    pub avg_actual_rows: f64,
    pub fence_breach_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fence_breached() {
        assert!(!AdaptiveOptimizer::fence_breached(100.0, 150)); // 1.5× OK
        assert!(AdaptiveOptimizer::fence_breached(100.0, 250)); // 2.5× breach
        assert!(AdaptiveOptimizer::fence_breached(100.0, 30)); // 0.3× breach
        assert!(!AdaptiveOptimizer::fence_breached(100.0, 100)); // exact
    }

    #[test]
    fn test_correction_ema() {
        let mut opt = AdaptiveOptimizer::new();

        // First observation: actual was 2× estimated
        opt.update_correction("users", "status", 100.0, 200);
        let c = opt.get_correction("users", "status");
        assert!((c - 2.0).abs() < 0.01);

        // Second observation: actual was 1× estimated
        opt.update_correction("users", "status", 100.0, 100);
        let c = opt.get_correction("users", "status");
        // EMA: 0.3 × 1.0 + 0.7 × 2.0 = 1.7
        assert!((c - 1.7).abs() < 0.01);
    }

    #[test]
    fn test_correct_estimate() {
        let mut opt = AdaptiveOptimizer::new();
        opt.update_correction("orders", "status", 1000.0, 3000); // 3× under-estimated

        let corrected = opt.correct_estimate("orders", "status", 500.0);
        assert!((corrected - 1500.0).abs() < 1.0);
    }

    #[test]
    fn test_no_correction() {
        let opt = AdaptiveOptimizer::new();
        let c = opt.get_correction("unknown", "col");
        assert_eq!(c, 1.0);
    }

    #[test]
    fn test_record_and_reoptimize() {
        let mut opt = AdaptiveOptimizer::new();

        opt.record_execution(PlanRecord {
            query_hash: 42,
            estimated_rows: 100.0,
            actual_rows: 100,
            execution_time: Duration::from_millis(10),
            plan_description: "seq_scan".to_string(),
            timestamp: Instant::now(),
        });

        assert!(!opt.should_reoptimize(42)); // only 1 record

        opt.record_execution(PlanRecord {
            query_hash: 42,
            estimated_rows: 100.0,
            actual_rows: 500, // 5× breach
            execution_time: Duration::from_millis(50),
            plan_description: "seq_scan".to_string(),
            timestamp: Instant::now(),
        });

        assert!(opt.should_reoptimize(42)); // fence breached
    }

    #[test]
    fn test_join_strategy() {
        let opt = AdaptiveOptimizer::new();
        assert_eq!(
            opt.suggest_join_strategy("a", "b", 10, 100000),
            "nested_loop"
        );
        assert_eq!(
            opt.suggest_join_strategy("a", "b", 10000, 50000),
            "hash_join"
        );
    }

    #[test]
    fn test_execution_stats() {
        let mut opt = AdaptiveOptimizer::new();
        for i in 0..5 {
            opt.record_execution(PlanRecord {
                query_hash: 1,
                estimated_rows: 100.0,
                actual_rows: 100 + i * 50,
                execution_time: Duration::from_millis(10),
                plan_description: "test".to_string(),
                timestamp: Instant::now(),
            });
        }

        let stats = opt.get_stats(1).unwrap();
        assert_eq!(stats.execution_count, 5);
    }
}
