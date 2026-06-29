# NativeSqlEngine Identity / UUID / Hash / JSON Audit - 2026-06-02

## 1. Executive Summary

This pass focused on semantic correctness and benchmark honesty for automatic integer IDs, UUID, hash usage, JSON, and JSONB-like behavior in NativeSqlEngine/QMvir. It did not change benchmark workload semantics and does not claim PostgreSQL-compatible JSONB.

Status: correctness guardrails improved and validated. Updated on 2026-06-03 with follow-up concurrency, process-kill recovery, JSON policy, Python bridge, UUID lookup, and JSON index parity coverage.

Main fixes:

- Added persistent per-table `next_auto_id` for omitted `id INTEGER` inserts.
- Added UUID type validation and lowercase canonicalization on insert/update.
- Added JSON/JSONB alias validation and canonical storage with `serde_json`.
- Fixed quoted JSON object parsing and value splitting through nested JSON.
- Made unsupported JSON containment `@>` fail clearly instead of returning a fake result.
- Moved autocommit WAL append after successful mutation execution, so failed invalid UUID/JSON/duplicate statements are not replayed from WAL.
- Replaced randomized `DefaultHasher` use in `ANALYZE` distinct-value statistics with stable FNV-1a text hashing.

Release claim boundary:

- NativeSqlEngine has a narrow, tested identity/UUID/JSON subset.
- JSONB is an alias over canonical JSON text, not PostgreSQL binary JSONB.
- UUID is stored as canonical text, not native 16-byte binary.
- Stable hash added here is diagnostic/statistical, not cryptographic.

## 2. Feature Inventory

| Feature | Files | Current behavior | Current tests | Known gaps | Risk |
| --- | --- | --- | --- | --- | --- |
| Automatic ID | `qm_engine/src/gateway/native_sql.rs`, `qm_engine/src/backup/restore.rs`, `qm_engine/src/bin/native_sql_core_bench.rs` | Omitted integer column named `id` generates monotonic IDs from persistent `next_auto_id`. Explicit high IDs advance counter. Delete/rollback do not reuse generated IDs. Failed inserts restore counter. | persistent/reopen, rollback, process-abort, and cloned-handle concurrent generated-ID tests | Not PostgreSQL SERIAL/IDENTITY syntax; only integer column named `id`; collision safety is tested for one shared engine instance/cloned handles, not independently opened engines sharing one data directory | Medium |
| UUID | `qm_engine/src/gateway/native_sql.rs` | `UUID` column values are validated, canonicalized lowercase, stored as text-like cell, persisted/reopened. UUID generation functions return v4-shaped strings. | explicit, invalid, canonical, duplicate, generated, persistence, process-abort, rollback, Python string-binding, lookup probe tests | Stored as string, not 16-byte binary; generator is not documented as cryptographic or v7; UUID primary-key lookup is generic scan/generic text index, not native UUID O(1)/binary path | Medium |
| Hash | `qm_engine/src/gateway/native_sql.rs` | `ANALYZE` distinct-value stats use stable FNV-1a text hash. | internal unit test for stable text hash and canonical JSON equivalence | No public SQL hash API validated here; not a checksum/security hash; collision handling not exposed/tested for this stats-only path | Medium |
| JSON | `qm_engine/src/gateway/native_sql.rs` | Valid JSON is parsed and stored as canonical JSON text. Objects, arrays, booleans, null, and extraction predicates are tested. Invalid JSON errors. | JSON insert/select/extraction/persistence/process-abort/rollback/index-parity tests | Python dict/list direct binding is rejected by the current string-parameter API; limited query operators; missing key and JSON null both extract as SQL NULL in supported extraction path | Medium |
| JSONB alias | `qm_engine/src/gateway/native_sql.rs` | `JSONB` parses through same canonical JSON storage as `JSON`. Structural object equality for different key order is tested through canonical text. Duplicate keys keep the last parsed value through serde_json. | JSONB canonical equality, duplicate-key/numeric/array policy, and unsupported containment error tests | No binary JSONB, no GIN, no containment, no full PostgreSQL JSONB null/missing/operator semantics | High if overclaimed |
| Python/Rust bridge | Rust wheel + Python tests | Rebuilt wheel exposes the updated Rust implementation. `execute_prepared` accepts string parameters; UUID/JSON/JSONB round-trip as canonical strings. | `python3 -m pytest -q -rxX` plus direct bridge policy tests | Direct Python `uuid.UUID` and dict/list JSON binding are explicitly rejected by the binding signature | Medium |

