# QMvir Python/Rust Bridge True Zero-Copy Hardening

Audit date: 2026-06-08

## Verdict

`TRUE_ZERO_COPY_PARTIAL_SUCCESSFUL`

QMvir now has a zero-copy-capable Python bridge for selected direct columnar SQL
result paths. The result buffers are Rust-owned `Arc` buffers exposed to Python
as `memoryview` objects through a PyO3 buffer-protocol owner class.

This is partial. The legacy row API and unsupported SQL paths still use
`Vec<Vec<Option<Vec<u8>>>>` and are classified as fallback.

## What Changed

- Added internal result types:
  - `NativeColumnData`
  - `NativeColumn`
  - `NativeColumnarBatch`
  - `ColumnarClassification`
- Added `NativeSqlEngine::execute_columnar_internal(sql)` for supported direct
  columnar scans.
- Added Python `NativeBuffer`, a read-only PyO3 buffer-protocol owner over
  Rust `Arc` data.
- Added `NativeSqlEngine.execute_columnar_zero_copy(sql)`.
- Kept existing APIs unchanged:
  - `execute(...)`
  - `execute_columnar(...)`
  - `prepare(...)`
  - `execute_prepared(...)`
- Added compact bridge helpers:
  - `search_bm25_compact(...)`
  - `search_hybrid_compact(...)`
- Extended `scripts/bridge_materialization_audit.py` with:
  - `--api`
  - `--verify-buffer-kind`
  - `--verify-memoryview`
  - zero-copy/reduced-copy/row metrics
  - per-column classification

## Zero-Copy Claim Status

`ZERO_COPY_PROVEN` for supported `execute_columnar_zero_copy(...)` result buffers.

Proof scope:

- Python receives `memoryview` objects.
- The memoryviews are over PyO3 `NativeBuffer` objects.
- `NativeBuffer` owns Rust `Arc<[i64]>`, `Arc<[f64]>`, or `Arc<[u8]>`.
- Tests keep memoryviews after deleting the result dict and verify data remains
  readable.

Not claimed:

- full SQL-engine zero-copy for every query
- Arrow C Data Interface
- zero-copy over original storage pages
- zero-copy for legacy `execute(...)`

## Supported Zero-Copy Paths

Supported direct SQL shapes:

- `SELECT cols FROM table`
- `SELECT cols FROM table ORDER BY id`
- optional `LIMIT`
- empty result sets

Supported column types:

- integers as `int64_le`
- floats as `float64_le`
- text as `utf8_offsets_data`

Batch-level `zero_copy = true` is set only when every column buffer in the result
is exposed as a memoryview over Rust-owned buffers.

## Reduced-Copy Fallback Paths

Fallback classification:

`REDUCED_COPY_FALLBACK`

Fallback applies to:

- non-SELECT statements
- WHERE predicates
- JOIN/GROUP/HAVING
- UNION/INTERSECT/EXCEPT
- CTE/subquery shapes
- expression projections
- vector distance ordering
- ORDER BY non-id columns
- ORDER BY id DESC/OFFSET

Fallback still goes through the legacy row result and therefore must not be
called true zero-copy.

## Python Lifetime / Ownership Model

The result dict contains memoryviews and owner references. The memoryview also
keeps the owner alive through the Python buffer protocol. Holding a memoryview
after deleting the result dict remains safe; the Rust `Arc` data is retained
until the memoryview is released.

All buffers are read-only.

## BM25 / Hybrid Compact Bridge Status

Added compact helper APIs:

- `NativeSqlEngine.search_bm25_compact(documents, query, top_k=10)`
- `NativeSqlEngine.search_hybrid_compact(bm25_scores, vector_scores, alpha=0.5, top_k=10)`

These return compact numeric buffers:

- `doc_id`: `int64_le`
- `score`: `float64_le`
- `rank`: `int64_le`

