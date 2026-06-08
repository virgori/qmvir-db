/*
 * Vectorized Operators - SIMD-optimized execution primitives
 */

use super::batch::Bitmap;
use rayon::prelude::*;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// SIMD-optimized dot product for f32 vectors
#[inline]
pub fn simd_dot_product(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let n = a.len();
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    let mut sum = unsafe {
        let mut acc = vdupq_n_f32(0.0);
        while i + 4 <= n {
            let va = vld1q_f32(a.as_ptr().add(i));
            let vb = vld1q_f32(b.as_ptr().add(i));
            acc = vfmaq_f32(acc, va, vb);
            i += 4;
        }
        // Horizontal sum
        let sum2 = vadd_f32(vget_low_f32(acc), vget_high_f32(acc));
        vget_lane_f32(vpadd_f32(sum2, sum2), 0)
    };

    #[cfg(target_arch = "x86_64")]
    let mut sum = unsafe {
        if is_x86_feature_detected!("avx512f") {
            let mut acc = _mm512_setzero_ps();
            while i + 16 <= n {
                let va = _mm512_loadu_ps(a.as_ptr().add(i));
                let vb = _mm512_loadu_ps(b.as_ptr().add(i));
                acc = _mm512_fmadd_ps(va, vb, acc);
                i += 16;
            }
            _mm512_reduce_add_ps(acc)
        } else if is_x86_feature_detected!("avx2") {
            let mut acc = _mm256_setzero_ps();
            while i + 8 <= n {
                let va = _mm256_loadu_ps(a.as_ptr().add(i));
                let vb = _mm256_loadu_ps(b.as_ptr().add(i));
                acc = _mm256_fmadd_ps(va, vb, acc);
                i += 8;
            }
            // Horizontal sum
            let hi = _mm256_extractf128_ps(acc, 1);
            let lo = _mm256_castps256_ps128(acc);
            let sum128 = _mm_add_ps(lo, hi);
            let sum128 = _mm_hadd_ps(sum128, sum128);
            let sum128 = _mm_hadd_ps(sum128, sum128);
            _mm_cvtss_f32(sum128)
        } else {
            let mut acc = _mm_setzero_ps();
            while i + 4 <= n {
                let va = _mm_loadu_ps(a.as_ptr().add(i));
                let vb = _mm_loadu_ps(b.as_ptr().add(i));
                acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
                i += 4;
            }
            acc = _mm_hadd_ps(acc, acc);
            acc = _mm_hadd_ps(acc, acc);
            _mm_cvtss_f32(acc)
        }
    };

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let mut sum = 0.0f32;

    // Scalar fallback for remaining elements
    while i < n {
        sum += a[i] * b[i];
        i += 1;
    }

    sum
}

/// SIMD-optimized L2 distance squared
#[inline]
pub fn simd_l2_distance_sq(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let n = a.len();
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    let mut sum = unsafe {
        let mut acc = vdupq_n_f32(0.0);
        while i + 4 <= n {
            let va = vld1q_f32(a.as_ptr().add(i));
            let vb = vld1q_f32(b.as_ptr().add(i));
            let diff = vsubq_f32(va, vb);
            acc = vfmaq_f32(acc, diff, diff);
            i += 4;
        }
        let sum2 = vadd_f32(vget_low_f32(acc), vget_high_f32(acc));
        vget_lane_f32(vpadd_f32(sum2, sum2), 0)
    };

    #[cfg(target_arch = "x86_64")]
    let mut sum = unsafe {
        if is_x86_feature_detected!("avx512f") {
            let mut acc = _mm512_setzero_ps();
            while i + 16 <= n {
                let va = _mm512_loadu_ps(a.as_ptr().add(i));
                let vb = _mm512_loadu_ps(b.as_ptr().add(i));
                let diff = _mm512_sub_ps(va, vb);
                acc = _mm512_fmadd_ps(diff, diff, acc);
                i += 16;
            }
            _mm512_reduce_add_ps(acc)
        } else if is_x86_feature_detected!("avx2") {
            let mut acc = _mm256_setzero_ps();
            while i + 8 <= n {
                let va = _mm256_loadu_ps(a.as_ptr().add(i));
                let vb = _mm256_loadu_ps(b.as_ptr().add(i));
                let diff = _mm256_sub_ps(va, vb);
                acc = _mm256_fmadd_ps(diff, diff, acc);
                i += 8;
            }
            let hi = _mm256_extractf128_ps(acc, 1);
            let lo = _mm256_castps256_ps128(acc);
            let sum128 = _mm_add_ps(lo, hi);
            let sum128 = _mm_hadd_ps(sum128, sum128);
            let sum128 = _mm_hadd_ps(sum128, sum128);
            _mm_cvtss_f32(sum128)
        } else {
            0.0
        }
    };

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let mut sum = 0.0f32;

    while i < n {
        let d = a[i] - b[i];
        sum += d * d;
        i += 1;
    }

    sum
}

