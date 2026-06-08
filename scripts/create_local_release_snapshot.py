#!/usr/bin/env python3
"""Create a clean local QM Engine release snapshot.

This script intentionally does not use Git as the source of truth. It copies a
curated source/test/doc allowlist from the local workspace and applies release
safety exclusions for secrets, caches, build outputs, local state, generated
artifacts, and duplicate-looking local copies.
"""

from __future__ import annotations

import argparse
import fnmatch
import os
import shutil
import tarfile
from pathlib import Path


ROOT_FILES = {
    ".dockerignore",
    ".gitignore",
    ".gitlab-ci.yml",
    "BUILD_GUIDE.md",
    "CI_CD_WORKFLOW.md",
    "Dockerfile",
    "Dockerfile.release",
    "IMPLEMENTATION_COMPLETE.txt",
    "LICENSE",
    "MVCC_COMPLETION_REPORT.txt",
    "QM_FULL_AUDIT_REPORT.md",
    "README.md",
    "USAGE_GUIDE_EN.md",
    "USAGE_GUIDE_VI.md",
    "benchmark_native.py",
    "benchmark_parallel.py",
    "benchmark_rust_vs_c.py",
    "build.sh",
    "build_from_source.sh",
    "install.sh",
    "pyproject.toml",
    "qm_app.py",
}

ROOT_DIRS = {
    ".cargo",
    ".github",
    "MUST_READ_CONTEXT",
    "analytics_platform",
    "benchmark",
    "benchmarks",
    "cache_layer",
    "core_db",
    "docs",
    "gateway",
    "include",
    "indexing",
    "observability",
    "pipelines",
    "qm_core",
    "qm_engine",
    "scripts",
    "sdk",
    "search_platform",
    "storage",
    "tests",
    "tools",
    "vector_platform",
    "version",
}

EXCLUDED_DIR_NAMES = {
    ".git",
    ".pytest_cache",
    ".venv",
    "__pycache__",
    "_py_legacy",
    "build",
    "data",
    "dist",
    "node_modules",
    "qm_engine/target",
    "qmvir-studio",
    "qmvir.egg-info",
    "release_snapshots",
    "target",
    "tmp",
    "wal",
}

EXCLUDED_BASENAMES = {
    ".DS_Store",
    "full_sql_regression_report.txt",
    "performance_regression_report.txt",
    "transaction_regression_report.txt",
    "vector_timing_breakdown.json",
    "verify_vector_mapping_last.txt",
}

EXCLUDED_PATTERNS = {
    "*.pyc",
    "*.pyo",
    "vector_last_run_*.json",
    "lib/libqm_*.a",
    "npm/qm-*",
    "docs/native_sql_benchmark_last.json",
}


