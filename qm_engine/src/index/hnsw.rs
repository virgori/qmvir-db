/*
 * HNSW + Product Quantization — Vector ANN Index
 *
 * Implements:
 *   • HNSW (Hierarchical Navigable Small World) — multi-layer proximity graph
 *   • Product Quantization (PQ) — subspace vector compression (~100× reduction)
 *   • Two-stage search: PQ candidate retrieval → exact re-ranking
 *
 * Complexity:
 *   • HNSW insert:  O(log n × ef_construction × dim)
 *   • HNSW search:  O(log n × ef_search × dim)
 *   • PQ encode:    O(dim × n_sub × k) — offline
 *   • PQ distance:  O(n_sub) — ADC lookup table
 *
 * Design choices:
 *   • Level probability: 1/ln(M) (standard HNSW)
 *   • Distance metrics: L2, Cosine, Inner Product
 *   • PQ sub-quantizers: 8-byte subspaces with 256 centroids each
 */

use ahash::AHashSet;
use rand::Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fs;
use std::path::Path;
use std::time::Instant;

// ── Distance metrics (SIMD-accelerated) ─────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DistanceMetric {
    L2,
    Cosine,
    InnerProduct,
}

// ── NEON SIMD for aarch64 (Apple Silicon / ARM64) ───────────────────────

#[cfg(target_arch = "aarch64")]
#[inline]
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let n = a.len();
    let chunks = n / 4;
    let remainder = n % 4;
    let mut sum: f32;

    unsafe {
        let mut acc = vdupq_n_f32(0.0);
        let a_ptr = a.as_ptr();
        let b_ptr = b.as_ptr();

        for i in 0..chunks {
            let va = vld1q_f32(a_ptr.add(i * 4));
            let vb = vld1q_f32(b_ptr.add(i * 4));
            let diff = vsubq_f32(va, vb);
            acc = vfmaq_f32(acc, diff, diff); // fused multiply-add
        }

        // Horizontal sum of 4 lanes
        sum = vaddvq_f32(acc);
    }

    // Handle remaining elements
    let tail_start = chunks * 4;
    for i in 0..remainder {
        let d = a[tail_start + i] - b[tail_start + i];
        sum += d * d;
    }
    sum
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let n = a.len();
    let chunks = n / 4;
    let remainder = n % 4;
    let (mut dot, mut norm_a_sq, mut norm_b_sq): (f32, f32, f32);

    unsafe {
        let mut acc_dot = vdupq_n_f32(0.0);
        let mut acc_na = vdupq_n_f32(0.0);
        let mut acc_nb = vdupq_n_f32(0.0);
        let a_ptr = a.as_ptr();
        let b_ptr = b.as_ptr();

        for i in 0..chunks {
            let va = vld1q_f32(a_ptr.add(i * 4));
            let vb = vld1q_f32(b_ptr.add(i * 4));
            acc_dot = vfmaq_f32(acc_dot, va, vb);
            acc_na = vfmaq_f32(acc_na, va, va);
            acc_nb = vfmaq_f32(acc_nb, vb, vb);
        }

        dot = vaddvq_f32(acc_dot);
        norm_a_sq = vaddvq_f32(acc_na);
        norm_b_sq = vaddvq_f32(acc_nb);
    }

    let tail_start = chunks * 4;
    for i in 0..remainder {
        let ai = a[tail_start + i];
        let bi = b[tail_start + i];
        dot += ai * bi;
        norm_a_sq += ai * ai;
        norm_b_sq += bi * bi;
    }

    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }
    1.0 - dot / (norm_a * norm_b)
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn inner_product_distance(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let n = a.len();
    let chunks = n / 4;
    let remainder = n % 4;
    let mut sum: f32;

    unsafe {
        let mut acc = vdupq_n_f32(0.0);
        let a_ptr = a.as_ptr();
        let b_ptr = b.as_ptr();

        for i in 0..chunks {
            let va = vld1q_f32(a_ptr.add(i * 4));
            let vb = vld1q_f32(b_ptr.add(i * 4));
            acc = vfmaq_f32(acc, va, vb);
        }

        sum = vaddvq_f32(acc);
    }

    let tail_start = chunks * 4;
    for i in 0..remainder {
        sum += a[tail_start + i] * b[tail_start + i];
    }
    -sum
}

// ── x86-64 SIMD (AVX-512F → AVX2 → SSE2 cascade) ───────────────────────

