#!/usr/bin/env python3
"""Profile autocommit DML gaps (vector/json insert-update) with native counters.

Usage:
  QMVIR_NATIVE_SQL_PROFILE=1 python3 scripts/dml_gap_profile.py --rows 100000
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import time
from typing import Any


def vec_literal(i: int, dim: int) -> str:
    return "[" + ",".join(f"{((i + j) % 17) / 17.0:.6f}" for j in range(dim)) + "]"


def percentile(samples: list[float], pct: float) -> float:
    if not samples:
        return 0.0
    ordered = sorted(samples)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def bench_ms(fn, *, warmup: int = 5, iters: int = 50) -> dict[str, float]:
    for _ in range(warmup):
        fn()
    samples: list[float] = []
    for _ in range(iters):
        t0 = time.perf_counter()
        fn()
        samples.append((time.perf_counter() - t0) * 1000.0)
    return {
        "p50_ms": percentile(samples, 50),
        "p95_ms": percentile(samples, 95),
        "iters": iters,
    }


def profile_breakdown(engine: Any) -> dict[str, Any]:
    snap = engine.profile_snapshot()
    keys = [
        "execute_ns",
        "parse_ns",
        "sql_dispatch_ns",
        "index_update_ns",
        "dml_secondary_index_ns",
        "dml_catalog_snapshot_ns",
        "dml_catalog_snapshot_count",
        "row_clone_ns",
        "wal_write_ns",
        "wal_sync_ns",
        "pk_lookup_ns",
    ]
    out: dict[str, Any] = {}
    total = 0.0
    for k in keys:
        v = float(snap.get(k, 0))
        out[k] = v
        if k.endswith("_ns"):
            total += v
    if total > 0:
        out["pct"] = {k: round(100.0 * out[k] / total, 2) for k in keys if k.endswith("_ns")}
    return out


def seed_json(engine: Any, rows: int) -> None:
    engine.execute(
        "CREATE TABLE json_bench (id INTEGER PRIMARY KEY, data JSON, tags TEXT, body TEXT)"
    )
    engine.execute("CREATE INDEX json_bench_tags ON json_bench (tags)")
    engine.execute(
        f"INSERT INTO json_bench SELECT i, "
        f"'{{\"name\":\"user' || i || '\",\"score\":' || (i % 100) || '}}', "
        f"'tag_' || (i % 20), "
        f"'alpha beta gamma text chunk ' || i || ' needle' "
        f"FROM generate_series(0, {rows - 1}) AS t(i)"
    )
    engine.execute("CREATE INDEX idx_json_bench_body ON json_bench (body) USING gin")
    engine.execute("CREATE INDEX idx_json_bench_body_trgm ON json_bench (body) USING gin_trgm")
    engine.execute(
        "CREATE INDEX idx_json_bench_data_name ON json_bench (data) USING json_path('name')"
    )


def seed_vector(engine: Any, dim: int, rows: int) -> None:
    engine.execute(f"CREATE TABLE vec_bench (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    batch = 500
    for start in range(0, rows, batch):
        end = min(rows, start + batch)
        for i in range(start, end):
            engine.execute(
                f"INSERT INTO vec_bench (id, embedding) VALUES ({i}, '{vec_literal(i, dim)}')"
            )
    engine.execute(
        "CREATE INDEX idx_vec_bench_embedding_l2 ON vec_bench (embedding) "
        "USING hnsw (embedding vector_l2_ops)"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="DML gap profiler")
    parser.add_argument("--rows", type=int, default=int(os.environ.get("DML_PROFILE_ROWS", "100000")))
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--iters", type=int, default=30)
    parser.add_argument("--output", type=str, default="/tmp/qm_dml_gap_profile.json")
    args = parser.parse_args()

    import qm_engine  # type: ignore

    payload: dict[str, Any] = {"rows": args.rows, "dim": args.dim, "workloads": {}}

    # JSON insert autocommit
    qm = qm_engine.NativeSqlEngine()
    seed_json(qm, args.rows)
    qm.reset_profile_snapshot()
    next_id = args.rows + 10_000

    def json_insert() -> None:
        nonlocal next_id
        next_id += 1
        qm.execute(
            f"INSERT INTO json_bench (id, data, tags, body) VALUES "
            f"({next_id}, '{{\"name\":\"new\"}}', 'tag_1', 'fresh text')"
        )

    payload["workloads"]["json.insert_autocommit"] = {
        "latency": bench_ms(json_insert, iters=args.iters),
        "profile": profile_breakdown(qm),
    }

    # Vector insert / update autocommit
    vec = qm_engine.NativeSqlEngine()
    seed_vector(vec, args.dim, args.rows)

    vec.reset_profile_snapshot()
    next_vec_id = args.rows + 10_000
    update_id = args.rows // 2

    def vec_insert() -> None:
        nonlocal next_vec_id
        next_vec_id += 1
        lit = vec_literal(next_vec_id, args.dim)
        vec.execute(f"INSERT INTO vec_bench (id, embedding) VALUES ({next_vec_id}, '{lit}')")

    def vec_update() -> None:
        nonlocal update_id
        update_id = (update_id + 1) % args.rows
        lit = vec_literal(update_id + 7_000, args.dim)
        vec.execute(f"UPDATE vec_bench SET embedding = '{lit}' WHERE id = {update_id}")

    payload["workloads"]["vector.insert_autocommit"] = {
        "latency": bench_ms(vec_insert, iters=max(10, args.iters // 2)),
        "profile": profile_breakdown(vec),
    }

    vec.reset_profile_snapshot()
    payload["workloads"]["vector.update_autocommit"] = {
        "latency": bench_ms(vec_update, iters=max(10, args.iters // 2)),
        "profile": profile_breakdown(vec),
    }

    with open(args.output, "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
    print(json.dumps(payload, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
