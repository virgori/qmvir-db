//! Comprehensive MVCC integration tests.
//!
//! These tests verify the core MVCC functionality:
//! - Multi-session transaction isolation
//! - Visibility correctness
//! - Read/Write conflict detection
//! - Concurrent operation safety

#[cfg(test)]
mod tests {
    use qm_engine::gateway::native_sql::{Cell, NativeRow};
    use qm_engine::mvcc::prelude::*;
    use qm_engine::mvcc::visibility::{is_visible, visible_version_for_row};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    /// Test helper: create transaction manager with sessions
    fn setup_mgr_with_sessions(num_sessions: usize) -> (TransactionManager, Vec<SessionId>) {
        let mgr = TransactionManager::new();
        let mut sessions = Vec::new();

        for i in 0..num_sessions {
            let sess_id = (i + 1) as SessionId;
            mgr.register_session(sess_id);
            sessions.push(sess_id);
        }

        (mgr, sessions)
    }

    fn test_row(id: i64, name: &str) -> Arc<NativeRow> {
        let mut cols = HashMap::new();
        cols.insert("id".to_string(), Cell::Int(id));
        cols.insert("name".to_string(), Cell::Text(name.to_string()));

        Arc::new(NativeRow {
            cols,
            last_modified_lsn: 0,
        })
    }

    fn test_version(
        version_id: VersionId,
        row_id: LogicalRowId,
        created_by_tx: TxId,
        commit_ts: Option<CommitTs>,
        name: &str,
    ) -> MvccRowVersion {
        let mut version =
            MvccRowVersion::new(version_id, row_id, created_by_tx, test_row(row_id, name));
        version.created_commit_ts = commit_ts;
        version
    }

    fn test_snapshot(read_ts: CommitTs, own_tx_id: TxId, isolation: Isolation) -> Snapshot {
        Snapshot {
            read_ts,
            own_tx_id,
            active_tx_ids: HashSet::new(),
            isolation,
        }
    }

    fn tx_record(tx_id: TxId, session_id: SessionId, state: TxState) -> TransactionRecord {
        TransactionRecord {
            tx_id,
            session_id,
            state,
            start_ts: 0,
            snapshot: None,
            touched_tables: HashSet::new(),
            write_set: Vec::new(),
        }
    }

