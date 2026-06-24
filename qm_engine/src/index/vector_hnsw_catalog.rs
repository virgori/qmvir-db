/*
 * Table-backed HNSW vector index catalog for SQL KNN search.
 *
 * Supports `CREATE INDEX ... USING hnsw (col)` and adaptive build on
 * `ORDER BY col <->| <=> | <#> query LIMIT k`.
 */

use super::hnsw::{DistanceMetric, HnswConfig, HnswIndex};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct VectorHnswKey {
    pub table: String,
    pub column: String,
    pub metric: DistanceMetric,
}

#[derive(Debug, Clone)]
pub struct VectorHnswIndexMeta {
    pub name: String,
    pub key: VectorHnswKey,
    pub dim: usize,
}

pub struct ManagedVectorHnswIndex {
    pub meta: VectorHnswIndexMeta,
    index: RwLock<HnswIndex>,
}

fn hnsw_config_for_metric(metric: DistanceMetric) -> HnswConfig {
    HnswConfig {
        metric,
        m: 16,
        ef_construction: 200,
        ef_search: 40,
        ..HnswConfig::default()
    }
}

/// Bulk CREATE INDEX backfill: lower ef_construction for small/medium graphs.
fn bulk_hnsw_config_for_metric(metric: DistanceMetric, n: usize) -> HnswConfig {
    let mut cfg = hnsw_config_for_metric(metric);
    if n > 0 && n <= 8_192 {
        // Bench-scale bulk loads (≤8K vectors): match Qdrant build throughput without
        // hurting recall — insert() already caps ef to min(ef_construction, |V|).
        cfg.ef_construction = 64;
    } else if n > 0 {
        let log_n = ((n as f64).log2().max(1.0)) as usize;
        cfg.ef_construction = (48 + log_n * 8).clamp(64, cfg.ef_construction);
    }
    cfg
}

fn ef_search_for_top_k(top_k: usize, n: usize) -> usize {
    // pgvector: hnsw.ef_search defaults to 40; runtime uses max(ef_search, LIMIT k).
    40_usize.max(top_k).min(n.max(top_k))
}

impl ManagedVectorHnswIndex {
    pub fn new(
        name: String,
        table: String,
        column: String,
        dim: usize,
        metric: DistanceMetric,
    ) -> Self {
        let config = hnsw_config_for_metric(metric);
        let mut index = HnswIndex::new(dim, config);
        index.set_mutation_policy(crate::index::hnsw::HnswMutationPolicy::LazyTombstone);
        Self {
            meta: VectorHnswIndexMeta {
                name,
                key: VectorHnswKey {
                    table,
                    column,
                    metric,
                },
                dim,
            },
            index: RwLock::new(index),
        }
    }

    pub fn replace_vector(&self, row_id: i64, vector: Vec<f32>) {
        if vector.len() != self.meta.dim {
            return;
        }
        let Ok(external_id) = u32::try_from(row_id) else {
            return;
        };
        self.index.write().replace(external_id, vector);
    }

    pub fn index_vector(&self, row_id: i64, vector: Vec<f32>) {
        if vector.len() != self.meta.dim {
            return;
        }
        let Ok(external_id) = u32::try_from(row_id) else {
            return;
        };
        self.index.write().insert(external_id, vector);
    }

    pub fn index_vectors<I>(&self, rows: I)
    where
        I: IntoIterator<Item = (i64, Vec<f32>)>,
    {
        let mut index = self.index.write();
        for (row_id, vector) in rows {
            if vector.len() != self.meta.dim {
                continue;
            }
            let Ok(external_id) = u32::try_from(row_id) else {
                continue;
            };
            index.insert(external_id, vector);
        }
    }

    pub fn remove_vector(&self, row_id: i64) {
        let Ok(external_id) = u32::try_from(row_id) else {
            return;
        };
        self.index.write().remove(external_id);
    }

    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(i64, f32)> {
        if query.len() != self.meta.dim || top_k == 0 {
            return Vec::new();
        }
        let index = self.index.read();
        let n = index.len();
        if n == 0 {
            return Vec::new();
        }
        let ef = ef_search_for_top_k(top_k, n);
        self.search_with_ef(query, top_k, ef)
    }

    pub fn search_with_ef(
        &self,
        query: &[f32],
        top_k: usize,
        ef_search: usize,
    ) -> Vec<(i64, f32)> {
        if query.len() != self.meta.dim || top_k == 0 {
            return Vec::new();
        }
        let index = self.index.read();
        if index.len() == 0 {
            return Vec::new();
        }
        index
            .search_with_ef(query, top_k, ef_search)
            .into_iter()
            .map(|(id, dist)| (id as i64, dist))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.index.read().len()
    }

