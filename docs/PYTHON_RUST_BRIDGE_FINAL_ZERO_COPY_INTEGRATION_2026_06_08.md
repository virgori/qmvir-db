# QMvir Python/Rust Bridge Final Zero-Copy Integration - 2026-06-08

## Summary

- Overall verdict: `TRUE_ZERO_COPY_PARTIAL_WITH_EXPLICIT_FALLBACKS`
- APIs finalized: `execute_columnar_zero_copy`, `search_bm25_compact`, `search_hybrid_compact`
- Files changed: bridge implementation, module export, bridge audit script, true zero-copy tests, zero-copy docs
- Tests added/updated: Python bridge zero-copy tests and Rust internal columnar tests
- Benchmarks run: row vs reduced-copy vs zero-copy bridge, plus BM25/hybrid compact modes
- Full gate: Rust and Python full gates passed
- Zero-copy claim status: selected direct SQL result buffers are zero-copy to Python; unsupported SQL falls back

## Snapshot

| Item | Path |
|---|---|
| status before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/status_before.txt` |
| unstaged name-status before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/unstaged_name_status_before.txt` |
| staged name-status before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/staged_name_status_before.txt` |
| unstaged diff before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/unstaged_before.diff` |
| staged diff before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/staged_before.diff` |
| native_sql staged diff before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/native_sql_staged_before.diff` |
| native_sql unstaged diff before | `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/native_sql_unstaged_before.diff` |

## API Contract

`NativeSqlEngine.execute_columnar_zero_copy(sql)` returns a dict with stable top-level fields:

```python
{
    "classification": "ZERO_COPY" | "REDUCED_COPY_FALLBACK",
    "zero_copy_subtype": "ZERO_COPY_NUMERIC_ONLY" | "ZERO_COPY_UTF8_OFFSETS_DATA" | None,
    "column_names": ["id", "score", "name"],
    "types": ["int64", "float64", "utf8"],
    "row_count": 123,
    "column_count": 3,
    "buffers": {
        "id": memoryview,
        "score": memoryview,
        "name": {"offsets": memoryview, "data": memoryview},
    },
    "columns": [... per-column metadata ...],
    "fallback_reason": None | str,
}
```

The existing per-column metadata remains for compatibility and includes
`physical_type`, `zero_copy`, `copy_reason`, `buffer_owner`, and `buffer_kind`.

## Supported Paths

| Path | Classification | Evidence |
|---|---|---|
| `SELECT cols FROM table` | `ZERO_COPY` | direct `NativeColumnarBatch` path |
| `SELECT cols FROM table ORDER BY id` | `ZERO_COPY` | id-sorted typed column cache |
| `SELECT cols FROM table LIMIT n` | `ZERO_COPY` | bounded direct batch |
| mixed int/float/text | `ZERO_COPY` | memoryview numeric buffers plus UTF-8 offsets/data memoryviews |
| empty result set | `ZERO_COPY` | zero-length buffers with schema metadata |

## Fallback Paths

| Path | Classification | Reason |
|---|---|---|
| WHERE | `REDUCED_COPY_FALLBACK` | predicate path materializes/filter rows |
| JOIN | `REDUCED_COPY_FALLBACK` | complex row construction |
| GROUP BY | `REDUCED_COPY_FALLBACK` | aggregation output is row-shaped |
| CTE/subquery | `REDUCED_COPY_FALLBACK` | nested result handling uses legacy rows |
| set operations | `REDUCED_COPY_FALLBACK` | row set materialization |
| expression projection | `REDUCED_COPY_FALLBACK` | expression evaluation returns cells/bytes |
| vector distance ordering | `REDUCED_COPY_FALLBACK` | scoring path materializes ranked rows |
| non-id ORDER BY | `REDUCED_COPY_FALLBACK` | sorted row path |
| DESC/OFFSET | `REDUCED_COPY_FALLBACK` | direct path currently supports ASC/default id order only |
| non-SELECT | error or fallback error propagation | old error mapping preserved |

## Memory Ownership And Lifetime

`NativeBuffer` is a PyO3 class that owns immutable Rust `Arc` buffers. It
implements the read-only Python buffer protocol. Python `memoryview` objects hold
the buffer owner alive, so memoryviews remain readable after deleting the result
dict. Tests cover escaped memoryviews for numeric and UTF-8 offsets/data buffers.

The bridge does not expose mutable engine-owned table storage. Supported result
buffers are immutable Arc-backed batches.

## Tests

