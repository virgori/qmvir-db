/*
 * Hybrid Search — Phase 14 (BM25 + Vector + Cross-Encoder Reranking)
 *
 * Combines keyword-based text search (BM25) with vector similarity
 * search (HNSW), then applies a cross-encoder reranker for maximum
 * relevance in RAG pipelines.
 *
 * Pipeline:
 *   1. BM25 → Top-K₁ text matches (sparse retrieval)
 *   2. HNSW → Top-K₂ vector matches (dense retrieval)
 *   3. Merge & deduplicate (reciprocal rank fusion)
 *   4. Cross-encoder rerank → Top-K final results
 */

use ahash::AHashMap;
use std::cmp::Ordering;
use std::time::Instant;

// ── Score fusion strategies ─────────────────────────────────────────

/// Method for combining BM25 and vector scores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FusionMethod {
    /// Reciprocal Rank Fusion: score = Σ 1/(k + rank_i)
    ReciprocalRank { k: f64 },
    /// Weighted linear: score = α * norm_bm25 + (1-α) * norm_vector
    WeightedLinear { alpha: f64 },
    /// Distribution-Based Score Fusion (DBSF): normalize to [0,1] using z-score
    DistributionBased,
}

impl Default for FusionMethod {
    fn default() -> Self {
        FusionMethod::ReciprocalRank { k: 60.0 }
    }
}

/// A scored document from one retrieval source.
#[derive(Clone, Debug)]
pub struct ScoredDoc {
    pub id: i64,
    pub score: f64,
}

/// A fully-scored result with provenance information.
#[derive(Clone, Debug)]
pub struct HybridResult {
    pub id: i64,
    /// Final fused/reranked score.
    pub score: f64,
    /// BM25 score (if present).
    pub bm25_score: Option<f64>,
    /// Vector distance (if present).
    pub vector_distance: Option<f64>,
    /// Cross-encoder relevance score (if reranked).
    pub rerank_score: Option<f64>,
}

#[derive(Clone, Debug)]
struct TopHybridResult {
    result: HybridResult,
}

impl PartialEq for TopHybridResult {
    fn eq(&self, other: &Self) -> bool {
        self.result.id == other.result.id && self.result.score == other.result.score
    }
}

impl Eq for TopHybridResult {}

impl PartialOrd for TopHybridResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TopHybridResult {
    fn cmp(&self, other: &Self) -> Ordering {
        self.result
            .score
            .partial_cmp(&other.result.score)
            .unwrap_or(Ordering::Equal)
            .reverse()
            .then_with(|| self.result.id.cmp(&other.result.id))
    }
}

fn hybrid_result_cmp(a: &HybridResult, b: &HybridResult) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.id.cmp(&b.id))
}

fn select_top_hybrid_results(mut results: Vec<HybridResult>, top_k: usize) -> Vec<HybridResult> {
    use std::collections::BinaryHeap;

    if top_k == 0 {
        return Vec::new();
    }
    if results.len() <= top_k {
        results.sort_by(hybrid_result_cmp);
        return results;
    }

    let mut heap: BinaryHeap<TopHybridResult> = BinaryHeap::with_capacity(top_k + 1);
    for result in results {
        let candidate = TopHybridResult { result };
        if heap.len() < top_k {
            heap.push(candidate);
        } else if let Some(worst) = heap.peek() {
            if hybrid_result_cmp(&candidate.result, &worst.result) == Ordering::Less {
                heap.pop();
                heap.push(candidate);
            }
        }
    }

    let mut selected: Vec<HybridResult> = heap.into_iter().map(|entry| entry.result).collect();
    selected.sort_by(hybrid_result_cmp);
    selected
}

// ── Reciprocal Rank Fusion ──────────────────────────────────────────

