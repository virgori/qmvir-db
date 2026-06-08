# QMvir Integrated Stabilization Report - 2026-06-08

## Summary

- Overall verdict: `PARTIAL_STABILIZATION_TESTS_PASS`
- Files changed by this pass:
  - `qm_engine/src/gateway/native_sql.rs`
  - `qm_engine/tests/native_sql_query_update_delete_audit.rs`
  - `README.md`
  - `USAGE_GUIDE_EN.md`
  - `USAGE_GUIDE_VI.md`
  - `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md`
  - `docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md`
  - `docs/INTEGRATED_STABILIZATION_REPORT_2026_06_08.md`
- Tests added: malformed Native SQL regression coverage in `native_sql_query_update_delete_audit`.
- Tests fixed: no existing test was removed or weakened.
- Docs claims cleaned: PostgreSQL replacement, production HA, group-commit durability, sharding/replication wording.
- Durability semantics: `UNCHANGED`
- Distributed/HA claim status: No production HA claim.
- Benchmark status: quick comparison script ran with PostgreSQL unavailable; release benchmark was blocked by local gateway start permission.

## Snapshot

| Item | Path |
|---|---|
| status before | `/private/tmp/qmvir_stabilization_2026_06_08/status_before.txt` |
| staged before diff | `/private/tmp/qmvir_stabilization_2026_06_08/staged_before.diff` |
| unstaged before diff | `/private/tmp/qmvir_stabilization_2026_06_08/unstaged_before.diff` |
| staged before name-status | `/private/tmp/qmvir_stabilization_2026_06_08/staged_before_name_status.txt` |
| unstaged before name-status | `/private/tmp/qmvir_stabilization_2026_06_08/unstaged_before_name_status.txt` |
| status after unstage | `/private/tmp/qmvir_stabilization_2026_06_08/status_after_unstage.txt` |
| benchmark output dir | `/private/tmp/qmvir_stabilization_2026_06_08/benchmarks` |

Snapshot line counts:

| Item | Lines |
|---|---:|
| status before | 495 |
| staged before diff | 127170 |
| unstaged before diff | 48822 |
| status after unstage | 95 |

