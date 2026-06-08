//! Transaction manager for MVCC.
//!
//! Manages global transaction state, commit timestamps, session context, and
//! visibility tracking. Provides efficient snapshot acquisition for READ COMMITTED
//! isolation level.

use ahash::AHashMap;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique transaction ID (monotonic counter).
pub type TxId = u64;

/// Commit timestamp (monotonic epoch counter).
pub type CommitTs = u64;

/// Unique session ID (monotonic counter).
pub type SessionId = u64;

/// Transaction state for visibility determination.
#[derive(Clone, Debug)]
pub enum TxState {
    /// Transaction is active and can read/write.
    Active,
    /// Transaction committed with a stable timestamp.
    Committed { commit_ts: CommitTs },
    /// Transaction aborted; all uncommitted versions are invisible.
    Aborted,
}

/// Snapshot for READ COMMITTED statement isolation.
///
/// Each statement acquires a new snapshot with:
/// - A read timestamp (maximum committed epoch at snapshot time)
/// - Own transaction ID (to make own writes visible)
/// - Active transaction IDs (to filter out in-flight writes)
/// - Isolation level (currently READ COMMITTED)
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Timestamp of this snapshot (max committed tx at snapshot time).
    pub read_ts: CommitTs,
    /// Owning transaction ID (own writes always visible).
    pub own_tx_id: TxId,
    /// Set of active transaction IDs at snapshot time (for visibility checks).
    pub active_tx_ids: HashSet<TxId>,
    /// Isolation level.
    pub isolation: Isolation,
}

impl Snapshot {
    /// Create a new READ COMMITTED snapshot.
    #[inline]
    pub fn read_committed(
        read_ts: CommitTs,
        own_tx_id: TxId,
        active_tx_ids: HashSet<TxId>,
    ) -> Self {
        Self {
            read_ts,
            own_tx_id,
            active_tx_ids,
            isolation: Isolation::ReadCommitted,
        }
    }
}

/// Isolation level for snapshots.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// READ COMMITTED: each statement sees data committed before it starts.
    /// Non-repeatable reads and phantoms allowed.
    ReadCommitted,
    /// SNAPSHOT ISOLATION: transaction snapshot fixed at BEGIN.
    /// Repeatable reads guaranteed. No phantoms within visible set.
    SnapshotIsolation,
}

/// Session transaction context.
///
/// Tracks the active transaction for a session and session-local settings.
#[derive(Clone, Debug)]
pub struct SessionTxContext {
    /// Unique session identifier.
    pub session_id: SessionId,
    /// Active transaction ID (None if no transaction active).
    pub tx_id: Option<TxId>,
    /// Autocommit mode: auto-commit each statement.
    pub autocommit: bool,
}

impl SessionTxContext {
    /// Create a new session context.
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            tx_id: None,
            autocommit: true,
        }
    }

    /// Begin a transaction.
    pub fn begin(&mut self, tx_id: TxId) {
        self.tx_id = Some(tx_id);
        self.autocommit = false;
    }

    /// End the active transaction.
    pub fn end_tx(&mut self) {
        self.tx_id = None;
        // Note: autocommit is NOT reset here; it's only reset on new session
    }

    /// Check if transaction is active.
    #[inline]
    pub fn in_transaction(&self) -> bool {
        self.tx_id.is_some()
    }
}

/// Record of an active or completed transaction.
///
/// Tracks ownership of row versions, write set, and commit state.
#[derive(Clone, Debug)]
pub struct TransactionRecord {
    /// Unique transaction ID.
    pub tx_id: TxId,
    /// Session that created this transaction.
    pub session_id: SessionId,
    /// Current transaction state.
    pub state: TxState,
    /// Start timestamp (epoch when transaction began).
    pub start_ts: CommitTs,
    /// Snapshot used for this transaction (for SNAPSHOT ISOLATION).
    /// For READ COMMITTED, snapshots are acquired per-statement.
    pub snapshot: Option<Snapshot>,
    /// Table names touched by this transaction.
    pub touched_tables: HashSet<String>,
    /// Write set (table + row_id pairs modified).
    pub write_set: Vec<(String, i64)>,
}

/// Transaction manager for MVCC.
///
/// Global shared state managing:
/// - Transaction IDs and commit timestamps
/// - Active transaction registry
/// - Session-to-transaction mapping
/// - Oldest active snapshot tracking
pub struct TransactionManager {
    /// Next transaction ID to allocate.
    next_tx_id: AtomicU64,
    /// Highest commit timestamp that has been published.
    latest_commit_ts: AtomicU64,
    /// Active transaction registry.
    registry: RwLock<AHashMap<TxId, TransactionRecord>>,
    /// Session ID to active transaction ID mapping.
    sessions: RwLock<AHashMap<SessionId, Option<TxId>>>,
}

