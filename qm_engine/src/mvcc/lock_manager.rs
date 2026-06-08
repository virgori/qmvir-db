//! Lock manager for write concurrency control.
//!
//! Implements Phase 1 table-level locks with immediate conflict rejection.
//! Future upgrades can implement row-level locks and wait queues.

use ahash::AHashMap;
use parking_lot::Mutex;

/// Lock ownership information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockOwner {
    pub tx_id: u64,
    pub session_id: u64,
}

/// Lock mode for a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    /// Exclusive write lock (for inserts, updates, deletes).
    /// Only one writer per table in Phase 1.
    TableWrite,
}

/// Lock manager for MVCC.
///
/// Phase 1 implementation:
/// - Table-level write locks only
/// - Immediate conflict rejection (no waiting)
/// - Readers don't take locks
/// - Same transaction can re-enter its own lock
///
/// Future upgrades:
/// - Row-level locks for better concurrency
/// - Wait queues with deadlock detection
/// - Read locks for integrity checking
pub struct LockManager {
    /// Per-table write lock owner (None if uncontended).
    table_write_locks: Mutex<AHashMap<String, LockOwner>>,
}

impl LockManager {
    /// Create a new lock manager.
    pub fn new() -> Self {
        Self {
            table_write_locks: Mutex::new(AHashMap::new()),
        }
    }

    /// Acquire a table write lock.
    ///
    /// Returns Ok(()) if lock acquired.
    /// Returns Err with conflicting tx_id if lock is held by another transaction.
    pub fn acquire_table_lock(
        &self,
        table_name: &str,
        tx_id: u64,
        session_id: u64,
    ) -> Result<(), u64> {
        let mut locks = self.table_write_locks.lock();

        // Check if already locked
        if let Some(owner) = locks.get(table_name) {
            // Same transaction can re-enter its own lock
            if owner.tx_id == tx_id {
                return Ok(());
            }
            // Conflict: another transaction holds the lock
            return Err(owner.tx_id);
        }

        // Lock available: acquire it
        locks.insert(table_name.to_string(), LockOwner { tx_id, session_id });

        Ok(())
    }

    /// Release a table write lock.
    ///
    /// Only the lock owner can release.
    pub fn release_table_lock(&self, table_name: &str, tx_id: u64) -> Result<(), String> {
        let mut locks = self.table_write_locks.lock();

        match locks.get(table_name) {
            Some(owner) if owner.tx_id == tx_id => {
                locks.remove(table_name);
                Ok(())
            }
            Some(owner) => Err(format!("Cannot release lock owned by tx {}", owner.tx_id)),
            None => Err(format!("Lock not held for table {}", table_name)),
        }
    }

    /// Check if a table is locked.
    #[inline]
    pub fn is_table_locked(&self, table_name: &str) -> bool {
        self.table_write_locks.lock().contains_key(table_name)
    }

    /// Get lock owner for a table (if locked).
    #[inline]
    pub fn get_table_lock_owner(&self, table_name: &str) -> Option<LockOwner> {
        self.table_write_locks.lock().get(table_name).cloned()
    }

    /// Release all locks held by a transaction.
    ///
    /// Called on commit/rollback.
    pub fn release_all_for_transaction(&self, tx_id: u64) {
        let mut locks = self.table_write_locks.lock();
        locks.retain(|_, owner| owner.tx_id != tx_id);
    }

    /// Check if transaction holds any locks.
    pub fn has_locks_for_transaction(&self, tx_id: u64) -> bool {
        self.table_write_locks
            .lock()
            .values()
            .any(|owner| owner.tx_id == tx_id)
    }

    /// Clear all locks (for testing/cleanup).
    pub fn clear(&self) {
        self.table_write_locks.lock().clear();
    }

    /// Get count of held locks (for testing/metrics).
    #[inline]
    pub fn lock_count(&self) -> usize {
        self.table_write_locks.lock().len()
    }
}

impl Default for LockManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_release_lock() {
        let mgr = LockManager::new();

        // Acquire lock
        assert!(mgr.acquire_table_lock("users", 1, 1).is_ok());
        assert!(mgr.is_table_locked("users"));

        // Release lock
        assert!(mgr.release_table_lock("users", 1).is_ok());
        assert!(!mgr.is_table_locked("users"));
    }

    #[test]
    fn test_lock_conflict() {
        let mgr = LockManager::new();

        assert!(mgr.acquire_table_lock("users", 1, 1).is_ok());

        // Different transaction tries to acquire same lock
        let result = mgr.acquire_table_lock("users", 2, 2);
        assert_eq!(result, Err(1)); // Conflicting with tx 1
    }

    #[test]
    fn test_reentrant_lock() {
        let mgr = LockManager::new();

        assert!(mgr.acquire_table_lock("users", 1, 1).is_ok());
        // Same transaction re-enters
        assert!(mgr.acquire_table_lock("users", 1, 1).is_ok());
    }

    #[test]
    fn test_multiple_table_locks() {
        let mgr = LockManager::new();

        assert!(mgr.acquire_table_lock("users", 1, 1).is_ok());
        assert!(mgr.acquire_table_lock("orders", 1, 1).is_ok());

        assert_eq!(mgr.lock_count(), 2);
    }

    #[test]
    fn test_release_all_for_transaction() {
        let mgr = LockManager::new();

        assert!(mgr.acquire_table_lock("users", 1, 1).is_ok());
        assert!(mgr.acquire_table_lock("orders", 1, 1).is_ok());
        assert!(mgr.acquire_table_lock("products", 2, 2).is_ok());

        mgr.release_all_for_transaction(1);

        assert_eq!(mgr.lock_count(), 1);
        assert!(!mgr.is_table_locked("users"));
        assert!(!mgr.is_table_locked("orders"));
        assert!(mgr.is_table_locked("products"));
    }

    #[test]
    fn test_get_lock_owner() {
        let mgr = LockManager::new();

        mgr.acquire_table_lock("users", 5, 3).ok();

        let owner = mgr.get_table_lock_owner("users").unwrap();
        assert_eq!(owner.tx_id, 5);
        assert_eq!(owner.session_id, 3);
    }
}