    #[test]
    fn test_multi_session_isolation() {
        let (mgr, sessions) = setup_mgr_with_sessions(2);
        let sess1 = sessions[0];
        let sess2 = sessions[1];

        // Session 1: Begin transaction
        let tx1 = mgr.begin_transaction(sess1).unwrap();

        // Session 2: Begin transaction
        let tx2 = mgr.begin_transaction(sess2).unwrap();

        // Both should be active
        let active = mgr.get_active_tx_ids();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&tx1));
        assert!(active.contains(&tx2));

        // Session 1: Commit
        let ts1 = mgr.commit_transaction(tx1).unwrap();
        assert!(ts1 > 0);

        // Session 2: Still active
        let active = mgr.get_active_tx_ids();
        assert_eq!(active.len(), 1);
        assert!(active.contains(&tx2));

        // Session 2: Commit
        let ts2 = mgr.commit_transaction(tx2).unwrap();
        assert!(ts2 > ts1); // Timestamps are monotonic

        let active = mgr.get_active_tx_ids();
        assert_eq!(active.len(), 0);
    }

    #[test]
    fn test_concurrent_read_write() {
        let (mgr, sessions) = setup_mgr_with_sessions(2);
        let reader_sess = sessions[0];
        let writer_sess = sessions[1];

        // Reader: Begin
        let read_tx = mgr.begin_transaction(reader_sess).unwrap();
        let reader_snapshot = mgr.acquire_statement_snapshot(reader_sess).unwrap();
        assert_eq!(reader_snapshot.read_ts, 0);

        // Writer: Begin, write, commit
        let write_tx = mgr.begin_transaction(writer_sess).unwrap();
        mgr.touch_table(write_tx, "users".to_string()).unwrap();
        mgr.add_write(write_tx, "users".to_string(), 1).unwrap();
        let write_commit_ts = mgr.commit_transaction(write_tx).unwrap();

        // READ COMMITTED acquires a new statement snapshot and sees commits
        // published before that statement starts.
        let new_snapshot = mgr.acquire_statement_snapshot(reader_sess).unwrap();
        assert_eq!(new_snapshot.read_ts, write_commit_ts);

        // Reader can commit
        mgr.commit_transaction(read_tx).unwrap();
    }

    #[test]
    fn test_session_tx_context_lifecycle() {
        let mut ctx = SessionTxContext::new(42);
        assert!(!ctx.in_transaction());
        assert!(ctx.autocommit);
        assert_eq!(ctx.session_id, 42);

        // Begin transaction
        ctx.begin(100);
        assert!(ctx.in_transaction());
        assert!(!ctx.autocommit);
        assert_eq!(ctx.tx_id, Some(100));

        // End transaction
        ctx.end_tx();
        assert!(!ctx.in_transaction());
        assert_eq!(ctx.tx_id, None);
    }

    #[test]
    fn test_rollback_aborts_transaction() {
        let (mgr, sessions) = setup_mgr_with_sessions(1);
        let sess = sessions[0];

        let tx = mgr.begin_transaction(sess).unwrap();
        mgr.touch_table(tx, "users".to_string()).unwrap();

        mgr.abort_transaction(tx).unwrap();

        let record = mgr.get_transaction(tx).unwrap();
        assert!(matches!(record.state, TxState::Aborted));
    }

    #[test]
    fn test_oldest_active_snapshot_tracking() {
        let (mgr, sessions) = setup_mgr_with_sessions(3);

        let tx1 = mgr.begin_transaction(sessions[0]).unwrap();
        let start1 = mgr.get_transaction(tx1).unwrap().start_ts;

        // Artificial delay
        std::thread::sleep(std::time::Duration::from_micros(100));

        let tx2 = mgr.begin_transaction(sessions[1]).unwrap();
        let start2 = mgr.get_transaction(tx2).unwrap().start_ts;

        let _tx3 = mgr.begin_transaction(sessions[2]).unwrap();

        // Oldest should be tx1
        let oldest = mgr.oldest_active_snapshot();
        assert_eq!(oldest, start1);

        // Commit tx1
        mgr.commit_transaction(tx1).unwrap();

        // Oldest should now be tx2
        let oldest = mgr.oldest_active_snapshot();
        assert_eq!(oldest, start2);
    }

    #[test]
    fn test_cannot_begin_during_active_tx() {
        let (mgr, sessions) = setup_mgr_with_sessions(1);
        let sess = sessions[0];

        mgr.begin_transaction(sess).unwrap();

        // Try to begin again
        let result = mgr.begin_transaction(sess);
        assert!(result.is_err());
    }

    #[test]
    fn test_write_set_tracking() {
        let (mgr, sessions) = setup_mgr_with_sessions(1);
        let sess = sessions[0];

        let tx = mgr.begin_transaction(sess).unwrap();
        mgr.touch_table(tx, "users".to_string()).unwrap();
        mgr.add_write(tx, "users".to_string(), 1).unwrap();
        mgr.add_write(tx, "users".to_string(), 2).unwrap();
        mgr.add_write(tx, "orders".to_string(), 100).unwrap();

        let record = mgr.get_transaction(tx).unwrap();
        assert_eq!(record.touched_tables.len(), 2);
        assert_eq!(record.write_set.len(), 3);
        assert!(record.touched_tables.contains("users"));
        assert!(record.touched_tables.contains("orders"));
    }

    #[test]
    fn test_can_begin_new_transaction_after_commit_or_abort() {
        let (mgr, sessions) = setup_mgr_with_sessions(1);
        let sess = sessions[0];

        let tx1 = mgr.begin_transaction(sess).unwrap();
        mgr.commit_transaction(tx1).unwrap();

        let tx2 = mgr.begin_transaction(sess).unwrap();
        assert_ne!(tx1, tx2);
        mgr.abort_transaction(tx2).unwrap();

        let tx3 = mgr.begin_transaction(sess).unwrap();
        assert_ne!(tx2, tx3);
    }

    #[test]
    fn test_own_uncommitted_write_is_visible() {
        let version = test_version(1, 10, 7, None, "self");
        let snapshot = test_snapshot(0, 7, Isolation::ReadCommitted);
        let registry = HashMap::new();

        assert!(is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_other_uncommitted_write_is_invisible() {
        let version = test_version(1, 10, 7, None, "other");
        let snapshot = test_snapshot(0, 8, Isolation::ReadCommitted);
        let registry = HashMap::new();

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_snapshot_isolation_does_not_see_commit_after_snapshot() {
        let version = test_version(1, 10, 7, Some(2), "new");
        let snapshot = test_snapshot(1, 8, Isolation::SnapshotIsolation);
        let registry = HashMap::new();

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_rollback_create_leaves_no_visible_row() {
        let version = test_version(1, 10, 7, None, "rolled-back");
        let snapshot = test_snapshot(1, 8, Isolation::ReadCommitted);
        let mut registry = HashMap::new();
        registry.insert(7, tx_record(7, 1, TxState::Aborted));

        assert!(!is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_delete_rollback_restores_visibility() {
        let mut version = test_version(1, 10, 7, Some(1), "live");
        version.mark_deleted(8);

        let snapshot = test_snapshot(2, 9, Isolation::ReadCommitted);
        let mut registry = HashMap::new();
        registry.insert(8, tx_record(8, 2, TxState::Aborted));

        assert!(is_visible(&version, &snapshot, &registry));
    }

    #[test]
    fn test_update_preserves_old_version_for_old_snapshot() {
        let old_version = test_version(1, 10, 7, Some(1), "old");
        let mut new_version = test_version(2, 10, 8, Some(2), "new");
        new_version.previous_version = Some(1);

        let mut versions = HashMap::new();
        versions.insert(1, old_version);
        versions.insert(2, new_version);

        let mut heads = HashMap::new();
        heads.insert(10, 2);

        let registry = HashMap::new();
        let old_snapshot = test_snapshot(1, 9, Isolation::SnapshotIsolation);
        let visible_old =
            visible_version_for_row(&versions, &heads, 10, &old_snapshot, &registry).unwrap();
        assert_eq!(visible_old.version_id, 1);

        let new_snapshot = test_snapshot(2, 9, Isolation::ReadCommitted);
        let visible_new =
            visible_version_for_row(&versions, &heads, 10, &new_snapshot, &registry).unwrap();
        assert_eq!(visible_new.version_id, 2);
    }

    #[test]
    fn test_concurrent_write_conflict_detected() {
        let lock_mgr = LockManager::new();

        lock_mgr.acquire_table_lock("users", 1, 1).unwrap();
        let conflict = lock_mgr.acquire_table_lock("users", 2, 2);

        assert_eq!(conflict, Err(1));
    }

    #[test]
    fn test_commit_and_rollback_release_locks() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut exec1 = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());
        let tx1 = exec1.begin().unwrap();
        exec1.acquire_write_lock("users").unwrap();
        assert!(lock_mgr.has_locks_for_transaction(tx1));
        exec1.commit().unwrap();
        assert!(!lock_mgr.has_locks_for_transaction(tx1));

        let mut exec2 = MvccQueryExecutor::new(2, tx_mgr, lock_mgr.clone());
        let tx2 = exec2.begin().unwrap();
        exec2.acquire_write_lock("orders").unwrap();
        assert!(lock_mgr.has_locks_for_transaction(tx2));
        exec2.rollback().unwrap();
        assert!(!lock_mgr.has_locks_for_transaction(tx2));
    }

    #[test]
    fn test_multiple_commits_ordered() {
        let (mgr, sessions) = setup_mgr_with_sessions(3);

        let tx1 = mgr.begin_transaction(sessions[0]).unwrap();
        let tx2 = mgr.begin_transaction(sessions[1]).unwrap();
        let tx3 = mgr.begin_transaction(sessions[2]).unwrap();

        let ts1 = mgr.commit_transaction(tx1).unwrap();
        let ts2 = mgr.commit_transaction(tx2).unwrap();
        let ts3 = mgr.commit_transaction(tx3).unwrap();

        // Timestamps should be ordered
        assert!(ts1 < ts2);
        assert!(ts2 < ts3);
    }

    #[test]
    fn test_session_lifecycle() {
        let mgr = TransactionManager::new();

        let sess1 = mgr.register_session(1);
        let sess2 = mgr.register_session(2);

        assert_eq!(sess1.session_id, 1);
        assert_eq!(sess2.session_id, 2);

        // Can unregister
        mgr.unregister_session(1);

        // Can still register new session with same ID
        let sess1_new = mgr.register_session(1);
        assert_eq!(sess1_new.session_id, 1);
    }

    #[test]
    fn test_snapshot_includes_active_tx_ids() {
        let (mgr, sessions) = setup_mgr_with_sessions(3);

        let tx1 = mgr.begin_transaction(sessions[0]).unwrap();
        let tx2 = mgr.begin_transaction(sessions[1]).unwrap();
        let tx3 = mgr.begin_transaction(sessions[2]).unwrap();

        let snapshot = mgr.acquire_statement_snapshot(sessions[0]).unwrap();

        // Snapshot should include all active transactions
        assert!(snapshot.active_tx_ids.contains(&tx1));
        assert!(snapshot.active_tx_ids.contains(&tx2));
        assert!(snapshot.active_tx_ids.contains(&tx3));
        assert_eq!(snapshot.active_tx_ids.len(), 3);
    }

    #[test]
    fn test_committed_tx_not_in_active_set() {
        let (mgr, sessions) = setup_mgr_with_sessions(2);

        let tx1 = mgr.begin_transaction(sessions[0]).unwrap();
        let _tx2 = mgr.begin_transaction(sessions[1]).unwrap();

        mgr.commit_transaction(tx1).unwrap();

        let snapshot = mgr.acquire_statement_snapshot(sessions[1]).unwrap();

        // Committed tx1 should not be in active set
        assert!(!snapshot.active_tx_ids.contains(&tx1));
    }

    #[test]
    fn test_read_committed_isolation() {
        let (mgr, sessions) = setup_mgr_with_sessions(2);
        let sess1 = sessions[0];
        let sess2 = sessions[1];

        // Session 1: Begin
        let _tx1 = mgr.begin_transaction(sess1).unwrap();

        // Session 2: Begin and commit
        let tx2 = mgr.begin_transaction(sess2).unwrap();
        mgr.touch_table(tx2, "users".to_string()).unwrap();
        mgr.commit_transaction(tx2).unwrap();

        // Session 1: Get new snapshot (READ COMMITTED)
        let snapshot = mgr.acquire_statement_snapshot(sess1).unwrap();

        // Should see the committed transaction
        // (In real implementation, visibility filtering would apply)
        assert!(snapshot.read_ts > 0);
    }

    #[test]
    fn read_committed_new_statement_sees_newer_commit_and_allows_phantom_boundary() {
        let (mgr, sessions) = setup_mgr_with_sessions(2);
        let reader_sess = sessions[0];
        let writer_sess = sessions[1];

        let reader = mgr.begin_transaction(reader_sess).unwrap();
        let first_statement = mgr.acquire_statement_snapshot(reader_sess).unwrap();
        assert_eq!(first_statement.read_ts, 0);

        let writer = mgr.begin_transaction(writer_sess).unwrap();
        mgr.touch_table(writer, "orders".to_string()).unwrap();
        mgr.add_write(writer, "orders".to_string(), 42).unwrap();
        let writer_commit_ts = mgr.commit_transaction(writer).unwrap();

        let second_statement = mgr.acquire_statement_snapshot(reader_sess).unwrap();
        assert_eq!(second_statement.read_ts, writer_commit_ts);
        assert!(second_statement.read_ts > first_statement.read_ts);

        // Boundary: READ COMMITTED refreshes per statement, so phantoms are allowed.
        assert!(second_statement.read_ts > first_statement.read_ts);
        mgr.commit_transaction(reader).unwrap();
    }

    #[test]
    fn snapshot_fixed_read_ts_hides_newer_commit_boundary_not_serializable_claim() {
        let old_version = test_version(1, 10, 1, Some(1), "old");
        let mut newer_version = test_version(2, 10, 2, Some(2), "new");
        newer_version.previous_version = Some(1);

        let mut versions = HashMap::new();
        versions.insert(1, old_version);
        versions.insert(2, newer_version);

        let mut heads = HashMap::new();
        heads.insert(10, 2);

        let registry = HashMap::new();
        let snapshot = test_snapshot(1, 3, Isolation::SnapshotIsolation);
        let visible = visible_version_for_row(&versions, &heads, 10, &snapshot, &registry).unwrap();

        assert_eq!(visible.version_id, 1);
        match visible.payload.cols.get("name") {
            Some(Cell::Text(value)) => assert_eq!(value, "old"),
            other => panic!("expected old text payload, got {other:?}"),
        }
    }

    #[test]
    fn write_conflict_lock_released_after_rollback_allows_later_writer() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut first = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());
        let first_tx = first.begin().unwrap();
        first.acquire_write_lock("accounts").unwrap();

        let mut conflicting = MvccQueryExecutor::new(2, tx_mgr.clone(), lock_mgr.clone());
        let conflicting_tx = conflicting.begin().unwrap();
        let conflict = conflicting.acquire_write_lock("accounts");
        assert!(conflict.unwrap_err().contains(&first_tx.to_string()));

        first.rollback().unwrap();
        assert!(!lock_mgr.has_locks_for_transaction(first_tx));

        conflicting.acquire_write_lock("accounts").unwrap();
        assert!(lock_mgr.has_locks_for_transaction(conflicting_tx));
        conflicting.rollback().unwrap();
        assert!(!lock_mgr.has_locks_for_transaction(conflicting_tx));
    }

    #[test]
    fn test_transaction_record_state_transitions() {
        let (mgr, sessions) = setup_mgr_with_sessions(1);
        let sess = sessions[0];

        let tx = mgr.begin_transaction(sess).unwrap();
        let mut record = mgr.get_transaction(tx).unwrap();
        assert!(matches!(record.state, TxState::Active));

        mgr.commit_transaction(tx).unwrap();
        record = mgr.get_transaction(tx).unwrap();
        assert!(matches!(record.state, TxState::Committed { .. }));

        // Can't re-commit
        let result = mgr.commit_transaction(tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_cleanup_clears_state() {
        let (mgr, sessions) = setup_mgr_with_sessions(3);

        mgr.begin_transaction(sessions[0]).unwrap();
        mgr.begin_transaction(sessions[1]).unwrap();
        mgr.begin_transaction(sessions[2]).unwrap();

        assert_eq!(mgr.get_active_tx_ids().len(), 3);

        mgr.cleanup();

        assert_eq!(mgr.get_active_tx_ids().len(), 0);
    }
}
