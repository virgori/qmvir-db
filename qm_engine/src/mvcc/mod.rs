//! High-performance MVCC (Multi-Version Concurrency Control) for NativeSqlEngine
//!
//! This module provides:
//! - Transaction management with per-transaction isolation
//! - Immutable row version storage and visibility filtering
//! - READ COMMITTED snapshot isolation
//! - Session-based transaction context
//! - Fast visibility checks optimized for latest-committed reads
//! - Lock management for write conflict detection
//! - MVCC-aware query executor for SQL operations
//!
//! Philosophy:
//! - Append-mostly writes: versions never mutate after creation
//! - Zero unnecessary cloning: Arc<> for shared payloads
//! - Visibility filtering over rollback restoration: scalable undo
//! - Latest-visible fast paths: common case is O(1) lookup
//! - Lock minimization: table-level locks initially, row-level later

pub mod executor;
pub mod lock_manager;
pub mod row_version;
pub mod tx_manager;
pub mod visibility;

pub use executor::MvccQueryExecutor;
pub use lock_manager::{LockManager, LockMode, LockOwner};
pub use row_version::{LogicalRowId, MvccRowVersion, MvccTable, VersionId};
pub use tx_manager::{
    CommitTs, SessionId, SessionTxContext, TransactionManager, TransactionRecord, TxId, TxState,
};
pub use visibility::{Isolation, Snapshot};

/// Re-export commonly used MVCC types
pub mod prelude {
    pub use crate::mvcc::{
        CommitTs, Isolation, LockManager, LockMode, LockOwner, LogicalRowId, MvccQueryExecutor,
        MvccRowVersion, MvccTable, SessionId, SessionTxContext, Snapshot, TransactionManager,
        TransactionRecord, TxId, TxState, VersionId,
    };
}
