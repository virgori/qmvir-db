#!/usr/bin/env python3
"""Run QM segment benchmarks: PostgreSQL, DuckDB, Qdrant."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from segment_benchmark_lib import SEGMENT_BENCHMARKS, write_segment_result  # noqa: E402


def run_script(name: str, extra_args: list[str]) -> dict:
    path = SCRIPT_DIR / name
    if not path.exists():
        return {"skipped": True, "reason": f"{name} not found"}
    out = Path(f"/tmp/qm_segment_{path.stem}.json")
    env = os.environ.copy()
    env["PYTHONPATH"] = str(SCRIPT_DIR) + os.pathsep + env.get("PYTHONPATH", "")
    cmd = [sys.executable, str(path), "--output", str(out), *extra_args]
    proc = subprocess.run(
        cmd, cwd=SCRIPT_DIR.parent, env=env, capture_output=True, text=True
    )
    if proc.returncode != 0:
        return {
            "error": (proc.stderr or proc.stdout)[-3000:],
            "exit_code": proc.returncode,
            "script": name,
        }
    if out.exists():
        return json.loads(out.read_text())
    return {"error": "no output json", "stdout": proc.stdout[-1500:], "script": name}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--segments",
        default="duckdb,qdrant",
        help="postgresql,duckdb,qdrant (comma-separated)",
    )
    parser.add_argument("--rows", type=int, default=100_000)
    parser.add_argument("--iterations", type=int, default=50)
    parser.add_argument("--output-dir", type=Path, default=Path("/tmp/qm_segments"))
    args = parser.parse_args()

    extra = ["--rows", str(args.rows), "--iterations", str(args.iterations)]
    segments = [s.strip().lower() for s in args.segments.split(",") if s.strip()]
    summary: dict = {
        "label": "QM_SEGMENT_BENCHMARK_SUITE",
        "segments": {},
        "registry": SEGMENT_BENCHMARKS,
    }

    if "postgresql" in segments or "pg" in segments:
        if os.environ.get("POSTGRES_DSN"):
            for label, script in [
                ("vector_postgresql", "compare_postgres_vector_bench.py"),
                ("search_postgresql", "compare_postgres_search_bench.py"),
            ]:
                summary["segments"][label] = run_script(script, extra)
        else:
            summary["segments"]["postgresql"] = {
                "skipped": True,
                "reason": "POSTGRES_DSN not set",
            }

    if "duckdb" in segments:
        summary["segments"]["olap_duckdb"] = run_script(
            "compare_duckdb_olap_bench.py", extra
        )

    if "qdrant" in segments:
        summary["segments"]["vector_qdrant"] = run_script(
            "compare_qdrant_vector_bench.py", extra
        )

    args.output_dir.mkdir(parents=True, exist_ok=True)
    out = args.output_dir / "segment_suite_summary.json"
    write_segment_result(out, summary)
    print(json.dumps(summary, indent=2))
    print(f"\nWrote {out}")

    failed = any(
        isinstance(v, dict) and v.get("error") and not v.get("skipped")
        for v in summary["segments"].values()
    )
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
