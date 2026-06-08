# QMvir Python/Rust Bridge Zero-Copy Hardening Report

Audit date: 2026-06-08

## Verdict

`REDUCED_COPY_COLUMNAR_BRIDGE_SUCCESSFUL`

True zero-copy was not proven or claimed. The existing `NativeSqlEngine.execute(...)`
API remains unchanged and still returns Python `list`/`str` row materialization. A
new `NativeSqlEngine.execute_columnar(sql)` API now returns Python-owned columnar
`bytes` buffers with metadata, classified as `REDUCED_COPY` and `COLUMNAR_BATCH`.

## What Changed

- Added `NativeSqlEngine.execute_columnar(sql)` to the PyO3 wrapper.
- Kept `NativeSqlEngine.execute(...)`, `prepare(...)`, and `execute_prepared(...)`
  compatible.
- Encoded result columns as:
  - `int64_le` for parseable `INT8` columns.
  - `float64_le` for parseable `FLOAT8` columns.
  - `utf8_offsets_data` for text/vector/other columns.
- Added validity bitmap, offsets for variable-width data, row/column counts, type
  metadata, and explicit claim fields:
  - `classification = REDUCED_COPY`
  - `batch_kind = COLUMNAR_BATCH`
  - `zero_copy = false`
- Extended `scripts/bridge_materialization_audit.py` with `--cols`, `--quick-safe`,
  row-vs-columnar comparison, object estimates, and honest zero-copy status.
- Added Python bridge tests for old API compatibility, column order, numeric/text
  buffer decoding, lifetime, vector query ordering, and stable error mapping.

## Zero-Copy Claim Status

`ZERO_COPY_NOT_PROVEN_REDUCED_COPY_ONLY`

Reason:

- The Rust query surface already exposes result rows as `Vec<Vec<Option<Vec<u8>>>>`.
- The new Python API avoids per-row/per-cell Python string materialization, but
  it still copies into Python-owned `bytes`.
- Python receives `bytes` buffers, not borrowed Rust memory or Arrow C Data
  Interface buffers.

## Benchmark Audit

Artifacts were written under `/private/tmp/qmvir_bridge_zero_copy_2026_06_08/`
and were not committed.

| Case | Rows Used | Cols | Row API ms | Columnar ms | Speedup | Columnar Classification |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1k x 4 | 1,000 | 4 | 5.035 | 4.475 | 1.13x | `REDUCED_COPY` |
| 10k x 8 | 10,000 | 8 | 76.337 | 62.184 | 1.23x | `REDUCED_COPY` |
| 100k x 8 quick-safe | 10,000 | 8 | 101.189 | 93.695 | 1.08x | `REDUCED_COPY` |

The 100k run used `--quick-safe` and was capped to 10k rows by design.

## Coverage

New test file:

- `tests/test_python_rust_bridge_columnar.py`

Covered:

- Old `execute(...)` contract remains unchanged.
- Columnar result has no `rows` key.
- Numeric buffers decode in row order.
- UTF-8 offsets/data decode in row order.
- Empty text offset handling is stable.
- Python-owned buffer lifetime survives result object deletion.
- Vector SQL query ID order is preserved through columnar API.
- Vector dimension mismatch errors propagate through columnar API.

BM25 and hybrid compact Python bridge APIs were not added in this task. Existing
Rust BM25/hybrid gates were run; exposing dedicated compact Python APIs for those
paths remains deferred.

## Gates Run

Passed:

- `cargo check --manifest-path qm_engine/Cargo.toml`
- `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m py_compile scripts/bridge_materialization_audit.py`
- `python3 -m pytest tests/test_native_sql_python_bridge_identity_uuid_json.py tests/test_python_rust_bridge_columnar.py -q -rxX`
- `python3 -m pytest tests/test_native_sql_python_bridge_identity_uuid_json.py tests/test_python_rust_bridge_columnar.py tests/test_vector_comprehensive.py tests/test_core_internals.py tests/test_full_engine.py -q -rxX`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hnsw -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features hybrid -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features bm25 -- --nocapture`
- `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
- `python3 -m pytest -q -rxX`

Full gate summaries:

- Rust full: passed, including `449 passed; 0 failed; 15 ignored` lib tests,
  `35 passed` bench-new-components tests, integration suites, crash recovery,
  and doc tests ignored as expected.
- Python full: `1150 passed, 12 skipped`.

Release hygiene check:

- `git ls-files | grep -E '(^|/)\.DS_Store$|(^|/)\.env$|dist/|target/|node_modules/|__pycache__|\.pytest_cache|\.a$|docs/.*(_latest|_last|report).*\.json$' || true`
  produced no tracked artifact hits.

## Files Changed By This Task

- `qm_engine/src/gateway/native_sql.rs`
- `scripts/bridge_materialization_audit.py`
- `tests/test_python_rust_bridge_columnar.py`
- `docs/PYTHON_RUST_BRIDGE_ZERO_COPY_HARDENING_2026_06_08.md`

Note: `qm_engine/src/gateway/native_sql.rs` already had staged changes before
this task. This task added an unstaged overlay to that file and did not restore
or delete the pre-existing staged content.

## Release Notes

- Do not describe this as zero-copy.
- Correct claim: reduced-copy columnar Python bridge for `NativeSqlEngine`
  selected SQL result paths.
- Existing row API remains available for compatibility.
- Generated wheel/build artifacts and benchmark JSON outputs should remain out
  of commits.
