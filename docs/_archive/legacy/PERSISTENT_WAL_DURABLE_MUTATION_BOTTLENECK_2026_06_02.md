# Persistent WAL Durable Mutation Bottleneck - 2026-06-02

## Summary

This pass isolated and partially fixed the persistent WAL durable mutation bottleneck.

Implemented:

- Explicit NativeSqlEngine WAL sync policy state.
- Engine-native `per_mutation_sync` and `per_commit_sync`.
- `append_only_profile` remains a profiling-only mode.
- Benchmark-controller `group_commit` remains a throughput prototype only.
- WAL sync counters are exposed and written into PostgreSQL comparison JSON.
- Explicit transaction batch workloads now report WAL bytes, sync count, fsync count, and whether COMMIT waits for fsync.
- Crash/recovery tests now cover many-row COMMIT/ROLLBACK and process abort before/after COMMIT marker under per-commit sync.

Result:

- WAL append is not the bottleneck: `persistent.insert_wal_append_only` p50 is `0.006000 ms`.
- Durable WAL sync is the bottleneck: `persistent.insert_sync_wal_only` p50 is `2.993625 ms`.
- QMvir wins memory-mode and persistent read/index/count workloads in the local PostgreSQL comparison.
- QMvir still loses durable-at-return insert/update/delete/commit workloads to PostgreSQL because each acknowledged durable mutation still pays an approximately 3 ms sync.
- Group commit wins many mutation workloads, but it acknowledges before fsync and is not durability-equivalent to PostgreSQL `synchronous_commit=on`.
- Dirty checkpoint remains table-level. Segment/page checkpointing is designed below, but not implemented in this pass.

## Environment

From benchmark artifacts:

| field | value |
| --- | --- |
| OS | `macOS-26.5-arm64-arm-64bit-Mach-O` |
| machine | `arm64` |
| CPU count | `8` |
| RAM | `16.00 GiB` |
| Python | `3.13.3` |
| Rust | `rustc 1.94.0 (4a4ef493e 2026-03-02)` |
| PostgreSQL | `psql (PostgreSQL) 17.9 (Homebrew)` |
| PostgreSQL fsync | `on` |
| PostgreSQL synchronous_commit | `on` |
| QM build mode | `release` for benchmark artifacts |
| iterations | `1000` for comparison runs unless workload scales batch count |

## Code Paths Changed

| file | change |
| --- | --- |
| `qm_engine/src/gateway/native_sql.rs` | Added `WalSyncPolicy`, engine-native sync policy storage, `set_wal_sync_policy`, `wal_sync_count`, autocommit sync routing, COMMIT sync routing, and sync counter increments after durable flush. |
| `scripts/compare_postgres_native_sql.py` | Added engine-native sync policy configuration, group-commit controller mode, transaction batch workloads, WAL byte deltas, sync/fsync counters, and durable claim metadata. |
| `qm_engine/tests/release_crash_recovery.rs` | Added/updated many-row transaction COMMIT/UPDATE/DELETE/ROLLBACK restart coverage under per-commit sync. |
| `qm_engine/tests/release_crash_kill_recovery.rs` | Added process-abort coverage before COMMIT marker and after synced COMMIT marker under per-commit sync. |
| `docs/postgres_comparison_memory_latest.json` | Strict local memory-mode PostgreSQL comparison. |
| `docs/postgres_comparison_persistent_wal_per_mutation_latest.json` | Strict persistent WAL per-mutation comparison. |
| `docs/postgres_comparison_persistent_wal_per_commit_latest.json` | Strict persistent WAL per-commit comparison. |
| `docs/postgres_comparison_persistent_wal_group_commit_latest.json` | Group-commit comparison; not durable-at-return. |
| `docs/postgres_comparison_latest.json` | Copied from the strict per-commit persistent WAL comparison. |

## WAL Sync Policies

