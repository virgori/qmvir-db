//! Table-level MVCC store — version chains with real visibility on reads/writes.

use crate::gateway::native_sql::{NativeRow, NativeTable};
use crate::mvcc::row_version::{LogicalRowId, MvccRowVersion, MvccTable, VersionId};
use crate::mvcc::tx_manager::TransactionManager;
use crate::mvcc::visibility::{self, Snapshot};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct TableMvccStore {
    pub(crate) tables: DashMap<String, Arc<RwLock<MvccTable>>>,
    tx_mgr: Arc<TransactionManager>,
    /// Pending uncommitted versions keyed by tx_id → (table, row_id, version_id).
    pending: DashMap<u64, Vec<(String, LogicalRowId, VersionId)>>,
    /// Deferred `touch_table` / `add_write` — flushed once per commit.
    deferred_touches: DashMap<u64, HashSet<String>>,
}

impl TableMvccStore {
    pub fn new(tx_mgr: Arc<TransactionManager>) -> Self {
        Self {
            tables: DashMap::new(),
            tx_mgr,
            pending: DashMap::new(),
            deferred_touches: DashMap::new(),
        }
    }

    fn ensure_table(&self, name: &str, native: &NativeTable) -> Arc<RwLock<MvccTable>> {
        if let Some(existing) = self.tables.get(name) {
            return Arc::clone(existing.value());
        }
        let mut mvcc = MvccTable::new(native.columns.clone(), native.column_types.clone());
        mvcc.foreign_keys = native.foreign_keys.clone();
        mvcc.constraints = native.constraints.clone();
        mvcc.table_checks = native.table_checks.clone();
        for (&row_id, row) in &native.rows {
            let vid = mvcc.allocate_version_id();
            let mut version = MvccRowVersion::new(vid, row_id, 0, Arc::new(row.clone()));
            version.created_commit_ts = Some(0);
            mvcc.insert_version(version);
        }
        let arc = Arc::new(RwLock::new(mvcc));
        self.tables.insert(name.to_string(), Arc::clone(&arc));
        arc
    }

    pub fn bootstrap_table(&self, name: &str, native: &NativeTable) {
        let _ = self.ensure_table(name, native);
    }

    pub fn rebuild_table(&self, name: &str, native: &NativeTable) {
        self.tables.remove(name);
        self.bootstrap_table(name, native);
    }

    pub fn statement_snapshot(
        &self,
        tx_mgr: &TransactionManager,
        session_id: u64,
    ) -> Snapshot {
        if let Ok(snap) = tx_mgr.acquire_statement_snapshot(session_id) {
            return snap;
        }
        // Autocommit read: see all committed versions as of now.
        let read_ts = tx_mgr.current_commit_ts();
        Snapshot::read_committed(read_ts, 0, tx_mgr.get_active_tx_ids())
    }

    fn registry_map(&self) -> ahash::AHashMap<u64, crate::mvcc::tx_manager::TransactionRecord> {
        self.tx_mgr.registry_snapshot()
    }

    pub fn visible_row(
        &self,
        tx_mgr: &TransactionManager,
        session_id: u64,
        table: &str,
        row_id: i64,
    ) -> Option<Arc<NativeRow>> {
        let shared = self.tables.get(table)?;
        let mvcc = shared.read();
        if !mvcc.heads.contains_key(&row_id) {
            return None;
        }
        let snapshot = self.statement_snapshot(tx_mgr, session_id);
        let registry = self.registry_map();
        let version = visibility::visible_version_for_row(
            &mvcc.versions,
            &mvcc.heads,
            row_id,
            &snapshot,
            &registry,
        )?;
        Some(Arc::clone(&version.payload))
    }

    pub fn visible_row_count(
        &self,
        tx_mgr: &TransactionManager,
        session_id: u64,
        table: &str,
    ) -> usize {
        let Some(shared) = self.tables.get(table) else {
            return 0;
        };
        let mvcc = shared.read();
        let snapshot = self.statement_snapshot(tx_mgr, session_id);
        let registry = self.registry_map();
        mvcc.heads
            .keys()
            .filter(|row_id| {
                visibility::visible_version_for_row(
                    &mvcc.versions,
                    &mvcc.heads,
                    **row_id,
                    &snapshot,
                    &registry,
                )
                .is_some()
            })
            .count()
    }

