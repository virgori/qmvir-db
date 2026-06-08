# Staged Source Review and Commit Split Plan - 2026-06-08

## Scope

This plan covers the remaining source and release-surface changes after release-hygiene cleanup. It does not recommend changing engine behavior, deleting source files, changing benchmark numbers, or broad-formatting the repo.

No commits were made while producing this plan.

## Required Inspection Results

Commands run:

```bash
git diff --cached --name-status
git diff --name-status
git status --short
```

High-level state:

- Staged paths: about `418`.
- Unstaged modified paths: about `114`.
- Visible untracked paths: about `70`.
- Staged tree is mostly `A` entries, which makes this look like a large source import plus follow-up edits.
- Many paths are `AM`, meaning a file has staged content and additional unstaged changes. Committing from the current index would likely commit stale staged versions and leave related fixes behind.

Top staged directories by path count:

| Area | Count |
| --- | ---: |
| `qm_engine` | 110 |
| `qm_core` | 84 |
| `qmvir-studio` | 60 |
| `tests` | 37 |
| `search_platform` | 13 |
| `vector_platform` | 11 |
| `indexing` | 11 |
| `core_db` | 11 |
| `pipelines` | 9 |
| `npm` | 8 |

Top unstaged directories by path count:

| Area | Count |
| --- | ---: |
| `qm_engine` | 93 |
| `tests` | 7 |
| `vector_platform` | 2 |
| `npm` | 2 |
| `docs` | 2 |

Top untracked areas:

| Area | Count |
| --- | ---: |
| `docs` | 34 |
| `qm_engine` | 14 |
| `scripts` | 10 |
| `.github` | 2 |

Largest staged files by added lines:

| File | Added lines |
| --- | ---: |
| `qm_engine/src/gateway/native_sql.rs` | 10910 |
| `qmvir-studio/src-tauri/Cargo.lock` | 7004 |
| `qm_engine/src/gateway/native_sql_v2_wip.rs` | 5101 |
| `qm_engine/Cargo.lock` | 3134 |
| `qmvir-studio/package-lock.json` | 2740 |
| `tests/test_final_sprint.py` | 1611 |
| `qm_engine/src/index/hnsw.rs` | 1495 |
| `qm_core/engine.py` | 1408 |
| `qm_engine/tests/bench_engine.rs` | 1299 |
| `tests/test_distributed.py` | 1287 |

Largest unstaged files by changed lines:

| File | Added | Deleted |
| --- | ---: | ---: |
| `qm_engine/src/gateway/native_sql.rs` | 17334 | 7870 |
| `qm_engine/src/index/hnsw.rs` | 1826 | 125 |
| `qm_engine/tests/bench_new_components.rs` | 744 | 270 |
| `qm_engine/tests/bench_engine.rs` | 521 | 234 |
| `qm_engine/src/index/inverted.rs` | 411 | 48 |
| `qm_engine/src/index/bplus_tree.rs` | 341 | 105 |
| `qm_engine/src/executor/hybrid_search.rs` | 275 | 33 |
| `tests/test_vector_comprehensive.py` | 170 | 3 |

## Commit-Splitting Principles

1. Do not commit directly from the current staged set.
2. Rebuild the index per commit slice from the working tree.
3. For `AM` files, decide whether the unstaged edits belong to the same slice before staging.
4. Keep lockfiles with the manifests they belong to.
5. Keep tests with the subsystem they validate when narrowly related; otherwise commit broad regression tests separately.
6. Keep generated reports and benchmark output JSON out of source commits unless they are approved baseline artifacts.
7. Treat WIP files as review blockers unless explicitly accepted.

Recommended staging workflow for each slice:

```bash
git restore --staged .
git add <paths for one slice>
git diff --cached --stat
git diff --cached --name-status
```

Then run only the validation relevant to that slice. Commit only after review.

## Proposed Commit Slices

### 1. Release Hygiene and Repo Metadata

Purpose: land cleanup and packaging guardrails without engine changes.

Candidate paths:

```text
.gitignore
.dockerignore
.github/FUNDING.yml
LICENSE
Dockerfile
Dockerfile.release
docs/RELEASE_HYGIENE_CLEANUP_2026_06_08.md
```

Notes:

- `.gitignore` has unstaged changes from the cleanup pass; stage the working-tree version.
- Keep CI workflow files for a later CI commit, not here.
- Do not include broad docs or source changes.

Validation:

```bash
git diff --cached --name-status
git ls-files | grep -E '(^|/)\.DS_Store$|(^|/)\.env$|dist/|target/|node_modules/|__pycache__|\.pytest_cache|\.a$|docs/.*(_latest|_last|report).*\.json$' || true
```