/// Reciprocal Rank Fusion (RRF) — rank-based score combination.
///
/// For each document d:  score(d) = Σ_source 1/(k + rank(d, source))
///
/// Advantages:
/// - No need to normalize scores across different systems
/// - Robust to outlier scores
/// - Works well when combining >2 retrieval sources
pub fn reciprocal_rank_fusion(
    sources: &[Vec<ScoredDoc>],
    k: f64,
    top_k: usize,
) -> Vec<HybridResult> {
    let mut scores: AHashMap<i64, f64> = AHashMap::new();

    for source in sources {
        for (rank, doc) in source.iter().enumerate() {
            if !doc.score.is_finite() {
                continue;
            }
            *scores.entry(doc.id).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        }
    }

    let results: Vec<HybridResult> = scores
        .into_iter()
        .map(|(id, score)| HybridResult {
            id,
            score,
            bm25_score: None,
            vector_distance: None,
            rerank_score: None,
        })
        .collect();

    select_top_hybrid_results(results, top_k)
}

// ── Weighted Linear Fusion ──────────────────────────────────────────

/// Weighted linear combination with min-max normalization.
///
/// score(d) = α × norm(bm25_score) + (1-α) × norm(1 - vector_distance)
pub fn weighted_linear_fusion(
    bm25_results: &[ScoredDoc],
    vector_results: &[ScoredDoc],
    alpha: f64,
    top_k: usize,
) -> Vec<HybridResult> {
    // Normalize BM25 scores to [0, 1]
    let bm25_norm = normalize_scores(bm25_results);
    // Normalize vector distances: convert to similarity (1 - dist) then normalize
    let vec_as_sim: Vec<ScoredDoc> = vector_results
        .iter()
        .filter(|d| d.score.is_finite())
        .map(|d| ScoredDoc {
            id: d.id,
            score: 1.0 - d.score.min(1.0), // clamp distance ≤ 1 for cosine
        })
        .collect();
    let vec_norm = normalize_scores(&vec_as_sim);

    // Merge
    let mut combined: AHashMap<i64, (f64, f64)> = AHashMap::new(); // (bm25, vec)
    for (id, score) in &bm25_norm {
        combined.entry(*id).or_insert((0.0, 0.0)).0 = *score;
    }
    for (id, score) in &vec_norm {
        combined.entry(*id).or_insert((0.0, 0.0)).1 = *score;
    }

    let results: Vec<HybridResult> = combined
        .into_iter()
        .map(|(id, (bm25, vec))| {
            let fused = alpha * bm25 + (1.0 - alpha) * vec;
            HybridResult {
                id,
                score: fused,
                bm25_score: if bm25 > 0.0 { Some(bm25) } else { None },
                vector_distance: if vec > 0.0 { Some(1.0 - vec) } else { None },
                rerank_score: None,
            }
        })
        .collect();

    select_top_hybrid_results(results, top_k)
}

/// Distribution-Based Score Fusion (DBSF) — z-score normalization.
pub fn distribution_based_fusion(
    bm25_results: &[ScoredDoc],
    vector_results: &[ScoredDoc],
    top_k: usize,
) -> Vec<HybridResult> {
    let bm25_z = z_score_normalize(bm25_results);
    let vec_z = z_score_normalize(
        &vector_results
            .iter()
            .filter(|d| d.score.is_finite())
            .map(|d| ScoredDoc {
                id: d.id,
                score: -d.score,
            }) // negate distance → similarity
            .collect::<Vec<_>>(),
    );

    let mut combined: AHashMap<i64, (f64, f64)> = AHashMap::new();
    for (id, z) in &bm25_z {
        combined.entry(*id).or_insert((0.0, 0.0)).0 = *z;
    }
    for (id, z) in &vec_z {
        combined.entry(*id).or_insert((0.0, 0.0)).1 = *z;
    }

    let results: Vec<HybridResult> = combined
        .into_iter()
        .map(|(id, (bm25_z, vec_z))| HybridResult {
            id,
            score: bm25_z + vec_z,
            bm25_score: Some(bm25_z),
            vector_distance: Some(-vec_z),
            rerank_score: None,
        })
        .collect();

    select_top_hybrid_results(results, top_k)
}

