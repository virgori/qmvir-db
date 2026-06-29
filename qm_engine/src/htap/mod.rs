//! Production HTAP layer — MVCC visibility, column/row segments, planner, spill, PITR.
//!
//! Integrates OLTP row paths with OLAP column segments under a unified snapshot.

pub mod certify;
pub mod column_segment;
pub mod columnizer;
pub mod isolation;
pub mod mvcc_store;
pub mod pitr;
pub mod planner;
pub mod row_segment;
pub mod spill;

use crate::mvcc::TransactionManager;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use certify::{evaluate_htap_certification, evaluate_htap_engine, HtapCertificationReport};
pub use isolation::{run_htap_isolation_battery, IsolationReport, IsolationViolation};
pub use column_segment::ColumnSegmentStore;
pub use columnizer::Columnizer;
pub use mvcc_store::TableMvccStore;
pub use pitr::{
    materialize_pitr_data_dir, replay_wal_to_lsn, restore_to_timestamp, wal_lines_to_replay,
    PitrManifest, WalArchiveIndex,
};
pub use planner::{HtapPhysicalPlan, HtapPlanner, ScanPath};
pub use row_segment::RowSegmentStore;
pub use spill::SpillArena;

/// Shared HTAP runtime attached to each `NativeSqlEngine`.
#[derive(Clone)]
pub struct HtapRuntime {
    pub mvcc: Arc<TableMvccStore>,
    pub row_segments: Arc<RowSegmentStore>,
    pub column_segments: Arc<ColumnSegmentStore>,
    pub columnizer: Arc<Columnizer>,
    pub planner: Arc<HtapPlanner>,
    pub spill: Arc<SpillArena>,
    pub tx_mgr: Arc<TransactionManager>,
    pub data_dir: Option<PathBuf>,
    /// Tables with row changes not yet reflected in column segments.
    column_dirty: Arc<RwLock<HashSet<String>>>,
    /// Cached WAL archive index (avoid load+parse on every commit).
    wal_archive: Arc<RwLock<WalArchiveIndex>>,
    commit_seq: Arc<AtomicU64>,
    wal_archive_dirty: Arc<AtomicU64>,
}