## 3. Auto ID Findings

Implemented policy:

- Automatic generation applies to omitted `id INTEGER` columns.
- Generated IDs are monotonic per table.
- Explicit `NULL` for an `id INTEGER PRIMARY KEY` still fails through NOT NULL/PK constraints.
- Explicit `id = 100` advances the next generated ID to `101`.
- Deleted IDs are not reused.
- Rolled-back generated IDs are not reused.
- Failed inserts do not advance the counter.
- Persistent reopen continues from the stored counter.
- Concurrent inserts through cloned handles of one shared `NativeSqlEngine` instance do not duplicate generated IDs.

Implementation details:

- `NativeTable` now stores `next_auto_id: i64`.
- Insert normalization fills omitted integer `id`.
- Constraint/vector/FK validation failure restores the prior counter.
- Rollback restores table state while preserving the greater of current/restored `next_auto_id` to prevent reuse.
- Restore/core-benchmark table constructors initialize or advance the counter.

Concurrency boundary:

- Approved: generated ID collision safety within one shared `NativeSqlEngine` instance, including cloned handles that share the same table lock.
- Not claimed: independent processes or independently opened engine instances sharing one data directory. No cross-process generated-ID allocator is implemented in this pass.

Rejected claim: PostgreSQL-compatible `SERIAL`, `AUTOINCREMENT`, or SQL-standard `IDENTITY` semantics. The supported subset is narrower.

## 4. UUID Findings

Implemented behavior:

- `UUID` column insert/update validates canonical UUID syntax.
- Uppercase UUID input is normalized to lowercase.
- Invalid UUID strings fail clearly.
- Duplicate UUID primary keys fail.
- UUID values persist and can be selected after reopen.
- Projection paths now return the actual UUID `id` cell instead of hidden integer row id.

Known limitations:

- UUID storage is canonical string, not 16-byte binary.
- UUID generation is v4-shaped smoke-tested for uniqueness, but not claimed cryptographically secure.
- UUID v7/time ordering is not supported.
- UUID primary-key lookup is correctness-tested, but not claimed as a specialized UUID primary-key index ceiling path.
- A 512-row lookup probe measured `uuid_pk_lookup_text_scan_us=4576` and `uuid_pk_lookup_generic_text_index_us=37` in the focused test run. This is diagnostic only; no latency claim is made.

## 5. Hash Findings

Before this pass, `ANALYZE` distinct-value stats used Rust randomized process-local hashing. That is unsuitable for any persistent/restart-stable semantic.

Fix:

- Added `NativeSqlEngine::stable_text_hash`, FNV-1a 64-bit over text bytes.
- Replaced `DefaultHasher` in `ANALYZE` distinct stats.
- Added internal unit test proving stable same-input output and canonical JSON object order equivalence before hashing.

Claim scope:

- Approved: deterministic non-cryptographic stats hash.
- Rejected: content checksum, security hash, persistent row ID hash, or collision-proof index key.

## 6. JSON Findings

Implemented behavior:

- `JSON` columns reject invalid JSON on insert/update.
- Valid JSON values are canonicalized with `serde_json::to_string`.
- Object key order canonicalization is provided by `serde_json` map serialization for parsed values.
- Scalars, arrays, booleans, null, nested extraction, equality, persistence, and full-scan/index parity are tested in the supported subset.
- Duplicate keys use `serde_json` parser behavior: the last key wins. `{"a":1,"a":2}` stores as `{"a":2}`.
- Numeric canonical representation preserves serde_json text distinctions such as `1` vs `1.0`; they are not normalized to one representation.
- Object key order canonicalizes for equality through serialized text.
- Array order remains significant.
- In the supported `JSON_EXTRACT_PATH_TEXT` path, JSON null and a missing key both evaluate as SQL NULL.

Parser fixes:

- Quoted JSON object literals are no longer misclassified as PostgreSQL array literals.
- Value splitting now respects nested `{}`, `[]`, and `()` in addition to SQL quotes.
- `JSON_EXTRACT_PATH_TEXT` parsing uses the function name length instead of a bad hardcoded offset.

Unsupported:

- Full PostgreSQL JSON operator suite.
- PostgreSQL JSON null-vs-missing-key parity.
- Python dict/list direct insertion.

## 7. JSONB Findings

`JSONB` currently aliases the canonical JSON text representation. This pass intentionally rejects PostgreSQL-compatible JSONB claims.

Approved:

- Valid JSONB input is parsed and stored canonically.
- Equivalent object key order can compare equally in the tested path.
- Duplicate-key, numeric text, array-order, and null/missing policies are pinned to current serde_json-backed behavior, not PostgreSQL JSONB parity.
- Unsupported containment `@>` errors clearly.

Rejected:

- Binary JSONB storage.
- PostgreSQL JSONB containment, GIN indexes, structural operator coverage, duplicate-key policy parity, and null/missing semantics.

## 8. Bugs Fixed

| Area | Fix |
| --- | --- |
| Auto ID | Added persistent `next_auto_id` and omitted integer-id generation. |
| Auto ID failure | Restored counter on failed insert validation. |
| Auto ID rollback | Preserved monotonic no-reuse policy across rollback. |
| Restore | Initialized/advanced `next_auto_id` during restore paths. |
| UUID | Validated and canonicalized UUID values on insert/update. |
| UUID projection | Returned actual UUID `id` cell rather than hidden row id. |
| JSON | Validated/canonicalized JSON and JSONB alias values. |
| JSON parser | Fixed quoted object parsing, nested splitter, and extract-path offset. |
| JSONB honesty | Unsupported `@>` now errors instead of pretending support. |
| WAL correctness | Autocommit WAL append now occurs only after successful mutation execution. |
| Hash | Replaced randomized stats hashing with stable FNV-1a text hash. |

## 9. Tests Added

New Rust integration target:

- `qm_engine/tests/native_sql_identity_uuid_json_hash.rs`

Coverage:

- Generated integer IDs: basic, multiple rows, explicit high ID, delete no reuse, rollback no reuse, failed insert deterministic counter behavior.
- Persistent generated ID reopen.
- UUID validation, lowercase canonicalization, uniqueness, generated functions, persistence.
- JSON validation, canonicalization, scalar/array/object support, extraction predicates, persistence.
- JSONB alias canonical equality and unsupported containment error.
- UUID/JSON index path vs full scan parity.
- Concurrent generated IDs through cloned engine handles.
- JSONB duplicate-key, numeric representation, object key-order, array-order, and null/missing extraction policy.
- UUID primary-key lookup correctness and diagnostic scan-vs-generic-index timings.
- JSON update/delete/rollback/reopen parity for supported extraction predicates and generic JSON text indexes.

New Python bridge tests:

- String UUID/JSON/JSONB prepared parameters round-trip as canonical strings.
- Python `uuid.UUID`, `dict`, `list`, and arbitrary object parameters are rejected by the current `Vec<String>` binding.

New internal unit test:

- `gateway::native_sql::tests::analyze_distinct_hash_is_stable_for_canonical_json_text`

## 10. Persistence / Reopen Validation

Direct tests added:

- Generated integer ID counter survives persistent reopen.
- UUID value survives persistent reopen and remains queryable.
- JSON value survives persistent reopen and remains queryable.
- JSONB canonical storage persists through the same NativeSqlEngine storage model.
- Process-abort recovery validates committed generated IDs, committed UUID/JSON, invalid UUID/JSON failures, duplicate failure before WAL append, and rolled-back UUID/JSON.

Existing release recovery suites also pass after this pass.

## 11. Crash / Recovery Validation

Process-kill coverage was extended for UUID/JSON/auto-ID. Current crash/recovery targets pass:

- `release_crash_recovery`: 10 passed.
- `release_crash_kill_recovery`: 15 passed.

Validated process-abort cases:

- committed generated ID survives abort after synced COMMIT marker.
- generated counter continues after recovery.
- failed duplicate insert before WAL append is not replayed.
- invalid UUID insert does not appear after crash/reopen.
- invalid JSON insert does not appear after crash/reopen.
- committed UUID survives crash/recovery.
- committed JSON survives crash/recovery.
- rolled-back UUID/JSON mutations do not reappear.

## 12. Python / Rust Bridge Behavior

Wheel rebuilt and reinstalled:

- `/private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl`

Python validation:

- `python3 -m pytest -q -rxX`: `1136 passed, 12 skipped in 28.24s`.

Current bridge behavior:

- `execute_prepared` accepts string parameters (`Vec<String>` in PyO3).
- Python `uuid.UUID` direct object binding is rejected.
- Python dict/list direct JSON/JSONB binding is rejected.
- JSON/JSONB results are returned as canonical JSON strings, not Python dict/list objects.

## 13. Benchmarks

Environment from `docs/native_sql_perf_investigation_last.json`:

