# MVCC Integration Strategy for NativeSqlEngine

## Overview
This document outlines how to integrate the high-performance MVCC system into the existing NativeSqlEngine while maintaining backward compatibility and correctness.

## Current State
- NativeSqlEngine uses snapshot cloning for transactions
- Tables stored as: `Arc<RwLock<HashMap<String, NativeTable>>>`
- NativeTable stores: `HashMap<i64, NativeRow>` (mutable)
- TransactionState clones entire table map

## Integration Points

### 1. Engine-Level Changes
- Add `TransactionManager` and `LockManager` to shared engine state
- Keep old snapshot-clone path for compatibility during migration
- Add `use_mvcc` feature flag to enable MVCC mode

### 2. Table-Level Changes
Replace or augment current storage:
```rust
// Keep old storage for compat
pub tables: Arc<RwLock<HashMap<String, NativeTable>>>,

// Add MVCC storage (optional, feature-gated)
#[cfg(feature = "mvcc")]
pub mvcc_tables: Arc<RwLock<HashMap<String, MvccTable>>>,
```

### 3. Transaction Context
Replace TransactionState with proper context:
```rust
// Old: Full snapshot clone
pub transaction: Arc<RwLock<Option<TransactionState>>>,

// New: Session context + snapshot reference
pub session_context: RwLock<SessionTxContext>,
pub tx_manager: Arc<TransactionManager>,
pub lock_manager: Arc<LockManager>,
```

### 4. Critical Paths to Modify

#### SELECT Execution
- Add visibility filtering to all table scans
- Apply is_visible() to each row before inclusion
- Maintain compatibility with indices

#### INSERT
- Acquire table write lock
- Create MvccRowVersion instead of direct insert
- Add to write set for current transaction

#### UPDATE
- Acquire row write lock
- Create new version with previous_version pointer
- Mark old version as updated

#### DELETE
- Acquire row write lock
- Mark version as deleted_by_tx
- Don't physically remove

### 5. Index Integration
- Indexes store (key, row_id, version_id) tuples
- Lookup filters by visibility
- Deduplicates logical row IDs

### 6. Vector Search Integration
- Vector cache stores version_ids
- Visibility filtering in top-k loop
- Commit invalidates latest cache epoch

### 7. Checkpoint & Recovery
- Snapshot format versioning
- Only persist committed versions
- WAL records include tx/version ownership

## Phase 3A: Core Integration
1. Add fields to NativeSqlEngine
2. Implement visibility-aware SELECT
3. Implement visibility-aware table scans
4. Add transaction lifecycle integration

## Phase 3B: Write Operations
5. Implement INSERT with locking
6. Implement UPDATE with versioning
7. Implement DELETE with marking
8. Add write-write conflict detection

## Phase 3C: Index Integration
9. Make indexes MVCC-aware
10. Add visibility filtering to lookups
11. Implement deduplication

## Phase 3D: Vector Integration
12. Make vector cache MVCC-aware
13. Implement visibility filtering for top-k
14. Commit epoch tracking

## Phase 3E: Recovery & Safety
15. Implement MVCC checkpoint
16. Implement MVCC recovery
17. Add crash safety tests

## Compatibility Strategy
1. Feature flag: `mvcc` (disabled by default)
2. Old path: snapshot-clone transactions (existing)
3. New path: MVCC transactions (feature-gated)
4. Tests for both paths
5. Gradual cutover to MVCC-only in production

## Testing Strategy
- Unit tests for MVCC operations
- Integration tests with SQL queries
- Visibility correctness verification
- Concurrent operation fuzzing
- Performance benchmarks (MVCC vs snapshot)

## Performance Targets
- Latest committed SELECT near non-MVCC speed
- INSERT/UPDATE/DELETE with locking overhead < 10%
- No full snapshot clone overhead for rollback
- Vector search remains extremely fast