def rel_posix(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix()


def contains_space_duplicate(rel: str) -> bool:
    return " 2" in rel


def is_env_file(path: Path) -> bool:
    name = path.name
    if name.endswith(".env.example"):
        return False
    return name == ".env" or name.endswith(".env")


def is_excluded(rel: str, path: Path, *, is_dir: bool) -> tuple[bool, str]:
    parts = rel.split("/")
    if contains_space_duplicate(rel):
        return True, "duplicate-looking local copy"
    if path.name in EXCLUDED_BASENAMES:
        return True, "local/generated artifact"
    if is_env_file(path):
        return True, "secret/local env file"
    for part in parts:
        if part in EXCLUDED_DIR_NAMES:
            return True, "excluded directory"
    if rel == "qm_engine/target" or rel.startswith("qm_engine/target/"):
        return True, "Rust build output"
    for pattern in EXCLUDED_PATTERNS:
        if fnmatch.fnmatch(rel, pattern):
            return True, "excluded generated/binary artifact"
    if is_dir and path.name in EXCLUDED_DIR_NAMES:
        return True, "excluded directory"
    return False, ""


def iter_allowlist(source_root: Path):
    for name in sorted(ROOT_FILES):
        path = source_root / name
        if path.exists():
            yield path
    for name in sorted(ROOT_DIRS):
        path = source_root / name
        if path.exists():
            yield path


def copy_snapshot(source_root: Path, dest_root: Path) -> dict[str, int]:
    copied_files = 0
    skipped = 0
    bytes_copied = 0

    for source in iter_allowlist(source_root):
        rel = rel_posix(source, source_root)
        excluded, _reason = is_excluded(rel, source, is_dir=source.is_dir())
        if excluded:
            skipped += 1
            continue
        if source.is_file():
            target = dest_root / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            copied_files += 1
            bytes_copied += target.stat().st_size
            continue

        for current_dir, dir_names, file_names in os.walk(source):
            current = Path(current_dir)
            current_rel = rel_posix(current, source_root)
            filtered_dirs = []
            for dir_name in sorted(dir_names):
                dir_path = current / dir_name
                dir_rel = rel_posix(dir_path, source_root)
                excluded, _reason = is_excluded(dir_rel, dir_path, is_dir=True)
                if excluded:
                    skipped += 1
                else:
                    filtered_dirs.append(dir_name)
            dir_names[:] = filtered_dirs

            excluded, _reason = is_excluded(current_rel, current, is_dir=True)
            if excluded:
                skipped += len(file_names)
                dir_names[:] = []
                continue

            for file_name in sorted(file_names):
                file_path = current / file_name
                file_rel = rel_posix(file_path, source_root)
                excluded, _reason = is_excluded(file_rel, file_path, is_dir=False)
                if excluded:
                    skipped += 1
                    continue
                target = dest_root / file_rel
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(file_path, target)
                copied_files += 1
                bytes_copied += target.stat().st_size

    return {"copied_files": copied_files, "skipped": skipped, "bytes": bytes_copied}


def archive_snapshot(snapshot_dir: Path) -> Path:
    archive_path = snapshot_dir.parent / f"{snapshot_dir.name}.tar"
    if archive_path.exists():
        archive_path.unlink()
    with tarfile.open(archive_path, "w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(snapshot_dir.rglob("*")):
            arcname = snapshot_dir.name + "/" + path.relative_to(snapshot_dir).as_posix()
            info = archive.gettarinfo(str(path), arcname)
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            if path.is_file():
                with path.open("rb") as handle:
                    archive.addfile(info, handle)
            else:
                archive.addfile(info)
    return archive_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", default="5.4.0")
    parser.add_argument("--rc", default="rc1")
    parser.add_argument("--source-root", default=".")
    parser.add_argument("--output-root", default="release_snapshots")
    parser.add_argument("--clean", action="store_true")
    parser.add_argument("--archive", action="store_true")
    args = parser.parse_args()

    source_root = Path(args.source_root).resolve()
    output_root = Path(args.output_root)
    if not output_root.is_absolute():
        output_root = source_root / output_root
    output_root.mkdir(parents=True, exist_ok=True)

    snapshot_dir = output_root / f"qm_engine_{args.version}_{args.rc}"
    if snapshot_dir.exists():
        if not args.clean:
            raise SystemExit(f"Snapshot exists: {snapshot_dir}. Re-run with --clean to replace it.")
        shutil.rmtree(snapshot_dir)
    snapshot_dir.mkdir(parents=True)

    stats = copy_snapshot(source_root, snapshot_dir)
    size_mb = sum(path.stat().st_size for path in snapshot_dir.rglob("*") if path.is_file()) / (1024 * 1024)

    print(f"snapshot: {snapshot_dir}")
    print(f"copied_files: {stats['copied_files']}")
    print(f"skipped_entries: {stats['skipped']}")
    print(f"size_mb: {size_mb:.2f}")

    if args.archive:
        archive_path = archive_snapshot(snapshot_dir)
        archive_mb = archive_path.stat().st_size / (1024 * 1024)
        print(f"archive: {archive_path}")
        print(f"archive_size_mb: {archive_mb:.2f}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
