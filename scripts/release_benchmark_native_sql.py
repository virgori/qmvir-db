#!/usr/bin/env python3
"""Release benchmark gate for NativeSqlEngine and the Python gateway.

The quick mode is intended for CI smoke gating. The full mode uses more
iterations but keeps the same stable, coarse operations to avoid flaky
microbenchmark claims.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def bench_latency(name: str, iterations: int, fn: Callable[[], Any]) -> dict[str, Any]:
    latencies_ms: list[float] = []
    for _ in range(min(10, iterations)):
        fn()
    start = time.perf_counter()
    for _ in range(iterations):
        op_start = time.perf_counter()
        fn()
        latencies_ms.append((time.perf_counter() - op_start) * 1000.0)
    elapsed = time.perf_counter() - start
    return {
        "name": name,
        "iterations": iterations,
        "p50_ms": percentile(latencies_ms, 50),
        "p95_ms": percentile(latencies_ms, 95),
        "p99_ms": percentile(latencies_ms, 99),
        "mean_ms": statistics.fmean(latencies_ms) if latencies_ms else 0.0,
        "throughput_ops_sec": iterations / elapsed if elapsed > 0 else 0.0,
    }


def max_rss_mb() -> float:
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if sys.platform == "darwin":
        return rss / (1024.0 * 1024.0)
    return rss / 1024.0


def command_version(cmd: list[str]) -> str:
    try:
        result = subprocess.run(cmd, check=False, capture_output=True, text=True, timeout=5)
    except Exception as exc:  # pragma: no cover - environment dependent
        return f"unavailable: {exc}"
    return (result.stdout or result.stderr).strip().splitlines()[0] if result.returncode == 0 else "unavailable"


def environment(build_mode: str, feature_flags: str) -> dict[str, Any]:
    return {
        "os": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "cpu_count": os.cpu_count(),
        "python_version": platform.python_version(),
        "rust_version": command_version(["rustc", "--version"]),
        "build_mode": build_mode,
        "feature_flags": feature_flags,
        "max_rss_mb_start": max_rss_mb(),
    }


def make_engine(qm_engine: Any, data_dir: str | None = None) -> Any:
    return qm_engine.NativeSqlEngine(data_dir) if data_dir is not None else qm_engine.NativeSqlEngine()


def run_native_sql_suite(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    engine = make_engine(qm_engine)
    engine.execute("CREATE TABLE bench_crud (id INTEGER PRIMARY KEY, v INTEGER, name TEXT)")
    counter = 0

    def insert_op() -> None:
        nonlocal counter
        counter += 1
        engine.execute(f"INSERT INTO bench_crud (id, v, name) VALUES ({counter}, {counter % 17}, 'n{counter}')")

    results.append(bench_latency("native_sql.simple_insert", iterations, insert_op))
    existing_id = max(1, counter // 2)
    results.append(
        bench_latency(
            "native_sql.simple_select",
            iterations,
            lambda: engine.execute(f"SELECT name FROM bench_crud WHERE id = {existing_id}"),
        )
    )
    results.append(
        bench_latency(
            "native_sql.simple_update",
            iterations,
            lambda: engine.execute(f"UPDATE bench_crud SET v = v + 1 WHERE id = {existing_id}"),
        )
    )
    delete_counter = counter

    def delete_op() -> None:
        nonlocal delete_counter
        delete_counter += 1
        engine.execute(f"INSERT INTO bench_crud (id, v, name) VALUES ({delete_counter}, 1, 'd')")
        engine.execute(f"DELETE FROM bench_crud WHERE id = {delete_counter}")

    results.append(bench_latency("native_sql.simple_delete", max(1, iterations // 2), delete_op))
    results.append(
        bench_latency(
            "native_sql.predicate_path",
            iterations,
            lambda: engine.execute("SELECT COUNT(*) FROM bench_crud WHERE v IN (1, 3, 5) AND id > 0"),
        )
    )

    mvcc_engine = make_engine(qm_engine)
    mvcc_engine.execute("CREATE TABLE bench_mvcc (id INTEGER PRIMARY KEY, v INTEGER)")
    mvcc_id = 0

    def mvcc_rw_op() -> None:
        nonlocal mvcc_id
        mvcc_id += 1
        mvcc_engine.execute("BEGIN")
        mvcc_engine.execute(f"INSERT INTO bench_mvcc (id, v) VALUES ({mvcc_id}, {mvcc_id})")
        mvcc_engine.execute(f"SELECT v FROM bench_mvcc WHERE id = {mvcc_id}")
        mvcc_engine.execute("COMMIT")

    results.append(bench_latency("native_sql.mvcc_read_write", max(1, iterations // 2), mvcc_rw_op))

    concurrent_engine = make_engine(qm_engine)
    concurrent_engine.execute("CREATE TABLE bench_concurrent (id INTEGER PRIMARY KEY, v INTEGER)")
    lock = threading.Lock()
    next_id = 0

    def worker(count: int) -> None:
        nonlocal next_id
        for _ in range(count):
            with lock:
                next_id += 1
                row_id = next_id
            concurrent_engine.execute(f"INSERT INTO bench_concurrent (id, v) VALUES ({row_id}, {row_id % 11})")
            concurrent_engine.execute("SELECT COUNT(*) FROM bench_concurrent WHERE v >= 0")

    thread_count = 2 if iterations < 200 else 4
    per_thread = max(5, iterations // (thread_count * 4))
    start = time.perf_counter()
    threads = [threading.Thread(target=worker, args=(per_thread,)) for _ in range(thread_count)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    elapsed = time.perf_counter() - start
    results.append(
        {
            "name": "native_sql.concurrent_read_write_smoke",
            "iterations": thread_count * per_thread * 2,
            "p50_ms": None,
            "p95_ms": None,
            "p99_ms": None,
            "mean_ms": None,
            "throughput_ops_sec": (thread_count * per_thread * 2) / elapsed if elapsed > 0 else 0.0,
        }
    )

    vector_engine = make_engine(qm_engine)
    vector_engine.execute("CREATE TABLE bench_vec (id INTEGER PRIMARY KEY, embedding VECTOR(3))")
    for i in range(1, 33):
        vector_engine.execute(f"INSERT INTO bench_vec (id, embedding) VALUES ({i}, '[{i / 10.0},0.1,0.2]')")
    results.append(
        bench_latency(
            "native_sql.vector_cache_hot_path",
            max(1, iterations // 2),
            lambda: vector_engine.execute("SELECT id FROM bench_vec ORDER BY embedding <-> '[0.1,0.1,0.2]' LIMIT 5"),
        )
    )
    return results


def run_gateway_suite(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    if not hasattr(qm_engine, "PostgresGateway"):
        return [{"name": "python_gateway.startup_shutdown", "skipped": "PostgresGateway unavailable"}]

    port_base = 58433
    current = 0

    def lifecycle() -> None:
        nonlocal current
        current += 1
        port = port_base + (current % 200)
        gw = qm_engine.PostgresGateway("127.0.0.1", port)

        def execute(_sql: str) -> tuple[list[str], list[int], list[list[str | None]]]:
            return (["ok"], [25], [["1"]])

        gw.start(execute)
        gw.stop()

    return [bench_latency("python_gateway.startup_shutdown", max(3, min(iterations, 25)), lifecycle)]


def compare_baseline(results: list[dict[str, Any]], baseline_path: Path | None) -> dict[str, Any]:
    if baseline_path is None or not baseline_path.exists():
        return {"baseline_path": str(baseline_path) if baseline_path else None, "available": False}
    baseline = json.loads(baseline_path.read_text())
    by_name = {item["name"]: item for item in baseline.get("results", [])}
    comparisons = []
    for item in results:
        base = by_name.get(item["name"])
        if not base or item.get("throughput_ops_sec") in (None, 0):
            continue
        base_tp = base.get("throughput_ops_sec") or 0
        if base_tp:
            comparisons.append(
                {
                    "name": item["name"],
                    "throughput_ratio_vs_baseline": item["throughput_ops_sec"] / base_tp,
                    "baseline_throughput_ops_sec": base_tp,
                }
            )
    return {"baseline_path": str(baseline_path), "available": True, "comparisons": comparisons}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--quick", action="store_true", help="short CI smoke benchmark")
    parser.add_argument("--iterations", type=int, default=None)
    parser.add_argument("--baseline", type=Path, default=Path("docs/native_sql_benchmark_baseline.json"))
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--build-mode", default="unknown")
    parser.add_argument("--feature-flags", default="default")
    args = parser.parse_args()

    try:
        import qm_engine  # type: ignore
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2

    iterations = args.iterations if args.iterations is not None else (50 if args.quick else 1000)
    env = environment(args.build_mode, args.feature_flags)
    with tempfile.TemporaryDirectory(prefix="qm-release-bench-"):
        results = run_native_sql_suite(qm_engine, iterations)
        results.extend(run_gateway_suite(qm_engine, iterations))
    env["max_rss_mb_end"] = max_rss_mb()
    env["peak_rss_mb"] = env["max_rss_mb_end"]
    report = {
        "schema_version": 1,
        "mode": "quick" if args.quick else "full",
        "environment": env,
        "results": results,
        "baseline": compare_baseline(results, args.baseline),
    }

    text = json.dumps(report, indent=2, sort_keys=True)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
