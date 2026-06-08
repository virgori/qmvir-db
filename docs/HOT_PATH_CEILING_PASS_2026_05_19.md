# NativeSqlEngine / QMvir Hot Path Ceiling Pass - 2026-05-19

## Executive Summary

This pass focused on the requested priority order: delete path, insert path, MVCC read/write overhead, then gateway lifecycle and steady-state gateway cost. The largest implemented optimization is in the predicate path: OR equality predicates now use secondary-index union where possible, and simple COUNT predicates use a compiled non-allocating predicate evaluator instead of the generic SQL condition evaluator.

The release benchmark remains green and the deeper performance investigation now separates benchmark layers rather than mixing core, SQL, Python, gateway, and WAL costs into one number.

Final performance status for this pass: **Ready with performance caveats**.

No PostgreSQL superiority claim is approved in this report because the local PostgreSQL comparison did not run: the local server rejected role `postgres`.

## Methodology

- Build mode: release for benchmark artifacts.
- Feature flags: default.
- Cache policy: warm cache unless the workload name says cold/recovery.
- Dataset rows: 10,000 for the full performance investigation.
- Iterations: 1,000 for full latency loops unless reduced by benchmark type.
- Benchmark layers are reported separately:
  - Direct Rust core: explicitly marked skipped because no direct Rust core benchmark harness is exposed through Python yet.
  - Prepared plan: explicitly marked skipped because a true prepared-plan executor is not implemented yet.
  - SQL string execution: measured through `NativeSqlEngine.execute`.
  - Python boundary: measured separately with no-op and empty SQL paths.
  - Gateway steady-state: measured with a reused local pgwire connection.
  - Gateway lifecycle: measured separately as startup/shutdown.
  - WAL/checkpoint: measured separately from in-memory DML.

## Environment

From `docs/native_sql_perf_investigation_last.json`:

- OS: macOS-26.5-arm64-arm-64bit-Mach-O
- CPU count: 8
- Memory: 16.00 GiB
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build mode: release
- Feature flags: default
- Peak RSS during investigation: 51.796875 MB

## Code Changes

- `qm_engine/src/gateway/native_sql.rs`
  - Added parsed fast predicate terms for COUNT workloads.
  - Added OR equality predicate parsing for indexable branches.
  - Added secondary-index union for `col = literal OR col = literal` predicates.
  - Added duplicate row-id elimination for OR index-union results.
  - Added a regression test covering numeric OR equality, duplicate OR branches, string OR equality, and compiled COUNT predicate evaluation.

- `scripts/perf_investigate_native_sql.py`
  - Added layer-decomposition benchmark categories.
  - Added explicit skipped records for unavailable direct-core and true prepared-plan benchmarks.
  - Added Python boundary measurements.
  - Added SQL string measurements.
  - Added gateway steady-state reused-connection measurements.

- Benchmark artifacts updated:
  - `docs/native_sql_perf_investigation_last.json`
  - `docs/native_sql_benchmark_last.json`
  - `docs/native_sql_benchmark_baseline.json`
  - `docs/postgres_comparison_latest.json`

## Correctness Coverage Added

Rust regression test:

- `gateway::native_sql::tests::indexed_or_equality_uses_index_union_without_duplicates`

Coverage:

- Indexed numeric OR equality returns the correct count.
- Duplicate OR branches do not duplicate row IDs.
- Indexed string OR equality returns the correct count.
- Compiled COUNT predicate handles `IN` plus numeric comparison.

## Benchmark Summary

Full release benchmark command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

Key results:

| workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.003542 | 0.005542 | 0.019500 | 233027.77 |
| native_sql.simple_select | 0.003083 | 0.003209 | 0.004083 | 316484.94 |
| native_sql.simple_update | 0.002500 | 0.002625 | 0.003417 | 377786.17 |
| native_sql.simple_delete | 0.005125 | 0.005292 | 0.005500 | 191644.31 |
| native_sql.predicate_path | 0.037125 | 0.052583 | 0.122750 | 24088.72 |
| native_sql.mvcc_read_write | 0.033084 | 0.062084 | 0.084334 | 28861.76 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 197393.29 |
| native_sql.vector_cache_hot_path | 0.003958 | 0.004042 | 0.004084 | 247908.28 |
| python_gateway.startup_shutdown | 0.286625 | 0.577291 | 0.703000 | 3134.22 |

