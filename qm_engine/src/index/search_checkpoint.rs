/*
 * Unified checkpoint persistence for inverted, JSON-path, trigram, and HNSW catalogs.
 */

use super::hnsw::DistanceMetric;
use super::inverted_catalog::{InvertedCatalogSnapshot, InvertedIndexCatalog, InvertedIndexMeta};
use super::json_path_catalog::{JsonPathCatalog, JsonPathCatalogSnapshot, JsonPathIndexMeta};
use super::trigram_catalog::{TrigramCatalog, TrigramCatalogSnapshot, TrigramIndexMeta};
use super::vector_hnsw_catalog::{
    VectorHnswCatalog, VectorHnswCatalogSnapshot, VectorHnswIndexMeta,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const CHECKPOINT_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct SearchIndexCheckpoint {
    version: u32,
    inverted: HashMap<String, PersistInvertedEntry>,
    json_path: HashMap<String, PersistJsonPathEntry>,
    trigram: HashMap<String, PersistTrigramEntry>,
    #[serde(default)]
    hnsw: HashMap<String, PersistHnswEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistInvertedEntry {
    meta: PersistInvertedMeta,
    documents: Vec<(u32, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistInvertedMeta {
    name: String,
    table: String,
    columns: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistJsonPathEntry {
    meta: PersistJsonPathMeta,
    postings: HashMap<String, Vec<i64>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistJsonPathMeta {
    name: String,
    table: String,
    column: String,
    path: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistTrigramEntry {
    meta: PersistTrigramMeta,
    postings: HashMap<String, Vec<i64>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistTrigramMeta {
    name: String,
    table: String,
    column: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistHnswEntry {
    meta: PersistHnswMeta,
    vectors: Vec<(u32, Vec<f32>)>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistHnswMeta {
    name: String,
    table: String,
    column: String,
    dim: usize,
    metric: DistanceMetric,
}

pub fn encode_search_indexes(
    inverted: &InvertedIndexCatalog,
    json_path: &JsonPathCatalog,
    trigram: &TrigramCatalog,
    hnsw: &VectorHnswCatalog,
) -> Result<Vec<u8>, String> {
    let inv = inverted.snapshot();
    let jp = json_path.snapshot();
    let tg = trigram.snapshot();
    let hv = hnsw.snapshot();

    let mut inverted_map = HashMap::new();
    for (name, (meta, docs)) in inv.entries {
        inverted_map.insert(
            name,
            PersistInvertedEntry {
                meta: PersistInvertedMeta {
                    name: meta.name,
                    table: meta.table,
                    columns: meta.columns,
                },
                documents: docs,
            },
        );
    }

    let mut json_path_map = HashMap::new();
    for (name, (meta, postings)) in jp.entries {
        json_path_map.insert(
            name,
            PersistJsonPathEntry {
                meta: PersistJsonPathMeta {
                    name: meta.name,
                    table: meta.key.table,
                    column: meta.key.column,
                    path: meta.key.path,
                },
                postings,
            },
        );
    }

    let mut trigram_map = HashMap::new();
    for (name, (meta, postings)) in tg.entries {
        trigram_map.insert(
            name,
            PersistTrigramEntry {
                meta: PersistTrigramMeta {
                    name: meta.name,
                    table: meta.key.table,
                    column: meta.key.column,
                },
                postings,
            },
        );
    }

    let mut hnsw_map = HashMap::new();
    for (name, (meta, vectors)) in hv.entries {
        hnsw_map.insert(
            name,
            PersistHnswEntry {
                meta: PersistHnswMeta {
                    name: meta.name,
                    table: meta.key.table,
                    column: meta.key.column,
                    dim: meta.dim,
                    metric: meta.key.metric,
                },
                vectors,
            },
        );
    }

    let checkpoint = SearchIndexCheckpoint {
        version: CHECKPOINT_VERSION,
        inverted: inverted_map,
        json_path: json_path_map,
        trigram: trigram_map,
        hnsw: hnsw_map,
    };
    serde_json::to_vec(&checkpoint).map_err(|err| format!("search index encode failed: {err}"))
}

pub fn load_search_indexes(
    inverted: &InvertedIndexCatalog,
    json_path: &JsonPathCatalog,
    trigram: &TrigramCatalog,
    hnsw: &VectorHnswCatalog,
    data: &[u8],
) -> Result<(), String> {
    let checkpoint: SearchIndexCheckpoint = serde_json::from_slice(data)
        .map_err(|err| format!("search index decode failed: {err}"))?;
    if checkpoint.version != 1 && checkpoint.version != CHECKPOINT_VERSION {
        return Err(format!(
            "unsupported search index checkpoint version {}",
            checkpoint.version
        ));
    }

    let mut inverted_entries = HashMap::new();
    for (name, entry) in checkpoint.inverted {
        inverted_entries.insert(
            name,
            (
                InvertedIndexMeta {
                    name: entry.meta.name,
                    table: entry.meta.table,
                    columns: entry.meta.columns,
                },
                entry.documents,
            ),
        );
    }
    inverted.restore_snapshot(InvertedCatalogSnapshot {
        entries: inverted_entries,
    });

    let mut json_path_entries = HashMap::new();
    for (name, entry) in checkpoint.json_path {
        json_path_entries.insert(
            name,
            (
                JsonPathIndexMeta {
                    name: entry.meta.name,
                    key: super::json_path_catalog::JsonPathKey {
                        table: entry.meta.table,
                        column: entry.meta.column,
                        path: entry.meta.path,
                    },
                },
                entry.postings,
            ),
        );
    }
    json_path.restore_snapshot(JsonPathCatalogSnapshot {
        entries: json_path_entries,
    });

    let mut trigram_entries = HashMap::new();
    for (name, entry) in checkpoint.trigram {
        trigram_entries.insert(
            name,
            (
                TrigramIndexMeta {
                    name: entry.meta.name,
                    key: super::trigram_catalog::TrigramKey {
                        table: entry.meta.table,
                        column: entry.meta.column,
                    },
                },
                entry.postings,
            ),
        );
    }
    trigram.restore_snapshot(TrigramCatalogSnapshot {
        entries: trigram_entries,
    });

    if !checkpoint.hnsw.is_empty() {
        let mut hnsw_entries = HashMap::new();
        for (name, entry) in checkpoint.hnsw {
            hnsw_entries.insert(
                name,
                (
                    VectorHnswIndexMeta {
                        name: entry.meta.name,
                        key: super::vector_hnsw_catalog::VectorHnswKey {
                            table: entry.meta.table,
                            column: entry.meta.column,
                            metric: entry.meta.metric,
                        },
                        dim: entry.meta.dim,
                    },
                    entry.vectors,
                ),
            );
        }
        hnsw.restore_snapshot(VectorHnswCatalogSnapshot { entries: hnsw_entries });
    }

    Ok(())
}
