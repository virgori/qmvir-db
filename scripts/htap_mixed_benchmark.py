#!/usr/bin/env python3
"""Mixed OLTP+OLAP HTAP benchmark for QMvir vs optional PostgreSQL baseline."""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run_qm_sql(engine_bin: str, data_dir: str, sql: str) -> float:
    t0 = time.perf_counter()
    subprocess.run(
        [engine_bin, "sql", sql, "--data-dir", data_dir],
        check=True,
        capture_output=True,
        text=True,
    )
    return time.perf_counter() - t0


def oltp_worker(
    engine_bin: str, data_dir: str, worker_id: int, n: int, latencies: list[float]
) -> None:
    for i in range(n):
        row_id = worker_id * n + i + 1
        latencies.append(
            run_qm_sql(
                engine_bin,
                data_dir,
                f"INSERT INTO bench (id, val) VALUES ({row_id}, {row_id % 100})",
            )
        )


def olap_worker(engine_bin: str, data_dir: str, n: int, latencies: list[float]) -> None:
    for _ in range(n):
        latencies.append(
            run_qm_sql(
                engine_bin,
                data_dir,
                "SELECT SUM(val) FROM bench WHERE id BETWEEN 1 AND 500",
            )
        )


def main() -> int:
    ap = argparse.ArgumentParser(description="QMvir mixed HTAP benchmark")
    ap.add_argument("--engine-bin", default=os.environ.get("QM_BIN", "qm"))
    ap.add_argument("--oltp-workers", type=int, default=8)
    ap.add_argument("--olap-workers", type=int, default=4)
    ap.add_argument("--ops-per-worker", type=int, default=100)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    with tempfile.TemporaryDirectory(prefix="qm_htap_") as tmp:
        data_dir = tmp
        subprocess.run(
            [
                args.engine_bin,
                "sql",
                "CREATE TABLE bench (id INTEGER PRIMARY KEY, val INTEGER)",
                "--data-dir",
                data_dir,
            ],
            check=True,
            capture_output=True,
        )

        oltp_lat: list[float] = []
        olap_lat: list[float] = []
        threads = []
        t0 = time.perf_counter()
        for w in range(args.oltp_workers):
            threads.append(
                threading.Thread(
                    target=oltp_worker,
                    args=(args.engine_bin, data_dir, w, args.ops_per_worker, oltp_lat),
                )
            )
        for _ in range(args.olap_workers):
            threads.append(
                threading.Thread(
                    target=olap_worker,
                    args=(args.engine_bin, data_dir, args.ops_per_worker // 2, olap_lat),
                )
            )
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        elapsed = time.perf_counter() - t0

        total_ops = len(oltp_lat) + len(olap_lat)
        report = {
            "elapsed_s": round(elapsed, 3),
            "total_ops": total_ops,
            "throughput_ops_s": round(total_ops / elapsed, 1),
            "oltp_p50_ms": round(statistics.median(oltp_lat) * 1000, 2) if oltp_lat else 0,
            "olap_p50_ms": round(statistics.median(olap_lat) * 1000, 2) if olap_lat else 0,
            "oltp_workers": args.oltp_workers,
            "olap_workers": args.olap_workers,
        }
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            print("=== QMvir Mixed HTAP Benchmark ===")
            for k, v in report.items():
                print(f"  {k}: {v}")
        return 0


if __name__ == "__main__":
    sys.exit(main())
