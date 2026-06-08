/*
 * Transaction Manager — Phase 7c (MVCC)
 *
 * Implements multi-version concurrency control with snapshot isolation:
 *   - Each transaction gets a monotonic txn_id
 *   - Snapshot records the set of active txns at BEGIN time
 *   - Writes are buffered until COMMIT
 *   - Conflict detection: write-write conflicts on the same key → abort
 */

use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

// ── Transaction ID ──────────────────────────────────────────────────────

pub type TxnId = u64;

/// Global transaction ID generator.
static NEXT_TXN_ID: AtomicU64 = AtomicU64::new(1);

fn next_txn_id() -> TxnId {
    // H-11: Use SeqCst for cross-thread monotonic visibility.
    NEXT_TXN_ID.fetch_add(1, Ordering::SeqCst)
}

// ── Version / Write-set ─────────────────────────────────────────────────

/// A versioned value (conceptual — actual data is opaque bytes).
#[derive(Debug, Clone)]
pub struct VersionedValue {
    pub data: Vec<u8>,
    pub created_by: TxnId,
    pub deleted_by: Option<TxnId>,
}

/// A pending write within a transaction.
#[derive(Debug, Clone)]
pub enum WriteOp {
    Put(Vec<u8>),
    Delete,
}

// ── Transaction state ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnStatus {
    Active,
    Committed,
    Aborted,
}

/// Per-transaction state.
#[derive(Debug)]
pub struct Transaction {
    pub id: TxnId,
    pub status: TxnStatus,
    /// Snapshot of active txn IDs at BEGIN time.
    pub snapshot: BTreeSet<TxnId>,
    /// Buffered writes: key → operation.
    pub write_set: HashMap<Vec<u8>, WriteOp>,
}

// ── MVCC Store (in-memory) ──────────────────────────────────────────────

/// Multi-version key-value store.
///
/// Each key maps to a list of versions (newest first).
pub struct MvccStore {
    /// key → list of versions (most recent first).
    data: DashMap<Vec<u8>, Vec<VersionedValue>>,
    /// All transactions.
    txns: DashMap<TxnId, Transaction>,
    /// Currently active transaction IDs (for snapshot).
    active: Mutex<BTreeSet<TxnId>>,
    /// H-10 fix: Global commit serialization lock.
    /// Ensures conflict-check + status-set + write-apply is atomic.
    commit_mu: Mutex<()>,
}

impl MvccStore {
    pub fn new() -> Self {
        Self {
            data: DashMap::new(),
            txns: DashMap::new(),
            active: Mutex::new(BTreeSet::new()),
            commit_mu: Mutex::new(()),
        }
    }

    // ── BEGIN ───────────────────────────────────────────────────────────

    pub fn begin(&self) -> TxnId {
        let id = next_txn_id();
        let snapshot = {
            let mut active = self.active.lock();
            let snap = active.clone();
            active.insert(id);
            snap
        };
        let txn = Transaction {
            id,
            status: TxnStatus::Active,
            snapshot,
            write_set: HashMap::new(),
        };
        self.txns.insert(id, txn);
        id
    }

    // ── READ (snapshot isolation) ───────────────────────────────────────

    /// Read a key using snapshot isolation.
    ///
    /// A version is visible if:
    ///   - created_by is committed AND not in our snapshot's active set
    ///   - created_by == our own txn_id (read-your-writes)
    ///   - NOT deleted by a committed txn that's also visible
    pub fn get(&self, txn_id: TxnId, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let txn = self
            .txns
            .get(&txn_id)
            .ok_or_else(|| format!("txn {} not found", txn_id))?;
        if txn.status != TxnStatus::Active {
            return Err(format!("txn {} is not active", txn_id));
        }

        // Check write-set first (read-your-writes)
        if let Some(op) = txn.write_set.get(key) {
            return match op {
                WriteOp::Put(data) => Ok(Some(data.clone())),
                WriteOp::Delete => Ok(None),
            };
        }

        // Search committed versions
        if let Some(versions) = self.data.get(key) {
            for v in versions.iter() {
                if !self.is_visible(&txn, v) {
                    continue;
                }
                // Check if deleted by a visible txn
                if let Some(del_txn) = v.deleted_by {
                    if self.is_txn_visible(&txn, del_txn) {
                        continue;
                    }
                }
                return Ok(Some(v.data.clone()));
            }
        }

        Ok(None)
    }

