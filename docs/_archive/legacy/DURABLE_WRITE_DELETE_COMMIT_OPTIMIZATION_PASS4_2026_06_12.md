# QMvir Durable Write/Delete/Commit Optimization Pass 4 - 2026-06-12

## Verdict

`PASS4_PG_VALIDATED`

This verdict means the Pass 3 verification debt was closed, the Python extension was rebuilt from current Rust source, the profiler was rerun with the rebuilt wheel, and a full PostgreSQL comparison completed with raw artifacts outside the repo.

It does not mean QMvir wins every PostgreSQL workload. The validated comparison is mixed.

## Xcode / Toolchain Status

| Check | Result |
|---|---|
| `xcode-select -p` | PASS, `/Applications/Xcode.app/Contents/Developer` |
| `xcrun --find cc` | PASS, Xcode default toolchain `cc` |
| `xcrun cc --version` | PASS, Apple clang `21.0.0` |
| `cc --version` | PASS, Apple clang `21.0.0` |
| `rustc --version` | PASS, `rustc 1.94.0 (4a4ef493e 2026-03-02)` |
| `cargo --version` | PASS, `cargo 1.94.0 (85eff7c80 2026-01-15)` |
| `python3 --version` | PASS, `Python 3.13.3` |

Pass 3 linked gate debt closed.

## Rust Linked Gates

| Gate | Result |
|---|---|
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture` | PASS, `129 passed; 15 ignored`; filtered integration binaries also linked |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features wal -- --nocapture` | PASS, `15 passed` unit tests plus filtered WAL/recovery integration tests |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features checkpoint -- --nocapture` | PASS, `6 passed` unit tests plus filtered checkpoint/recovery integration tests |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery -- --nocapture` | PASS, `10 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery -- --nocapture` | PASS, `18 passed` |

No crash recovery or kill recovery regression was observed.

## Python Wheel Rebuild

Build/install convention used:

```bash
python3 -m maturin build --release --out /private/tmp/qmvir_pg_perf_pass4_2026_06_12/wheels
python3 -m pip install --force-reinstall /private/tmp/qmvir_pg_perf_pass4_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
```

Result:

- Wheel built: `/private/tmp/qmvir_pg_perf_pass4_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- Installed package: `qmvir 5.4.0`
- Imported module: `/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/site-packages/qm_engine/__init__.py`
- Imported extension: `/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/site-packages/qm_engine/qm_engine.cpython-313-darwin.so`
- `profile_snapshot()` and `reset_profile_snapshot()` are exposed.

## Pass 3 Profiler Rerun With Rebuilt Wheel

Command:

```bash
python3 scripts/native_sql_write_profile.py \
  --iterations 50 \
  --warmup 5 \
  --repeat 3 \
  --trace \
  --output /private/tmp/qmvir_pg_perf_pass4_2026_06_12/write_profile_rebuilt_wheel.json
