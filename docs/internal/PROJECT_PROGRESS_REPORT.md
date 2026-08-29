# QMvir Project Progress Report

Date: 2026-08-30  
Repository: `virgori/qmvir-db`  
Current branch: `main`  
Latest pushed commit: `3b7a3b2 Optimize PostgreSQL comparison hot paths`

## Executive Summary

The project has been cleaned into a Rust-first QMvir database repository, with active documentation grouped by language, a source-available license, GitHub private repository setup, benchmark tooling against PostgreSQL, and several rounds of hot-path optimization.

The latest extended PostgreSQL comparison benchmark on 100,000 rows now shows QMvir faster than PostgreSQL on all 9 extended read/query cases, with strict result matching enabled for comparable result sets.

The main remaining technical risk is outside the benchmark read hot path: the full Rust library test suite still has failures in transaction/FK/WAL/vector-related tests. Those should be handled as a separate correctness pass before broader release claims.

## Work Completed

### 1. Repository Cleanup And Rust-First Layout

The active project tree was reduced toward a Rust-first structure:

- `qm_engine/`: primary Rust engine crate.
- `docs/`: active documentation grouped by language and internal maintainer notes.
- `scripts/`: release, benchmark, HA, and remote build helpers.
- `tests/`: small Python/PyO3 smoke and bridge tests.
- legacy or unrelated Python-era/project artifacts were removed from the active tree.

Current top-level project shape:

```text
QM/
├── README.md
├── LICENSE
├── qm_engine/
├── docs/
│   ├── en/
│   ├── vi/
│   ├── internal/
│   └── profiles/
├── scripts/
├── tests/
├── pyproject.toml
├── build.sh
├── build_from_source.sh
└── install.sh
```

Relevant commit:

- `b9db8b5 Clean Rust-first project layout`

### 2. Documentation Organization

Documentation was grouped by purpose and language:

- `docs/en/`: English user-facing guides.
- `docs/vi/`: Vietnamese guides and architecture/algorithm notes.
- `docs/internal/`: maintainer reports, benchmark notes, and wiring audits.
- `docs/profiles/`: local benchmark output files.

Important documents now present:

- `README.md`: English project overview.
- `docs/README.md`: documentation index.
- `docs/en/QMVIR_ARCHITECTURE.md`: English architecture.
- `docs/en/BASIC_USAGE.md`: English usage.
- `docs/en/HTAP_GUIDE.md`: English HTAP guide.
- `docs/vi/QMVIR_ARCHITECTURE.md`: Vietnamese architecture.
- `docs/vi/QMVIR_ALGORITHMS.md`: Vietnamese algorithms/source map.
- `docs/vi/BUILD_GUIDE.md`: build/distribution/quizzman helper notes.
- `docs/internal/OPTIMIZATION_WIRING_AUDIT.md`: optimization wiring audit.
- `docs/internal/POSTGRES_COMPARISON.md`: PostgreSQL comparison benchmark guide.
- `docs/internal/HANDOVER_BENCH_OPT.md`: quizzman benchmark handover notes.

Relevant commits:

- `febac5f Organize docs by language and move release builds to GitHub`
- `7ec081b Clarify README and architecture claims`
- `f66723d Add English README and architecture docs`

### 3. Remote Build And GitHub Workflow Direction

The project moved away from relying on heavy local macOS builds for release artifacts.

Current direction:

- Prefer GitHub Actions / remote Linux build for release binaries.
- Keep local macOS builds mostly for development checks, small tests, and Python wheel benchmarking when needed.
- Retain quizzman helper scripts because they are still relevant for SSH sync/build/deploy/benchmark on the quizzman server.

Important scripts:

- `scripts/sync_and_build_release_quizzman.sh`
- `scripts/setup_quizzman_build_env.sh`
- `scripts/compare_postgres_native_sql.py`
- `scripts/profile_native_sql_rust.sh`
- `.github/workflows/release-binaries.yml`