| Test | Scenario | Result |
|---|---|---|
| `test_old_execute_still_returns_row_materialization` | old row API compatibility | pass |
| `test_reduced_copy_execute_columnar_still_works` | previous reduced-copy API | pass |
| `test_execute_columnar_zero_copy_api_exists_and_returns_memoryviews` | new API/schema/memoryviews | pass |
| `test_numeric_and_float_memoryviews_decode_correctly` | int/float roundtrip | pass |
| `test_utf8_offsets_and_data_memoryviews_decode_correctly` | text offsets/data roundtrip | pass |
| `test_memoryview_lifetime_survives_result_deletion` | lifetime safety | pass |
| `test_unsupported_sql_falls_back_with_explicit_reason` | non-id order fallback | pass |
| `test_vector_query_ordering_falls_back_stably` | vector fallback order | pass |
| `test_vector_dimension_mismatch_error_propagates` | error stability | pass |
| `test_bm25_compact_result_order_scores_limit_and_empty` | BM25 compact buffers | pass |
| `test_hybrid_compact_result_order_scores_limit_and_empty` | hybrid compact buffers | pass |
| Rust internal columnar tests | typed batch and unsupported order | pass |

## Benchmarks

| Rows | Cols | Row API | Reduced | Zero-copy | Speedup | Notes |
|---:|---:|---:|---:|---:|---:|---|
| 1,000 | 4 | 18.910 ms | 15.379 ms | 7.376 ms | 2.56x vs row | `ZERO_COPY` |
| 10,000 | 8 | 90.775 ms | 82.956 ms | 56.991 ms | 1.59x vs row | `ZERO_COPY` |
| 10,000 | 8 | 90.800 ms | 75.145 ms | 53.378 ms | 1.70x vs row | 100k requested, quick-safe cap |

## BM25/Hybrid Compact Buffers

| Path | Buffers | Classification | Caveat |
|---|---|---|---|
| BM25 compact | `doc_id`, `score`, `rank` | `ZERO_COPY` output buffers | BM25 scoring may allocate internally |
| Hybrid compact | `doc_id`, `score`, `rank` | `ZERO_COPY` output buffers | hybrid scoring may allocate internally |

## Claim Matrix

| Claim | Verdict | Reason |
|---|---|---|
| Selected direct SQL columnar buffers zero-copy | SAFE | Python memoryviews over PyO3 owner with Rust Arc buffers |
| Old row API unchanged | SAFE | existing tests pass |
| All SQL paths zero-copy | UNSAFE | unsupported paths fallback |
| BM25/hybrid output buffers zero-copy | CONDITIONAL | output buffers are memoryviews; internals may allocate |
| BM25/hybrid internals zero-copy | UNSAFE | scoring uses temporary vectors |
| Full engine zero-copy | UNSAFE | legacy row API and many SQL paths still materialize |

## Final Gates

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml` | pass |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | pass: lib `451 passed; 15 ignored`, bench-new-components `35 passed`, integration/crash/doc suites pass/ignored |
| targeted Python bridge/vector/core/full-engine | pass |
| `python3 -m pytest -q -rxX` | pass: `1163 passed, 12 skipped` |
| Rust `native_sql`, `hnsw`, `hybrid`, `bm25` filters | pass |
| `python3 -m py_compile scripts/bridge_materialization_audit.py` | pass |
| bridge audit 1k/10k/100k quick-safe | pass |
| BM25/hybrid compact audit modes | pass |
| release artifact grep | pass: no tracked artifact hits |

## Artifacts

Raw benchmark and snapshot outputs are under:

`/private/tmp/qmvir_finalize_zero_copy_2026_06_08/`

Generated JSON was not written under `docs/` and was not staged.

## Suggested Commit Split

| Slice | Files | Tests | Suggested commit |
|---|---|---|---|
| Core zero-copy columnar bridge | `qm_engine/src/gateway/native_sql.rs`, `qm_engine/src/lib.rs`, `tests/test_python_rust_bridge_true_zero_copy.py` | bridge tests, Rust native_sql | `feat: add selected-path zero-copy columnar bridge` |
| Compact search bridge buffers | `qm_engine/src/gateway/native_sql.rs`, `tests/test_python_rust_bridge_true_zero_copy.py` | BM25/hybrid compact tests | `feat: add compact search result bridge buffers` |
| Bridge materialization benchmark | `scripts/bridge_materialization_audit.py` | py_compile, audit script modes | `bench: add bridge materialization audit modes` |
| Zero-copy docs | zero-copy audit/hardening/final docs | doc review plus full gates | `docs: document true zero-copy bridge boundaries` |

`qm_engine/src/gateway/native_sql.rs` cannot be cleanly split by file because
core bridge, compact helpers, and Rust tests share the same file, and the file
already had a large staged Native SQL addition before this task. Use interactive
hunk staging if a strict split is required.

## Remaining Risks

- Unsupported SQL fallback paths still use reduced-copy row materialization.
- UTF-8 buffers are offsets/data batches, not borrowed original storage pages.
- Reduced-copy fallback speed can still dominate unsupported queries.
- `native_sql.rs` has staged/unstaged overlap from prior work.
- Future Arrow C Data Interface or NumPy integration could improve external
  interoperability.

## Final Verdict

`TRUE_ZERO_COPY_PARTIAL_WITH_EXPLICIT_FALLBACKS`
