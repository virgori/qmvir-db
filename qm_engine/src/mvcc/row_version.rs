//! MVCC row version storage.
//!
//! Immutable versioned rows with efficient version chain traversal.
//! Each logical row maintains a chain of versions, with the head being
//! the latest version. Visibility filtering determines which version
//! a transaction should see.

use crate::gateway::native_sql::NativeRow;
use ahash::AHashMap;
use std::sync::Arc;

/// Unique version identifier (globally unique within table).
pub type VersionId = u64;

/// Logical row ID (stable across versions of same row).
pub type LogicalRowId = i64;

/// Immutable row version with ownership and visibility markers.
///
/// Each version:
/// - Is created by a specific transaction
/// - May be deleted by a specific transaction
/// - Points to the previous version in the chain
/// - Uses Arc<> for immutable payload sharing
#[derive(Clone, Debug)]
pub struct MvccRowVersion {
    /// Unique version ID (globally unique).
    pub version_id: VersionId,
    /// Logical row ID (multiple versions per logical row).
    pub logical_row_id: LogicalRowId,
    /// Transaction that created this version.
    pub created_by_tx: u64,
    /// Transaction that deleted this version (if any).
    pub deleted_by_tx: Option<u64>,
    /// Commit timestamp when this version was created (None if uncommitted).
    pub created_commit_ts: Option<u64>,
    /// Commit timestamp when this version was deleted (if any).
    pub deleted_commit_ts: Option<u64>,
    /// Version ID of the previous version in the chain.
    pub previous_version: Option<VersionId>,
    /// Immutable row payload (shared via Arc for cheap clones).
    pub payload: Arc<NativeRow>,
}

impl MvccRowVersion {
    /// Create a new version.
    #[inline]
    pub fn new(
        version_id: VersionId,
        logical_row_id: LogicalRowId,
        created_by_tx: u64,
        payload: Arc<NativeRow>,
    ) -> Self {
        Self {
            version_id,
            logical_row_id,
            created_by_tx,
            deleted_by_tx: None,
            created_commit_ts: None,
            deleted_commit_ts: None,
            previous_version: None,
            payload,
        }
    }

    /// Mark this version as deleted by a transaction.
    #[inline]
    pub fn mark_deleted(&mut self, tx_id: u64) {
        self.deleted_by_tx = Some(tx_id);
    }

    /// Publish creation by committing.
    #[inline]
    pub fn publish_create(&mut self, commit_ts: u64) {
        self.created_commit_ts = Some(commit_ts);
    }

    /// Publish deletion by committing.
    #[inline]
    pub fn publish_delete(&mut self, commit_ts: u64) {
        self.deleted_commit_ts = Some(commit_ts);
    }

    /// Check if this version is created and potentially visible.
    #[inline]
    pub fn is_created(&self) -> bool {
        self.created_commit_ts.is_some()
    }

    /// Check if this version is deleted.
    #[inline]
    pub fn is_deleted(&self) -> bool {
        self.deleted_by_tx.is_some()
    }
}

/// MVCC-aware table storage using immutable versions.
///
/// Replaces the simple HashMap<i64, NativeRow> with versioned storage:
/// - `heads`: latest version ID for each logical row
/// - `versions`: all versions (committed and uncommitted)
/// - Version chains allow traversal back through history
pub struct MvccTable {
    /// Column names (same as original table).
    pub columns: Vec<String>,
    /// Column types (same as original table).
    pub column_types: Vec<crate::gateway::native_sql::ColType>,
    /// Latest version ID for each logical row.
    pub heads: AHashMap<LogicalRowId, VersionId>,
    /// All versions (committed and uncommitted).
    pub versions: AHashMap<VersionId, MvccRowVersion>,
    /// Next version ID to allocate.
    next_version_id: std::sync::atomic::AtomicU64,
    /// Foreign key constraints (same as original).
    pub foreign_keys: Vec<crate::gateway::native_sql::ForeignKey>,
    /// Per-column constraints (same as original).
    pub constraints: Vec<crate::gateway::native_sql::ColumnConstraint>,
    /// Table-level CHECK constraints.
    pub table_checks: Vec<String>,
}

impl MvccTable {
    /// Create a new MVCC table.
    pub fn new(
        columns: Vec<String>,
        column_types: Vec<crate::gateway::native_sql::ColType>,
    ) -> Self {
        let n = columns.len();
        Self {
            columns,
            column_types,
            heads: AHashMap::new(),
            versions: AHashMap::new(),
            next_version_id: std::sync::atomic::AtomicU64::new(1),
            foreign_keys: Vec::new(),
            constraints: (0..n)
                .map(|_| crate::gateway::native_sql::ColumnConstraint {
                    not_null: false,
                    unique: false,
                    default_value: None,
                    check_exprs: Vec::new(),
                })
                .collect(),
            table_checks: Vec::new(),
        }
    }

