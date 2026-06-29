# Group Commit Durability Hardening - 2026-06-14

Verdict: `GROUP_COMMIT_HARDENED`

## Scope

This pass hardens the NativeSqlEngine WAL durability error surface. It does not optimize engine logic, change benchmark workload semantics, or rewrite storage subsystems.

Primary files:

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/compare_postgres_group_commit.py`
- `docs/GROUP_COMMIT_DURABILITY_HARDENING_2026_06_14.md`

## Durability Changes

- `wal_append`, `wal_append_batch`, `wal_sync`, `wal_sync_data`, `wal_flush_only`, and `wal_group_commit_sync` now return `Result<(), String>`.
- WAL writer lock poison, append write errors, flush errors, `sync_all` errors, and `sync_data` errors now propagate to the caller.
- In-memory engines without `data_dir` remain no-op success for WAL append/sync/flush. Persistent engines with a missing WAL writer still return an error.
- Strict `per_commit_sync`, `per_commit_sync_data`, relaxed flush, transaction commit, prepared autocommit, SQL autocommit, checkpoint pre-sync, and Python `sync_wal()` paths now observe WAL I/O errors.
- Sync/flush counters are only incremented after successful flush/sync operations.

## Group Commit Failure Semantics

- Group commit now records a completed generation and the generation error.
- If the leader's `sync_all` fails, all waiters in the same generation receive the same error instead of being falsely acknowledged.
- A later generation can recover and sync successfully after an injected failure.
- Benchmark reporting now normalizes `group-commit-sync` and `group_commit_sync`, so `acknowledged_before_fsync=false` is reported correctly.
- The group-commit benchmark runner now isolates each QM workload/concurrency pair in a subprocess with a timeout. This prevents stale per-process native state from making the aggregate smoke command hang while preserving the measured workload semantics inside each child process.

## Fault Injection Coverage

Test-only WAL fault injection was added for:

- next WAL flush failure
- next WAL `sync_all` failure
- next WAL `sync_data` failure

Focused tests added/validated:

- `per_commit_sync_returns_error_on_sync_failure`
- `per_commit_sync_data_returns_error_on_sync_data_failure`
- `relaxed_os_buffered_returns_error_on_flush_failure`
- `group_commit_sync_propagates_sync_failure_to_all_waiters_and_recovers_later`

## Verification

Passed:

- `cargo fmt --manifest-path qm_engine/Cargo.toml`
- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features sync_failure -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features sync_data_failure -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features flush_failure -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features group_commit_sync -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m py_compile scripts/compare_postgres_group_commit.py scripts/compare_postgres_native_sql.py`
- `python3 -m maturin build --release --out /tmp/qmvir_group_commit_hardening/wheels`
- `python3 -m pip install --force-reinstall /tmp/qmvir_group_commit_hardening/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX`
- `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX`
- `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8`
- `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke`
- `python3 scripts/compare_postgres_group_commit.py --iterations 10 --concurrency 1,4,8,16 --qm-sync-policy group-commit-sync --output /tmp/qmvir_group_commit_hardening/group_commit_smoke.json`

Artifacts:

- `/tmp/qmvir_group_commit_hardening/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `/tmp/qmvir_group_commit_hardening/bridge_materialization_audit.json`
- `/tmp/qmvir_group_commit_hardening/vector_search_medium_smoke.json`
- `/tmp/qmvir_group_commit_hardening/group_commit_smoke.json`

Benchmark smoke notes:

- `postgresql_available=false` in this local run.
- QM group-commit smoke completed all six workloads across concurrency `1,4,8,16`.
- The report records `sync_before_commit_return=true` and `acknowledged_before_fsync=false`.

## Remaining Release Risks

- This pass hardens API acknowledgement semantics: callers no longer receive success when the relevant WAL flush/sync call fails.
- It does not prove that bytes appended before a failed sync can never be persisted later by the OS and replayed. That stronger guarantee needs WAL generation commit markers, durable epoch records, or a writer-poison/rollback design.
- Autocommit mutation may have already modified in-memory state before a sync failure is returned. Persistent truth after such an error should be established by restart/recovery semantics.
- Group commit is still opportunistic and in-thread; there is no dedicated WAL writer thread yet.
- Heavy transaction workloads still show contention and variable tail latency.
- Linux filesystem and storage-device validation is still required before broader release claims.

## Commit Split Suggestion

1. `native-sql-wal-error-propagation`: Rust WAL helper return types, sync/flush propagation, checkpoint pre-sync behavior, and Python error mapping.
2. `native-sql-wal-fault-injection-tests`: test-only fault injection and focused durability regression tests.
3. `group-commit-benchmark-hygiene`: subprocess isolation, PostgreSQL connect timeout, progress logging, and policy normalization in `compare_postgres_group_commit.py`.
4. `group-commit-durability-report`: this report and generated `/tmp` evidence references only; do not commit `/tmp` artifacts.

