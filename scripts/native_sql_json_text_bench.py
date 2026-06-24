#!/usr/bin/env python3
"""Native SQL JSON, text, and vector ORDER BY microbenchmarks."""

from __future__ import annotations

import argparse
import json
import platform
import statistics
import time
from pathlib import Path
from typing import Any, Callable


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def bench(name: str, iterations: int, fn: Callable[[], Any]) -> dict[str, Any]:
    for _ in range(min(10, iterations)):
        fn()
    samples: list[float] = []
    start = time.perf_counter()
    for _ in range(iterations):
        t0 = time.perf_counter()
        fn()
        samples.append((time.perf_counter() - t0) * 1000.0)
    elapsed = time.perf_counter() - start
    return {
        "name": name,
        "iterations": iterations,
        "p50_ms": percentile(samples, 50),
        "p95_ms": percentile(samples, 95),
        "mean_ms": statistics.fmean(samples) if samples else 0.0,
        "throughput_ops_sec": iterations / elapsed if elapsed > 0 else 0.0,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=200)
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_native_sql_json_text_linux.json"))
    args = parser.parse_args()

    import qm_engine  # type: ignore

    engine = qm_engine.NativeSqlEngine()
    results: list[dict[str, Any]] = []

    engine.execute(
        "CREATE TABLE json_bench (id INTEGER PRIMARY KEY, data JSON, tags TEXT, body TEXT)"
    )
    engine.execute("CREATE INDEX json_bench_tags ON json_bench (tags)")
    for i in range(500):
        engine.execute(
            f"INSERT INTO json_bench (id, data, tags, body) VALUES "
            f"({i}, '{{\"name\":\"user{i}\",\"score\":{i % 100}}}', 'tag_{i % 20}', "
            f"'alpha beta gamma text chunk {i} needle')"
        )

    next_id = 100000
    def json_insert() -> None:
        nonlocal next_id
        next_id += 1
        engine.execute(
            f"INSERT INTO json_bench (id, data, tags, body) VALUES "
            f"({next_id}, '{{\"name\":\"new\"}}', 'tag_1', 'fresh text')"
        )

    results.append(
        bench(
            "json.insert_autocommit",
            max(50, args.iterations // 4),
            json_insert,
        )
    )
    results.append(
        bench(
            "json.extract_path_text_filter",
            args.iterations,
            lambda: engine.execute(
                "SELECT id FROM json_bench "
                "WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'user42'"
            ),
        )
    )
    results.append(
        bench(
            "text.indexed_equality",
            args.iterations,
            lambda: engine.execute("SELECT id FROM json_bench WHERE tags = 'tag_7'"),
        )
    )
    results.append(
        bench(
            "text.like_contains",
            args.iterations,
            lambda: engine.execute("SELECT id FROM json_bench WHERE body LIKE '%needle%'"),
        )
    )

    vec = qm_engine.NativeSqlEngine()
    dim = 32
    vec.execute(f"CREATE TABLE vec_bench (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    for i in range(1000):
        literal = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
        vec.execute(f"INSERT INTO vec_bench (id, embedding) VALUES ({i}, '{literal}')")
    query = "[" + ",".join(f"{0.1 + j * 0.01:.4f}" for j in range(dim)) + "]"

    results.append(
        bench(
            "vector.order_by_l2_top10",
            max(30, args.iterations // 2),
            lambda: vec.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <-> '{query}' LIMIT 10"
            ),
        )
    )
    results.append(
        bench(
            "vector.order_by_cosine_top10",
            max(30, args.iterations // 2),
            lambda: vec.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <=> '{query}' LIMIT 10"
            ),
        )
    )

    bm25_docs = [(i, f"alpha beta text chunk {i} needle") for i in range(2000)]
    results.append(
        bench(
            "text.bm25_compact_top10",
            max(30, args.iterations // 2),
            lambda: engine.search_bm25_compact(bm25_docs, "needle alpha", 10),
        )
    )
    bm25_scores = [(i, float(i % 17)) for i in range(200)]
    vector_scores = [(i, float((200 - i) % 23)) for i in range(200)]
    results.append(
        bench(
            "text.hybrid_compact_top10",
            max(30, args.iterations // 2),
            lambda: engine.search_hybrid_compact(bm25_scores, vector_scores, 0.5, 10),
        )
    )

    payload = {
        "environment": {"os": platform.platform(), "python": platform.python_version()},
        "iterations_requested": args.iterations,
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    print(f"wrote {args.output}")


if __name__ == "__main__":
    main()
