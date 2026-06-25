#!/usr/bin/env python3
"""QM-only durable write profiler for NativeSqlEngine.

This is a calibration aid, not a release benchmark and not a PostgreSQL
comparison. It uses persistent NativeSqlEngine WAL with per-commit sync.
"""

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
from typing import Any, Callable


PROFILE_KEYS = [
    "parse_ns",
    "plan_ns",
    "execute_ns",
    "sql_dispatch_ns",
    "tx_begin_ns",
    "tx_stage_ns",
    "tx_commit_ns",
    "tx_rollback_ns",
    "row_lookup_ns",
    "pk_lookup_ns",
    "row_clone_ns",
    "wal_encode_ns",
    "wal_lock_wait_ns",
    "wal_write_ns",
    "wal_flush_ns",
    "wal_sync_ns",
    "wal_sync_all_ns",
    "wal_sync_data_ns",
    "checkpoint_ns",
    "index_update_ns",
    "table_scan_ns",
    "table_rewrite_count",
    "index_rebuild_count",
    "checkpoint_count",
    "wal_append_count",
    "wal_batch_append_count",
    "wal_bytes",
    "sync_all_count",
    "sync_data_count",
    "flush_count",
    "pk_lookup_count",
    "row_undo_count",
    "wal_record_count",
    "tx_sync_count",
]


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


def make_engine(qm_engine: Any, root: Path, name: str, sync_policy: str) -> Any:
    data_dir = Path(tempfile.mkdtemp(prefix=f"{name}_", dir=root))
    engine = qm_engine.NativeSqlEngine(str(data_dir))
    engine.set_wal_sync_policy(sync_policy)
    return engine


def empty_profile() -> dict[str, int]:
    return {key: 0 for key in PROFILE_KEYS}


def normalize_profile(snapshot: dict[str, Any]) -> dict[str, int]:
    out = empty_profile()
    for key in PROFILE_KEYS:
        value = snapshot.get(key, 0)
        if isinstance(value, bool):
            out[key] = int(value)
        elif isinstance(value, (int, float)):
            out[key] = int(value)
    return out


def merge_profile(target: dict[str, int], snapshot: dict[str, Any]) -> None:
    normalized = normalize_profile(snapshot)
    for key in PROFILE_KEYS:
        target[key] += normalized[key]


class WorkloadState:
    def __init__(self) -> None:
        self.next_id = 1

    def alloc_id(self) -> int:
        value = self.next_id
        self.next_id += 1
        return value


def create_table(engine: Any) -> None:
    engine.execute(
        "CREATE TABLE write_profile "
        "(id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, category TEXT, note TEXT)"
    )
    engine.execute("CREATE INDEX idx_write_profile_v ON write_profile (v)")
    engine.execute("CREATE INDEX idx_write_profile_category ON write_profile (category)")


def seed_rows(engine: Any, state: WorkloadState, count: int) -> None:
    for _ in range(count):
        row_id = state.alloc_id()
        engine.execute(
            "INSERT INTO write_profile (id, v, score, category, note) "
            f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'c{row_id % 7}', 'seed{row_id}')"
        )


def op_insert(engine: Any, state: WorkloadState) -> None:
    row_id = state.alloc_id()
    engine.execute(
        "INSERT INTO write_profile (id, v, score, category, note) "
        f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'c{row_id % 7}', 'insert{row_id}')"
    )


def op_update_by_pk(engine: Any, state: WorkloadState) -> None:
    row_id = ((state.alloc_id() - 1) % 500) + 1
    engine.execute(
        f"UPDATE write_profile SET v = {(row_id + state.next_id) % 23}, "
        f"score = {(row_id + 3) % 101}, category = 'u{row_id % 5}' WHERE id = {row_id}"
    )


def op_delete_by_pk(engine: Any, state: WorkloadState) -> None:
    row_id = state.alloc_id()
    engine.execute(f"DELETE FROM write_profile WHERE id = {row_id}")


def op_transaction_commit(engine: Any, state: WorkloadState) -> None:
    row_id = state.alloc_id()
    engine.execute("BEGIN")
    engine.execute(
        "INSERT INTO write_profile (id, v, score, category, note) "
        f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'tc', 'commit{row_id}')"
    )
    engine.execute("COMMIT")


def op_transaction_insert_n(engine: Any, state: WorkloadState, n: int) -> None:
    engine.execute("BEGIN")
    for _ in range(n):
        row_id = state.alloc_id()
        engine.execute(
            "INSERT INTO write_profile (id, v, score, category, note) "
            f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'b{row_id % 7}', 'batch{row_id}')"
        )
    engine.execute("COMMIT")


def op_transaction_mixed_dml_100(engine: Any, state: WorkloadState) -> None:
    engine.execute("BEGIN")
    for i in range(40):
        row_id = ((state.next_id + i) % 500) + 1
        engine.execute(f"UPDATE write_profile SET score = {(row_id + i) % 101} WHERE id = {row_id}")
    for _ in range(30):
        row_id = state.alloc_id()
        engine.execute(
            "INSERT INTO write_profile (id, v, score, category, note) "
            f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'm{row_id % 5}', 'mixed{row_id}')"
        )
    for i in range(30):
        row_id = state.alloc_id()
        engine.execute(
            "INSERT INTO write_profile (id, v, score, category, note) "
            f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'x{row_id % 3}', 'temp{row_id}')"
        )
        engine.execute(f"DELETE FROM write_profile WHERE id = {row_id}")
    engine.execute("COMMIT")


