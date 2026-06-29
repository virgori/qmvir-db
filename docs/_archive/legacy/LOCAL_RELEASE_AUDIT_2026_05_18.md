# Local Release Audit - 2026-05-18

Scope: QM Engine / NativeSqlEngine local release snapshot from `/Users/gengyang/QM`.

Git status is not used as the authoritative release source. This audit classifies
files by local project structure, validation requirements, release safety, and
whether a clean reproducible snapshot can be made.

## A. Required Source Files

Required for Rust engine validation:

- `qm_engine/Cargo.toml`
- `qm_engine/Cargo.lock`
- `qm_engine/src/`
- `qm_engine/benches/`
- `qm_engine/scripts/`
- `qm_engine/static/`
- `qm_engine/examples/`
- `include/qm_api.h`

Required for Python package/test validation:

- `pyproject.toml`
- `qm_app.py`
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

Supporting root files:

- `README.md`
- `LICENSE`
- `.gitignore`
- `.dockerignore`
- `Dockerfile`
- `Dockerfile.release`
- `build.sh`
- `build_from_source.sh`
- `install.sh`

## B. Required Tests

- `qm_engine/tests/`
- `tests/`
- `scripts/check_no_space_number_duplicates.sh`

These are required because the release snapshot must validate from itself with
Rust tests, Python tests, crash/recovery tests, duplicate hygiene, and benchmark
smoke.

## C. Required Docs

Included release docs:

- `docs/STABILIZATION_REPORT_2026_05_16.md`
- `docs/RELEASE_GATE_2026_05_18.md`
- `docs/LOCAL_RELEASE_AUDIT_2026_05_18.md`
- `docs/LOCAL_RELEASE_MANIFEST_2026_05_18.md`
- `docs/LOCAL_RELEASE_CANDIDATE_REPORT_2026_05_18.md`
- `docs/engine_invariants.md`
- `docs/mvcc_design.md`
- `docs/native_vector_storage_design.md`
- `docs/native_sql_benchmark_baseline.json`

General user and architecture docs under `docs/`, root `USAGE_GUIDE_*.md`,
`BUILD_GUIDE.md`, and `MUST_READ_CONTEXT/` are release-relevant docs.

## D. Required Benchmark / Release-Gate Files

- `scripts/release_benchmark_native_sql.py`
- `benchmark/README.md`
- `benchmark/full_benchmark.py`
- `benchmark/backup_predict_chaos.py`
- selected benchmark scripts in `benchmarks/`
- `docs/native_sql_benchmark_baseline.json`
- `.github/workflows/release-gate.yml`
- `.github/workflows/repo-hygiene.yml`
- `.github/workflows/release.yml`
- `.github/workflows/publish.yml`

`docs/native_sql_benchmark_last.json` is generated evidence. It is intentionally
excluded from the source snapshot and regenerated during snapshot validation.

## E. Build Outputs

Excluded from the release snapshot:

- `build/`
- `dist/`
- `qm_engine/target/`
- `qmvir-studio/dist/`
- `qmvir-studio/src-tauri/target/`
- `qmvir.egg-info/`
- `lib/libqm_arm64.a` (0 bytes, May 12 2026)
- `lib/libqm_x64.a` (0 bytes, May 12 2026)
- `npm/qm-darwin-arm64` (7,818,176 bytes, May 12 2026)
- `npm/qm-linux-aarch64` (8,431,080 bytes, May 12 2026)
- `npm/qm-linux-x64` (9,955,120 bytes, May 12 2026)
- `npm/qm-win32-x64.exe` (9,588,736 bytes, May 12 2026)

Reason: these are distribution/build products, not source needed to validate the
local release candidate.

## F. Generated Benchmark / Report Artifacts

Excluded unless separately promoted as release evidence:

- `vector_last_run_cosine.json` (2,986 bytes, May 15 2026)
- `vector_last_run_ip.json` (3,010 bytes, May 15 2026)
- `vector_last_run_l2.json` (2,919 bytes, May 15 2026)
- `vector_timing_breakdown.json` (14,010 bytes, May 15 2026)
- `verify_vector_mapping_last.txt` (731 bytes, May 15 2026)
- `full_sql_regression_report.txt` (4,850 bytes, May 15 2026)
- `performance_regression_report.txt` (3,026 bytes, May 15 2026)
- `transaction_regression_report.txt` (5,202 bytes, May 15 2026)
- historical `benchmarks/*.json`

Reason: release snapshot validation regenerates current evidence from the clean
snapshot. Historical/generated outputs should not be mistaken for source.

## G. Local Machine Junk

Excluded:

- `.DS_Store` and nested `.DS_Store` files
- `.pytest_cache/`
- `__pycache__/`
- `*.pyc`
- `.benchmarks/`
- `.vscode/`
- `tmp/`
- `data/`
- `wal/`

Reason: local OS/editor/test/runtime state.

## H. Secret / Env Files

Excluded:

- `npm/.env` (106 bytes, May 8 2026)

The file was classified by path and metadata only. Its contents were not printed
or copied. `.env` files are always excluded from the release snapshot.

Included as non-secret example/config:

- `npm/.env.example`
- `.cargo/config.toml.example`

## I. Experimental / Unreviewed Workspace Files

Excluded from the QM Engine / NativeSqlEngine snapshot:

- `_py_legacy/`
- `qmvir-studio/`
- `.gitlab-ci.yml`
- `CI_CD_WORKFLOW.md`
- `IMPLEMENTATION_COMPLETE.txt`
- `MVCC_COMPLETION_REPORT.txt`
- `qm_engine/studio_smoke_data/`
- duplicate-looking paths containing `" 2"` under legacy/platform subtrees

Reason: not required for the engine validation target and requires owner review
before inclusion in a release source snapshot.

## J. Files Requiring Owner Review

- `npm/.env`: confirm deletion/rotation outside release snapshot.
- `npm/qm-*`: decide whether these belong to a separate binary distribution.
- `lib/libqm_*.a`: currently zero-byte placeholders; do not ship as source.
- `_py_legacy/`: decide whether to archive or delete.
- `qmvir-studio/`: separate product release scope.
- generated report artifacts from May 15 2026: decide whether any should be
  promoted into immutable release evidence docs.
- `.cargo/`: only `config.toml.example` is safe to copy; real local cargo config
  should remain excluded unless reviewed.
