# QMvir Performance + Correctness/Safety Hardening Report - 2026-06-08

## Summary

- Primary target: **NOT_SELECTED**
- Secondary target: **NOT_SELECTED**
- Files changed: this report only
- Tests added: none
- Tests run: Rust full gate, Python full gate, targeted Rust/Python suites
- Benchmark result: baseline captured to `/private/tmp`; no benchmark numbers were edited
- Net verdict: **INCONCLUSIVE_NEEDS_RERUN**

No engine optimization was applied in this pass. The worktree is already heavily dirty across the exact hot files that would be touched for WAL, Native SQL, index, vector, bridge, and docs claim cleanup. Applying source edits now would mix unrelated staged/unstaged work with a performance hardening change and make review/rollback unsafe.

## Baseline

| Area | Result |
|---|---|
| Worktree | Dirty; staged source surface is very broad |
| `git diff --stat` | 114 files changed, `26310 insertions`, `10913 deletions` |
| `git diff --cached --stat` | 418 files changed, `124694 insertions` |
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed |
| `python3 -m pytest -q -rxX` | `1139 passed, 12 skipped` |
| Targeted Python suite | `188 passed` |
| Vector audit benchmark | Started, then stopped after long-running `hnsw_rust_benchmark --iterations 1`; no output JSON produced |

## Changes Made

| File | Change | Reason | Risk |
|---|---|---|---|
| `docs/PERFORMANCE_CORRECTNESS_SAFETY_HARDENING_REPORT_2026_06_08.md` | Added this report | Capture baseline, benchmark status, and safe next pass boundary | Low |

No Rust, Python, benchmark script, benchmark JSON, or release-claim document was modified.

## Performance Before/After

No before/after optimization delta exists because no source optimization was applied.

Baseline files written outside the repo:

- `/private/tmp/qm_hardening_compare_memory.json`
- `/private/tmp/qm_hardening_compare_persistent_per_commit.json`
- `/private/tmp/qm_hardening_compare_persistent_group_commit.json`
- `/private/tmp/qm_hardening_release_native_sql.json`

`/private/tmp/qm_hardening_vector_audit.json` was not produced because the vector audit benchmark was stopped.

### Native SQL Quick Baseline

| Workload | Mode | p50 ms | p95 ms | ops/s | Notes |
|---|---|---:|---:|---:|---|
| `native_sql.simple_insert` | quick/memory | 0.004167 | 0.005958 | 219318 | Script baseline only |
| `native_sql.simple_select` | quick/memory | 0.003500 | 0.003750 | 266341 | Script baseline only |
| `native_sql.simple_update` | quick/memory | 0.003042 | 0.003208 | 307298 | Script baseline only |
| `native_sql.simple_delete` | quick/memory | 0.006458 | 0.012291 | 139519 | Script baseline only |
| `native_sql.predicate_path` | quick/memory | 0.007667 | 0.009750 | 120974 | Script baseline only |
| `native_sql.mvcc_read_write` | quick/memory | 0.007792 | 0.013666 | 108303 | Script baseline only |
| `native_sql.vector_cache_hot_path` | quick/memory | 0.004584 | 0.005750 | 199800 | Script baseline only |

### Compare Script Baseline

PostgreSQL was not available through the default socket/role:

```text
connection to server on socket "/tmp/.s.PGSQL.5432" failed: FATAL: role "postgres" does not exist
```

The numbers below are local QM-side measurements only. They are not PostgreSQL comparison claims.

| Workload | Mode | sync policy | before fsync? | p50 ms | p95 ms | ops/s |
|---|---|---|---:|---:|---:|---:|
| `insert` | memory | none | n/a | 0.005500 | 0.007167 | 172018 |
| `select_by_pk` | memory | none | n/a | 0.003417 | 0.003541 | 258816 |
| `update_by_pk` | memory | none | n/a | 0.003208 | 0.003292 | 307220 |
| `delete_by_pk` | memory | none | n/a | 0.008625 | 0.009917 | 96177 |
| `transaction_commit` | memory | none | n/a | 0.004250 | 0.006625 | 222099 |
| `insert` | persistent-wal | `per_commit_sync` | no | 3.037709 | 6.006375 | 251 |
| `select_by_pk` | persistent-wal | `per_commit_sync` | no | 0.003416 | 0.003542 | 284968 |
| `update_by_pk` | persistent-wal | `per_commit_sync` | no | 2.998292 | 6.011709 | 303 |
| `delete_by_pk` | persistent-wal | `per_commit_sync` | no | 6.001625 | 9.005875 | 156 |
| `transaction_commit` | persistent-wal | `per_commit_sync` | no | 3.041375 | 5.957875 | 293 |
| `insert` | persistent-wal | `group_commit` | yes | 0.012250 | 2.501542 | 3352 |
| `select_by_pk` | persistent-wal | `group_commit` | yes | 0.003458 | 0.003583 | 283957 |
| `update_by_pk` | persistent-wal | `group_commit` | yes | 0.008750 | 2.427333 | 3724 |
| `delete_by_pk` | persistent-wal | `group_commit` | yes | 0.024625 | 2.878375 | 1642 |
| `transaction_commit` | persistent-wal | `group_commit` | yes | 0.011459 | 3.896875 | 1910 |

