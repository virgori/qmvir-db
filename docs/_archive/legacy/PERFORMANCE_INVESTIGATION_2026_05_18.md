# Performance Investigation Methodology - 2026-05-18

Scope: QM Engine / NativeSqlEngine 5.4.0 rc1, local performance gate.

## Environment

- OS: macOS-26.5 arm64
- CPU: 8 logical CPUs, Apple arm64
- RAM: 16 GiB
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build mode: release wheel built with `maturin build --release`
- Feature flags: default for Python extension benchmarks; `--no-default-features` for Rust release-gate tests
- Durability: NativeSqlEngine default. In-memory engines are used for non-persistent executor microbenchmarks; persistent engines are used for WAL/checkpoint/recovery workloads.
- Cache policy: warm-cache loops unless a workload name explicitly says cold/recovery.
- Dataset size: 10,000 rows for the full investigation script.
- Iterations: 1,000 for full latency loops unless the workload is capped by the script for cost.
- Warmup: 10 iterations for latency loops, 1 warmup for large batch loops.
- PostgreSQL: local PostgreSQL 17.9 via `/tmp` socket, DSN `dbname=postgres user=gengyang host=/tmp`.

## Commands

```bash
maturin build --release
python3 -m pip install --force-reinstall qm_engine/target/wheels/qm_engine-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --dsn 'dbname=postgres user=gengyang host=/tmp' --output docs/postgres_comparison_2026_05_18.json
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

`cargo build --manifest-path qm_engine/Cargo.toml --release` was also tried earlier and failed at link time for the PyO3 extension-module symbols on macOS. The release benchmark path for this package is the `maturin build --release` wheel, which succeeded.

## Fairness Rules

- PostgreSQL comparisons use the same simple schema shape: integer primary key, integer predicate column, text payload.
- Query shapes are matched only for supported NativeSqlEngine operations.
- PostgreSQL runs with `synchronous_commit=on`.
- NativeSqlEngine in-memory microbenchmarks are not treated as a durability-equivalent claim against PostgreSQL's server durability path.
- PostgreSQL results are local-machine evidence, not a universal database ranking.