## Baseline Results

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `python3 -m pytest tests/test_native_sql_python_bridge_identity_uuid_json.py tests/test_full_engine.py tests/test_vector_comprehensive.py tests/test_bm25.py tests/test_core_internals.py -q -rxX` | pass, 188 passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture` | pass, 127 passed, 15 ignored |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features mvcc -- --nocapture` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features index -- --nocapture` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hnsw -- --nocapture` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features bm25 -- --nocapture` | pass |

## Native SQL Safety

| Issue | Fix | Test |
|---|---|---|
| `GREATEST` / `LEAST` used internal `best.unwrap()` even though the invariant was guarded. | Replaced with explicit state-preserving `Some(cur)` flow. | `cargo test ... native_sql`; `cargo test --test native_sql_query_update_delete_audit` |
| `DELETE FROM t WHERE` returned success with an empty predicate. | Added `DELETE: missing WHERE predicate` typed error before scanning or mutating rows. | `malformed_native_sql_inputs_return_results_without_panicking` |
| `DROP TABLE IF EXISTS` without a table name returned a no-op success. | Added `DROP TABLE: missing table name` typed error while preserving `DROP TABLE IF EXISTS missing_table` behavior. | `malformed_native_sql_inputs_return_results_without_panicking` |
| Malformed ALTER/UUID/JSON/function inputs needed regression coverage. | Added catch-unwind regression checks around malformed DDL/DML and expression cases. | `malformed_native_sql_inputs_return_results_without_panicking` |

## Native SQL Index/Rollback Consistency

| Issue | Fix | Test |
|---|---|---|
| No new index/rollback bug was found in this pass. | No engine index rewrite. Existing consistency surface retained. | `native_sql`, `index`, `native_sql_query_update_delete_audit`, crash/recovery tests |

## WAL/Checkpoint/Crash Safety

| Issue | Fix | Test |
|---|---|---|
| No WAL/checkpoint durability semantic bug was changed in this pass. | No WAL, checkpoint, sync, truncate, or commit-ack behavior changed. | `wal`, `checkpoint`, `release_crash`, `tests/test_wal_concurrency.py`, `tests/test_full_engine.py` |

## HNSW/BM25/Hybrid

| Issue | Fix | Test |
|---|---|---|
| No HNSW/BM25/hybrid correctness bug was found in this pass. | No search/index logic changed. | `hnsw`, `bm25`, `hybrid`, `tests/test_vector_comprehensive.py`, `tests/test_bm25.py` |

## Python/Rust Bridge

| Issue | Fix | Test |
|---|---|---|
| No bridge bug was found in this pass. | No bridge logic changed. | `tests/test_native_sql_python_bridge_identity_uuid_json.py`, `tests/test_core_internals.py`, `tests/test_full_engine.py` |

## Documentation Claim Cleanup

| File | Claim cleaned |
|---|---|
| `README.md` | Removed general PostgreSQL replacement, drop-in, full compatibility, and broad enterprise-ready wording. Added benchmark mode caveat. |
| `USAGE_GUIDE_EN.md` | Clarified group commit is throughput mode, not PostgreSQL synchronous-commit equivalence when acknowledged before fsync. Clarified sharding/replication are not production HA claims. |
| `USAGE_GUIDE_VI.md` | Renamed Docker production heading to Docker deployment wording. |
| `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md` | Replaced broad PostgreSQL replacement/superiority wording with selected embedded/local workload wording and durability caveats. |
| `docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md` | Same claim cleanup in Vietnamese. |

## Benchmark Sanity

| Benchmark | Result | Notes |
|---|---|---|
| `python3 scripts/release_benchmark_native_sql.py` | failed | Gateway startup failed with `Operation not permitted (os error 1)` in this environment. Output: `/private/tmp/qmvir_stabilization_2026_06_08/benchmarks/release_benchmark_native_sql.out`. |
| `python3 scripts/compare_postgres_native_sql.py` | completed | PostgreSQL unavailable due socket permission; output records `postgresql_available=false`. Output: `/private/tmp/qmvir_stabilization_2026_06_08/benchmarks/compare_postgres_native_sql.out`. |

Benchmark interpretation:

- Memory mode: comparison script ran QM memory-mode local results only.
- Persistent-WAL `per_commit_sync`: not run in benchmark sanity in this pass.
- Persistent-WAL `group_commit`: not run in benchmark sanity in this pass.
- PostgreSQL comparison: unavailable in this environment; no PostgreSQL performance claim is approved from this pass.

## Durability Semantics

`UNCHANGED`

No WAL, checkpoint, transaction commit, fsync, sync policy, or benchmark-number semantics were changed. `per_commit_sync` was not weakened. Group commit documentation was made more conservative.

## Distributed/HA Claim Safety

No production HA claim.

Distributed claim remains limited to basic deterministic shard routing, local sharded index behavior, and experimental distributed modules.

## Final Gates

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | pass, 444 passed, 15 ignored; integration suites pass; doctests 0 passed, 2 ignored |
| `python3 -m pytest -q -rxX` | pass, 1139 passed, 12 skipped |
| `git ls-files | grep -E ...` artifact scan | pass, no tracked matches |

## Remaining Risks

- The worktree is intentionally dirty and mostly unstaged after normalization; `git status --short` still reports 95 visible entries.
- Many source files are untracked because the large staged layer was intentionally unstaged. Commit splitting must explicitly add files by slice.
- `native_sql.rs` still contains user-input-adjacent parser assumptions that need deeper typed-expression parsing before every malformed function can return a precise SQL error.
- `native_sql_v2_wip.rs` remains out of release scope.
- Unsafe SIMD and low-level storage/index code still require focused review outside this pass.
- Historical/archive docs still contain older performance and production wording; this pass cleaned the main release-surface files, not every historical report.
- Benchmark sanity did not produce a live PostgreSQL comparison in this environment.
- Distributed/HA modules remain experimental; no network partition, failover, linearizability, serializable distributed transaction, or durable cross-shard 2PC proof was added.

## Suggested Commit Split

| Slice | Files | Tests | Suggested message |
|---|---|---|---|
| 1. Native SQL malformed input safety | `qm_engine/src/gateway/native_sql.rs`, `qm_engine/tests/native_sql_query_update_delete_audit.rs` | `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test native_sql_query_update_delete_audit -- --nocapture`; `cargo test ... native_sql` | `Harden Native SQL malformed DDL/DML handling` |
| 2. Documentation claim cleanup | `README.md`, `USAGE_GUIDE_EN.md`, `USAGE_GUIDE_VI.md`, `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md`, `docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md` | claim scan; full Rust/Python gates | `Clarify release claims for PostgreSQL, durability, and HA` |
| 3. Stabilization report | `docs/INTEGRATED_STABILIZATION_REPORT_2026_06_08.md` | n/a | `Add integrated stabilization report` |
| 4. Existing staged-source surface | remaining untracked source tree from prior staged layer | full gates above plus focused per-slice tests | split according to prior staged source commit plan before adding to index |

## Final Verdict

`PARTIAL_STABILIZATION_TESTS_PASS`

The pass produced coherent source fixes and claim cleanup, preserved durability semantics, passed full Rust and Python gates, and left the repository uncommitted for explicit commit splitting.
