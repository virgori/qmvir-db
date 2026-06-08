/*
 * Storage Module - ACID-compliant storage engine with WAL
 *
 * Features:
 * - Write-Ahead Logging (WAL) for durability
 * - MVCC for concurrency control
 * - Memory-mapped I/O for performance
 * - Crash recovery
 */

pub mod cache;
mod page;
pub mod snapshot;
mod transaction;
pub mod uring_wal;
mod wal;

#[cfg(feature = "python")]
pub use cache::PyWTinyLfuCache;
pub use cache::{ConcurrentCache, PageCache, QueryResultCache, WTinyLfuCache};
pub use page::*;
pub use transaction::*;
#[cfg(feature = "python")]
pub use uring_wal::PyUringWalWriter;
pub use uring_wal::UringWalWriter;
pub use wal::*;

use parking_lot::RwLock;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Storage engine configuration
#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
    pub wal_dir: PathBuf,
    pub page_size: usize,
    pub buffer_pool_size: usize,
    pub wal_buffer_size: usize,
    pub fsync_mode: FsyncMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsyncMode {
    /// fsync after every commit (safest, slowest)
    Always,
    /// fsync periodically (balance)
    Periodic,
    /// No fsync, rely on OS (fastest, least safe)
    None,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            wal_dir: PathBuf::from("./wal"),
            page_size: 8192,
            buffer_pool_size: 128 * 1024 * 1024, // 128MB
            wal_buffer_size: 16 * 1024 * 1024,   // 16MB
            fsync_mode: FsyncMode::Periodic,
        }
    }
}

/// Main storage engine
pub struct StorageEngine {
    config: StorageConfig,
    wal: Arc<RwLock<WalWriter>>,
    next_txn_id: std::sync::atomic::AtomicU64,
}

impl StorageEngine {
    pub fn new(config: StorageConfig) -> std::io::Result<Self> {
        // Create directories if they don't exist
        std::fs::create_dir_all(&config.data_dir)?;
        std::fs::create_dir_all(&config.wal_dir)?;

        let wal = WalWriter::new(config.wal_dir.clone(), config.wal_buffer_size)?;

        Ok(Self {
            config,
            wal: Arc::new(RwLock::new(wal)),
            next_txn_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Begin a new transaction
    pub fn begin_transaction(&self) -> Transaction {
        let txn_id = self
            .next_txn_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // Write BEGIN record to WAL
        {
            let mut wal = self.wal.write();
            let _ = wal.write_begin(txn_id);
        }

        Transaction::new(txn_id, self.wal.clone())
    }

    /// Commit a transaction
    pub fn commit(&self, txn: &mut Transaction) -> std::io::Result<()> {
        txn.commit()
    }

    /// Rollback a transaction
    pub fn rollback(&self, txn: &mut Transaction) -> std::io::Result<()> {
        txn.rollback()
    }

    /// Force WAL flush
    pub fn flush_wal(&self) -> std::io::Result<()> {
        let mut wal = self.wal.write();
        wal.flush()
    }

    /// Recover from WAL after crash
    pub fn recover(&self) -> std::io::Result<()> {
        let wal = self.wal.read();
        wal.recover()
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.config.data_dir
    }
}

/// Python-exposed storage engine
#[cfg(feature = "python")]
#[pyclass(name = "StorageEngine")]
pub struct PyStorageEngine {
    inner: Arc<StorageEngine>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyStorageEngine {
    #[new]
    #[pyo3(signature = (data_dir="./data", wal_dir="./wal"))]
    pub fn new(data_dir: &str, wal_dir: &str) -> PyResult<Self> {
        let config = StorageConfig {
            data_dir: PathBuf::from(data_dir),
            wal_dir: PathBuf::from(wal_dir),
            ..Default::default()
        };

        match StorageEngine::new(config) {
            Ok(engine) => Ok(Self {
                inner: Arc::new(engine),
            }),
            Err(e) => Err(pyo3::exceptions::PyIOError::new_err(e.to_string())),
        }
    }

    pub fn begin_transaction(&self) -> PyTransaction {
        // Get next transaction ID from storage engine
        static TXN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let txn_id = TXN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        PyTransaction {
            txn_id,
            isolation: IsolationLevel::default(),
            state: TransactionState::Active,
        }
    }

    pub fn flush(&self) -> PyResult<()> {
        self.inner
            .flush_wal()
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))
    }
}

/// Python-exposed transaction (standalone, without WAL)
#[cfg(feature = "python")]
#[pyclass(name = "Transaction")]
pub struct PyTransaction {
    txn_id: u64,
    isolation: IsolationLevel,
    state: TransactionState,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyTransaction {
    #[new]
    #[pyo3(signature = (txn_id, isolation="read_committed"))]
    pub fn new(txn_id: u64, isolation: &str) -> PyResult<Self> {
        let isolation_level = match isolation.to_lowercase().as_str() {
            "read_uncommitted" => IsolationLevel::ReadUncommitted,
            "read_committed" => IsolationLevel::ReadCommitted,
            "repeatable_read" => IsolationLevel::RepeatableRead,
            "serializable" | "snapshot_isolation" => IsolationLevel::Serializable,
            _ => return Err(pyo3::exceptions::PyValueError::new_err(
                format!("Unknown isolation level: {}. Use: read_uncommitted, read_committed, repeatable_read, snapshot_isolation, serializable", isolation)
            )),
        };

        Ok(Self {
            txn_id,
            isolation: isolation_level,
            state: TransactionState::Active,
        })
    }

    #[getter]
    pub fn txn_id(&self) -> u64 {
        self.txn_id
    }

    #[getter]
    pub fn state(&self) -> &'static str {
        match self.state {
            TransactionState::Active => "active",
            TransactionState::Committed => "committed",
            TransactionState::RolledBack => "rolled_back",
        }
    }

    #[getter]
    pub fn isolation_level(&self) -> &'static str {
        match self.isolation {
            IsolationLevel::ReadUncommitted => "read_uncommitted",
            IsolationLevel::ReadCommitted => "read_committed",
            IsolationLevel::RepeatableRead => "repeatable_read",
            IsolationLevel::Serializable => "snapshot_isolation",
        }
    }

    pub fn commit(&mut self) -> PyResult<()> {
        if self.state != TransactionState::Active {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Transaction not active",
            ));
        }
        self.state = TransactionState::Committed;
        Ok(())
    }

    pub fn rollback(&mut self) -> PyResult<()> {
        if self.state != TransactionState::Active {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Transaction not active",
            ));
        }
        self.state = TransactionState::RolledBack;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.state == TransactionState::Active
    }

    pub fn id(&self) -> u64 {
        self.txn_id
    }
}
