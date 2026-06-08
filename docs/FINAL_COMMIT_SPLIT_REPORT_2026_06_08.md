# QMvir Final Commit Split Report - 2026-06-08

## Summary

- Slices prepared: 3
- Files staged: 8
- Files intentionally left unstaged: remaining large source surface, owner-review/WIP paths, historical reports, generated/local artifacts, binary/static payloads.
- Tests run:
  - `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test native_sql_query_update_delete_audit -- --nocapture`
  - `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features native_sql -- --nocapture`
  - `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features`
  - `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features`
  - `python3 -m pytest -q -rxX`
- Final gate: pass

Prepared staged files:

```text
README.md
USAGE_GUIDE_EN.md
USAGE_GUIDE_VI.md
docs/INTEGRATED_STABILIZATION_REPORT_2026_06_08.md
docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md
docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md
qm_engine/src/gateway/native_sql.rs
qm_engine/tests/native_sql_query_update_delete_audit.rs
```

Important staging note: `qm_engine/src/gateway/native_sql.rs`, `qm_engine/tests/native_sql_query_update_delete_audit.rs`, `USAGE_GUIDE_EN.md`, and `USAGE_GUIDE_VI.md` are staged as added files because the prior stabilization pass intentionally unstaged the previous large staged source layer.

## Slice Readiness

| Slice | Files | Tests | Ready? | Suggested commit |
|---|---|---|---|---|
| Native SQL malformed input safety | `qm_engine/src/gateway/native_sql.rs`; `qm_engine/tests/native_sql_query_update_delete_audit.rs` | `native_sql_query_update_delete_audit`: 3 passed; `native_sql`: 127 passed, 15 ignored | Yes | `Harden Native SQL malformed DDL/DML handling` |
| Documentation claim cleanup | `README.md`; `USAGE_GUIDE_EN.md`; `USAGE_GUIDE_VI.md`; `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md`; `docs/QM_VS_POSTGRES_READINESS_REPORT_VI.md` | claim scan reviewed; full Rust/Python gates pass | Yes | `Clarify PostgreSQL durability and HA release claims` |
| Stabilization report | `docs/INTEGRATED_STABILIZATION_REPORT_2026_06_08.md` | full Rust/Python gates pass | Yes | `Add integrated stabilization report` |

## Final Gate Results

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | pass: 444 passed, 15 ignored; integration suites pass; doctests 0 passed, 2 ignored |
| `python3 -m pytest -q -rxX` | pass: 1139 passed, 12 skipped |
| tracked artifact scan | pass: no tracked `.env`, `.DS_Store`, `dist/`, `target/`, `node_modules/`, cache dirs, `.a`, or generated report JSON matches |

## Claim Scan

The Slice 2 claim scan still finds conservative/negative wording such as:

- not a general PostgreSQL replacement;
- not a production HA database;
- group commit is not PostgreSQL `synchronous_commit=on` equivalent when acknowledged before fsync;
- sharding/replication are experimental and not network-partition safety claims.

These are allowed release-safety statements, not unsafe claims.

## Remaining Worktree Inventory

| Bucket | Files | Risk | Recommendation |
|---|---|---|---|
| Rust foundation | `qm_engine/Cargo.toml`, `qm_engine/Cargo.lock`, `qm_engine/src/lib.rs`, `qm_engine/src/types.rs`, `qm_engine/src/metrics.rs`, parser/optimizer/executor/statistics/procedures modules | Large newly added source surface; broad behavior and dependency risk | Owner-review and split by subsystem with focused tests |
| Rust Native SQL broader source | `qm_engine/src/gateway/*.rs` except staged `native_sql.rs`; `native_sql_v2_wip.rs` | Gateway behavior, auth/protocol/server surface, WIP Native SQL v2 risk | Review separately; do not include `native_sql_v2_wip.rs` in release commit without explicit approval |
| Rust index/search | `qm_engine/src/index/`, `qm_engine/src/search/`, HNSW/BM25/sharded index tests | Correctness/performance-sensitive; some bench-style tests are long | Stage as separate index/search slice after targeted tests |
| Rust WAL/storage/MVCC | `qm_engine/src/storage/`, `qm_engine/src/mvcc/`, release crash tests | Durability and recovery risk | Stage only after WAL/checkpoint/recovery tests and durability claim review |
| Python platform | `qm_core/`, `core_db/`, `analytics_platform/`, `cache_layer/`, `storage/`, `indexing/`, `pipelines/`, `observability/`, `search_platform/`, `vector_platform/` | Large API surface; includes legacy/duplicate package layout | Split into package-level commits with Python tests |
| tests | `tests/`, remaining `qm_engine/tests/` | Test additions are useful but include `_probe5.py` and broad historical tests | Stage after classifying smoke/regression vs probe/temporary files |
| npm/sdk | `npm/`, `sdk/` | Contains npm binary payloads and local env risk | Review carefully; do not add `npm/.env` or `npm/qm-*` binaries |
| studio | `qmvir-studio/` | Frontend build previously had `tsc` missing; includes `dist/` on disk | Owner-review; do not stage `dist/` |
| docs | many `docs/*.md`, version reports, historical reports | Stale claims and benchmark-number context risk | Stage only reviewed current-release docs; archive stale reports separately |
| WIP/owner-review | `.cargo/`, `_py_legacy/`, `gateway/`, `release_snapshots/`, `docs/profiles/`, `MUST_READ_CONTEXT/`, completion reports | Not release-ready by default | Leave unstaged unless owner explicitly approves |
| generated/artifact | local `.DS_Store`, `.pytest_cache`, `__pycache__`, `wal/`, `dist/`, `build/`, `tmp/`, `lib/*.a`, `npm/qm-*`, benchmark JSON outputs | Must not be committed | Keep unstaged/ignored; cleanup only under a separate hygiene task |

## Owner-Review Required

Do not commit these blindly:

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
npm/.env
npm/qm-*
qmvir-studio/dist/
lib/libqm_*.a
```

## Final Recommendation

`READY_TO_COMMIT_PREPARED_SLICES`

The three prepared slices are staged and test-clean. The remaining worktree still needs owner-review and artifact hygiene, but those files are not part of the prepared staged commit set.