| policy | implemented where | sync before return | acknowledged before fsync | claim scope |
| --- | --- | ---: | ---: | --- |
| `append_only_profile` | engine | no | yes | WAL append profiling only; not durable-at-return. |
| `per_mutation_sync` | engine for autocommit and COMMIT sync point | yes | no | Strict durable-at-return baseline for autocommit. Inside explicit transactions, WAL is staged until COMMIT, so the safe sync point is COMMIT. |
| `per_commit_sync` | engine | yes | no | Autocommit statements sync before return; explicit transactions sync once at COMMIT before returning. |
| `group_commit` | benchmark controller | no | yes | Throughput prototype only; not equivalent to PostgreSQL `synchronous_commit=on`. |

Important boundary: current explicit transaction WAL records are staged until COMMIT. Therefore `per_mutation_sync` cannot fsync each individual mutation inside an active explicit transaction without changing the transaction WAL design. For explicit transactions, both `per_mutation_sync` and `per_commit_sync` sync at COMMIT in the current implementation.

## PostgreSQL Comparison Results

All PostgreSQL comparisons used:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench'
```

### Memory Mode

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode memory \
  --output docs/postgres_comparison_memory_latest.json \
  --strict
```

Memory mode is not durability-equivalent.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio |
| --- | ---: | ---: | --- | ---: |
| insert | 0.004542 | 0.092500 | QM | 20.994x |
| select_by_pk | 0.002958 | 0.039000 | QM | 13.022x |
| update_by_pk | 0.002542 | 0.090500 | QM | 36.098x |
| delete_by_pk | 0.007375 | 0.176875 | QM | 23.276x |
| indexed_string_equality_duplicate_heavy | 0.015958 | 0.053250 | QM | 3.305x |
| count_indexed_equality | 0.003250 | 0.043583 | QM | 13.170x |
| transaction_commit | 0.003875 | 0.130250 | QM | 31.106x |
| transaction_insert_1000_commit | 7.983292 | 37.277292 | QM | 4.776x |

Approved claim: QMvir wins the measured supported local non-durable workloads.

### Persistent WAL Per-Mutation Sync

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy per-mutation \
  --output docs/postgres_comparison_persistent_wal_per_mutation_latest.json \
  --strict
```

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio | sync count | waits for fsync |
| --- | ---: | ---: | --- | ---: | ---: | --- |
| insert | 2.993167 | 0.097292 | PostgreSQL | 0.039x | 1010 | yes |
| select_by_pk | 0.008416 | 0.039250 | QM | 4.064x | 0 | yes |
| update_by_pk | 2.999542 | 0.112125 | PostgreSQL | 0.037x | 1010 | yes |
| delete_by_pk | 5.996500 | 0.220416 | PostgreSQL | 0.038x | 1020 | yes |
| indexed_string_equality_duplicate_heavy | 0.031292 | 0.052250 | QM | 1.656x | 0 | yes |
| count_indexed_equality | 0.004708 | 0.043333 | QM | 9.025x | 0 | yes |
| transaction_commit | 2.991333 | 0.148042 | PostgreSQL | 0.055x | 510 | yes |
| transaction_insert_100_commit | 5.415959 | 3.878209 | PostgreSQL | 0.686x | 111 | yes |
| transaction_insert_1000_commit | 14.283625 | 37.175000 | QM | 2.101x | 22 | yes |
| transaction_rollback_100 | 2.885916 | 4.108042 | QM | 1.422x | 0 | yes |

Interpretation:

- Autocommit insert/update/delete lose because each acknowledged mutation waits for durable WAL sync.
- Large explicit batches can win because one COMMIT sync amortizes many row operations.
- `transaction_insert_1000_commit` has low sync count but its WAL byte delta is net file growth, not gross WAL bytes written; checkpoint truncation can make this counter understate actual write volume.

### Persistent WAL Per-Commit Sync

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy per-commit \
  --output docs/postgres_comparison_persistent_wal_per_commit_latest.json \
  --strict
```

