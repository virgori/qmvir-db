# NativeSqlEngine MVCC Design

Date: 2026-05-15

Status: architecture design only. This document does not imply an immediate engine-wide MVCC implementation.

## Goals

- Support multiple concurrent engine sessions.
- Provide real transaction visibility across sessions.
- Keep readers non-blocking where practical.
- Preserve rollback-safe scalar, text, vector, and secondary-index behavior.
- Keep exact vector search deterministic and visibility-correct.
- Keep checkpoint/WAL recovery compatible with current `native_sql.snap`, `native_sql.indexes`, and SQL WAL behavior during migration.

## Current Architecture Summary

`NativeSqlEngine` currently stores all tables in:

```rust
Arc<RwLock<HashMap<String, NativeTable>>>
```

`NativeTable` stores:

```rust
columns: Vec<String>
column_types: Vec<ColType>
rows: HashMap<i64, NativeRow>
foreign_keys: Vec<ForeignKey>
constraints: Vec<ColumnConstraint>
table_checks: Vec<String>
```

`NativeRow` stores a mutable row payload:

```rust
cols: HashMap<String, Cell>
last_modified_lsn: u64
```

Vector values use native typed storage:

```rust
Cell::Vector {
    dim: usize,
    data: Vec<f32>,
    norm: f32,
    text: String,
}
```

Transactions are currently session-local snapshots. `BEGIN` clones the committed table map, tombstone log, and `IndexManager` state. DML mutates the live working state. `COMMIT` keeps the working state and appends staged DML to the SQL WAL. `ROLLBACK` restores the cloned table/tombstone/index snapshot.

Current rollback covers:

- table rows
- tombstones
- typed vector payloads
- vector cache invalidation
- secondary B+Tree indexes
- staged WAL discard

Current cache model:

- `BufferPool` owns per-table generation counters.
- Mutations invalidate a table by bumping its generation.
- Dimension/SoA/columnar/vector caches carry the generation at build time.
- Exact vector cache is keyed by `table + '\0' + column`, stores flat `Vec<f32>`, row ids, row norms, typed/fallback counts, and table generation.
- Current-generation vector cache entries must match row storage exactly.

Current checkpoint model:

- `native_sql.snap` stores committed table state.
- `native_sql.indexes` stores committed secondary index catalog/tree state.
- `native_sql.wal` stores SQL mutations after snapshot.
- `checkpoint()` is a no-op inside an active transaction.
- Load order is snapshot, index catalog, WAL replay, then debug validation.

Current limitations:

- No multi-session MVCC isolation.
- Shared engine clones can observe live transaction working state.
- Readers and writers use coarse table map locking.
- DDL is rejected inside active transactions.
- WAL is SQL-statement oriented, not row-version oriented.
- Rollback uses full snapshots, which is simple and safe but not scalable for concurrent transactions.

## Transaction Manager

Add a transaction manager owned by the shared engine state:

```rust
type TxId = u64;
type CommitTs = u64;
type SessionId = u64;

enum TxState {
    Active,
    Committed { commit_ts: CommitTs },
    Aborted,
}

struct TransactionRecord {
    tx_id: TxId,
    session_id: SessionId,
    state: TxState,
    start_ts: CommitTs,
    snapshot: Snapshot,
    touched_tables: HashSet<String>,
    write_set: Vec<WriteIntent>,
}

struct TransactionManager {
    next_tx_id: AtomicU64,
    next_commit_ts: AtomicU64,
    registry: RwLock<HashMap<TxId, TransactionRecord>>,
    sessions: RwLock<HashMap<SessionId, Option<TxId>>>,
}
```

Session transaction context:

```rust
struct SessionTxContext {
    session_id: SessionId,
    tx_id: Option<TxId>,
    autocommit: bool,
}
```

Transaction states:

- `ACTIVE`: can read and write.
- `COMMITTED`: has a stable commit timestamp.
- `ABORTED`: all uncommitted row versions are invisible and eligible for GC.

Global counters:

- `tx_id` identifies ownership of uncommitted writes.
- `commit_ts` / global epoch defines visibility order.
- `oldest_active_snapshot` is the minimum snapshot timestamp among active transactions and drives vacuum safety.