def op_transaction_rollback_100(engine: Any, state: WorkloadState) -> None:
    engine.execute("BEGIN")
    for _ in range(100):
        row_id = state.alloc_id()
        engine.execute(
            "INSERT INTO write_profile (id, v, score, category, note) "
            f"VALUES ({row_id}, {row_id % 17}, {row_id % 101}, 'r{row_id % 7}', 'rollback{row_id}')"
        )
    engine.execute("ROLLBACK")


def workload_fn(name: str) -> Callable[[Any, WorkloadState], None]:
    if name == "insert":
        return op_insert
    if name == "update_by_pk":
        return op_update_by_pk
    if name == "delete_by_pk":
        return op_delete_by_pk
    if name == "transaction_commit":
        return op_transaction_commit
    if name == "transaction_insert_10_commit":
        return lambda engine, state: op_transaction_insert_n(engine, state, 10)
    if name == "transaction_insert_100_commit":
        return lambda engine, state: op_transaction_insert_n(engine, state, 100)
    if name == "transaction_insert_1000_commit":
        return lambda engine, state: op_transaction_insert_n(engine, state, 1000)
    if name == "transaction_mixed_dml_100_commit":
        return op_transaction_mixed_dml_100
    if name == "transaction_rollback_100":
        return op_transaction_rollback_100
    raise ValueError(f"unknown workload {name}")


def effective_iterations(name: str, requested: int) -> int:
    caps = {
        "transaction_insert_100_commit": 10,
        "transaction_insert_1000_commit": 3,
        "transaction_mixed_dml_100_commit": 10,
        "transaction_rollback_100": 10,
    }
    return min(requested, caps.get(name, requested))


def run_one(
    qm_engine: Any,
    root: Path,
    name: str,
    iterations: int,
    warmup: int,
    repeat: int,
    sync_policy: str,
) -> dict[str, Any]:
    latencies: list[float] = []
    profile_total = empty_profile()
    op = workload_fn(name)
    measured_iterations = effective_iterations(name, iterations)

    for rep in range(repeat):
        engine = make_engine(qm_engine, root, f"{name}_r{rep}", sync_policy)
        state = WorkloadState()
        create_table(engine)
        seed_count = 600
        if name == "delete_by_pk":
            seed_count = warmup + measured_iterations + 10
        seed_rows(engine, state, seed_count)
        if name == "delete_by_pk":
            state.next_id = 1
        for _ in range(warmup):
            op(engine, state)
        if hasattr(engine, "reset_profile_snapshot"):
            engine.reset_profile_snapshot()
        for _ in range(measured_iterations):
            start = time.perf_counter()
            op(engine, state)
            latencies.append((time.perf_counter() - start) * 1000.0)
        if hasattr(engine, "profile_snapshot"):
            merge_profile(profile_total, engine.profile_snapshot())

    return {
        "label": "CALIBRATION_ONLY_NOT_RELEASE_CLAIM",
        "workload": name,
        "iterations": iterations,
        "effective_iterations": measured_iterations,
        "warmup": warmup,
        "repeat": repeat,
        "samples": len(latencies),
        "p50_ms": percentile(latencies, 50),
        "p95_ms": percentile(latencies, 95),
        "p99_ms": percentile(latencies, 99),
        "mean_ms": statistics.fmean(latencies) if latencies else 0.0,
        "profile": profile_total,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=50)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--trace", action="store_true")
    parser.add_argument(
        "--sync-policy",
        choices=["per_commit", "per_commit_sync", "per_commit_sync_data", "relaxed_os_buffered"],
        default="per_commit",
    )
    args = parser.parse_args()

    if args.trace:
        os.environ["QMVIR_WAL_TRACE"] = "1"
        os.environ["QMVIR_NATIVE_SQL_PROFILE"] = "1"

    import qm_engine  # type: ignore

    root = args.output.parent
    root.mkdir(parents=True, exist_ok=True)
    workloads = [
        "insert",
        "update_by_pk",
        "delete_by_pk",
        "transaction_commit",
        "transaction_insert_10_commit",
        "transaction_insert_100_commit",
        "transaction_insert_1000_commit",
        "transaction_mixed_dml_100_commit",
        "transaction_rollback_100",
    ]
    results = [
        run_one(qm_engine, root, name, args.iterations, args.warmup, args.repeat, args.sync_policy)
        for name in workloads
    ]
    report = {
        "label": "CALIBRATION_ONLY_NOT_RELEASE_CLAIM",
        "schema_version": 1,
        "mode": f"qm_only_persistent_wal_{args.sync_policy}",
        "sync_policy": args.sync_policy,
        "acknowledged_before_fsync": False,
        "sync_before_commit_return": True,
        "trace_enabled": bool(args.trace),
        "environment": {
            "os": platform.platform(),
            "machine": platform.machine(),
            "python_version": platform.python_version(),
            "peak_rss_mb": max_rss_mb(),
        },
        "results": results,
    }
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