```

| Workload | Pass 3 stale p50 | Pass 3 stale p95 | Pass 4 rebuilt p50 | Pass 4 rebuilt p95 | Note |
|---|---:|---:|---:|---:|---|
| insert | 2.998 | 3.534 | 3.001 | 4.346 | Autocommit still sync dominated |
| update_by_pk | 3.000 | 3.060 | 3.003 | 4.096 | Autocommit still sync dominated |
| delete_by_pk | 2.997 | 3.048 | 3.001 | 3.810 | DELETE-only correction preserved |
| transaction_commit | 9.004 | 10.993 | 3.019 | 4.161 | Rebuilt release wheel materially faster |
| transaction_insert_10_commit | 12.022 | 15.000 | 3.771 | 5.303 | Rebuilt release wheel materially faster |
| transaction_insert_100_commit | 27.266 | 37.186 | 5.681 | 6.600 | Rebuilt release wheel materially faster |
| transaction_insert_1000_commit | 647.068 | 992.198 | 14.166 | 23.084 | Rebuilt release wheel materially faster |
| transaction_mixed_dml_100_commit | 16.081 | 18.009 | 5.979 | 6.992 | Rebuilt release wheel materially faster |
| transaction_rollback_100 | 9.991 | 10.678 | 1.409 | 2.078 | Rebuilt release wheel materially faster |

Interpretation:

- The Pass 4 rerun confirms the Python timing is no longer using the stale wheel.
- The large transaction improvement should be attributed primarily to rebuilt release-wheel validation, not solely to `wal_sql: Vec::with_capacity(128)`.
- The Pass 3 `delete_by_pk` profiler correction held: sync count stayed at 150 and WAL bytes stayed at 5988, not the old INSERT+DELETE shape.

## Counter Evidence

| Workload | Pass 3 stale sync_all | Pass 4 rebuilt sync_all | Pass 3 stale WAL bytes | Pass 4 rebuilt WAL bytes |
|---|---:|---:|---:|---:|
| insert | 150 | 150 | 14430 | 14430 |
| update_by_pk | 150 | 150 | 11322 | 11322 |
| delete_by_pk | 150 | 150 | 5988 | 5988 |
| transaction_commit | 150 | 150 | 14430 | 14430 |
| transaction_insert_10_commit | 150 | 150 | 143886 | 143886 |
| transaction_insert_100_commit | 30 | 30 | 291960 | 291960 |
| transaction_insert_1000_commit | 9 | 9 | 875904 | 875904 |
| transaction_mixed_dml_100_commit | 30 | 30 | 272277 | 272277 |
| transaction_rollback_100 | 0 | 0 | 0 | 0 |

Strict sync accounting did not change. This supports the durability claim that the flush/sync path was not weakened.

## WRITE Diagnosis

- Autocommit `insert` and `update_by_pk` remain dominated by strict `sync_all()` latency.
- The safe optimization space is still pre-sync overhead: schema lookup, row/value clone cost, index maintenance, WAL serialization, and lock scope.
- Transaction insert workloads are now much faster under the rebuilt release wheel, but `transaction_insert_1000_commit` still has p95 variance and should not be overclaimed from a single calibration run.

## DELETE Diagnosis

- The profiler now measures DELETE by primary key directly.
- `delete_by_pk` no longer includes a hidden INSERT in the timed operation.
- DELETE by PK uses direct row lookup in the engine path rather than a full table scan for simple `WHERE id = ...`.
- With strict durable autocommit, the remaining latency is mostly sync-bound.
- Crash/kill recovery tests covering committed delete, checkpoint delete, and abort recovery passed.

## COMMIT Diagnosis

- Explicit transactions batch WAL statements and then sync once in strict mode.
- Rollback workloads do not WAL/sync committed data in the measured rollback cases.
- Empty/clean transactions do not append WAL statements.
- `wal_sql: Vec::with_capacity(128)` is present in transaction staging and was included in the rebuilt wheel, but the benchmark improvement should be interpreted as rebuilt-wheel validation plus existing transaction batching, not as a standalone PostgreSQL claim.

## Durability Semantics

The strict durability path did not change in Pass 4:

- `acknowledged_before_fsync=false` for the durable comparison.
- `sync_before_commit_return=true`.
- Autocommit mutations sync before returning.
- Explicit transaction mutations append during the transaction and sync once after `COMMIT`.
- The WAL flush plus `sync_all()` path was not disabled, bypassed, or moved after client acknowledgement.

## PostgreSQL Benchmark Status

Full PostgreSQL comparison completed after rerunning with permission for local TCP access.

Command:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --iterations 1000 \
  --qm-mode persistent-wal \
  --qm-sync-policy per-commit \
  --output /private/tmp/qmvir_pg_perf_pass4_2026_06_12/postgres_comparison_persistent_wal_per_commit.json \
  --strict
```

Environment:

- PostgreSQL: `17.9 (Homebrew)`
- PostgreSQL `fsync=on`
- PostgreSQL `synchronous_commit=on`
- QM mode: `persistent-wal`
- QM sync policy: `per_commit_sync`
- QM `sync_before_commit_return=true`

| Workload | PG p50 | PG p95 | QM p50 | QM p95 | Winner |
|---|---:|---:|---:|---:|---|
| insert | 0.098 | 0.171 | 2.991 | 3.169 | PostgreSQL |
| select_by_pk | 0.039 | 0.048 | 0.010 | 0.011 | QM |
| update_by_pk | 0.095 | 0.124 | 2.999 | 3.213 | PostgreSQL |
| delete_by_pk | 0.189 | 0.246 | 5.995 | 6.084 | PostgreSQL |
| indexed_integer_equality | 0.041 | 0.051 | 0.009 | 0.009 | QM |
| indexed_string_equality_duplicate_heavy | 0.052 | 0.063 | 0.027 | 0.032 | QM |
| count_indexed_equality | 0.043 | 0.053 | 0.005 | 0.005 | QM |
| predicate_range | 0.050 | 0.059 | 0.006 | 0.006 | QM |
| transaction_commit | 0.144 | 0.176 | 2.989 | 3.986 | PostgreSQL |
| transaction_rollback | 0.088 | 0.103 | 0.021 | 0.029 | QM |
| transaction_insert_10_commit | 0.487 | 0.733 | 3.992 | 5.641 | PostgreSQL |
| transaction_insert_100_commit | 3.971 | 4.213 | 5.013 | 6.020 | PostgreSQL |
| transaction_insert_1000_commit | 36.690 | 37.127 | 14.759 | 48.147 | QM |
| transaction_mixed_dml_100_commit | 4.201 | 4.410 | 5.022 | 6.049 | PostgreSQL |
| transaction_rollback_100 | 4.217 | 4.341 | 2.177 | 2.365 | QM |
| indexed_string_equality_unique | 0.039 | 0.048 | 0.013 | 0.014 | QM |

