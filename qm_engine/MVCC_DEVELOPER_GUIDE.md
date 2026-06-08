# MVCC Quick Start & Developer Guide

## For Developers: Understanding MVCC

### Quick Architecture Overview

```rust
// 1. Create a transaction manager (shared across sessions)
let tx_manager = Arc::new(TransactionManager::new());
let lock_manager = Arc::new(LockManager::new());

// 2. Register a session
let session_id = 1;
tx_manager.register_session(session_id);

// 3. Create an executor for the session
let mut executor = MvccQueryExecutor::new(
    session_id,
    tx_manager.clone(),
    lock_manager.clone(),
);

// 4. Execute transaction
executor.begin()?;                           // BEGIN
executor.acquire_write_lock("users")?;     // For DML
let snapshot = executor.get_snapshot()?;    // Get visibility scope
executor.commit()?;                         // COMMIT
```

### Core Concepts

#### 1. **Transaction Manager**
Manages global transaction state and commit timestamps.

```rust
// Key methods:
tx_manager.begin_transaction(session_id)    // Get TxId
tx_manager.commit_transaction(tx_id)        // Get CommitTs
tx_manager.abort_transaction(tx_id)         // Mark aborted
tx_manager.acquire_statement_snapshot(session_id)  // Get snapshot
tx_manager.get_active_tx_ids()              // For visibility
```

#### 2. **Snapshots**
Define what's visible to a transaction.

```rust
struct Snapshot {
    read_ts: CommitTs,              // Max committed at snapshot time
    own_tx_id: TxId,                // Own transaction (always visible)
    active_tx_ids: HashSet<TxId>,   // Other concurrent txs
    isolation: Isolation,            // READ COMMITTED (currently)
}
```

#### 3. **Row Versions**
Immutable versions with ownership tracking.

```rust
struct MvccRowVersion {
    version_id: VersionId,          // Unique ID
    logical_row_id: LogicalRowId,   // Logical row (multiple versions)
    created_by_tx: u64,             // Creator transaction
    deleted_by_tx: Option<u64>,     // Deleter transaction
    created_commit_ts: Option<u64>, // When committed
    deleted_commit_ts: Option<u64>, // When deletion committed
    previous_version: Option<VersionId>, // Version chain link
    payload: Arc<NativeRow>,        // Shared immutable data
}
```

#### 4. **Visibility Filtering**
Fast-path function determines if row is visible.

```rust
// This is the hot path - called millions of times per second
#[inline(always)]
fn is_visible(version, snapshot, registry) -> bool {
    // Own writes always visible
    if version.created_by_tx == snapshot.own_tx_id { return true; }
    
    // Committed before snapshot?
    if let Some(ts) = version.created_commit_ts {
        if ts <= snapshot.read_ts { return true; }
    }
    
    // Deleted?
    if version.deleted_by_tx.is_some() { return false; }
    
    false
}
```

#### 5. **Lock Manager**
Write concurrency control with immediate conflict detection.

```rust
// Acquire table write lock
lock_manager.acquire_table_lock("users", tx_id, session_id)
    // Ok(()) if acquired
    // Err(conflicting_tx_id) if conflict

// Release on commit/rollback
lock_manager.release_all_for_transaction(tx_id);
```

### Common Patterns

#### Pattern 1: Read-Only Query
```rust
// Gets snapshot for visibility filtering
let snapshot = executor.get_snapshot()?;

// Filter rows by visibility
for row_id in all_rows {
    if is_visible(versions[row_id], &snapshot, registry) {
        results.push(row_id);
    }
}
```

#### Pattern 2: Write Operation
```rust
// Acquire exclusive lock
executor.acquire_write_lock("table")?;

// Create new version
let new_version = MvccRowVersion::new(
    next_version_id,
    row_id,
    executor.active_tx().unwrap(),
    Arc::new(updated_row),
);

// Insert into table
table.insert_version(new_version);

// On commit: all versions become visible
executor.commit()?;  // Publishes versions
```

#### Pattern 3: Handling Conflicts
```rust
match executor.acquire_write_lock("table") {
    Ok(()) => {
        // Write...
        executor.commit()?;
    }
    Err(conflicting_tx) => {
        // Row is locked by another transaction
        executor.rollback()?;
        // Retry or fail
    }
}
```

## For Integrators: Adding MVCC to NativeSqlEngine

### Step 1: Add MVCC to Engine State
```rust
pub struct NativeSqlEngine {
    pub tables: Arc<RwLock<HashMap<String, NativeTable>>>,
    // ... existing fields ...
    
    // NEW: MVCC support
    #[cfg(feature = "mvcc")]
    pub tx_manager: Arc<TransactionManager>,
    #[cfg(feature = "mvcc")]
    pub lock_manager: Arc<LockManager>,
}
```