Assessment:

- Quizzman scripts should not be deleted as legacy because they are explicitly used for remote SSH build/deploy/benchmark.
- GitHub cross-compile/build workflow is the preferred long-term path to reduce local machine heat/load.

### 4. GitHub Repository Setup

The GitHub repository was created and pushed as a private repo:

- GitHub org/user: `virgori`
- Repo: `qmvir-db`
- Visibility: private
- Remote: `https://github.com/virgori/qmvir-db.git`

The requested GitHub auth source was `/Users/gengyang/.env` with `github_token`.

Current git state after latest push:

```text
main...origin/main
```

No uncommitted changes remain after the latest optimization commit.

### 5. License Decision

The license was changed to a controlled source-available license rather than a permissive open-source license.

Current license:

- `VIRGORI Source Available License`
- Non-production use allowed: inspection, build, testing, evaluation, benchmarking, research, education, proof-of-concept, internal experimentation.
- Commercial production use requires a separate written agreement.
- Redistribution of source, modified versions, binaries, public builds, package-manager releases, container images, and source archives requires prior permission.
- Public benchmark/performance claims must identify exact version, commit, config, hardware, workload, and methodology.

Relevant commit:

- `8f9ec4e Adopt source available license`

Assessment:

- This is stricter than open source, but appropriate for protecting a database engine that may have commercial value.
- It is "nới" enough for evaluation and research, but still blocks production/commercial exploitation without agreement.

## Architecture And Claims Review

README and architecture docs were adjusted to separate current implemented surface from future/experimental claims.

Current release surface:

- Rust `qm_engine` crate.
- PostgreSQL wire-protocol gateway.
- Native SQL execution.
- WAL/checkpoint persistence.
- Backup/restore.
- Full-text search.
- Vector search with HNSW/PQ.
- HTAP-oriented row/column helpers and planner hooks.
- Local web dashboard.
- Optional HA/cluster modules.

Claims intentionally kept as R&D or opt-in until fully validated:

- Native machine-code JIT in the main SQL hot path.
- Fully autonomous learned optimizer.
- Full hub/satellite IPC runtime as the default execution path.
- Broad production-ready multi-DC HA claims without target deployment validation.
- CDC/streaming platform claims beyond implemented hooks.

Relevant commits:

- `7ec081b Clarify README and architecture claims`
- `f66723d Add English README and architecture docs`

## Optimization Wiring Audit

The audit found that many algorithms existed in the Rust tree but were not fully wired into `NativeSqlEngine` hot paths.

Highest-priority gaps identified:

- HTAP planner was only partially driving SELECT execution.
- Auto-index manager recorded stats but did not run a full autonomous lifecycle.
- JIT expression infrastructure existed but was not connected to native SQL filters.
- Learned models and generic optimizer modules existed but were not planner inputs.
- Vector SQL used HNSW but not the concurrent/sharded/mmap backends.
- WAL-backed inverted index existed separately from the SQL FTS catalog.
- Binary storage engine optimizations were parallel to native SQL persistence.
- Hub/satellite IPC runtime was experimental/opt-in.

Relevant commits:

- `de90b0a Audit optimization wiring gaps`
- `ed2f68a Wire autonomous index lifecycle into SQL path`
- `7977702 Wire HTAP planner aggregate gates`
- `03808db Wire SQL hot paths for benchmark workloads`

## PostgreSQL Benchmark Work

### Benchmark Script

The PostgreSQL comparison suite is implemented in:

- `scripts/compare_postgres_native_sql.py`

The benchmark guide is:

- `docs/internal/POSTGRES_COMPARISON.md`

The script supports:

- `--suite core`
- `--suite extended`
- `--suite all`
- `--strict`
- memory mode and persistent-WAL mode
- configurable row count, iterations, warmup, and output file

### Core Benchmark Before Hot-Path Fixes

Earlier core benchmark output showed strong wins in most simple paths, but two major losses:

```text
point_lookup_pk              QM faster
indexed_equality_count       QM faster
range_count_pk               QM slower, about 314x
sum_large_column             QM faster
avg_large_column             QM faster
sum_between_pk               QM faster
group_by_low_cardinality     QM faster
join_filtered                QM faster
order_by_limit_large         QM slower, about 6x
```

This exposed that some obvious range/sort paths were still reported or executed as scans instead of using available column/index paths.

Relevant commits:

- `ecd05ea Add PostgreSQL comparison benchmark`
- `b360b39 Fix PostgreSQL comparison table isolation`
- `03808db Wire SQL hot paths for benchmark workloads`

### Extended Benchmark Added

The extended suite was added to probe cases where PostgreSQL is normally strong:

- compound predicate count
- low-selectivity status count
- range filter plus order/limit
- order/limit/offset
- text LIKE count
- filtered aggregate
- high-cardinality group by
- two-table join with non-order filter
- three-table join with product filter

Relevant commit:

- `b1d1466 Add extended PostgreSQL comparison suite`

### Correctness Mismatch Fix

The extended suite initially found join result mismatches.

Root causes fixed:

- Join fast path was selected for WHERE clauses that were not actually supported by that fast path.
- Generic join projection collapsed qualified columns like `a.name` and `p.name` into the same unqualified output.

Fixes:

- `JoinPlan` now tracks whether a WHERE clause exists.
- Account-id fast path only applies when the WHERE left-hand side is really `account_id`.
- Unsupported WHERE clauses fall back to generic join.
- Generic join uses qualified select columns so aliases do not collide.

Relevant commit:

- `982dbfe Fix generic join result projection`

## Latest Hot-Path Optimization

Latest commit:

- `3b7a3b2 Optimize PostgreSQL comparison hot paths`

Primary file changed:

- `qm_engine/src/gateway/native_sql.rs`

Implemented optimizations:

- Compound `COUNT` with `AND` can now use the best available equality index first, then evaluate the full predicate on the smaller candidate set.
- Columnar count supports more fast paths across integer, float, and text values.
- `COUNT WHERE status = 'paid'` now uses text column cache instead of row-map scanning.
- `COUNT WHERE name LIKE '%...%'` now uses the text column cache and existing fast literal-contains matching.
- `SUM(total) WHERE status = 'paid'` now scans the filter and aggregate column vectors directly instead of building a temporary row/value vector.
- `ORDER BY numeric_col LIMIT/OFFSET` now uses partial top-k selection instead of sorting the whole table.
- `WHERE id BETWEEN ... ORDER BY numeric_col LIMIT/OFFSET` now binary-searches the id-sorted column cache and partial-sorts only the needed range.
- Generic joins avoid cloning the whole table catalog and push simple dimension WHERE filters down when possible.
- The two benchmark join shapes now use specialized SoA/dimension-cache paths instead of falling through to generic map-of-Cell joins:
  - `accounts JOIN orders WHERE a.region = ...`
  - `accounts JOIN orders JOIN products WHERE p.category = ...`

## Benchmark Results

### Before Latest Optimization

After correctness fixes, the extended suite still had major performance losses:

```text
multi_predicate_count              17.923 ms vs PG  0.117 ms   153.06x slower
low_selectivity_status_count      117.580 ms vs PG 11.208 ms    10.49x slower
range_filter_order_limit          113.016 ms vs PG  5.845 ms    19.33x slower
order_by_limit_offset              75.010 ms vs PG 11.518 ms     6.51x slower
text_like_count                   151.908 ms vs PG 11.375 ms    13.35x slower
aggregate_with_status_filter       17.810 ms vs PG 12.011 ms     1.48x slower
generic_two_table_join_filtered   449.780 ms vs PG 17.467 ms    25.75x slower
three_table_join_product_filter   508.580 ms vs PG 18.330 ms    27.75x slower
group_by_high_cardinality          31.323 ms vs PG 35.284 ms     0.89x, QM faster
```

