# QMvir MVCC + Hybrid + Vector + Bridge Hardening - 2026-06-08

## Summary

- Overall verdict: `CORE_EVIDENCE_HARDENING_SUCCESSFUL`
- Files changed: `qm_engine/tests/mvcc_integration.rs`, `qm_engine/src/executor/hybrid_search.rs`, `scripts/vector_search_audit_benchmark.py`, `scripts/bridge_materialization_audit.py`, `tests/test_vector_comprehensive.py`, `tests/test_native_sql_python_bridge_identity_uuid_json.py`, this report.
- Tests added: 3 MVCC integration tests, 5 hybrid fixtures, 1 vector rebuild-first fixture, 4 Python bridge tests.
- Scripts added/changed: medium vector smoke in `scripts/vector_search_audit_benchmark.py`; new `scripts/bridge_materialization_audit.py`.
- Benchmarks run: vector quick, medium vector smoke, bridge materialization audit.
- Full gate: Rust and Python full gates passed after changes.
- Claim status: stronger local evidence; no full SERIALIZABLE, production cold-cache, production relevance, or zero-copy bridge claim.

## Snapshot

| Item | Path |
|---|---|
| Status before | `/private/tmp/qmvir_core_hardening_2026_06_08/status_before.txt` |
| Unstaged names before | `/private/tmp/qmvir_core_hardening_2026_06_08/unstaged_name_status_before.txt` |
| Staged names before | `/private/tmp/qmvir_core_hardening_2026_06_08/staged_name_status_before.txt` |
| Unstaged diff before | `/private/tmp/qmvir_core_hardening_2026_06_08/unstaged_before.diff` |
| Staged diff before | `/private/tmp/qmvir_core_hardening_2026_06_08/staged_before.diff` |

## Baseline

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | passed: lib `444 passed, 15 ignored`; full integration passed |
| `python3 -m pytest -q -rxX` | passed: `1139 passed, 12 skipped` |
| targeted Rust `mvcc`, `hybrid`, `hnsw`, `bm25`, `index` | passed; `hnsw` baseline took `288.71s` |
| targeted Python MVCC/WAL/vector/BM25/core/bridge | passed: `132 passed` |

## MVCC Evidence Expansion

### Tests added

| Test | Scenario | Claim boundary |
|---|---|---|
| `read_committed_new_statement_sees_newer_commit_and_allows_phantom_boundary` | READ COMMITTED reader gets a newer statement snapshot after concurrent commit. | Phantoms are allowed; this is not SERIALIZABLE. |
| `snapshot_fixed_read_ts_hides_newer_commit_boundary_not_serializable_claim` | Snapshot read timestamp keeps older row version visible while newer committed version exists. | Local snapshot visibility only. |
| `write_conflict_lock_released_after_rollback_allows_later_writer` | Write lock conflict is observed; rollback releases lock for later writer. | Table-lock conflict behavior, not distributed deadlock proof. |

### Findings

| Capability | Status | Evidence | Claim allowed |
|---|---|---|---|
| Read-your-writes | covered | existing `test_own_uncommitted_write_is_visible` | local own-write visibility |
| Rollback visibility | covered | rollback create/delete tests plus new lock rollback test | rollback visibility for tested local rows/locks |
| Snapshot stability | expanded | new fixed read-ts version-chain test | local snapshot does not see newer version |
| Lost update behavior | partial | lock conflict and existing ignored stress marker | no broad lost-update claim |
| Write conflict behavior | covered | lock conflict tests and rollback release test | local table write conflict behavior |
| Phantom behavior | documented boundary | new READ COMMITTED phantom boundary test | phantoms allowed under READ COMMITTED |
| Serializable semantics | not proven | no full serializable anomaly suite | no SERIALIZABLE claim |

### Remaining MVCC risks

Full SERIALIZABLE, distributed transactions, write-skew prevention, and production multi-session DB correctness remain unproven.

## Hybrid Search Evidence Expansion

### Fixtures added

