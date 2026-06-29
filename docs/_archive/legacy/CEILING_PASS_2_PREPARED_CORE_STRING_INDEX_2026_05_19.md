# NativeSqlEngine / QMvir Ceiling Pass 2 - Prepared, Core, String Index

Date: 2026-05-19

## Summary

This pass removed two major measurement blockers and partially closed the largest remaining non-durability hot-path bottleneck.

Status: **Ready with performance caveats**.

No PostgreSQL-beating claim is approved. The local PostgreSQL comparison script now honors `POSTGRES_DSN`, but the default local DSN still cannot connect because the PostgreSQL role `postgres` does not exist.

## Methodology

- Build mode: release for benchmark artifacts.
- Feature flags: default for Python-facing performance benchmarks.
- Direct Rust core harness: release, `--no-default-features`, standalone binary.
- Dataset size: 10,000 rows in the full investigation.
- Iterations: 1,000 unless the benchmark explicitly reduces count for expensive loops.
- Warmup policy: warm cache with 10 warmup iterations for latency loops unless overridden.
- Durability mode: in-memory for non-persistent engines; NativeSqlEngine default WAL/checkpoint behavior for persistent WAL benchmarks.
- Layer separation:
  - Direct Rust core: `native_sql_core_bench`, no Python, SQL parser, pgwire, or result conversion.
  - Prepared execution: `NativeSqlEngine.prepare` plus `execute_prepared`.
  - SQL string execution: `NativeSqlEngine.execute`.
  - Python boundary: no-op and empty SQL paths.
  - Gateway steady-state: reused local pgwire connection.
  - WAL/checkpoint: persistent-engine durability path, not compared to in-memory DML.

Environment from `docs/native_sql_perf_investigation_last.json`:

| item | value |
| --- | --- |
| OS | macOS-26.5-arm64-arm-64bit-Mach-O |
| CPU count | 8 |
| RAM | 16.00 GiB |
| Python | 3.13.3 |
| Rust | rustc 1.94.0 (4a4ef493e 2026-03-02) |
| Build mode | release |
| Feature flags | default |
| Peak RSS | 46.71875 MB |

## Code Paths Changed

| file | change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | Added a narrow true prepared-plan API and executor for supported in-memory autocommit simple DML, indexed equality, and compiled count predicates. Added regression coverage for prepared fast paths. |
| `qm_engine/src/bin/native_sql_core_bench.rs` | Added a Rust-native direct core benchmark harness that bypasses Python, SQL parsing/planning, pgwire, and Python result conversion. |
| `qm_engine/Cargo.toml` | Registered the `native_sql_core_bench` binary. |
| `scripts/perf_investigate_native_sql.py` | Integrated direct core benchmarks, real prepared benchmarks, and string-index breakdown benchmarks. |
| `scripts/compare_postgres_native_sql.py` | Added `POSTGRES_DSN` support before `QM_POSTGRES_DSN` and preserved explicit skip output when PostgreSQL is unavailable. |
| `docs/native_sql_benchmark_last.json` | Updated release benchmark output. |
| `docs/native_sql_benchmark_baseline.json` | Updated local release benchmark baseline from the latest passing release run. |
| `docs/native_sql_perf_investigation_last.json` | Updated full performance investigation output. |
| `docs/postgres_comparison_latest.json` | Updated PostgreSQL comparison attempt and skip reason. |

## Prepared-Plan Status

Prepared execution is now real for a narrow, safety-scoped subset:

- `INSERT INTO table (...) VALUES ($1, ...)`
- `SELECT ... FROM table WHERE primary_key = $1`
- `SELECT ... FROM table WHERE indexed_column = $1`
- `UPDATE table SET col = $1 WHERE primary_key = $2`
- `DELETE FROM table WHERE primary_key = $1`
- compiled `COUNT(*)` predicate plans for supported simple OR/equality forms

The prepared executor resolves SQL once, stores a plan id, and repeated execution avoids reparsing SQL through the public `execute` path. It preserves constraint checks, FK checks/actions, vector dimension validation, index maintenance, and cache invalidation for supported DML.

