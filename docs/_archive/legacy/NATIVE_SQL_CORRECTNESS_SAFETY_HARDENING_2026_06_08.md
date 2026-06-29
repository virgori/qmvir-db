# QMvir Native SQL Correctness/Safety Hardening Report - 2026-06-08

## Summary

- Target: Native SQL correctness/safety hardening
- Files changed: this report only
- Tests added: none
- Unsafe sites fixed: none
- Unsafe sites remaining: yes, audit-only findings listed below
- Net verdict: **ISOLATION_FAILED_NO_CHANGES**

This pass did not modify Native SQL source. The required worktree safety check showed that `qm_engine/src/gateway/native_sql.rs` has both staged and unstaged changes, with very large diffs in each layer. Editing it now would mix a new safety pass into existing staged/unstaged work and make review or rollback unsafe.

## Worktree Isolation

Required safety check results:

| Check | Result |
|---|---|
| `git status --short` | Very broad dirty worktree |
| `git diff --stat` | 114 files changed, `26310 insertions`, `10913 deletions` |
| `git diff --cached --stat` | 418 files changed, `124694 insertions` |
| `git diff --name-status -- qm_engine/src/gateway/native_sql.rs qm_engine/tests tests` | `M qm_engine/src/gateway/native_sql.rs` plus modified test files |
| `git diff --cached --name-status -- qm_engine/src/gateway/native_sql.rs qm_engine/tests tests` | `A qm_engine/src/gateway/native_sql.rs` plus many staged tests |

`native_sql.rs` isolation detail:

| Layer | Status | Stat |
|---|---|---|
| staged | `A qm_engine/src/gateway/native_sql.rs` | `10910 insertions` |
| unstaged | `M qm_engine/src/gateway/native_sql.rs` | `17334 insertions`, `7870 deletions` |

Diff snapshots were saved before stopping:

- `/private/tmp/qm_before_native_sql_safety_unstaged.diff` (`48822` lines)
- `/private/tmp/qm_before_native_sql_safety_staged.diff` (`127170` lines)

Because isolation failed, no source edits, tests, or selective staging were attempted.

## Baseline

Phase 1 baseline was not run in this pass. The prompt allows stopping and reporting if isolation is impossible, and the safety check found that the primary target file has both staged and unstaged changes.

Recent prior baseline from the immediately preceding hardening pass remains useful context but is not treated as a fresh verification for this pass:

| Command | Prior Result |
|---|---|
| `cargo check --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed |
| `cargo test --manifest-path qm_engine/Cargo.toml --no-default-features` | Passed |
| `python3 -m pytest -q -rxX` | `1139 passed, 12 skipped` |
| Targeted Native SQL/Python suite | `188 passed` |

## Unsafe Site Audit

Audit commands run:

```bash
rg -n "panic!|unwrap\(|expect\(|unreachable!|todo!|unsafe" qm_engine/src/gateway/native_sql.rs qm_engine/src/gateway qm_engine/src/storage qm_engine/src/index
rg -n "Result<|Err\(|return Err|NativeSql|SqlError|error|invalid|parse|rollback|commit|delete|update|identity|uuid|json|index" qm_engine/src/gateway/native_sql.rs
```

Representative findings:

| File | Line/Pattern | Classification | Action |
|---|---|---|---|
| `qm_engine/src/gateway/native_sql.rs` | `eval_expr` function parsing helpers, e.g. `expr.find('(').unwrap()` | USER_INPUT_REACHABLE | Not changed due isolation failure |
| `qm_engine/src/gateway/native_sql.rs` | `GREATEST` / `LEAST` use `best.unwrap()` | PROVEN_SAFE_INVARIANT likely, but still worth cleanup | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | `then_idx.unwrap()` after explicit `is_none` check | PROVEN_SAFE_INVARIANT | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | SIMD fast paths and unsafe SIMD fns | PROVEN_SAFE_INVARIANT if guards are correct | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | debug-only `panic!("NativeSqlEngine internal validation failed after reload...")` | DEBUG_ASSERTION / CORRUPTION_REACHABLE in debug builds | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | `ALTER TABLE` keyword offsets via `up.find(...).unwrap()` | USER_INPUT_REACHABLE but guarded by `contains` | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | `DROP TABLE IF EXISTS` `up.find("IF EXISTS").unwrap()` | USER_INPUT_REACHABLE but guarded by `contains` | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | `DELETE ... WHERE` `up_work.find("WHERE").unwrap()` | USER_INPUT_REACHABLE but guarded by `has_where` | Not changed |
| `qm_engine/src/gateway/native_sql.rs` | line range after test module starts around `mod tests` | TEST_ONLY | Not changed |
| `qm_engine/src/gateway/native_sql_v2_wip.rs` | `unwrap` / SIMD `unsafe` | OUT_OF_SCOPE / WIP file | Not changed |
| `qm_engine/src/gateway/mod.rs` | Tokio runtime `.expect(...)` | IO_REACHABLE / runtime setup | Not changed |
| `qm_engine/src/storage/uring_wal.rs` | recovery parsing `try_into().unwrap()` and unsafe I/O | CORRUPTION_REACHABLE / LOW_LEVEL_IO | Not changed |
| `qm_engine/src/index/hnsw.rs` | SIMD unsafe and entry-point unwraps | MIXED: PROVEN_SAFE_INVARIANT plus potential NEEDS_REVIEW | Not changed |

The highest-value next Native SQL safety target is replacing user-input-reachable `unwrap()` in expression and statement parsing with explicit `Err(...)` paths, backed by focused malformed-SQL tests.

## Tests Added

No tests were added because no implementation changes were made.

Recommended first tests for the next isolated pass:

| Test | Scenario | Expected result |
|---|---|---|
| malformed function call | `SELECT LENGTH` or malformed `LENGTH(` shape reaches expression evaluator | Error, not panic |
| malformed `CASE` | `CASE WHEN ...` without valid `THEN` result | Error or safe fallback, not panic |
| malformed `ALTER TABLE` | partial `ADD COLUMN`, `DROP COLUMN`, `ALTER COLUMN`, `RENAME COLUMN` syntax | Typed error |
| malformed `DELETE WHERE` | `DELETE FROM t WHERE` | Typed error, no mutation |
| debug reload validation | corrupt or inconsistent reload state in debug | Clear validation error path where possible |

## Fixes Made

| File | Change | Reason | Risk |
|---|---|---|---|
| `docs/NATIVE_SQL_CORRECTNESS_SAFETY_HARDENING_2026_06_08.md` | Added report | Record isolation failure and audit findings | Low |

No source fixes were made.

## Verification

No verification gate was run after changes because there were no source/test changes. The only new repo file is this report.

| Command | Result |
|---|---|
| `git status --short` | Confirmed broad dirty worktree |
| `git diff --stat` | Confirmed broad unstaged diff |
| `git diff --cached --stat` | Confirmed broad staged diff |
| `git diff --name-status -- qm_engine/src/gateway/native_sql.rs qm_engine/tests tests` | Confirmed unstaged Native SQL/test modifications |
| `git diff --cached --name-status -- qm_engine/src/gateway/native_sql.rs qm_engine/tests tests` | Confirmed staged Native SQL/test additions |
| `rg ... panic/unwrap/expect/...` | Audit findings collected |

## Performance

**PERFORMANCE_NOT_MEASURED**

No implementation changed, and no benchmark script was run in this pass.

## Durability Semantics

**NOT_TOUCHED**

No WAL, checkpoint, transaction, fsync, or storage semantics were changed.

## Distributed/HA Claim Safety

```text
No production HA claim.
Distributed claim remains limited to basic deterministic shard routing and experimental modules.
```

No distributed/HA files were touched.

## Remaining Risk

Remaining risks are unchanged from the start of this pass:

- `native_sql.rs` still contains user-input-reachable or likely user-input-reachable `unwrap()` sites in expression and statement parsing.
- `native_sql.rs` contains many test-only `unwrap()` sites after the test module begins; those are not release-path risk but add scan noise.
- Gateway runtime setup and lower-level storage/index files contain `expect`, `unwrap`, and `unsafe` sites that need separate ownership review.
- Because `native_sql.rs` has both staged and unstaged changes, line numbers and exact classifications may shift after the existing work is split or committed.

## Final Verdict

**ISOLATION_FAILED_NO_CHANGES**

Next action: split or commit the current Native SQL staged/unstaged work first. Then rerun this pass against a clean or narrowly dirty worktree and replace one small group of parser `unwrap()` sites with typed errors plus focused malformed-SQL tests.