impl HtapRuntime {
    pub fn new_in_memory() -> Self {
        let tx_mgr = Arc::new(TransactionManager::new());
        Self {
            mvcc: Arc::new(TableMvccStore::new(Arc::clone(&tx_mgr))),
            row_segments: Arc::new(RowSegmentStore::new(None)),
            column_segments: Arc::new(ColumnSegmentStore::new(None)),
            columnizer: Arc::new(Columnizer::new()),
            planner: Arc::new(HtapPlanner::new()),
            spill: Arc::new(SpillArena::new(None)),
            tx_mgr,
            data_dir: None,
            column_dirty: Arc::new(RwLock::new(HashSet::new())),
            wal_archive: Arc::new(RwLock::new(WalArchiveIndex {
                entries: Vec::new(),
            })),
            commit_seq: Arc::new(AtomicU64::new(0)),
            wal_archive_dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        let wal_archive = Arc::new(RwLock::new(WalArchiveIndex::load(&data_dir)));
        let tx_mgr = Arc::new(TransactionManager::new());
        Self {
            mvcc: Arc::new(TableMvccStore::new(Arc::clone(&tx_mgr))),
            row_segments: Arc::new(RowSegmentStore::new(Some(data_dir.join("row_segments")))),
            column_segments: Arc::new(ColumnSegmentStore::new(Some(
                data_dir.join("column_segments"),
            ))),
            columnizer: Arc::new(Columnizer::new()),
            planner: Arc::new(HtapPlanner::new()),
            spill: Arc::new(SpillArena::new(Some(data_dir.join("spill")))),
            tx_mgr,
            data_dir: Some(data_dir),
            column_dirty: Arc::new(RwLock::new(HashSet::new())),
            wal_archive,
            commit_seq: Arc::new(AtomicU64::new(0)),
            wal_archive_dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn after_commit(
        &self,
        commit_ts: u64,
        wal_path: &Path,
        wal_bytes: u64,
        oldest_snapshot: u64,
    ) {
        self.after_commit_profiled(
            commit_ts,
            wal_path,
            wal_bytes,
            oldest_snapshot,
            None,
            None,
            false,
            false,
        );
    }

    pub fn after_commit_profiled(
        &self,
        commit_ts: u64,
        wal_path: &Path,
        wal_bytes: u64,
        oldest_snapshot: u64,
        archive_ns: Option<&std::sync::atomic::AtomicU64>,
        vacuum_ns: Option<&std::sync::atomic::AtomicU64>,
        skip_archive: bool,
        skip_vacuum: bool,
    ) {
        let seq = self.commit_seq.fetch_add(1, Ordering::Relaxed);
        if let Some(ref dir) = self.data_dir {
            if !skip_archive {
                let archive_start = archive_ns.map(|_| std::time::Instant::now());
                let wall_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let mut idx = self.wal_archive.write();
                idx.record_with_bytes(commit_ts, wall_time, wal_path, wal_bytes);
                self.wal_archive_dirty.store(1, Ordering::Relaxed);
                if seq % 64 == 0 {
                    let _ = idx.save(dir);
                    self.wal_archive_dirty.store(0, Ordering::Relaxed);
                }
                if let (Some(counter), Some(start)) = (archive_ns, archive_start) {
                    counter.fetch_add(
                        start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                        Ordering::Relaxed,
                    );
                }
            }
        }
        if !skip_vacuum && seq % 32 == 0 {
            let vacuum_start = vacuum_ns.map(|_| std::time::Instant::now());
            self.mvcc.vacuum(oldest_snapshot);
            if let (Some(counter), Some(start)) = (vacuum_ns, vacuum_start) {
                counter.fetch_add(
                    start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                    Ordering::Relaxed,
                );
            }
        }
    }

    pub fn flush_wal_archive(&self) {
        if self.wal_archive_dirty.load(Ordering::Relaxed) == 0 {
            return;
        }
        if let Some(ref dir) = self.data_dir {
            let idx = self.wal_archive.read();
            let _ = idx.save(dir);
            self.wal_archive_dirty.store(0, Ordering::Relaxed);
        }
    }

    pub fn wal_archive_snapshot(&self) -> WalArchiveIndex {
        self.wal_archive.read().clone()
    }

    pub fn maybe_vacuum_mvcc(&self, oldest_snapshot: u64) {
        let seq = self.commit_seq.load(Ordering::Relaxed);
        if seq % 32 == 0 {
            self.mvcc.vacuum(oldest_snapshot);
        }
    }

    pub fn mark_column_dirty(&self, table: &str) {
        self.column_dirty.write().insert(table.to_string());
    }

    pub fn is_column_dirty(&self, table: &str) -> bool {
        self.column_dirty.read().contains(table)
    }

    pub fn clear_column_dirty(&self, table: &str) {
        self.column_dirty.write().remove(table);
    }

    pub fn register_session(&self) -> u64 {
        let id = self.tx_mgr.allocate_session_id();
        self.tx_mgr.register_session(id);
        id
    }

    /// Implicit per-statement transaction for autocommit mutations (real commit, not fake).
    pub fn autocommit_scope(
        &self,
        session_id: u64,
        f: impl FnOnce(u64) -> Result<(), String>,
    ) -> Result<(), String> {
        let tx_id = self.tx_mgr.begin_transaction(session_id)?;
        let result = f(tx_id);
        match result {
            Ok(()) => {
                let commit_ts = self.tx_mgr.commit_transaction(tx_id)?;
                self.mvcc.publish_transaction(tx_id, commit_ts)?;
                Ok(())
            }
            Err(e) => {
                let _ = self.tx_mgr.abort_transaction(tx_id);
                self.mvcc.abort_transaction(tx_id);
                Err(e)
            }
        }
    }

    pub fn bootstrap_table_engine(
        &self,
        tables: &crate::gateway::table_store::TableStore,
        name: &str,
    ) {
        if self.mvcc.tables.contains_key(name) {
            return;
        }
        if let Ok(shared) = tables.lock_table_read(name) {
            self.mvcc.bootstrap_table(name, &shared.read());
        }
    }
}

impl crate::gateway::native_sql::NativeSqlEngine {
    /// MVCC-visible row lookup (replaces direct `t.rows.get` on hot paths).
    pub fn htap_visible_row(
        &self,
        table: &str,
        row_id: i64,
    ) -> Option<std::sync::Arc<crate::gateway::native_sql::NativeRow>> {
        self.htap.bootstrap_table_engine(&self.tables, table);
        self.htap
            .mvcc
            .visible_row(&self.htap.tx_mgr, self.session_id, table, row_id)
    }

    /// Row count respecting MVCC visibility.
    pub fn htap_visible_row_count(&self, table: &str) -> usize {
        self.htap.bootstrap_table_engine(&self.tables, table);
        self.htap
            .mvcc
            .visible_row_count(&self.htap.tx_mgr, self.session_id, table)
    }

    /// Visible primary keys for table scans without cloning the catalog.
    pub fn htap_visible_row_ids(&self, table: &str) -> Vec<i64> {
        self.htap.bootstrap_table_engine(&self.tables, table);
        let ids = self
            .htap
            .mvcc
            .visible_row_ids(&self.htap.tx_mgr, self.session_id, table);
        if !ids.is_empty() {
            return ids;
        }
        self.tables
            .with_read_opt(table, |t| t.rows.keys().copied().collect())
            .unwrap_or_default()
    }
}