Oldest active snapshot:

```rust
fn oldest_active_snapshot(registry: &HashMap<TxId, TransactionRecord>) -> CommitTs {
    registry
        .values()
        .filter(|tx| matches!(tx.state, TxState::Active))
        .map(|tx| tx.snapshot.read_ts)
        .min()
        .unwrap_or(current_commit_ts())
}
```

## Isolation Model

Recommended first target: `READ COMMITTED`.

Why first:

- Simpler migration from the current engine.
- Each statement can acquire a fresh snapshot.
- Long-running readers do not retain old versions as long as snapshot isolation.
- It is enough to prevent dirty reads and enable concurrent readers/writers.
- It lets us harden MVCC storage, WAL, indexes, and vector visibility before committing to stronger semantics.

READ COMMITTED semantics:

- A statement sees rows committed before that statement starts.
- A transaction always sees its own writes.
- Dirty reads are not allowed.
- Non-repeatable reads are allowed: a later statement in the same transaction can see newer committed data.
- Phantoms are allowed: a later statement can see newly committed rows matching a predicate.
- Write-write conflict handling: first implementation should reject immediately if another active transaction owns the latest row version or row write lock.

Future target: `SNAPSHOT ISOLATION`.

- Transaction snapshot is fixed at `BEGIN`.
- Repeatable reads for row versions are guaranteed.
- Phantoms are prevented for visible committed row sets, but predicate-write conflicts are not fully serializable.
- Write-write conflicts are detected at update/delete/commit.

The engine should define the `Snapshot` type so both modes are possible:

```rust
enum IsolationLevel {
    ReadCommitted,
    SnapshotIsolation,
}

struct Snapshot {
    read_ts: CommitTs,
    own_tx_id: TxId,
    active_tx_ids: HashSet<TxId>,
    isolation: IsolationLevel,
}
```

Snapshot acquisition timing:

- READ COMMITTED: acquire a new snapshot at each statement boundary.
- SNAPSHOT ISOLATION: acquire once at `BEGIN`.

## MVCC Row Version Storage

Move from one mutable `NativeRow` per logical row to immutable row versions.

```rust
type LogicalRowId = i64;
type VersionId = u64;

struct MvccRowVersion {
    version_id: VersionId,
    logical_row_id: LogicalRowId,
    created_by_tx: TxId,
    deleted_by_tx: Option<TxId>,
    created_commit_ts: Option<CommitTs>,
    deleted_commit_ts: Option<CommitTs>,
    previous_version: Option<VersionId>,
    payload: Arc<NativeRow>,
}

struct MvccTable {
    columns: Vec<String>,
    column_types: Vec<ColType>,
    heads: HashMap<LogicalRowId, VersionId>,
    versions: HashMap<VersionId, MvccRowVersion>,
    foreign_keys: Vec<ForeignKey>,
    constraints: Vec<ColumnConstraint>,
    table_checks: Vec<String>,
}
```

Version payloads should be immutable after publication. `Arc<NativeRow>` avoids cloning large vector payloads for readers. `Cell::Vector` remains valid inside the immutable payload.

INSERT:

- Allocate a new logical row id.
- Create one version with `created_by_tx = tx_id`, `created_commit_ts = None`.
- Set `heads[row_id] = version_id`.
- Add index entries as pending/uncommitted or add committed entries at commit depending on the chosen index strategy.

UPDATE:

- Find the latest visible version.
- Acquire write ownership for the logical row id.
- Create a new immutable row payload with assigned columns updated.
- New version points to the previous head.
- Old version remains visible to snapshots that cannot see the new version.
- The previous visible version gets `deleted_by_tx = Some(tx_id)` or a separate tombstone update record.

DELETE:

- Find latest visible version.
- Acquire write ownership for logical row id.
- Mark the latest version `deleted_by_tx = Some(tx_id)`.
- Do not physically remove row versions immediately.

ABORT:

- Versions `created_by_tx = tx_id` remain invisible and become garbage.
- Delete markers `deleted_by_tx = tx_id` are ignored and later cleared or garbage-collected.

COMMIT:

