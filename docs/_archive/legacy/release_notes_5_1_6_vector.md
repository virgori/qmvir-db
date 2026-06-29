# QMvir 5.1.6 Vector Fix Release Notes

## Vector Correctness

- Fixed pgvector operator dispatch so `<->`, `<=>`, and `<#>` use distinct L2,
  cosine-distance, and negative-inner-product scorers.
- Added brute-force golden tests that verify exact top-k parity for all three metrics.
- Added guards so malformed, empty, non-finite, and dimension-mismatched vector literals fail
  clearly instead of silently falling back to the wrong path.

## Native Vector Storage

- Added native `Cell::Vector { dim, data, norm, text }` row payload.
- `INSERT` and `UPDATE` of `::vector` literals parse vectors once, compute norms once, and keep
  pgvector-compatible text output for SQL display.
- Exact SQL vector scan now builds cache from typed rows; legacy text rows still work through a
  fallback parser.

## Latency

- Warm exact SQL vector path is roughly 0.17-0.19 ms for N=2000, dim=128, top-k=10 in the
  latest local release benchmark.
- Cold cache and rebuild-after-mutation paths are roughly 0.33-0.35 ms for native rows.
- Benchmark artifacts now report typed row count, text fallback count, cold/warm/rebuild latency,
  operator, dimension, row count, and limit.

## Cache Safety

- Hardened invalidation for insert, update, delete, vacuum, drop/create with same table name,
  multiple vector columns, and multiple tables with the same column name.
- Added tests for zero-vector cosine behavior, LIMIT 0, LIMIT greater than row count, and
  checkpoint/reload preservation.

## Transactions And Indexes

- `BEGIN` / `COMMIT` / `ROLLBACK` now provide real session-local DML transactions.
- Rollback restores table rows, tombstones, vector payloads/caches, and secondary B+Tree index
  state.
- DDL, COPY, and VACUUM are rejected inside active transactions.
- Secondary indexes are persisted through `native_sql.indexes` during checkpoint and restored on
  reload.
- Added `engine.validate_internal_state()` for release diagnostics; debug builds validate after
  commit, rollback, reload, and vacuum.
- Added crash-style recovery and deterministic mutation fuzz tests for scalar/text/vector/indexed
  rows.

## Known Limitations

- PostgreSQL/pgvector comparison remains `not_run` unless a local DSN is configured.
- Default-feature `cargo test` is still blocked by Python/PyO3 `_Py...` linking in this local
  environment; vector verification uses `cargo test --lib --no-default-features vector_ -- --nocapture`.
- Transaction isolation is session-local and is not MVCC across concurrent sessions/clones.
