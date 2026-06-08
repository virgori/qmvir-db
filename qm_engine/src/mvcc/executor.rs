//! MVCC-aware SQL query executor.
//!
//! Provides high-performance visibility-aware query execution for the NativeSqlEngine
//! with READ COMMITTED isolation level.
//!
//! Key features:
//! - Visibility filtering integrated into scan operators
//! - Write lock acquisition for DML operations
//! - Transaction lifecycle management
//! - Deterministic visibility ordering

use crate::mvcc::prelude::*;
use ahash::AHashMap;
use std::sync::Arc;

/// MVCC-aware query executor for SQL operations.
///
/// Handles transaction management and visibility-aware execution of SQL statements.
pub struct MvccQueryExecutor {
    /// Shared transaction manager
    tx_manager: Arc<TransactionManager>,
    /// Shared lock manager
    lock_manager: Arc<LockManager>,
    /// Current session context
    session_context: SessionTxContext,
}

impl MvccQueryExecutor {
    /// Create a new MVCC query executor for a session.
    pub fn new(
        session_id: SessionId,
        tx_manager: Arc<TransactionManager>,
        lock_manager: Arc<LockManager>,
    ) -> Self {
        let session_context = tx_manager.register_session(session_id);

        Self {
            tx_manager,
            lock_manager,
            session_context,
        }
    }

    /// Get current session ID.
    pub fn session_id(&self) -> SessionId {
        self.session_context.session_id
    }

    /// Begin a transaction.
    pub fn begin(&mut self) -> Result<TxId, String> {
        let session_id = self.session_context.session_id;
        let tx_id = self.tx_manager.begin_transaction(session_id)?;
        self.session_context.begin(tx_id);
        Ok(tx_id)
    }

    /// Commit current transaction.
    pub fn commit(&mut self) -> Result<CommitTs, String> {
        if let Some(tx_id) = self.session_context.tx_id {
            let result = self.tx_manager.commit_transaction(tx_id)?;
            self.lock_manager.release_all_for_transaction(tx_id);
            self.session_context.end_tx();
            Ok(result)
        } else {
            Err("No active transaction".to_string())
        }
    }

    /// Rollback current transaction.
    pub fn rollback(&mut self) -> Result<(), String> {
        if let Some(tx_id) = self.session_context.tx_id {
            self.tx_manager.abort_transaction(tx_id)?;
            self.lock_manager.release_all_for_transaction(tx_id);
            self.session_context.end_tx();
            Ok(())
        } else {
            Err("No active transaction".to_string())
        }
    }

    /// Get or create active transaction for statement.
    ///
    /// For autocommit mode, creates a temporary transaction.
    /// For explicit transactions, uses existing one.
    pub fn get_statement_tx(&mut self) -> Result<TxId, String> {
        if let Some(tx_id) = self.session_context.tx_id {
            Ok(tx_id)
        } else if self.session_context.autocommit {
            // Autocommit: implicit transaction for single statement
            self.begin()
        } else {
            Err("No active transaction for statement".to_string())
        }
    }

    /// Get a statement snapshot for visibility filtering.
    pub fn get_snapshot(&self) -> Result<Snapshot, String> {
        if self.session_context.in_transaction() {
            let session_id = self.session_context.session_id;
            self.tx_manager.acquire_statement_snapshot(session_id)
        } else {
            Err("No active transaction".to_string())
        }
    }

    /// Acquire table write lock for DML operation.
    pub fn acquire_write_lock(&self, table_name: &str) -> Result<(), String> {
        if let Some(tx_id) = self.session_context.tx_id {
            let session_id = self.session_context.session_id;
            match self
                .lock_manager
                .acquire_table_lock(table_name, tx_id, session_id)
            {
                Ok(()) => {
                    // Track table access
                    self.tx_manager
                        .touch_table(tx_id, table_name.to_string())
                        .ok();
                    Ok(())
                }
                Err(conflicting_tx) => Err(format!(
                    "Row is concurrently modified by transaction {}",
                    conflicting_tx
                )),
            }
        } else {
            Err("No active transaction".to_string())
        }
    }

    /// Check if in transaction.
    pub fn in_transaction(&self) -> bool {
        self.session_context.in_transaction()
    }

    /// Get active transaction ID.
    pub fn active_tx(&self) -> Option<TxId> {
        self.session_context.tx_id
    }

    /// Check current autocommit mode.
    pub fn autocommit(&self) -> bool {
        self.session_context.autocommit
    }