- Allocate `commit_ts`.
- Set `created_commit_ts = Some(commit_ts)` for created versions.
- Set `deleted_commit_ts = Some(commit_ts)` for deleted versions.
- Mark transaction committed in registry.

## Snapshot Visibility Rules

Visibility must be computed per row version. Pseudocode:

```rust
fn is_tx_committed_before(tx_id: TxId, ts: CommitTs, registry: &TxRegistry) -> bool {
    match registry.state(tx_id) {
        TxState::Committed { commit_ts } => commit_ts <= ts,
        _ => false,
    }
}

fn is_visible(version: &MvccRowVersion, snapshot: &Snapshot, registry: &TxRegistry) -> bool {
    let own_write = version.created_by_tx == snapshot.own_tx_id;

    let created_visible =
        own_write ||
        version
            .created_commit_ts
            .map(|ts| ts <= snapshot.read_ts)
            .unwrap_or(false);

    if !created_visible {
        return false;
    }

    if registry.is_aborted(version.created_by_tx) {
        return false;
    }

    match version.deleted_by_tx {
        None => true,
        Some(delete_tx) if delete_tx == snapshot.own_tx_id => false,
        Some(delete_tx) if registry.is_aborted(delete_tx) => true,
        Some(_) => {
            match version.deleted_commit_ts {
                Some(delete_ts) => delete_ts > snapshot.read_ts,
                None => true,
            }
        }
    }
}
```

Finding a visible row:

```rust
fn visible_version_for_row(table: &MvccTable, row_id: LogicalRowId, snapshot: &Snapshot) -> Option<VersionId> {
    let mut current = table.heads.get(&row_id).copied();
    while let Some(version_id) = current {
        let version = table.versions.get(&version_id)?;
        if is_visible(version, snapshot, registry) {
            return Some(version_id);
        }
        current = version.previous_version;
    }
    None
}
```

Rules covered:

- Committed before snapshot: visible unless deleted before snapshot.
- Own transaction writes: visible to own transaction.
- Aborted versions: invisible.
- Deleted by own transaction: invisible to own transaction.
- Deleted after snapshot: old version remains visible.
- Committed after snapshot: invisible to that snapshot.
- Old version fallback: scan version chain until a visible version is found.

## Session And Concurrency Model

Introduce explicit engine sessions:

```rust
struct NativeSqlSession {
    engine: Arc<NativeSqlEngineShared>,
    tx_context: RwLock<SessionTxContext>,
}
```

`NativeSqlEngine` can remain as a compatibility wrapper for single-session use, but gateway connections should use per-connection sessions.

Concurrency goals:

- Readers do not block writers.
- Writers do not block readers.
- Writers conflict with writers on the same table/row according to the lock phase.
- DDL remains globally exclusive and outside MVCC in early phases.

READ COMMITTED lifecycle:

1. Autocommit statement starts.
2. Acquire a statement snapshot.
3. Execute reads using visibility filters.
4. For DML, create a short transaction if no explicit transaction exists.
5. Commit or abort at statement end.

Explicit transaction lifecycle:

1. `BEGIN` allocates tx id and session context.
2. Each statement gets a fresh READ COMMITTED snapshot.
3. DML versions are owned by tx id.
4. `COMMIT` publishes commit timestamp.
5. `ROLLBACK` marks tx aborted.

Write-write conflict options:

- First implementation: immediate conflict reject with a clear error.
- Later: wait with timeout, deadlock detection, or lock queueing.

Recommended first behavior:

```text
ERROR: row is concurrently modified by transaction <tx_id>
```

This avoids hidden blocking and simplifies gateway behavior.

## Lock Manager

Start simple with table-level write locks.

```rust
enum LockMode {
    TableWrite,
}

struct LockOwner {
    tx_id: TxId,
    session_id: SessionId,
}

struct LockManager {
    table_write_locks: Mutex<HashMap<String, LockOwner>>,
}
```

Rules:

- A transaction acquiring a table write lock can insert/update/delete rows in that table.
- Readers do not take table write locks.
- Locks release on commit or rollback.
- Same transaction can re-enter its own lock.
- Conflicting writer receives immediate error in Phase 1.

Deadlock handling:

