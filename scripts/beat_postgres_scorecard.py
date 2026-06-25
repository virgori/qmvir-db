#!/usr/bin/env python3
"""Aggregate benchmark JSONs into a beat-PostgreSQL gate scorecard."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


GATES = {
    "oltp_strict_macos": {
        "label": "OLTP strict durable (macOS per-commit)",
        "paths": ["docs/postgres_comparison_persistent_wal_latest.json"],
        "goal": "qm_wins == total",
    },
    "oltp_linux_release": {
        "label": "OLTP Linux release",
        "paths": ["benchmarks/RELEASE_2026_06_23_linux.json"],
        "section": "oltp_vs_postgres",
        "goal": "qm_wins == total",
    },
    "search_10k": {
        "label": "Search @10k",
        "paths": ["benchmarks/RELEASE_2026_06_23_linux.json"],
        "section": "search_scale_10000",
        "goal": "postgresql_wins == 0",
    },
    "search_100k": {
        "label": "Search @100k",
        "paths": ["benchmarks/RELEASE_2026_06_23_linux.json"],
        "section": "search_scale_100000",
        "goal": "postgresql_wins == 0",
    },
}


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    return json.loads(path.read_text())


def section_stats(doc: dict[str, Any], section: str | None) -> dict[str, Any]:
    if section:
        if section in doc:
            doc = doc[section]
        elif isinstance(doc.get("sections"), dict) and section in doc["sections"]:
            doc = doc["sections"][section]
        else:
            doc = {}
    comparison = doc.get("comparison") or []
    qm_wins = doc.get("qm_wins")
    pg_wins = doc.get("postgresql_wins")
    total = doc.get("total")
    if qm_wins is None:
        qm_wins = sum(1 for c in comparison if c.get("winner") == "QM")
    if pg_wins is None:
        pg_wins = sum(1 for c in comparison if c.get("winner") == "PostgreSQL")
    if total is None:
        total = len([c for c in comparison if not c.get("skipped")])
    pg_losses = [
        c["workload"]
        for c in comparison
        if c.get("winner") == "PostgreSQL" and not c.get("skipped")
    ]
    return {
        "qm_wins": qm_wins,
        "postgresql_wins": pg_wins,
        "total": total,
        "postgresql_losses": pg_losses,
        "publish_ready": doc.get("deployment", {}).get("publish_ready"),
        "fairness_mode": doc.get("deployment", {}).get("fairness_mode"),
    }


def eval_gate(stats: dict[str, Any], goal: str) -> bool:
    if goal == "qm_wins == total":
        return stats["qm_wins"] == stats["total"] and stats["total"] > 0
    if goal == "postgresql_wins == 0":
        return stats["postgresql_wins"] == 0 and stats["total"] > 0
    return False


def scan_dir(run_dir: Path) -> list[Path]:
    return sorted(run_dir.glob("*.json"))


def summarize_file(path: Path) -> dict[str, Any]:
    doc = load_json(path)
    if not doc:
        return {"path": str(path), "error": "missing"}
    stats = section_stats(doc, None)
    return {"path": str(path), **stats}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="*",
        help="JSON files or directories from parallel benchmark runs",
    )
    parser.add_argument("--repo-root", default=".", help="Repo root for canonical gate paths")
    args = parser.parse_args()

    root = Path(args.repo_root).resolve()
    print("=== Beat PostgreSQL gate scorecard ===\n")

    passed = 0
    failed = 0
    for gate_id, gate in GATES.items():
        found = False
        for rel in gate["paths"]:
            path = root / rel
            doc = load_json(path)
            if not doc:
                continue
            stats = section_stats(doc, gate.get("section"))
            ok = eval_gate(stats, gate["goal"])
            status = "PASS" if ok else "FAIL"
            if ok:
                passed += 1
            else:
                failed += 1
            found = True
            print(f"[{status}] {gate['label']}")
            print(f"       file: {path}")
            print(
                f"       QM {stats['qm_wins']}/{stats['total']} | "
                f"PG wins {stats['postgresql_wins']}"
            )
            if stats.get("fairness_mode"):
                print(f"       fairness={stats['fairness_mode']} publish_ready={stats['publish_ready']}")
            if stats["postgresql_losses"]:
                print(f"       PG still wins: {', '.join(stats['postgresql_losses'])}")
            print()
            break
        if not found:
            failed += 1
            print(f"[SKIP] {gate['label']} — no artifact at {gate['paths']}\n")

    extra_paths: list[Path] = []
    for p in args.paths:
        path = Path(p)
        if path.is_dir():
            extra_paths.extend(scan_dir(path))
        elif path.is_file():
            extra_paths.append(path)

    if extra_paths:
        print("--- Parallel run artifacts ---")
        for path in extra_paths:
            row = summarize_file(path)
            if row.get("error"):
                print(f"  ? {path}: missing")
                continue
            tag = "OK" if row["postgresql_wins"] == 0 else "PG"
            print(
                f"  [{tag}] {path.name}: QM {row['qm_wins']}/{row['total']} "
                f"(PG wins {row['postgresql_wins']})"
            )
            if row["postgresql_losses"]:
                print(f"        PG wins: {', '.join(row['postgresql_losses'])}")
        print()

    print(f"Gates: {passed} passed, {failed} failed/skipped")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
