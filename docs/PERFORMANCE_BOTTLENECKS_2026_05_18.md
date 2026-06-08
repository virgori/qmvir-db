# Performance Bottlenecks - 2026-05-18

This document was refreshed on 2026-05-19 after the extreme hot-path optimization pass requested for delete, insert, MVCC read/write overhead, and then gateway lifecycle.

## Fixed Bottlenecks

| Bottleneck | Evidence | Code path | Fix | Result |
| --- | --- | --- | --- | --- |
| Primary-key `UPDATE` scanned every row to find `WHERE id = N`. | Earlier pass: `single.update_by_pk` p50 about 0.083 ms; `batch.update_1000_by_pk` p50 about 862 ms. | `NativeSqlEngine::handle_update` in `qm_engine/src/gateway/native_sql.rs` | Added a narrow `id = integer` HashMap lookup path while preserving constraint, FK, index, and cache logic. | Current: `single.update_by_pk` p50 0.0025 ms; `batch.update_1000_by_pk` p50 2.6675 ms. |
| Primary-key `DELETE` scanned every row to find `WHERE id = N`. | Earlier pass: `single.delete_by_pk` p50 about 0.568 ms; `batch.delete_500_by_pk` p50 about 3825 ms. | `NativeSqlEngine::handle_delete` | Added the same narrow `id = integer` lookup path, preserving RETURNING/FK/index behavior. | Current: `single.delete_by_pk` p50 0.0052 ms; `batch.delete_500_by_pk` p50 3.1037 ms. |
| Insert into tables with `id PRIMARY KEY` scanned all existing rows for the primary-key UNIQUE check. | Earlier pass: `single.insert_one` p50 about 0.098 ms; `batch.insert_10000` p50 about 9837 ms. | `NativeSqlEngine::enforce_constraints_on_insert` | Special-cased unique `id` to use `table.rows.contains_key()` plus a new-row set. Other UNIQUE columns still use the full semantic scan. | Helped, but a larger O(n^2) vector-validation issue remained. |
| Insert validated vector dimensions by scanning existing rows for every column on every insert, even for non-vector tables. | Before this pass: targeted 10k inserts took ~4390 ms; full `batch.insert_10000` p50 4508.50 ms. | `NativeSqlEngine::validate_insert_vector_dimensions` | First inspect incoming rows. Only if a new vector value exists, look up existing dimensions for those vector columns. Invalid vector literals are still rejected. | Current targeted 10k inserts: ~34 ms. Full `batch.insert_10000` p50 34.3215 ms. |
| Insert stats wrote under repeated locks and allocated an intermediate numeric sample vector. | Code-path analysis showed per-column stats locking and extra allocation in `handle_insert`. | `IndexManager::record_write`, `IndexManager::record_numeric_value`, `NativeSqlEngine::handle_insert` | Added batched `record_writes` and `record_numeric_values`, and narrowed index-change cloning to tables that actually have indexes. | Contributes to current `single.insert_one` p50 0.0035 ms and `vector.insert` p50 0.0042 ms. |
| Read-only transactions paid write-transaction snapshot cost. | MVCC read-only benchmark was slower than necessary; begin cloned table/index state even if no DML followed. | `TransactionState`, `begin_transaction`, `ensure_transaction_snapshot`, `commit_transaction`, `rollback_transaction` | Made table/tombstone/index snapshots lazy and only materialized on first DML in a transaction. | `mvcc.read_only_transaction` p50 0.0036 ms; `mvcc.concurrent_writer_readers` p50 improved from 1.7629 ms to 0.7846 ms in same run series. |
| Gateway startup used DNS resolution for literal bind addresses. | Lifecycle path called `lookup_host` for `127.0.0.1`. | `PostgresGateway::bind_listener` in `qm_engine/src/gateway/server.rs` | Parse literal `host:port` as `SocketAddr` first; fall back to DNS only for non-literals. | Release benchmark `python_gateway.startup_shutdown` p50 0.2652 ms. |
| B+Tree secondary index lookup missed duplicate keys split across leaves. | Performance benchmark initially panicked during secondary-index recovery validation: missing `row_id` for duplicate key. | `BPlusTree::search` and `BPlusTree::delete` in `qm_engine/src/index/bplus_tree.rs` | Search now walks the duplicate-key leaf span; delete checks the full span and starts from the routed leaf first. | Correctness fixed; recovery benchmark passes. |

## Current Measurements

| Workload | p50 ms | p95 ms | p99 ms | Throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| single.insert_one | 0.0035 | 0.0045 | 0.0062 | 267242.70 |
| single.delete_by_pk | 0.0052 | 0.0054 | 0.0055 | 188533.97 |
| batch.insert_10000 | 34.3215 | 39.7327 | 39.7327 | 28.28 batches/s |
| batch.delete_500_by_pk | 3.1037 | 6.6577 | 11.9556 | 242.47 batches/s |
| mvcc.read_only_transaction | 0.0036 | 0.0041 | 0.0047 | 263440.96 |
| mvcc.write_transaction | 0.1264 | 0.1475 | 0.1649 | 7875.23 |
| mvcc.concurrent_writer_readers | 0.7846 | 1.0100 | 1.1374 | 1261.41 |
| gateway.startup_shutdown_callback | 0.3382 | 0.4386 | 0.4427 | 2884.59 |

## Remaining Bottlenecks

| Bottleneck | Evidence | Likely cause | Status |
| --- | --- | --- | --- |
| OR predicate scans remain slower than simple predicates. | `predicate.or_scan` p50 0.7861 ms vs `predicate.range_scan` p50 0.0022 ms. | Predicate strings are evaluated recursively per row; no parsed predicate AST is reused. | Non-blocking performance caveat. Next pass should add parsed predicate plan reuse. |
| Indexed string equality is slower than numeric indexed equality. | `predicate.indexed_string_equality` p50 0.2121 ms vs numeric equality p50 0.0185 ms. | String key conversion/comparison and duplicate-key path cost. | Non-blocking caveat; needs focused string-index profiling. |
| Checkpoint pressure remains expensive relative to in-memory DML. | `wal.commit_with_checkpoint_pressure` p50 10.0009 ms; `wal.bulk_insert_1000_then_checkpoint` p50 21.7920 ms. | Expected synchronous snapshot/checkpoint work, but still a real optimization target. | Non-blocking for current scoped release; do not claim full PostgreSQL durability-equivalent performance. |
| Gateway lifecycle still has measurable lifecycle overhead. | `gateway.startup_shutdown_callback` p50 0.3382 ms; release gate p50 0.2652 ms. | Runtime/listener/server task lifecycle dominates after literal-address bind optimization. | Non-blocking; next optimization would require pooling/reuse semantics or a benchmark mode that does not model full lifecycle. |

No severe unexplained bottleneck remains for simple insert/delete/update/select, MVCC smoke, or vector insert in the supported NativeSqlEngine scope measured here.