/// SIMD-optimized vector norm
#[inline]
pub fn simd_norm(a: &[f32]) -> f32 {
    simd_dot_product(a, a).sqrt()
}

/// SIMD-optimized SUM for f64 slices.
#[inline]
pub fn simd_sum_f64(values: &[f64]) -> f64 {
    let n = values.len();
    let mut i = 0usize;

    #[cfg(target_arch = "aarch64")]
    let mut sum = unsafe {
        let mut acc = vdupq_n_f64(0.0);
        while i + 2 <= n {
            let v = vld1q_f64(values.as_ptr().add(i));
            acc = vaddq_f64(acc, v);
            i += 2;
        }
        let pair = vadd_f64(vget_low_f64(acc), vget_high_f64(acc));
        vget_lane_f64(pair, 0)
    };

    #[cfg(target_arch = "x86_64")]
    let mut sum = unsafe {
        if is_x86_feature_detected!("avx512f") {
            let mut acc = _mm512_setzero_pd();
            while i + 8 <= n {
                let v = _mm512_loadu_pd(values.as_ptr().add(i));
                acc = _mm512_add_pd(acc, v);
                i += 8;
            }
            _mm512_reduce_add_pd(acc)
        } else if is_x86_feature_detected!("avx2") {
            let mut acc = _mm256_setzero_pd();
            while i + 4 <= n {
                let v = _mm256_loadu_pd(values.as_ptr().add(i));
                acc = _mm256_add_pd(acc, v);
                i += 4;
            }
            let hi = _mm256_extractf128_pd(acc, 1);
            let lo = _mm256_castpd256_pd128(acc);
            let s = _mm_add_pd(lo, hi);
            let shuf = _mm_unpackhi_pd(s, s);
            _mm_cvtsd_f64(_mm_add_sd(s, shuf))
        } else {
            0.0
        }
    };

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let mut sum = 0.0f64;

    while i < n {
        sum += values[i];
        i += 1;
    }
    sum
}

/// Batch cosine distances from query to all vectors
pub fn batch_cosine_distances(query: &[f32], vectors: &[f32], dim: usize) -> Vec<f32> {
    let n_vectors = vectors.len() / dim;
    let norm_q = simd_norm(query);

    if n_vectors > 1000 {
        // Parallel execution for large batches
        (0..n_vectors)
            .into_par_iter()
            .map(|i| {
                let v = &vectors[i * dim..(i + 1) * dim];
                let dot = simd_dot_product(query, v);
                let norm_v = simd_norm(v);
                let denom = norm_q * norm_v;
                if denom < 1e-10 {
                    1.0
                } else {
                    1.0 - dot / denom
                }
            })
            .collect()
    } else {
        // Sequential for small batches (avoid thread overhead)
        (0..n_vectors)
            .map(|i| {
                let v = &vectors[i * dim..(i + 1) * dim];
                let dot = simd_dot_product(query, v);
                let norm_v = simd_norm(v);
                let denom = norm_q * norm_v;
                if denom < 1e-10 {
                    1.0
                } else {
                    1.0 - dot / denom
                }
            })
            .collect()
    }
}