    /// Visible row ids for scan paths (avoids cloning full table map).
    pub fn visible_row_ids(
        &self,
        tx_mgr: &TransactionManager,
        session_id: u64,
        table: &str,
    ) -> Vec<i64> {
        let Some(shared) = self.tables.get(table) else {
            return Vec::new();
        };
        let mvcc = shared.read();
        let snapshot = self.statement_snapshot(tx_mgr, session_id);
        let registry = self.registry_map();
        let mut ids: Vec<i64> = mvcc
            .heads
            .keys()
            .filter_map(|row_id| {
                visibility::visible_version_for_row(
                    &mvcc.versions,
                    &mvcc.heads,
                    *row_id,
                    &snapshot,
                    &registry,
                )
                .map(|_| *row_id)
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Write a row version inside an active transaction.
    pub fn upsert_row(
        &self,
        tx_id: u64,
        table: &str,
        native: &NativeTable,
        row_id: i64,
        row: NativeRow,
    ) -> Result<(), String> {
        let shared = self.ensure_table(table, native);
        let mut mvcc = shared.write();
        let vid = mvcc.allocate_version_id();
        let version = MvccRowVersion::new(vid, row_id, tx_id, Arc::new(row));
        mvcc.insert_version(version);
        self.pending
            .entry(tx_id)
            .or_default()
            .push((table.to_string(), row_id, vid));
        self.deferred_touches
            .entry(tx_id)
            .or_default()
            .insert(table.to_string());
        Ok(())
    }

    pub fn delete_row(
        &self,
        tx_id: u64,
        table: &str,
        native: &NativeTable,
        row_id: i64,
    ) -> Result<(), String> {
        let shared = self.ensure_table(table, native);
        let mut mvcc = shared.write();
        let head = mvcc
            .get_head_version(row_id)
            .ok_or_else(|| format!("row {} not found in {}", row_id, table))?;
        let head_id = head.version_id;
        if let Some(v) = mvcc.get_version_mut(head_id) {
            v.mark_deleted(tx_id);
        }
        self.pending
            .entry(tx_id)
            .or_default()
            .push((table.to_string(), row_id, head_id));
        self.deferred_touches
            .entry(tx_id)
            .or_default()
            .insert(table.to_string());
        Ok(())
    }

    pub fn apply_deferred_touches(&self, tx_id: u64) {
        let Some((_, tables)) = self.deferred_touches.remove(&tx_id) else {
            return;
        };
        for table in tables {
            let _ = self.tx_mgr.touch_table(tx_id, table);
        }
    }

    pub fn publish_transaction(&self, tx_id: u64, commit_ts: u64) -> Result<(), String> {
        let pending = self.pending.remove(&tx_id).map(|(_, v)| v).unwrap_or_default();
        for (table, row_id, version_id) in &pending {
            if let Some(shared) = self.tables.get(table) {
                let mut mvcc = shared.write();
                if let Some(v) = mvcc.get_version_mut(*version_id) {
                    if v.deleted_by_tx == Some(tx_id) {
                        v.publish_delete(commit_ts);
                    } else if v.created_by_tx == tx_id {
                        v.publish_create(commit_ts);
                    }
                }
                let _ = row_id;
            }
        }
        Ok(())
    }

    pub fn abort_transaction(&self, tx_id: u64) {
        let Some(pending) = self.pending.remove(&tx_id).map(|(_, v)| v) else {
            return;
        };
        for (table, row_id, version_id) in pending {
            if let Some(shared) = self.tables.get(&table) {
                let mut mvcc = shared.write();
                if let Some(head) = mvcc.heads.get(&row_id).copied() {
                    if head == version_id {
                        if let Some(prev) = mvcc
                            .get_version(version_id)
                            .and_then(|v| v.previous_version)
                        {
                            mvcc.heads.insert(row_id, prev);
                        } else {
                            mvcc.heads.remove(&row_id);
                        }
                    }
                }
                mvcc.versions.remove(&version_id);
            }
        }
    }

    /// Materialize committed visible rows into `NativeTable.rows` after commit.
    pub fn sync_native_table(
        &self,
        tx_mgr: &TransactionManager,
        session_id: u64,
        table: &str,
        native: &mut NativeTable,
    ) {
        let Some(shared) = self.tables.get(table) else {
            return;
        };
        let mvcc = shared.read();
        let snapshot = Snapshot::read_committed(
            tx_mgr.current_commit_ts(),
            0,
            tx_mgr.get_active_tx_ids(),
        );
        let registry = self.registry_map();
        native.rows.clear();
        for &row_id in mvcc.heads.keys() {
            if let Some(v) = visibility::visible_version_for_row(
                &mvcc.versions,
                &mvcc.heads,
                row_id,
                &snapshot,
                &registry,
            ) {
                native.rows.insert(row_id, (*v.payload).clone());
            }
        }
    }

    pub fn vacuum(&self, oldest_snapshot: u64) {
        for entry in self.tables.iter() {
            entry.value().write().reclaim_versions(oldest_snapshot);
        }
    }
}