This is the main durable-at-return mode for explicit transactions.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio | sync count | waits for fsync |
| --- | ---: | ---: | --- | ---: | ---: | --- |
| insert | 2.991292 | 0.102208 | PostgreSQL | 0.039x | 1010 | yes |
| select_by_pk | 0.007541 | 0.039208 | QM | 4.229x | 0 | yes |
| update_by_pk | 2.998666 | 0.092875 | PostgreSQL | 0.031x | 1010 | yes |
| delete_by_pk | 5.998167 | 0.179417 | PostgreSQL | 0.032x | 1020 | yes |
| indexed_string_equality_duplicate_heavy | 0.031291 | 0.052292 | QM | 1.634x | 0 | yes |
| count_indexed_equality | 0.004625 | 0.043333 | QM | 9.144x | 0 | yes |
| transaction_commit | 2.991125 | 0.159917 | PostgreSQL | 0.058x | 510 | yes |
| transaction_insert_10_commit | 4.066000 | 0.488083 | PostgreSQL | 0.125x | 1011 | yes |
| transaction_insert_100_commit | 5.596166 | 3.883292 | PostgreSQL | 0.703x | 111 | yes |
| transaction_insert_1000_commit | 13.179750 | 36.543750 | QM | 2.254x | 22 | yes |
| transaction_rollback_100 | 2.115292 | 4.107667 | QM | 1.934x | 0 | yes |

Interpretation:

- Per-commit sync fixes the semantics boundary: COMMIT does not return before WAL sync.
- It does not fix the underlying sync cost. A single QM WAL sync still costs roughly 3 ms p50 on this machine.
- Large explicit transactions can beat PostgreSQL by amortizing that sync over many rows.
- Small durable transactions and autocommit writes still lose badly.

### Persistent WAL Group Commit

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy group-commit \
  --sync-every-n 100 \
  --output docs/postgres_comparison_persistent_wal_group_commit_latest.json \
  --strict
```

This mode is not durable-at-return.

| workload | QM p50 ms | PG p50 ms | winner | QM ops ratio | sync count | waits for fsync |
| --- | ---: | ---: | --- | ---: | ---: | --- |
| insert | 0.009459 | 0.102666 | QM | 2.704x | 10 | no |
| select_by_pk | 0.003666 | 0.038833 | QM | 10.388x | 0 | no |
| update_by_pk | 0.007000 | 0.111583 | QM | 3.023x | 10 | no |
| delete_by_pk | 0.019250 | 0.214958 | QM | 2.749x | 10 | no |
| indexed_string_equality_duplicate_heavy | 0.019459 | 0.052583 | QM | 2.723x | 0 | no |
| count_indexed_equality | 0.003625 | 0.043041 | QM | 11.811x | 0 | no |
| transaction_commit | 0.009458 | 0.155875 | QM | 2.585x | 10 | no |
| transaction_insert_10_commit | 0.368292 | 0.484125 | PostgreSQL | 0.815x | 102 | no |
| transaction_insert_100_commit | 4.989041 | 4.377375 | QM | 1.764x | 111 | no |
| transaction_insert_1000_commit | 13.575916 | 38.948583 | QM | 2.207x | 22 | no |

Interpretation:

- Group commit shows that the executor/storage path can beat PostgreSQL when fsync is amortized.
- This is not a durable-at-return win because mutations are acknowledged before fsync.
- The high sync count in some batch workloads is influenced by checkpoint pressure and benchmark setup; it is not a pure WAL-only group-commit measurement.

## WAL and Checkpoint Cost Split

From `docs/native_sql_perf_investigation_last.json`:

| workload | p50 ms | p95 ms | throughput ops/s | interpretation |
| --- | ---: | ---: | ---: | --- |
| `persistent.insert_wal_append_only` | 0.006000 | 0.006416 | 162149.03 | WAL append is fast. |
| `persistent.insert_sync_wal_only` | 2.993625 | 3.224958 | 361.54 | WAL sync is the durable mutation bottleneck. |
| `checkpoint.no_dirty_tables` | 0.001875 | 0.002166 | 502613.12 | Clean checkpoint is effectively elided. |
| `checkpoint.dirty_small_table` | 7.744333 | 8.684625 | 134.96 | Dirty checkpoint still rewrites/syncs a table-level snapshot. |
| `checkpoint.dirty_large_table` | 9.965542 | 11.897250 | 100.40 | Large dirty table remains table-level. |
| `wal.commit_with_checkpoint_pressure` | 9.636167 | 11.382292 | 104.45 | Checkpoint pressure remains a major durable path cost. |
| `wal.bulk_insert_1000_then_checkpoint` | 23.790167 | 23.790167 | 42.03 | Bulk checkpoint still table-level. |

Checkpoint profile for `checkpoint.dirty_small_table`:

| counter | value |
| --- | ---: |
| dirty table count | 1 |
| dirty row count | 1 |
| dirty page or segment count | 1 |
| table bytes written | 104014 |
| segment bytes written | 104014 |
| fsync count | 2 |
| lock held ms | 0.527500 |
| temp file write ms | 1.056667 |
| temp file sync ms | 4.998667 |
| rename ms | 1.085959 |

The `segment_bytes_written` field currently aliases `table_bytes_written`. It is profile scaffolding only, not real segment checkpointing.

## Segment/Page Checkpoint Design

The next checkpoint implementation should move from table-level snapshot files to crash-safe segments.

Proposed layout:

```text
native_sql_tables/
  manifest.current
  manifest.<generation>.tmp
  table_<table_id>/
    schema.json
    segment_<segment_id>.<generation>.bin
    index_catalog.<generation>.bin