impl TransactionManager {
    /// Create a new transaction manager.
    pub fn new() -> Self {
        Self {
            next_tx_id: AtomicU64::new(1),
            latest_commit_ts: AtomicU64::new(0),
            registry: RwLock::new(AHashMap::new()),
            sessions: RwLock::new(AHashMap::new()),
        }
    }

    /// Allocate a new transaction ID.
    #[inline]
    fn allocate_tx_id(&self) -> TxId {
        self.next_tx_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Allocate a new commit timestamp.
    #[inline]
    fn allocate_commit_ts(&self) -> CommitTs {
        self.latest_commit_ts.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Get the current commit timestamp (latest committed global epoch).
    #[inline]
    pub fn current_commit_ts(&self) -> CommitTs {
        self.latest_commit_ts.load(Ordering::SeqCst)
    }

    /// Register a new session.
    pub fn register_session(&self, session_id: SessionId) -> SessionTxContext {
        let mut sessions = self.sessions.write();
        sessions.insert(session_id, None);
        SessionTxContext::new(session_id)
    }

    /// Unregister a session.
    pub fn unregister_session(&self, session_id: SessionId) {
        let mut sessions = self.sessions.write();
        sessions.remove(&session_id);
    }

    /// Begin a new transaction for a session.
    ///
    /// # Returns
    /// Transaction ID and snapshot for the transaction.
    pub fn begin_transaction(&self, session_id: SessionId) -> Result<TxId, String> {
        let mut sessions = self.sessions.write();
        if !sessions.contains_key(&session_id) {
            return Err(format!("Session {} not registered", session_id));
        }

        if sessions[&session_id].is_some() {
            return Err("Transaction already active in session".to_string());
        }

        let tx_id = self.allocate_tx_id();
        let start_ts = self.current_commit_ts();
        let active_tx_ids = self.get_active_tx_ids_locked(&self.registry.read());

        let record = TransactionRecord {
            tx_id,
            session_id,
            state: TxState::Active,
            start_ts,
            snapshot: Some(Snapshot::read_committed(start_ts, tx_id, active_tx_ids)),
            touched_tables: HashSet::new(),
            write_set: Vec::new(),
        };

        self.registry.write().insert(tx_id, record);
        sessions.insert(session_id, Some(tx_id));

        Ok(tx_id)
    }

    /// Commit a transaction.
    ///
    /// # Returns
    /// Commit timestamp.
    pub fn commit_transaction(&self, tx_id: TxId) -> Result<CommitTs, String> {
        let (session_id, commit_ts) = {
            let mut registry = self.registry.write();

            let record = registry
                .get_mut(&tx_id)
                .ok_or_else(|| format!("Transaction {} not found", tx_id))?;

            match &record.state {
                TxState::Active => {
                    let commit_ts = self.allocate_commit_ts();
                    record.state = TxState::Committed { commit_ts };
                    (record.session_id, commit_ts)
                }
                TxState::Committed { .. } => {
                    return Err("Transaction already committed".to_string())
                }
                TxState::Aborted => return Err("Transaction already aborted".to_string()),
            }
        };

        self.clear_session_tx(session_id, tx_id);
        Ok(commit_ts)
    }

    /// Abort a transaction.
    pub fn abort_transaction(&self, tx_id: TxId) -> Result<(), String> {
        let session_id = {
            let mut registry = self.registry.write();

            let record = registry
                .get_mut(&tx_id)
                .ok_or_else(|| format!("Transaction {} not found", tx_id))?;

            match &record.state {
                TxState::Active => {
                    record.state = TxState::Aborted;
                    Some(record.session_id)
                }
                TxState::Committed { .. } => {
                    return Err("Cannot abort committed transaction".to_string())
                }
                TxState::Aborted => None,
            }
        };

        if let Some(session_id) = session_id {
            self.clear_session_tx(session_id, tx_id);
        }
        Ok(())
    }

    /// Get transaction record.
    pub fn get_transaction(&self, tx_id: TxId) -> Option<TransactionRecord> {
        self.registry.read().get(&tx_id).cloned()
    }

    /// Update touched tables for transaction.
    pub fn touch_table(&self, tx_id: TxId, table_name: String) -> Result<(), String> {
        let mut registry = self.registry.write();
        if let Some(record) = registry.get_mut(&tx_id) {
            record.touched_tables.insert(table_name);
            Ok(())
        } else {
            Err(format!("Transaction {} not found", tx_id))
        }
    }

    /// Add row to write set.
    pub fn add_write(&self, tx_id: TxId, table_name: String, row_id: i64) -> Result<(), String> {
        let mut registry = self.registry.write();
        if let Some(record) = registry.get_mut(&tx_id) {
            record.touched_tables.insert(table_name.clone());
            record.write_set.push((table_name, row_id));
            Ok(())
        } else {
            Err(format!("Transaction {} not found", tx_id))
        }
    }

    /// Acquire a new READ COMMITTED snapshot for a statement.
    ///
    /// Uses current commit timestamp and active transaction set.
    pub fn acquire_statement_snapshot(&self, session_id: SessionId) -> Result<Snapshot, String> {
        let sessions = self.sessions.read();
        let tx_id = sessions
            .get(&session_id)
            .copied()
            .ok_or_else(|| "Session not registered".to_string())?
            .ok_or_else(|| "No active transaction".to_string())?;

        let read_ts = self.current_commit_ts();
        let active_tx_ids = {
            let registry = self.registry.read();
            self.get_active_tx_ids_locked(&registry)
        };

        Ok(Snapshot::read_committed(read_ts, tx_id, active_tx_ids))
    }

    /// Get set of currently active transaction IDs.
    pub fn get_active_tx_ids(&self) -> HashSet<TxId> {
        let registry = self.registry.read();
        self.get_active_tx_ids_locked(&registry)
    }

    /// Helper: get active tx IDs from locked registry.
    #[inline]
    fn get_active_tx_ids_locked(
        &self,
        registry: &AHashMap<TxId, TransactionRecord>,
    ) -> HashSet<TxId> {
        registry
            .values()
            .filter(|record| matches!(record.state, TxState::Active))
            .map(|record| record.tx_id)
            .collect()
    }

    /// Calculate oldest active snapshot timestamp.
    ///
    /// Returns the minimum start_ts among all active transactions.
    /// Used for vacuum safety.
    pub fn oldest_active_snapshot(&self) -> CommitTs {
        let registry = self.registry.read();
        registry
            .values()
            .filter(|record| matches!(record.state, TxState::Active))
            .map(|record| record.start_ts)
            .min()
            .unwrap_or_else(|| self.current_commit_ts())
    }

    /// Cleanup transaction records (for testing/debugging).
    pub fn cleanup(&self) {
        self.registry.write().clear();
        self.sessions.write().clear();
        self.latest_commit_ts.store(0, Ordering::SeqCst);
        self.next_tx_id.store(1, Ordering::SeqCst);
    }

    /// Clear a session's active transaction if it still points at `tx_id`.
    fn clear_session_tx(&self, session_id: SessionId, tx_id: TxId) {
        let mut sessions = self.sessions.write();
        if let Some(active_tx) = sessions.get_mut(&session_id) {
            if *active_tx == Some(tx_id) {
                *active_tx = None;
            }
        }
    }
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tx_manager_basic_lifecycle() {
        let mgr = TransactionManager::new();

        // Register session
        let sess_id = 1;
        let tx_ctx = mgr.register_session(sess_id);
        assert!(!tx_ctx.in_transaction());

        // Begin transaction
        let tx_id = mgr.begin_transaction(sess_id).unwrap();
        assert!(mgr.get_transaction(tx_id).is_some());

        // Commit
        let commit_ts = mgr.commit_transaction(tx_id).unwrap();
        assert!(commit_ts > 0);

        let record = mgr.get_transaction(tx_id).unwrap();
        assert!(matches!(record.state, TxState::Committed { .. }));
    }

    #[test]
    fn test_session_context() {
        let mut ctx = SessionTxContext::new(1);
        assert!(!ctx.in_transaction());
        assert!(ctx.autocommit);

        ctx.begin(42);
        assert!(ctx.in_transaction());
        assert!(!ctx.autocommit);

        ctx.end_tx();
        assert!(!ctx.in_transaction());
    }

    #[test]
    fn test_active_tx_ids() {
        let mgr = TransactionManager::new();

        mgr.register_session(1);
        mgr.register_session(2);

        let tx1 = mgr.begin_transaction(1).unwrap();
        let tx2 = mgr.begin_transaction(2).unwrap();

        let active = mgr.get_active_tx_ids();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&tx1));
        assert!(active.contains(&tx2));

        mgr.commit_transaction(tx1).unwrap();
        let active = mgr.get_active_tx_ids();
        assert_eq!(active.len(), 1);
        assert!(active.contains(&tx2));
    }

    #[test]
    fn test_oldest_active_snapshot() {
        let mgr = TransactionManager::new();

        mgr.register_session(1);
        mgr.register_session(2);

        let tx1 = mgr.begin_transaction(1).unwrap();
        let start1 = mgr.get_transaction(tx1).unwrap().start_ts;

        // Small delay to ensure different timestamp
        std::thread::sleep(std::time::Duration::from_micros(100));

        let _tx2 = mgr.begin_transaction(2).unwrap();

        let oldest = mgr.oldest_active_snapshot();
        assert_eq!(oldest, start1);
    }

    #[test]
    fn test_abort_transaction() {
        let mgr = TransactionManager::new();
        mgr.register_session(1);

        let tx_id = mgr.begin_transaction(1).unwrap();
        assert!(mgr.abort_transaction(tx_id).is_ok());

        let record = mgr.get_transaction(tx_id).unwrap();
        assert!(matches!(record.state, TxState::Aborted));
    }
}