// ── Reranker interface ──────────────────────────────────────────────

/// Cross-encoder reranker trait. Implementations may call an external model.
pub trait Reranker: Send + Sync {
    /// Score each (query, document_id) pair.
    /// Returns scores in the same order as `doc_ids`.
    fn rerank(&self, query: &str, doc_ids: &[i64]) -> Vec<f64>;
}

/// Simple dot-product reranker using precomputed query embedding.
/// This is a fast placeholder; production would use a cross-encoder model.
pub struct DotProductReranker {
    /// doc_id → embedding
    pub embeddings: AHashMap<i64, Vec<f64>>,
}

impl DotProductReranker {
    pub fn new(embeddings: AHashMap<i64, Vec<f64>>) -> Self {
        Self { embeddings }
    }
}

impl Reranker for DotProductReranker {
    fn rerank(&self, _query: &str, doc_ids: &[i64]) -> Vec<f64> {
        // Simple scoring: average of embedding values (placeholder)
        doc_ids
            .iter()
            .map(|&id| {
                self.embeddings
                    .get(&id)
                    .map(|emb| emb.iter().sum::<f64>() / emb.len() as f64)
                    .unwrap_or(0.0)
            })
            .collect()
    }
}

/// Apply reranking to hybrid results.
pub fn apply_reranker(
    results: &mut [HybridResult],
    reranker: &dyn Reranker,
    query: &str,
    _top_k: usize,
) {
    let doc_ids: Vec<i64> = results.iter().map(|r| r.id).collect();
    let rerank_scores = reranker.rerank(query, &doc_ids);

    for (r, &score) in results.iter_mut().zip(rerank_scores.iter()) {
        r.rerank_score = Some(score);
        r.score = score; // Reranker score overrides fusion score
    }

    results.sort_by(hybrid_result_cmp);
}

// ── Full hybrid search pipeline ─────────────────────────────────────

/// Configuration for hybrid search.
#[derive(Clone)]
pub struct HybridSearchConfig {
    /// Number of results from BM25 stage.
    pub bm25_top_k: usize,
    /// Number of results from vector stage.
    pub vector_top_k: usize,
    /// Final number of results.
    pub final_top_k: usize,
    /// Score fusion method.
    pub fusion: FusionMethod,
    /// Whether to apply reranking.
    pub rerank: bool,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            bm25_top_k: 100,
            vector_top_k: 100,
            final_top_k: 10,
            fusion: FusionMethod::default(),
            rerank: false,
        }
    }
}

/// Execute hybrid search pipeline.
/// Takes pre-computed BM25 and vector results and returns fused+reranked results.
pub fn hybrid_search(
    bm25_results: &[ScoredDoc],
    vector_results: &[ScoredDoc],
    config: &HybridSearchConfig,
    reranker: Option<&dyn Reranker>,
    query: &str,
) -> (Vec<HybridResult>, HybridSearchStats) {
    let start = Instant::now();

    // Stage 1: Fusion
    let mut results = match config.fusion {
        FusionMethod::ReciprocalRank { k } => reciprocal_rank_fusion(
            &[bm25_results.to_vec(), vector_results.to_vec()],
            k,
            if config.rerank {
                config.bm25_top_k + config.vector_top_k
            } else {
                config.final_top_k
            },
        ),
        FusionMethod::WeightedLinear { alpha } => weighted_linear_fusion(
            bm25_results,
            vector_results,
            alpha,
            if config.rerank {
                config.bm25_top_k + config.vector_top_k
            } else {
                config.final_top_k
            },
        ),
        FusionMethod::DistributionBased => distribution_based_fusion(
            bm25_results,
            vector_results,
            if config.rerank {
                config.bm25_top_k + config.vector_top_k
            } else {
                config.final_top_k
            },
        ),
    };
    let fusion_elapsed = start.elapsed();

    // Stage 2: Reranking (optional)
    let rerank_elapsed = if config.rerank {
        if let Some(rr) = reranker {
            let rr_start = Instant::now();
            apply_reranker(&mut results, rr, query, config.final_top_k);
            rr_start.elapsed()
        } else {
            Duration::from_secs(0)
        }
    } else {
        Duration::from_secs(0)
    };

    results.truncate(config.final_top_k);

    let stats = HybridSearchStats {
        bm25_candidates: bm25_results.len(),
        vector_candidates: vector_results.len(),
        fused_candidates: results.len(),
        fusion_time_us: fusion_elapsed.as_micros() as u64,
        rerank_time_us: rerank_elapsed.as_micros() as u64,
        total_time_us: start.elapsed().as_micros() as u64,
    };

    (results, stats)
}

