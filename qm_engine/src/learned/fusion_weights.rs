/*
 * Fusion Weight Tuner — Per-Intent Hybrid Search Alpha
 *
 * Tunes the fusion weight α between lexical and vector scores
 * for hybrid search: score = α × lexical + (1-α) × vector
 *
 * Learns optimal α per query intent category based on user feedback
 * (click-through, relevance signals).
 *
 * Fallback: α = 0.5 (equal weight) when no feedback exists.
 */

use std::collections::HashMap;

/// Fusion weight for a specific intent category.
#[derive(Clone, Debug)]
struct IntentWeight {
    alpha: f64, // lexical weight (0..1)
    feedback_count: u64,
    total_reward: f64, // accumulated relevance reward
}

/// Learned fusion weight tuner for hybrid search.
pub struct FusionWeightTuner {
    /// Intent category → fusion weight
    weights: HashMap<String, IntentWeight>,
    /// Learning rate for gradient updates
    learning_rate: f64,
    /// Default alpha when no data exists
    default_alpha: f64,
}

impl FusionWeightTuner {
    pub fn new() -> Self {
        Self {
            weights: HashMap::new(),
            learning_rate: 0.05,
            default_alpha: 0.5,
        }
    }

    pub fn with_default_alpha(mut self, alpha: f64) -> Self {
        self.default_alpha = alpha.max(0.0).min(1.0);
        self
    }

    /// Get the fusion alpha for a given intent.
    /// Returns (lexical_weight, vector_weight) where sum = 1.0.
    pub fn get_weights(&self, intent: &str) -> (f64, f64) {
        let alpha = self
            .weights
            .get(intent)
            .map(|w| w.alpha)
            .unwrap_or(self.default_alpha);
        (alpha, 1.0 - alpha)
    }

    /// Compute fused score: α × lexical + (1-α) × vector.
    pub fn fuse(&self, intent: &str, lexical_score: f64, vector_score: f64) -> f64 {
        let (alpha, beta) = self.get_weights(intent);
        alpha * lexical_score + beta * vector_score
    }

    /// Record feedback: how relevant were the results for this intent+alpha?
    /// `reward`: 0.0 (irrelevant) to 1.0 (perfect).
    /// `used_lexical_more`: true if the clicked result ranked higher in lexical.
    pub fn record_feedback(&mut self, intent: &str, reward: f64, used_lexical_more: bool) {
        let entry = self
            .weights
            .entry(intent.to_string())
            .or_insert(IntentWeight {
                alpha: self.default_alpha,
                feedback_count: 0,
                total_reward: 0.0,
            });

        entry.feedback_count += 1;
        entry.total_reward += reward;

        // Simple gradient: nudge alpha toward whichever modality was more useful
        let direction = if used_lexical_more { 1.0 } else { -1.0 };
        let gradient = direction * reward * self.learning_rate;
        entry.alpha = (entry.alpha + gradient).max(0.05).min(0.95);
    }

    /// Batch update from multiple feedback signals.
    pub fn batch_feedback(&mut self, feedbacks: &[(String, f64, bool)]) {
        for (intent, reward, lexical_more) in feedbacks {
            self.record_feedback(intent, *reward, *lexical_more);
        }
    }

    /// Get the current alpha value for an intent.
    pub fn alpha(&self, intent: &str) -> f64 {
        self.weights
            .get(intent)
            .map(|w| w.alpha)
            .unwrap_or(self.default_alpha)
    }

    /// Get feedback count for an intent.
    pub fn feedback_count(&self, intent: &str) -> u64 {
        self.weights
            .get(intent)
            .map(|w| w.feedback_count)
            .unwrap_or(0)
    }

    /// List all intent weights.
    pub fn all_weights(&self) -> Vec<(String, f64, u64)> {
        self.weights
            .iter()
            .map(|(k, v)| (k.clone(), v.alpha, v.feedback_count))
            .collect()
    }

    /// Reset all learned weights.
    pub fn reset(&mut self) {
        self.weights.clear();
    }
}

impl Default for FusionWeightTuner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_weights() {
        let tuner = FusionWeightTuner::new();
        let (a, b) = tuner.get_weights("unknown");
        assert_eq!(a, 0.5);
        assert_eq!(b, 0.5);
    }

    #[test]
    fn test_fusion() {
        let tuner = FusionWeightTuner::new();
        let score = tuner.fuse("keyword", 0.8, 0.6);
        assert!((score - 0.7).abs() < 0.01); // 0.5*0.8 + 0.5*0.6 = 0.7
    }

    #[test]
    fn test_learn_toward_lexical() {
        let mut tuner = FusionWeightTuner::new();
        // Feedback: lexical was consistently better
        for _ in 0..20 {
            tuner.record_feedback("keyword_search", 0.9, true);
        }
        let alpha = tuner.alpha("keyword_search");
        assert!(alpha > 0.5, "Alpha should increase for lexical: {}", alpha);
    }

    #[test]
    fn test_learn_toward_vector() {
        let mut tuner = FusionWeightTuner::new();
        // Feedback: vector was consistently better
        for _ in 0..20 {
            tuner.record_feedback("semantic_search", 0.9, false);
        }
        let alpha = tuner.alpha("semantic_search");
        assert!(alpha < 0.5, "Alpha should decrease for vector: {}", alpha);
    }

    #[test]
    fn test_alpha_bounds() {
        let mut tuner = FusionWeightTuner::new();
        for _ in 0..500 {
            tuner.record_feedback("extreme", 1.0, true);
        }
        let alpha = tuner.alpha("extreme");
        assert!(
            alpha <= 0.95 && alpha >= 0.05,
            "Alpha out of bounds: {}",
            alpha
        );
    }

    #[test]
    fn test_reset() {
        let mut tuner = FusionWeightTuner::new();
        tuner.record_feedback("test", 0.5, true);
        assert_eq!(tuner.all_weights().len(), 1);
        tuner.reset();
        assert_eq!(tuner.all_weights().len(), 0);
    }
}
