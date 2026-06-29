# QMvir HTAP Guide

Hybrid Transaction/Analytical Processing (HTAP) in QMvir combines OLTP row storage with durable column segments, MVCC transactions, and a cost-based planner.

## Architecture

```
                    ┌─────────────────────────────────────┐
                    │         NativeSqlEngine             │
                    │  BEGIN/COMMIT/ROLLBACK → tx_mgr     │
                    └──────────────┬──────────────────────┘
                                   │
         ┌─────────────────────────┼─────────────────────────┐
         ▼                         ▼                         ▼
  TableMvccStore           Row heap (TableStore)     HtapPlanner
  version chains           per-table RwLock          EXPLAIN + path pick
         │                         │
         │ COMMIT                  │
         ▼                         ▼
  columnizer ──────────► column_segments/ (QMCS mmap)
  wal_archive.json       pitr_manifest.json
```

## MVCC transactions

| Command | Behavior |
|---------|----------|
| `BEGIN` | Starts real transaction in `TransactionManager` |
| `COMMIT` | Publishes versions, syncs heap, columnizes, updates WAL archive |
| `ROLLBACK` | Aborts pending versions, rebuilds MVCC from heap |
| Autocommit DML | Implicit `BEGIN` → DML → `COMMIT` per statement |

Visibility helpers (Rust API):

- `htap_visible_row(table, id)` — MVCC-aware point lookup
- `htap_visible_row_ids(table)` — scan without cloning catalog
- `htap_visible_row_count(table)` — `COUNT(*)` without full map clone

## Planner paths

Run `EXPLAIN <sql>` to see the chosen path:

| Path | When |
|------|------|
| **Index Scan** | `WHERE id = ?` or indexed equality |
| **Column Scan** | Analytics (`SUM`, `COUNT`, `GROUP BY`, `BETWEEN`) with ≥4096 rows and durable column segments |
| **Seq Scan** | Default row iteration |
| **HNSW Vector Scan** | `ORDER BY col <-> query` (vector KNN) |
| **GIN/Inverted Scan** | Full-text `@@` predicates |

HTAP changes **do not alter vector distance math** — KNN still uses HNSW / exact scan paths in `handle_select_vector_knn`.

## Durable column segments

On each `COMMIT` (and autocommit mutation), `columnizer` writes `QMCS` files under:

```
<data-dir>/column_segments/<table>/<column>/cseg_00000001.qmcs
```

`SELECT SUM(col)`, `AVG(col)`, `COUNT(*)` use mmap reads when Column Scan is selected (no `to_native_map()` on hot paths).

## Point-in-time recovery (PITR)

```bash
# Backup with WAL
qm --data-dir ./data backup -o snap.qmvb --pitr

# Plan restore LSN for a timestamp
qm --data-dir ./data pitr plan --timestamp 1719000000

# Materialize a new data directory at that LSN
qm --data-dir ./data pitr restore --timestamp 1719000000 -o ./pitr_out
qm --data-dir ./data pitr restore --lsn 42 -o ./pitr_out
```

Files:

- `native_sql.wal` — append-only mutation log
- `wal_archive.json` — LSN ↔ wall-clock index (updated on commit)
- `pitr_manifest.json` — output of `pitr plan`

## Certification

```bash
qm --data-dir ./data htap certify
qm --data-dir ./data htap certify --isolation
```

Gates checked:

1. MVCC visibility wired
2. Durable column segments (`data_dir` present)
3. HTAP planner enabled
4. WAL archive index (after first durable commit)

Isolation battery (with `--isolation`):

- Read-your-writes inside transaction
- Aborted writes invisible after `ROLLBACK`
- Commit atomicity + no leaked transactions
- Autocommit uses real tx_mgr

## Mixed workload benchmark

```bash
cargo build --release --bin qm --no-default-features
python3 scripts/htap_mixed_benchmark.py --engine-bin ./qm_engine/target/release/qm --json
```

Runs concurrent OLTP `INSERT` and OLAP `SELECT SUM ... BETWEEN` workers.

## Cluster analytics routing

When `QM_CLUSTER_*` is configured, analytics `SELECT` (aggregates, vector KNN) may route to an async replica. See `qm guide cluster`.

## CLI quick reference

```bash
qm guide htap              # built-in HTAP guide
qm htap certify [--isolation]
qm pitr plan --timestamp T
qm pitr restore --timestamp T -o ./out
qm sql "EXPLAIN SELECT SUM(val) FROM t"
```

## Performance notes

- Hot reads use `table_read_guard()` instead of `to_native_map()` where possible.
- `to_native_map()` remains for FK checks, joins, and transaction snapshots (multi-table).
- Vector KNN fast paths (`try_fast_id_vector_knn`) are unchanged.
- Run `qm benchtest --profile quick` after engine changes to check for regressions.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Column Scan not used | Table needs ≥4096 rows + `column_segments/` populated after commits |
| PITR plan fails | Ensure `wal_archive.json` exists (requires durable `data_dir` commits) |
| ROLLBACK still visible | Run `qm htap certify --isolation` |
| Vector results differ | Compare with exact scan: omit HNSW index or use small table |
