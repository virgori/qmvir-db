use dashmap::DashMap;
use serde_json::Value;
use std::fs;
use std::path::Path;

use crate::hub_engine::errors::PlanError;
use crate::hub_engine::types::TableMeta;

#[derive(Debug, Default)]
pub struct Catalog {
    tables: DashMap<String, TableMeta>,
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            tables: DashMap::new(),
        }
    }

    pub fn register_table(&self, meta: TableMeta) {
        self.tables.insert(meta.name.clone(), meta);
    }

    pub fn update_row_count(&self, table: &str, row_count: u64) {
        if let Some(mut v) = self.tables.get_mut(table) {
            v.row_count = row_count;
        }
    }

    pub fn get_table_meta(&self, table: &str) -> Result<TableMeta, PlanError> {
        self.tables
            .get(table)
            .map(|v| v.clone())
            .ok_or_else(|| PlanError::MissingTableMeta(table.to_string()))
    }

    /// Ingest row_count metadata from a JSON state file.
    /// Expected shape: {"tables": {"t1": {"row_count": 123}, ...}}
    pub fn ingest_row_counts_from_state<P: AsRef<Path>>(&self, path: P) -> Result<usize, String> {
        let content = fs::read_to_string(path.as_ref()).map_err(|e| e.to_string())?;
        let json: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let mut updated = 0usize;

        if let Some(tables) = json.get("tables").and_then(|v| v.as_object()) {
            for (name, tmeta) in tables {
                let Some(row_count) = tmeta.get("row_count").and_then(|v| v.as_u64()) else {
                    continue;
                };
                if let Some(mut existing) = self.tables.get_mut(name) {
                    existing.row_count = row_count;
                    updated += 1;
                }
            }
        }

        Ok(updated)
    }
}