Full investigation command:

```bash
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
```

Key deeper results:

| workload | category | p50 ms | p95 ms | p99 ms | ops/s |
| --- | --- | ---: | ---: | ---: | ---: |
| python.noop_boundary | python_boundary | 0.000042 | 0.000084 | 0.000125 | 6650041.76 |
| python.execute_sql_noop | python_boundary | 0.000625 | 0.000750 | 0.000875 | 1415596.47 |
| sql.predicate_or | sql_string | 0.005042 | 0.005333 | 0.005708 | 194467.40 |
| sql.indexed_string_equality | sql_string | 0.042084 | 0.053959 | 0.069000 | 22524.19 |
| single.insert_one | single_row | 0.003417 | 0.003667 | 0.004833 | 282060.95 |
| batch.insert_10000 | batch | 38.180667 | 42.935458 | 42.935458 | 25.49 batches/s |
| predicate.indexed_string_equality | predicate | 0.213833 | 0.347708 | 0.654375 | 4192.07 |
| predicate.or_scan | predicate | 0.011334 | 0.013000 | 0.029583 | 84292.75 |
| mvcc.write_transaction | mvcc | 0.134458 | 0.174333 | 0.217500 | 7283.27 |
| wal.commit_with_checkpoint_pressure | wal_checkpoint | 9.323250 | 11.002750 | 11.438459 | 106.07 |
| gateway.startup_shutdown_callback | gateway_lifecycle | 0.298666 | 0.395292 | 0.434500 | 3368.87 |
| gateway.reused_connection_select_by_pk | gateway_steady_state | 0.026000 | 0.056833 | 0.105083 | 32648.09 |

Important baseline note: the previous much lower predicate-path baseline was not valid for the corrected predicate semantics because complex COUNT predicates could bypass real evaluation. The updated baseline records the corrected behavior.

## Profiler Evidence

The investigation script generated a cProfile predicate profile at `docs/native_sql_perf_profile_2026_05_18.prof` and embedded the top cumulative rows in `docs/native_sql_perf_investigation_last.json`.

A redundant external `python3 -m cProfile ... scripts/perf_investigate_native_sql.py` wrapper was attempted on 2026-05-19 and failed with `ValueError: Another profiling tool is already active`, because the benchmark script already owns the cProfile session internally. The failed external profiler artifact was removed and is not used as evidence.

Top profile evidence:

- `run_predicates`: 0.205 sec cumulative
- `execute` wrapper: 0.204 sec cumulative across 2,483 calls
- `{method 'execute' of 'builtins.NativeSqlEngine' objects}`: 0.203 sec cumulative

Interpretation: for the measured predicate profile, Python harness overhead is negligible compared with time inside the Rust `NativeSqlEngine.execute` boundary. Remaining predicate cost should be treated as engine/parser/executor/index behavior unless a lower-level Rust profiler proves otherwise.

## PostgreSQL Comparison Status

Command run:

```bash
python3 scripts/compare_postgres_native_sql.py --iterations 1000 --output docs/postgres_comparison_latest.json
```

Result: PostgreSQL comparison did not run. The local server rejected the configured role:

```text
role "postgres" does not exist
```

Approved claim: local QM-only benchmark numbers are available.

Rejected claim: QM Engine exceeds PostgreSQL. There is no valid local PostgreSQL comparison in this pass.

## Bottleneck Table