### 2. Release Gate and Repo Hygiene CI

Purpose: add automation that checks the release surface.

Candidate paths:

```text
.github/workflows/release-gate.yml
.github/workflows/repo-hygiene.yml
.gitlab-ci.yml
scripts/check_no_space_number_duplicates.sh
scripts/check_local_release_snapshot.py
scripts/create_local_release_snapshot.py
scripts/validate_local_release_snapshot.sh
CI_CD_WORKFLOW.md
```

Notes:

- This should stay separate from engine and benchmark logic.
- Review whether `.gitlab-ci.yml` is actually wanted in a GitHub-primary repo.

Validation:

```bash
bash scripts/check_no_space_number_duplicates.sh
python3 -m py_compile scripts/check_local_release_snapshot.py scripts/create_local_release_snapshot.py scripts/validate_local_release_snapshot.py
```

### 3. Python Package Metadata, CLI Entry, and npm Wrapper

Purpose: package entry points and distribution metadata.

Candidate paths:

```text
pyproject.toml
qm_app.py
npm/.npmignore
npm/README.md
npm/bin/qmvir.js
npm/package.json
npm/scripts/postinstall.js
npm/sdk/client.d.ts
npm/sdk/client.js
npm/sdk/client.ts
npm/.env.example
sdk/
include/qm_api.h
```

Notes:

- `npm/package.json` and `npm/scripts/postinstall.js` are `AM`; stage their working-tree versions only after review.
- `npm/.env.example` must be placeholder-only.
- Do not include `npm/qm-*` binaries.

Validation:

```bash
python3 -m py_compile qm_app.py
node --check npm/bin/qmvir.js
node --check npm/scripts/postinstall.js
```

### 4. Python Core Database and Platform Layers

Purpose: commit pure Python platform modules separately from Rust engine.

Candidate paths:

```text
analytics_platform/
cache_layer/
core_db/
indexing/
observability/
pipelines/
search_platform/
storage/
vector_platform/
qm_core/
```

Suggested split within this slice if review load is too high:

- storage/index/cache layers: `core_db/`, `storage/`, `cache_layer/`, `indexing/`
- search/vector layers: `search_platform/`, `vector_platform/`
- orchestration/runtime: `qm_core/`, `analytics_platform/`, `pipelines/`, `observability/`

Notes:

- `core_db/transaction_engine/mvcc.py`, `qm_core/hub/hub.py`, `search_platform/lexical_search/bm25.py`, `vector_platform/ann_index/hnsw.py`, and `vector_platform/quantization/quantizer.py` have unstaged edits. Do not commit only the staged versions unless intentionally splitting older/newer revisions.

Validation:

```bash
python3 -m pytest tests/test_bm25.py tests/test_vector_comprehensive.py tests/test_core_internals.py -q
```

### 5. Rust Engine Foundation: Cargo, Types, Metrics, Storage, IPC, Statistics

Purpose: commit lower-level Rust modules before SQL/gateway features.

Candidate paths:

```text
qm_engine/Cargo.toml
qm_engine/Cargo.lock
qm_engine/src/lib.rs
qm_engine/src/types.rs
qm_engine/src/metrics.rs
qm_engine/src/statistics/
qm_engine/src/storage/
qm_engine/src/ipc/
```

Notes:

- `Cargo.toml`, `Cargo.lock`, storage, IPC, statistics, and type files have unstaged edits. Stage final intended working-tree versions for this slice.
- Keep `qm_engine/src/storage/wal_streaming.rs` with storage/WAL if accepted.

Validation:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features storage statistics ipc metrics types
```

### 6. Rust Index and Search Primitives

Purpose: isolate B+Tree, HNSW, inverted index, roaring, mmap, sharded and WAL-backed index work.

Candidate paths:

```text
qm_engine/src/index/
qm_engine/src/search/
qm_engine/src/executor/hybrid_search.rs
qm_engine/src/executor/vectorized.rs
qm_engine/src/hub_engine/vector_gate.rs
qm_engine/src/bin/hnsw_rust_benchmark.rs
scripts/vector_search_audit_benchmark.py
tests/test_vector_comprehensive.py
tests/test_bm25.py
```

Notes:

- `qm_engine/src/index/hnsw.rs` has very large unstaged edits; this should not be mixed with unrelated SQL gateway work.
- `scripts/vector_search_audit_benchmark.py` and output docs should not be mixed unless the script itself is being reviewed.
- Keep benchmark output JSON ignored unless explicitly approved as a baseline.

Validation:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features index executor::hybrid_search
python3 -m pytest tests/test_vector_comprehensive.py tests/test_bm25.py -q
```