Limitation: prepared fast execution intentionally rejects persistent engines and active-transaction engines in this pass. That keeps WAL and MVCC semantics from being bypassed. Prepared vector exact search is not implemented.

Prepared results:

| workload | p50 ms | p95 ms | p99 ms | ops/s | interpretation |
| --- | ---: | ---: | ---: | ---: | --- |
| prepared.insert_one | 0.003584 | 0.004875 | 0.006208 | 245951.999 | not faster than SQL insert; row construction/index/stat/cache work dominates |
| prepared.select_by_pk | 0.001000 | 0.001042 | 0.001125 | 942729.200 | materially faster than SQL select |
| prepared.update_by_pk | 0.001458 | 0.001542 | 0.001625 | 653149.947 | materially faster than SQL update |
| prepared.delete_by_pk | 0.004583 | 0.004791 | 0.004958 | 214953.543 | slightly faster than SQL delete, but benchmark includes insert+delete setup |
| prepared.predicate_or | 0.002750 | 0.002875 | 0.002958 | 356548.568 | faster than SQL OR predicate |
| prepared.indexed_string_equality | 0.017417 | 0.018583 | 0.018833 | 56850.079 | faster than SQL string equality, still slower than numeric paths |
| prepared.count_compiled_predicate | 0.028375 | 0.030291 | 0.031916 | 34988.942 | compiled plan exists, but COUNT/materialization path still dominates |
| prepared.vector_exact_search | skipped | skipped | skipped | skipped | prepared vector exact-search plan is not implemented yet |

Comparable SQL string results:

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| sql.insert_one | 0.003250 | 0.003500 | 0.005125 | 292640.073 |
| sql.select_by_pk | 0.002791 | 0.002833 | 0.002834 | 353705.846 |
| sql.update_by_pk | 0.002458 | 0.002500 | 0.002542 | 399640.323 |
| sql.delete_by_pk | 0.005042 | 0.005208 | 0.005333 | 195372.943 |
| sql.predicate_or | 0.005000 | 0.005333 | 0.006542 | 193653.022 |
| sql.indexed_string_equality | 0.043459 | 0.091125 | 0.163083 | 20010.706 |

## Direct Rust Core Harness

The direct core harness exists and is now part of `scripts/perf_investigate_native_sql.py`. It runs:

```bash
cargo run --quiet --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin native_sql_core_bench -- --iterations 1000 --output <tmp>
```

Results:

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| core.noop | 0.000042 | 0.000042 | 0.000042 | 10471204.188 |
| core.table_lookup_by_id | 0.000041 | 0.000042 | 0.000042 | 16724925.156 |
| core.row_insert_direct_no_index | 0.000167 | 0.000209 | 0.001167 | 3892944.039 |
| core.row_select_direct_by_id | 0.000083 | 0.000167 | 0.000208 | 8295863.682 |
| core.row_update_direct_by_id | 0.000417 | 0.000875 | 0.002709 | 1628332.994 |
| core.row_delete_direct_by_id | 0.000292 | 0.000375 | 0.000459 | 2560000.000 |
| core.index_lookup_numeric_direct | 0.000375 | 0.000417 | 0.000417 | 2264149.235 |
| core.index_lookup_string_direct | 0.000667 | 0.000709 | 0.000750 | 1369082.824 |
| core.mvcc_visibility_check_direct | 0.000041 | 0.000042 | 0.000042 | 14151076.897 |
| core.vector_distance_l2_direct | 0.000084 | 0.000167 | 0.000167 | 7319304.666 |
| core.vector_distance_cosine_direct | 0.000167 | 0.000167 | 0.000167 | 4589787.722 |
| core.vector_distance_ip_direct | 0.000083 | 0.000125 | 0.000125 | 7478984.055 |

Interpretation: the raw storage/index/MVCC visibility ceiling is far below SQL/Python/gateway latencies. Sub-microsecond figures are local timer-sensitive evidence, not universal claims.

## String Index Equality