| bottleneck | evidence | likely layer | status |
| --- | --- | --- | --- |
| OR predicate path previously slow | `predicate.or_scan` now p50 0.011334 ms; `sql.predicate_or` p50 0.005042 ms | predicate/index | Partially fixed with OR equality index-union and compiled COUNT predicate |
| Indexed string equality slower than simple numeric paths | `sql.indexed_string_equality` p50 0.042084 ms; duplicate-heavy `predicate.indexed_string_equality` p50 0.213833 ms | index/result materialization | Still open |
| True prepared-plan executor absent | prepared benchmarks intentionally skipped | planner/executor | Still open and blocks prepared-path performance claims |
| Direct Rust core benchmark harness absent | direct core benchmarks intentionally skipped | benchmark infrastructure | Still open and blocks raw-core ceiling claims |
| WAL/checkpoint pressure expensive | `wal.commit_with_checkpoint_pressure` p50 9.323250 ms | durability/checkpoint | Still open; not comparable to in-memory DML |
| MVCC write transaction still measurable | `mvcc.write_transaction` p50 0.134458 ms | transaction manager / metadata | Open optimization target, correctness tests pass |
| Gateway steady-state overhead measurable | reused select-by-pk p50 0.026000 ms | pgwire/gateway | Open optimization target |
| Gateway lifecycle overhead separate from query path | startup/shutdown p50 0.298666 ms | lifecycle | Open but no longer mixed with steady-state query performance |

## Approved Claims

- Simple Native SQL insert/select/update/delete are low-microsecond in release build on this local machine.
- Vector cache hot path remains low-microsecond.
- OR equality predicate handling was materially improved for indexed equality forms.
- Python boundary overhead is not the dominant cost in the profiled predicate workload.
- Gateway lifecycle and steady-state query costs are now measured separately.
- WAL/checkpoint pressure is materially more expensive than in-memory DML and must be reported separately.

## Rejected Claims

- No PostgreSQL-beating claim is approved because PostgreSQL comparison did not execute.
- No direct Rust core performance claim is approved because a direct-core harness is not implemented.
- No prepared-plan performance claim is approved because a true prepared-plan executor is not implemented.
- No durability-equivalent PostgreSQL claim is approved because the WAL/checkpoint path was not compared under matched durability settings.

## Validation Commands

Passed:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features indexed_or_equality_uses_index_union_without_duplicates
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

Default-feature Rust test note: plain `cargo test --manifest-path qm_engine/Cargo.toml` hit a macOS/PyO3 linker configuration issue in this environment. The same test suite passed with explicit `PYO3_PYTHON` and Python framework linker flags.

Pytest result:

```text
1113 passed, 12 skipped, 6 xfailed in 27.83s
```

Default-feature Rust result:

```text
401 passed; 0 failed; 15 ignored
35 passed in bench_new_components
23 passed in mvcc_integration
7 passed in release_crash_kill_recovery
7 passed in release_crash_recovery
doc-tests: 0 passed; 2 ignored
```

## Next Optimization Targets

1. Implement a real prepared-plan executor using table IDs, column IDs, index IDs, parameter slots, compiled predicates, and reusable result buffers.
2. Add a Rust-native direct-core benchmark harness so raw storage/index/MVCC claims are not inferred through Python.
3. Optimize indexed string equality with borrowed lookup keys or a specialized string key representation.
4. Break down duplicate-heavy string result materialization cost separately from index lookup cost.
5. Reduce MVCC write transaction metadata overhead without weakening conflict detection or rollback cleanup.
6. Split checkpoint pressure into dirty-table/page tracking, WAL flush, serialization, and recovery replay components.
7. Configure a valid local PostgreSQL role/DSN and rerun the comparison before making PostgreSQL-class or PostgreSQL-beating claims.

## Final Decision

Status: **Ready with performance caveats**.

Correctness gates passed, crash/recovery gates passed, Python xfails are expected and documented by pytest, benchmark gates passed, and the hot OR predicate path was improved with correctness coverage.

The pass is not promoted to an unrestricted performance claim because PostgreSQL comparison is unavailable, true prepared-plan execution is not implemented, direct Rust core benchmarks are not available, and WAL/checkpoint plus indexed string equality remain measurable bottlenecks in supported workloads.
