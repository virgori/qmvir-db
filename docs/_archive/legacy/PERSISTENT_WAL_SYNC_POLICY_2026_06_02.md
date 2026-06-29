# Persistent WAL Sync Policy Report - 2026-06-02

## Summary

This pass added explicit NativeSqlEngine persistent WAL benchmark sync policies and validated them against PostgreSQL 17.9 on the local machine.

Result: QMvir is faster than PostgreSQL on supported in-memory/local scalar workloads and on persistent read/index/count workloads. QMvir is still much slower than PostgreSQL on synchronous persistent mutating workloads when `sync_wal()` must complete before the statement or COMMIT returns.

No PostgreSQL-wide superiority claim is approved. The approved claim is narrower: QMvir wins the measured supported embedded/in-memory workloads and several persistent read-only/index workloads. PostgreSQL wins the measured durable-at-return insert/update/delete/commit workloads.

## Files Changed

| file | change |
| --- | --- |
| `scripts/compare_postgres_native_sql.py` | Added `--qm-sync-policy`, group commit knobs, sync policy metadata, explicit transaction batch workloads, and strict durability claim metadata. |
| `qm_engine/tests/release_crash_recovery.rs` | Added many-row transaction commit/update/delete/rollback restart tests. |
| `qm_engine/tests/release_crash_kill_recovery.rs` | Added process-abort tests before and after many-row transaction COMMIT marker. |
| `qm_engine/src/gateway/native_sql.rs` | Exposed additional checkpoint profile counters/aliases for benchmark interpretation. Segment counters are aliases only, not real segment checkpointing. |
| `docs/native_sql_benchmark_last.json` | Updated release benchmark result. |
| `docs/native_sql_perf_investigation_last.json` | Updated full performance investigation result. |
| `docs/postgres_comparison_memory_latest.json` | Updated strict memory-mode PostgreSQL comparison. |
| `docs/postgres_comparison_persistent_wal_per_mutation_latest.json` | Added strict persistent WAL per-mutation sync comparison. |
| `docs/postgres_comparison_persistent_wal_per_commit_latest.json` | Added strict persistent WAL per-commit sync comparison. |
| `docs/postgres_comparison_persistent_wal_group_commit_latest.json` | Added group-commit comparison; not durable-at-return. |
| `docs/postgres_comparison_latest.json` | Updated to the strict per-commit persistent WAL comparison as the default "latest" artifact. |
| `docs/profiles/native_sql_core_bench_profile_summary.txt` | Updated profiler availability summary. |
| `docs/profiles/native_sql_core_bench_timing_summary.json` | Updated fallback timing profile. |

## Sync Policies

| policy | sync before COMMIT/statement return | meaning |
| --- | ---: | --- |
| `per_mutation_sync` | yes | Strongest benchmark mode. Every benchmarked mutating statement or COMMIT waits for `sync_wal()`. |
| `per_commit_sync` | yes | Autocommit statements sync before return; explicit transactions sync once after COMMIT. |
| `group_commit` | no | Batches `sync_wal()` every N operations. Useful throughput profile, not a synchronous durability claim. |
| `wal_append_only` / `append-only-profile` | no | WAL append profile only; not a durability-at-return claim. |

Important limitation: explicit transaction WAL is staged until COMMIT, so `per_mutation_sync` cannot physically fsync each individual mutation inside an active transaction. The strongest safe point for explicit transactions remains COMMIT return.

## PostgreSQL Comparison

Environment:

- PostgreSQL: 17.9 (Homebrew)
- PostgreSQL settings recorded by script: `fsync=on`, `synchronous_commit=on`
- OS: macOS arm64
- Python: 3.13.3
- Rust: `rustc 1.94.0 (4a4ef493e 2026-03-02)`
- Iterations: 1000 unless benchmark script scales a workload down

### Memory Mode

This is not durability-equivalent. It is a fair local non-durable comparison for supported scalar/index workloads.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 0.004292 | 0.110708 | QM | 25.780x |
| select_by_pk | 0.002958 | 0.039375 | QM | 13.228x |
| update_by_pk | 0.002625 | 0.110125 | QM | 40.292x |
| delete_by_pk | 0.007250 | 0.212208 | QM | 28.066x |
| indexed_string_equality_duplicate_heavy | 0.016083 | 0.052042 | QM | 3.261x |
| count_indexed_equality | 0.003208 | 0.043833 | QM | 13.512x |
| transaction_commit | 0.003833 | 0.157667 | QM | 39.205x |
| transaction_batch_insert_10_commit | 0.102125 | 0.499458 | QM | 5.113x |

Approved claim: QMvir beats PostgreSQL on these measured supported non-durable/local workloads.

### Persistent WAL Per-Commit Sync

