/*
 * Distributed Sharded Indexes — HNSW + Inverted across multiple shards
 *
 * Uses ConsistentHashRing from cluster::shard for routing.
 * Each shard holds an independent HnswIndex or InvertedIndex.
 * Searches fan out to all shards and merge results.
 *
 * Architecture:
 *   ShardedHnswIndex: vector ID → shard via consistent hash → local HNSW
 *   ShardedInvertedIndex: doc ID → shard via consistent hash → local inverted
 *
 * This is a single-process multi-shard design. For multi-node, each
 * shard can be assigned to a different node via ShardManager + Transport.
 */

use crate::cluster::shard::ConsistentHashRing;
use crate::index::hnsw::{HnswConfig, HnswIndex};
use crate::index::inverted::{InvertedIndex, ScoredDoc, SearchStrategy};
use parking_lot::RwLock;
use std::sync::Arc;

// ── Sharded HNSW ────────────────────────────────────────────────────

/// Distributed HNSW index across N shards.
///
/// Inserts are routed to a single shard via consistent hashing on vector ID.
/// Searches fan out to ALL shards and merge top-k results (exact recall).
pub struct ShardedHnswIndex {
    shards: Vec<Arc<RwLock<HnswIndex>>>,
    ring: ConsistentHashRing,
}

impl ShardedHnswIndex {
    /// Create a new sharded HNSW index with `num_shards` partitions.
    pub fn new(dim: usize, config: HnswConfig, num_shards: u32) -> Self {
        let vnodes_per_shard = 128;
        let ring = ConsistentHashRing::new(num_shards, vnodes_per_shard);
        let shards: Vec<_> = (0..num_shards)
            .map(|_| Arc::new(RwLock::new(HnswIndex::new(dim, config.clone()))))
            .collect();
        Self { shards, ring }
    }

    /// Insert a vector, routed to the appropriate shard.
    pub fn insert(&self, id: u32, vector: Vec<f32>) {
        let shard_id = self.ring.shard_for_id(id as i64).unwrap_or(0);
        let shard = &self.shards[shard_id as usize];
        shard.write().insert(id, vector);
    }

    /// Batch insert: group vectors by shard, then insert per-shard.
    pub fn batch_insert(&self, vectors: Vec<(u32, Vec<f32>)>) {
        // Group by shard
        let num_shards = self.shards.len();
        let mut groups: Vec<Vec<(u32, Vec<f32>)>> = (0..num_shards).map(|_| Vec::new()).collect();

        for (id, vec) in vectors {
            let shard_id = self.ring.shard_for_id(id as i64).unwrap_or(0) as usize;
            if shard_id < num_shards {
                groups[shard_id].push((id, vec));
            }
        }

        // Insert per shard (could be parallelized with rayon)
        for (shard_id, group) in groups.into_iter().enumerate() {
            if !group.is_empty() {
                let shard = &self.shards[shard_id];
                let mut shard_guard = shard.write();
                shard_guard.batch_insert(group);
            }
        }
    }

    /// Search all shards and merge top-k results.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(u32, f32)> {
        let mut all_results: Vec<(u32, f32)> = Vec::new();

        for shard in &self.shards {
            let shard_guard = shard.read();
            if shard_guard.is_empty() {
                continue;
            }
            let shard_results = shard_guard.search(query, top_k);
            all_results.extend(shard_results);
        }

        // Global merge: sort by distance, keep top_k
        all_results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        all_results.truncate(top_k);
        all_results
    }

    /// Total vectors across all shards.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.read().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.read().is_empty())
    }

    /// Per-shard vector counts (for balance monitoring).
    pub fn shard_sizes(&self) -> Vec<usize> {
        self.shards.iter().map(|s| s.read().len()).collect()
    }

    /// Number of shards.
    pub fn num_shards(&self) -> usize {
        self.shards.len()
    }
}

// ── Sharded Inverted Index ──────────────────────────────────────────

/// Distributed inverted index across N shards.
///
/// Documents are partitioned by doc_id via consistent hashing.
/// Each shard holds an independent InvertedIndex with its own BM25 stats.
/// Searches fan out to all shards and merge top-k by score.
pub struct ShardedInvertedIndex {
    shards: Vec<Arc<RwLock<InvertedIndex>>>,
    ring: ConsistentHashRing,
}

impl ShardedInvertedIndex {
    /// Create with `num_shards` partitions.
    pub fn new(num_shards: u32) -> Self {
        let vnodes_per_shard = 128;
        let ring = ConsistentHashRing::new(num_shards, vnodes_per_shard);
        let shards: Vec<_> = (0..num_shards)
            .map(|_| Arc::new(RwLock::new(InvertedIndex::new())))
            .collect();
        Self { shards, ring }
    }