String index equality was decomposed into key construction, unique lookup, duplicate-heavy lookup, and result materialization.

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| string_index.key_build_owned | 0.000042 | 0.000084 | 0.000084 | 9592326.139 |
| core.index_lookup_numeric_direct | 0.000375 | 0.000417 | 0.000417 | 2264149.235 |
| core.index_lookup_string_direct | 0.000667 | 0.000709 | 0.000750 | 1369082.824 |
| string_index.lookup_unique | 0.003875 | 0.004292 | 0.005625 | 245793.309 |
| string_index.lookup_duplicate_heavy_count | 0.006250 | 0.014458 | 0.035417 | 127377.712 |
| string_index.result_materialization_duplicate_heavy | 0.042417 | 0.053042 | 0.072542 | 22525.757 |

Root cause:

- Owned string key construction is not the dominant cost.
- Unique direct string lookup is about 1.8x slower than direct numeric lookup in this run.
- SQL/Python unique string lookup is much slower than direct lookup, so executor/result conversion still matters.
- Duplicate-heavy string equality is dominated by duplicate span walk and row/result materialization, not only key comparison.

Still open:

- Borrowed B+Tree lookup API.
- Interned/symbol string key option.
- Specialized string index key that avoids generic `Value::Text(String)` comparison in all hot paths.
- Better separation between lookup-only and materialization-heavy benchmark claims.

## MVCC Write Transaction Metadata

Evidence:

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| core.mvcc_visibility_check_direct | 0.000041 | 0.000042 | 0.000042 | 14151076.897 |
| mvcc.write_transaction | 0.126667 | 0.153541 | 0.172375 | 7812.953 |

Interpretation: visibility checks themselves are not the bottleneck. The measurable write-transaction cost is in transaction metadata, rollback/snapshot bookkeeping, lock/session paths, SQL execution around the write, and any WAL staging used by that workload. No MVCC semantics were weakened.

## Gateway Steady-State

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| gateway.reused_connection_select_by_pk | 0.025792 | 0.049250 | 0.107417 | 32389.408 |

Interpretation: steady-state pgwire/socket/protocol/task/result encoding overhead remains much higher than prepared direct execution. It is now measured separately from lifecycle startup/shutdown and should not be used as a raw engine ceiling metric.

## WAL / Checkpoint

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| wal.commit_with_checkpoint_pressure | 9.432042 | 12.134416 | 13.639208 | 101.035 |

Interpretation: checkpoint pressure remains the largest measured bottleneck. It is durability/checkpoint work and must stay separate from in-memory DML claims. Next work should decompose dirty discovery, serialization, fsync, index checkpointing, table snapshot cloning, and recovery replay.

## Release Benchmark

Command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

Result: passed.

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.003292 | 0.003792 | 0.005208 | 286116.238 |
| native_sql.simple_select | 0.002958 | 0.003042 | 0.003208 | 328933.844 |
| native_sql.simple_update | 0.002625 | 0.003125 | 0.018458 | 312239.768 |
| native_sql.simple_delete | 0.005458 | 0.005833 | 0.016708 | 167082.089 |
| native_sql.predicate_path | 0.039083 | 0.047500 | 0.065166 | 24829.530 |
| native_sql.mvcc_read_write | 0.033542 | 0.054458 | 0.062417 | 29728.577 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 184741.473 |
| native_sql.vector_cache_hot_path | 0.003959 | 0.004292 | 0.005458 | 243170.909 |
| python_gateway.startup_shutdown | 0.296167 | 0.441792 | 0.473167 | 3303.437 |

## Profiler Evidence

The investigation embeds cProfile evidence for the Python-side performance harness:

- `run_predicates`: 0.213 sec cumulative.
- `execute` wrapper: 0.211 sec cumulative across 2,483 calls.
- Rust `NativeSqlEngine.execute` boundary: 0.211 sec cumulative.

Interpretation: for predicate workloads, the Python loop is not the dominant measured cost. Remaining predicate cost is inside Rust execution or the SQL execution boundary. A Rust profiler is still needed for exact internal frame attribution.