This is the durable-at-return comparison. Mutating workloads are the main bottleneck.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 2.990042 | 0.097458 | PostgreSQL | 0.037x |
| select_by_pk | 0.004083 | 0.039625 | QM | 7.018x |
| update_by_pk | 2.996583 | 0.093333 | PostgreSQL | 0.031x |
| delete_by_pk | 5.994041 | 0.177958 | PostgreSQL | 0.033x |
| indexed_string_equality_duplicate_heavy | 0.024709 | 0.052458 | QM | 2.032x |
| count_indexed_equality | 0.004541 | 0.043625 | QM | 9.434x |
| transaction_commit | 2.988708 | 0.144167 | PostgreSQL | 0.055x |
| transaction_batch_insert_10_commit | 3.939750 | 0.502041 | PostgreSQL | 0.154x |

Root cause: synchronous `sync_wal()` is too expensive in the current persistent path. This is implementation overhead, not proven architecture limit.

### Persistent WAL Per-Mutation Sync

Results are similar to per-commit for autocommit workloads and slightly worse in read/index paths because of stricter benchmark sequencing.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 2.992125 | 0.107375 | PostgreSQL | 0.039x |
| select_by_pk | 0.006250 | 0.039042 | QM | 4.637x |
| update_by_pk | 2.998958 | 0.103750 | PostgreSQL | 0.035x |
| delete_by_pk | 5.996708 | 0.183792 | PostgreSQL | 0.034x |
| indexed_string_equality_duplicate_heavy | 0.031583 | 0.052542 | QM | 1.716x |
| count_indexed_equality | 0.004833 | 0.043291 | QM | 8.924x |
| transaction_commit | 2.989917 | 0.133500 | PostgreSQL | 0.049x |
| transaction_batch_insert_10_commit | 3.940208 | 0.494250 | PostgreSQL | 0.148x |

### Persistent WAL Group Commit

This mode acknowledges before fsync (`sync_before_commit_return=false`). It is useful for profiling batching potential only.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 0.008125 | 0.099541 | QM | 2.715x |
| select_by_pk | 0.003125 | 0.039708 | QM | 12.806x |
| update_by_pk | 0.006042 | 0.101791 | QM | 3.039x |
| delete_by_pk | 0.014750 | 0.180916 | QM | 2.827x |
| indexed_string_equality_duplicate_heavy | 0.016375 | 0.053625 | QM | 3.284x |
| count_indexed_equality | 0.003333 | 0.043625 | QM | 11.624x |
| transaction_commit | 0.009000 | 0.100625 | QM | 2.658x |
| transaction_batch_insert_10_commit | 0.163333 | 0.492000 | QM | 1.270x |

Rejected claim: this does not prove durable-at-return PostgreSQL superiority because mutations are acknowledged before fsync.

## Performance Investigation Highlights

| workload | p50 ms | p95 ms | interpretation |
| --- | ---: | ---: | --- |
| `materialization.count_no_projection_duplicate_heavy` | 0.007125 | 0.007250 | COUNT avoids row materialization and remains fast. |
| `materialization.row_id_only_duplicate_heavy` | 0.033625 | 0.034875 | Row-id collection is still materially slower than COUNT. |
| `materialization.one_col_projection_duplicate_heavy` | 0.039791 | 0.041375 | Projection/materialization remains a bottleneck. |
| `materialization.full_row_projection_duplicate_heavy` | 0.113875 | 0.117250 | Full row projection remains expensive. |
| `prepared.indexed_string_equality` | 0.016667 | 0.018333 | Prepared path is fast but still materialization-bound on duplicate-heavy rows. |
| `checkpoint.no_dirty_tables` | 0.001875 | 0.002166 | Clean checkpoint is effectively elided. |
| `checkpoint.dirty_small_table` | 7.744333 | 8.684625 | Dirty checkpoint is still table-level and dominated by table rewrite/fsync. |
| `checkpoint.dirty_large_table` | 9.965542 | 11.897250 | Large dirty table still rewrites broad state. |
| `wal.commit_with_checkpoint_pressure` | 9.636167 | 11.382292 | Checkpoint pressure remains a major WAL bottleneck. |
| `gateway.reused_connection_select_by_pk` | 0.023042 | 0.030000 | Gateway steady-state is much slower than direct SQL/core but stable. |
| `vector.cache_hot_path` | 0.031416 | 0.036417 | Vector cache hot path is acceptable but not yet at raw vector distance ceiling. |

## Correctness Validation

Commands run and results:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
# pass

cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
# pass

cargo check --manifest-path qm_engine/Cargo.toml
# pass

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
# pass: lib 412 passed, 15 ignored; bench_new_components 35 passed; mvcc_integration 23 passed; release_crash_kill_recovery 9 passed; release_crash_recovery 10 passed; doctests 2 ignored

RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' \
PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 \
cargo test --manifest-path qm_engine/Cargo.toml
# pass: same Rust/default/PyO3 suite shape as above

python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels
# pass: built qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl

python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
# pass

python3 -m pytest -q -rxX
# pass: 1131 passed, 12 skipped in 33.41s

bash scripts/check_no_space_number_duplicates.sh
# pass

python3 -m py_compile scripts/compare_postgres_native_sql.py scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py
# pass

python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
# pass

python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
# pass

ITERATIONS=1000 OUT_DIR=docs/profiles scripts/profile_native_sql_rust.sh
# pass: no flamegraph/samply/instruments profiler found; timing summary written

POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode memory --output docs/postgres_comparison_memory_latest.json --strict
# pass

POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --qm-sync-policy per-mutation --output docs/postgres_comparison_persistent_wal_per_mutation_latest.json --strict
# pass after escalated rerun; first run completed benchmark but failed artifact write with PermissionError

POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --qm-sync-policy per-commit --output docs/postgres_comparison_persistent_wal_per_commit_latest.json --strict
# pass

POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --qm-sync-policy group-commit --sync-every-n 100 --output docs/postgres_comparison_persistent_wal_group_commit_latest.json --strict
# pass
```

Crash/recovery coverage added:

- Many-row committed transaction survives restart.
- Many-row update/delete transaction survives restart.
- Many-row rollback does not become visible after restart.
- Process abort before many-row COMMIT marker does not recover transaction body.
- Process abort after many-row COMMIT marker recovers transaction body.

## Remaining Bottlenecks

1. Synchronous persistent WAL sync is too expensive.
   - Insert/update/commit p50 is roughly 2.99 ms under `per_commit_sync`, while PostgreSQL is about 0.09-0.14 ms.
   - Delete benchmark is worse because the workload includes insert plus delete sync behavior.
   - Next fix area: persistent WAL writer batching, fdatasync strategy, WAL buffer reuse, commit record layout, and one-fsync-per-commit without redundant filesystem work.

2. Checkpoint is still table-level.
   - Clean checkpoint is solved.
   - Dirty checkpoint still rewrites a table-level file and fsyncs it.
   - `segment_bytes_written` currently aliases `table_bytes_written`; segment/page checkpoint is not implemented.

3. Duplicate-heavy materialization remains expensive.
   - COUNT is fast because it avoids projection.
   - Row-id/one-column/full-row duplicate-heavy paths still pay row-id collection and materialization overhead.
   - Next fix area: late materialization, arena/reusable result batches, compact row-id result, and boundary-separated Python conversion.

4. Gateway steady-state remains slower than direct execution.
   - `gateway.reused_connection_select_by_pk` p50 is 0.023 ms versus direct SQL/select paths in low microseconds.
   - This is not a storage ceiling; it is pgwire/socket/encoding overhead.

5. Query executor expression gap discovered during test writing.
   - `UPDATE tx_many SET v = v + 100` stores/handles the expression incorrectly on this path, so WAL recovery tests use constant updates.
   - This should be tracked as a planner/executor correctness gap outside the WAL sync policy pass.

## Approved Claims

- QMvir beats PostgreSQL on the measured supported in-memory/local scalar and index workloads.
- QMvir beats PostgreSQL on persistent read/index/count workloads even when the engine is opened in persistent WAL mode.
- QMvir does not currently beat PostgreSQL on durable-at-return insert/update/delete/commit workloads.
- Group-commit shows the architecture can recover much of the mutation speed when fsync is amortized, but it is not synchronous durability.
- Clean checkpoint no-op remains effectively elided.
- Checkpoint remains table-level, not segment/page-level.

## Rejected Claims

- "QMvir beats PostgreSQL" as a general database claim.
- "QMvir durable WAL path beats PostgreSQL" for synchronous mutating workloads.
- "Segment checkpoint is implemented."
- "Group commit result is durability-equivalent to PostgreSQL `synchronous_commit=on`."
- "String index has reached numeric index ceiling."

## Next Targets

1. Implement a real WAL sync engine: group commit with durable acknowledgement semantics, fdatasync/fcntl behavior audit on macOS, buffer reuse, and explicit commit record flush accounting.
2. Implement segment/page-level dirty checkpoint with atomic manifest and crash-safe recovery.
3. Finish duplicate-heavy materialization ceiling: row-id batch type, borrowed projection batch, reusable result buffers, and Python conversion isolation.
4. Add executor correctness tests for arithmetic update expressions and fix `UPDATE SET col = col + literal`.
5. Run a real Rust flamegraph/samply profile on a machine with profiler tools installed.