### Step 2: Implement Visibility-Aware SELECT
```rust
fn execute_select_mvcc(
    &self,
    table_name: &str,
    executor: &MvccQueryExecutor,
) -> Result<Vec<Row>> {
    let snapshot = executor.get_snapshot()?;
    let table = self.tables.read().get(table_name)?;
    
    // Filter by visibility
    let results: Vec<_> = table.rows
        .iter()
        .filter(|(row_id, _)| {
            // Check visibility via version chain
            is_visible_in_snapshot(row_id, &snapshot)
        })
        .collect();
    
    Ok(results)
}
```

### Step 3: Integrate DML Operations
```rust
fn execute_insert_mvcc(
    &mut self,
    table_name: &str,
    row: NativeRow,
    executor: &mut MvccQueryExecutor,
) -> Result<()> {
    // Acquire write lock
    executor.acquire_write_lock(table_name)?;
    
    // Create MVCC version
    let version = MvccRowVersion::new(
        self.next_version_id(),
        self.next_row_id(),
        executor.active_tx().unwrap(),
        Arc::new(row),
    );
    
    // Insert into table
    let mut table = self.tables.write();
    table.insert_version(version);
    
    Ok(())
}
```

## Testing Guide

### Running Tests
```bash
# All MVCC tests
cargo test --lib mvcc:: --no-default-features

# Specific test module
cargo test --lib mvcc::visibility:: --no-default-features

# With output
cargo test --lib mvcc:: -- --nocapture

# Single threaded (for determinism)
cargo test --lib mvcc:: -- --test-threads=1
```

### Writing New Tests
```rust
#[test]
fn test_my_mvcc_feature() {
    // Setup
    let mgr = TransactionManager::new();
    mgr.register_session(1);
    
    // Execute
    let tx_id = mgr.begin_transaction(1).unwrap();
    
    // Verify
    let record = mgr.get_transaction(tx_id).unwrap();
    assert!(matches!(record.state, TxState::Active));
}
```

## Performance Tips

### 1. Minimize Lock Contention
- Use row-level locks (Phase 3B) to avoid table-level lock bottleneck
- Batch similar operations to reduce lock/unlock cycles

### 2. Optimize Visibility Checks
- `is_visible()` is already inline-optimized
- Cache snapshot for multiple checks
- Avoid hashmap lookups in tight loops (use registry hints)

### 3. Manage Version Chains
- Keep version chains short with periodic vacuum
- Monitor `table.version_count(row_id)` for hot rows
- Consider lazy materialization for very long chains

### 4. Memory Efficiency
- Arc<NativeRow> shares payloads across versions
- Write set is sparse (only modified rows)
- Snapshot size is O(active_transactions)

## Troubleshooting

### Issue: "Row is concurrently modified"
**Cause**: Write-write conflict on same row  
**Solution**: Retry with exponential backoff  
**Code**:
```rust
loop {
    match executor.acquire_write_lock("table") {
        Ok(()) => break,
        Err(_) => {
            executor.rollback()?;
            std::thread::sleep(backoff_duration);
            executor.begin()?;
        }
    }
}
```

### Issue: Old versions accumulating
**Cause**: No vacuum/GC implemented yet (Phase 7)  
**Solution**: Manual cleanup or wait for Phase 7  
**Monitor**: `table.total_versions()` and `table.version_count(row_id)`

### Issue: Slow reads
**Cause**: Long version chains for hot rows  
**Solution**: 
1. Check version chain length: `table.version_count(row_id)`
2. If > 100 versions, implement vacuum (Phase 7)
3. Consider latest-visible caching

### Issue: High memory usage
**Cause**: Too many active versions
**Solution**:
1. Monitor total versions: `table.total_versions()`
2. Check transaction counts: `tx_manager.get_active_tx_ids().len()`
3. Implement vacuum in Phase 7

## Extension Points

### Future: Row-Level Locks
Replace `LockManager::table_write_locks` with:
```rust
row_write_locks: Mutex<HashMap<(TableId, LogicalRowId), LockOwner>>
```

### Future: Snapshot Isolation
Add to Snapshot:
```rust
pub enum Isolation {
    ReadCommitted,
    SnapshotIsolation { fixed_at: CommitTs },
}
```

### Future: HNSW/ANN
Add version-aware nodes:
```rust
struct HnswNode {
    vector: Vec<f32>,
    version_id: VersionId,  // For visibility filtering
    neighbors: Vec<HnswNode>,
}
```

## Documentation References

1. **mvcc_design.md** - Full architectural blueprint
2. **mvcc_runtime_notes.md** - Implementation details
3. **mvcc_integration_strategy.md** - SQL engine integration plan
4. **mvcc_performance_report.txt** - Performance characteristics
5. **mvcc_visibility_tests.txt** - Correctness verification

## Support

For questions or issues:
1. Check the test suite (35 tests cover most scenarios)
2. Review inline documentation in source code
3. Consult the design document (mvcc_design.md)
4. Look at test examples for usage patterns

---

**Last Updated**: 2026-05-15  
**Version**: Phase 1-5 Complete  
**Status**: Production Ready (Foundation)