- Phase 1 table locks with immediate conflict reject avoid deadlocks.
- Future row-level locks should use wait-for graph detection or lock wait timeout.

Future row-level locks:

```rust
HashMap<(TableId, LogicalRowId), LockOwner>
```

Row-level locking allows concurrent writers on different rows but requires conflict detection during index and vector updates.

## Secondary Index MVCC Strategy

Option A: index all committed versions and filter by visibility during lookup.

Pros:

- Correct for old snapshots.
- Does not require removing old entries at update commit.
- Natural fit for version chains.
- Rollback is simple because uncommitted versions are not exposed or are filtered out.

Cons:

- Index scans may return stale version candidates.
- Lookup must perform visibility filtering and deduplicate logical row ids.
- Vacuum must eventually remove dead index entries.

Option B: index latest committed versions only.

Pros:

- Smaller indexes.
- Faster point lookup for latest READ COMMITTED snapshots.

Cons:

- Old snapshots cannot use indexes correctly after updates/deletes.
- Snapshot isolation becomes difficult or requires separate historical index state.
- Commit must update indexes atomically across old and new versions.

Recommendation: Option A.

Use entries like:

```rust
struct IndexEntry {
    key: IndexKey,
    logical_row_id: LogicalRowId,
    version_id: VersionId,
    created_commit_ts: CommitTs,
    deleted_commit_ts: Option<CommitTs>,
}
```

Lookup flow:

1. B+Tree returns candidate `(logical_row_id, version_id)` entries for key/range.
2. Executor loads the version.
3. Apply `is_visible(version, snapshot)`.
4. Deduplicate logical row ids by keeping the newest visible version for the snapshot.
5. Materialize rows.

Rollback:

- If uncommitted index entries are inserted eagerly, mark them owned by tx id and filter them out for other sessions; abort removes or tombstones them.
- Simpler first implementation: collect index additions in transaction write set and install them at commit.

Checkpoint/reload:

- Persist committed MVCC index entries.
- Rebuild index from row versions if catalog is missing or corrupt.
- Never persist uncommitted index entries.

## Vector Cache And ANN MVCC Strategy

Exact vector scan:

- A snapshot-visible scan must iterate visible row versions, not current table heads only.
- Ranking uses only versions visible to the statement snapshot.
- Own writes are visible to the owning transaction.
- Versions committed after snapshot are invisible.
- Deleted-before-snapshot versions are invisible.

Vector cache key should include MVCC visibility scope:

```rust
struct VectorCacheKey {
    table_id: TableId,
    column_id: ColumnId,
    visibility_epoch: CommitTs,
}
```

For READ COMMITTED, caching every statement timestamp is too expensive. Recommended first cache model:

- Keep a latest-committed vector column cache for autocommit/latest snapshots.
- For transactions with own writes or older snapshots, use visibility filtering over row/version metadata.
- Cache row vector payloads by version id where useful.

Exact cache representation:

```rust
struct MvccVectorColumnCache {
    table_generation: u64,
    max_commit_ts: CommitTs,
    version_ids: Vec<VersionId>,
    logical_row_ids: Vec<LogicalRowId>,
    data: Vec<f32>,
    norms: Vec<f32>,
    dim: usize,
}
```

Visibility-aware top-k:

```rust
for candidate in cache.version_ids {
    let version = table.version(candidate);
    if !is_visible(version, snapshot, registry) {
        continue;
    }
    score_and_push_topk(candidate.logical_row_id, version.payload.vector);
}
```

HNSW / ANN:

- HNSW should not physically remove old versions synchronously.
- Store `version_id` as the document id, not only logical row id.
- ANN search returns candidate version ids.
- Executor filters candidates by snapshot visibility.
- If too many candidates are invisible, over-fetch and retry with larger `ef_search` or fall back to exact scan.
- Vacuum removes ANN nodes only after `oldest_active_snapshot` no longer needs them.

Rollback/commit invalidation:

- Commit increments table MVCC generation and latest commit epoch.
- Abort marks owned versions invisible and invalidates latest caches if transaction touched latest cacheable rows.
- Exact vector cache can remain structurally valid if visibility filtering is applied, but latest-cache metadata must advance on commit.

## WAL And Recovery