    fn is_visible(&self, txn: &Transaction, v: &VersionedValue) -> bool {
        if v.created_by == txn.id {
            return true;
        }
        // Snapshot isolation: only see versions created by txns that:
        // 1. Started BEFORE us (created_by < our txn_id)
        // 2. Were committed at our BEGIN time (not in our active snapshot)
        if v.created_by >= txn.id {
            return false; // Txn that started after us — invisible
        }
        if let Some(creator) = self.txns.get(&v.created_by) {
            creator.status == TxnStatus::Committed && !txn.snapshot.contains(&v.created_by)
        } else {
            false
        }
    }

    fn is_txn_visible(&self, txn: &Transaction, other_id: TxnId) -> bool {
        if other_id == txn.id {
            return true;
        }
        if other_id >= txn.id {
            return false;
        }
        if let Some(other) = self.txns.get(&other_id) {
            other.status == TxnStatus::Committed && !txn.snapshot.contains(&other_id)
        } else {
            false
        }
    }

    // ── WRITE (buffered) ────────────────────────────────────────────────

    pub fn put(&self, txn_id: TxnId, key: Vec<u8>, value: Vec<u8>) -> Result<(), String> {
        let mut txn = self
            .txns
            .get_mut(&txn_id)
            .ok_or_else(|| format!("txn {} not found", txn_id))?;
        if txn.status != TxnStatus::Active {
            return Err(format!("txn {} is not active", txn_id));
        }
        txn.write_set.insert(key, WriteOp::Put(value));
        Ok(())
    }

    pub fn delete(&self, txn_id: TxnId, key: Vec<u8>) -> Result<(), String> {
        let mut txn = self
            .txns
            .get_mut(&txn_id)
            .ok_or_else(|| format!("txn {} not found", txn_id))?;
        if txn.status != TxnStatus::Active {
            return Err(format!("txn {} is not active", txn_id));
        }
        txn.write_set.insert(key, WriteOp::Delete);
        Ok(())
    }

    // ── COMMIT ──────────────────────────────────────────────────────────

