# QMvir Release Readiness Final Gate - 2026-06-08

## Verdict

`READY_FOR_TECHNICAL_PREVIEW_RELEASE`

## Scope

This gate consolidated completed release-hardening work into clean commits and
verified the result from a clean checkout. No engine optimization or benchmark
semantic changes were made in this gate.

## Commit Slices

| Commit | Slice | Notes |
|---|---|---|
| `81e4d0f` | release hygiene | Adds `.gitignore` coverage for local/generated artifacts. |
| `166be89` | artifact cleanup | Removes tracked benchmark JSON, snapshot, and WAL artifacts from the release index. |
| `2ff455e` | Native SQL / MVCC / hybrid / vector source | Consolidates Rust crate source, Native SQL safety hardening, bridge implementation, and Rust tests. |
| `00d246e` | Python/Rust bridge and Python tests | Consolidates Python source and Python test coverage, including bridge zero-copy tests. |
| `dc6617e` | benchmark/audit scripts | Adds bridge, vector, PostgreSQL comparison, profiling, and release audit scripts. |
| `84c9951` | docs claim cleanup and reports | Adds claim-boundary docs, hardening reports, readiness reports, and usage guides. |
| `77ca750` | release support surface | Adds release metadata, workflows, Docker/package surfaces, SDK/studio source, and support docs. |

## Clean Checkout

Clean checkout path:

`/private/tmp/qmvir_release_clean_gate_2026_06_08`

Clean checkout commit tested:

`77ca750 chore: add release support surface`

Pre-gate checks:

| Check | Result |
|---|---|
| `git status --short` | clean |
| tracked artifact grep | clean |
| `npm/.env` presence check | absent |

Tracked artifact grep pattern covered `.DS_Store`, `.env`, `dist/`, `target/`,
`node_modules/`, `__pycache__`, `.pytest_cache`, `.a`, WAL/snapshot files,
benchmark JSON, doc JSON artifacts, npm binary packages, native npm binaries,
and `.tgz` packages.

## Required Gate Results

| Command | Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | pass |
| `python3 -m pytest -q -rxX` | pass: `1163 passed, 12 skipped` |
| `python3 scripts/bridge_materialization_audit.py --rows 10000 --cols 8` | pass, wrote `/tmp/qmvir_bridge_materialization_audit.json` |
| `python3 scripts/vector_search_audit_benchmark.py --quick --medium-vector-smoke` | pass, wrote `/tmp/qmvir_vector_search_medium_smoke.json` |

Rust full gate summary:

- crate lib tests: `451 passed; 15 ignored`
- `bench_engine`: `15 ignored`
- `bench_new_components`: `35 passed`
- `mvcc_integration`: `26 passed`
- `native_sql_identity_uuid_json_hash`: `19 passed`
- `native_sql_query_update_delete_audit`: `3 passed`
- `release_crash_kill_recovery`: `18 passed`
- `release_crash_recovery`: `10 passed`
- doc tests: `2 ignored`

## Artifact Policy

The following were deliberately excluded from commits:

- `.env` and `.env.*`, except `.env.example`
- `.DS_Store`
- WAL and snapshot runtime files
- Rust/Node build output: `target/`, `dist/`, `node_modules/`
- static libraries and native binary package payloads
- generated benchmark JSON and generated audit JSON
- local probe file `tests/_probe5.py`
- duplicated local archive `_py_legacy/`

Generated gate outputs were written outside the repository under `/tmp`.

## Claim Risk Review

Release claims are bounded as follows:

- Native SQL hardening is covered by focused Rust and Python tests.
- MVCC/hybrid/vector evidence is documented as tested evidence, not as a
  blanket correctness proof beyond the covered gates.
- Python/Rust bridge zero-copy is claimed only for selected direct columnar
  paths and compact output buffers; unsupported SQL paths explicitly fallback.
- Benchmark/audit scripts are treated as reproducibility tooling, not as
  committed benchmark-number semantics.

No release-blocking claim risk remains for a technical preview release.

## Residual Local State

The source workspace still has two untracked local-only paths that were not
included in release commits:

- `_py_legacy/`
- `tests/_probe5.py`

They are absent from the clean checkout used for the gate above.

## Final Verdict

`READY_FOR_TECHNICAL_PREVIEW_RELEASE`
