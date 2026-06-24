/*
 * Table-backed inverted index catalog for SQL full-text search.
 *
 * Supports `CREATE INDEX ... USING gin (col)` and `WHERE col @@ 'query'`.
 */

use super::inverted::{InvertedIndex, ScoredDoc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct InvertedIndexMeta {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
}

pub struct ManagedInvertedIndex {
    pub meta: InvertedIndexMeta,
    index: RwLock<InvertedIndex>,
    needs_finalize: AtomicBool,
}

impl ManagedInvertedIndex {
    pub fn new(name: String, table: String, columns: Vec<String>) -> Self {
        Self {
            meta: InvertedIndexMeta {
                name,
                table,
                columns,
            },
            index: RwLock::new(InvertedIndex::new()),
            needs_finalize: AtomicBool::new(false),
        }
    }

    pub fn index_row(&self, doc_id: u32, text: &str) {
        let mut index = self.index.write();
        index.index_document(doc_id, text);
        self.needs_finalize.store(true, Ordering::Release);
    }

    pub fn remove_row(&self, doc_id: u32) {
        let mut index = self.index.write();
        index.remove_document(doc_id);
        self.needs_finalize.store(true, Ordering::Release);
    }

    pub fn finalize_if_needed(&self) {
        if self.needs_finalize.load(Ordering::Acquire) {
            let mut index = self.index.write();
            index.finalize();
            self.needs_finalize.store(false, Ordering::Release);
        }
    }

    pub fn search(&self, query: &str, top_k: usize) -> Vec<ScoredDoc> {
        self.finalize_if_needed();
        self.index.read().search(query, top_k)
    }

    pub fn document_snapshot(&self) -> Vec<(u32, String)> {
        self.index.read().clone_documents()
    }

    pub fn restore_documents(&self, docs: Vec<(u32, String)>) {
        let rebuilt = InvertedIndex::from_documents(docs);
        *self.index.write() = rebuilt;
        self.needs_finalize.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct InvertedCatalogSnapshot {
    pub entries: HashMap<String, (InvertedIndexMeta, Vec<(u32, String)>)>,
}

pub struct InvertedIndexCatalog {
    entries: RwLock<HashMap<String, Arc<ManagedInvertedIndex>>>,
}

impl InvertedIndexCatalog {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_index(
        &self,
        name: String,
        table: String,
        columns: Vec<String>,
    ) -> Arc<ManagedInvertedIndex> {
        let entry = Arc::new(ManagedInvertedIndex::new(name.clone(), table, columns));
        self.entries.write().insert(name, Arc::clone(&entry));
        entry
    }

    pub fn drop_index(&self, name: &str) -> bool {
        self.entries.write().remove(name).is_some()
    }

    pub fn get(&self, name: &str) -> Option<Arc<ManagedInvertedIndex>> {
        self.entries.read().get(name).cloned()
    }

    pub fn indexes_for_table(&self, table: &str) -> Vec<Arc<ManagedInvertedIndex>> {
        self.entries
            .read()
            .values()
            .filter(|entry| entry.meta.table == table)
            .cloned()
            .collect()
    }

    pub fn find_for_column(&self, table: &str, column: &str) -> Option<Arc<ManagedInvertedIndex>> {
        self.entries.read().values().find_map(|entry| {
            if entry.meta.table == table && entry.meta.columns.iter().any(|c| c == column) {
                Some(Arc::clone(entry))
            } else {
                None
            }
        })
    }

    pub fn snapshot(&self) -> InvertedCatalogSnapshot {
        let entries = self.entries.read();
        let mut snap = HashMap::with_capacity(entries.len());
        for (name, entry) in entries.iter() {
            snap.insert(
                name.clone(),
                (entry.meta.clone(), entry.document_snapshot()),
            );
        }
        InvertedCatalogSnapshot { entries: snap }
    }

    pub fn restore_snapshot(&self, snapshot: InvertedCatalogSnapshot) {
        let mut entries = HashMap::with_capacity(snapshot.entries.len());
        for (name, (meta, docs)) in snapshot.entries {
            let entry = Arc::new(ManagedInvertedIndex::new(
                meta.name.clone(),
                meta.table.clone(),
                meta.columns.clone(),
            ));
            entry.restore_documents(docs);
            entries.insert(name, entry);
        }
        *self.entries.write() = entries;
    }
}
