/*
 * Transaction Management - ACID transactions with MVCC
 */

use super::wal::WalWriter;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;

/// Transaction state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Active,
    Committed,
    RolledBack,
}

/// Transaction isolation level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Default for IsolationLevel {
    fn default() -> Self {
        IsolationLevel::ReadCommitted
    }
}

/// Transaction handle
pub struct Transaction {
    id: u64,
    state: TransactionState,
    isolation: IsolationLevel,
    wal: Arc<RwLock<WalWriter>>,
    modified_keys: HashSet<Vec<u8>>,
    read_set: HashSet<Vec<u8>>,
    write_set: Vec<WriteOp>,
}

#[derive(Clone)]
pub struct WriteOp {
    pub table: String,
    pub key: Vec<u8>,
    pub old_value: Option<Vec<u8>>,
    pub new_value: Option<Vec<u8>>,
}

impl Transaction {
    pub fn new(id: u64, wal: Arc<RwLock<WalWriter>>) -> Self {
        Self {
            id,
            state: TransactionState::Active,
            isolation: IsolationLevel::default(),
            wal,
            modified_keys: HashSet::new(),
            read_set: HashSet::new(),
            write_set: Vec::new(),
        }
    }

    /// Create a new transaction with specified isolation level
    pub fn with_isolation(id: u64, isolation: IsolationLevel, wal: Arc<RwLock<WalWriter>>) -> Self {
        Self {
            id,
            state: TransactionState::Active,
            isolation,
            wal,
            modified_keys: HashSet::new(),
            read_set: HashSet::new(),
            write_set: Vec::new(),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_active(&self) -> bool {
        self.state == TransactionState::Active
    }

    pub fn state(&self) -> TransactionState {
        self.state
    }

    pub fn isolation(&self) -> IsolationLevel {
        self.isolation
    }

    /// Set isolation level (must be called before any operations)
    pub fn set_isolation(&mut self, level: IsolationLevel) {
        self.isolation = level;
    }

    /// Record a read operation
    pub fn record_read(&mut self, key: &[u8]) {
        if self.isolation == IsolationLevel::Serializable {
            self.read_set.insert(key.to_vec());
        }
    }

    /// Record an insert operation
    pub fn insert(&mut self, table: &str, key: &[u8], value: &[u8]) -> std::io::Result<()> {
        if self.state != TransactionState::Active {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Transaction not active",
            ));
        }

        // Write to WAL
        {
            let mut wal = self.wal.write();
            wal.write_insert(self.id, table, value)?;
        }

        // Track modification
        self.modified_keys.insert(key.to_vec());
        self.write_set.push(WriteOp {
            table: table.to_string(),
            key: key.to_vec(),
            old_value: None,
            new_value: Some(value.to_vec()),
        });

        Ok(())
    }

    /// Record an update operation
    pub fn update(
        &mut self,
        table: &str,
        key: &[u8],
        old_value: &[u8],
        new_value: &[u8],
    ) -> std::io::Result<()> {
        if self.state != TransactionState::Active {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Transaction not active",
            ));
        }

        // Write to WAL
        {
            let mut wal = self.wal.write();
            wal.write_update(self.id, table, key, old_value, new_value)?;
        }

        // Track modification
        self.modified_keys.insert(key.to_vec());
        self.write_set.push(WriteOp {
            table: table.to_string(),
            key: key.to_vec(),
            old_value: Some(old_value.to_vec()),
            new_value: Some(new_value.to_vec()),
        });

        Ok(())
    }

    /// Record a delete operation
    pub fn delete(&mut self, table: &str, key: &[u8]) -> std::io::Result<()> {
        if self.state != TransactionState::Active {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Transaction not active",
            ));
        }

        // Write to WAL
        {
            let mut wal = self.wal.write();
            wal.write_delete(self.id, table, key)?;
        }

        // Track modification
        self.modified_keys.insert(key.to_vec());
        self.write_set.push(WriteOp {
            table: table.to_string(),
            key: key.to_vec(),
            old_value: None, // Should be populated from actual storage
            new_value: None,
        });

        Ok(())
    }

    /// Commit the transaction
    pub fn commit(&mut self) -> std::io::Result<()> {
        if self.state != TransactionState::Active {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Transaction not active",
            ));
        }

        // Write COMMIT record to WAL
        {
            let mut wal = self.wal.write();
            wal.write_commit(self.id)?;
        }

        self.state = TransactionState::Committed;
        Ok(())
    }

    /// Rollback the transaction
    pub fn rollback(&mut self) -> std::io::Result<()> {
        if self.state != TransactionState::Active {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Transaction not active",
            ));
        }

        // Write ROLLBACK record to WAL
        {
            let mut wal = self.wal.write();
            wal.write_rollback(self.id)?;
        }

        // Undo modifications in reverse order
        for _op in self.write_set.iter().rev() {
            // In a real implementation, this would undo the changes
            // For now, the ROLLBACK record in WAL is sufficient for recovery
        }

        self.state = TransactionState::RolledBack;
        Ok(())
    }

    /// Get modified keys (for conflict detection)
    pub fn modified_keys(&self) -> &HashSet<Vec<u8>> {
        &self.modified_keys
    }

    /// Check for conflicts with another transaction
    pub fn conflicts_with(&self, other: &Transaction) -> bool {
        // Check if any modified keys overlap
        for key in &self.modified_keys {
            if other.modified_keys.contains(key) {
                return true;
            }
            // For serializable, also check read set
            if self.isolation == IsolationLevel::Serializable {
                if other.read_set.contains(key) {
                    return true;
                }
            }
        }

        // Check if we read anything they modified
        if self.isolation == IsolationLevel::Serializable {
            for key in &self.read_set {
                if other.modified_keys.contains(key) {
                    return true;
                }
            }
        }

        false
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // If transaction is still active when dropped, rollback
        if self.state == TransactionState::Active {
            let _ = self.rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_transaction_lifecycle() {
        let dir = tempdir().unwrap();
        let wal = WalWriter::new(dir.path().to_path_buf(), 1024 * 1024).unwrap();
        let wal = Arc::new(RwLock::new(wal));

        let mut txn = Transaction::new(1, wal);
        assert!(txn.is_active());

        txn.commit().unwrap();
        assert_eq!(txn.state(), TransactionState::Committed);
    }
}
