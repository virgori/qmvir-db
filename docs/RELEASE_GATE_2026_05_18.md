# Release Gate Report - 2026-05-18

Release readiness: **Not Ready**

This pass turns the stabilization work into a stronger release gate: benchmark
smoke coverage now exists, crash/recovery tests now exist, pytest warning and
XPASS triage is complete, and CI has a focused release-gate workflow. The
repository is still not release-ready because the worktree remains massively
dirty with pre-existing staged/untracked files that need owner review before a
release cut.

## Worktree Audit

Command run:

```bash
git status --short
git diff --stat
git diff --cached --stat
```

Categories:

- Release-critical stabilization/release-gate changes:
  - `qm_engine/Cargo.toml`
  - `qm_engine/tests/release_crash_recovery.rs`
  - `scripts/release_benchmark_native_sql.py`
  - `docs/native_sql_benchmark_baseline.json`
  - `docs/native_sql_benchmark_last.json`
  - `.github/workflows/release-gate.yml`
  - `benchmark/README.md`
  - `tests/test_full_engine.py`
  - `tests/test_task3_integration.py`
- Existing stabilization pass files:
  - `docs/STABILIZATION_REPORT_2026_05_16.md`
  - `scripts/check_no_space_number_duplicates.sh`
  - `qm_engine/src/mvcc/*`
  - `qm_engine/tests/mvcc_integration.rs`
  - broad modified `qm_engine/src/*`, Python gateway, docs, and metadata files.
- Unrelated/pre-existing workspace changes:
  - broad staged additions across Python packages, `qmvir-studio`, SDKs,
    docs, npm package assets, static libraries, and version reports.
- Generated/build/artifact candidates:
  - `.DS_Store`
  - `lib/libqm_*.a`
  - `npm/qm-*`
  - `vector_last_run_*.json`
  - `vector_timing_breakdown.json`
  - `verify_vector_mapping_last.txt`
  - `full_sql_regression_report.txt`
  - `performance_regression_report.txt`
  - `transaction_regression_report.txt`
- Suspicious accidental changes:
  - `.DS_Store` is staged despite `.gitignore` already ignoring it.
  - `npm/.env` is untracked and should not be committed.
  - `.cargo/` is untracked and should be owner-reviewed before commit.

Cleanup plan:

- Do not revert or delete the broad staged tree automatically.
- Owner should split release-critical stabilization/release-gate changes into a
  focused commit.
- Owner should remove or unstage generated artifacts, especially `.DS_Store` and
  `npm/.env`.
- Keep `scripts/check_no_space_number_duplicates.sh` in CI; current run passed.

## Benchmark Gate

Added:

- `scripts/release_benchmark_native_sql.py`
- `docs/native_sql_benchmark_baseline.json`
- `docs/native_sql_benchmark_last.json`
- benchmark usage notes in `benchmark/README.md`

Quick CI command:

```bash
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json
```

Full local command:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

Coverage:

- simple insert/select/update/delete
- MVCC read/write transaction path
- concurrent read/write smoke throughput
- predicate execution path
- Python gateway startup/shutdown path
- vector/cache hot path
- p50/p95/p99 where per-operation latency is meaningful
- throughput for every benchmark
- peak RSS and environment metadata
- baseline comparison against `docs/native_sql_benchmark_baseline.json`

Quick benchmark result on this machine:

| Benchmark | p50 ms | p95 ms | p99 ms | throughput ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.1057 | 0.1662 | 0.1749 | 9398.57 |
| native_sql.simple_select | 0.0140 | 0.0147 | 0.0148 | 70638.13 |
| native_sql.simple_update | 0.0441 | 0.0482 | 0.0508 | 22335.97 |
| native_sql.simple_delete | 0.3470 | 0.3543 | 0.3580 | 2876.79 |
| native_sql.predicate_path | 0.0107 | 0.0110 | 0.0131 | 92208.39 |
| native_sql.mvcc_read_write | 0.1050 | 0.2388 | 0.2922 | 8055.53 |
| native_sql.concurrent_read_write_smoke | n/a | n/a | n/a | 31518.44 |
| native_sql.vector_cache_hot_path | 0.0318 | 0.0324 | 0.0324 | 31356.14 |
| python_gateway.startup_shutdown | 0.3408 | 0.4261 | 0.7878 | 2814.29 |

Environment:

- OS: macOS-26.5-arm64-arm-64bit-Mach-O
- CPU/machine: arm64, 8 logical CPUs
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- build mode: dev
- feature flags: default
- peak RSS: 30.33 MB

Baseline status:

- Baseline file exists, but it is a placeholder with no approved historical
  results yet. No performance regression claim is made.

## Pytest Warning And XPASS Triage

Changes:

- Removed the class-level `pytest.mark.asyncio` marker from
  `tests/test_task3_integration.py`; the class has a synchronous test and each
  async test already has its own marker.
- Removed obsolete `xfail` markers from 9 passing pgwire tests in
  `tests/test_full_engine.py`.
- Kept 7 `xfail` markers where the unresolved issue still reproduces.

Result:

```text
1107 passed, 12 skipped, 7 xfailed in 33.72s
```

Remaining xfails:

- Python MVCC write-write conflict detection is still unresolved.
- HubEngine SQL passthrough is still not wired for create/insert/select.
- pgwire float `BETWEEN` remains unsupported.
- VectorQuantizer memory usage and compression-ratio accounting remain coarse.

## Crash Safety Validation

Added deterministic simulated-crash coverage:

- `qm_engine/tests/release_crash_recovery.rs`

Coverage:

- committed transaction survives restart
- uncommitted transaction is not replayed after restart
- rollback state survives restart
- checkpoint reload does not resurrect deleted rows
- missing dirty page snapshot path fails fast
- secondary index catalog/query state survives checkpoint recovery
- MVCC active transaction metadata is reset after restart

Limitation:

- These are deterministic simulated crashes by dropping/reopening the engine.
  They are not true process-kill/fsync-fault tests. A later durability release
  should add subprocess kill tests with controlled fsync modes and torn-write
  fixtures.

## CI Release Gate

Added:

- `.github/workflows/release-gate.yml`

CI coverage:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `cargo check --manifest-path qm_engine/Cargo.toml`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python -m pytest -q`
- `bash scripts/check_no_space_number_duplicates.sh .`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery`
- `python scripts/release_benchmark_native_sql.py --quick --build-mode ci --feature-flags default`

## Commands Run

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
python3 -m pytest -q -rxX
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
bash scripts/check_no_space_number_duplicates.sh
```

Results:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
- `cargo check --manifest-path qm_engine/Cargo.toml`: pass
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`: pass
  - lib unit tests: 397 passed, 15 ignored
  - `bench_engine`: 15 ignored
  - `bench_new_components`: 35 passed
  - `mvcc_integration`: 23 passed
  - `release_crash_recovery`: 7 passed
  - doctests: 2 ignored
- `python3 -m pytest -q -rxX`: pass, 1107 passed, 12 skipped, 7 xfailed
- `bash scripts/check_no_space_number_duplicates.sh`: pass
- benchmark smoke: pass

## Remaining Risks

- The worktree is still too dirty for a real release cut.
- The benchmark baseline is only a placeholder; full release hardware baseline
  has not been approved yet.
- Crash safety tests are simulated, not true process-kill/torn-write tests.
- Native SQL MVCC metadata is improved, but full storage MVCC semantics still
  need deeper integration before durability/isolation claims should be expanded.

## Readiness

Status remains **Not Ready**.

Reason: release-gate mechanics are materially better and all required commands
passed, but the repository state is not clean enough and the benchmark baseline
is not mature enough for a release candidate.