Current SQL WAL is not sufficient for full MVCC crash recovery because it does not encode row version ownership or commit state. Introduce row-version WAL records in phases.

Record types:

```rust
enum WalRecord {
    TxBegin { tx_id, session_id, start_ts },
    RowVersionInsert { tx_id, table, row_id, version_id, payload },
    RowVersionDelete { tx_id, table, row_id, version_id },
    IndexEntryInsert { tx_id, index, key, row_id, version_id },
    TxCommit { tx_id, commit_ts },
    TxAbort { tx_id },
    CheckpointBegin { checkpoint_ts },
    CheckpointEnd { checkpoint_ts },
}
```

Crash recovery:

1. Load latest checkpoint.
2. Replay row-version WAL records after checkpoint.
3. Build transaction registry from begin/commit/abort records.
4. Treat transactions without commit records as aborted.
5. Rebuild or validate secondary indexes.
6. Rebuild vector caches lazily.

Checkpoint under active transactions:

- Checkpoint may include committed versions with `commit_ts <= checkpoint_ts`.
- It must not include uncommitted versions unless there is a recovery protocol that can abort them on replay.
- Simplest first rule: checkpoint only committed versions and transaction registry commit state.
- Active transaction write sets remain WAL-only and are aborted on crash unless a commit record exists.

Compatibility:

- During migration, keep SQL WAL for legacy mode.
- MVCC mode uses version WAL records.
- Loader detects snapshot format version.

## Vacuum And Garbage Collection

Vacuum safety depends on `oldest_active_snapshot`.

A version can be reclaimed if:

- It is aborted and no active transaction owns or references it.
- Or it is deleted and `deleted_commit_ts < oldest_active_snapshot`.
- And it is not the visible version for any active snapshot.

Index entries can be reclaimed if:

- Their version was reclaimed.
- Or their delete timestamp is older than `oldest_active_snapshot`.

Vector cache state can be reclaimed if:

- It references reclaimed version ids.
- Its visibility epoch is older than all active snapshots and not the latest cache.

Vacuum modes:

- Manual `VACUUM`: run bounded cleanup synchronously.
- Background vacuum: periodic bounded cleanup based on version count, tombstone count, and memory pressure.

Recommended first implementation:

- Manual vacuum only.
- No compaction of active version chains.
- Add metrics before background cleanup.

## Compatibility And Migration

Migration from current engine:

- Each current `NativeRow` becomes one committed MVCC version.
- Assign `created_by_tx = SYSTEM_TX`.
- Assign `created_commit_ts = checkpoint_ts` or `0` for legacy snapshots.
- `deleted_by_tx = None`.
- Existing tombstones can be migrated into deleted marker versions only if their row payload is still available; otherwise preserve them as tombstone metadata for validation/history until GC.
- Existing `Cell::Vector` payloads are reused without reparsing.
- Existing secondary indexes can be loaded as latest committed indexes, then rebuilt into version-aware indexes in MVCC mode.

Snapshot format:

```rust
enum SnapshotFormat {
    LegacyNativeTableV1,
    MvccNativeTableV2,
}
```

Fallback compatibility:

- Keep current non-MVCC engine path behind a feature/config flag during migration.
- Loader can run in compatibility mode if it sees legacy snapshot and MVCC is disabled.
- MVCC-enabled loader upgrades in memory first; writing a new MVCC checkpoint should be an explicit format bump.

## Testing Plan

Required future tests:

- concurrent reader sees old committed row while writer has uncommitted update
- concurrent reader sees committed row after writer commits under READ COMMITTED next statement
- explicit transaction own writes visible
- rollback hides inserted versions and restores old visible versions
- commit publishes inserted/updated/deleted versions
- write-write conflict on same row
- table-level write conflict on same table in Phase 1
- stale snapshot under SNAPSHOT ISOLATION does not see later commits
- index point lookup filters invisible versions
- index range lookup deduplicates logical row ids
- vector exact top-k filters invisible versions
- vector own writes appear in same transaction
- vector delete/update rollback restores ranking
- HNSW over-fetch handles invisible candidates
- vacuum does not remove versions needed by active snapshots
- vacuum removes old versions after oldest active snapshot advances
- crash recovery aborts active uncommitted transactions
- crash recovery preserves committed row versions and index entries
- checkpoint with active transactions excludes uncommitted versions
- migration from legacy snapshot produces the same visible SQL results
- deterministic fuzz with concurrent sessions, random commit/rollback, index lookup, vector top-k, checkpoint/reload