```

Required metadata:

```rust
struct DirtyTracker {
    global_generation: u64,
    last_checkpoint_generation: u64,
    dirty_tables: HashSet<TableId>,
    dirty_segments: HashMap<TableId, HashSet<SegmentId>>,
    table_generations: HashMap<TableId, u64>,
    segment_generations: HashMap<(TableId, SegmentId), u64>,
}

struct SegmentManifestEntry {
    table_id: TableId,
    segment_id: SegmentId,
    generation: u64,
    path: String,
    bytes: u64,
    checksum: u64,
}
```

Crash-safe write order:

1. Write dirty segment temp files.
2. `sync_data` each dirty segment file.
3. Atomically rename segment temp files into generation-stamped final files.
4. Write manifest temp file that references old clean segments plus new dirty segments.
5. `sync_data` manifest temp file.
6. Atomically rename `manifest.<generation>.tmp` to `manifest.current`.
7. Fsync the checkpoint directory if the platform path supports it.
8. Only after the manifest is durable, mark segments clean and allow WAL truncation up to the checkpoint LSN.

Correctness constraints:

- A crash during segment write must leave the previous manifest valid.
- A crash after segment write but before manifest swap must ignore unreferenced new segments.
- A crash after manifest swap must load the new manifest and replay WAL after the checkpoint LSN.
- WAL truncation must never happen before the manifest that covers that LSN is durable.
- Index/catalog checkpoint must be generationed with the table data it describes, or recovery must rebuild/validate the index from table segments.
- Existing table-level snapshot compatibility must remain during migration.

Expected benchmark counters:

| counter | meaning |
| --- | --- |
| `dirty_segment_count` | Number of segments serialized in this checkpoint. |
| `segment_bytes_written` | Gross bytes written for dirty segments only. |
| `manifest_bytes_written` | Bytes written for manifest temp file. |
| `fsync_count` | Segment + manifest data sync count. |
| `directory_fsync_count` | Directory fsync count after atomic renames. |
| `marker_rewritten` | Whether compatibility marker was rewritten. |
| `index_catalog_rewritten` | Whether index metadata was rewritten. |

Status: design documented; implementation remains open. Current engine is still table-level checkpoint.

## Correctness Validation

Commands run after implementation:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
# pass

cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
# pass

cargo check --manifest-path qm_engine/Cargo.toml
# pass

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
# pass: 10 passed

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
# pass: 9 passed

cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
# pass: lib 412 passed, 15 ignored; bench_new_components 35 passed; mvcc_integration 23 passed; release crash tests passed; doctests 2 ignored

RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' \
PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 \
cargo test --manifest-path qm_engine/Cargo.toml
# pass: default/PyO3 Rust suite passed

python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels
# pass: built qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl

python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
# pass

python3 -m pytest -q -rxX
# pass: 1131 passed, 12 skipped in 28.55s

bash scripts/check_no_space_number_duplicates.sh
# pass

python3 -m py_compile scripts/compare_postgres_native_sql.py scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py
# pass after final CLI help text edit
```