/// Batch L2 distances from query to all vectors
pub fn batch_l2_distances(query: &[f32], vectors: &[f32], dim: usize) -> Vec<f32> {
    let n_vectors = vectors.len() / dim;

    if n_vectors > 1000 {
        (0..n_vectors)
            .into_par_iter()
            .map(|i| {
                let v = &vectors[i * dim..(i + 1) * dim];
                simd_l2_distance_sq(query, v).sqrt()
            })
            .collect()
    } else {
        (0..n_vectors)
            .map(|i| {
                let v = &vectors[i * dim..(i + 1) * dim];
                simd_l2_distance_sq(query, v).sqrt()
            })
            .collect()
    }
}

/// Filter rows using SIMD comparison
pub fn filter_greater_than_f32(values: &[f32], threshold: f32) -> Bitmap {
    let n = values.len();
    let mut bitmap = Bitmap::all_null(n);
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    unsafe {
        let thresh = vdupq_n_f32(threshold);
        while i + 4 <= n {
            let v = vld1q_f32(values.as_ptr().add(i));
            let cmp = vcgtq_f32(v, thresh);
            // Extract comparison results
            let mask = [
                vgetq_lane_u32(cmp, 0) != 0,
                vgetq_lane_u32(cmp, 1) != 0,
                vgetq_lane_u32(cmp, 2) != 0,
                vgetq_lane_u32(cmp, 3) != 0,
            ];
            for j in 0..4 {
                bitmap.set_valid(i + j, mask[j]);
            }
            i += 4;
        }
    }

    // Scalar fallback
    while i < n {
        bitmap.set_valid(i, values[i] > threshold);
        i += 1;
    }

    bitmap
}

/// Aggregate sum using SIMD
pub fn aggregate_sum_f32(values: &[f32], validity: &Bitmap) -> f32 {
    let n = values.len();
    let mut sum = 0.0f32;
    let mut i = 0;

    // For simplicity, use scalar with validity check
    // SIMD optimization would require masked loads
    while i < n {
        if validity.is_valid(i) {
            sum += values[i];
        }
        i += 1;
    }

    sum
}

/// Aggregate sum for i64 using SIMD
pub fn aggregate_sum_i64(values: &[i64], validity: &Bitmap) -> i64 {
    let n = values.len();
    let mut sum = 0i64;

    #[cfg(target_arch = "aarch64")]
    {
        // ARM NEON doesn't have great i64 SIMD, use scalar
        for i in 0..n {
            if validity.is_valid(i) {
                sum += values[i];
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        if is_x86_feature_detected!("avx2") {
            let mut i = 0;
            let mut acc = _mm256_setzero_si256();

            while i + 4 <= n {
                let v = _mm256_loadu_si256(values.as_ptr().add(i) as *const __m256i);
                // TODO: Add validity masking
                acc = _mm256_add_epi64(acc, v);
                i += 4;
            }

            // Horizontal sum
            let lo = _mm256_castsi256_si128(acc);
            let hi = _mm256_extracti128_si256(acc, 1);
            let sum128 = _mm_add_epi64(lo, hi);
            let hi64 = _mm_unpackhi_epi64(sum128, sum128);
            let result = _mm_add_epi64(sum128, hi64);
            sum = _mm_cvtsi128_si64(result);

            // Handle remaining
            while i < n {
                if validity.is_valid(i) {
                    sum += values[i];
                }
                i += 1;
            }
        } else {
            for i in 0..n {
                if validity.is_valid(i) {
                    sum += values[i];
                }
            }
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        for i in 0..n {
            if validity.is_valid(i) {
                sum += values[i];
            }
        }
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dot_product() {
        let a = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = vec![8.0f32, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0];

        let result = simd_dot_product(&a, &b);
        let expected: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

        assert!((result - expected).abs() < 1e-5);
    }

    #[test]
    fn test_l2_distance() {
        let a = vec![1.0f32, 2.0, 3.0, 4.0];
        let b = vec![5.0f32, 6.0, 7.0, 8.0];

        let result = simd_l2_distance_sq(&a, &b).sqrt();
        let expected: f32 = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt();

        assert!((result - expected).abs() < 1e-5);
    }

    #[test]
    fn test_simd_sum_f64() {
        let vals: Vec<f64> = (1..=1024).map(|x| x as f64 * 0.5).collect();
        let got = simd_sum_f64(&vals);
        let expected: f64 = vals.iter().sum();
        assert!((got - expected).abs() < 1e-9 * expected.max(1.0));
    }
}