## Phased Implementation Plan

### Phase 1: Transaction Manager And Sessions

- Add global `TransactionManager`.
- Add `SessionId` and session transaction context.
- Keep current row storage.
- Preserve current behavior by default.
- Add tests for transaction registry lifecycle and per-session state.

Exit criteria:

- Multiple sessions can independently `BEGIN`, `COMMIT`, and `ROLLBACK` in metadata.
- No storage visibility changes yet.

### Phase 2: MVCC Row Version Storage

- Add `MvccTable` and `MvccRowVersion`.
- Implement legacy table-to-MVCC in-memory migration.
- Implement insert/update/delete version creation behind a flag.
- Keep table-level write lock.

Exit criteria:

- Single-session MVCC mode matches current SQL behavior.
- Existing vector correctness tests pass in MVCC mode.

### Phase 3: Visibility-Aware Scans

- Add `Snapshot`.
- Apply `is_visible()` to full table scans, WHERE filters, ORDER BY, aggregation, joins, and vector exact scan.
- Implement READ COMMITTED statement snapshots.

Exit criteria:

- No dirty reads across two sessions.
- Readers do not block writers for row visibility.

### Phase 4: MVCC Secondary Indexes

- Store `(key, row_id, version_id)` entries.
- Filter by visibility during point/range lookup.
- Deduplicate logical row ids.
- Rebuild/validate version-aware indexes.

Exit criteria:

- Indexed lookup parity with full scan for concurrent committed/uncommitted versions.

### Phase 5: MVCC Vector Cache And HNSW

- Store vector cache entries by version id.
- Add visibility filtering in exact vector top-k.
- Add HNSW candidate visibility filtering and over-fetch fallback.
- Invalidate latest caches on commit/abort.

Exit criteria:

- Vector top-k parity with brute force under concurrent transactions.

### Phase 6: WAL Recovery Hardening

- Add row-version WAL records.
- Add transaction begin/commit/abort records.
- Replay committed transactions and abort incomplete ones.
- Version snapshot format to MVCC.

Exit criteria:

- Crash/reload tests pass with active, committed, and aborted transactions.

### Phase 7: Vacuum And GC

- Track oldest active snapshot.
- Reclaim aborted versions and deleted old versions.
- Reclaim stale index/vector entries.
- Add manual vacuum first, background vacuum later.

Exit criteria:

- Vacuum safety tests pass with active snapshots.

### Phase 8: Performance Optimization

- Replace table-level write locks with row-level locks where needed.
- Optimize version chain traversal.
- Add latest-visible row head cache for READ COMMITTED.
- Optimize visibility-aware vector scan and index lookup.
- Add metrics for version chain length, dead versions, invisible index candidates, and ANN over-fetch rate.

Exit criteria:

- MVCC mode approaches current exact vector and indexed lookup performance for common latest-snapshot reads.

## Risks

- Visibility bugs can silently corrupt query results; validator and full-scan/index parity tests are mandatory.
- Indexing all versions can increase memory until vacuum is mature.
- HNSW with invisible candidates can hurt recall unless over-fetch/fallback is tuned.
- SQL WAL compatibility during migration can be confusing; snapshot format versioning must be explicit.
- Table-level write locks will limit write concurrency but are safer for the first MVCC storage cut.
- Snapshot isolation later may expose assumptions made under READ COMMITTED, so `Snapshot` must be designed for both from Phase 1.

## Release Invariants For MVCC Mode

- No dirty reads.
- Own transaction writes are visible to the owning transaction.
- Aborted versions are never visible.
- A committed checkpoint never contains rolled-back data.
- A secondary index lookup and a full scan return the same visible logical row ids for the same snapshot.
- A vector exact top-k query and brute-force visible-version computation return the same ranking for the same snapshot.
- Vacuum never removes a version visible to any active snapshot.
- WAL recovery treats transactions without commit records as aborted.
