#!/usr/bin/env python3
"""Concurrent PostgreSQL vs NativeSqlEngine group-commit comparison.

This is a durability benchmark for engine-native group_commit_sync. It is not a
single-thread latency claim: the relevant evidence is throughput, sync count,
and group size under concurrent writers.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import multiprocessing
import os
import platform
import resource
import statistics
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable


WORKLOADS = [
    "autocommit_insert",
    "autocommit_update_by_pk",
    "autocommit_delete_by_pk",
    "transaction_insert_10_commit",
    "transaction_insert_100_commit",
    "mixed_dml_100_commit",
]

POSTGRES_CONNECT_TIMEOUT_SECONDS = 2
QM_WORKLOAD_TIMEOUT_SECONDS = 120


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


def summarize_latencies(latencies: list[float], elapsed_s: float) -> dict[str, Any]:
    return {
        "samples": len(latencies),
        "p50_ms": percentile(latencies, 50),
        "p95_ms": percentile(latencies, 95),
        "p99_ms": percentile(latencies, 99),
        "mean_ms": statistics.fmean(latencies) if latencies else 0.0,
        "max_ms": max(latencies) if latencies else 0.0,
        "throughput_ops_sec": len(latencies) / elapsed_s if elapsed_s > 0 else 0.0,
    }


def run_qm_workload(
    qm_engine: Any,
    workload: str,
    concurrency: int,
    operations_per_worker: int,
    sync_policy: str,
    root: Path,
) -> dict[str, Any]:
    data_dir = Path(tempfile.mkdtemp(prefix=f"qm_group_{workload}_{concurrency}_", dir=root))
    engine = qm_engine.NativeSqlEngine(str(data_dir))
    engine.set_wal_sync_policy("per_commit_sync")
    engine.execute(
        "CREATE TABLE gc_bench "
        "(id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, category TEXT, note TEXT)"
    )
    engine.execute("CREATE INDEX idx_gc_bench_score ON gc_bench(score)")
    engine.execute("CREATE INDEX idx_gc_bench_category ON gc_bench(category)")
    seed_count = max(operations_per_worker * concurrency + 1000, 2000)
    engine.set_wal_sync_policy("relaxed_os_buffered")
    for row_id in range(1, seed_count + 1):
        engine.execute(
            "INSERT INTO gc_bench (id, v, score, category, note) "
            f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'c{row_id % 7}', 'seed')"
        )
    engine.sync_wal()
    before_sync = int(engine.wal_sync_count())
    before_group = dict(engine.group_commit_snapshot()) if hasattr(engine, "group_commit_snapshot") else {}
    engine.set_wal_sync_policy(sync_policy)

    def one_op(worker_engine: Any, worker: int, op_index: int) -> None:
        base = 1_000_000 + worker * 1_000_000 + op_index * 1000
        target = (worker * operations_per_worker + op_index) % seed_count + 1
        if workload == "autocommit_insert":
            worker_engine.execute(
                "INSERT INTO gc_bench (id, v, score, category, note) "
                f"VALUES ({base}, {base % 17}, {base % 101}, 'ins', 'insert')"
            )
        elif workload == "autocommit_update_by_pk":
            worker_engine.execute(f"UPDATE gc_bench SET v = v + 1 WHERE id = {target}")
        elif workload == "autocommit_delete_by_pk":
            delete_id = seed_count + 1 + worker * operations_per_worker + op_index
            worker_engine.execute(
                "INSERT INTO gc_bench (id, v, score, category, note) "
                f"VALUES ({delete_id}, 1, 1, 'delete', 'delete')"
            )
            worker_engine.execute(f"DELETE FROM gc_bench WHERE id = {delete_id}")
        elif workload == "transaction_insert_10_commit":
            worker_engine.execute("BEGIN")
            for i in range(10):
                row_id = base + i
                worker_engine.execute(
                    "INSERT INTO gc_bench (id, v, score, category, note) "
                    f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'tx10', 'batch')"
                )
            worker_engine.execute("COMMIT")
        elif workload == "transaction_insert_100_commit":
            worker_engine.execute("BEGIN")
            for i in range(100):
                row_id = base + i
                worker_engine.execute(
                    "INSERT INTO gc_bench (id, v, score, category, note) "
                    f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'tx100', 'batch')"
                )
            worker_engine.execute("COMMIT")
        elif workload == "mixed_dml_100_commit":
            worker_engine.execute("BEGIN")
            for i in range(40):
                row_id = ((target + i - 1) % seed_count) + 1
                worker_engine.execute(
                    f"UPDATE gc_bench SET score = {(row_id + i) % 101} WHERE id = {row_id}"
                )
            for i in range(30):
                row_id = base + i
                worker_engine.execute(
                    "INSERT INTO gc_bench (id, v, score, category, note) "
                    f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'mixed', 'batch')"
                )
            for i in range(30):
                row_id = base + 100 + i
                worker_engine.execute(
                    "INSERT INTO gc_bench (id, v, score, category, note) "
                    f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'temp', 'temp')"
                )
                worker_engine.execute(f"DELETE FROM gc_bench WHERE id = {row_id}")
            worker_engine.execute("COMMIT")
        else:
            raise ValueError(workload)

    latencies: list[float] = []
    failures = 0

    def worker(worker_id: int) -> list[float]:
        nonlocal failures
        worker_engine = engine.new_session() if hasattr(engine, "new_session") else engine
        local: list[float] = []
        for op_index in range(operations_per_worker):
            start = time.perf_counter()
            try:
                one_op(worker_engine, worker_id, op_index)
            except Exception:
                failures += 1
                continue
            local.append((time.perf_counter() - start) * 1000.0)
        return local

    start_all = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        for result in pool.map(worker, range(concurrency)):
            latencies.extend(result)
    elapsed = time.perf_counter() - start_all
    after_sync = int(engine.wal_sync_count())
    after_group = dict(engine.group_commit_snapshot()) if hasattr(engine, "group_commit_snapshot") else {}
    total_groups = int(after_group.get("total_groups", 0)) - int(before_group.get("total_groups", 0))
    total_commits = int(after_group.get("total_commits", 0)) - int(before_group.get("total_commits", 0))
    max_group_size = int(after_group.get("max_group_size", 0))
    avg_group_size = total_commits / total_groups if total_groups else 0.0
    avg_wait_ms = (
        (int(after_group.get("total_wait_ns", 0)) - int(before_group.get("total_wait_ns", 0)))
        / max(1, total_commits)
        / 1_000_000.0
    )
    max_wait_ms = int(after_group.get("max_wait_ns", 0)) / 1_000_000.0
    summary = summarize_latencies(latencies, elapsed)
    summary.update(
        {
            "engine": "qm",
            "workload": workload,
            "concurrency": concurrency,
            "operations_per_worker": operations_per_worker,
            "failed_commits": failures,
            "sync_count": after_sync - before_sync,
            "group_count": total_groups,
            "total_group_commits": total_commits,
            "avg_group_size": avg_group_size,
            "max_group_size": max_group_size,
            "avg_group_wait_ms": avg_wait_ms,
            "max_group_wait_ms": max_wait_ms,
            "wal_bytes": (data_dir / "native_sql.wal").stat().st_size if (data_dir / "native_sql.wal").exists() else 0,
            "data_dir": str(data_dir),
        }
    )
    return summary


def run_qm_workload_child(args: tuple[str, int, int, str, str]) -> dict[str, Any]:
    workload, concurrency, operations_per_worker, sync_policy, root = args
    import qm_engine  # type: ignore

    return run_qm_workload(
        qm_engine,
        workload,
        concurrency,
        operations_per_worker,
        sync_policy,
        Path(root),
    )


def run_qm_workload_isolated(
    workload: str,
    concurrency: int,
    operations_per_worker: int,
    sync_policy: str,
    root: Path,
) -> dict[str, Any]:
    ctx = multiprocessing.get_context("spawn")
    with ctx.Pool(processes=1) as pool:
        result = pool.apply_async(
            run_qm_workload_child,
            ((workload, concurrency, operations_per_worker, sync_policy, str(root)),),
        )
        return result.get(timeout=QM_WORKLOAD_TIMEOUT_SECONDS)


def run_postgres_workload(
    dsn: str,
    workload: str,
    concurrency: int,
    operations_per_worker: int,
) -> dict[str, Any] | None:
    try:
        import psycopg2  # type: ignore
    except Exception:
        return None

    table = f"gc_bench_{workload}_{concurrency}_{int(time.time() * 1000)}"
    try:
        setup_conn = psycopg2.connect(dsn, connect_timeout=POSTGRES_CONNECT_TIMEOUT_SECONDS)
    except Exception:
        return None
    setup_conn.autocommit = True
    with setup_conn.cursor() as cur:
        cur.execute("SHOW fsync")
        fsync = cur.fetchone()[0]
        cur.execute("SHOW synchronous_commit")
        synchronous_commit = cur.fetchone()[0]
        cur.execute(
            f"CREATE TABLE {table} "
            "(id BIGINT PRIMARY KEY, v BIGINT, score BIGINT, category TEXT, note TEXT)"
        )
        cur.execute(f"CREATE INDEX idx_{table}_score ON {table}(score)")
        cur.execute(f"CREATE INDEX idx_{table}_category ON {table}(category)")
        seed_count = max(operations_per_worker * concurrency + 1000, 2000)
        cur.executemany(
            f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
            [
                (row_id, row_id % 17, row_id % 101, f"c{row_id % 7}", "seed")
                for row_id in range(1, seed_count + 1)
            ],
        )
    setup_conn.close()

    latencies: list[float] = []
    failures = 0

    def worker(worker_id: int) -> list[float]:
        nonlocal failures
        conn = psycopg2.connect(dsn, connect_timeout=POSTGRES_CONNECT_TIMEOUT_SECONDS)
        conn.autocommit = True
        local: list[float] = []
        with conn.cursor() as cur:
            cur.execute("SET synchronous_commit = on")
            for op_index in range(operations_per_worker):
                base = 1_000_000 + worker_id * 1_000_000 + op_index * 1000
                target = (worker_id * operations_per_worker + op_index) % seed_count + 1
                start = time.perf_counter()
                try:
                    if workload == "autocommit_insert":
                        cur.execute(
                            f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
                            (base, base % 17, base % 101, "ins", "insert"),
                        )
                    elif workload == "autocommit_update_by_pk":
                        cur.execute(f"UPDATE {table} SET v = v + 1 WHERE id = %s", (target,))
                    elif workload == "autocommit_delete_by_pk":
                        delete_id = seed_count + 1 + worker_id * operations_per_worker + op_index
                        cur.execute(
                            f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
                            (delete_id, 1, 1, "delete", "delete"),
                        )
                        cur.execute(f"DELETE FROM {table} WHERE id = %s", (delete_id,))
                    else:
                        cur.execute("BEGIN")
                        if workload == "transaction_insert_10_commit":
                            n = 10
                            for i in range(n):
                                row_id = base + i
                                cur.execute(
                                    f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
                                    (row_id, row_id % 17, row_id % 101, "tx10", "batch"),
                                )
                        elif workload == "transaction_insert_100_commit":
                            for i in range(100):
                                row_id = base + i
                                cur.execute(
                                    f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
                                    (row_id, row_id % 17, row_id % 101, "tx100", "batch"),
                                )
                        elif workload == "mixed_dml_100_commit":
                            for i in range(40):
                                row_id = ((target + i - 1) % seed_count) + 1
                                cur.execute(
                                    f"UPDATE {table} SET score = %s WHERE id = %s",
                                    ((row_id + i) % 101, row_id),
                                )
                            for i in range(30):
                                row_id = base + i
                                cur.execute(
                                    f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
                                    (row_id, row_id % 17, row_id % 101, "mixed", "batch"),
                                )
                            for i in range(30):
                                row_id = base + 100 + i
                                cur.execute(
                                    f"INSERT INTO {table} (id, v, score, category, note) VALUES (%s, %s, %s, %s, %s)",
                                    (row_id, row_id % 17, row_id % 101, "temp", "temp"),
                                )
                                cur.execute(f"DELETE FROM {table} WHERE id = %s", (row_id,))
                        else:
                            raise ValueError(workload)
                        cur.execute("COMMIT")
                except Exception:
                    failures += 1
                    conn.rollback()
                    continue
                local.append((time.perf_counter() - start) * 1000.0)
        conn.close()
        return local

    start_all = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        for result in pool.map(worker, range(concurrency)):
            latencies.extend(result)
    elapsed = time.perf_counter() - start_all

    cleanup = psycopg2.connect(dsn, connect_timeout=POSTGRES_CONNECT_TIMEOUT_SECONDS)
    cleanup.autocommit = True
    with cleanup.cursor() as cur:
        cur.execute(f"DROP TABLE IF EXISTS {table}")
    cleanup.close()

    summary = summarize_latencies(latencies, elapsed)
    summary.update(
        {
            "engine": "postgresql",
            "workload": workload,
            "concurrency": concurrency,
            "operations_per_worker": operations_per_worker,
            "failed_commits": failures,
            "fsync": fsync,
            "synchronous_commit": synchronous_commit,
        }
    )
    return summary


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dsn", default=os.environ.get("POSTGRES_DSN", "dbname=postgres user=postgres host=/tmp"))
    parser.add_argument("--concurrency", default="1,2,4,8,16")
    parser.add_argument("--operations-per-worker", type=int, default=50)
    parser.add_argument("--iterations", type=int, default=None, help="Alias for --operations-per-worker")
    parser.add_argument("--qm-sync-policy", default="group_commit_sync")
    parser.add_argument("--quick", action="store_true")
    args = parser.parse_args()
    if args.iterations is not None:
        args.operations_per_worker = args.iterations
    normalized_sync_policy = args.qm_sync_policy.replace("-", "_")

    concurrencies = [int(item) for item in args.concurrency.split(",") if item.strip()]
    workloads = WORKLOADS[:3] if args.quick else WORKLOADS
    root = args.output.parent
    root.mkdir(parents=True, exist_ok=True)

    qm_results = []
    pg_results = []
    for workload in workloads:
        for concurrency in concurrencies:
            print(
                f"running qm workload={workload} concurrency={concurrency}",
                file=sys.stderr,
                flush=True,
            )
            qm_results.append(
                run_qm_workload_isolated(
                    workload,
                    concurrency,
                    args.operations_per_worker,
                    args.qm_sync_policy,
                    root,
                )
            )
            print(
                f"checking postgresql workload={workload} concurrency={concurrency}",
                file=sys.stderr,
                flush=True,
            )
            pg = run_postgres_workload(args.dsn, workload, concurrency, args.operations_per_worker)
            if pg is not None:
                pg_results.append(pg)

    report = {
        "label": "GROUP_COMMIT_CONCURRENT_BENCHMARK",
        "schema_version": 1,
        "qm_sync_policy": args.qm_sync_policy,
        "sync_before_commit_return": normalized_sync_policy == "group_commit_sync",
        "acknowledged_before_fsync": normalized_sync_policy != "group_commit_sync",
        "operations_per_worker": args.operations_per_worker,
        "concurrency": concurrencies,
        "workloads": workloads,
        "environment": {
            "os": platform.platform(),
            "machine": platform.machine(),
            "python_version": platform.python_version(),
            "peak_rss_mb": max_rss_mb(),
        },
        "qm_results": qm_results,
        "postgresql_available": bool(pg_results),
        "postgresql_results": pg_results,
    }
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