Crash/recovery evidence:

- `BEGIN; INSERT many; COMMIT; reopen` passes.
- `BEGIN; UPDATE/DELETE many; COMMIT; reopen` passes.
- `BEGIN; INSERT many; ROLLBACK; reopen` passes.
- Process abort before many-row COMMIT marker recovers no transaction body.
- Process abort after synced many-row COMMIT marker recovers committed rows.

## Remaining Bottlenecks

| area | status | release interpretation |
| --- | --- | --- |
| WAL `sync_all()` | Still roughly 3 ms p50 | Main reason durable autocommit insert/update/delete/commit lose to PostgreSQL. |
| Per-commit explicit transactions | Correct, but sync cost remains | Wins only when enough rows amortize one sync. |
| Group commit | Fast, not durable-at-return | Useful future direction, not a PostgreSQL synchronous durability claim. |
| Dirty checkpoint | Still 7-10 ms p50 | Clean checkpoint is solved; dirty checkpoint still table-level rewrite plus fsync. |
| Segment/page checkpoint | Not implemented | Required before claiming checkpoint ceiling is solved. |
| Gross WAL bytes | Not fully measured | Current `wal_bytes_delta` is net WAL file size delta and can be affected by checkpoint truncation. |
| Total fsync count | WAL sync count only in comparison JSON | Checkpoint file fsyncs are in checkpoint profile, not folded into per-workload PostgreSQL comparison counters. |

## Approved Claims

- QMvir wins the measured supported memory-mode workloads against local PostgreSQL.
- QMvir wins persistent read/index/count workloads even when WAL is enabled.
- Per-commit sync is now engine-native and COMMIT waits for WAL sync before returning.
- Committed many-row transactions survive restart and process abort after synced COMMIT marker.
- Rolled-back many-row transactions do not reappear after restart.
- WAL append is not the persistent mutation bottleneck.
- The remaining persistent durable mutation bottleneck is WAL sync plus checkpoint pressure, not parser/planner/core row mutation.

## Rejected Claims

- Do not claim QMvir beats PostgreSQL for durable-at-return autocommit insert/update/delete/commit.
- Do not claim group-commit results are PostgreSQL `synchronous_commit=on` equivalent.
- Do not claim segment/page checkpoint exists.
- Do not claim dirty checkpoint is solved.
- Do not merge memory, per-mutation, per-commit, and group-commit benchmark results into one performance claim.

## Next Work

1. Implement real segment-level checkpoint with atomic generationed manifest and WAL truncation only after manifest durability.
2. Measure gross WAL bytes written, not just net WAL file size delta.
3. Split WAL sync implementation into write, data sync, metadata sync, and optional platform-specific sync modes without disabling fsync.
4. Build a real background/group commit service with explicit durability-window API semantics instead of benchmark-controller batching.
5. Keep optimizing duplicate-heavy materialization separately; it is not the current durable mutation bottleneck.
