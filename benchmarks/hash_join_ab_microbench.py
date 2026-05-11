#!/usr/bin/env python3
"""Micro-benchmark: HashJoin fixed-right vs adaptive build-side.

This isolates operator-level behavior and avoids network/parser overhead.
"""

from __future__ import annotations

import argparse
import json
import random
import time
from pathlib import Path
from typing import Any

from qm_core.execution.join import HashJoin, JoinType, ScanOperator


def _gen_rows(n: int, key_cardinality: int, prefix: str) -> list[dict[str, Any]]:
    rnd = random.Random(20260309 + n + key_cardinality)
    rows: list[dict[str, Any]] = []
    for i in range(n):
        rows.append(
            {
                "id": i,
                "k": rnd.randint(0, max(1, key_cardinality - 1)),
                f"{prefix}_payload": f"{prefix}-{i}",
            }
        )
    return rows


def _run_once_fixed_right(left_rows: list[dict[str, Any]], right_rows: list[dict[str, Any]]) -> int:
    op = HashJoin(ScanOperator(left_rows), ScanOperator(right_rows), "k", "k", JoinType.INNER)
    return sum(1 for _ in op)


def _run_once_adaptive(left_rows: list[dict[str, Any]], right_rows: list[dict[str, Any]]) -> int:
    if len(left_rows) <= len(right_rows):
        # Build on left by swapping sides while preserving INNER semantics.
        op = HashJoin(ScanOperator(right_rows), ScanOperator(left_rows), "k", "k", JoinType.INNER)
    else:
        op = HashJoin(ScanOperator(left_rows), ScanOperator(right_rows), "k", "k", JoinType.INNER)
    return sum(1 for _ in op)


def _bench(case: tuple[int, int, int], rounds: int) -> dict[str, Any]:
    left_n, right_n, card = case
    left_rows = _gen_rows(left_n, card, "l")
    right_rows = _gen_rows(right_n, card, "r")

    fixed_lat_ms: list[float] = []
    adaptive_lat_ms: list[float] = []
    fixed_out = 0
    adaptive_out = 0

    for _ in range(rounds):
        t0 = time.perf_counter()
        fixed_out = _run_once_fixed_right(left_rows, right_rows)
        fixed_lat_ms.append((time.perf_counter() - t0) * 1000.0)

        t1 = time.perf_counter()
        adaptive_out = _run_once_adaptive(left_rows, right_rows)
        adaptive_lat_ms.append((time.perf_counter() - t1) * 1000.0)

    fixed_avg = sum(fixed_lat_ms) / max(1, len(fixed_lat_ms))
    adaptive_avg = sum(adaptive_lat_ms) / max(1, len(adaptive_lat_ms))
    speedup = fixed_avg / adaptive_avg if adaptive_avg > 0 else 0.0

    return {
        "left_rows": left_n,
        "right_rows": right_n,
        "key_cardinality": card,
        "rounds": rounds,
        "output_rows_fixed": fixed_out,
        "output_rows_adaptive": adaptive_out,
        "fixed_avg_ms": fixed_avg,
        "adaptive_avg_ms": adaptive_avg,
        "adaptive_speedup": speedup,
    }


def _to_markdown(results: list[dict[str, Any]]) -> str:
    lines = [
        "# HashJoin Micro-benchmark (A/B)",
        "",
        "| Left | Right | Card | Fixed ms | Adaptive ms | Adaptive speedup |",
        "|---:|---:|---:|---:|---:|---:|",
    ]
    for r in results:
        lines.append(
            f"| {r['left_rows']} | {r['right_rows']} | {r['key_cardinality']} | "
            f"{r['fixed_avg_ms']:.3f} | {r['adaptive_avg_ms']:.3f} | {r['adaptive_speedup']:.2f}x |"
        )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="HashJoin A/B micro-benchmark")
    p.add_argument("--rounds", type=int, default=6)
    p.add_argument(
        "--cases",
        nargs="*",
        default=["2000:200:256", "2000:2000:512", "5000:500:1024", "500:5000:1024"],
        help="Each case is left:right:key_cardinality",
    )
    p.add_argument(
        "--output-json",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/HASH_JOIN_AB.json",
    )
    p.add_argument(
        "--output-md",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/HASH_JOIN_AB.md",
    )
    return p.parse_args()


def main() -> None:
    args = parse_args()

    parsed_cases: list[tuple[int, int, int]] = []
    for c in args.cases:
        l, r, k = c.split(":")
        parsed_cases.append((int(l), int(r), int(k)))

    results = [_bench(case, args.rounds) for case in parsed_cases]

    out_json = Path(args.output_json)
    out_md = Path(args.output_md)
    out_json.write_text(json.dumps({"results": results}, indent=2))
    out_md.write_text(_to_markdown(results))

    print(f"JSON written: {out_json}")
    print(f"MD written:   {out_md}")


if __name__ == "__main__":
    main()
