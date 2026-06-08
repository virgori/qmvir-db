/*
 * Execution Kernel - Vectorized SIMD Execution Engine
 *
 * High-performance columnar execution with:
 * - SIMD operations (NEON on ARM, AVX2 on x86)
 * - Batch processing (columnar format)
 * - Zero-copy data access
 * - Parallel execution with Rayon
 */

pub mod agg;
mod batch;
pub mod hybrid_search;
pub mod jit;
pub mod join;
mod operators;
pub mod txn;
mod vectorized;

pub use agg::{apply_having, AggFunction, AggSpec, AggValue, AggregateExecutor, HavingPredicate};
pub use batch::*;
#[cfg(feature = "python")]
pub use jit::PyJitCompiler;
pub use jit::{JitCache, JitExpr};
pub use join::{JoinCell, JoinExecutor, JoinKind, JoinRow, JoinStrategy};
pub use operators::*;
pub use txn::{MvccStore, TxnId, TxnStatus};
pub use vectorized::*;

#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use std::sync::Arc;

/// Execution result
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub columns: Vec<ColumnBatch>,
    pub row_count: usize,
    pub affected_rows: usize,
}

/// Vector executor for query execution
pub struct VectorExecutor {
    batch_size: usize,
    parallel_threshold: usize,
}

impl VectorExecutor {
    pub fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            parallel_threshold: 10000,
        }
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn parallel_threshold(&self) -> usize {
        self.parallel_threshold
    }
}

impl Default for VectorExecutor {
    fn default() -> Self {
        Self::new(1024)
    }
}

/// Python-exposed vector executor
#[cfg(feature = "python")]
#[pyclass(name = "VectorExecutor")]
pub struct PyVectorExecutor {
    inner: Arc<VectorExecutor>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyVectorExecutor {
    #[new]
    #[pyo3(signature = (batch_size=1024))]
    pub fn new(batch_size: usize) -> Self {
        Self {
            inner: Arc::new(VectorExecutor::new(batch_size)),
        }
    }

    pub fn get_batch_size(&self) -> usize {
        self.inner.batch_size()
    }

    /// Compute dot products between vectors and query (SIMD accelerated)
    pub fn batch_dot_product(&self, vectors: Vec<Vec<f32>>, query: Vec<f32>) -> Vec<f32> {
        vectors
            .iter()
            .map(|v| simd_dot_product(v, &query))
            .collect()
    }

    /// Compute L2 distances between vectors and query (SIMD accelerated)
    pub fn batch_l2_distance(&self, vectors: Vec<Vec<f32>>, query: Vec<f32>) -> Vec<f32> {
        vectors
            .iter()
            .map(|v| simd_l2_distance_sq(v, &query).sqrt())
            .collect()
    }

    /// Search for top-k nearest vectors using dot product similarity
    pub fn search(&self, vectors: Vec<Vec<f32>>, query: Vec<f32>, k: usize) -> Vec<(usize, f32)> {
        let mut scores: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (i, simd_dot_product(v, &query)))
            .collect();

        // Partial sort for top-k
        if k < scores.len() {
            scores.select_nth_unstable_by(k, |a, b| b.1.partial_cmp(&a.1).unwrap());
            scores.truncate(k);
        }
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scores
    }

    /// Parallel batch dot product with Rayon
    pub fn parallel_batch_dot_product(&self, vectors: Vec<Vec<f32>>, query: Vec<f32>) -> Vec<f32> {
        use rayon::prelude::*;
        vectors
            .par_iter()
            .map(|v| simd_dot_product(v, &query))
            .collect()
    }
}
