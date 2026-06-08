# Local Release Candidate Report - 2026-05-18

Status: **Local Release Candidate**

Snapshot path:

- `/Users/gengyang/QM/release_snapshots/qm_engine_5.4.0_rc1`

This status is scoped to the local QM Engine / NativeSqlEngine source snapshot.
It is not a final "Ready" release because the benchmark baseline is still a
placeholder and owner review is still required for excluded experimental/local
files.

## Snapshot Summary

Created a clean source snapshot from `/Users/gengyang/QM` without treating Git
dirty/staged/untracked status as the release source of truth.

Included:

- Rust engine source, tests, examples, static assets, Cargo manifest/lock.
- Python source packages needed by the test suite.
- Root package/build metadata.
- Python and Rust test suites.
- Release-gate benchmark script and baseline placeholder.
- Docs, stabilization reports, release-gate reports, and this local audit set.
- Relevant CI/workflow files.

Excluded:

- `.env` files, including `npm/.env`.
- `.DS_Store` files.
- `.git/` and local VCS metadata.
- local caches and build outputs.
- `lib/libqm_*.a` placeholder/binary artifacts.
- `npm/qm-*` platform binaries.
- historical generated benchmark/vector/regression outputs.
- `_py_legacy/`.
- `qmvir-studio/`.
- duplicate-looking paths containing `" 2"`.

Post-validation cleanup:

- Validation created local build/test outputs inside the snapshot.
- Removed generated `target/`, `__pycache__/`, and `.pytest_cache/` after
  validation.
- Kept `docs/native_sql_benchmark_last.json` as intentionally generated release
  evidence from the snapshot benchmark smoke.

Final snapshot safety check:

- no `.env`
- no `.DS_Store`
- no `target`
- no `__pycache__`
- no `.pytest_cache`
- no copied platform binaries
- no copied historical vector/regression generated artifacts
- final snapshot size: about 55 MB

## Exact Commands Run From Snapshot

Working directory:

```bash
/Users/gengyang/QM/release_snapshots/qm_engine_5.4.0_rc1
```

Commands:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
python3 -m pytest -q
bash scripts/check_no_space_number_duplicates.sh
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json
```

## Validation Results

Rust:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
- `cargo check --manifest-path qm_engine/Cargo.toml`: pass
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
  - lib unit tests: 397 passed, 15 ignored
  - `bench_engine`: 15 ignored
  - `bench_new_components`: 35 passed
  - `mvcc_integration`: 23 passed
  - `release_crash_recovery`: 7 passed
  - doctests: 2 ignored
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery`: pass, 7 passed

Python:

- `python3 -m pytest -q`: pass
- Result: `1107 passed, 12 skipped, 7 xfailed in 33.57s`

Repo hygiene:

- `bash scripts/check_no_space_number_duplicates.sh`: pass

## Benchmark Smoke Result

Command:

```bash
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json
```

Environment:

- OS: macOS-26.5-arm64-arm-64bit-Mach-O
- CPU/machine: arm64, 8 logical CPUs
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build mode: dev
- Feature flags: default
- Peak RSS: 30.33 MB

Results:

| Benchmark | p50 ms | p95 ms | p99 ms | throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.1148 | 0.2498 | 0.8989 | 7234.71 |
| native_sql.simple_select | 0.0138 | 0.0145 | 0.0156 | 71774.63 |
| native_sql.simple_update | 0.0435 | 0.0467 | 0.0483 | 22772.56 |
| native_sql.simple_delete | 0.3493 | 0.6814 | 1.2418 | 2483.61 |
| native_sql.predicate_path | 0.0114 | 0.0151 | 0.0271 | 81086.56 |
| native_sql.mvcc_read_write | 0.1052 | 0.1448 | 0.1540 | 9050.05 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 30275.97 |
| native_sql.vector_cache_hot_path | 0.0315 | 0.0573 | 0.6983 | 16756.03 |
| python_gateway.startup_shutdown | 0.2916 | 0.4002 | 0.5534 | 3117.90 |

Baseline comparison:

- `docs/native_sql_benchmark_baseline.json` exists.
- It is still a placeholder with no approved historical results.
- No regression or speedup claim is made.

## Crash / Recovery Result

Command:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
```

Result:

- pass, 7 passed

Coverage:

- committed transaction survives restart
- uncommitted transaction is not replayed after restart
- rollback state survives restart
- checkpoint reload does not resurrect deleted rows
- missing dirty page snapshot metadata fails fast
- secondary index state survives checkpoint/recovery
- MVCC active transaction metadata resets after restart

Limitation:

- Crash tests are deterministic simulated restart tests, not true process-kill,
  torn-write, or fsync-fault injection tests.

## Remaining Risks

- Benchmark baseline is not approved; this blocks a final Ready status.
- Snapshot reproducibility is documented by include/exclude rules, but there is
  not yet a checked-in deterministic packaging script that recreates the exact
  snapshot byte-for-byte.
- Excluded owner-review files still need a product decision:
  - `npm/.env`
  - `npm/qm-*`
  - `lib/libqm_*.a`
  - `_py_legacy/`
  - `qmvir-studio/`
  - historical generated vector/regression reports
- Crash safety coverage is stronger, but still simulated rather than
  process-kill/fault-injection durability validation.
- Full release benchmark should be run on stable release hardware before
  performance claims.

## Readiness Decision

Snapshot status: **Local Release Candidate**

Reason:

- clean snapshot was created
- `.env` and `.DS_Store` are not included
- generated junk and binary artifacts are excluded
- tests pass from inside the snapshot
- crash/recovery tests pass from inside the snapshot
- benchmark smoke passes from inside the snapshot
- excluded files and remaining risks are documented

Not marked **Ready** because the benchmark baseline is not approved, snapshot
reproducibility is not yet scripted as a deterministic packaging command, and
owner review is still required for excluded local/experimental files.
