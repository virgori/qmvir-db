# PostgreSQL Benchmark Setup

This comparison is optional release evidence. It must run with a valid local
PostgreSQL DSN before any QMvir-vs-PostgreSQL performance claim is approved.

## Create Role and Database

Example local setup:

```bash
createuser --createdb qm_bench
createdb --owner=qm_bench qm_bench
```

If password authentication is required:

```bash
psql postgres -c "ALTER USER qm_bench WITH PASSWORD 'change-me';"
```

## DSN

Set `POSTGRES_DSN` before running the comparison:

```bash
export POSTGRES_DSN="postgresql://qm_bench:change-me@localhost:5432/qm_bench"
```

The comparison script writes only a sanitized DSN to JSON output.

## Run

```bash
POSTGRES_DSN="postgresql://qm_bench:change-me@localhost:5432/qm_bench" \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --durability-mode wal_fsync \
  --output docs/postgres_comparison_latest.json \
  --strict
```

Use `--strict` for release evidence. In strict mode the command exits non-zero
if PostgreSQL is unavailable.

## Durability Modes

- `memory`: QM in-memory mode; PostgreSQL is still server-backed and not a direct durability match.
- `relaxed`: PostgreSQL `synchronous_commit=off`; useful for relaxed local comparison.
- `wal_no_fsync`: records a relaxed WAL comparison intent; PostgreSQL uses `synchronous_commit=off`.
- `wal_fsync`: PostgreSQL uses `synchronous_commit=on`.
- `checkpoint_pressure`: checkpoint-heavy QM workloads must be interpreted separately from ordinary DML.

The script records PostgreSQL `server_version`, `fsync`, and effective
`synchronous_commit` when the connection succeeds.

## Workloads

The comparison covers supported shapes only:

- insert one
- select by primary key
- update by primary key
- delete by primary key
- indexed integer equality
- indexed string equality, unique
- indexed string equality, duplicate-heavy
- COUNT indexed equality
- range predicate
- transaction commit
- transaction rollback

Do not compare direct Rust core numbers to PostgreSQL server-mode numbers as a
general DBMS superiority claim. Direct-core QM numbers are architecture-specific
embedded-engine evidence.
