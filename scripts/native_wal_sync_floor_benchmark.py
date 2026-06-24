#!/usr/bin/env python3
"""Measure local WAL flush/fsync floor outside the SQL execution path."""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import statistics
import tempfile
import time
from pathlib import Path
from typing import Any


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def max_rss_mb() -> float:
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if os.sys.platform == "darwin":
        return rss / (1024.0 * 1024.0)
    return rss / 1024.0


def timed_ns(fn) -> int:
    start = time.perf_counter_ns()
    fn()
    return time.perf_counter_ns() - start


def summarize(values_ns: list[int]) -> dict[str, float]:
    values_ms = [v / 1_000_000.0 for v in values_ns]
    return {
        "min_ms": min(values_ms) if values_ms else 0.0,
        "mean_ms": statistics.fmean(values_ms) if values_ms else 0.0,
        "p50_ms": percentile(values_ms, 50),
        "p95_ms": percentile(values_ms, 95),
        "p99_ms": percentile(values_ms, 99),
        "max_ms": max(values_ms) if values_ms else 0.0,
    }


def run_case(path: Path, name: str, payload_size: int, records: int, iterations: int) -> dict[str, Any]:
    serialize_ns: list[int] = []
    write_ns: list[int] = []
    flush_ns: list[int] = []
    sync_ns: list[int] = []
    total_ns: list[int] = []
    payload = b"x" * payload_size

    with path.open("ab", buffering=1024 * 1024) as f:
        for i in range(iterations):
            start_total = time.perf_counter_ns()
            record_holder: list[bytes] = []

            def serialize() -> None:
                if records == 0:
                    return
                prefix = f"{name}:{i}:".encode("ascii")
                record_holder.append(prefix + payload + b"\n")

            serialize_ns.append(timed_ns(serialize))
            record = record_holder[0] if record_holder else b""

            def write_records() -> None:
                for _ in range(records):
                    f.write(record)

            write_ns.append(timed_ns(write_records))
            flush_ns.append(timed_ns(f.flush))
            sync_ns.append(timed_ns(lambda: os.fsync(f.fileno())))
            total_ns.append(time.perf_counter_ns() - start_total)

    bytes_per_iter = records * (payload_size + len(name) + 32)
    return {
        "case": name,
        "payload_size": payload_size,
        "records_per_sync": records,
        "iterations": iterations,
        "approx_bytes_per_sync": bytes_per_iter,
        "serialize": summarize(serialize_ns),
        "write": summarize(write_ns),
        "flush": summarize(flush_ns),
        "sync_all": summarize(sync_ns),
        "total": summarize(total_ns),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=200)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--root", type=Path, default=None)
    args = parser.parse_args()

    root = args.root or Path(tempfile.mkdtemp(prefix="qmvir_wal_sync_floor_"))
    root.mkdir(parents=True, exist_ok=True)
    wal_path = root / "sync_floor.wal"

    cases = [
        ("no_write_sync", 0, 0),
        ("tiny_append_flush_sync", 16, 1),
        ("append_1kb_flush_sync", 1024, 1),
        ("append_4kb_flush_sync", 4096, 1),
        ("append_64kb_flush_sync", 65536, 1),
        ("batch_10_tiny_one_sync", 16, 10),
        ("batch_100_tiny_one_sync", 16, 100),
        ("batch_1000_tiny_one_sync", 16, 1000),
    ]

    results = [run_case(wal_path, name, size, records, args.iterations) for name, size, records in cases]
    report = {
        "label": "WAL_SYNC_FLOOR_MICROBENCH_NOT_SQL_BENCHMARK",
        "iterations": args.iterations,
        "wal_path": str(wal_path),
        "environment": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python_version": platform.python_version(),
            "peak_rss_mb": max_rss_mb(),
        },
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
