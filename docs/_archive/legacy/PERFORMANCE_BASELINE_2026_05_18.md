# Performance Baseline - 2026-05-18

Baseline status: approved local-machine baseline for QM Engine / NativeSqlEngine 5.4.0 rc1.

Baseline file: `docs/native_sql_benchmark_baseline.json`

This baseline was refreshed after the 2026-05-19 hot-path optimization pass that removed an accidental O(n^2) insert cost from vector dimension validation, reduced transaction snapshot overhead for read-only transactions, batched index stats writes, and avoided DNS lookup for literal gateway bind addresses.

## Command

```bash
maturin build --release
python3 -m pip install --force-reinstall qm_engine/target/wheels/qm_engine-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
cp docs/native_sql_benchmark_last.json docs/native_sql_benchmark_baseline.json
```

## Environment

- OS: macOS-26.5-arm64-arm-64bit-Mach-O
- CPU count: 8
- RAM: 16.00 GiB observed by the full investigation script
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build mode: release
- Feature flags: default
- Peak RSS during release benchmark: 26.72 MB

## Result Table

| Workload | p50 ms | p95 ms | p99 ms | Throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.0033 | 0.0038 | 0.0056 | 278862.24 |
| native_sql.simple_select | 0.0028 | 0.0030 | 0.0031 | 343736.16 |
| native_sql.simple_update | 0.0025 | 0.0026 | 0.0027 | 392631.72 |
| native_sql.simple_delete | 0.0051 | 0.0053 | 0.0054 | 192249.13 |
| native_sql.predicate_path | 0.0023 | 0.0025 | 0.0080 | 379356.77 |
| native_sql.mvcc_read_write | 0.0326 | 0.0563 | 0.0702 | 29606.53 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 238547.55 |
| native_sql.vector_cache_hot_path | 0.0040 | 0.0042 | 0.0055 | 236910.69 |
| python_gateway.startup_shutdown | 0.2652 | 0.5115 | 0.7172 | 3425.32 |

## Baseline Approval

Approved as a local release baseline. The benchmark completed from a rebuilt release wheel, recorded environment metadata, included p50/p95/p99 where applicable, included throughput, and recorded peak RSS.

## Limitations

- This is a local baseline, not a universal performance claim.
- NativeSqlEngine non-persistent microbenchmarks are not durability-equivalent to PostgreSQL server benchmarks.
- Gateway startup/shutdown is capped at 25 measured iterations.
- PostgreSQL comparison was attempted after the refresh but did not run because local PostgreSQL rejected the default DSN: role `postgres` does not exist.
- Future baseline changes must be made from a rebuilt release wheel and after correctness/crash tests pass.
