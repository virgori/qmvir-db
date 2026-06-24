/*
 * Adaptive JSON path index — hash lookup on extracted JSON path values.
 *
 * Supports:
 *   CREATE INDEX idx ON t (data) USING json_path('name')
 *   WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'alice'
 */

use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct JsonPathKey {
    pub table: String,
    pub column: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct JsonPathIndexMeta {
    pub name: String,
    pub key: JsonPathKey,
}

pub struct ManagedJsonPathIndex {
    pub meta: JsonPathIndexMeta,
    postings: RwLock<HashMap<String, Vec<i64>>>,
}

impl ManagedJsonPathIndex {
    pub fn new(name: String, table: String, column: String, path: String) -> Self {
        Self {
            meta: JsonPathIndexMeta {
                name,
                key: JsonPathKey {
                    table,
                    column,
                    path,
                },
            },
            postings: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, row_id: i64, value: &str) {
        if value.is_empty() {
            return;
        }
        let mut postings = self.postings.write();
        let rows = postings.entry(value.to_string()).or_default();
        if !rows.contains(&row_id) {
            rows.push(row_id);
        }
    }

    pub fn remove(&self, row_id: i64, value: &str) {
        let mut postings = self.postings.write();
        if let Some(rows) = postings.get_mut(value) {
            rows.retain(|id| *id != row_id);
            if rows.is_empty() {
                postings.remove(value);
            }
        }
    }

    pub fn lookup(&self, value: &str) -> Vec<i64> {
        self.postings
            .read()
            .get(value)
            .cloned()
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        self.postings.write().clear();
    }

    pub fn snapshot_postings(&self) -> HashMap<String, Vec<i64>> {
        self.postings.read().clone()
    }

    pub fn restore_postings(&self, postings: HashMap<String, Vec<i64>>) {
        *self.postings.write() = postings;
    }
}

#[derive(Clone)]
pub struct JsonPathCatalogSnapshot {
    pub entries: HashMap<String, (JsonPathIndexMeta, HashMap<String, Vec<i64>>)>,
}

pub struct JsonPathCatalog {
    by_name: RwLock<HashMap<String, Arc<ManagedJsonPathIndex>>>,
    by_key: RwLock<HashMap<JsonPathKey, Arc<ManagedJsonPathIndex>>>,
}

impl JsonPathCatalog {
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
        path: String,
    ) -> Arc<ManagedJsonPathIndex> {
        let entry = Arc::new(ManagedJsonPathIndex::new(
            name.clone(),
            table.clone(),
            column.clone(),
            path.clone(),
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

    pub fn find(&self, table: &str, column: &str, path: &str) -> Option<Arc<ManagedJsonPathIndex>> {
        self.by_key
            .read()
            .get(&JsonPathKey {
                table: table.to_string(),
                column: column.to_string(),
                path: path.to_string(),
            })
            .cloned()
    }

    pub fn indexes_for_table(&self, table: &str) -> Vec<Arc<ManagedJsonPathIndex>> {
        self.by_name
            .read()
            .values()
            .filter(|entry| entry.meta.key.table == table)
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> JsonPathCatalogSnapshot {
        let entries = self
            .by_name
            .read()
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    (entry.meta.clone(), entry.snapshot_postings()),
                )
            })
            .collect();
        JsonPathCatalogSnapshot { entries }
    }

    pub fn restore_snapshot(&self, snapshot: JsonPathCatalogSnapshot) {
        let mut by_name = HashMap::with_capacity(snapshot.entries.len());
        let mut by_key = HashMap::with_capacity(snapshot.entries.len());
        for (name, (meta, postings)) in snapshot.entries {
            let entry = Arc::new(ManagedJsonPathIndex::new(
                meta.name.clone(),
                meta.key.table.clone(),
                meta.key.column.clone(),
                meta.key.path.clone(),
            ));
            entry.restore_postings(postings);
            by_key.insert(entry.meta.key.clone(), Arc::clone(&entry));
            by_name.insert(name, entry);
        }
        *self.by_name.write() = by_name;
        *self.by_key.write() = by_key;
    }
}

pub fn extract_json_path_text(json_str: &str, path: &str) -> Option<String> {
    let val = serde_json::from_str::<Value>(json_str).ok()?;
    let mut current = &val;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        current = current.get(segment)?;
    }
    json_value_to_text(current)
}

fn json_value_to_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| n.as_f64().map(|f| f.to_string())),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}