Release claim allowed from this pass:

- Valid statement: the full comparison completed and produced mixed results under the recorded environment.
- Invalid statement: QMvir is broadly faster than PostgreSQL for durable writes.

## Final Regression Gates

| Check | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | PASS |
| Rust linked native SQL/WAL/checkpoint/crash/kill gates | PASS |
| `python3 -m pytest tests/test_wal_concurrency.py tests/test_full_engine.py -q -rxX` | PASS, `126 passed` |
| `python3 -m pytest tests/test_python_rust_bridge_true_zero_copy.py tests/test_vector_comprehensive.py -q -rxX` | PASS, `59 passed` |
| `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8` | PASS |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | PASS |
| Full PostgreSQL comparison | PASS |

Full Rust and full Python gates were not rerun in this pass; the targeted release gates above were completed.

## Files Changed

Source/release-surface files currently relevant to this optimization series:

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/native_sql_write_profile.py`
- `scripts/release_benchmark_native_sql.py`
- `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS3_2026_06_12.md`
- `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS4_2026_06_12.md`
- `docs/DURABLE_WRITE_OPTIMIZATION_FIRST_PASS_2026_06_08.md`
- `docs/DURABLE_WRITE_OPTIMIZATION_PASS2_PROFILE_2026_06_08.md`

Pre-existing/local untracked paths remain outside this report scope:

- `_py_legacy/`
- `tests/_probe5.py`

No commit was created.

## Artifacts Outside Repo

- `/private/tmp/qmvir_pg_perf_pass4_2026_06_12/write_profile_rebuilt_wheel.json`
- `/private/tmp/qmvir_pg_perf_pass4_2026_06_12/postgres_comparison_persistent_wal_per_commit.json`
- `/private/tmp/qmvir_pg_perf_pass4_2026_06_12/wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`
- `/private/tmp/qmvir_pg_perf_pass4_2026_06_12/qmvir_bridge_materialization_audit.json`
- `/private/tmp/qmvir_pg_perf_pass4_2026_06_12/qmvir_vector_search_medium_smoke.json`

## Suggested Commit Split

| Slice | Files | Suggested message |
|---|---|---|
| Durable WAL transaction path and profiling counters | `qm_engine/src/gateway/native_sql.rs` | `Harden native SQL durable transaction profiling` |
| Native write profiler and DELETE calibration fix | `scripts/native_sql_write_profile.py` | `Add calibrated native SQL write profiler` |
| Benchmark harness warmup control | `scripts/release_benchmark_native_sql.py` | `Add configurable native SQL benchmark warmup` |
| Pass 1/2 reports | `docs/DURABLE_WRITE_OPTIMIZATION_FIRST_PASS_2026_06_08.md`, `docs/DURABLE_WRITE_OPTIMIZATION_PASS2_PROFILE_2026_06_08.md` | `Document durable write optimization passes` |
| Pass 3/4 release validation reports | `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS3_2026_06_12.md`, `docs/DURABLE_WRITE_DELETE_COMMIT_OPTIMIZATION_PASS4_2026_06_12.md` | `Document durable write/delete/commit validation` |

## Final Release Interpretation

Pass 4 closes the stale-wheel and Xcode-linker verification debt.

QMvir is not ready for a blanket durable-write performance claim against PostgreSQL. The validated result is narrower:

- Durable read/index/rollback workloads are competitive or faster in this benchmark.
- Strict durable autocommit write/delete workloads remain substantially slower than PostgreSQL because QM waits for per-mutation sync.
- Batched large transaction insert can beat PostgreSQL in median throughput but still shows p95 variance.

Final verdict:

`PASS4_PG_VALIDATED`
