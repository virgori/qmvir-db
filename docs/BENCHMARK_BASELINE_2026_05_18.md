# Benchmark Baseline - 2026-05-18

Status: **Approved local-machine baseline**

This baseline is approved only for local QM Engine 5.4.0 rc1 release gating on
this machine. It is not a universal performance claim.

## Command

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

The resulting `docs/native_sql_benchmark_last.json` was promoted to
`docs/native_sql_benchmark_baseline.json` with baseline metadata.

## Environment

| Field | Value |
| --- | --- |
| OS | macOS-26.5-arm64-arm-64bit-Mach-O |
| CPU count | 8 |
| Machine | arm64 |
| Processor | arm |
| Python | 3.13.3 |
| Rust | rustc 1.94.0 (4a4ef493e 2026-03-02) |
| Build mode metadata | release |
| Feature flags | default |
| Peak RSS | 31.625 MB |

## Results

| Benchmark | Iterations | p50 ms | p95 ms | p99 ms | Throughput ops/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| `native_sql.simple_insert` | 1000 | 1.2757 | 2.4230 | 2.5259 | 788.86 |
| `native_sql.simple_select` | 1000 | 0.0137 | 0.0148 | 0.0215 | 71166.57 |
| `native_sql.simple_update` | 1000 | 0.4643 | 0.5156 | 0.5781 | 2114.84 |
| `native_sql.simple_delete` | 500 | 5.5904 | 6.1132 | 7.8631 | 169.54 |
| `native_sql.predicate_path` | 1000 | 0.0105 | 0.0107 | 0.0109 | 94067.11 |
| `native_sql.mvcc_read_write` | 500 | 0.7687 | 1.4114 | 1.4592 | 1326.44 |
| `native_sql.concurrent_read_write_smoke` | 496 | n/a | n/a | n/a | 8703.71 |
| `native_sql.vector_cache_hot_path` | 500 | 0.0313 | 0.0318 | 0.0378 | 31682.25 |
| `python_gateway.startup_shutdown` | 25 | 0.3325 | 0.3756 | 0.4017 | 3073.20 |

## Approval

Approved: **yes**

Future local release-gate runs should compare quick or full results against
`docs/native_sql_benchmark_baseline.json`. Re-baselining requires a successful
full benchmark run with environment metadata, p50/p95/p99 latency where
applicable, throughput, and peak RSS.

## Limitations

- This is a local machine baseline only.
- `python_gateway.startup_shutdown` is capped at 25 iterations by the benchmark
  script.
- The command records `--build-mode release`; it does not independently prove
  the imported Python extension was rebuilt as a fresh release wheel in that
  same command.
- This baseline is suitable for local release gating, not for public
  cross-machine performance claims.
