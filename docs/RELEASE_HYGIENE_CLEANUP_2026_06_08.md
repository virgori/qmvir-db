# Release Hygiene Cleanup - 2026-06-08

## Summary

This pass cleaned release-hygiene issues only. It did not optimize engine logic, change benchmark semantics, rewrite subsystems, or commit anything.

Result:

- `git status --short` went from about `518` lines to `491` lines, including this new untracked report.
- staged paths remain high: `418`.
- working-tree modified paths remain high: `114`.
- untracked visible paths remain: `70`, including this new report.
- accidental generated/local artifacts are no longer staged.
- `.env` files are now ignored while `.env.example` remains visible for review.
- generated benchmark/report outputs are now ignored.

The repository is **cleaner but still not release-ready**. The remaining staged tree is broad source/release-surface work that needs owner review and split commits.

## Commands Run

Initial required inspection:

```bash
git status --short
git diff --stat
git diff --cached --stat
git ls-files | grep -E '(^|/)\.DS_Store$|(^|/)\.env$|dist/|target/|node_modules/|__pycache__|\.pytest_cache|\.a$|docs/.*(_latest|_last|report).*\.json$' || true
```

Additional checks:

```bash
git diff --cached --name-status | rg '(^A\s+(\.DS_Store|lib/.*\.a|npm/qm-|wal/.*\.log)$|dist/|__pycache__|\.pytest_cache|node_modules|target/|\.env$|docs/.*(_latest|_last|report).*\.json$)'
git status --short --ignored | rg '(^\?\?|^!!).*?(\.DS_Store|\.env$|node_modules|dist/|target/|__pycache__|\.pytest_cache|\.a$|\.prof$|_latest\.json$|_last\.json$|report\.txt$|wal_.*\.log)'
git diff --cached --name-only | wc -l
git diff --name-only | wc -l
git status --short | rg '^\?\?' | wc -l
```

## Files Removed From Staging

These were removed from the Git index only. The working-tree files were not deleted.

```text
.DS_Store
lib/libqm_arm64.a
lib/libqm_x64.a
npm/qm-darwin-arm64
npm/qm-linux-aarch64
npm/qm-linux-x64
npm/qm-win32-x64.exe
wal/wal_0000000000000000.log
```

Reason:

- `.DS_Store` is OS metadata.
- `lib/*.a` are static archive artifacts.
- `npm/qm-*` are platform binary payloads, not source.
- `wal/*.log` is runtime data.

## .gitignore Changes

Updated `.gitignore` to cover:

- `node_modules/`
- `.env`, `.env.*`
- exceptions for `.env.example`
- `*.a`
- `*.prof`
- `tmp/`
- `qm_engine/target/`
- `qmvir-studio/src-tauri/target/`
- `npm/qm-*`
- `wal/*.log`
- generated benchmark/report outputs:
  - `docs/*_latest.json`
  - `docs/*_last.json`
  - `docs/*_last_*.json`
  - `docs/native_sql_perf_profile_*.prof`
  - `*_regression_report.txt`
  - `*_performance_report.txt`
  - `*_visibility_tests.txt`
  - `verify_vector_mapping_last.txt`
  - `vector_last_run_*.json`
  - `vector_timing_breakdown.json`

Validation after the change:

```bash
git diff --cached --name-status | rg '(^A\s+(\.DS_Store|lib/.*\.a|npm/qm-|wal/.*\.log)$|(^|/)\.env$|docs/.*(_latest|_last|report).*\.json$)' || true
```

Output: no matches.

```bash
git ls-files | grep -E '(^|/)\.DS_Store$|(^|/)\.env$|dist/|target/|node_modules/|__pycache__|\.pytest_cache|\.a$|docs/.*(_latest|_last|report).*\.json$' || true
```

Output: no matches.

## Secret Handling

`npm/.env` is now ignored and no longer appears in normal `git status --short`.

`npm/.env.example` remains visible:

```text
?? npm/.env.example
```

That is intentional. The example file should be reviewed before commit to ensure it contains placeholders only.

## Remaining Release Blockers

The release surface is still too broad:

- `418` paths are still staged.
- `114` paths have unstaged diffs.
- `70` visible untracked paths remain, including this report.
- Large subsystem changes are still staged across:
  - Python platform packages
  - `qm_core`
  - `qm_engine`
  - tests
  - `qmvir-studio`
  - npm package metadata and SDK files
  - docs and release/config files

These are not safe to auto-unstage or delete because many appear to be meaningful source or release assets.

`qmvir-studio` remains a reproducibility blocker until dependencies are installed and the build is rerun:

```text
npm run build
tsc: command not found
```

No `npm ci` was run in this cleanup pass.

## Recommended Next Steps

1. Review and commit `.gitignore` plus this cleanup report as a dedicated release-hygiene change.
2. Review `npm/.env.example` for placeholder-only content.
3. Split the remaining staged source work into focused commits by subsystem.
4. From a clean checkout, rerun:
   - Rust release gate
   - Python pytest suite
   - benchmark scripts that produce ignored local outputs
   - `npm ci && npm run build` in `qmvir-studio`
5. Keep generated binaries, WAL logs, local reports, and benchmark output JSON out of source commits unless a release manifest explicitly requires them.