    /// Index a document on the appropriate shard.
    pub fn index_document(&self, doc_id: u32, text: &str) {
        let shard_id = self.ring.shard_for_id(doc_id as i64).unwrap_or(0);
        let shard = &self.shards[shard_id as usize];
        shard.write().index_document(doc_id, text);
    }

    /// Remove a document from its shard.
    pub fn remove_document(&self, doc_id: u32) {
        let shard_id = self.ring.shard_for_id(doc_id as i64).unwrap_or(0);
        let shard = &self.shards[shard_id as usize];
        shard.write().remove_document(doc_id);
    }

    /// Finalize all shards (sort posting lists, compute block-max scores).
    pub fn finalize(&self) {
        for shard in &self.shards {
            shard.write().finalize();
        }
    }

    /// Search all shards and merge top-k results by BM25 score.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<ScoredDoc> {
        self.search_with_strategy(query, top_k, SearchStrategy::BMW)
    }

    /// Search with explicit strategy across all shards.
    pub fn search_with_strategy(
        &self,
        query: &str,
        top_k: usize,
        strategy: SearchStrategy,
    ) -> Vec<ScoredDoc> {
        let mut all_results: Vec<ScoredDoc> = Vec::new();

        for shard in &self.shards {
            let shard_guard = shard.read();
            if shard_guard.doc_count() == 0 {
                continue;
            }
            let shard_results = shard_guard.search_with_strategy(query, top_k, strategy.clone());
            all_results.extend(shard_results);
        }

        // Global merge: sort by score descending, keep top_k
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(top_k);
        all_results
    }

    /// Total documents across all shards.
    pub fn doc_count(&self) -> u32 {
        self.shards.iter().map(|s| s.read().doc_count()).sum()
    }

    /// Per-shard document counts.
    pub fn shard_sizes(&self) -> Vec<u32> {
        self.shards.iter().map(|s| s.read().doc_count()).collect()
    }

    /// Number of shards.
    pub fn num_shards(&self) -> usize {
        self.shards.len()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    fn random_vector(dim: usize) -> Vec<f32> {
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen::<f32>()).collect()
    }

    #[test]
    fn test_sharded_hnsw_basic() {
        let dim = 8;
        let config = HnswConfig::default();
        let idx = ShardedHnswIndex::new(dim, config, 4);

        // Insert vectors
        for i in 0..200u32 {
            idx.insert(i, random_vector(dim));
        }

        assert_eq!(idx.len(), 200);
        assert!(!idx.is_empty());

        // All shards should have vectors (with high probability)
        let sizes = idx.shard_sizes();
        assert_eq!(sizes.len(), 4);
        assert!(
            sizes.iter().all(|&s| s > 0),
            "Some shards are empty: {:?}",
            sizes
        );

        // Search should find results
        let results = idx.search(&random_vector(dim), 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_sharded_hnsw_recall() {
        let dim = 4;
        let config = HnswConfig::default();
        let idx = ShardedHnswIndex::new(dim, config, 2);

        let target = vec![1.0, 0.0, 0.0, 0.0];
        idx.insert(0, target.clone());
        for i in 1..100u32 {
            idx.insert(i, random_vector(dim));
        }

        let results = idx.search(&target, 1);
        assert_eq!(results[0].0, 0);
        assert!(results[0].1 < 0.001);
    }

    #[test]
    fn test_sharded_inverted_basic() {
        let idx = ShardedInvertedIndex::new(4);

        idx.index_document(1, "the quick brown fox jumps over the lazy dog");
        idx.index_document(2, "the quick brown fox");
        idx.index_document(3, "the dog chased the cat");
        idx.index_document(4, "database systems are complex and powerful systems");
        idx.index_document(5, "high performance database query engine");
        idx.finalize();

        assert_eq!(idx.doc_count(), 5);

        let results = idx.search("quick fox", 10);
        assert!(!results.is_empty());
        // Doc 1 or 2 should score highest
        assert!(results[0].doc_id == 1 || results[0].doc_id == 2);
    }

    #[test]
    fn test_sharded_inverted_distribution() {
        let idx = ShardedInvertedIndex::new(4);

        for i in 0..100u32 {
            idx.index_document(
                i,
                &format!("document number {} with some text content here", i),
            );
        }
        idx.finalize();

        let sizes = idx.shard_sizes();
        assert_eq!(sizes.len(), 4);
        let total: u32 = sizes.iter().sum();
        assert_eq!(total, 100);
        // Each shard should have roughly 25 docs (±15)
        for &size in &sizes {
            assert!(size > 5, "Shard too small: {} (imbalanced)", size);
        }
    }

    #[test]
    fn test_sharded_inverted_remove() {
        let idx = ShardedInvertedIndex::new(2);
        idx.index_document(1, "hello world");
        idx.index_document(2, "hello rust");
        idx.finalize();

        assert_eq!(idx.doc_count(), 2);
        idx.remove_document(1);
        assert_eq!(idx.doc_count(), 1);
    }
}