`group_commit` is faster on several mutating workloads, but it acknowledged before fsync in this run. It is not durability-equivalent to PostgreSQL `synchronous_commit=on`.

## Correctness/Safety Verification

| Invariant | Verification | Result |
|---|---|---|
| Rust engine compiles without default features | `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed |
| Rust full test gate | `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed |
| Python full test gate | `python3 -m pytest -q -rxX` | `1139 passed, 12 skipped` |
| Native SQL targeted tests | `cargo test ... native_sql -- --nocapture` | `127 passed, 15 ignored` in lib tests; related integration matches also passed |
| WAL targeted tests | `cargo test ... wal -- --nocapture` | `15 passed`; plus WAL-related integration matches passed |
| Checkpoint targeted tests | `cargo test ... checkpoint -- --nocapture` | `6 passed`; plus checkpoint crash matches passed |
| MVCC targeted tests | `cargo test ... mvcc -- --nocapture` | `31 passed, 1 ignored`; plus one restart match passed |
| Index targeted tests | `cargo test ... index -- --nocapture` | `108 passed`; related integration and benchmark-style matches passed |
| HNSW targeted tests | `cargo test ... hnsw -- --nocapture` | `37 passed`; plus 6 benchmark-style matches passed |
| BM25 targeted tests | `cargo test ... bm25 -- --nocapture` | `8 passed`; plus `bench_inverted_bm25_ranking` passed |
| Python vector/BM25/core/native SQL bridge/full engine targeted suite | `python3 -m pytest ... -q -rxX` | `188 passed` |

The prompt's combined Rust targeted command was attempted exactly, but `cargo test` rejected multiple positional filters:

```text
error: unexpected argument 'release_crash' found
```

The filters were then run separately.

## Durability Semantics

Allowed value: **NOT_TOUCHED**

No WAL, commit, checkpoint, fsync, group commit, or benchmark script semantics were changed.

## Benchmark Claim Safety

Safe claims from this pass:

- Local baseline scripts ran for memory mode and persistent-WAL modes.
- `per_commit_sync` reported `sync_before_commit_return=true` and `acknowledged_before_fsync=false`.
- `group_commit` reported `sync_before_commit_return=false` and `acknowledged_before_fsync=true`.

Unsafe claims:

- Do not claim a fresh PostgreSQL comparison from this pass; PostgreSQL was unavailable through the default role/socket.
- Do not compare `group_commit` as synchronous durability-equivalent.
- Do not use the killed vector benchmark as evidence of vector speed or recall.
- Do not claim performance improved; no optimization was applied.

## Distributed/HA Claim Safety

Distributed/HA claim remains limited to:

```text
basic deterministic shard routing and experimental modules.
No production HA claim.
```

No distributed, transport, consensus, failover, or 2PC behavior was changed or proven in this pass.

## Safety Review Findings

The final safety `rg` checks found two release risks:

1. `qm_engine/src/gateway/native_sql.rs` and other Rust files still contain production-path `unwrap`, `expect`, `panic!`, and `unsafe` sites. Many are tests or guarded SIMD/mmap code, but this pass did not audit them individually because no source target was selected.
2. Older docs still contain broad PostgreSQL/distributed/performance claims, for example `README.md`, `docs/ARCHITECTURE.md`, `docs/QM_VS_POSTGRES_READINESS_REPORT_EN.md`, and `USAGE_GUIDE_EN.md`. These conflict with the current conservative release boundary and need a dedicated claim-cleanup pass.

## Remaining Risks

- Dirty worktree makes optimization ownership unclear.
- Durable write path remains dominated by fsync cost in `per_commit_sync`.
- Vector audit benchmark did not complete in this run.
- PostgreSQL comparison requires a valid local benchmark DSN/role.
- Documentation claim surface still contains older broad claims.

## Recommended Next Pass

1. Commit or shelve the current staged source slices so a hardening change can be reviewed independently.
2. Pick **one** target: durable WAL commit latency or Native SQL panic/unwrap hardening.
3. Run the same baseline with a valid `POSTGRES_DSN`.
4. Use `/private/tmp` outputs during iteration, then copy only intentional final benchmark artifacts.
5. Run a separate documentation claim cleanup for PostgreSQL and distributed/HA wording.

## Final Verdict

**INCONCLUSIVE_NEEDS_RERUN**

The test baseline is healthy, but this pass intentionally avoided source optimization because the repository state is too broad and dirty to make a small, reviewable performance/correctness change safely.