Classification is `ZERO_COPY` for Python output buffers, with subtype `ZERO_COPY_NUMERIC_ONLY`. This does
not claim that BM25/hybrid internals avoid all temporary score materialization.

## Benchmark Audit

Artifacts were written under `/private/tmp/qmvir_finalize_zero_copy_2026_06_08/`
and were not committed.

| Case | Rows Used | Cols | Row API ms | Reduced Columnar ms | Zero-Copy API ms | Classification |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1k x 4 | 1,000 | 4 | 18.910 | 15.379 | 7.376 | `ZERO_COPY` / subtype `ZERO_COPY_UTF8_OFFSETS_DATA` |
| 10k x 8 | 10,000 | 8 | 90.775 | 82.956 | 56.991 | `ZERO_COPY` / subtype `ZERO_COPY_UTF8_OFFSETS_DATA` |
| 100k x 8 quick-safe | 10,000 | 8 | 90.800 | 75.145 | 53.378 | `ZERO_COPY` / subtype `ZERO_COPY_UTF8_OFFSETS_DATA` |

The 100k run used `--quick-safe` and was capped to 10k rows.

## Tests Added

- `tests/test_python_rust_bridge_true_zero_copy.py`

Coverage includes:

- old row API unchanged
- previous reduced-copy API still works
- zero-copy API exists
- numeric memoryview decoding
- float memoryview decoding
- UTF-8 offsets/data decoding
- lifetime after result deletion
- safe deletion when no views escape
- column-level classification fields
- honest fallback classification
- vector query fallback order
- vector dimension mismatch propagation
- empty result buffers
- BM25 compact order/score/top-k/empty behavior
- hybrid compact order/score/top-k/empty behavior

Rust unit tests were added for:

- internal numeric columnar batch
- internal UTF-8 offsets/data batch
- unsupported order fallback reason

## Gates Run

Passed:

- `cargo check --manifest-path qm_engine/Cargo.toml`
- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m py_compile scripts/bridge_materialization_audit.py`
- `python3 -m pytest tests/test_native_sql_python_bridge_identity_uuid_json.py tests/test_python_rust_bridge_columnar.py tests/test_python_rust_bridge_true_zero_copy.py -q -rxX`
- `python3 -m pytest tests/test_vector_comprehensive.py tests/test_core_internals.py tests/test_full_engine.py -q -rxX`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hnsw -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hybrid -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features bm25 -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m pytest -q -rxX`
- `git ls-files | grep -E '(^|/)\.DS_Store$|(^|/)\.env$|dist/|target/|node_modules/|__pycache__|\.pytest_cache|\.a$|docs/.*(_latest|_last|report).*\.json$' || true`

Gate summaries:

- targeted bridge Python: `26 passed`
- vector/core/full-engine Python: `178 passed`
- full Python: `1163 passed, 12 skipped`
- Rust `native_sql` filter: `129 passed; 0 failed; 15 ignored`
- Rust `hnsw`, `hybrid`, and `bm25` filters: passed
- full Rust: lib `451 passed; 0 failed; 15 ignored`, bench-new-components
  `35 passed`, integration/crash/doc suites passed or ignored as expected
- release hygiene grep: no tracked artifact hits

## Files Changed

- `qm_engine/src/gateway/native_sql.rs`
- `qm_engine/src/lib.rs`
- `scripts/bridge_materialization_audit.py`
- `tests/test_python_rust_bridge_true_zero_copy.py`
- `docs/PYTHON_RUST_BRIDGE_TRUE_ZERO_COPY_AUDIT_2026_06_08.md`
- `docs/PYTHON_RUST_BRIDGE_TRUE_ZERO_COPY_HARDENING_2026_06_08.md`

## Release Notes

- Safe public claim: partial true zero-copy Python bridge for selected direct
  columnar result paths.
- Do not claim full-engine or all-query zero-copy.
- Existing row API remains the compatibility API.
- Unsupported SQL falls back explicitly and honestly.
- Generated wheels, target files, and benchmark JSON outputs must not be
  committed.