### 7. Rust Backup, CLI, Web, Auth, and Packaging Surface

Purpose: separate operational tooling from query engine behavior.

Candidate paths:

```text
qm_engine/src/backup/
qm_engine/src/cli/
qm_engine/src/bin/qm.rs
qm_engine/src/bin/qm_web.rs
qm_engine/src/web/
qm_engine/src/gateway/auth.rs
qm_engine/src/gateway/scram.rs
qm_engine/static/dashboard.html
qm_engine/scripts/build_release.sh
qm_engine/scripts/bench_vs_postgres.sh
qm_engine/tests/pg_benchmark.sql
```

Notes:

- Gateway auth may also belong with PostgreSQL wire protocol depending on review ownership.
- Keep scripts/deploy.sh in a separate deployment docs/tooling commit if included.

Validation:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
python3 -m pytest tests/test_gateway_auth.py tests/test_cli_e2e.py tests/test_web_dashboard.py -q
```

### 8. Rust SQL Gateway and Native SQL Engine

Purpose: isolate the largest and riskiest engine behavior changes.

Candidate paths:

```text
qm_engine/src/gateway/connection.rs
qm_engine/src/gateway/mod.rs
qm_engine/src/gateway/native_sql.rs
qm_engine/src/gateway/protocol.rs
qm_engine/src/gateway/server.rs
qm_engine/tests/native_sql_identity_uuid_json_hash.rs
qm_engine/tests/native_sql_query_update_delete_audit.rs
qm_engine/tests/release_crash_kill_recovery.rs
qm_engine/tests/release_crash_recovery.rs
tests/test_native_sql_python_bridge_identity_uuid_json.py
```

Review blockers:

```text
qm_engine/src/gateway/native_sql_v2_wip.rs
```

Recommendation:

- Do not include `native_sql_v2_wip.rs` in a release commit unless it is deliberately compiled or gated and reviewed as non-production code.
- Because `native_sql.rs` has both the largest staged content and largest unstaged delta, this slice should be rebuilt from the working tree and reviewed carefully.

Validation:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql release_crash
python3 -m pytest tests/test_full_engine.py tests/test_native_sql_python_bridge_identity_uuid_json.py -q
```

### 9. Rust Executor, Planner, Optimizer, Cluster, Hub Engine

Purpose: commit query execution and distributed planning separately from native SQL parser/storage changes.

Candidate paths:

```text
qm_engine/src/executor/
qm_engine/src/optimizer/
qm_engine/src/parser/
qm_engine/src/hub_engine/
qm_engine/src/cluster/
qm_engine/src/learned/
qm_engine/src/procedures/
tests/test_distributed.py
tests/test_distributed_sharding.py
tests/test_planner.py
tests/test_hub_satellite_arch.py
tests/test_vector_gate_policy.py
```

Notes:

- `cluster/transport.rs` and `cluster/two_phase_commit.rs` are new and should be reviewed as experimental unless proven otherwise.
- Do not claim production HA from this commit.

Validation:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features executor optimizer parser hub_engine cluster learned procedures
python3 -m pytest tests/test_distributed.py tests/test_distributed_sharding.py tests/test_planner.py tests/test_hub_satellite_arch.py -q
```

### 10. MVCC Rust Module and Crash/Recovery Documentation

Purpose: isolate MVCC subsystem and crash safety artifacts.

Candidate paths:

```text
qm_engine/src/mvcc/
qm_engine/tests/mvcc_integration.rs
qm_engine/MVCC_DEVELOPER_GUIDE.md
qm_engine/MVCC_IMPLEMENTATION_SUMMARY.md
MVCC_COMPLETION_REPORT.txt
qm_engine/src/storage/transaction.rs
qm_engine/src/executor/txn.rs
```

Notes:

- `MVCC_COMPLETION_REPORT.txt` should be reviewed: if it is generated narrative output, prefer moving content into a docs markdown file or leaving it uncommitted.

Validation:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features mvcc mvcc_integration
python3 -m pytest tests/test_mvcc.py tests/test_wal_concurrency.py -q
```

### 11. Tests and Benchmark Harnesses

Purpose: land test coverage after or with the related implementation.

Candidate paths:

```text
tests/
qm_engine/tests/
qm_engine/benches/
benchmark/
benchmarks/
benchmark_native.py
benchmark_parallel.py
benchmark_rust_vs_c.py
tools/auto_bench.py
tools/benchmark.py
scripts/compare_postgres_native_sql.py
scripts/perf_investigate_native_sql.py
scripts/release_benchmark_native_sql.py
scripts/profile_native_sql_rust.sh
```