#[cfg(target_arch = "x86_64")]
#[inline]
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    if is_x86_feature_detected!("avx512f") {
        unsafe { l2_distance_avx512(a, b) }
    } else if is_x86_feature_detected!("avx2") {
        unsafe { l2_distance_avx2(a, b) }
    } else {
        unsafe { l2_distance_sse2(a, b) }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn l2_distance_avx512(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 16;
    let remainder = n % 16;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc = _mm512_setzero_ps();
    for i in 0..chunks {
        let va = _mm512_loadu_ps(a_ptr.add(i * 16));
        let vb = _mm512_loadu_ps(b_ptr.add(i * 16));
        let diff = _mm512_sub_ps(va, vb);
        acc = _mm512_fmadd_ps(diff, diff, acc);
    }

    let mut sum = _mm512_reduce_add_ps(acc);

    let tail_start = chunks * 16;
    for i in 0..remainder {
        let d = a[tail_start + i] - b[tail_start + i];
        sum += d * d;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn l2_distance_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 8;
    let remainder = n % 8;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc = _mm256_setzero_ps();
    for i in 0..chunks {
        let va = _mm256_loadu_ps(a_ptr.add(i * 8));
        let vb = _mm256_loadu_ps(b_ptr.add(i * 8));
        let diff = _mm256_sub_ps(va, vb);
        acc = _mm256_fmadd_ps(diff, diff, acc); // FMA: acc += diff * diff
    }

    // Horizontal sum: 8 → 4 → scalar
    let hi = _mm256_extractf128_ps(acc, 1);
    let lo = _mm256_castps256_ps128(acc);
    let sum128 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehdup_ps(sum128);
    let sums = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    let mut sum = _mm_cvtss_f32(_mm_add_ss(sums, shuf2));

    let tail_start = chunks * 8;
    for i in 0..remainder {
        let d = a[tail_start + i] - b[tail_start + i];
        sum += d * d;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn l2_distance_sse2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 4;
    let remainder = n % 4;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc = _mm_setzero_ps();
    for i in 0..chunks {
        let va = _mm_loadu_ps(a_ptr.add(i * 4));
        let vb = _mm_loadu_ps(b_ptr.add(i * 4));
        let diff = _mm_sub_ps(va, vb);
        let sq = _mm_mul_ps(diff, diff);
        acc = _mm_add_ps(acc, sq);
    }

    let shuf = _mm_movehdup_ps(acc);
    let sums = _mm_add_ps(acc, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    let mut sum = _mm_cvtss_f32(_mm_add_ss(sums, shuf2));

    let tail_start = chunks * 4;
    for i in 0..remainder {
        let d = a[tail_start + i] - b[tail_start + i];
        sum += d * d;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if is_x86_feature_detected!("avx512f") {
        unsafe { cosine_distance_avx512(a, b) }
    } else if is_x86_feature_detected!("avx2") {
        unsafe { cosine_distance_avx2(a, b) }
    } else {
        unsafe { cosine_distance_sse2(a, b) }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn cosine_distance_avx512(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 16;
    let remainder = n % 16;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc_dot = _mm512_setzero_ps();
    let mut acc_na = _mm512_setzero_ps();
    let mut acc_nb = _mm512_setzero_ps();

    for i in 0..chunks {
        let va = _mm512_loadu_ps(a_ptr.add(i * 16));
        let vb = _mm512_loadu_ps(b_ptr.add(i * 16));
        acc_dot = _mm512_fmadd_ps(va, vb, acc_dot);
        acc_na = _mm512_fmadd_ps(va, va, acc_na);
        acc_nb = _mm512_fmadd_ps(vb, vb, acc_nb);
    }

    let mut dot = _mm512_reduce_add_ps(acc_dot);
    let mut norm_a_sq = _mm512_reduce_add_ps(acc_na);
    let mut norm_b_sq = _mm512_reduce_add_ps(acc_nb);

    let tail_start = chunks * 16;
    for i in 0..remainder {
        let ai = a[tail_start + i];
        let bi = b[tail_start + i];
        dot += ai * bi;
        norm_a_sq += ai * ai;
        norm_b_sq += bi * bi;
    }

    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }
    1.0 - dot / (norm_a * norm_b)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn cosine_distance_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 8;
    let remainder = n % 8;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc_dot = _mm256_setzero_ps();
    let mut acc_na = _mm256_setzero_ps();
    let mut acc_nb = _mm256_setzero_ps();

    for i in 0..chunks {
        let va = _mm256_loadu_ps(a_ptr.add(i * 8));
        let vb = _mm256_loadu_ps(b_ptr.add(i * 8));
        acc_dot = _mm256_fmadd_ps(va, vb, acc_dot);
        acc_na = _mm256_fmadd_ps(va, va, acc_na);
        acc_nb = _mm256_fmadd_ps(vb, vb, acc_nb);
    }

    // Horizontal sums
    let hsum256 = |v: __m256| -> f32 {
        let hi = _mm256_extractf128_ps(v, 1);
        let lo = _mm256_castps256_ps128(v);
        let sum128 = _mm_add_ps(lo, hi);
        let shuf = _mm_movehdup_ps(sum128);
        let sums = _mm_add_ps(sum128, shuf);
        let shuf2 = _mm_movehl_ps(sums, sums);
        _mm_cvtss_f32(_mm_add_ss(sums, shuf2))
    };

    let mut dot = hsum256(acc_dot);
    let mut norm_a_sq = hsum256(acc_na);
    let mut norm_b_sq = hsum256(acc_nb);

    let tail_start = chunks * 8;
    for i in 0..remainder {
        let ai = a[tail_start + i];
        let bi = b[tail_start + i];
        dot += ai * bi;
        norm_a_sq += ai * ai;
        norm_b_sq += bi * bi;
    }

    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }
    1.0 - dot / (norm_a * norm_b)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn cosine_distance_sse2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 4;
    let remainder = n % 4;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc_dot = _mm_setzero_ps();
    let mut acc_na = _mm_setzero_ps();
    let mut acc_nb = _mm_setzero_ps();

    for i in 0..chunks {
        let va = _mm_loadu_ps(a_ptr.add(i * 4));
        let vb = _mm_loadu_ps(b_ptr.add(i * 4));
        acc_dot = _mm_add_ps(acc_dot, _mm_mul_ps(va, vb));
        acc_na = _mm_add_ps(acc_na, _mm_mul_ps(va, va));
        acc_nb = _mm_add_ps(acc_nb, _mm_mul_ps(vb, vb));
    }

    let hsum = |v: __m128| -> f32 {
        let shuf = _mm_movehdup_ps(v);
        let sums = _mm_add_ps(v, shuf);
        let shuf2 = _mm_movehl_ps(sums, sums);
        _mm_cvtss_f32(_mm_add_ss(sums, shuf2))
    };
    let mut dot = hsum(acc_dot);
    let mut norm_a_sq = hsum(acc_na);
    let mut norm_b_sq = hsum(acc_nb);

    let tail_start = chunks * 4;
    for i in 0..remainder {
        let ai = a[tail_start + i];
        let bi = b[tail_start + i];
        dot += ai * bi;
        norm_a_sq += ai * ai;
        norm_b_sq += bi * bi;
    }

    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }
    1.0 - dot / (norm_a * norm_b)
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn inner_product_distance(a: &[f32], b: &[f32]) -> f32 {
    if is_x86_feature_detected!("avx512f") {
        unsafe { inner_product_distance_avx512(a, b) }
    } else if is_x86_feature_detected!("avx2") {
        unsafe { inner_product_distance_avx2(a, b) }
    } else {
        unsafe { inner_product_distance_sse2(a, b) }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn inner_product_distance_avx512(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 16;
    let remainder = n % 16;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc = _mm512_setzero_ps();
    for i in 0..chunks {
        let va = _mm512_loadu_ps(a_ptr.add(i * 16));
        let vb = _mm512_loadu_ps(b_ptr.add(i * 16));
        acc = _mm512_fmadd_ps(va, vb, acc);
    }

    let mut sum = _mm512_reduce_add_ps(acc);

    let tail_start = chunks * 16;
    for i in 0..remainder {
        sum += a[tail_start + i] * b[tail_start + i];
    }
    -sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn inner_product_distance_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 8;
    let remainder = n % 8;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc = _mm256_setzero_ps();
    for i in 0..chunks {
        let va = _mm256_loadu_ps(a_ptr.add(i * 8));
        let vb = _mm256_loadu_ps(b_ptr.add(i * 8));
        acc = _mm256_fmadd_ps(va, vb, acc);
    }

    let hi = _mm256_extractf128_ps(acc, 1);
    let lo = _mm256_castps256_ps128(acc);
    let sum128 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehdup_ps(sum128);
    let sums = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    let mut sum = _mm_cvtss_f32(_mm_add_ss(sums, shuf2));

    let tail_start = chunks * 8;
    for i in 0..remainder {
        sum += a[tail_start + i] * b[tail_start + i];
    }
    -sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn inner_product_distance_sse2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 4;
    let remainder = n % 4;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    let mut acc = _mm_setzero_ps();
    for i in 0..chunks {
        let va = _mm_loadu_ps(a_ptr.add(i * 4));
        let vb = _mm_loadu_ps(b_ptr.add(i * 4));
        acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
    }

    let shuf = _mm_movehdup_ps(acc);
    let sums = _mm_add_ps(acc, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    let mut sum = _mm_cvtss_f32(_mm_add_ss(sums, shuf2));

    let tail_start = chunks * 4;
    for i in 0..remainder {
        sum += a[tail_start + i] * b[tail_start + i];
    }
    -sum
}

// ── Scalar fallback for other architectures ─────────────────────────────

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }
    1.0 - dot / (norm_a * norm_b)
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn inner_product_distance(a: &[f32], b: &[f32]) -> f32 {
    -a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
}

#[inline]
fn compute_distance(a: &[f32], b: &[f32], metric: &DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::L2 => l2_distance(a, b),
        DistanceMetric::Cosine => cosine_distance(a, b),
        DistanceMetric::InnerProduct => inner_product_distance(a, b),
    }
}

// ── Neighbor candidate (for priority queues) ────────────────────────────

#[derive(Clone, Debug)]
struct Candidate {
    id: u32,
    distance: f32,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Max-heap: worst candidate at top (for eviction)
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(Ordering::Equal)
    }
}

// Min-heap wrapper
#[derive(Clone, Debug)]
struct MinCandidate(Candidate);

impl PartialEq for MinCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}
impl Eq for MinCandidate {}

impl PartialOrd for MinCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MinCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .distance
            .partial_cmp(&self.0.distance)
            .unwrap_or(Ordering::Equal)
    }
}

// ── HNSW graph node ─────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct HnswNode {
    vector: Vec<f32>,
    /// Neighbors at each level: level → list of neighbor IDs
    neighbors: Vec<Vec<u32>>,
}

// ── HNSW config ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HnswConfig {
    /// Max neighbors per node per level
    pub m: usize,
    /// Max neighbors at level 0 (typically 2×M)
    pub m0: usize,
    /// Construction beam width
    pub ef_construction: usize,
    /// Search beam width
    pub ef_search: usize,
    /// Distance metric
    pub metric: DistanceMetric,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HnswMutationPolicy {
    RebuildImmediate,
    LazyTombstone,
    AutoCompact {
        threshold: f64,
    },
    AdaptiveCompact {
        min_threshold: f64,
        max_threshold: f64,
        search_latency_multiplier: f64,
        max_tombstone_ratio: f64,
        max_internal_growth_ratio: f64,
    },
}

impl Default for HnswMutationPolicy {
    fn default() -> Self {
        Self::RebuildImmediate
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HnswSearchStats {
    pub distance_evaluations: usize,
    pub candidate_pushes: usize,
    pub candidate_pops: usize,
    pub visited_nodes: usize,
    pub max_candidate_queue_size: usize,
    pub result_heap_size: usize,
    pub tombstone_checks: usize,
    pub tombstone_hits: usize,
    pub tombstone_ratio: f64,
    pub tombstone_fast_path_used: bool,
    pub no_tombstone_fast_path_used: bool,
    pub compact_triggered: bool,
    pub live_candidates_returned: usize,
    pub filtered_candidates_count: usize,
    pub stale_generation_filtered_count: usize,
    pub live_external_candidates: usize,
    pub duplicate_external_filtered_count: usize,
    pub current_generation_checks: usize,
    pub generation_model_enabled: bool,
    pub internal_node_count: usize,
    pub live_external_id_count: usize,
    pub metric_dispatch_count: usize,
    pub norm_cache_used: bool,
    pub ef_search: usize,
    pub m: usize,
    pub average_degree: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HnswCompactionProfile {
    pub compact_total_ms: f64,
    pub live_vectors_collected: usize,
    pub tombstones_removed: usize,
    pub old_internal_node_count: usize,
    pub new_internal_node_count: usize,
    pub graph_rebuild_ms: f64,
    pub vector_copy_ms: f64,
    pub map_rebuild_ms: f64,
    pub level_assignment_ms: f64,
    pub neighbor_build_ms: f64,
    pub distance_eval_count: Option<usize>,
    pub memory_before_bytes: usize,
    pub memory_after_bytes: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 50,
            metric: DistanceMetric::L2,
        }
    }
}

// ── HNSW Index ──────────────────────────────────────────────────────────

/// HNSW — Hierarchical Navigable Small World graph for ANN search.
#[derive(Clone)]
pub struct HnswIndex {
    nodes: HashMap<u32, HnswNode>,
    entry_point: Option<u32>,
    max_level: usize,
    config: HnswConfig,
    dim: usize,
    /// Level multiplier: 1/ln(M)
    level_mult: f64,
    level_rng_state: u64,
    tombstoned_ids: AHashSet<u32>,
    mutation_policy: HnswMutationPolicy,
    external_to_internal: HashMap<u32, u32>,
    internal_to_external: HashMap<u32, u32>,
    internal_generation: HashMap<u32, u64>,
    latest_generation: HashMap<u32, u64>,
    next_internal_node_id: u32,
    generation_counter: u64,
    compact_trigger_count: usize,
    auto_compact_total_ms: f64,
    last_compaction_profile: Option<HnswCompactionProfile>,
    adaptive_trigger_count: usize,
    last_adaptive_trigger_reason: Option<String>,
    last_compact_skipped_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HnswVectorSnapshot {
    version: u32,
    dim: usize,
    config: HnswConfig,
    vectors: Vec<(u32, Vec<f32>)>,
}

impl HnswIndex {
    /// Fixed seed for geometric level assignment — insert order must be sorted by
    /// external id (`batch_build`) so graphs are reproducible across platforms.
    const LEVEL_RNG_SEED: u64 = 0x4E4E_5F42_454E_4348;

    pub fn new(dim: usize, config: HnswConfig) -> Self {
        let level_mult = 1.0 / (config.m as f64).ln();
        let level_rng_state = Self::LEVEL_RNG_SEED
            ^ ((dim as u64) << 32)
            ^ ((config.m as u64) << 16)
            ^ config.m0 as u64
            ^ ((config.ef_construction as u64) << 1)
            ^ ((config.ef_search as u64) << 48);
        Self {
            nodes: HashMap::new(),
            entry_point: None,
            max_level: 0,
            config,
            dim,
            level_mult,
            level_rng_state,
            tombstoned_ids: AHashSet::new(),
            mutation_policy: HnswMutationPolicy::default(),
            external_to_internal: HashMap::new(),
            internal_to_external: HashMap::new(),
            internal_generation: HashMap::new(),
            latest_generation: HashMap::new(),
            next_internal_node_id: 0,
            generation_counter: 0,
            compact_trigger_count: 0,
            auto_compact_total_ms: 0.0,
            last_compaction_profile: None,
            adaptive_trigger_count: 0,
            last_adaptive_trigger_reason: None,
            last_compact_skipped_reason: None,
        }
    }

    pub fn with_default_config(dim: usize) -> Self {
        Self::new(dim, HnswConfig::default())
    }

    /// Random level assignment (geometric distribution).
    fn random_level(&mut self) -> usize {
        let r = self.next_level_random_f64();
        (-r.ln() * self.level_mult).floor() as usize
    }

    fn next_level_random_f64(&mut self) -> f64 {
        self.level_rng_state = self.level_rng_state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.level_rng_state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let mantissa = z >> 11;
        ((mantissa as f64) + 1.0) / (((1u64 << 53) as f64) + 1.0)
    }

    /// Get max neighbors for a given level.
    fn max_neighbors(&self, level: usize) -> usize {
        if level == 0 {
            self.config.m0
        } else {
            self.config.m
        }
    }

    /// Insert a vector into the index.
    pub fn insert(&mut self, id: u32, vector: Vec<f32>) {
        assert_eq!(vector.len(), self.dim, "Vector dimension mismatch");
        assert!(
            vector.iter().all(|x| x.is_finite()),
            "Vector must contain only finite values"
        );

        if self.external_to_internal.contains_key(&id) {
            self.replace(id, vector);
            return;
        }
        self.insert_live_generation(id, vector);
    }

    fn allocate_internal_node_id(&mut self, preferred: u32) -> u32 {
        if !self.nodes.contains_key(&preferred) && preferred >= self.next_internal_node_id {
            self.next_internal_node_id = preferred.saturating_add(1);
            return preferred;
        }
        while self.nodes.contains_key(&self.next_internal_node_id) {
            self.next_internal_node_id = self.next_internal_node_id.saturating_add(1);
        }
        let internal_id = self.next_internal_node_id;
        self.next_internal_node_id = self.next_internal_node_id.saturating_add(1);
        internal_id
    }

    fn insert_live_generation(&mut self, external_id: u32, vector: Vec<f32>) -> u32 {
        let internal_id = self.allocate_internal_node_id(external_id);
        self.generation_counter = self.generation_counter.saturating_add(1);
        let generation = self.generation_counter;
        self.insert_internal_node(internal_id, vector);
        self.external_to_internal.insert(external_id, internal_id);
        self.internal_to_external.insert(internal_id, external_id);
        self.internal_generation.insert(internal_id, generation);
        self.latest_generation.insert(external_id, generation);
        self.tombstoned_ids.remove(&internal_id);
        internal_id
    }

    fn insert_internal_node(&mut self, id: u32, vector: Vec<f32>) {
        let level = self.random_level();

        if self.entry_point.is_none() {
            let node = HnswNode {
                vector,
                neighbors: vec![Vec::new(); level + 1],
            };
            self.entry_point = Some(id);
            self.max_level = level;
            self.nodes.insert(id, node);
            return;
        }

        // Insert the node into self.nodes FIRST (with empty neighbors)
        // so that shrink_neighbors can find it during bidirectional edge creation.
        let node = HnswNode {
            vector: vector.clone(),
            neighbors: vec![Vec::new(); level + 1],
        };
        self.nodes.insert(id, node);

        let entry_id = self.entry_point.unwrap();
        let mut curr = entry_id;

        // Adaptive ef_construction: reduce beam width for small graphs
        // When graph is small, ef_construction >> n just wastes cycles visiting the same nodes.
        let n = self.nodes.len();
        let ef_c = self.config.ef_construction.min(n);

        // Phase 1: Greedy descent from top level to node's level + 1
        for lev in (level + 1..=self.max_level).rev() {
            curr = self.greedy_closest(curr, &vector, lev);
        }

        // Phase 2: From min(level, max_level) down to 0, insert with ef_construction beam
        let start_level = level.min(self.max_level);
        for lev in (0..=start_level).rev() {
            let mut neighbors = self.search_layer(curr, &vector, ef_c, lev);

            // Select neighbors using heuristic (diverse) selection.
            // Limit candidates to 2×max_n closest to keep heuristic O(M² × dim)
            // instead of O(ef × M × dim).
            let max_n = self.max_neighbors(lev);
            neighbors.truncate(max_n * 2);
            let selected = self.select_neighbors_heuristic(&vector, &neighbors, max_n);

            // Set node's neighbors at this level
            if let Some(node) = self.nodes.get_mut(&id) {
                if lev < node.neighbors.len() {
                    node.neighbors[lev] = selected.clone();
                }
            }

            // Add bidirectional connections
            for &neighbor_id in &selected {
                if let Some(neighbor) = self.nodes.get_mut(&neighbor_id) {
                    if lev < neighbor.neighbors.len() {
                        if !neighbor.neighbors[lev].contains(&id) {
                            neighbor.neighbors[lev].push(id);
                            // Shrink if exceeds max
                            if neighbor.neighbors[lev].len() > max_n {
                                let nv = neighbor.vector.clone();
                                self.shrink_neighbors(neighbor_id, &nv, lev, max_n);
                            }
                        }
                    }
                }
            }

            if !neighbors.is_empty() {
                curr = neighbors[0].id;
            }
        }

        if level > self.max_level {
            self.max_level = level;
            self.entry_point = Some(id);
        }
    }

    /// Replace an existing vector, or insert it if the ID is new.
    ///
    /// HNSW graph edges depend on vector positions. A correctness-first replace
    /// rebuilds the graph from live vectors so stale neighbor edges cannot affect
    /// search semantics.
    pub fn replace(&mut self, id: u32, vector: Vec<f32>) {
        assert_eq!(vector.len(), self.dim, "Vector dimension mismatch");
        assert!(
            vector.iter().all(|x| x.is_finite()),
            "Vector must contain only finite values"
        );
        if self.uses_tombstones() {
            if let Some(old_internal) = self.external_to_internal.remove(&id) {
                self.tombstoned_ids.insert(old_internal);
            }
            let new_internal = self.insert_live_generation(id, vector);
            self.entry_point = Some(new_internal);
            self.compact_if_needed();
            return;
        }
        let mut vectors: Vec<(u32, Vec<f32>)> = self
            .live_vectors()
            .into_iter()
            .filter(|(external_id, _)| *external_id != id)
            .collect();
        vectors.push((id, vector));
        vectors.sort_by_key(|(external_id, _)| *external_id);
        self.rebuild_from(vectors);
    }

    /// Remove a vector from the index. Returns true when a live vector existed.
    pub fn remove(&mut self, id: u32) -> bool {
        let Some(internal_id) = self.external_to_internal.get(&id).copied() else {
            return false;
        };
        if self.uses_tombstones() {
            self.external_to_internal.remove(&id);
            let was_live = self.tombstoned_ids.insert(internal_id);
            if was_live {
                self.compact_if_needed();
            }
            return was_live;
        }
        let mut vectors: Vec<(u32, Vec<f32>)> = self
            .live_vectors()
            .into_iter()
            .filter(|(external_id, _)| *external_id != id)
            .collect();
        vectors.sort_by_key(|(external_id, _)| *external_id);
        self.rebuild_from(vectors);
        true
    }

    /// Replace multiple vectors with one correctness rebuild.
    ///
    /// Later entries for the same ID win, matching repeated single-ID replace
    /// semantics without paying one full graph rebuild per mutation.
    pub fn batch_replace(&mut self, replacements: Vec<(u32, Vec<f32>)>) {
        if replacements.is_empty() {
            return;
        }
        if self.uses_tombstones() {
            let mut latest: HashMap<u32, Vec<f32>> = HashMap::new();
            for (id, vector) in replacements {
                assert_eq!(vector.len(), self.dim, "Vector dimension mismatch");
                assert!(
                    vector.iter().all(|x| x.is_finite()),
                    "Vector must contain only finite values"
                );
                latest.insert(id, vector);
            }
            let mut replacements: Vec<(u32, Vec<f32>)> = latest.into_iter().collect();
            replacements.sort_by_key(|(id, _)| *id);
            for (id, vector) in replacements {
                if let Some(old_internal) = self.external_to_internal.remove(&id) {
                    self.tombstoned_ids.insert(old_internal);
                }
                let new_internal = self.insert_live_generation(id, vector);
                self.entry_point = Some(new_internal);
            }
            self.compact_if_needed();
            return;
        }
        let mut live: HashMap<u32, Vec<f32>> = self.live_vectors().into_iter().collect();
        for (id, vector) in replacements {
            assert_eq!(vector.len(), self.dim, "Vector dimension mismatch");
            assert!(
                vector.iter().all(|x| x.is_finite()),
                "Vector must contain only finite values"
            );
            live.insert(id, vector);
        }
        let mut vectors: Vec<(u32, Vec<f32>)> = live.into_iter().collect();
        vectors.sort_by_key(|(external_id, _)| *external_id);
        self.rebuild_from(vectors);
    }

    /// Remove multiple vectors with one correctness rebuild.
    ///
    /// Returns the number of live vectors removed. Unknown IDs are ignored.
    pub fn batch_remove(&mut self, ids: &[u32]) -> usize {
        if ids.is_empty() {
            return 0;
        }
        if self.uses_tombstones() {
            let mut removed = 0usize;
            for &id in ids {
                if let Some(internal_id) = self.external_to_internal.remove(&id) {
                    self.tombstoned_ids.insert(internal_id);
                    removed += 1;
                }
            }
            if removed > 0 {
                self.compact_if_needed();
            }
            return removed;
        }
        let remove_ids: AHashSet<u32> = ids.iter().copied().collect();
        let removed = self
            .external_to_internal
            .keys()
            .filter(|external_id| remove_ids.contains(external_id))
            .count();
        if removed == 0 {
            return 0;
        }
        let mut vectors: Vec<(u32, Vec<f32>)> = self
            .live_vectors()
            .into_iter()
            .filter(|(external_id, _)| !remove_ids.contains(external_id))
            .collect();
        vectors.sort_by_key(|(external_id, _)| *external_id);
        self.rebuild_from(vectors);
        removed
    }

    pub fn graph_node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn total_node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn tombstone_count(&self) -> usize {
        self.tombstoned_ids.len()
    }

    pub fn tombstone_ratio(&self) -> f64 {
        if self.nodes.is_empty() {
            0.0
        } else {
            self.tombstoned_ids.len() as f64 / self.nodes.len() as f64
        }
    }

    pub fn live_count(&self) -> usize {
        self.external_to_internal.len()
    }

    pub fn internal_node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn current_internal_node_id(&self, external_id: u32) -> Option<u32> {
        self.external_to_internal.get(&external_id).copied()
    }

    pub fn latest_generation_for_external_id(&self, external_id: u32) -> Option<u64> {
        self.latest_generation.get(&external_id).copied()
    }

    pub fn compact_trigger_count(&self) -> usize {
        self.compact_trigger_count
    }

    pub fn auto_compact_total_ms(&self) -> f64 {
        self.auto_compact_total_ms
    }

    pub fn last_compaction_profile(&self) -> Option<&HnswCompactionProfile> {
        self.last_compaction_profile.as_ref()
    }

    pub fn adaptive_trigger_count(&self) -> usize {
        self.adaptive_trigger_count
    }

    pub fn last_adaptive_trigger_reason(&self) -> Option<&str> {
        self.last_adaptive_trigger_reason.as_deref()
    }

    pub fn last_compact_skipped_reason(&self) -> Option<&str> {
        self.last_compact_skipped_reason.as_deref()
    }

    pub fn internal_growth_ratio(&self) -> f64 {
        let live = self.live_count();
        if live == 0 {
            if self.internal_node_count() == 0 {
                1.0
            } else {
                f64::INFINITY
            }
        } else {
            self.internal_node_count() as f64 / live as f64
        }
    }

    pub fn average_degree(&self) -> f64 {
        let mut level_count = 0usize;
        let mut edge_count = 0usize;
        for node in self.nodes.values() {
            for neighbors in &node.neighbors {
                level_count += 1;
                edge_count += neighbors.len();
            }
        }
        if level_count == 0 {
            0.0
        } else {
            edge_count as f64 / level_count as f64
        }
    }

    pub fn config(&self) -> &HnswConfig {
        &self.config
    }

    pub fn mutation_policy(&self) -> &HnswMutationPolicy {
        &self.mutation_policy
    }

    pub fn set_mutation_policy(&mut self, policy: HnswMutationPolicy) {
        match policy {
            HnswMutationPolicy::AutoCompact { threshold } => {
                assert!(
                    (0.0..=1.0).contains(&threshold),
                    "AutoCompact threshold must be between 0.0 and 1.0"
                );
                self.mutation_policy = HnswMutationPolicy::AutoCompact { threshold };
            }
            HnswMutationPolicy::AdaptiveCompact {
                min_threshold,
                max_threshold,
                search_latency_multiplier,
                max_tombstone_ratio,
                max_internal_growth_ratio,
            } => {
                assert!(
                    (0.0..=1.0).contains(&min_threshold)
                        && (0.0..=1.0).contains(&max_threshold)
                        && min_threshold <= max_threshold,
                    "AdaptiveCompact thresholds must be between 0.0 and 1.0 with min_threshold <= max_threshold"
                );
                assert!(
                    search_latency_multiplier >= 1.0,
                    "AdaptiveCompact search_latency_multiplier must be at least 1.0"
                );
                assert!(
                    (0.0..=1.0).contains(&max_tombstone_ratio),
                    "AdaptiveCompact max_tombstone_ratio must be between 0.0 and 1.0"
                );
                assert!(
                    max_internal_growth_ratio >= 1.0,
                    "AdaptiveCompact max_internal_growth_ratio must be at least 1.0"
                );
                self.mutation_policy = HnswMutationPolicy::AdaptiveCompact {
                    min_threshold,
                    max_threshold,
                    search_latency_multiplier,
                    max_tombstone_ratio,
                    max_internal_growth_ratio,
                };
            }
            other => {
                self.mutation_policy = other;
            }
        }
        self.compact_if_needed();
    }

    fn rebuild_from(&mut self, vectors: Vec<(u32, Vec<f32>)>) {
        let config = self.config.clone();
        let mutation_policy = self.mutation_policy.clone();
        let compact_trigger_count = self.compact_trigger_count;
        let auto_compact_total_ms = self.auto_compact_total_ms;
        let last_compaction_profile = self.last_compaction_profile.clone();
        let adaptive_trigger_count = self.adaptive_trigger_count;
        let last_adaptive_trigger_reason = self.last_adaptive_trigger_reason.clone();
        let last_compact_skipped_reason = self.last_compact_skipped_reason.clone();
        *self = Self::new(self.dim, config);
        self.mutation_policy = mutation_policy;
        self.compact_trigger_count = compact_trigger_count;
        self.auto_compact_total_ms = auto_compact_total_ms;
        self.last_compaction_profile = last_compaction_profile;
        self.adaptive_trigger_count = adaptive_trigger_count;
        self.last_adaptive_trigger_reason = last_adaptive_trigger_reason;
        self.last_compact_skipped_reason = last_compact_skipped_reason;
        for (id, vector) in vectors {
            self.insert(id, vector);
        }
        self.tombstoned_ids.clear();
    }

    pub fn compact(&mut self) {
        if self.tombstoned_ids.is_empty() {
            self.last_compact_skipped_reason = Some("no_tombstones".to_string());
            return;
        }
        let total_start = Instant::now();
        let old_internal_node_count = self.internal_node_count();
        let tombstones_removed = self.tombstone_count();
        let memory_before_bytes = self.estimated_memory_bytes();
        let copy_start = Instant::now();
        let mut vectors = self.live_vectors();
        let vector_copy_ms = copy_start.elapsed().as_secs_f64() * 1000.0;
        let map_start = Instant::now();
        vectors.sort_by_key(|(external_id, _)| *external_id);
        let map_rebuild_ms = map_start.elapsed().as_secs_f64() * 1000.0;
        let live_vectors_collected = vectors.len();
        let rebuild_start = Instant::now();
        self.rebuild_from(vectors);
        let graph_rebuild_ms = rebuild_start.elapsed().as_secs_f64() * 1000.0;
        let memory_after_bytes = self.estimated_memory_bytes();
        let compact_total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
        self.last_compaction_profile = Some(HnswCompactionProfile {
            compact_total_ms,
            live_vectors_collected,
            tombstones_removed,
            old_internal_node_count,
            new_internal_node_count: self.internal_node_count(),
            graph_rebuild_ms,
            vector_copy_ms,
            map_rebuild_ms,
            level_assignment_ms: 0.0,
            neighbor_build_ms: graph_rebuild_ms,
            distance_eval_count: None,
            memory_before_bytes,
            memory_after_bytes,
        });
        self.last_compact_skipped_reason = None;
    }

    fn uses_tombstones(&self) -> bool {
        !matches!(self.mutation_policy, HnswMutationPolicy::RebuildImmediate)
    }

    fn compact_if_needed(&mut self) -> bool {
        let trigger_reason = match self.mutation_policy {
            HnswMutationPolicy::AutoCompact { threshold }
                if self.tombstone_ratio() >= threshold && self.tombstone_count() > 0 =>
            {
                Some("tombstone_ratio")
            }
            HnswMutationPolicy::AdaptiveCompact {
                min_threshold,
                max_threshold,
                max_tombstone_ratio,
                max_internal_growth_ratio,
                ..
            } if self.tombstone_count() > 0 => {
                let tombstone_ratio = self.tombstone_ratio();
                let internal_growth_ratio = self.internal_growth_ratio();
                if tombstone_ratio >= max_tombstone_ratio {
                    Some("max_tombstone_ratio")
                } else if tombstone_ratio >= max_threshold {
                    Some("max_threshold")
                } else if internal_growth_ratio >= max_internal_growth_ratio
                    && tombstone_ratio >= min_threshold
                {
                    Some("max_internal_growth_ratio")
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(reason) = trigger_reason {
            let adaptive = matches!(
                self.mutation_policy,
                HnswMutationPolicy::AdaptiveCompact { .. }
            );
            self.compact();
            let compact_ms = self
                .last_compaction_profile
                .as_ref()
                .map(|profile| profile.compact_total_ms)
                .unwrap_or(0.0);
            self.auto_compact_total_ms += compact_ms;
            self.compact_trigger_count = self.compact_trigger_count.saturating_add(1);
            if adaptive {
                self.adaptive_trigger_count = self.adaptive_trigger_count.saturating_add(1);
                self.last_adaptive_trigger_reason = Some(reason.to_string());
            }
            true
        } else {
            if matches!(
                self.mutation_policy,
                HnswMutationPolicy::AdaptiveCompact { .. }
            ) {
                self.last_compact_skipped_reason =
                    Some("below_adaptive_thresholds_or_search_latency_not_evaluated_without_query_context".to_string());
            }
            false
        }
    }

    fn live_vectors(&self) -> Vec<(u32, Vec<f32>)> {
        let mut vectors = Vec::with_capacity(self.external_to_internal.len());
        for (&external_id, &internal_id) in &self.external_to_internal {
            if self.tombstoned_ids.contains(&internal_id) {
                continue;
            }
            if let Some(node) = self.nodes.get(&internal_id) {
                vectors.push((external_id, node.vector.clone()));
            }
        }
        vectors
    }

    fn estimated_memory_bytes(&self) -> usize {
        let vector_bytes = self.nodes.len() * self.dim * std::mem::size_of::<f32>();
        let edge_count: usize = self
            .nodes
            .values()
            .map(|node| node.neighbors.iter().map(Vec::len).sum::<usize>())
            .sum();
        let edge_bytes = edge_count * std::mem::size_of::<u32>();
        let generation_bytes = (self.internal_generation.len()
            + self.latest_generation.len()
            + self.external_to_internal.len()
            + self.internal_to_external.len())
            * (std::mem::size_of::<u32>() + std::mem::size_of::<u64>());
        vector_bytes + edge_bytes + generation_bytes
    }

    /// Persist a correctness snapshot of live vectors and configuration.
    ///
    /// Graph links are intentionally rebuilt on load because replace/remove already
    /// use correctness-first rebuilds and persisted links need their own corruption
    /// and compatibility contract.
    pub fn save_vectors<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let mut vectors: Vec<(u32, Vec<f32>)> = self.live_vectors();
        vectors.sort_by_key(|(id, _)| *id);
        let snapshot = HnswVectorSnapshot {
            version: 1,
            dim: self.dim,
            config: self.config.clone(),
            vectors,
        };
        let encoded = serde_json::to_vec_pretty(&snapshot)
            .map_err(|err| format!("failed to encode HNSW vector snapshot: {err}"))?;
        fs::write(path, encoded).map_err(|err| format!("failed to write HNSW snapshot: {err}"))
    }

    pub fn load_vectors<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|err| format!("failed to read HNSW snapshot: {err}"))?;
        let snapshot: HnswVectorSnapshot = serde_json::from_slice(&bytes)
            .map_err(|err| format!("invalid HNSW snapshot JSON: {err}"))?;
        Self::from_vector_snapshot(snapshot, None)
    }

    pub fn load_vectors_with_expected<P: AsRef<Path>>(
        path: P,
        expected_dim: usize,
        expected_metric: DistanceMetric,
    ) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|err| format!("failed to read HNSW snapshot: {err}"))?;
        let snapshot: HnswVectorSnapshot = serde_json::from_slice(&bytes)
            .map_err(|err| format!("invalid HNSW snapshot JSON: {err}"))?;
        Self::from_vector_snapshot(snapshot, Some((expected_dim, expected_metric)))
    }

    fn from_vector_snapshot(
        snapshot: HnswVectorSnapshot,
        expected: Option<(usize, DistanceMetric)>,
    ) -> Result<Self, String> {
        if snapshot.version != 1 {
            return Err(format!(
                "unsupported HNSW snapshot version {}",
                snapshot.version
            ));
        }
        if let Some((expected_dim, expected_metric)) = expected {
            if snapshot.dim != expected_dim {
                return Err(format!(
                    "HNSW snapshot dimension mismatch: expected {}, got {}",
                    expected_dim, snapshot.dim
                ));
            }
            if snapshot.config.metric != expected_metric {
                return Err(format!(
                    "HNSW snapshot metric mismatch: expected {:?}, got {:?}",
                    expected_metric, snapshot.config.metric
                ));
            }
        }

        let mut seen = AHashSet::new();
        let mut vectors = snapshot.vectors;
        vectors.sort_by_key(|(id, _)| *id);
        for (id, vector) in &vectors {
            if !seen.insert(*id) {
                return Err(format!("duplicate vector id {} in HNSW snapshot", id));
            }
            if vector.len() != snapshot.dim {
                return Err(format!(
                    "HNSW snapshot vector dimension mismatch for id {}: expected {}, got {}",
                    id,
                    snapshot.dim,
                    vector.len()
                ));
            }
            if !vector.iter().all(|x| x.is_finite()) {
                return Err(format!("non-finite vector in HNSW snapshot for id {}", id));
            }
        }

        let mut index = Self::new(snapshot.dim, snapshot.config);
        for (id, vector) in vectors {
            index.insert(id, vector);
        }
        Ok(index)
    }

    /// Batch insert multiple vectors using Rayon parallelism.
    ///
    /// Bulk insert with the same graph semantics as repeated [`Self::insert`].
    ///
    /// A prior fast path only wired level-0 neighbors from a stale snapshot and skipped
    /// multi-level greedy descent; that broke ANN recall on CREATE INDEX backfills.
    pub fn batch_insert(&mut self, vectors: Vec<(u32, Vec<f32>)>) {
        let n = vectors.len();
        if n > 0 {
            self.nodes.reserve(n);
            self.external_to_internal.reserve(n);
            self.internal_to_external.reserve(n);
            self.internal_generation.reserve(n);
            self.latest_generation.reserve(n);
        }
        for (id, vec) in vectors {
            self.insert(id, vec);
        }
    }

    /// Greedy closest neighbor at a specific level.
    fn greedy_closest(&self, start: u32, query: &[f32], level: usize) -> u32 {
        let mut curr = start;
        let mut curr_dist = self.distance(curr, query);

        loop {
            let mut changed = false;
            for &n in self.neighbors_at(curr, level) {
                let d = self.distance(n, query);
                if d < curr_dist {
                    curr = n;
                    curr_dist = d;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        curr
    }

    fn greedy_closest_with_stats(
        &self,
        start: u32,
        query: &[f32],
        level: usize,
        stats: &mut HnswSearchStats,
    ) -> u32 {
        let mut curr = start;
        let mut curr_dist = self.distance_with_stats(curr, query, stats);

        loop {
            let mut changed = false;
            for &n in self.neighbors_at(curr, level) {
                let d = self.distance_with_stats(n, query, stats);
                if d < curr_dist {
                    curr = n;
                    curr_dist = d;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        curr
    }

    /// Beam search at a specific level, returns top-ef candidates sorted by distance.
    fn search_layer(&self, entry: u32, query: &[f32], ef: usize, level: usize) -> Vec<Candidate> {
        let mut visited = AHashSet::with_capacity(ef * 2);
        let entry_dist = self.distance(entry, query);

        let mut candidates: BinaryHeap<MinCandidate> = BinaryHeap::new();
        let mut results: BinaryHeap<Candidate> = BinaryHeap::new(); // max-heap

        candidates.push(MinCandidate(Candidate {
            id: entry,
            distance: entry_dist,
        }));
        results.push(Candidate {
            id: entry,
            distance: entry_dist,
        });
        visited.insert(entry);

        while let Some(MinCandidate(current)) = candidates.pop() {
            let worst_result = results.peek().map_or(f32::MAX, |c| c.distance);
            if current.distance > worst_result && results.len() >= ef {
                break;
            }

            for &n in self.neighbors_at(current.id, level) {
                if visited.contains(&n) {
                    continue;
                }
                visited.insert(n);

                let d = self.distance(n, query);
                let worst = results.peek().map_or(f32::MAX, |c| c.distance);

                if d < worst || results.len() < ef {
                    candidates.push(MinCandidate(Candidate { id: n, distance: d }));
                    results.push(Candidate { id: n, distance: d });
                    if results.len() > ef {
                        results.pop(); // remove worst
                    }
                }
            }
        }

        let mut sorted: Vec<Candidate> = results.into_sorted_vec();
        sorted.sort_by(|a, b| cmp_distance_id(a.distance, a.id, b.distance, b.id));
        sorted
    }

    fn search_layer_with_stats(
        &self,
        entry: u32,
        query: &[f32],
        ef: usize,
        level: usize,
        stats: &mut HnswSearchStats,
    ) -> Vec<Candidate> {
        let mut visited = AHashSet::with_capacity(ef * 2);
        let entry_dist = self.distance_with_stats(entry, query, stats);

        let mut candidates: BinaryHeap<MinCandidate> = BinaryHeap::new();
        let mut results: BinaryHeap<Candidate> = BinaryHeap::new();

        candidates.push(MinCandidate(Candidate {
            id: entry,
            distance: entry_dist,
        }));
        stats.candidate_pushes += 1;
        stats.max_candidate_queue_size = stats.max_candidate_queue_size.max(candidates.len());
        results.push(Candidate {
            id: entry,
            distance: entry_dist,
        });
        visited.insert(entry);

        while let Some(MinCandidate(current)) = candidates.pop() {
            stats.candidate_pops += 1;
            let worst_result = results.peek().map_or(f32::MAX, |c| c.distance);
            if current.distance > worst_result && results.len() >= ef {
                break;
            }

            for &n in self.neighbors_at(current.id, level) {
                if visited.contains(&n) {
                    continue;
                }
                visited.insert(n);

                let d = self.distance_with_stats(n, query, stats);
                let worst = results.peek().map_or(f32::MAX, |c| c.distance);

                if d < worst || results.len() < ef {
                    candidates.push(MinCandidate(Candidate { id: n, distance: d }));
                    stats.candidate_pushes += 1;
                    stats.max_candidate_queue_size =
                        stats.max_candidate_queue_size.max(candidates.len());
                    results.push(Candidate { id: n, distance: d });
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        stats.visited_nodes = stats.visited_nodes.max(visited.len());
        stats.result_heap_size = results.len();
        let mut sorted: Vec<Candidate> = results.into_sorted_vec();
        sorted.sort_by(|a, b| cmp_distance_id(a.distance, a.id, b.distance, b.id));
        sorted
    }

    /// Heuristic neighbor selection (Malkov & Yashunin 2018).
    ///
    /// Instead of just keeping the M closest, this ensures diversity:
    /// a candidate is selected only if it's closer to the node than to any
    /// already-selected neighbor. This prevents clustering of connections
    /// and ensures the graph covers different "directions" in vector space.
    fn select_neighbors_heuristic(
        &self,
        _node_vec: &[f32],
        candidates: &[Candidate],
        max_n: usize,
    ) -> Vec<u32> {
        if candidates.len() <= max_n {
            return candidates.iter().map(|c| c.id).collect();
        }

        // Pre-fetch all candidate vectors to avoid repeated HashMap lookups
        let mut sorted: Vec<(u32, f32, &[f32])> = candidates
            .iter()
            .filter_map(|c| {
                self.nodes
                    .get(&c.id)
                    .map(|n| (c.id, c.distance, n.vector.as_slice()))
            })
            .collect();
        sorted.sort_by(|a, b| cmp_distance_id(a.1, a.0, b.1, b.0));

        let mut selected: Vec<(u32, f32, &[f32])> = Vec::with_capacity(max_n);

        for &(cid, cdist, cvec) in &sorted {
            if selected.len() >= max_n {
                break;
            }

            // Heuristic: only add this candidate if it's closer to the node
            // than to any already-selected neighbor (ensures diversity)
            let mut is_diverse = true;
            for &(_, _, sel_vec) in &selected {
                let dist_to_selected = compute_distance(cvec, sel_vec, &self.config.metric);
                if dist_to_selected < cdist {
                    is_diverse = false;
                    break;
                }
            }

            if is_diverse {
                selected.push((cid, cdist, cvec));
            }
        }

        // If heuristic was too aggressive, fill remaining slots with closest
        if selected.len() < max_n {
            for &(cid, cdist, cvec) in &sorted {
                if selected.len() >= max_n {
                    break;
                }
                if !selected.iter().any(|&(id, _, _)| id == cid) {
                    selected.push((cid, cdist, cvec));
                }
            }
        }

        selected.into_iter().map(|(id, _, _)| id).collect()
    }

    /// Shrink a node's neighbor list at a level — keep M closest neighbors.
    /// Uses simple distance-based truncation (fast) rather than heuristic selection
    /// since the graph quality impact of heuristic shrinking is marginal while
    /// the cost is O(M² × dim) per shrink call.
    fn shrink_neighbors(&mut self, node_id: u32, node_vec: &[f32], level: usize, max_n: usize) {
        let neighbor_ids: Vec<u32> = match self.nodes.get(&node_id) {
            Some(node) if level < node.neighbors.len() => node.neighbors[level].clone(),
            _ => return,
        };

        let mut candidates: Vec<(u32, f32)> = neighbor_ids
            .iter()
            .filter_map(|&nid| {
                self.nodes.get(&nid).map(|n| {
                    (
                        nid,
                        compute_distance(node_vec, &n.vector, &self.config.metric),
                    )
                })
            })
            .collect();
        candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        candidates.truncate(max_n);

        let selected: Vec<u32> = candidates.into_iter().map(|(id, _)| id).collect();

        if let Some(node) = self.nodes.get_mut(&node_id) {
            if level < node.neighbors.len() {
                node.neighbors[level] = selected;
            }
        }
    }

    fn neighbors_at(&self, id: u32, level: usize) -> &[u32] {
        self.nodes
            .get(&id)
            .and_then(|n| n.neighbors.get(level))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn distance(&self, id: u32, query: &[f32]) -> f32 {
        self.nodes
            .get(&id)
            .map(|n| compute_distance(&n.vector, query, &self.config.metric))
            .unwrap_or(f32::MAX)
    }

    fn distance_with_stats(&self, id: u32, query: &[f32], stats: &mut HnswSearchStats) -> f32 {
        stats.distance_evaluations += 1;
        stats.metric_dispatch_count += 1;
        self.distance(id, query)
    }

    fn live_external_for_internal(&self, internal_id: u32) -> Option<u32> {
        if self.tombstoned_ids.contains(&internal_id) {
            return None;
        }
        let external_id = *self.internal_to_external.get(&internal_id)?;
        if self.external_to_internal.get(&external_id).copied() != Some(internal_id) {
            return None;
        }
        let internal_generation = self.internal_generation.get(&internal_id).copied();
        let latest_generation = self.latest_generation.get(&external_id).copied();
        if internal_generation != latest_generation {
            return None;
        }
        Some(external_id)
    }

    /// Search for top-k nearest neighbors with an explicit ef_search bound.
    pub fn search_with_ef(&self, query: &[f32], top_k: usize, ef_search: usize) -> Vec<(u32, f32)> {
        assert_eq!(query.len(), self.dim, "Vector dimension mismatch");
        assert!(
            query.iter().all(|x| x.is_finite()),
            "Vector must contain only finite values"
        );
        if self.nodes.is_empty() || self.entry_point.is_none() || self.live_count() == 0 {
            return Vec::new();
        }
        if top_k == 0 {
            return Vec::new();
        }

        let entry = self.entry_point.unwrap();
        let mut curr = entry;

        for lev in (1..=self.max_level).rev() {
            curr = self.greedy_closest(curr, query, lev);
        }

        let search_ef = ef_search
            .max(top_k)
            .saturating_add(self.tombstone_count().min(16))
            .min(self.nodes.len().max(top_k));
        let candidates = self.search_layer(curr, query, search_ef, 0);

        if self.tombstone_count() == 0 {
            let mut results: Vec<(u32, f32)> = candidates
                .into_iter()
                .filter_map(|candidate| {
                    self.internal_to_external
                        .get(&candidate.id)
                        .copied()
                        .map(|external_id| (external_id, candidate.distance))
                })
                .collect();
            if results.len() > 1 {
                results.sort_by(|a, b| cmp_distance_id(a.1, a.0, b.1, b.0));
            }
            results.truncate(top_k);
            return results;
        }

        let mut seen_external = AHashSet::new();
        let mut results = Vec::new();
        for candidate in candidates {
            let Some(external_id) = self.live_external_for_internal(candidate.id) else {
                continue;
            };
            if !seen_external.insert(external_id) {
                continue;
            }
            results.push((external_id, candidate.distance));
        }
        results.sort_by(|a, b| cmp_distance_id(a.1, a.0, b.1, b.0));
        results.truncate(top_k);
        results
    }

    /// Search for top-k nearest neighbors.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(u32, f32)> {
        let ef = self
            .config
            .ef_search
            .max(top_k)
            .saturating_add(self.tombstone_count())
            .min(self.nodes.len().max(top_k));
        self.search_with_ef(query, top_k, ef)
    }

    pub fn search_with_stats(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> (Vec<(u32, f32)>, HnswSearchStats) {
        assert_eq!(query.len(), self.dim, "Vector dimension mismatch");
        assert!(
            query.iter().all(|x| x.is_finite()),
            "Vector must contain only finite values"
        );
        let mut stats = HnswSearchStats {
            ef_search: self
                .config
                .ef_search
                .max(top_k)
                .saturating_add(self.tombstone_count())
                .min(self.nodes.len().max(top_k)),
            m: self.config.m,
            average_degree: self.average_degree(),
            tombstone_ratio: self.tombstone_ratio(),
            tombstone_fast_path_used: self.tombstone_count() == 0,
            no_tombstone_fast_path_used: self.tombstone_count() == 0,
            norm_cache_used: false,
            generation_model_enabled: true,
            internal_node_count: self.nodes.len(),
            live_external_id_count: self.external_to_internal.len(),
            ..HnswSearchStats::default()
        };
        if self.nodes.is_empty()
            || self.entry_point.is_none()
            || top_k == 0
            || self.live_count() == 0
        {
            return (Vec::new(), stats);
        }

        let entry = self.entry_point.unwrap();
        let mut curr = entry;
        for lev in (1..=self.max_level).rev() {
            curr = self.greedy_closest_with_stats(curr, query, lev, &mut stats);
        }

        let candidates = self.search_layer_with_stats(curr, query, stats.ef_search, 0, &mut stats);
        let mut results = Vec::new();
        let mut seen_external = AHashSet::new();
        for candidate in candidates {
            stats.current_generation_checks += 1;
            if self.tombstone_count() > 0 {
                stats.tombstone_checks += 1;
            }
            if self.tombstoned_ids.contains(&candidate.id) {
                stats.tombstone_hits += 1;
                stats.filtered_candidates_count += 1;
                continue;
            }
            let Some(external_id) = self.internal_to_external.get(&candidate.id).copied() else {
                stats.stale_generation_filtered_count += 1;
                stats.filtered_candidates_count += 1;
                continue;
            };
            if self.external_to_internal.get(&external_id).copied() != Some(candidate.id)
                || self.internal_generation.get(&candidate.id).copied()
                    != self.latest_generation.get(&external_id).copied()
            {
                stats.stale_generation_filtered_count += 1;
                stats.filtered_candidates_count += 1;
                continue;
            }
            stats.live_external_candidates += 1;
            if !seen_external.insert(external_id) {
                stats.duplicate_external_filtered_count += 1;
                stats.filtered_candidates_count += 1;
                continue;
            }
            results.push((external_id, candidate.distance));
        }
        results.sort_by(|a, b| cmp_distance_id(a.1, a.0, b.1, b.0));
        results.truncate(top_k);
        stats.live_candidates_returned = results.len();
        (results, stats)
    }

    /// Number of vectors in the index.
    pub fn len(&self) -> usize {
        self.live_count()
    }

    /// Export live (external_id, vector) pairs for checkpointing.
    pub fn live_vectors_snapshot(&self) -> Vec<(u32, Vec<f32>)> {
        self.live_vectors()
    }

    pub fn is_empty(&self) -> bool {
        self.live_count() == 0
    }

    /// Dimensionality of indexed vectors.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Current max level in the graph.
    pub fn levels(&self) -> usize {
        self.max_level + 1
    }
}

fn cmp_distance_id(a_dist: f32, a_id: u32, b_dist: f32, b_id: u32) -> Ordering {
    a_dist
        .partial_cmp(&b_dist)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a_id.cmp(&b_id))
}

// ── Product Quantization ────────────────────────────────────────────────

/// Product Quantization — subspace vector compression.
///
/// Splits D-dimensional vector into `n_sub` subspaces of `sub_dim` dimensions each.
/// Each subspace is quantized to one of `k_centroids` centroids (typically 256).
/// Storage: D×f32 → n_sub × u8 (~100× compression for D=384, n_sub=48).
pub struct ProductQuantizer {
    /// Number of subspaces
    n_sub: usize,
    /// Dimensions per subspace
    sub_dim: usize,
    /// Total dimensions
    dim: usize,
    /// Number of centroids per subspace (max 256 for u8 codes)
    k_centroids: usize,
    /// Codebooks: n_sub × k_centroids × sub_dim
    codebooks: Vec<Vec<Vec<f32>>>,
    /// Whether codebooks have been trained
    trained: bool,
}

impl ProductQuantizer {
    /// Create a new PQ with `n_sub` subspaces.
    /// `dim` must be divisible by `n_sub`.
    pub fn new(dim: usize, n_sub: usize) -> Self {
        assert!(dim % n_sub == 0, "dim must be divisible by n_sub");
        Self {
            n_sub,
            sub_dim: dim / n_sub,
            dim,
            k_centroids: 256,
            codebooks: Vec::new(),
            trained: false,
        }
    }

    /// Train codebooks using simplified k-means on training vectors.
    pub fn train(&mut self, vectors: &[Vec<f32>], max_iters: usize) {
        let n = vectors.len();
        if n == 0 {
            return;
        }

        let mut rng = rand::thread_rng();
        let k = self.k_centroids.min(n);
        self.codebooks = Vec::with_capacity(self.n_sub);

        for sub in 0..self.n_sub {
            let offset = sub * self.sub_dim;
            // Extract sub-vectors
            let sub_vecs: Vec<&[f32]> = vectors
                .iter()
                .map(|v| &v[offset..offset + self.sub_dim])
                .collect();

            // Initialize centroids randomly
            let mut centroids: Vec<Vec<f32>> = (0..k)
                .map(|_| sub_vecs[rng.gen_range(0..n)].to_vec())
                .collect();

            // K-means iterations
            for _ in 0..max_iters {
                // Assign
                let mut assignments: Vec<Vec<usize>> = vec![Vec::new(); k];
                for (i, sv) in sub_vecs.iter().enumerate() {
                    let mut best_c = 0;
                    let mut best_d = f32::MAX;
                    for (c, centroid) in centroids.iter().enumerate() {
                        let d = l2_distance(sv, centroid);
                        if d < best_d {
                            best_d = d;
                            best_c = c;
                        }
                    }
                    assignments[best_c].push(i);
                }

                // Update centroids
                for (c, indices) in assignments.iter().enumerate() {
                    if indices.is_empty() {
                        continue;
                    }
                    let mut new_centroid = vec![0.0f32; self.sub_dim];
                    for &i in indices {
                        for (j, &v) in sub_vecs[i].iter().enumerate() {
                            new_centroid[j] += v;
                        }
                    }
                    let n_assigned = indices.len() as f32;
                    for v in &mut new_centroid {
                        *v /= n_assigned;
                    }
                    centroids[c] = new_centroid;
                }
            }

            self.codebooks.push(centroids);
        }

        self.trained = true;
    }

    /// Encode a vector to PQ codes.
    pub fn encode(&self, vector: &[f32]) -> Vec<u8> {
        assert!(self.trained, "PQ must be trained before encoding");
        assert_eq!(vector.len(), self.dim);

        let mut codes = Vec::with_capacity(self.n_sub);
        for sub in 0..self.n_sub {
            let offset = sub * self.sub_dim;
            let sub_vec = &vector[offset..offset + self.sub_dim];

            let mut best_c: u8 = 0;
            let mut best_d = f32::MAX;
            for (c, centroid) in self.codebooks[sub].iter().enumerate() {
                let d = l2_distance(sub_vec, centroid);
                if d < best_d {
                    best_d = d;
                    best_c = c as u8;
                }
            }
            codes.push(best_c);
        }
        codes
    }

    /// Asymmetric Distance Computation (ADC): query (raw) vs code (PQ).
    /// Pre-computes lookup table for O(n_sub) per comparison.
    pub fn build_distance_table(&self, query: &[f32]) -> Vec<Vec<f32>> {
        assert!(self.trained);
        let mut table = Vec::with_capacity(self.n_sub);
        for sub in 0..self.n_sub {
            let offset = sub * self.sub_dim;
            let sub_query = &query[offset..offset + self.sub_dim];
            let dists: Vec<f32> = self.codebooks[sub]
                .iter()
                .map(|centroid| l2_distance(sub_query, centroid))
                .collect();
            table.push(dists);
        }
        table
    }

    /// Compute approximate distance using ADC lookup table.
    pub fn adc_distance(table: &[Vec<f32>], codes: &[u8]) -> f32 {
        table
            .iter()
            .zip(codes.iter())
            .map(|(dists, &code)| dists[code as usize])
            .sum()
    }

    /// Compression ratio.
    pub fn compression_ratio(&self) -> f32 {
        (self.dim as f32 * 4.0) / self.n_sub as f32
    }

    pub fn is_trained(&self) -> bool {
        self.trained
    }
}

// ── Two-Stage Index (HNSW + PQ) ─────────────────────────────────────────

/// Combined HNSW + PQ index for high-recall, low-memory ANN search.
///
/// Strategy:
///   1. PQ for fast candidate retrieval (ADC distance)
///   2. Exact re-ranking of top candidates
pub struct HnswPqIndex {
    hnsw: HnswIndex,
    pq: ProductQuantizer,
    /// PQ codes per vector ID
    pq_codes: HashMap<u32, Vec<u8>>,
    /// Raw vectors for exact re-ranking
    raw_vectors: HashMap<u32, Vec<f32>>,
    /// Whether to store raw vectors for re-ranking
    store_raw: bool,
}

impl HnswPqIndex {
    pub fn new(dim: usize, n_sub: usize, store_raw: bool) -> Self {
        Self {
            hnsw: HnswIndex::with_default_config(dim),
            pq: ProductQuantizer::new(dim, n_sub),
            pq_codes: HashMap::new(),
            raw_vectors: HashMap::new(),
            store_raw,
        }
    }

    pub fn with_config(dim: usize, n_sub: usize, hnsw_config: HnswConfig, store_raw: bool) -> Self {
        Self {
            hnsw: HnswIndex::new(dim, hnsw_config),
            pq: ProductQuantizer::new(dim, n_sub),
            pq_codes: HashMap::new(),
            raw_vectors: HashMap::new(),
            store_raw,
        }
    }

    /// Train PQ codebooks on a set of training vectors.
    pub fn train_pq(&mut self, training_vectors: &[Vec<f32>], max_iters: usize) {
        self.pq.train(training_vectors, max_iters);
    }

    /// Insert a vector. PQ must be trained first if PQ codes are desired.
    pub fn insert(&mut self, id: u32, vector: Vec<f32>) {
        if self.pq.is_trained() {
            let codes = self.pq.encode(&vector);
            self.pq_codes.insert(id, codes);
        }
        if self.store_raw {
            self.raw_vectors.insert(id, vector.clone());
        }
        self.hnsw.insert(id, vector);
    }

    /// Two-stage search: HNSW candidate retrieval → exact re-ranking.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(u32, f32)> {
        // Stage 1: HNSW retrieval (request more candidates for re-ranking)
        let ef = (top_k * 4).max(self.hnsw.config.ef_search);
        let candidates = self.hnsw.search(query, ef);

        if !self.store_raw || candidates.len() <= top_k {
            return candidates.into_iter().take(top_k).collect();
        }

        // Stage 2: Exact re-ranking with raw vectors
        let mut reranked: Vec<(u32, f32)> = candidates
            .into_iter()
            .filter_map(|(id, _)| {
                self.raw_vectors
                    .get(&id)
                    .map(|v| (id, compute_distance(query, v, &self.hnsw.config.metric)))
            })
            .collect();

        reranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        reranked.truncate(top_k);
        reranked
    }

    /// PQ-only approximate search (for large-scale, memory-constrained scenarios).
    pub fn pq_search(&self, query: &[f32], top_k: usize) -> Vec<(u32, f32)> {
        if !self.pq.is_trained() {
            return self.hnsw.search(query, top_k);
        }

        let table = self.pq.build_distance_table(query);
        let mut scored: Vec<(u32, f32)> = self
            .pq_codes
            .iter()
            .map(|(&id, codes)| (id, ProductQuantizer::adc_distance(&table, codes)))
            .collect();

        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        scored.truncate(top_k);
        scored
    }

    pub fn len(&self) -> usize {
        self.hnsw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hnsw.is_empty()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vector(dim: usize) -> Vec<f32> {
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen::<f32>()).collect()
    }

    fn exact_top_k(
        vectors: &[(u32, Vec<f32>)],
        query: &[f32],
        metric: &DistanceMetric,
        top_k: usize,
    ) -> Vec<(u32, f32)> {
        let mut scored: Vec<(u32, f32)> = vectors
            .iter()
            .map(|(id, vector)| (*id, compute_distance(vector, query, metric)))
            .collect();
        scored.sort_by(|a, b| cmp_distance_id(a.1, a.0, b.1, b.0));
        scored.truncate(top_k);
        scored
    }

    #[test]
    fn test_hnsw_basic() {
        let dim = 8;
        let mut idx = HnswIndex::with_default_config(dim);

        for i in 0..100 {
            idx.insert(i, random_vector(dim));
        }

        assert_eq!(idx.len(), 100);
        let results = idx.search(&random_vector(dim), 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_hnsw_recall() {
        let dim = 4;
        let mut idx = HnswIndex::with_default_config(dim);
        let target = vec![1.0, 0.0, 0.0, 0.0];

        // Insert target and random vectors
        idx.insert(0, target.clone());
        for i in 1..50 {
            idx.insert(i, random_vector(dim));
        }

        let results = idx.search(&target, 1);
        // The exact vector should be returned as nearest
        assert_eq!(results[0].0, 0);
        assert!(results[0].1 < 0.001);
    }

    #[test]
    fn test_hnsw_cosine() {
        let dim = 8;
        let config = HnswConfig {
            metric: DistanceMetric::Cosine,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(dim, config);

        for i in 0..30 {
            idx.insert(i, random_vector(dim));
        }

        let results = idx.search(&random_vector(dim), 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_pq_encode_decode() {
        let dim = 16;
        let n_sub = 4;
        let mut pq = ProductQuantizer::new(dim, n_sub);

        // Train on random data
        let training: Vec<Vec<f32>> = (0..100).map(|_| random_vector(dim)).collect();
        pq.train(&training, 10);
        assert!(pq.is_trained());

        // Encode
        let v = random_vector(dim);
        let codes = pq.encode(&v);
        assert_eq!(codes.len(), n_sub);

        // ADC distance
        let table = pq.build_distance_table(&v);
        let d = ProductQuantizer::adc_distance(&table, &codes);
        // Self-distance should be very small (quantization error only)
        assert!(d < 1.0, "Self-distance too large: {}", d);
    }

    #[test]
    fn test_hnsw_pq_combined() {
        let dim = 16;
        let n_sub = 4;
        let mut idx = HnswPqIndex::new(dim, n_sub, true);

        // Train PQ
        let training: Vec<Vec<f32>> = (0..100).map(|_| random_vector(dim)).collect();
        idx.train_pq(&training, 10);

        // Insert
        let target = random_vector(dim);
        idx.insert(0, target.clone());
        for i in 1..50 {
            idx.insert(i, random_vector(dim));
        }

        // Two-stage search
        let results = idx.search(&target, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 0); // exact match should be first
    }

    #[test]
    fn test_pq_compression_ratio() {
        let pq = ProductQuantizer::new(384, 48);
        // 384 floats (1536 bytes) → 48 bytes = 32× compression
        assert!(pq.compression_ratio() > 30.0);
    }

    #[test]
    fn test_empty_index() {
        let idx = HnswIndex::with_default_config(8);
        assert!(idx.is_empty());
        let results = idx.search(&random_vector(8), 5);
        assert!(results.is_empty());
    }

    #[test]
    fn vector_distance_formula_golden_values_match_reference() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![4.0_f32, 6.0, 3.0];

        assert!((compute_distance(&a, &b, &DistanceMetric::L2) - 25.0).abs() < 1e-6);
        assert!((compute_distance(&a, &b, &DistanceMetric::InnerProduct) + 25.0).abs() < 1e-6);

        let dot = 25.0_f32;
        let norm_a = 14.0_f32.sqrt();
        let norm_b = 61.0_f32.sqrt();
        let expected_cosine_distance = 1.0 - dot / (norm_a * norm_b);
        assert!(
            (compute_distance(&a, &b, &DistanceMetric::Cosine) - expected_cosine_distance).abs()
                < 1e-6
        );
        assert_eq!(
            compute_distance(&[0.0, 0.0], &[1.0, 0.0], &DistanceMetric::Cosine),
            1.0
        );
    }

    #[test]
    fn hnsw_top_k_edges_and_deterministic_ties() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 32,
            ef_construction: 64,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(2, config);
        idx.insert(2, vec![1.0, 0.0]);
        idx.insert(1, vec![-1.0, 0.0]);

        assert!(idx.search(&[0.0, 0.0], 0).is_empty());
        assert_eq!(idx.search(&[0.0, 0.0], 1)[0].0, 1);
        assert_eq!(
            idx.search(&[0.0, 0.0], 10)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn hnsw_duplicate_id_replaces_vector_without_duplicate_live_entry() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 32,
            ef_construction: 64,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(2, config);
        idx.insert(1, vec![100.0, 100.0]);
        idx.insert(2, vec![5.0, 0.0]);
        idx.insert(1, vec![0.0, 0.0]);

        assert_eq!(idx.len(), 2);
        assert_eq!(idx.search(&[0.0, 0.0], 2)[0].0, 1);
        assert_eq!(idx.search(&[100.0, 100.0], 2)[0].0, 2);
    }

    #[test]
    fn hnsw_remove_filters_deleted_ids_and_reinsert_is_single_live_entry() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 32,
            ef_construction: 64,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(2, config);
        idx.insert(1, vec![0.0, 0.0]);
        idx.insert(2, vec![10.0, 0.0]);

        assert!(idx.remove(1));
        assert!(!idx.remove(1));
        assert_eq!(idx.len(), 1);
        assert_eq!(
            idx.search(&[0.0, 0.0], 10)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec![2]
        );

        idx.insert(1, vec![0.1, 0.0]);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.search(&[0.0, 0.0], 10)[0].0, 1);
    }

    #[test]
    fn hnsw_batch_replace_remove_rebuild_once_semantics_stay_correct() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 64,
            ef_construction: 64,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(2, config);
        for id in 0..10 {
            idx.insert(id, vec![id as f32, 0.0]);
        }

        idx.batch_replace(vec![
            (1, vec![100.0, 0.0]),
            (2, vec![0.02, 0.0]),
            (2, vec![0.01, 0.0]),
            (20, vec![0.03, 0.0]),
        ]);
        assert_eq!(idx.len(), 11);
        assert_eq!(idx.graph_node_count(), 11);
        let ids = idx
            .search(&[0.0, 0.0], 5)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(ids[0], 0);
        assert_eq!(ids[1], 2);
        assert_eq!(ids[2], 20);
        assert_eq!(
            ids.iter().copied().collect::<AHashSet<_>>().len(),
            ids.len()
        );

        let removed = idx.batch_remove(&[0, 1, 1, 99]);
        assert_eq!(removed, 2);
        assert_eq!(idx.len(), 9);
        let ids = idx
            .search(&[0.0, 0.0], 20)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert!(!ids.contains(&0));
        assert!(!ids.contains(&1));
        assert_eq!(
            ids.iter().copied().collect::<AHashSet<_>>().len(),
            ids.len()
        );
    }

    #[test]
    fn hnsw_lazy_remove_tombstone_filters_deleted_id() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 32,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        idx.insert(1, vec![0.0, 0.0]);
        idx.insert(2, vec![10.0, 0.0]);

        assert!(idx.remove(1));
        assert!(!idx.remove(1));
        assert_eq!(idx.tombstone_count(), 1);
        assert_eq!(idx.live_count(), 1);
        assert_eq!(idx.graph_node_count(), 2);
        let (rows, stats) = idx.search_with_stats(&[0.0, 0.0], 10);
        assert!(!rows.iter().any(|(id, _)| *id == 1));
        assert!(stats.tombstone_checks > 0);
        assert!(stats.tombstone_hits > 0);
        assert!(!stats.no_tombstone_fast_path_used);
    }

    #[test]
    fn hnsw_lazy_replace_keeps_single_live_id_and_new_vector_visible() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        idx.insert(1, vec![0.0, 0.0]);
        idx.insert(2, vec![1.0, 0.0]);
        let old_internal = idx.current_internal_node_id(1).unwrap();
        idx.replace(1, vec![50.0, 0.0]);
        let new_internal = idx.current_internal_node_id(1).unwrap();

        assert_ne!(old_internal, new_internal);
        assert_eq!(idx.tombstone_count(), 1);
        assert_eq!(idx.live_count(), 2);
        assert_eq!(idx.graph_node_count(), 3);
        assert_eq!(idx.search(&[50.0, 0.0], 1)[0].0, 1);
        assert_ne!(idx.search(&[0.0, 0.0], 1)[0].0, 1);
        let (_, stats) = idx.search_with_stats(&[0.0, 0.0], 10);
        assert!(stats.generation_model_enabled);
        assert!(stats.current_generation_checks > 0);
    }

    #[test]
    fn hnsw_repeated_lazy_replace_returns_only_latest_generation() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 128,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        idx.insert(1, vec![0.0, 0.0]);
        let first_internal = idx.current_internal_node_id(1).unwrap();
        for generation in 1..=20 {
            idx.replace(1, vec![generation as f32, 0.0]);
        }
        let latest_internal = idx.current_internal_node_id(1).unwrap();
        assert_ne!(first_internal, latest_internal);
        assert_eq!(idx.live_count(), 1);
        assert_eq!(idx.tombstone_count(), 20);
        let results = idx.search(&[20.0, 0.0], 50);
        assert_eq!(results.iter().filter(|(id, _)| *id == 1).count(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn hnsw_lazy_remove_after_replace_removes_live_mapping() {
        let mut idx = HnswIndex::new(2, HnswConfig::default());
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        idx.insert(1, vec![0.0, 0.0]);
        idx.replace(1, vec![5.0, 0.0]);
        assert!(idx.remove(1));
        assert_eq!(idx.live_count(), 0);
        assert_eq!(idx.current_internal_node_id(1), None);
        assert!(idx.search(&[5.0, 0.0], 10).is_empty());
    }

    #[test]
    fn hnsw_lazy_replace_after_remove_inserts_new_generation() {
        let mut idx = HnswIndex::new(2, HnswConfig::default());
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        idx.insert(1, vec![0.0, 0.0]);
        let first_internal = idx.current_internal_node_id(1).unwrap();
        assert!(idx.remove(1));
        idx.replace(1, vec![7.0, 0.0]);
        let new_internal = idx.current_internal_node_id(1).unwrap();
        assert_ne!(first_internal, new_internal);
        assert_eq!(idx.live_count(), 1);
        assert_eq!(idx.search(&[7.0, 0.0], 1)[0].0, 1);
    }

    #[test]
    fn hnsw_lazy_compact_clears_tombstones_and_preserves_absence() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        for id in 0..20 {
            idx.insert(id, vec![id as f32, 0.0]);
        }
        assert_eq!(idx.batch_remove(&[0, 1, 2, 3]), 4);
        assert_eq!(idx.tombstone_count(), 4);
        idx.compact();
        assert_eq!(idx.tombstone_count(), 0);
        assert_eq!(idx.graph_node_count(), idx.live_count());
        let ids = idx
            .search(&[0.0, 0.0], 20)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for removed in [0, 1, 2, 3] {
            assert!(!ids.contains(&removed));
        }
    }

    #[test]
    fn hnsw_auto_compact_threshold_triggers_when_crossed() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::AutoCompact { threshold: 0.25 });
        for id in 0..20 {
            idx.insert(id, vec![id as f32, 0.0]);
        }
        assert_eq!(idx.batch_remove(&[0, 1, 2, 3]), 4);
        assert_eq!(idx.tombstone_count(), 4);
        assert_eq!(idx.batch_remove(&[4]), 1);
        assert_eq!(idx.tombstone_count(), 0);
        assert_eq!(idx.graph_node_count(), idx.live_count());
        for removed in 0..5 {
            assert!(!idx
                .search(&[removed as f32, 0.0], 20)
                .iter()
                .any(|(id, _)| *id == removed));
        }
    }

    #[test]
    fn hnsw_lazy_batch_replace_remove_and_reload_materialize_live_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lazy_hnsw_vectors.json");
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        for id in 0..120 {
            idx.insert(id, vec![id as f32, 0.0]);
        }
        assert_eq!(idx.batch_remove(&(0..20).collect::<Vec<_>>()), 20);
        idx.batch_replace(
            (20..40)
                .map(|id| (id, vec![1000.0 + id as f32, 0.0]))
                .collect(),
        );
        assert_eq!(idx.tombstone_count(), 40);
        assert_eq!(idx.live_count(), 100);
        assert_eq!(idx.graph_node_count(), 140);
        for removed in 0..20 {
            assert!(!idx
                .search(&[removed as f32, 0.0], 120)
                .iter()
                .any(|(id, _)| *id == removed));
        }
        assert_eq!(idx.search(&[1039.0, 0.0], 1)[0].0, 39);

        idx.save_vectors(&path).unwrap();
        let loaded = HnswIndex::load_vectors_with_expected(&path, 2, DistanceMetric::L2).unwrap();
        assert_eq!(loaded.tombstone_count(), 0);
        assert_eq!(loaded.live_count(), 100);
        for removed in 0..20 {
            assert!(!loaded
                .search(&[removed as f32, 0.0], 120)
                .iter()
                .any(|(id, _)| *id == removed));
        }
        assert!(loaded
            .search(&[1039.0, 0.0], 120)
            .iter()
            .any(|(id, _)| *id == 39));
    }

    #[test]
    fn hnsw_search_stats_report_no_tombstone_fast_path() {
        let mut idx = HnswIndex::new(2, HnswConfig::default());
        idx.insert(1, vec![0.0, 0.0]);
        idx.insert(2, vec![1.0, 0.0]);
        let (_, stats) = idx.search_with_stats(&[0.0, 0.0], 1);
        assert!(stats.no_tombstone_fast_path_used);
        assert_eq!(stats.tombstone_checks, 0);
        assert_eq!(stats.tombstone_hits, 0);
    }

    #[test]
    fn hnsw_generation_stats_filter_stale_and_duplicate_external_candidates() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 128,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        idx.insert(1, vec![0.0, 0.0]);
        idx.insert(2, vec![1.0, 0.0]);
        let old_internal = idx.current_internal_node_id(1).unwrap();
        idx.replace(1, vec![0.05, 0.0]);
        let latest_internal = idx.current_internal_node_id(1).unwrap();
        assert_ne!(old_internal, latest_internal);

        // Force an internally stale but non-tombstoned old generation so the
        // stale-generation filter is directly exercised by search stats.
        idx.tombstoned_ids.remove(&old_internal);
        idx.entry_point = Some(old_internal);

        let (rows, stats) = idx.search_with_stats(&[0.0, 0.0], 10);
        let ids = rows.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        assert_eq!(
            ids.iter().copied().collect::<AHashSet<_>>().len(),
            ids.len()
        );
        assert_eq!(ids.iter().filter(|id| **id == 1).count(), 1);
        assert!(stats.generation_model_enabled);
        assert!(stats.current_generation_checks > 0);
        assert!(stats.stale_generation_filtered_count > 0);
        assert_eq!(stats.duplicate_external_filtered_count, 0);
    }

    #[test]
    fn hnsw_auto_compact_trigger_count_tracks_threshold_compaction() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::AutoCompact { threshold: 0.10 });
        for id in 0..20 {
            idx.insert(id, vec![id as f32, 0.0]);
        }
        idx.batch_replace(
            (0..3)
                .map(|id| (id, vec![100.0 + id as f32, 0.0]))
                .collect(),
        );
        assert_eq!(idx.compact_trigger_count(), 1);
        assert_eq!(idx.tombstone_count(), 0);
        assert_eq!(idx.graph_node_count(), idx.live_count());
    }

    #[test]
    fn hnsw_compact_records_profile_and_preserves_live_view() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        for id in 0..40 {
            idx.insert(id, vec![id as f32, 0.0]);
        }
        assert_eq!(idx.batch_remove(&(0..10).collect::<Vec<_>>()), 10);
        let old_internal_node_count = idx.internal_node_count();
        idx.compact();

        let profile = idx.last_compaction_profile().unwrap();
        assert_eq!(profile.tombstones_removed, 10);
        assert_eq!(profile.live_vectors_collected, 30);
        assert_eq!(profile.old_internal_node_count, old_internal_node_count);
        assert_eq!(profile.new_internal_node_count, idx.live_count());
        assert!(profile.memory_after_bytes < profile.memory_before_bytes);
        assert_eq!(idx.tombstone_count(), 0);
        for removed in 0..10 {
            assert!(!idx
                .search(&[removed as f32, 0.0], 40)
                .iter()
                .any(|(id, _)| *id == removed));
        }
    }

    #[test]
    fn hnsw_adaptive_compact_triggers_on_internal_growth_and_records_reason() {
        let mut idx = HnswIndex::new(
            2,
            HnswConfig {
                metric: DistanceMetric::L2,
                ef_search: 64,
                ef_construction: 64,
                ..HnswConfig::default()
            },
        );
        idx.set_mutation_policy(HnswMutationPolicy::AdaptiveCompact {
            min_threshold: 0.10,
            max_threshold: 0.80,
            search_latency_multiplier: 2.0,
            max_tombstone_ratio: 0.80,
            max_internal_growth_ratio: 1.20,
        });
        for id in 0..50 {
            idx.insert(id, vec![id as f32, 0.0]);
        }
        idx.batch_replace(
            (0..10)
                .map(|id| (id, vec![100.0 + id as f32, 0.0]))
                .collect(),
        );

        assert_eq!(idx.adaptive_trigger_count(), 1);
        assert_eq!(
            idx.last_adaptive_trigger_reason(),
            Some("max_internal_growth_ratio")
        );
        assert_eq!(idx.tombstone_count(), 0);
        assert_eq!(idx.graph_node_count(), idx.live_count());
        let ids = idx
            .search(&[109.0, 0.0], 50)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&9));
        assert_eq!(
            ids.iter().copied().collect::<AHashSet<_>>().len(),
            ids.len()
        );
    }

    #[test]
    fn hnsw_long_run_lazy_snapshot_reload_materializes_live_generations_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw_long_run_vectors.json");
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 128,
            ef_construction: 128,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(4, config.clone());
        idx.set_mutation_policy(HnswMutationPolicy::LazyTombstone);
        let mut live = HashMap::new();
        for id in 0..100u32 {
            let vector = (0..4)
                .map(|dim| ((id + dim) as f32 * 0.17).sin())
                .collect::<Vec<_>>();
            idx.insert(id, vector.clone());
            live.insert(id, vector);
        }
        let mut deleted = AHashSet::new();
        for op in 0..1000u32 {
            let id = op % 130;
            match op % 10 {
                0 => {
                    let vector = (0..4)
                        .map(|dim| ((op + dim) as f32 * 0.11).cos())
                        .collect::<Vec<_>>();
                    idx.insert(id, vector.clone());
                    live.insert(id, vector);
                    deleted.remove(&id);
                }
                1 | 2 => {
                    idx.remove(id);
                    live.remove(&id);
                    deleted.insert(id);
                }
                _ => {
                    let vector = (0..4)
                        .map(|dim| ((op + id + dim) as f32 * 0.07).sin())
                        .collect::<Vec<_>>();
                    idx.replace(id, vector.clone());
                    live.insert(id, vector);
                    deleted.remove(&id);
                }
            }
        }
        assert_eq!(idx.live_count(), live.len());
        assert!(idx.tombstone_count() > 0);
        let rows = idx.search(&[0.0, 0.0, 0.0, 0.0], 1000);
        assert_eq!(
            rows.iter()
                .map(|(id, _)| *id)
                .collect::<AHashSet<_>>()
                .len(),
            rows.len()
        );
        assert!(!rows.iter().any(|(id, _)| deleted.contains(id)));

        idx.save_vectors(&path).unwrap();
        let loaded = HnswIndex::load_vectors_with_expected(&path, 4, DistanceMetric::L2).unwrap();
        assert_eq!(loaded.live_count(), live.len());
        assert_eq!(loaded.tombstone_count(), 0);
        assert_eq!(loaded.graph_node_count(), loaded.live_count());
        let rows = loaded.search(&[0.0, 0.0, 0.0, 0.0], 1000);
        assert_eq!(
            rows.iter()
                .map(|(id, _)| *id)
                .collect::<AHashSet<_>>()
                .len(),
            rows.len()
        );
        assert!(!rows.iter().any(|(id, _)| deleted.contains(id)));
    }

    #[test]
    fn batch_build_is_deterministic_for_sorted_inserts() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            m: 16,
            m0: 32,
            ef_search: 80,
            ef_construction: 120,
        };
        let vectors: Vec<(u32, Vec<f32>)> = (0..200u32)
            .map(|id| {
                (
                    id,
                    (0..8)
                        .map(|dim| ((id as f32 + 1.0) * (dim as f32 + 0.25)).sin())
                        .collect(),
                )
            })
            .collect();
        let query: Vec<f32> = (0..8)
            .map(|dim| ((dim as f32 + 0.5) * 0.31).cos())
            .collect();

        let mut a = HnswIndex::new(8, config.clone());
        a.batch_insert(vectors.clone());
        let mut b = HnswIndex::new(8, config);
        b.batch_insert(vectors);
        let ann_a = a.search(&query, 10);
        let ann_b = b.search(&query, 10);
        assert_eq!(
            ann_a.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ann_b.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "sorted batch_insert must produce identical graphs"
        );
    }

    #[test]
    fn batch_insert_matches_sequential_insert_recall() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            m: 12,
            m0: 24,
            ef_search: 80,
            ef_construction: 120,
        };
        let mut vectors = Vec::new();
        for id in 0..120 {
            let vector = (0..8)
                .map(|dim| ((id as f32 + 1.0) * (dim as f32 + 0.5)).sin())
                .collect::<Vec<_>>();
            vectors.push((id, vector));
        }
        let query = (0..8)
            .map(|dim| ((dim as f32 + 3.25) * 0.37).cos())
            .collect::<Vec<_>>();

        let mut batch_idx = HnswIndex::new(8, config.clone());
        batch_idx.batch_insert(vectors.clone());
        let exact = exact_top_k(&vectors, &query, &config.metric, 10);
        let ann = batch_idx.search(&query, 10);
        let exact_ids: std::collections::HashSet<u32> = exact.iter().map(|(id, _)| *id).collect();
        let hits = ann.iter().filter(|(id, _)| exact_ids.contains(id)).count();
        let recall_at_10 = hits as f32 / exact.len() as f32;
        assert!(
            recall_at_10 >= 0.8,
            "batch_insert recall@10={recall_at_10:.3} exact={exact:?} ann={ann:?}"
        );
    }

    #[test]
    fn hnsw_recall_fixture_reports_against_exact_bruteforce() {
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            m: 12,
            m0: 24,
            ef_search: 80,
            ef_construction: 120,
        };
        let mut idx = HnswIndex::new(8, config.clone());
        let mut vectors = Vec::new();
        for id in 0..120 {
            let vector = (0..8)
                .map(|dim| ((id as f32 + 1.0) * (dim as f32 + 0.5)).sin())
                .collect::<Vec<_>>();
            idx.insert(id, vector.clone());
            vectors.push((id, vector));
        }
        let query = (0..8)
            .map(|dim| ((dim as f32 + 3.25) * 0.37).cos())
            .collect::<Vec<_>>();

        let exact = exact_top_k(&vectors, &query, &config.metric, 10);
        let ann = idx.search(&query, 10);
        let exact_ids: std::collections::HashSet<u32> = exact.iter().map(|(id, _)| *id).collect();
        let hits = ann.iter().filter(|(id, _)| exact_ids.contains(id)).count();
        let recall_at_10 = hits as f32 / exact.len() as f32;
        println!(
            "hnsw_l2_fixture_recall_at_10={:.3} exact={:?} ann={:?}",
            recall_at_10, exact, ann
        );
        assert!(recall_at_10 >= 0.8);
    }

    #[test]
    fn hnsw_vector_snapshot_reload_preserves_replace_remove_reinsert_search() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw_vectors.json");
        let config = HnswConfig {
            metric: DistanceMetric::L2,
            ef_search: 64,
            ef_construction: 64,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(2, config.clone());
        idx.insert(1, vec![100.0, 100.0]);
        idx.insert(2, vec![10.0, 0.0]);
        idx.insert(3, vec![0.0, 10.0]);
        idx.replace(1, vec![0.0, 0.0]);
        assert!(idx.remove(2));
        idx.insert(2, vec![0.1, 0.0]);
        idx.save_vectors(&path).unwrap();

        let loaded = HnswIndex::load_vectors_with_expected(&path, 2, DistanceMetric::L2).unwrap();
        assert_eq!(loaded.len(), 3);
        let ids = loaded
            .search(&[0.0, 0.0], 10)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn hnsw_vector_snapshot_rejects_mismatch_duplicate_nonfinite_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw_vectors.json");
        let config = HnswConfig {
            metric: DistanceMetric::Cosine,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(2, config);
        idx.insert(1, vec![1.0, 0.0]);
        idx.save_vectors(&path).unwrap();

        let dim_err = match HnswIndex::load_vectors_with_expected(&path, 3, DistanceMetric::Cosine)
        {
            Ok(_) => panic!("expected HNSW dimension mismatch"),
            Err(err) => err,
        };
        assert!(dim_err.contains("dimension mismatch"));
        let metric_err = match HnswIndex::load_vectors_with_expected(&path, 2, DistanceMetric::L2) {
            Ok(_) => panic!("expected HNSW metric mismatch"),
            Err(err) => err,
        };
        assert!(metric_err.contains("metric mismatch"));

        let duplicate = dir.path().join("duplicate.json");
        std::fs::write(
            &duplicate,
            r#"{"version":1,"dim":2,"config":{"m":16,"m0":32,"ef_construction":200,"ef_search":50,"metric":"L2"},"vectors":[[1,[0.0,0.0]],[1,[1.0,0.0]]]}"#,
        )
        .unwrap();
        let duplicate_err = match HnswIndex::load_vectors(&duplicate) {
            Ok(_) => panic!("expected duplicate vector id error"),
            Err(err) => err,
        };
        assert!(duplicate_err.contains("duplicate vector id"));

        let bad_dim = dir.path().join("bad_dim.json");
        std::fs::write(
            &bad_dim,
            r#"{"version":1,"dim":2,"config":{"m":16,"m0":32,"ef_construction":200,"ef_search":50,"metric":"L2"},"vectors":[[1,[0.0]]]}"#,
        )
        .unwrap();
        let bad_dim_err = match HnswIndex::load_vectors(&bad_dim) {
            Ok(_) => panic!("expected vector dimension mismatch"),
            Err(err) => err,
        };
        assert!(bad_dim_err.contains("vector dimension mismatch"));

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{not json").unwrap();
        let corrupt_err = match HnswIndex::load_vectors(&corrupt) {
            Ok(_) => panic!("expected corrupt HNSW snapshot error"),
            Err(err) => err,
        };
        assert!(corrupt_err.contains("invalid HNSW snapshot JSON"));
    }
}