    /// Get transaction record (for testing/debugging).
    pub fn get_tx_record(&self) -> Option<TransactionRecord> {
        self.session_context
            .tx_id
            .and_then(|tx_id| self.tx_manager.get_transaction(tx_id))
    }

    /// Cleanup session (called on disconnect).
    pub fn cleanup(&mut self) {
        // Try to rollback active transaction
        if let Some(tx_id) = self.session_context.tx_id {
            let _ = self.tx_manager.abort_transaction(tx_id);
            self.lock_manager.release_all_for_transaction(tx_id);
            self.session_context.end_tx(); // Clear transaction context
        }

        // Unregister session
        self.tx_manager
            .unregister_session(self.session_context.session_id);
    }
}

/// Helper for filtering rows by snapshot visibility.
pub struct VisibilityFilter {
    _snapshot: Snapshot,
    _tx_registry: AHashMap<u64, TransactionRecord>,
}

impl VisibilityFilter {
    /// Create a new visibility filter for a snapshot.
    pub fn new(snapshot: Snapshot, _tx_manager: &TransactionManager) -> Self {
        // Note: In production, we'd efficiently track transaction state
        // For now, we store records needed for visibility checks
        let tx_registry = AHashMap::new();

        Self {
            _snapshot: snapshot,
            _tx_registry: tx_registry,
        }
    }

    /// Check if a row is visible to the snapshot.
    ///
    /// This is a placeholder implementation.
    /// Real implementation would integrate with MvccTable and versions.
    pub fn is_visible(&self, _row_id: i64) -> bool {
        // TODO: Integrate with actual version chain and is_visible()
        true
    }

    /// Filter rows by visibility.
    pub fn filter_rows(&self, row_ids: &[i64]) -> Vec<i64> {
        row_ids
            .iter()
            .copied()
            .filter(|row_id| self.is_visible(*row_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_creation() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let executor = MvccQueryExecutor::new(1, tx_mgr, lock_mgr);

        assert_eq!(executor.session_id(), 1);
        assert!(!executor.in_transaction());
        assert!(executor.autocommit());
    }

    #[test]
    fn test_begin_commit_cycle() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut executor = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());

        // Begin
        let tx_id = executor.begin().unwrap();
        assert!(executor.in_transaction());
        assert_eq!(executor.active_tx(), Some(tx_id));

        // Commit
        let commit_ts = executor.commit().unwrap();
        assert!(!executor.in_transaction());
        assert!(commit_ts > 0);
    }

    #[test]
    fn test_rollback() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut executor = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());

        executor.begin().unwrap();
        assert!(executor.in_transaction());

        executor.rollback().unwrap();
        assert!(!executor.in_transaction());
    }

    #[test]
    fn test_acquire_write_lock() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut executor = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());

        executor.begin().unwrap();

        // Acquire lock
        assert!(executor.acquire_write_lock("users").is_ok());

        executor.commit().unwrap();
    }

    #[test]
    fn test_write_lock_conflict() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut executor1 = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());
        let mut executor2 = MvccQueryExecutor::new(2, tx_mgr.clone(), lock_mgr.clone());

        executor1.begin().unwrap();
        executor2.begin().unwrap();

        // First transaction acquires lock
        assert!(executor1.acquire_write_lock("users").is_ok());

        // Second transaction conflicts
        let result = executor2.acquire_write_lock("users");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("concurrently modified"));

        executor1.commit().unwrap();
        executor2.rollback().unwrap();
    }

    #[test]
    fn test_get_snapshot() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut executor = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());

        // Without transaction
        assert!(executor.get_snapshot().is_err());

        // With transaction
        executor.begin().unwrap();
        let snapshot = executor.get_snapshot().unwrap();
        assert_eq!(snapshot.own_tx_id, executor.active_tx().unwrap());

        executor.commit().unwrap();
    }

    #[test]
    fn test_autocommit_mode() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let executor = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());

        assert!(executor.autocommit());
    }

    #[test]
    fn test_cleanup() {
        let tx_mgr = Arc::new(TransactionManager::new());
        let lock_mgr = Arc::new(LockManager::new());

        let mut executor = MvccQueryExecutor::new(1, tx_mgr.clone(), lock_mgr.clone());

        executor.begin().unwrap();
        executor.acquire_write_lock("users").ok();

        executor.cleanup();

        assert!(!executor.in_transaction());
        assert_eq!(lock_mgr.lock_count(), 0);
    }
}