- OS: macOS-26.5-arm64-arm-64bit-Mach-O
- CPU count: 8
- RAM: 16.00 GiB
- Python: 3.13.3
- Rust: rustc 1.94.0 (4a4ef493e 2026-03-02)
- Build mode: release
- Feature flags: default

Release benchmark:

| Workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| native_sql.simple_insert | 0.003833 | 0.004291 | 0.006292 | 244411.65 |
| native_sql.simple_select | 0.003250 | 0.003375 | 0.004000 | 288864.31 |
| native_sql.simple_update | 0.002792 | 0.002917 | 0.009875 | 311267.87 |
| native_sql.simple_delete | 0.005833 | 0.006041 | 0.013875 | 161570.46 |
| native_sql.predicate_path | 0.039792 | 0.045708 | 0.076875 | 24092.30 |
| native_sql.mvcc_read_write | 0.007459 | 0.014042 | 0.018875 | 122300.46 |
| native_sql.vector_cache_hot_path | 0.004334 | 0.004458 | 0.004542 | 225111.14 |
| python_gateway.startup_shutdown | 0.235458 | 0.275500 | 0.519500 | 4187.90 |

Perf investigation:

| Workload | p50 ms | p95 ms | p99 ms | ops/s |
| --- | ---: | ---: | ---: | ---: |
| prepared.indexed_string_equality | 0.019041 | 0.020875 | 0.021166 | 51907.04 |
| sql.indexed_string_equality | 0.039875 | 0.044667 | 0.050083 | 24621.45 |
| checkpoint.no_dirty_tables | 0.002042 | 0.002458 | 0.002542 | 457572.22 |
| checkpoint.dirty_small_table | 8.101541 | 10.248708 | 15.099875 | 115.50 |
| checkpoint.dirty_large_table | 9.747666 | 10.676000 | 10.676000 | 102.28 |
| wal.commit_with_checkpoint_pressure | 9.864917 | 12.346958 | 13.036084 | 99.50 |

These benchmarks are not ID/UUID/JSON-specific microbenchmarks. They are regression checks that this semantic hardening pass did not break the existing performance profile.

## 14. PostgreSQL Comparison Regression Status

PostgreSQL environment:

- PostgreSQL 17.9 (Homebrew)
- `fsync=on`
- `synchronous_commit=on`
- DSN: `postgresql://qm_bench:qm_bench@localhost:5432/qm_bench`

Memory mode, not durability-equivalent:

| Workload | Winner | QM p50 ms | PG p50 ms | QM ops ratio |
| --- | --- | ---: | ---: | ---: |
| insert | QM | 0.004625 | 0.084750 | 21.002 |
| select_by_pk | QM | 0.003000 | 0.032000 | 10.915 |
| update_by_pk | QM | 0.002750 | 0.075917 | 28.028 |
| delete_by_pk | QM | 0.007500 | 0.176792 | 25.564 |
| indexed_string_equality_duplicate_heavy | QM | 0.018000 | 0.051750 | 2.821 |
| count_indexed_equality | QM | 0.003292 | 0.043125 | 12.728 |
| transaction_commit | QM | 0.004000 | 0.145750 | 36.282 |

Persistent-WAL per-mutation sync:

| Workload | Winner | QM p50 ms | PG p50 ms | QM ops ratio |
| --- | --- | ---: | ---: | ---: |
| insert | PostgreSQL | 2.991750 | 0.099333 | 0.036 |
| select_by_pk | QM | 0.002959 | 0.032625 | 11.450 |
| update_by_pk | PostgreSQL | 3.000375 | 0.095041 | 0.032 |
| delete_by_pk | PostgreSQL | 5.997458 | 0.196041 | 0.034 |
| indexed_string_equality_duplicate_heavy | QM | 0.030958 | 0.052333 | 1.498 |
| count_indexed_equality | QM | 0.004667 | 0.042917 | 8.990 |
| transaction_commit | PostgreSQL | 2.991666 | 0.142708 | 0.053 |

Interpretation:

- QM wins supported in-memory/local scalar and index read workloads.
- QM still loses durable per-mutation WAL write workloads because every timed mutation waits for `sync_wal()`/fsync.
- This pass does not claim durable-write PostgreSQL superiority.

## 15. Commands Run

Validation:

