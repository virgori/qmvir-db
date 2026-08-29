# PostgreSQL Comparison Benchmark

Use `scripts/compare_postgres_native_sql.py` to find where QMvir is still slower than PostgreSQL on the same fixed SQL workload.

## Prerequisites

- A local or remote PostgreSQL database reachable through `psql`.
- The QM Python extension built in the current environment:

```bash
python3 -m maturin develop --release --features extension-module
```

## Quick Run

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --rows 100000 \
  --iterations 100 \
  --suite core \
  --output docs/profiles/postgres_comparison_latest.json \
  --strict
```

## Extended Run

Use the extended suite after the core suite is clean. It probes less-specialized
planner paths where PostgreSQL is usually strong:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --rows 100000 \
  --iterations 100 \
  --suite extended \
  --output docs/profiles/postgres_comparison_extended_latest.json \
  --strict
```

To run both suites in one report:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --rows 100000 \
  --iterations 100 \
  --suite all \
  --output docs/profiles/postgres_comparison_all_latest.json \
  --strict
```

## Persistent WAL Run

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --qm-mode persistent-wal \
  --qm-sync-policy per-commit \
  --rows 100000 \
  --iterations 100 \
  --suite core \
  --output docs/profiles/postgres_comparison_persistent_wal_latest.json \
  --strict
```

## What It Measures

The benchmark seeds the same tables in PostgreSQL and QMvir, then measures:

- primary-key point lookup
- indexed equality count
- primary-key range count
- large-column `SUM`
- large-column `AVG`
- range `SUM`
- low-cardinality `GROUP BY`
- filtered join
- large `ORDER BY ... LIMIT`

The extended suite adds:

- compound predicates
- low-selectivity text/status filters
- filtered `ORDER BY ... LIMIT`
- `ORDER BY ... LIMIT ... OFFSET`
- text `LIKE` count
- filtered aggregate
- high-cardinality `GROUP BY`
- generic two-table join
- three-table join with a non-account filter

PostgreSQL `SELECT` timings use `EXPLAIN ANALYZE` execution time. QM timings use embedded Python-to-PyO3 wall-clock time. Results include planner details where available and a `slower_than_postgres` section ranked by QM/PostgreSQL mean-latency ratio.