    pub fn commit(&self, txn_id: TxnId) -> Result<(), String> {
        // H-10 fix: Hold commit_mu across conflict-check AND write-apply.
        // This eliminates the TOCTOU gap where two concurrent commits could
        // both pass validation because neither sees the other as committed yet.
        let _commit_guard = self.commit_mu.lock();

        // Phase 1: Conflict detection — check for write-write conflicts
        {
            let txn = self
                .txns
                .get(&txn_id)
                .ok_or_else(|| format!("txn {} not found", txn_id))?;
            if txn.status != TxnStatus::Active {
                return Err(format!("txn {} is not active", txn_id));
            }

            for key in txn.write_set.keys() {
                if let Some(versions) = self.data.get(key) {
                    for v in versions.iter() {
                        if v.created_by != txn_id {
                            if let Some(other) = self.txns.get(&v.created_by) {
                                if other.status == TxnStatus::Committed
                                    && (v.created_by >= txn_id
                                        || txn.snapshot.contains(&v.created_by))
                                {
                                    drop(txn);
                                    // Abort while still holding commit_mu — safe because
                                    // abort only touches txns DashMap + active Mutex.
                                    self.abort(txn_id)?;
                                    return Err(format!(
                                        "write-write conflict on key (txn {} vs {})",
                                        txn_id, v.created_by
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Phase 2: Apply writes (still under commit_mu — atomic with Phase 1)
        let write_set = {
            let mut txn = self
                .txns
                .get_mut(&txn_id)
                .ok_or_else(|| format!("txn {} disappeared during commit", txn_id))?;
            txn.status = TxnStatus::Committed;
            std::mem::take(&mut txn.write_set)
        };

        for (key, op) in write_set {
            match op {
                WriteOp::Put(data) => {
                    let ver = VersionedValue {
                        data,
                        created_by: txn_id,
                        deleted_by: None,
                    };
                    self.data.entry(key).or_default().insert(0, ver);
                }
                WriteOp::Delete => {
                    if let Some(mut versions) = self.data.get_mut(&key) {
                        for v in versions.iter_mut() {
                            if v.deleted_by.is_none() {
                                v.deleted_by = Some(txn_id);
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Remove from active set
        self.active.lock().remove(&txn_id);
        Ok(())
    }

    // ── ABORT / ROLLBACK ────────────────────────────────────────────────

    pub fn abort(&self, txn_id: TxnId) -> Result<(), String> {
        let mut txn = self
            .txns
            .get_mut(&txn_id)
            .ok_or_else(|| format!("txn {} not found", txn_id))?;
        txn.status = TxnStatus::Aborted;
        txn.write_set.clear();
        drop(txn);
        self.active.lock().remove(&txn_id);
        Ok(())
    }

    // ── GC (purge old versions) ─────────────────────────────────────────

    /// Remove versions that are no longer visible to any active transaction.
    pub fn gc(&self) {
        let min_active = {
            let active = self.active.lock();
            active.iter().next().copied().unwrap_or(u64::MAX)
        };

        self.data.alter_all(|_key, mut versions| {
            versions.retain(|v| {
                // A version can only be removed if it was deleted by a
                // committed txn whose id is below min_active — meaning every
                // active transaction started after the delete committed, so
                // none of them can still see this version.
                match v.deleted_by {
                    Some(del_by) if del_by < min_active => false,
                    _ => true,
                }
            });
            versions
        });
    }

    /// Number of active transactions.
    pub fn active_count(&self) -> usize {
        self.active.lock().len()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_read_write() {
        let store = MvccStore::new();
        let tx = store.begin();
        store.put(tx, b"key1".to_vec(), b"val1".to_vec()).unwrap();
        // Read-your-writes
        let v = store.get(tx, b"key1").unwrap();
        assert_eq!(v, Some(b"val1".to_vec()));
        store.commit(tx).unwrap();

        // New txn should see committed data
        let tx2 = store.begin();
        let v2 = store.get(tx2, b"key1").unwrap();
        assert_eq!(v2, Some(b"val1".to_vec()));
        store.commit(tx2).unwrap();
    }

    #[test]
    fn test_snapshot_isolation() {
        let store = MvccStore::new();

        // Txn1 writes key1
        let tx1 = store.begin();
        store.put(tx1, b"k".to_vec(), b"v1".to_vec()).unwrap();
        store.commit(tx1).unwrap();

        // Txn2 begins, reads k=v1
        let tx2 = store.begin();

        // Txn3 begins after tx2, overwrites k
        let tx3 = store.begin();
        store.put(tx3, b"k".to_vec(), b"v3".to_vec()).unwrap();
        store.commit(tx3).unwrap();

        // Txn2 should still see k=v1 (snapshot isolation)
        let v = store.get(tx2, b"k").unwrap();
        assert_eq!(v, Some(b"v1".to_vec()));
        store.commit(tx2).unwrap();

        // New txn sees latest
        let tx4 = store.begin();
        let v4 = store.get(tx4, b"k").unwrap();
        assert_eq!(v4, Some(b"v3".to_vec()));
        store.commit(tx4).unwrap();
    }

    #[test]
    fn test_delete() {
        let store = MvccStore::new();
        let tx1 = store.begin();
        store.put(tx1, b"d".to_vec(), b"val".to_vec()).unwrap();
        store.commit(tx1).unwrap();

        let tx2 = store.begin();
        store.delete(tx2, b"d".to_vec()).unwrap();
        // Read-your-delete
        let v = store.get(tx2, b"d").unwrap();
        assert_eq!(v, None);
        store.commit(tx2).unwrap();

        // New txn: deleted
        let tx3 = store.begin();
        let v3 = store.get(tx3, b"d").unwrap();
        assert_eq!(v3, None);
        store.commit(tx3).unwrap();
    }

    #[test]
    fn test_abort_discards_writes() {
        let store = MvccStore::new();
        let tx = store.begin();
        store.put(tx, b"a".to_vec(), b"val".to_vec()).unwrap();
        store.abort(tx).unwrap();

        let tx2 = store.begin();
        let v = store.get(tx2, b"a").unwrap();
        assert_eq!(v, None); // Aborted write not visible
        store.commit(tx2).unwrap();
    }

    #[test]
    fn test_gc() {
        let store = MvccStore::new();
        let tx1 = store.begin();
        store.put(tx1, b"g".to_vec(), b"v1".to_vec()).unwrap();
        store.commit(tx1).unwrap();

        let tx2 = store.begin();
        store.put(tx2, b"g".to_vec(), b"v2".to_vec()).unwrap();
        store.commit(tx2).unwrap();

        store.gc();
        // After GC with no active txns, v1 can be purged (deleted_by is None though,
        // so it should be retained).  This is a basic smoke test.
        let tx3 = store.begin();
        let v = store.get(tx3, b"g").unwrap();
        assert_eq!(v, Some(b"v2".to_vec()));
        store.commit(tx3).unwrap();
    }
}