```bash
cargo fmt --manifest-path qm_engine/Cargo.toml
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test native_sql_identity_uuid_json_hash -- --nocapture
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
RUSTFLAGS='-L native=/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/config-3.13-darwin -l python3.13' PYO3_PYTHON=/Library/Frameworks/Python.framework/Versions/3.13/bin/python3 cargo test --manifest-path qm_engine/Cargo.toml
python3 -m maturin build --release --out /private/tmp/qm_maturin_wheels
python3 -m pip install --force-reinstall /private/tmp/qm_maturin_wheels/qmvir-5.4.0-cp313-cp313-macosx_11_0_arm64.whl
python3 -m pytest -q -rxX
bash scripts/check_no_space_number_duplicates.sh
```

Benchmarks:

```bash
python3 scripts/release_benchmark_native_sql.py --iterations 1000 --output docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
python3 scripts/perf_investigate_native_sql.py --iterations 1000 --output docs/native_sql_perf_investigation_last.json --build-mode release --feature-flags default
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode memory --output docs/postgres_comparison_memory_latest.json --strict
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' python3 scripts/compare_postgres_native_sql.py --iterations 1000 --qm-mode persistent-wal --output docs/postgres_comparison_persistent_wal_latest.json --strict
```

Results:

- `cargo check --no-default-features`: pass.
- `cargo check` default features: pass.
- `cargo test --no-default-features`: pass. Main lib `413 passed, 0 failed, 15 ignored`; integration targets including `native_sql_identity_uuid_json_hash` with 12 tests, `release_crash_kill_recovery` with 15 tests, and `release_crash_recovery` with 10 tests pass.
- Default/PyO3 cargo test: pass with same target set and `413 passed, 0 failed, 15 ignored` in lib.
- `maturin build`: pass, wheel built.
- `pytest`: `1136 passed, 12 skipped in 28.24s`.
- Duplicate hygiene check: pass.
- Release benchmark: pass.
- Perf investigation benchmark: pass.
- PostgreSQL memory comparison: pass.
- PostgreSQL persistent-WAL comparison: pass.

## 16. Approved Claims

- Auto ID supports monotonic per-table generated integer IDs for omitted `id INTEGER`.
- Explicit high integer IDs advance the generated counter.
- Deleted and rolled-back generated IDs are not reused.
- Failed inserts do not advance the generated counter.
- Generated counter persists and continues after reopen.
- Generated ID collision safety is tested for cloned handles of one shared engine instance.
- UUID values in UUID columns are validated, lowercase canonicalized, unique, and persisted.
- JSON and JSONB alias values are validated and canonicalized before storage.
- Tested JSON extraction/equality paths work and survive reopen.
- Tested JSON update/delete/rollback/reopen paths keep supported extraction and generic JSON text index results current.
- Python string parameters can bind UUID/JSON/JSONB through `execute_prepared` and return canonical strings.
- Unsupported JSONB containment fails clearly.
- `ANALYZE` distinct-value text hashing is stable for the stats path.
- Existing crash/recovery suites still pass.
- PostgreSQL memory-mode comparison shows QM ahead on supported local non-durable workloads.
- PostgreSQL persistent-WAL per-mutation comparison shows QM behind PostgreSQL on durable writes.

## 17. Rejected Claims

- PostgreSQL-compatible `SERIAL`, `IDENTITY`, or `AUTOINCREMENT`.
- PostgreSQL-compatible UUID type semantics.
- Cryptographically secure UUID generation.
- UUID v7/time-ordered UUID generation.
- Binary UUID storage.
- Native UUID O(1) or 16-byte binary primary-key lookup performance.
- PostgreSQL-compatible JSONB.
- JSONB containment/index/operator parity.
- JSONB missing-key vs JSON-null PostgreSQL parity.
- Python direct `uuid.UUID`, dict, or list parameter binding.
- Stable FNV hash as checksum/security hash.
- Durable-write PostgreSQL superiority.

## 18. Remaining Caveats

- Auto ID generation is narrow and keyed to integer column name `id`.
- Generated ID collision safety is not claimed for independently opened engines/processes sharing one data directory.
- UUID primary key correctness is covered, but specialized UUID index performance is not implemented or proven.
- UUID lookup currently uses generic scan or generic text index behavior; future 16-byte UUID storage remains a possible optimization.
- JSON/JSONB duplicate-key policy and numeric representation semantics are scoped to serde canonical text behavior, not PostgreSQL compatibility.
- Python direct object binding for `uuid.UUID`, dict, and list remains intentionally unimplemented in this pass and is rejected by tests.
- Persistent-WAL durable mutation path remains slower than PostgreSQL under per-mutation `sync_wal()` policy.