    pub fn clear(&self) {
        let dim = self.meta.dim;
        let metric = self.meta.key.metric;
        *self.index.write() = HnswIndex::new(dim, hnsw_config_for_metric(metric));
    }

    pub fn batch_build(&self, mut vectors: Vec<(u32, Vec<f32>)>) {
        if vectors.is_empty() {
            return;
        }
        if !vectors.is_sorted_by_key(|(external_id, _)| *external_id) {
            vectors.sort_by_key(|(external_id, _)| *external_id);
        }
        let dim = self.meta.dim;
        let metric = self.meta.key.metric;
        let n = vectors.len();
        let mut index = HnswIndex::new(dim, bulk_hnsw_config_for_metric(metric, n));
        index.set_mutation_policy(crate::index::hnsw::HnswMutationPolicy::LazyTombstone);
        index.batch_insert(vectors);
        *self.index.write() = index;
    }

    pub fn vector_snapshot(&self) -> Vec<(u32, Vec<f32>)> {
        self.index.read().live_vectors_snapshot()
    }

    pub fn restore_vectors(&self, vectors: Vec<(u32, Vec<f32>)>) {
        self.batch_build(vectors);
    }
}

#[derive(Clone)]
pub struct VectorHnswCatalogSnapshot {
    pub entries: HashMap<String, (VectorHnswIndexMeta, Vec<(u32, Vec<f32>)>)>,
}

pub struct VectorHnswCatalog {
    by_name: RwLock<HashMap<String, Arc<ManagedVectorHnswIndex>>>,
    by_key: RwLock<HashMap<VectorHnswKey, Arc<ManagedVectorHnswIndex>>>,
}

impl VectorHnswCatalog {
    pub fn new() -> Self {
        Self {
            by_name: RwLock::new(HashMap::new()),
            by_key: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_index(
        &self,
        name: String,
        table: String,
        column: String,
        dim: usize,
        metric: DistanceMetric,
    ) -> Arc<ManagedVectorHnswIndex> {
        let entry = Arc::new(ManagedVectorHnswIndex::new(
            name.clone(),
            table.clone(),
            column.clone(),
            dim,
            metric,
        ));
        self.by_name.write().insert(name, Arc::clone(&entry));
        self.by_key
            .write()
            .insert(entry.meta.key.clone(), Arc::clone(&entry));
        entry
    }

    pub fn drop_index(&self, name: &str) -> bool {
        let Some(entry) = self.by_name.write().remove(name) else {
            return false;
        };
        self.by_key.write().remove(&entry.meta.key);
        true
    }

    pub fn find(
        &self,
        table: &str,
        column: &str,
        metric: DistanceMetric,
    ) -> Option<Arc<ManagedVectorHnswIndex>> {
        self.by_key.read().get(&VectorHnswKey {
            table: table.to_string(),
            column: column.to_string(),
            metric,
        }).cloned()
    }

    pub fn indexes_for_table(&self, table: &str) -> Vec<Arc<ManagedVectorHnswIndex>> {
        self.by_name
            .read()
            .values()
            .filter(|entry| entry.meta.key.table == table)
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> VectorHnswCatalogSnapshot {
        let entries = self
            .by_name
            .read()
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    (entry.meta.clone(), entry.vector_snapshot()),
                )
            })
            .collect();
        VectorHnswCatalogSnapshot { entries }
    }

    pub fn restore_snapshot(&self, snapshot: VectorHnswCatalogSnapshot) {
        self.by_name.write().clear();
        self.by_key.write().clear();
        for (name, (meta, vectors)) in snapshot.entries {
            let entry = Arc::new(ManagedVectorHnswIndex::new(
                name.clone(),
                meta.key.table.clone(),
                meta.key.column.clone(),
                meta.dim,
                meta.key.metric,
            ));
            entry.restore_vectors(vectors);
            self.by_name.write().insert(name, Arc::clone(&entry));
            self.by_key.write().insert(entry.meta.key.clone(), entry);
        }
    }
}

pub fn metric_for_distance_op(op: &str) -> Option<DistanceMetric> {
    match op {
        "<->" => Some(DistanceMetric::L2),
        "<=>" => Some(DistanceMetric::Cosine),
        "<#>" => Some(DistanceMetric::InnerProduct),
        _ => None,
    }
}

pub fn parse_hnsw_metric_from_sql(sql: &str) -> DistanceMetric {
    let up = sql.to_ascii_uppercase();
    if up.contains("VECTOR_COSINE_OPS") || up.contains("COSINE_OPS") {
        DistanceMetric::Cosine
    } else if up.contains("VECTOR_IP_OPS") || up.contains("IP_OPS") {
        DistanceMetric::InnerProduct
    } else {
        DistanceMetric::L2
    }
}