Review blockers:

```text
tests/_probe5.py
```

Recommendation:

- Do not include `_probe5.py` in release commits unless it is intentionally renamed and documented.
- Keep benchmark scripts separate from benchmark output JSON.
- Avoid changing benchmark numbers in this task.

Validation:

```bash
python3 -m py_compile scripts/compare_postgres_native_sql.py scripts/perf_investigate_native_sql.py scripts/release_benchmark_native_sql.py scripts/vector_search_audit_benchmark.py
python3 -m pytest -q -rxX
```

### 12. QMvir Studio Desktop App

Purpose: commit Tauri/Solid desktop app as its own product surface.

Candidate paths:

```text
qmvir-studio/
```

Notes:

- Keep `qmvir-studio/package-lock.json` with `qmvir-studio/package.json`.
- Keep `qmvir-studio/src-tauri/Cargo.lock` with `qmvir-studio/src-tauri/Cargo.toml`.
- `qmvir-studio/dist/` must remain ignored and uncommitted.
- Current build blocker remains `tsc: command not found`; reproducibility is not proven until dependencies are installed.

Validation:

```bash
cd qmvir-studio
npm ci
npm run build
```

### 13. Documentation and Release Reports

Purpose: land human-facing docs only after source slices are stable.

Candidate paths:

```text
README.md
USAGE_GUIDE_EN.md
USAGE_GUIDE_VI.md
QM_FULL_AUDIT_REPORT.md
MUST_READ_CONTEXT/
docs/*.md
version/
docs/engine_invariants.md
docs/mvcc_design.md
docs/native_vector_storage_design.md
docs/postgres_benchmark_setup.md
docs/release_notes_5_1_6_vector.md
```

Notes:

- Keep docs that contain benchmark claims aligned with the actual benchmark mode.
- Do not include ignored `*_latest.json`, `*_last.json`, profiles, or generated reports unless explicitly approved as baselines.
- Consider a separate commit for the two new audit reports:
  - `docs/QMVIR_CURRENT_AUDIT_2026_06_08.md`
  - `docs/RELEASE_HYGIENE_CLEANUP_2026_06_08.md`
  - `docs/STAGED_SOURCE_COMMIT_SPLIT_PLAN_2026_06_08.md`

Validation:

```bash
rg -n "PostgreSQL|faster|durability|fsync|group_commit|in-memory" README.md docs/*.md USAGE_GUIDE_*.md
```

## Files Requiring Explicit Owner Decision

These should not be silently included in a release commit:

```text
qm_engine/src/gateway/native_sql_v2_wip.rs
tests/_probe5.py
release_snapshots/
_py_legacy/
gateway/
.cargo/
MVCC_COMPLETION_REPORT.txt
IMPLEMENTATION_COMPLETE.txt
docs/profiles/
docs/native_sql_benchmark_baseline.json
docs/postgres_comparison_2026_05_18.json
```

Reason:

- They may be WIP, legacy, generated, large snapshot, benchmark baseline, or migration material.
- Some may be valuable, but each needs an explicit release-owner decision.

## Practical Execution Order

1. Freeze current work:
   ```bash
   git status --short > /private/tmp/qm_status_before_split.txt
   git diff --cached --name-status > /private/tmp/qm_cached_before_split.txt
   git diff --name-status > /private/tmp/qm_unstaged_before_split.txt
   ```

2. Rebuild the index:
   ```bash
   git restore --staged .
   ```

3. Apply the commit slices above one at a time with `git add <paths>`.

4. For each slice:
   ```bash
   git diff --cached --name-status
   git diff --cached --stat
   ```

5. Run the narrow validation for that slice.

6. Commit only after review.

7. After all source slices, run full release gate from a clean checkout:
   ```bash
   cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
   python3 -m pytest -q -rxX
   cd qmvir-studio && npm ci && npm run build
   ```

## Bottom Line

The current index is not safe to commit as-is. The safest path is to reset staging, then rebuild commits by ownership boundary:

1. hygiene/metadata
2. CI/scripts
3. packaging/npm/sdk
4. Python platform
5. Rust foundation
6. Rust indexes/search
7. Rust ops/CLI/backup/web
8. Rust Native SQL/gateway
9. Rust executor/planner/cluster
10. MVCC
11. tests/bench harnesses
12. Studio
13. docs/release reports

The riskiest slices are Native SQL/gateway, HNSW/indexes, distributed/cluster, and Studio reproducibility.
