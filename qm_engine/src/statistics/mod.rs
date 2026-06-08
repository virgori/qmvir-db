/*
 * Statistics Module — Probabilistic Sketches & Cost Model
 *
 * Implements the 4 core sketches from ARCHITECTURE_CORE.md:
 *   • HyperLogLog (p=14)     — cardinality estimation, ~0.81% error
 *   • Count-Min Sketch        — frequency estimation, ε ≈ 0.0013
 *   • T-Digest (δ=100)        — quantile estimation, <1% error at tails
 *   • Bloom Filter             — set membership, configurable FP rate
 *
 * Plus the cost model for the query planner:
 *   • I/O cost (page reads/writes)
 *   • CPU cost (comparisons, hash computations)
 *   • Selectivity estimation
 */

pub mod bloom;
pub mod cost_model;
pub mod count_min;
pub mod hll;
pub mod tdigest;

pub use bloom::BloomFilter;
pub use cost_model::CostModel;
pub use count_min::CountMinSketch;
pub use hll::HyperLogLog;
pub use tdigest::TDigest;