/// Statistics from a hybrid search execution.
#[derive(Clone, Debug)]
pub struct HybridSearchStats {
    pub bm25_candidates: usize,
    pub vector_candidates: usize,
    pub fused_candidates: usize,
    pub fusion_time_us: u64,
    pub rerank_time_us: u64,
    pub total_time_us: u64,
}

use std::time::Duration;

// ── Helper functions ────────────────────────────────────────────────

/// Min-max normalize scores to [0, 1].
fn normalize_scores(docs: &[ScoredDoc]) -> Vec<(i64, f64)> {
    let finite_docs: Vec<&ScoredDoc> = docs.iter().filter(|doc| doc.score.is_finite()).collect();
    if finite_docs.is_empty() {
        return Vec::new();
    }
    let min = finite_docs
        .iter()
        .map(|d| d.score)
        .fold(f64::MAX, f64::min);
    let max = finite_docs
        .iter()
        .map(|d| d.score)
        .fold(f64::MIN, f64::max);
    let range = max - min;

    finite_docs
        .into_iter()
        .map(|d| {
            let norm = if range > 1e-10 {
                (d.score - min) / range
            } else {
                1.0
            };
            (d.id, norm)
        })
        .collect()
}

/// Z-score normalize scores (mean=0, stddev=1).
fn z_score_normalize(docs: &[ScoredDoc]) -> Vec<(i64, f64)> {
    let finite_docs: Vec<&ScoredDoc> = docs.iter().filter(|doc| doc.score.is_finite()).collect();
    if finite_docs.is_empty() {
        return Vec::new();
    }
    let mean = finite_docs.iter().map(|d| d.score).sum::<f64>() / finite_docs.len() as f64;
    let variance = finite_docs
        .iter()
        .map(|d| (d.score - mean).powi(2))
        .sum::<f64>()
        / finite_docs.len() as f64;
    let stddev = variance.sqrt();

    finite_docs
        .into_iter()
        .map(|d| {
            let z = if stddev > 1e-10 {
                (d.score - mean) / stddev
            } else {
                0.0
            };
            (d.id, z)
        })
        .collect()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::hnsw::{DistanceMetric, HnswConfig, HnswIndex};
    use crate::index::inverted::InvertedIndex;
    use std::collections::HashSet;

    fn bm25_docs(index: &InvertedIndex, query: &str, top_k: usize) -> Vec<ScoredDoc> {
        index
            .search(query, top_k)
            .into_iter()
            .map(|row| ScoredDoc {
                id: row.doc_id as i64,
                score: row.score as f64,
            })
            .collect()
    }

    fn vector_docs(index: &HnswIndex, query: &[f32], top_k: usize) -> Vec<ScoredDoc> {
        index
            .search(query, top_k)
            .into_iter()
            .map(|(id, distance)| ScoredDoc {
                id: id as i64,
                score: distance as f64,
            })
            .collect()
    }

    #[test]
    fn test_rrf_basic() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: 10.0 },
            ScoredDoc { id: 2, score: 8.0 },
            ScoredDoc { id: 3, score: 6.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 2, score: 0.1 },
            ScoredDoc { id: 4, score: 0.2 },
            ScoredDoc { id: 1, score: 0.3 },
        ];
        let results = reciprocal_rank_fusion(&[bm25, vector], 60.0, 5);
        assert!(results.len() <= 5);

        // Doc 1 and 2 appear in both → higher RRF score
        let top_ids: Vec<i64> = results.iter().take(2).map(|r| r.id).collect();
        assert!(top_ids.contains(&1) || top_ids.contains(&2));
    }

    #[test]
    fn test_weighted_linear() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: 10.0 },
            ScoredDoc { id: 2, score: 8.0 },
            ScoredDoc { id: 4, score: 2.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 2, score: 0.05 }, // distance 0.05 → sim 0.95
            ScoredDoc { id: 3, score: 0.2 },  // distance 0.2 → sim 0.8
        ];
        let results = weighted_linear_fusion(&bm25, &vector, 0.5, 5);
        assert!(!results.is_empty());
        // Doc 2 appears in both with high scores → should rank highest
        assert_eq!(results[0].id, 2);
    }

    #[test]
    fn test_dbsf() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: 10.0 },
            ScoredDoc { id: 2, score: 5.0 },
            ScoredDoc { id: 3, score: 2.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 2, score: 0.1 },
            ScoredDoc { id: 4, score: 0.3 },
            ScoredDoc { id: 1, score: 0.5 },
        ];
        let results = distribution_based_fusion(&bm25, &vector, 5);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_full_hybrid_pipeline() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: 10.0 },
            ScoredDoc { id: 2, score: 8.0 },
            ScoredDoc { id: 5, score: 3.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 2, score: 0.1 },
            ScoredDoc { id: 3, score: 0.15 },
            ScoredDoc { id: 1, score: 0.3 },
        ];

        let config = HybridSearchConfig {
            bm25_top_k: 100,
            vector_top_k: 100,
            final_top_k: 3,
            fusion: FusionMethod::ReciprocalRank { k: 60.0 },
            rerank: false,
        };

        let (results, stats) = hybrid_search(&bm25, &vector, &config, None, "test query");
        assert_eq!(results.len(), 3);
        assert_eq!(
            results.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
        assert!(results
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score));
        assert_eq!(
            results.iter().map(|r| r.id).collect::<HashSet<_>>().len(),
            results.len()
        );
        assert!(results.iter().all(|r| r.score.is_finite() && r.score > 0.0));
        assert!(results.iter().all(|r| r.bm25_score.is_none()));
        assert!(results.iter().all(|r| r.vector_distance.is_none()));
        assert!(results.iter().all(|r| r.rerank_score.is_none()));
        assert_eq!(stats.bm25_candidates, bm25.len());
        assert_eq!(stats.vector_candidates, vector.len());
        assert_eq!(stats.fused_candidates, results.len());
        assert!(stats.fusion_time_us <= stats.total_time_us);
        assert_eq!(stats.rerank_time_us, 0);
    }

    #[test]
    fn hybrid_top_k_heap_keeps_candidate_union_dedup_and_alpha_ranking() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: 10.0 },
            ScoredDoc { id: 2, score: 9.0 },
            ScoredDoc { id: 3, score: 8.0 },
            ScoredDoc { id: 4, score: 1.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 4, score: 0.01 },
            ScoredDoc { id: 3, score: 0.20 },
            ScoredDoc { id: 2, score: 0.40 },
            ScoredDoc { id: 5, score: 0.60 },
        ];

        let lexical = weighted_linear_fusion(&bm25, &vector, 0.9, 3);
        let semantic = weighted_linear_fusion(&bm25, &vector, 0.1, 3);

        assert_eq!(lexical[0].id, 1);
        assert_eq!(semantic[0].id, 4);
        for results in [&lexical, &semantic] {
            assert_eq!(results.len(), 3);
            assert_eq!(
                results
                    .iter()
                    .map(|row| row.id)
                    .collect::<HashSet<_>>()
                    .len(),
                results.len()
            );
            assert!(results
                .windows(2)
                .all(|pair| pair[0].score >= pair[1].score));
        }
    }

    #[test]
    fn lexical_heavy_exact_match_beats_vague_semantic_match_fixture() {
        let bm25 = vec![
            ScoredDoc { id: 10, score: 100.0 },
            ScoredDoc { id: 20, score: 5.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 20, score: 0.01 },
            ScoredDoc { id: 10, score: 0.80 },
        ];

        let results = weighted_linear_fusion(&bm25, &vector, 0.95, 2);

        // With alpha=0.95, exact lexical evidence dominates a vague vector match.
        assert_eq!(results[0].id, 10);
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn semantic_heavy_vector_match_beats_weak_lexical_noise_fixture() {
        let bm25 = vec![
            ScoredDoc { id: 10, score: 1.0 },
            ScoredDoc { id: 20, score: 0.9 },
        ];
        let vector = vec![
            ScoredDoc { id: 20, score: 0.01 },
            ScoredDoc { id: 10, score: 0.95 },
        ];

        let results = weighted_linear_fusion(&bm25, &vector, 0.05, 2);

        // With alpha=0.05, a close vector neighbor outranks weak lexical noise.
        assert_eq!(results[0].id, 20);
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn equal_fused_scores_tie_break_by_document_id_fixture() {
        let bm25 = vec![
            ScoredDoc { id: 2, score: 1.0 },
            ScoredDoc { id: 1, score: 1.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 2, score: 0.5 },
            ScoredDoc { id: 1, score: 0.5 },
        ];

        let results = weighted_linear_fusion(&bm25, &vector, 0.5, 2);

        // Equal normalized scores must be stable and sorted by lower document id.
        assert_eq!(results.iter().map(|row| row.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn missing_bm25_or_vector_score_keeps_deterministic_candidate_order_fixture() {
        let bm25 = vec![ScoredDoc { id: 30, score: 7.0 }];
        let vector = vec![ScoredDoc { id: 10, score: 0.1 }];

        let results = weighted_linear_fusion(&bm25, &vector, 0.5, 2);

        // One-source candidates are retained; equal fused scores tie by document id.
        assert_eq!(results.iter().map(|row| row.id).collect::<Vec<_>>(), vec![10, 30]);
        assert_eq!(results[0].bm25_score, None);
        assert_eq!(results[1].vector_distance, None);
    }

    #[test]
    fn non_finite_scores_are_sanitized_before_fusion_fixture() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: f64::NAN },
            ScoredDoc { id: 2, score: 10.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 3, score: f64::INFINITY },
            ScoredDoc { id: 4, score: 0.1 },
        ];

        let weighted = weighted_linear_fusion(&bm25, &vector, 0.5, 10);
        let rrf = reciprocal_rank_fusion(&[bm25, vector], 60.0, 10);

        // NaN/inf inputs are dropped so ranking never depends on partial_cmp fallback.
        assert_eq!(weighted.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(rrf.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2, 4]);
        assert!(weighted.iter().all(|row| row.score.is_finite()));
        assert!(rrf.iter().all(|row| row.score.is_finite()));
    }

    #[test]
    fn test_hybrid_with_reranker() {
        let bm25 = vec![
            ScoredDoc { id: 1, score: 10.0 },
            ScoredDoc { id: 2, score: 8.0 },
        ];
        let vector = vec![
            ScoredDoc { id: 2, score: 0.1 },
            ScoredDoc { id: 3, score: 0.2 },
        ];

        let mut embeddings = AHashMap::new();
        embeddings.insert(1, vec![0.5; 10]);
        embeddings.insert(2, vec![0.9; 10]);
        embeddings.insert(3, vec![0.3; 10]);
        let reranker = DotProductReranker::new(embeddings);

        let config = HybridSearchConfig {
            final_top_k: 3,
            rerank: true,
            ..Default::default()
        };

        let (results, stats) = hybrid_search(&bm25, &vector, &config, Some(&reranker), "query");
        assert!(!results.is_empty());
        assert!(stats.rerank_time_us <= stats.total_time_us);
        // Doc 2 has highest avg embedding → should be #1 after reranking
        assert_eq!(results[0].id, 2);
        assert!(results[0].rerank_score.is_some());
    }

    #[test]
    fn hybrid_inputs_reload_preserve_fusion_and_mutation_visibility() {
        let dir = tempfile::tempdir().unwrap();
        let hnsw_path = dir.path().join("hybrid_hnsw.json");
        let bm25_path = dir.path().join("hybrid_bm25.json");
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 64,
            ef_construction: 64,
            ..HnswConfig::default()
        };
        let mut hnsw = HnswIndex::new(2, config);
        hnsw.insert(1, vec![0.0, 0.0]);
        hnsw.insert(2, vec![0.2, 0.0]);
        hnsw.insert(3, vec![5.0, 0.0]);

        let mut bm25 = InvertedIndex::new();
        bm25.index_document(1, "alpha hybrid");
        bm25.index_document(2, "alpha vector hybrid");
        bm25.index_document(3, "deleted candidate");
        bm25.finalize();

        let hybrid_config = HybridSearchConfig {
            bm25_top_k: 3,
            vector_top_k: 3,
            final_top_k: 3,
            fusion: FusionMethod::ReciprocalRank { k: 60.0 },
            rerank: false,
        };
        let query_vec = [0.0_f32, 0.0];
        let before_bm25 = bm25_docs(&bm25, "alpha hybrid", 3);
        let before_vector = vector_docs(&hnsw, &query_vec, 3);
        let (before, _) = hybrid_search(
            &before_bm25,
            &before_vector,
            &hybrid_config,
            None,
            "alpha hybrid",
        );

        hnsw.save_vectors(&hnsw_path).unwrap();
        bm25.save_documents(&bm25_path).unwrap();
        let hnsw_loaded =
            HnswIndex::load_vectors_with_expected(&hnsw_path, 2, DistanceMetric::L2).unwrap();
        let bm25_loaded = InvertedIndex::load_documents(&bm25_path).unwrap();
        let (after, _) = hybrid_search(
            &bm25_docs(&bm25_loaded, "alpha hybrid", 3),
            &vector_docs(&hnsw_loaded, &query_vec, 3),
            &hybrid_config,
            None,
            "alpha hybrid",
        );
        assert_eq!(
            before.iter().map(|row| row.id).collect::<Vec<_>>(),
            after.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        for (left, right) in before.iter().zip(after.iter()) {
            assert!((left.score - right.score).abs() < 1e-12);
        }

        hnsw.replace(2, vec![0.01, 0.0]);
        assert!(hnsw.remove(3));
        bm25.index_document(2, "alpha updated vector");
        bm25.remove_document(3);
        bm25.finalize();
        hnsw.save_vectors(&hnsw_path).unwrap();
        bm25.save_documents(&bm25_path).unwrap();
        let hnsw_loaded =
            HnswIndex::load_vectors_with_expected(&hnsw_path, 2, DistanceMetric::L2).unwrap();
        let bm25_loaded = InvertedIndex::load_documents(&bm25_path).unwrap();
        let (mutated, _) = hybrid_search(
            &bm25_docs(&bm25_loaded, "alpha updated", 3),
            &vector_docs(&hnsw_loaded, &query_vec, 3),
            &hybrid_config,
            None,
            "alpha updated",
        );
        let ids = mutated.iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
        assert!(!ids.contains(&3));
        assert!(ids.contains(&2));
    }
}