## PostgreSQL Comparison Status

Command:

```bash
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json
```

Result: script passed, comparison unavailable.

Environment:

- psql: `psql (PostgreSQL) 17.9 (Homebrew)`
- default DSN: `dbname=postgres user=postgres host=/tmp`
- skip reason: PostgreSQL connection failed because role `postgres` does not exist.

To run a valid comparison:

```bash
POSTGRES_DSN="postgresql://user:password@localhost:5432/qm_bench" \
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json
```

Approved claim: QM-only local benchmark numbers are available.

Rejected claim: QMvir exceeds PostgreSQL. The comparison did not run.

## Correctness Risks

| risk | status |
| --- | --- |
| Prepared executor bypassing WAL/MVCC | Mitigated by rejecting prepared fast execution for persistent engines and active transactions. |
| Prepared DML skipping constraints/index/cache logic | Regression-covered; supported DML calls the same constraint/index/cache maintenance as SQL fast paths. |
| Prepared vector exact search | Not implemented; benchmark remains skipped. |
| String index borrowed lookup | Not implemented; no claim made. |
| Sub-microsecond core numbers | Timer-sensitive local evidence only; no universal claim. |
| PostgreSQL comparison | Environment unavailable; no PostgreSQL claim made. |

## Validation

Passed:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features prepared_plan_fast_paths_execute_without_reparsing_sql
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml
python3 -m pytest -q -rxX
bash scripts/check_no_space_number_duplicates.sh
python3 -m py_compile scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/compare_postgres_native_sql.py
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json
```

Observed results:

- No-default Rust test suite: 402 passed, 15 ignored; integration suites also passed, including 7 crash recovery and 7 crash-kill recovery tests.
- Default-feature Rust test suite with explicit Python link flags: 402 passed, 15 ignored; integration suites passed.
- Pytest: 1113 passed, 12 skipped, 6 xfailed.
- Duplicate hygiene check: passed.
- Release benchmark: passed.
- Full performance investigation: passed.
- PostgreSQL comparison script: passed but PostgreSQL unavailable due missing role.

Remaining pytest xfails:

- HubEngine SQL passthrough: 3 xfails, out of NativeSqlEngine performance scope.
- pgwire float `BETWEEN`: 1 xfail, known pgwire-path feature gap.
- VectorQuantizer memory/compression accounting: 2 xfails, out of NativeSqlEngine hot-path scope.

## Approved Claims

- Direct Rust core benchmark coverage now exists and is part of the full investigation.
- A real prepared-plan API and executor now exists for a narrow, safety-scoped subset.
- Prepared select/update/predicate OR and string equality are materially faster than SQL string execution on this machine.
- Raw direct core row and index operations are much faster than SQL/Python/gateway measurements.
- String index equality is no longer a black box: duplicate-heavy cost is primarily span/result cardinality and materialization, while unique string lookup still trails numeric lookup.
- WAL/checkpoint pressure remains a separate durability bottleneck.

## Rejected Claims

- No PostgreSQL-beating claim.
- No claim that prepared execution is complete across persistent engines, active transactions, or vector exact search.
- No claim that string index equality has reached the numeric-index ceiling.
- No claim that gateway steady-state reflects raw storage speed.
- No claim that WAL/checkpoint throughput is PostgreSQL durability-equivalent.

## Next Targets

1. Extend prepared execution into persistent and transactional engines by routing through WAL/MVCC-safe lower-level plans instead of rejecting.
2. Add borrowed B+Tree lookup keys and measure `string_index.lookup_borrowed_key`.
3. Add specialized string key representation or interning for repeated indexed strings.
4. Split duplicate-heavy string index benchmarks into row-id span walk, dedup, row fetch, and projection conversion.
5. Add Rust-level profiler/flamegraph for index and prepared paths.
6. Decompose WAL/checkpoint into append, flush, dirty discovery, serialization, fsync, and recovery replay.
7. Provide a valid `POSTGRES_DSN` and run matched PostgreSQL comparisons before making any PostgreSQL-class performance claim.
