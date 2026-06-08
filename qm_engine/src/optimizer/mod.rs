/*
 * Optimizer Module — Adaptive Query Execution
 *
 * Implements:
 *   • Rule-based query rewriting (predicate pushdown, projection pushdown,
 *     constant folding, join reordering)
 *   • Cardinality fences: detect plan regression at runtime
 *   • Plan history: track past plan costs for re-optimization
 *   • Adaptive re-planning: switch strategies mid-execution when
 *     cardinality estimates are off
 */

pub mod adaptive;
pub mod rules;

pub use adaptive::AdaptiveOptimizer;
pub use rules::{apply_rules, LogicalPlan, RewriteRule};
