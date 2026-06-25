#!/usr/bin/env python3
"""Small ANN correctness checks before running full vector benchmarks."""

from __future__ import annotations

import argparse
import json
import sys

from vector_recall_lib import l2_squared, qm_select_ids


def unique_vec_literal(row_id: int, dim: int) -> str:
    return "[" + ",".join(f"{row_id * 0.01 + j * 0.001:.6f}" for j in range(dim)) + "]"


def unique_vec(row_id: int, dim: int) -> list[float]:
    return [row_id * 0.01 + j * 0.001 for j in range(dim)]


def run_self_query_check(*, rows: int, dim: int, probe_id: int) -> dict:
    import qm_engine  # type: ignore

    qm = qm_engine.NativeSqlEngine()
    qm.execute(f"CREATE TABLE sanity_vec (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    for i in range(rows):
        qm.execute(
            f"INSERT INTO sanity_vec (id, embedding) VALUES ({i}, '{unique_vec_literal(i, dim)}')"
        )
    qm.execute("CREATE INDEX idx_sanity_hnsw ON sanity_vec (embedding) USING hnsw")

    query = unique_vec(probe_id, dim)
    query_lit = unique_vec_literal(probe_id, dim)
    top = qm_select_ids(
        qm,
        f"SELECT id FROM sanity_vec ORDER BY embedding <-> '{query_lit}' LIMIT 5",
    )
    top1 = top[0] if top else None
    top1_dist = l2_squared(query, unique_vec(top1, dim)) if top1 is not None else None
    return {
        "check": "unique_vector_self_query",
        "rows": rows,
        "dim": dim,
        "probe_id": probe_id,
        "top5_ids": top,
        "top1_id": top1,
        "top1_l2_squared": top1_dist,
        "pass": top1 == probe_id and top1_dist == 0.0,
        "expect": "top1 == probe_id and L2^2 == 0",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="QM vector ANN sanity checks")
    parser.add_argument("--rows", type=int, default=100)
    parser.add_argument("--dim", type=int, default=8)
    parser.add_argument("--probe-id", type=int, default=42)
    args = parser.parse_args()

    result = run_self_query_check(
        rows=args.rows,
        dim=args.dim,
        probe_id=args.probe_id,
    )
    print(json.dumps(result, indent=2))
    if not result["pass"]:
        print("FAIL: self-query did not return the inserted vector as top1", file=sys.stderr)
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
