/*
 * Learned Components — Adaptive ML Assistants
 *
 * Lightweight learned models that improve with runtime feedback:
 *   • SelectivityModel   — EMA correction factors for cardinality estimation
 *   • CachePredictor     — access interval prediction for cache eviction
 *   • FusionWeightTuner  — per-intent alpha tuning for hybrid search
 *   • IntentClassifier    — query intent classification (lookup/search/analytics/hybrid)
 *
 * All components use feature-based heuristics with fallback defaults.
 * No external ML framework dependencies.
 */

pub mod cache_predictor;
pub mod fusion_weights;
pub mod intent;
pub mod selectivity;

pub use cache_predictor::CachePredictor;
pub use fusion_weights::FusionWeightTuner;
pub use intent::{IntentClassifier, QueryIntent};
pub use selectivity::SelectivityModel;