    /// Allocate a new version ID.
    #[inline]
    pub fn allocate_version_id(&self) -> VersionId {
        self.next_version_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Insert a new version for a logical row.
    ///
    /// Creates the row if it doesn't exist, or chains the version to the existing head.
    pub fn insert_version(&mut self, mut version: MvccRowVersion) {
        let row_id = version.logical_row_id;

        // Link to previous version if one exists
        if let Some(prev_head) = self.heads.get(&row_id) {
            version.previous_version = Some(*prev_head);
        }

        let version_id = version.version_id;
        self.versions.insert(version_id, version);
        self.heads.insert(row_id, version_id);
    }

    /// Get the head version for a logical row (without visibility filtering).
    #[inline]
    pub fn get_head_version(&self, row_id: LogicalRowId) -> Option<&MvccRowVersion> {
        self.heads
            .get(&row_id)
            .and_then(|version_id| self.versions.get(version_id))
    }

    /// Get a specific version by ID.
    #[inline]
    pub fn get_version(&self, version_id: VersionId) -> Option<&MvccRowVersion> {
        self.versions.get(&version_id)
    }

    /// Get mutable reference to a version (for updating state).
    #[inline]
    pub fn get_version_mut(&mut self, version_id: VersionId) -> Option<&mut MvccRowVersion> {
        self.versions.get_mut(&version_id)
    }

    /// Traverse version chain for a row.
    ///
    /// Yields versions from newest to oldest.
    pub fn version_chain(&self, row_id: LogicalRowId) -> VersionChainIterator<'_> {
        let start_version_id = self.heads.get(&row_id).copied();
        VersionChainIterator {
            table: self,
            current_version_id: start_version_id,
        }
    }

    /// Count versions for a logical row (useful for metrics).
    pub fn version_count(&self, row_id: LogicalRowId) -> usize {
        self.version_chain(row_id).count()
    }

    /// Total number of versions in table.
    #[inline]
    pub fn total_versions(&self) -> usize {
        self.versions.len()
    }

    /// Reclaim dead versions (for vacuum).
    ///
    /// Removes versions that are:
    /// - Aborted (no commit_ts)
    /// - Deleted and older than oldest_active_snapshot
    ///
    /// Does NOT remove versions that are the head of a chain or visible to active snapshots.
    pub fn reclaim_versions(&mut self, oldest_active_snapshot: u64) {
        let mut to_remove = Vec::new();

        for (version_id, version) in &self.versions {
            // Don't remove heads
            if self.heads.values().any(|h| h == version_id) {
                continue;
            }

            // Remove aborted created versions
            if version.created_commit_ts.is_none() && version.deleted_commit_ts.is_none() {
                to_remove.push(*version_id);
                continue;
            }

            // Remove deleted versions older than oldest_active_snapshot
            if let Some(delete_ts) = version.deleted_commit_ts {
                if delete_ts < oldest_active_snapshot {
                    to_remove.push(*version_id);
                }
            }
        }

        for version_id in to_remove {
            self.versions.remove(&version_id);
        }
    }
}

/// Iterator over version chain (newest to oldest).
pub struct VersionChainIterator<'a> {
    table: &'a MvccTable,
    current_version_id: Option<VersionId>,
}

impl<'a> Iterator for VersionChainIterator<'a> {
    type Item = &'a MvccRowVersion;

    fn next(&mut self) -> Option<Self::Item> {
        let version = self
            .current_version_id
            .and_then(|vid| self.table.get_version(vid))?;
        self.current_version_id = version.previous_version;
        Some(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_row() -> Arc<NativeRow> {
        use crate::gateway::native_sql::Cell;
        let mut cols = std::collections::HashMap::new();
        cols.insert("id".to_string(), Cell::Int(1));
        cols.insert("name".to_string(), Cell::Text("test".to_string()));

        Arc::new(NativeRow {
            cols,
            last_modified_lsn: 0,
        })
    }

    #[test]
    fn test_mvcc_table_insert_version() {
        let mut table = MvccTable::new(
            vec!["id".to_string(), "name".to_string()],
            vec![
                crate::gateway::native_sql::ColType::Integer,
                crate::gateway::native_sql::ColType::Text,
            ],
        );

        let payload = create_test_row();
        let version = MvccRowVersion::new(1, 100, 1, payload);
        table.insert_version(version);

        assert_eq!(table.total_versions(), 1);
        assert!(table.get_head_version(100).is_some());
    }

    #[test]
    fn test_version_chain() {
        let mut table = MvccTable::new(
            vec!["id".to_string(), "name".to_string()],
            vec![
                crate::gateway::native_sql::ColType::Integer,
                crate::gateway::native_sql::ColType::Text,
            ],
        );

        let payload1 = create_test_row();
        let version1 = MvccRowVersion::new(1, 100, 1, payload1);
        table.insert_version(version1);

        let payload2 = create_test_row();
        let mut version2 = MvccRowVersion::new(2, 100, 1, payload2);
        version2.previous_version = Some(1);
        table.insert_version(version2);

        let chain: Vec<_> = table.version_chain(100).collect();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].version_id, 2);
        assert_eq!(chain[1].version_id, 1);
    }

    #[test]
    fn test_version_chain_count() {
        let mut table = MvccTable::new(
            vec!["id".to_string()],
            vec![crate::gateway::native_sql::ColType::Integer],
        );

        let payload = create_test_row();
        let version = MvccRowVersion::new(1, 100, 1, payload);
        table.insert_version(version);

        assert_eq!(table.version_count(100), 1);
        assert_eq!(table.version_count(999), 0);
    }
}
