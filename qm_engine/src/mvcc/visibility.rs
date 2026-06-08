//! Visibility filtering for MVCC row versions.
//!
//! Fast path for determining row version visibility to a snapshot.
//! Optimized for branch predictability and cache locality.

use super::row_version::MvccRowVersion;
pub use super::tx_manager::{Isolation, Snapshot};

/// Determine if a transaction is committed before a given timestamp.
#[inline]
pub fn is_tx_committed_before(
    tx_id: u64,
    ts: u64,
    registry: &std::collections::HashMap<u64, super::tx_manager::TransactionRecord>,
) -> bool {
    match registry.get(&tx_id) {
        Some(record) => matches!(
            &record.state,
            super::tx_manager::TxState::Committed { commit_ts } if *commit_ts <= ts
        ),
        None => false,
    }
}

/// Fast visibility check for a row version against a snapshot.
///
/// Returns true if the version should be visible to the snapshot.
/// Optimized for inline and branch prediction.
///
/// Rules:
/// - Own transaction writes always visible
/// - Created after snapshot → invisible
/// - Aborted creates → invisible
/// - Deleted before snapshot → invisible
/// - Deleted after snapshot (by other tx) → visible (old version)
#[inline(always)]
pub fn is_visible(
    version: &MvccRowVersion,
    snapshot: &Snapshot,
    registry: &std::collections::HashMap<u64, super::tx_manager::TransactionRecord>,
) -> bool {
    // FAST PATH: own writes always visible
    if version.created_by_tx == snapshot.own_tx_id {
        // Even own writes can be marked deleted by self
        if version.deleted_by_tx == Some(snapshot.own_tx_id) {
            return false;
        }
        return true;
    }

    if snapshot.active_tx_ids.contains(&version.created_by_tx) {
        return false;
    }

    // Check if version creation is visible.
    let created_visible = match version.created_commit_ts {
        Some(ts) => ts <= snapshot.read_ts,
        None => false, // Uncommitted by other tx → invisible
    };

    if !created_visible {
        return false;
    }

    // Check if creator was aborted
    if is_tx_aborted(version.created_by_tx, registry) {
        return false;
    }

    // Check deletion visibility
    match version.deleted_by_tx {
        None => true,                                                // Not deleted → visible
        Some(delete_tx) if delete_tx == snapshot.own_tx_id => false, // We deleted it
        Some(delete_tx) if snapshot.active_tx_ids.contains(&delete_tx) => {
            true // Delete was active at snapshot time, so old version remains visible
        }
        Some(delete_tx) if is_tx_aborted(delete_tx, registry) => {
            true // Delete was aborted, row is live
        }
        Some(_) => {
            // Deleted by other tx; visible only if delete is after snapshot
            match version.deleted_commit_ts {
                Some(delete_ts) => delete_ts > snapshot.read_ts, // Delete after snapshot
                None => true, // Uncommitted delete → treat as not deleted
            }
        }
    }
}

/// Find the first visible version for a logical row in the version chain.
///
/// Traverses the chain from head to tail (newest to oldest) and returns
/// the first version that is visible according to the snapshot.
pub fn visible_version_for_row<'a>(
    versions: &'a std::collections::HashMap<u64, MvccRowVersion>,
    heads: &std::collections::HashMap<i64, u64>,
    row_id: i64,
    snapshot: &Snapshot,
    registry: &std::collections::HashMap<u64, super::tx_manager::TransactionRecord>,
) -> Option<&'a MvccRowVersion> {
    let mut current_version_id = heads.get(&row_id).copied();

    while let Some(version_id) = current_version_id {
        let version = versions.get(&version_id)?;
        if is_visible(version, snapshot, registry) {
            return Some(version);
        }
        current_version_id = version.previous_version;
    }

    None
}

/// Check if a transaction is aborted.
#[inline]
fn is_tx_aborted(
    tx_id: u64,
    registry: &std::collections::HashMap<u64, super::tx_manager::TransactionRecord>,
) -> bool {
    matches!(
        registry.get(&tx_id).map(|r| &r.state),
        Some(super::tx_manager::TxState::Aborted)
    )
}

/// Collection of visible row IDs for a snapshot (for query result filtering).
pub struct VisibleRowIds {
    pub row_ids: Vec<i64>,
}

impl VisibleRowIds {
    /// Create visible row ID set by scanning all versions.
    pub fn from_versions(
        versions: &std::collections::HashMap<u64, MvccRowVersion>,
        heads: &std::collections::HashMap<i64, u64>,
        snapshot: &Snapshot,
        registry: &std::collections::HashMap<u64, super::tx_manager::TransactionRecord>,
    ) -> Self {
        let mut visible = std::collections::HashSet::new();

        for &row_id in heads.keys() {
            if visible_version_for_row(versions, heads, row_id, snapshot, registry).is_some() {
                visible.insert(row_id);
            }
        }

        Self {
            row_ids: visible.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::native_sql::{Cell, NativeRow};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn create_snapshot(read_ts: u64, own_tx_id: u64) -> Snapshot {
        Snapshot {
            read_ts,
            own_tx_id,
            active_tx_ids: HashSet::new(),
            isolation: Isolation::ReadCommitted,
        }
    }

    fn create_version(
        version_id: u64,
        logical_row_id: i64,
        created_by_tx: u64,
        created_commit_ts: Option<u64>,
    ) -> MvccRowVersion {
        let mut cols = std::collections::HashMap::new();
        cols.insert("id".to_string(), Cell::Int(1));

        let mut version = MvccRowVersion::new(
            version_id,
            logical_row_id,
            created_by_tx,
            Arc::new(NativeRow {
                cols,
                last_modified_lsn: 0,
            }),
        );
        version.created_commit_ts = created_commit_ts;
        version
    }

    #[test]
    fn test_visibility_own_write() {
        let snapshot = create_snapshot(10, 1);
        let version = create_version(1, 100, 1, Some(5));
        let registry = HashMap::new();

        assert!(is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_visibility_committed_before_snapshot() {
        let snapshot = create_snapshot(10, 2);
        let version = create_version(1, 100, 1, Some(5));
        let registry = HashMap::new();

        assert!(is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_visibility_committed_after_snapshot() {
        let snapshot = create_snapshot(10, 2);
        let version = create_version(1, 100, 1, Some(15));
        let registry = HashMap::new();

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_visibility_uncommitted() {
        let snapshot = create_snapshot(10, 2);
        let version = create_version(1, 100, 1, None);
        let registry = HashMap::new();

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_visibility_deleted_by_self() {
        let snapshot = create_snapshot(10, 1);
        let mut version = create_version(1, 100, 1, Some(5));
        version.mark_deleted(1);
        version.deleted_commit_ts = Some(7);
        let registry = HashMap::new();

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_visibility_deleted_before_snapshot() {
        let snapshot = create_snapshot(10, 2);
        let mut version = create_version(1, 100, 1, Some(5));
        version.mark_deleted(3);
        version.deleted_commit_ts = Some(7);
        let registry = HashMap::new();

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_visibility_deleted_after_snapshot() {
        let snapshot = create_snapshot(10, 2);
        let mut version = create_version(1, 100, 1, Some(5));
        version.mark_deleted(3);
        version.deleted_commit_ts = Some(15);
        let registry = HashMap::new();

        // Deleted after snapshot, so old version should be visible
        assert!(is_visible(&version, &snapshot, &registry));
    }
}
