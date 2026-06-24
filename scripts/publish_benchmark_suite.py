#!/usr/bin/env python3
"""Unified publish benchmark suite for QM Native SQL release gating.

Runs Tier 1–3 workloads, optionally repeats for median aggregation, and writes
JSON + Markdown artifacts under benchmarks/.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))

from publish_benchmark_extras import run_all as run_extras  # noqa: E402
from publish_benchmark_lib import (  # noqa: E402
    aggregate_comparison_runs,
    environment_info,
    max_rss_mb,
    render_markdown,
    run_subprocess_json,
)


def python_exe() -> str:
    return sys.executable


def run_release_native_sql(quick: bool, iterations: int | None, tmp: Path) -> dict[str, Any]:
    out = tmp / "release_native_sql.json"
    cmd = [
        python_exe(),
        str(SCRIPT_DIR / "release_benchmark_native_sql.py"),
        "--output",
        str(out),
        "--build-mode",
        "release",
    ]
    if quick:
        cmd.append("--quick")
    if iterations is not None:
        cmd.extend(["--iterations", str(iterations)])
    code, payload, log = run_subprocess_json(cmd, cwd=REPO_ROOT, timeout_s=600)
    if payload is None and out.exists():
        payload = json.loads(out.read_text())
    return {
        "exit_code": code,
        "payload": payload,
        "log_tail": log[-4000:] if log else "",
    }


def run_oltp_vs_postgres(
    dsn: str,
    iterations: int,
    tmp: Path,
    run_id: int,
) -> dict[str, Any] | None:
    out = tmp / f"oltp_run_{run_id}.json"
    cmd = [
        python_exe(),
        str(SCRIPT_DIR / "compare_postgres_native_sql.py"),
        "--iterations",
        str(iterations),
        "--output",
        str(out),
        "--qm-mode",
        "persistent-wal",
        "--qm-sync-policy",
        "per-commit",
        "--durability-mode",
        "wal_fsync",
        "--dsn",
        dsn,
    ]
    code, payload, log = run_subprocess_json(cmd, cwd=REPO_ROOT, env={"POSTGRES_DSN": dsn}, timeout_s=1800)
    if payload is None and out.exists():
        payload = json.loads(out.read_text())
    if code != 0 and payload is None:
        return {"error": f"oltp benchmark failed (exit {code})", "log_tail": log[-4000:]}
    return payload


def run_search_vs_postgres(
    dsn: str,
    iterations: int,
    rows: int,
    tmp: Path,
    run_id: int,
) -> dict[str, Any] | None:
    out = tmp / f"search_{rows}_run_{run_id}.json"
    cmd = [
        python_exe(),
        str(SCRIPT_DIR / "compare_postgres_search_bench.py"),
        "--iterations",
        str(iterations),
        "--rows",
        str(rows),
        "--output",
        str(out),
    ]
    code, payload, log = run_subprocess_json(cmd, cwd=REPO_ROOT, env={"POSTGRES_DSN": dsn}, timeout_s=3600)
    if payload is None and out.exists():
        payload = json.loads(out.read_text())
    if code != 0 and payload is None:
        return {"error": f"search benchmark failed rows={rows} (exit {code})", "log_tail": log[-4000:]}
    return payload


def run_group_commit(dsn: str, tmp: Path) -> dict[str, Any]:
    out = tmp / "group_commit.json"
    cmd = [
        python_exe(),
        str(SCRIPT_DIR / "compare_postgres_group_commit.py"),
        "--output",
        str(out),
        "--dsn",
        dsn,
        "--concurrency",
        "8",
        "--operations-per-worker",
        "200",
    ]
    code, payload, log = run_subprocess_json(cmd, cwd=REPO_ROOT, env={"POSTGRES_DSN": dsn}, timeout_s=1800)
    if payload is None and out.exists():
        payload = json.loads(out.read_text())
    return {"exit_code": code, "payload": payload, "log_tail": log[-2000:] if log else ""}


def normalize_oltp_payload(payload: dict[str, Any]) -> dict[str, Any]:
    if "comparison" in payload:
        return payload
    results = payload.get("results") or payload.get("qm", {}).get("results") or []
    pg_results = payload.get("postgresql", {}).get("results") or []
    pg_by_name = {r.get("name"): r for r in pg_results}
    comparison = []
    for row in results:
        name = row.get("name")
        pg = pg_by_name.get(name, {})
        qm_p50 = row.get("p50_ms")
        pg_p50 = pg.get("p50_ms")
        winner = "QM" if qm_p50 is not None and pg_p50 is not None and qm_p50 < pg_p50 else (
            "PostgreSQL" if qm_p50 is not None and pg_p50 is not None and pg_p50 < qm_p50 else "tie"
        )
        comparison.append(
            {
                "workload": name,
                "qm": {"p50_ms": qm_p50},
                "postgresql": {"p50_ms": pg_p50},
                "winner": winner,
            }
        )
    wins = sum(1 for c in comparison if c.get("winner") == "QM")
    pg_wins = sum(1 for c in comparison if c.get("winner") == "PostgreSQL")
    return {
        **payload,
        "comparison": comparison,
        "qm_wins": wins,
        "postgresql_wins": pg_wins,
        "total": len(comparison),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="QM publish benchmark suite")
    parser.add_argument("--quick", action="store_true", help="CI smoke: 1 run, smaller workloads")
    parser.add_argument("--runs", type=int, default=None, help="repeat PG comparisons for median (default 5 full, 1 quick)")
    parser.add_argument("--release-tag", default="unreleased")
    parser.add_argument("--output-json", type=Path, default=None)
    parser.add_argument("--output-md", type=Path, default=None)
    parser.add_argument("--skip-postgres", action="store_true")
    parser.add_argument("--include-group-commit", action="store_true")
    parser.add_argument("--scale-rows", type=int, nargs="*", default=None, help="e.g. 10000 100000")
    parser.add_argument("--oltp-iterations", type=int, default=None)
    parser.add_argument("--search-iterations", type=int, default=None)
    args = parser.parse_args()

    runs = args.runs if args.runs is not None else (1 if args.quick else 5)
    oltp_iters = args.oltp_iterations or (200 if args.quick else 1000)
    search_iters = args.search_iterations or (50 if args.quick else 200)
    scale_rows = args.scale_rows if args.scale_rows is not None else ([10000] if args.quick else [10000, 100000])

    dsn = os.environ.get("POSTGRES_DSN") or os.environ.get("QM_POSTGRES_DSN")
    postgres = bool(dsn) and not args.skip_postgres

    try:
        import qm_engine  # type: ignore  # noqa: F401
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2

    env = environment_info(build_mode="release")
    sections: dict[str, Any] = {}
    summary: dict[str, Any] = {}

    with tempfile.TemporaryDirectory(prefix="qm-publish-") as tmpdir:
        tmp = Path(tmpdir)

        print(f"[publish] release native_sql smoke (quick={args.quick})")
        sections["release_native_sql"] = run_release_native_sql(args.quick, None, tmp)

        if postgres:
            oltp_runs: list[dict[str, Any]] = []
            for i in range(runs):
                print(f"[publish] oltp vs postgres run {i + 1}/{runs}")
                payload = run_oltp_vs_postgres(dsn, oltp_iters, tmp, i)
                if payload and "error" not in payload:
                    oltp_runs.append(normalize_oltp_payload(payload))
            if oltp_runs:
                agg = aggregate_comparison_runs(oltp_runs)
                sections["oltp_vs_postgres"] = agg
                summary["oltp"] = {"qm_wins": agg["qm_wins"], "total": agg["total"], "runs": runs}

            search_runs: list[dict[str, Any]] = []
            for i in range(runs):
                print(f"[publish] search vs postgres (1k rows) run {i + 1}/{runs}")
                payload = run_search_vs_postgres(dsn, search_iters, 1000, tmp, i)
                if payload and "error" not in payload:
                    search_runs.append(payload)
            if search_runs:
                agg = aggregate_comparison_runs(search_runs)
                sections["search_vs_postgres"] = agg
                summary["search"] = {"qm_wins": agg["qm_wins"], "total": agg["total"], "runs": runs}

            scale_summary: list[dict[str, Any]] = []
            for rows in scale_rows:
                scale_runs: list[dict[str, Any]] = []
                scale_iters = max(30, search_iters // 2) if rows >= 100_000 else search_iters
                for i in range(runs if rows < 100_000 else max(1, runs // 2)):
                    print(f"[publish] search scale {rows} run {i + 1}")
                    payload = run_search_vs_postgres(dsn, scale_iters, rows, tmp, i)
                    if payload and "error" not in payload:
                        scale_runs.append(payload)
                if scale_runs:
                    agg = aggregate_comparison_runs(scale_runs)
                    agg["rows"] = rows
                    sections[f"search_scale_{rows}"] = agg
                    scale_summary.append({"rows": rows, "qm_wins": agg["qm_wins"], "total": agg["total"]})
            if scale_summary:
                summary["search_scale"] = scale_summary

            if args.include_group_commit and not args.quick:
                print("[publish] group commit comparison")
                sections["group_commit"] = run_group_commit(dsn, tmp)
        else:
            sections["postgres"] = {"skipped": True, "reason": "POSTGRES_DSN not set or --skip-postgres"}

        print("[publish] QM extras: recovery, concurrent, mixed")
        extras = run_extras(qm_engine, quick=args.quick)
        sections["extras"] = extras
        summary["recovery"] = extras.get("recovery", {})
        summary["concurrent_oltp"] = extras.get("concurrent_oltp", {})
        summary["mixed_workload"] = extras.get("mixed_workload", {})

    env["max_rss_mb_end"] = max_rss_mb()
    env["peak_rss_mb"] = env["max_rss_mb_end"]
    env["postgres_available"] = postgres
    if postgres:
        env["postgres_dsn_host"] = dsn.split("@")[-1] if "@" in (dsn or "") else "local"

    tag = args.release_tag.replace("/", "_")
    out_json = args.output_json or REPO_ROOT / "benchmarks" / f"RELEASE_{tag}_linux.json"
    out_md = args.output_md or REPO_ROOT / "benchmarks" / f"RELEASE_{tag}_linux.md"

    report = {
        "schema_version": 1,
        "release_tag": args.release_tag,
        "mode": "quick" if args.quick else "full",
        "runs": runs,
        "environment": env,
        "sections": sections,
        "summary": summary,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    out_md.write_text(render_markdown(report))
    print(f"wrote {out_json}")
    print(f"wrote {out_md}")

    if postgres and summary.get("oltp") and summary["oltp"].get("qm_wins", 0) == 0:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
