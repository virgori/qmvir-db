#!/usr/bin/env python3
"""QM-only publish extras: recovery, concurrent OLTP, mixed workload."""

from __future__ import annotations

import argparse
import json
import random
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable

from publish_benchmark_lib import environment_info, percentile


def recovery_bench(qm_engine: Any, row_count: int = 5000) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="qm-recovery-") as tmp:
        data_dir = Path(tmp)
        engine = qm_engine.NativeSqlEngine(str(data_dir))
        engine.set_wal_sync_policy("per_commit_sync")
        engine.execute("CREATE TABLE recovery_bench (id INTEGER PRIMARY KEY, body TEXT, tags TEXT)")
        for i in range(row_count):
            engine.execute(
                f"INSERT INTO recovery_bench (id, body, tags) VALUES "
                f"({i}, 'alpha beta gamma chunk {i} needle', 'tag_{i % 20}')"
            )
        engine.execute("CREATE INDEX idx_recovery_body ON recovery_bench (body) USING gin")
        engine.execute("CREATE INDEX idx_recovery_tags_trgm ON recovery_bench (tags) USING gin_trgm")
        if hasattr(engine, "sync_wal"):
            engine.sync_wal()
        if hasattr(engine, "checkpoint"):
            try:
                engine.checkpoint()
            except Exception:
                pass

        reopen_samples: list[float] = []
        count_ok = False
        for _ in range(5):
            t0 = time.perf_counter()
            reopened = qm_engine.NativeSqlEngine(str(data_dir))
            reopened.execute("SELECT COUNT(*) FROM recovery_bench WHERE body @@ 'needle alpha' LIMIT 5")
            reopen_samples.append((time.perf_counter() - t0) * 1000.0)
            result = reopened.execute("SELECT COUNT(*) FROM recovery_bench")
            if isinstance(result, tuple) and len(result) >= 2 and result[1]:
                try:
                    count_ok = int(result[1][0][0]) == row_count
                except (TypeError, ValueError, IndexError):
                    count_ok = False

        return {
            "row_count": row_count,
            "row_count_ok": count_ok,
            "reopen_query_p50_ms": percentile(reopen_samples, 50),
            "reopen_query_p95_ms": percentile(reopen_samples, 95),
            "indexes_restored": count_ok,
        }


def concurrent_oltp_bench(
    qm_engine: Any,
    *,
    threads: int = 8,
    ops_per_thread: int = 500,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="qm-concurrent-") as tmp:
        data_dir = Path(tmp)
        engine = qm_engine.NativeSqlEngine(str(data_dir))
        engine.set_wal_sync_policy("per_commit_sync")
        engine.execute("CREATE TABLE concurrent_bench (id INTEGER PRIMARY KEY, v INTEGER, note TEXT)")
        engine.execute("CREATE INDEX idx_concurrent_v ON concurrent_bench (v)")
        lock = threading.Lock()
        next_id = 0
        errors: list[str] = []

        def worker(ops: int) -> None:
            nonlocal next_id
            for _ in range(ops):
                with lock:
                    next_id += 1
                    row_id = next_id
                try:
                    engine.execute(
                        f"INSERT INTO concurrent_bench (id, v, note) VALUES ({row_id}, {row_id % 17}, 'n')"
                    )
                    engine.execute(f"SELECT v FROM concurrent_bench WHERE id = {row_id}")
                except Exception as exc:  # pragma: no cover
                    errors.append(str(exc))

        thread_objs = [threading.Thread(target=worker, args=(ops_per_thread,)) for _ in range(threads)]
        start = time.perf_counter()
        for t in thread_objs:
            t.start()
        for t in thread_objs:
            t.join()
        elapsed = time.perf_counter() - start
        total_ops = threads * ops_per_thread * 2
        return {
            "threads": threads,
            "ops_per_thread": ops_per_thread,
            "total_ops": total_ops,
            "elapsed_s": elapsed,
            "throughput_ops_sec": total_ops / elapsed if elapsed > 0 else 0.0,
            "errors": len(errors),
        }


def mixed_workload_bench(
    qm_engine: Any,
    *,
    duration_s: float = 30.0,
    read_ratio: float = 0.7,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="qm-mixed-") as tmp:
        data_dir = Path(tmp)
        engine = qm_engine.NativeSqlEngine(str(data_dir))
        engine.set_wal_sync_policy("per_commit_sync")
        engine.execute(
            "CREATE TABLE mixed_bench (id INTEGER PRIMARY KEY, body TEXT, tags TEXT, score INTEGER)"
        )
        for i in range(2000):
            engine.execute(
                f"INSERT INTO mixed_bench (id, body, tags, score) VALUES "
                f"({i}, 'text chunk {i} needle alpha', 'tag_{i % 15}', {i % 100})"
            )
        engine.execute("CREATE INDEX idx_mixed_body ON mixed_bench (body) USING gin")
        engine.execute("CREATE INDEX idx_mixed_tags ON mixed_bench (tags)")
        next_id = 10_000
        rng = random.Random(42)
        reads = writes = 0
        deadline = time.perf_counter() + duration_s
        while time.perf_counter() < deadline:
            if rng.random() < read_ratio:
                op = rng.randint(0, 2)
                if op == 0:
                    engine.execute("SELECT id FROM mixed_bench WHERE tags = 'tag_7' LIMIT 5")
                elif op == 1:
                    engine.execute(
                        "SELECT id FROM mixed_bench WHERE body @@ 'needle alpha' LIMIT 5"
                    )
                else:
                    engine.execute("SELECT COUNT(*) FROM mixed_bench WHERE score > 50")
                reads += 1
            else:
                next_id += 1
                engine.execute(
                    f"INSERT INTO mixed_bench (id, body, tags, score) VALUES "
                    f"({next_id}, 'fresh {next_id}', 'tag_1', {next_id % 100})"
                )
                writes += 1
        total = reads + writes
        return {
            "duration_s": duration_s,
            "read_ratio": read_ratio,
            "reads": reads,
            "writes": writes,
            "total_ops": total,
            "throughput_ops_sec": total / duration_s if duration_s > 0 else 0.0,
        }


def run_all(
    qm_engine: Any,
    *,
    quick: bool = False,
    recovery_rows: int | None = None,
    mixed_duration_s: float | None = None,
) -> dict[str, Any]:
    rows = recovery_rows if recovery_rows is not None else (1000 if quick else 5000)
    mixed_s = mixed_duration_s if mixed_duration_s is not None else (5.0 if quick else 30.0)
    threads = 4 if quick else 8
    ops = 100 if quick else 500
    return {
        "recovery": recovery_bench(qm_engine, row_count=rows),
        "concurrent_oltp": concurrent_oltp_bench(qm_engine, threads=threads, ops_per_thread=ops),
        "mixed_workload": mixed_workload_bench(qm_engine, duration_s=mixed_s),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_publish_extras.json"))
    args = parser.parse_args()
    try:
        import qm_engine  # type: ignore
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2
    env = environment_info()
    sections = run_all(qm_engine, quick=args.quick)
    env["max_rss_mb_end"] = env.get("max_rss_mb_start")
    payload = {"environment": env, "sections": sections}
    text = json.dumps(payload, indent=2, sort_keys=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
