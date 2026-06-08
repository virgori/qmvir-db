# Local Release Manifest - 2026-05-18

Snapshot target:

- `release_snapshots/qm_engine_5.4.0_rc1/`

## Included Source Directories / Files

- `.dockerignore`
- `.gitignore`
- `Dockerfile`
- `Dockerfile.release`
- `LICENSE`
- `README.md`
- `BUILD_GUIDE.md`
- `USAGE_GUIDE_EN.md`
- `USAGE_GUIDE_VI.md`
- `install.sh`
- `build.sh`
- `build_from_source.sh`
- `pyproject.toml`
- `qm_app.py`
- `include/`
- `qm_engine/`
- `qm_core/`
- `gateway/`
- `core_db/`
- `analytics_platform/`
- `cache_layer/`
- `indexing/`
- `observability/`
- `pipelines/`
- `sdk/`
- `search_platform/`
- `storage/`
- `vector_platform/`
- `tools/`

## Included Tests

- `tests/`
- `qm_engine/tests/`
- `scripts/check_no_space_number_duplicates.sh`

## Included Docs

- `docs/`
- `MUST_READ_CONTEXT/`
- `version/`
- `QM_FULL_AUDIT_REPORT.md`
- `docs/STABILIZATION_REPORT_2026_05_16.md`
- `docs/RELEASE_GATE_2026_05_18.md`
- `docs/LOCAL_RELEASE_AUDIT_2026_05_18.md`
- `docs/LOCAL_RELEASE_MANIFEST_2026_05_18.md`
- `docs/LOCAL_RELEASE_CANDIDATE_REPORT_2026_05_18.md`
- `docs/ROOT_CAUSE_CLOSURE_2026_05_18.md`
- `docs/BENCHMARK_BASELINE_2026_05_18.md`

## Included Benchmark Scripts

- `scripts/release_benchmark_native_sql.py`
- `scripts/create_local_release_snapshot.py`
- `scripts/check_local_release_snapshot.py`
- `scripts/validate_local_release_snapshot.sh`
- `benchmark/`
- selected non-generated scripts/docs from `benchmarks/`

Generated benchmark output is excluded from the initial snapshot and regenerated
by validation into `docs/native_sql_benchmark_last.json`.

## Included CI / Workflow Files

- `.github/workflows/release-gate.yml`
- `.github/workflows/repo-hygiene.yml`
- `.github/workflows/release.yml`
- `.github/workflows/publish.yml`
- `.github/FUNDING.yml`

## Excluded Files And Reasons

- `.env`, `*.env` except `*.env.example`: secret/local config safety.
- `.DS_Store`: local machine junk.
- `.git/`: VCS metadata is not release source.
- `.pytest_cache/`, `__pycache__/`, `*.pyc`: generated caches.
- `build/`, `dist/`, `qm_engine/target/`, `qmvir.egg-info/`: build outputs.
- `lib/libqm_*.a`: binary/placeholder build output.
- `npm/qm-*`: platform binary packages, not source snapshot input.
- `data/`, `wal/`, `tmp/`: local runtime state.
- `vector_last_run_*.json`, `vector_timing_breakdown.json`,
  `verify_vector_mapping_last.txt`: generated benchmark evidence, regenerated if
  needed.
- `full_sql_regression_report.txt`, `performance_regression_report.txt`,
  `transaction_regression_report.txt`: generated report artifacts.
- `_py_legacy/`: legacy/experimental archive requiring owner review.
- `qmvir-studio/`: separate product scope requiring owner review.
- paths containing `" 2"`: duplicate-looking local copies.

## Files Requiring Owner Review

- `npm/.env`
- `npm/qm-*`
- `lib/libqm_*.a`
- `_py_legacy/`
- `qmvir-studio/`
- May 15 generated regression/vector report artifacts
- `.cargo/` beyond `config.toml.example`

## Exact Validation Commands

Run from inside the clean snapshot:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
python3 -m pytest -q
bash scripts/check_no_space_number_duplicates.sh
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json
python3 scripts/check_local_release_snapshot.py .
```