| Fixture | Expected ranking/result | Reason |
|---|---|---|
| `lexical_heavy_exact_match_beats_vague_semantic_match_fixture` | doc `10` ranks first | `alpha=0.95` makes exact lexical evidence dominate weak vector evidence. |
| `semantic_heavy_vector_match_beats_weak_lexical_noise_fixture` | doc `20` ranks first | `alpha=0.05` makes close vector neighbor dominate weak lexical noise. |
| `equal_fused_scores_tie_break_by_document_id_fixture` | `[1, 2]` | equal fused scores sort by lower document id. |
| `missing_bm25_or_vector_score_keeps_deterministic_candidate_order_fixture` | `[10, 30]` | one-source candidates are retained; equal scores tie by id. |
| `non_finite_scores_are_sanitized_before_fusion_fixture` | NaN/inf docs are dropped | ranking must not depend on `partial_cmp` fallback for non-finite values. |

### Scoring and normalization

- Fusion method: RRF, weighted linear, and distribution-based are covered by tests.
- BM25 normalization: weighted linear uses min-max normalization after dropping non-finite scores.
- Vector normalization: vector distance is filtered for finiteness, converted to similarity with `1 - min(distance, 1)`, then min-max normalized.
- Missing scores: absent source contributes `0.0`; candidates remain in the union.
- Tie-break: final comparator sorts by descending fused score, then ascending document id.
- Determinism: deterministic for fixed candidate ordering, scores, and ids.

### Remaining hybrid risks

No production relevance-quality claim, no domain-general semantic superiority claim, and no external model ranking claim.

## HNSW / Vector Scale and Cold-ish Evidence

### Medium smoke methodology

| Setting | Value |
|---|---|
| Output | `/tmp/qmvir_vector_search_medium_smoke.json` |
| Dataset size | `10000` |
| Dimension | `128` |
| Queries | `50` |
| Seed | `20260608` |
| Metric | cosine |
| HNSW ef search | `200` |
| Ground truth | NumPy exact cosine full scan with deterministic id tie-break |
| Cold-ish method | drop in-memory object, rebuild from deterministic vector matrix, then query immediately; OS page cache not cleared |

### Results

| Dataset | Dim | recall@1 | recall@5 | recall@10 | hot p50 | hot p95 | cold-ish p50 | cold-ish p95 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 10000 | 128 | 0.94 | 0.912 | 0.922 | 0.481209 ms | 0.554375 ms | 0.332250 ms | 0.431500 ms |

Build time: `2515.340584 ms`; rebuild time: `2544.417292 ms`; index/memory estimate: `5760000 bytes`.

### Remaining vector risks

This is medium smoke evidence, not production vector DB scale. It does not prove OS cold-cache latency, 100M-scale memory efficiency, or general recall across datasets.

## Python/Rust Bridge

### Tests added

| Test | Scenario | Claim boundary |
|---|---|---|
| `test_large_result_ordering_preserved_across_python_rust_boundary` | 200 rows materialized through `NativeSqlEngine.execute`. | Ordering preserved; rows are Python list/list/string materialization. |
| `test_bridge_error_mapping_for_vector_dimension_mismatch_is_stable` | Rust vector dimension error maps to Python `RuntimeError`. | Stable error path for this error class. |
| `test_prepared_batch_like_execution_preserves_request_order_and_materialized_ids` | Prepared loop inserts requests and SELECT order is stable. | Prepared loop only; no exposed bulk row-return API claim. |
| `test_vector_query_result_order_survives_python_materialization` | Vector ORDER BY result ids preserve ranking order through Python. | IDs/order only; score alias materialization remains a boundary. |

### Materialization audit

| Path | Current data movement | Zero-copy? | Risk | Next improvement |
|---|---|---|---|---|
| Native SQL rows | Rust result converted to Python `list[list[str]]` | no | per-cell Python object materialization | typed column buffers or Arrow-style batches |
| Vector results | SQL vector ordering returns materialized Python rows | no | score/expression materialization is limited | explicit vector result API with typed distances |
| BM25 results | Rust/Python search fixtures materialize result objects/lists | no | object allocation per hit | batch result struct or array-backed scores |
| Hybrid results | Rust `HybridResult` vectors remain Rust-side in current tests | no Python zero-copy proven | bridge path for hybrid scores not exposed/proven | expose typed hybrid result API with score fields |
| Errors | Rust errors map to Python exceptions/messages | n/a | message stability must be guarded per class | typed exception classes |

