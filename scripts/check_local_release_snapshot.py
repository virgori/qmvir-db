#!/usr/bin/env python3
"""Validate that a local release snapshot is clean and complete."""

from __future__ import annotations

import argparse
import fnmatch
from pathlib import Path


FORBIDDEN_BASENAMES = {
    ".DS_Store",
    "full_sql_regression_report.txt",
    "performance_regression_report.txt",
    "transaction_regression_report.txt",
    "vector_timing_breakdown.json",
    "verify_vector_mapping_last.txt",
}

FORBIDDEN_PATTERNS = {
    "vector_last_run_*.json",
    "npm/qm-*",
    "lib/libqm_*.a",
}

FORBIDDEN_DIR_PARTS = {
    ".git",
    ".pytest_cache",
    "__pycache__",
    "build",
    "data",
    "dist",
    "node_modules",
    "qmvir-studio",
    "qmvir.egg-info",
    "target",
    "tmp",
    "wal",
}

REQUIRED_PATHS = {
    "pyproject.toml",
    "qm_engine/Cargo.toml",
    "qm_engine/Cargo.lock",
    "qm_engine/src",
    "qm_engine/tests",
    "tests",
    "scripts/check_no_space_number_duplicates.sh",
    "scripts/release_benchmark_native_sql.py",
    "docs/native_sql_benchmark_baseline.json",
    "docs/LOCAL_RELEASE_MANIFEST_2026_05_18.md",
    "docs/ROOT_CAUSE_CLOSURE_2026_05_18.md",
}


def is_env_file(path: Path) -> bool:
    name = path.name
    if name.endswith(".env.example"):
        return False
    return name == ".env" or name.endswith(".env")


def is_forbidden(root: Path, path: Path) -> str | None:
    rel = path.relative_to(root).as_posix()
    parts = rel.split("/")
    if " 2" in rel:
        return "duplicate-looking path containing ' 2'"
    if is_env_file(path):
        return "secret/local env file"
    if path.name in FORBIDDEN_BASENAMES:
        return "local/generated artifact"
    if any(part in FORBIDDEN_DIR_PARTS for part in parts):
        return "forbidden generated/local directory"
    if rel == "qm_engine/target" or rel.startswith("qm_engine/target/"):
        return "Rust build output"
    for pattern in FORBIDDEN_PATTERNS:
        if fnmatch.fnmatch(rel, pattern):
            return "forbidden generated/binary artifact"
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", nargs="?", default=".")
    args = parser.parse_args()

    root = Path(args.snapshot).resolve()
    if not root.exists():
        print(f"FAIL: snapshot does not exist: {root}")
        return 1

    failures: list[str] = []
    for required in sorted(REQUIRED_PATHS):
        if not (root / required).exists():
            failures.append(f"missing required path: {required}")

    for path in sorted(root.rglob("*")):
        reason = is_forbidden(root, path)
        if reason:
            failures.append(f"{reason}: {path.relative_to(root).as_posix()}")

    if failures:
        print("FAIL: local release snapshot check failed")
        for failure in failures:
            print(f"- {failure}")
        return 1

    file_count = sum(1 for path in root.rglob("*") if path.is_file())
    size_mb = sum(path.stat().st_size for path in root.rglob("*") if path.is_file()) / (1024 * 1024)
    print("PASS: local release snapshot check")
    print(f"snapshot: {root}")
    print(f"files: {file_count}")
    print(f"size_mb: {size_mb:.2f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
