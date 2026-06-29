# NativeSqlEngine Release Invariants

Date: 2026-05-15

## Transaction Semantics

- `BEGIN` starts one session-local DML transaction.
- Nested `BEGIN` is rejected.
- `COMMIT` without `BEGIN` and `ROLLBACK` without `BEGIN` return clear errors.
- Reads inside a transaction see writes made by that transaction.
- `ROLLBACK` restores the exact visible table, tombstone, vector, and secondary index state captured at `BEGIN`.
- `COMMIT` makes staged DML durable through WAL append and checkpoint eligibility.
- `CREATE TABLE`, `DROP TABLE`, `CREATE INDEX`, `DROP INDEX`, `ALTER TABLE`, `COPY`, and `VACUUM` are rejected inside active transactions.
- This implementation is session-local and is not MVCC across shared engine clones or concurrent sessions.

## Persistence And Checkpointing

- `checkpoint()` is a no-op while a transaction is active.
- Uncommitted transaction changes must never be written into `native_sql.snap` or `native_sql.indexes`.
- Committed table state is snapshotted into `native_sql.snap`.
- Committed secondary index catalog/tree state is snapshotted into `native_sql.indexes`.
- WAL replay runs after snapshot load and before debug internal validation.
- A restart/reload must preserve committed scalar, text, vector, and indexed rows.
- A restart/reload must exclude rolled-back scalar, text, vector, and indexed rows.

## Secondary Indexes

- No secondary index may contain a dangling row id.
- No secondary index may contain a stale key for a live row whose indexed value has changed.
- Every live row with an indexed non-null value must be searchable from the matching secondary index.
- Rollback must restore both B+Tree pages and IndexManager metadata.
- Manual index state is never independently trusted; validation compares index entries against table rows.

## Vector Storage And Cache

- Native vector rows use `Cell::Vector { dim, data, norm, text }`.
- `dim` must match `data.len()`.
- Vector data and cached norm must be finite.
- Empty, malformed, non-finite, and dimension-mismatched vector literals must fail clearly.
- Current-generation vector cache entries must match live row ids, vector data, and norms exactly.
- Older-generation vector cache entries are inert; query paths must ignore them via table generation checks.
- Vector rankings are deterministic for a fixed table snapshot, query vector, operator, and limit.

## Validation

- `engine.validate_internal_state()` is the public diagnostic entry point.
- Debug builds call it after `COMMIT`, after `ROLLBACK`, after checkpoint reload, and after `VACUUM`.
- The validator checks secondary index parity, tombstone table references, duplicate row ids, integer `id` column consistency, vector payload validity, vector cache shape, and current-generation vector cache parity.

## Known Limits

- Multi-session MVCC isolation is not implemented.
- Transactional DDL is not implemented.
- PostgreSQL/pgvector comparison requires an external DSN and is not part of the local default verification.