Bridge audit output: `/tmp/qmvir_bridge_materialization_audit.json`

- rows returned: `1000`
- columns per row: `4`
- payload estimate: `23560 bytes`
- fetch/materialize: `2.252584 ms`
- JSON serialization used by script: `false`
- batch API used: prepared statement loop
- ordering correct: `true`
- classification: Python object materialization; not zero-copy

### Remaining bridge risks

Zero-copy is not proven. Borrowed lifetime-safe Python views are not exposed. Large result materialization still allocates Python strings/lists.

## Claim Matrix

| Area | Safe claim | Forbidden claim |
|---|---|---|
| MVCC | local visibility, rollback, snapshot, and lock-release behavior for tested cases | full SERIALIZABLE, distributed transaction correctness, CockroachDB-style semantics |
| Hybrid | deterministic fusion/tie/missing/non-finite behavior for selected fixtures | production relevance quality or domain-general semantic superiority |
| HNSW/vector | quick and medium smoke recall/timing for measured dataset and config | production cold-cache latency, production vector DB scale, 100M-scale memory efficiency |
| Python/Rust bridge | ordering preserved, selected error mapping stable, materialization classified | zero-copy bridge, no materialization overhead, lifetime-safe borrowed result views |

## Tests Run

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | passed: lib `449 passed, 15 ignored`; full integration passed |
| `python3 -m pytest -q -rxX` | passed: `1144 passed, 12 skipped` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features mvcc -- --nocapture` | passed; integration filter matched zero new tests |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test mvcc_integration -- --nocapture` | passed: `26 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hybrid -- --nocapture` | passed: `13 passed` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hnsw -- --nocapture` | passed: `6 passed` in bench file; `48.34s` |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features bm25 -- --nocapture` | passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features index -- --nocapture` | passed |
| targeted Python Phase 6 set including `tests/test_full_engine.py` | passed: `231 passed` |
| `python3 -m py_compile scripts/vector_search_audit_benchmark.py scripts/bridge_materialization_audit.py` | passed |
| `python3 scripts/vector_search_audit_benchmark.py --quick` | passed |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | passed |
| `python3 scripts/bridge_materialization_audit.py` | passed |

## Artifacts

| Artifact | Purpose |
|---|---|
| `/tmp/qmvir_vector_search_audit_quick.json` | quick vector/search smoke output |
| `/tmp/qmvir_vector_search_medium_smoke.json` | medium vector smoke output |
| `/tmp/qmvir_bridge_materialization_audit.json` | bridge materialization audit output |
| `/private/tmp/qmvir_core_hardening_2026_06_08/*` | pre-change snapshot files |

Generated JSON outputs stayed outside the repo. Artifact scan found no tracked `.DS_Store`, `.env`, build/cache output, static archive, or generated report JSON matching the release-hygiene pattern.

## Suggested Commit Split

| Slice | Files | Tests | Suggested message |
|---|---|---|---|
| MVCC evidence tests | `qm_engine/tests/mvcc_integration.rs` | `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test mvcc_integration -- --nocapture` | `test: harden mvcc visibility and lock boundary evidence` |
| Hybrid relevance fixtures | `qm_engine/src/executor/hybrid_search.rs` | `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hybrid -- --nocapture` | `test: add deterministic hybrid ranking fixtures` |
| Vector medium/cold-ish tooling | `scripts/vector_search_audit_benchmark.py`, `tests/test_vector_comprehensive.py` | vector pytest and quick/medium script runs | `bench: add medium vector smoke evidence` |
| Python/Rust bridge materialization audit | `tests/test_native_sql_python_bridge_identity_uuid_json.py`, `scripts/bridge_materialization_audit.py` | bridge pytest and audit script | `test: document python bridge materialization boundaries` |
| Final hardening report | `docs/MVCC_HYBRID_VECTOR_BRIDGE_HARDENING_2026_06_08.md` | n/a | `docs: report core evidence hardening results` |

## Final Verdict

`CORE_EVIDENCE_HARDENING_SUCCESSFUL`

