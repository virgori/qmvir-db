# QMvir MVCC, Hybrid Search, and Vector Evidence Hardening

Audit date: 2026-06-08

## Scope

This pass hardened the release evidence surface only. It did not rewrite engine behavior, change benchmark numbers, delete source files, commit changes, or update generated benchmark artifacts under `docs/`.

The repository was already dirty before this pass. The pre-existing staged release slice remained staged and untouched:

- `README.md`
- `USAGE_GUIDE_EN.md`
- `USAGE_GUIDE_VI.md`
- `docs/INTEGRATED_STABILIZATION_REPORT_2026_06_08.md`
- `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md`
- `docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md`
- `qm_engine/src/gateway/native_sql.rs`
- `qm_engine/tests/native_sql_query_update_delete_audit.rs`

## Changes Made

- Added `--quick` mode to `scripts/vector_search_audit_benchmark.py`.
- Quick mode caps Python iterations at 3 and writes to `/tmp/qmvir_vector_search_audit_quick.json` unless `--output` is explicitly supplied.
- Quick mode skips the heavy exact SQL, Rust HNSW binary, and HNSW mutation workloads so it can be used as a fast reproducibility smoke gate.
- Default benchmark behavior remains unchanged: without `--quick`, the script still uses the default 200 iterations, writes to `docs/vector_search_audit_latest.json`, and runs the full workload set.

## Evidence Inventory

### MVCC

Existing Rust coverage in `qm_engine/tests/mvcc_integration.rs` verifies:

- multi-session transaction isolation and active transaction tracking
- READ COMMITTED statement snapshot refresh
- own uncommitted writes visible to the writer
- other uncommitted writes invisible
- snapshot isolation does not see commits after snapshot time
- rollback create leaves no visible row
- rollback delete restores visibility
- old snapshot reads old version after update
- lock conflict detection and lock release on commit/rollback
- monotonic commit ordering and transaction lifecycle errors

Boundary: this evidence supports implemented MVCC visibility and transaction-manager behavior. It does not claim serializable distributed correctness, CockroachDB-style guarantees, or HA consensus semantics.

### Hybrid Search

Existing Rust coverage in `qm_engine/src/executor/hybrid_search.rs` verifies:

- Reciprocal Rank Fusion candidate union, deduplication, ordering, and deterministic tie behavior
- weighted linear fusion with lexical-heavy and semantic-heavy alpha ranking
- distribution-based fusion smoke coverage
- reranker override ordering
- BM25 + HNSW reload preserves fused ranking
- text/vector mutation and deletion are reflected after reload

The benchmark script also includes Python hybrid fixture rows for BM25-only, vector-only, hybrid, explain/no-explain, update-text, update-vector, and delete-doc scenarios.

Boundary: this evidence is deterministic fixture coverage for fusion stability. It does not claim production ranking superiority or model-level relevance quality.

### HNSW and Vector

Existing Rust coverage in `qm_engine/src/index/hnsw.rs` verifies:

- top-k edge behavior and deterministic ties
- duplicate ID replacement without duplicate live entries
- remove, reinsert, lazy replace, lazy tombstone filtering, and compaction behavior
- long-run lazy mutation snapshot reload materializes live generations only
- recall fixture reported against exact brute force
- vector snapshot reload preserves replace/remove/reinsert search
- snapshot load rejects dimension mismatch, metric mismatch, duplicate IDs, non-finite vectors, and corrupt JSON

The quick benchmark smoke output produced 32 benchmark rows in `/tmp/qmvir_vector_search_audit_quick.json` with sections populated for:

- `exact_vector`
- `bm25_performance`
- `bm25_update_delete`
- `hybrid_benchmark`
- `quantization`

Boundary: quick mode is a smoke gate. Full HNSW Rust recall, persistence, compaction, mutation policy, and long-run rows remain part of the normal benchmark mode, not the quick path.

## Commands Run

Baseline and targeted gates:

- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` passed.
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` passed.
- `python3 -m pytest -q -rxX` passed with `1139 passed, 12 skipped`.
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features mvcc -- --nocapture` passed.
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hnsw -- --nocapture` passed.
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features bm25 -- --nocapture` passed.
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hybrid -- --nocapture` passed.
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features index -- --nocapture` passed.
- `python3 -m pytest tests/test_vector_comprehensive.py tests/test_bm25.py tests/test_core_internals.py tests/test_mvcc.py tests/test_wal_concurrency.py -q -rxX` passed with `129 passed`.

Post-change checks:

- `python3 -m py_compile scripts/vector_search_audit_benchmark.py` passed.
- `python3 scripts/vector_search_audit_benchmark.py --quick` passed and wrote `/tmp/qmvir_vector_search_audit_quick.json`.

## Release Gate Status

Status: evidence surface improved, but repository is not release-clean yet.

Reasons:

- Significant pre-existing staged and untracked source surface remains.
- `scripts/vector_search_audit_benchmark.py` is still untracked in the current worktree.
- This pass did not stage or commit changes.
- Quick benchmark smoke mode intentionally does not replace the full vector/HNSW audit benchmark.

## Recommended Commit Split

1. Evidence tooling:
   - `scripts/vector_search_audit_benchmark.py`
   - `docs/MVCC_HYBRID_VECTOR_EVIDENCE_HARDENING_2026_06_08.md`

2. Keep separate from the already staged native SQL/readiness slice.

3. Do not include generated JSON artifacts, local `.env`, static archives, npm binaries, WAL logs, or `dist/` output in this commit.