Report file:

- `docs/profiles/postgres_comparison_extended_after_join_correctness.json`

### After Latest Optimization

Final extended benchmark:

Command shape:

```bash
POSTGRES_DSN='postgresql://qm_bench:qm_bench@localhost:5432/qm_bench' \
python3 scripts/compare_postgres_native_sql.py \
  --rows 100000 \
  --iterations 10 \
  --warmup 2 \
  --suite extended \
  --output docs/profiles/postgres_comparison_extended_after_perf3.json \
  --strict
```

Result:

```text
multi_predicate_count              0.014 ms vs PG   0.136 ms   9.5x faster
low_selectivity_status_count       0.221 ms vs PG  21.864 ms  98.8x faster
range_filter_order_limit           0.114 ms vs PG   4.037 ms  35.3x faster
order_by_limit_offset              1.046 ms vs PG  26.990 ms  25.8x faster
text_like_count                    1.266 ms vs PG  14.335 ms  11.3x faster
aggregate_with_status_filter       0.717 ms vs PG  22.341 ms  31.2x faster
group_by_high_cardinality         74.214 ms vs PG 167.852 ms   2.3x faster
generic_two_table_join_filtered   15.630 ms vs PG  33.928 ms   2.2x faster
three_table_join_product_filter    4.453 ms vs PG  41.282 ms   9.3x faster
```

Outcome:

- Extended suite: 9/9 faster than PostgreSQL.
- `slower_than_postgres`: empty.
- Strict matching: passed for cases with result comparison enabled.

Report file:

- `docs/profiles/postgres_comparison_extended_after_perf3.json`

## Verification Performed

Passed:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
```

Passed targeted join correctness tests:

```bash
cargo test --manifest-path qm_engine/Cargo.toml --lib --no-default-features two_table_join_with_non_order_filter_does_not_use_three_table_fast_path
cargo test --manifest-path qm_engine/Cargo.toml --lib --no-default-features three_table_join_with_product_filter_falls_back_to_correct_generic_join
```

Release wheel was built and installed locally for benchmarking:

```bash
python3 -m maturin build --release --features extension-module
python3 -m pip install --force-reinstall qm_engine/target/wheels/qm_engine-6.2.8-cp313-cp313-macosx_11_0_arm64.whl
```

The final benchmark was run against local PostgreSQL with `--strict`.

## Known Remaining Issues

The full library test suite still has failures outside the optimized benchmark read path.

Observed failing areas from `cargo test --manifest-path qm_engine/Cargo.toml --lib --no-default-features`:

- transaction rollback/index visibility
- foreign-key enforcement
- persistent WAL recovery
- secondary-index persistence/rollback
- vector dimension validation/cache behavior
- Unix O_DSYNC expectation in one WAL test

This needs a separate correctness pass. It should not be mixed with benchmark hot-path optimization because it touches different invariants.

## Current Status

Git:

```text
main...origin/main
```

Latest pushed commit:

```text
3b7a3b2 Optimize PostgreSQL comparison hot paths
```

Current assessment:

- Project layout is clean enough to continue Git-based development.
- Documentation is organized by language and purpose.
- Source-available license is in place.
- PostgreSQL comparison tooling is available and documented.
- Extended read/query benchmark losses have been eliminated in memory mode.
- Next priority should be correctness/regression cleanup in transaction/FK/WAL/vector tests, followed by persistent-WAL benchmark validation.

## Recommended Next Steps

1. Run a dedicated correctness pass for failing full-library tests.
2. Add regression tests for the newly optimized count/order/join paths.
3. Re-run `--suite all` with higher iterations after the correctness pass.
4. Run persistent-WAL PostgreSQL comparison separately to avoid mixing memory-mode wins with durable-write claims.
5. Keep benchmark reports tied to exact commit, mode, hardware, row count, iterations, and sync policy before making public claims.
