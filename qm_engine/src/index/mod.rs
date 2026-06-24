/*
 * Index Module – B+Tree Native Index & Autonomous Indexer
 *
 * Public surface:
 *   • BPlusTree        – persistent B+Tree with CRC32-per-page
 *   • IndexManager     – catalogue + autonomous brain
 *   • IndexKey / RowId – key types shared across modules
 *   • PyIndexManager   – Python binding for CLI / dashboard
 */

pub mod auto_manager;
pub mod bplus_tree;
pub mod concurrent_hnsw;
pub mod hnsw;
pub mod inverted;
pub mod inverted_catalog;
pub mod json_path_catalog;
pub mod mmap_store;
pub mod search_checkpoint;
pub mod trigram_catalog;
pub mod vector_hnsw_catalog;
pub mod roaring;
pub mod sharded;
pub mod wal_inverted;

pub use auto_manager::{
    AutoDecision, ColumnStats, IndexManager, IndexManagerSnapshot, IndexMeta, IndexState,
    NumericHistogram,
};
pub use bplus_tree::{BPlusTree, IndexKey, IndexLookupKeyRef, RowId};
pub use concurrent_hnsw::ConcurrentHnswIndex;
pub use hnsw::{
    DistanceMetric, HnswConfig, HnswIndex, HnswMutationPolicy, HnswPqIndex, ProductQuantizer,
};
pub use inverted::{InvertedIndex, ScoredDoc, SearchStrategy};
pub use inverted_catalog::{InvertedCatalogSnapshot, InvertedIndexCatalog, ManagedInvertedIndex};
pub use json_path_catalog::{
    extract_json_path_text, JsonPathCatalog, JsonPathCatalogSnapshot, ManagedJsonPathIndex,
};
pub use search_checkpoint::{encode_search_indexes, load_search_indexes};
pub use trigram_catalog::{ManagedTrigramIndex, TrigramCatalog, TrigramCatalogSnapshot};
pub use vector_hnsw_catalog::{
    metric_for_distance_op, parse_hnsw_metric_from_sql, ManagedVectorHnswIndex,
    VectorHnswCatalog, VectorHnswCatalogSnapshot,
};
pub use mmap_store::{AccessPattern, MmapGraphStore, MmapVectorStore};
pub use roaring::RoaringBitmap;
pub use sharded::{ShardedHnswIndex, ShardedInvertedIndex};
pub use wal_inverted::WalInvertedIndex;

#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use std::sync::Arc;

// ── Python binding ──────────────────────────────────────────────────────────

#[cfg(feature = "python")]
#[pyclass(name = "IndexManager")]
pub struct PyIndexManager {
    inner: Arc<IndexManager>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyIndexManager {
    #[new]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(IndexManager::new()),
        }
    }

    /// Create a manual index. Returns entry count (0 for new empty index).
    pub fn create_index(&self, name: &str, table: &str, columns: Vec<String>) -> usize {
        let tree = self.inner.create_manual_index(name, table, &columns);
        tree.entry_count()
    }

    /// Drop an index by name. Returns true if it existed.
    pub fn drop_index(&self, name: &str) -> bool {
        self.inner.drop_index(name)
    }

    /// List all indexes as (name, table, columns, state, use_count).
    pub fn list_indexes(&self) -> Vec<(String, String, Vec<String>, String, u64)> {
        self.inner
            .list_indexes()
            .into_iter()
            .map(|m| {
                let state_str = match m.state {
                    IndexState::Building => "building",
                    IndexState::Shadow => "shadow",
                    IndexState::Active => "active",
                    IndexState::PendingDrop => "pending_drop",
                    IndexState::Manual => "manual",
                };
                (
                    m.name,
                    m.table,
                    m.columns,
                    state_str.to_string(),
                    m.use_count,
                )
            })
            .collect()
    }

    /// Run autonomous evaluation cycle. Returns list of decision descriptions.
    pub fn evaluate(&self) -> Vec<String> {
        self.inner
            .evaluate()
            .iter()
            .map(|d| format!("{:?}", d))
            .collect()
    }

    /// Apply autonomous decisions from the last evaluation.
    pub fn apply_decisions(&self) {
        let decisions = self.inner.evaluate();
        self.inner.apply_decisions(&decisions);
    }

    /// Record a query hit for statistics.
    pub fn record_query_hit(&self, table: &str, column: &str) {
        self.inner.record_query_hit(table, column);
    }

    /// Record a write operation for statistics.
    pub fn record_write(&self, table: &str, column: &str) {
        self.inner.record_write(table, column);
    }

    /// Update selectivity estimate.
    pub fn update_selectivity(&self, table: &str, column: &str, distinct: u64, total: u64) {
        self.inner
            .update_selectivity(table, column, distinct, total);
    }

    /// Record a numeric value sample into histogram statistics.
    pub fn record_numeric_value(&self, table: &str, column: &str, value: f64) {
        self.inner.record_numeric_value(table, column, value);
    }

    /// Estimate BETWEEN selectivity from histogram and update cached stat.
    pub fn update_selectivity_between(
        &self,
        table: &str,
        column: &str,
        lo: f64,
        hi: f64,
    ) -> Option<f64> {
        self.inner
            .update_selectivity_from_histogram_between(table, column, lo, hi)
    }

    /// Return histogram snapshot: (min, max, total, buckets).
    pub fn histogram(&self, table: &str, column: &str) -> Option<(f64, f64, u64, Vec<u64>)> {
        self.inner
            .histogram_snapshot(table, column)
            .map(|h| (h.min, h.max, h.total, h.buckets.to_vec()))
    }

    /// Check if an index build is running.
    pub fn is_building(&self) -> bool {
        self.inner.is_building()
    }
}
