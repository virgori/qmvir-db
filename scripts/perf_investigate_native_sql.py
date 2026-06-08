#!/usr/bin/env python3
"""Full local performance investigation for QM NativeSqlEngine.

The release benchmark is deliberately small and stable. This script is broader:
it exercises CRUD, predicates, MVCC-adjacent paths, secondary indexes,
WAL/checkpoint/recovery, pgwire gateway overhead, vector/cache paths, and Python
MVCC conflict detection. It records enough metadata to identify bottlenecks
without making public performance claims.
"""

from __future__ import annotations

import argparse
import cProfile
import io
import json
import os
import platform
import pstats
import resource
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def max_rss_mb() -> float:
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if sys.platform == "darwin":
        return rss / (1024.0 * 1024.0)
    return rss / 1024.0


def command_version(cmd: list[str]) -> str:
    try:
        result = subprocess.run(cmd, check=False, capture_output=True, text=True, timeout=5)
    except Exception as exc:
        return f"unavailable: {exc}"
    text = (result.stdout or result.stderr).strip().splitlines()
    return text[0] if result.returncode == 0 and text else "unavailable"


def memory_total() -> str:
    if sys.platform == "darwin":
        try:
            out = subprocess.check_output(
                ["sysctl", "-n", "hw.memsize"],
                text=True,
                stderr=subprocess.DEVNULL,
                timeout=5,
            )
            return f"{int(out.strip()) / (1024 ** 3):.2f} GiB"
        except Exception:
            return "unavailable"
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                kb = int(line.split()[1])
                return f"{kb / (1024 ** 2):.2f} GiB"
    except Exception:
        pass
    return "unavailable"


def bench_latency(
    name: str,
    iterations: int,
    fn: Callable[[], Any],
    *,
    warmup: int = 10,
    category: str,
) -> dict[str, Any]:
    latencies_ms: list[float] = []
    errors = 0
    for _ in range(min(warmup, iterations)):
        try:
            fn()
        except Exception:
            errors += 1
    rss_start = max_rss_mb()
    start = time.perf_counter()
    for _ in range(iterations):
        op_start = time.perf_counter()
        try:
            fn()
        except Exception:
            errors += 1
        latencies_ms.append((time.perf_counter() - op_start) * 1000.0)
    elapsed = time.perf_counter() - start
    rss_end = max_rss_mb()
    return {
        "name": name,
        "category": category,
        "iterations": iterations,
        "warmup_iterations": min(warmup, iterations),
        "p50_ms": percentile(latencies_ms, 50),
        "p95_ms": percentile(latencies_ms, 95),
        "p99_ms": percentile(latencies_ms, 99),
        "min_ms": min(latencies_ms) if latencies_ms else None,
        "max_ms": max(latencies_ms) if latencies_ms else None,
        "mean_ms": statistics.fmean(latencies_ms) if latencies_ms else None,
        "throughput_ops_sec": iterations / elapsed if elapsed > 0 else 0.0,
        "duration_sec": elapsed,
        "peak_rss_mb": rss_end,
        "rss_delta_mb": rss_end - rss_start,
        "error_count": errors,
    }


def timed_once(name: str, fn: Callable[[], Any], *, category: str) -> dict[str, Any]:
    rss_start = max_rss_mb()
    start = time.perf_counter()
    errors = 0
    try:
        fn()
    except Exception:
        errors = 1
    elapsed = time.perf_counter() - start
    rss_end = max_rss_mb()
    ms = elapsed * 1000.0
    return {
        "name": name,
        "category": category,
        "iterations": 1,
        "warmup_iterations": 0,
        "p50_ms": ms,
        "p95_ms": ms,
        "p99_ms": ms,
        "min_ms": ms,
        "max_ms": ms,
        "mean_ms": ms,
        "throughput_ops_sec": 1.0 / elapsed if elapsed > 0 else 0.0,
        "duration_sec": elapsed,
        "peak_rss_mb": rss_end,
        "rss_delta_mb": rss_end - rss_start,
        "error_count": errors,
    }


def skipped_result(name: str, category: str, reason: str) -> dict[str, Any]:
    return {
        "name": name,
        "category": category,
        "skipped": reason,
        "iterations": 0,
        "warmup_iterations": 0,
        "p50_ms": None,
        "p95_ms": None,
        "p99_ms": None,
        "min_ms": None,
        "max_ms": None,
        "mean_ms": None,
        "throughput_ops_sec": None,
        "duration_sec": 0.0,
        "peak_rss_mb": max_rss_mb(),
        "rss_delta_mb": 0.0,
        "error_count": 0,
    }


def run_core_direct(iterations: int) -> list[dict[str, Any]]:
    output = Path(tempfile.gettempdir()) / f"qm_native_sql_core_bench_{os.getpid()}.json"
    cmd = [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        str(PROJECT_ROOT / "qm_engine" / "Cargo.toml"),
        "--release",
        "--no-default-features",
        "--bin",
        "native_sql_core_bench",
        "--",
        "--iterations",
        str(iterations),
        "--output",
        str(output),
    ]
    try:
        result = subprocess.run(cmd, cwd=PROJECT_ROOT, check=False, capture_output=True, text=True, timeout=180)
        if result.returncode == 0 and output.exists():
            data = json.loads(output.read_text())
            return data.get("results", [])
        reason = (result.stderr or result.stdout or "core harness failed").strip().splitlines()[-1]
    except Exception as exc:
        reason = str(exc)
    finally:
        try:
            output.unlink()
        except FileNotFoundError:
            pass
    return [
        skipped_result(name, "core_direct", f"direct Rust core harness unavailable: {reason}")
        for name in [
            "core.noop",
            "core.table_lookup_by_id",
            "core.row_insert_direct_no_index",
            "core.row_select_direct_by_id",
            "core.row_update_direct_by_id",
            "core.row_delete_direct_by_id",
            "core.index_lookup_numeric_direct",
            "core.index_lookup_string_direct",
            "core.index_lookup_string_borrowed_direct",
            "string_index.lookup_borrowed_key",
            "core.mvcc_visibility_check_direct",
            "core.vector_distance_l2_direct",
            "core.vector_distance_cosine_direct",
            "core.vector_distance_ip_direct",
        ]
    ]


def execute(engine: Any, sql: str) -> Any:
    return engine.execute(sql)


def make_engine(qm_engine: Any, data_dir: str | None = None) -> Any:
    return qm_engine.NativeSqlEngine(data_dir) if data_dir is not None else qm_engine.NativeSqlEngine()


def checkpoint_profile(engine: Any) -> dict[str, Any] | None:
    getter = getattr(engine, "checkpoint_profile", None)
    if getter is None:
        return None
    try:
        return dict(getter())
    except Exception as exc:
        return {"error": str(exc)}


def with_checkpoint_profile(result: dict[str, Any], engine: Any) -> dict[str, Any]:
    profile = checkpoint_profile(engine)
    if profile is not None:
        result["checkpoint_profile_last"] = profile
    return result


def seed_rows(engine: Any, table: str, count: int) -> None:
    execute(engine, f"CREATE TABLE {table} (id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, name TEXT, category TEXT)")
    for i in range(1, count + 1):
        execute(
            engine,
            f"INSERT INTO {table} (id, v, score, name, category) VALUES "
            f"({i}, {i % 17}, {i % 101}, 'name_{i % 100}', 'cat_{i % 9}')",
        )


def run_layer_decomposition(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []

    if not hasattr(qm_engine.NativeSqlEngine(), "prepare"):
        prepared_reason = "installed qm_engine wheel does not expose prepare/execute_prepared yet"
        for name in [
            "prepared.insert_one",
            "prepared.select_by_pk",
            "prepared.update_by_pk",
            "prepared.delete_by_pk",
            "prepared.predicate_or",
            "prepared.indexed_string_equality",
            "prepared.count_compiled_predicate",
            "prepared.vector_exact_search",
        ]:
            results.append(skipped_result(name, "prepared_plan", prepared_reason))
    else:
        prep_engine = make_engine(qm_engine)
        execute(prep_engine, "CREATE TABLE layer_prepared (id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, name TEXT, category TEXT)")
        execute(prep_engine, "CREATE INDEX idx_layer_prepared_score ON layer_prepared(score)")
        execute(prep_engine, "CREATE INDEX idx_layer_prepared_category ON layer_prepared(category)")
        prepared_insert = prep_engine.prepare("INSERT INTO layer_prepared (id, v, score, name, category) VALUES ($1, $2, $3, $4, $5)")
        prepared_select = prep_engine.prepare("SELECT name FROM layer_prepared WHERE id = $1")
        prepared_update = prep_engine.prepare("UPDATE layer_prepared SET v = $1 WHERE id = $2")
        prepared_delete = prep_engine.prepare("DELETE FROM layer_prepared WHERE id = $1")
        next_prepared_id = 0

        def prepared_insert_one() -> None:
            nonlocal next_prepared_id
            next_prepared_id += 1
            prep_engine.execute_prepared(
                prepared_insert,
                [
                    str(next_prepared_id),
                    str(next_prepared_id % 17),
                    str(next_prepared_id % 101),
                    f"name_{next_prepared_id % 100}",
                    f"cat_{next_prepared_id % 9}",
                ],
            )

        results.append(bench_latency("prepared.insert_one", iterations, prepared_insert_one, category="prepared_plan"))
        target_id = max(1, next_prepared_id // 2)
        results.append(bench_latency("prepared.select_by_pk", iterations, lambda: prep_engine.execute_prepared(prepared_select, [str(target_id)]), category="prepared_plan"))
        results.append(bench_latency("prepared.update_by_pk", iterations, lambda: prep_engine.execute_prepared(prepared_update, ["999", str(target_id)]), category="prepared_plan"))

        delete_id = next_prepared_id + 10_000

        def prepared_delete_one() -> None:
            nonlocal delete_id
            delete_id += 1
            prep_engine.execute_prepared(
                prepared_insert,
                [str(delete_id), "1", "1", "delete", "cat_delete"],
            )
            prep_engine.execute_prepared(prepared_delete, [str(delete_id)])

        results.append(bench_latency("prepared.delete_by_pk", max(1, iterations // 2), prepared_delete_one, category="prepared_plan"))
        prepared_or = prep_engine.prepare("SELECT COUNT(*) FROM layer_prepared WHERE score = 10 OR score = 20")
        prepared_string = prep_engine.prepare("SELECT id FROM layer_prepared WHERE category = $1")
        prepared_count = prep_engine.prepare("SELECT COUNT(*) FROM layer_prepared WHERE score IN (1, 3) AND id > 10")
        results.extend(
            [
                bench_latency("prepared.predicate_or", iterations, lambda: prep_engine.execute_prepared(prepared_or, []), category="prepared_plan"),
                bench_latency("prepared.indexed_string_equality", iterations, lambda: prep_engine.execute_prepared(prepared_string, ["cat_3"]), category="prepared_plan"),
                bench_latency("prepared.count_compiled_predicate", iterations, lambda: prep_engine.execute_prepared(prepared_count, []), category="prepared_plan"),
                skipped_result("prepared.vector_exact_search", "prepared_plan", "prepared vector exact-search plan is not implemented yet"),
            ]
        )
        with tempfile.TemporaryDirectory(prefix="qm-prepared-persist-perf-") as data_dir:
            persistent = make_engine(qm_engine, data_dir)
            execute(persistent, "CREATE TABLE layer_prepared_persist (id INTEGER PRIMARY KEY, v INTEGER, name TEXT)")
            persistent_insert = persistent.prepare("INSERT INTO layer_prepared_persist (id, v, name) VALUES ($1, $2, $3)")
            persistent_next = 0

            def prepared_persistent_insert() -> None:
                nonlocal persistent_next
                persistent_next += 1
                persistent.execute_prepared(
                    persistent_insert,
                    [str(persistent_next), str(persistent_next % 17), f"persist_{persistent_next}"],
                )

            results.append(bench_latency("prepared.persistent_autocommit_insert", max(1, iterations // 2), prepared_persistent_insert, category="prepared_plan"))
            results.append(timed_once("prepared.persistent_reopen_count", lambda: make_engine(qm_engine, data_dir).execute("SELECT COUNT(*) FROM layer_prepared_persist"), category="prepared_plan"))

        tx_engine = make_engine(qm_engine)
        execute(tx_engine, "CREATE TABLE layer_prepared_tx (id INTEGER PRIMARY KEY, v INTEGER, name TEXT)")
        execute(tx_engine, "INSERT INTO layer_prepared_tx (id, v, name) VALUES (1, 1, 'base')")
        tx_insert = tx_engine.prepare("INSERT INTO layer_prepared_tx (id, v, name) VALUES ($1, $2, $3)")
        tx_update = tx_engine.prepare("UPDATE layer_prepared_tx SET v = $1 WHERE id = $2")
        tx_delete = tx_engine.prepare("DELETE FROM layer_prepared_tx WHERE id = $1")
        tx_next = 10_000

        def prepared_tx_rollback() -> None:
            nonlocal tx_next
            tx_next += 1
            execute(tx_engine, "BEGIN")
            tx_engine.execute_prepared(tx_insert, [str(tx_next), "7", "rollback"])
            tx_engine.execute_prepared(tx_update, ["9", "1"])
            tx_engine.execute_prepared(tx_delete, [str(tx_next)])
            execute(tx_engine, "ROLLBACK")

        results.append(bench_latency("prepared.transaction_rollback_roundtrip", max(1, iterations // 2), prepared_tx_rollback, category="prepared_plan"))

    results.append(bench_latency("python.noop_boundary", iterations, lambda: None, category="python_boundary"))

    engine = make_engine(qm_engine)
    execute(engine, "CREATE TABLE layer_sql (id INTEGER PRIMARY KEY, v INTEGER, name TEXT)")
    for i in range(1, 101):
        execute(engine, f"INSERT INTO layer_sql (id, v, name) VALUES ({i}, {i}, 'n{i}')")

    results.extend(
        [
            bench_latency("python.execute_sql_noop", iterations, lambda: execute(engine, "SELECT 1"), category="python_boundary"),
            bench_latency("python.result_conversion_empty", iterations, lambda: execute(engine, "SELECT name FROM layer_sql WHERE id = -1"), category="python_boundary"),
            bench_latency("python.result_conversion_one_row", iterations, lambda: execute(engine, "SELECT name FROM layer_sql WHERE id = 50"), category="python_boundary"),
            bench_latency("python.result_conversion_100_rows", max(1, iterations // 10), lambda: execute(engine, "SELECT * FROM layer_sql"), category="python_boundary"),
        ]
    )

    next_id = 1000

    def sql_insert() -> None:
        nonlocal next_id
        next_id += 1
        execute(engine, f"INSERT INTO layer_sql (id, v, name) VALUES ({next_id}, {next_id}, 'n{next_id}')")

    results.extend(
        [
            bench_latency("sql.insert_one", iterations, sql_insert, category="sql_string"),
            bench_latency("sql.select_by_pk", iterations, lambda: execute(engine, "SELECT name FROM layer_sql WHERE id = 50"), category="sql_string"),
            bench_latency("sql.update_by_pk", iterations, lambda: execute(engine, "UPDATE layer_sql SET v = v + 1 WHERE id = 50"), category="sql_string"),
        ]
    )

    delete_id = 10_000

    def sql_delete() -> None:
        nonlocal delete_id
        delete_id += 1
        execute(engine, f"INSERT INTO layer_sql (id, v, name) VALUES ({delete_id}, 1, 'd')")
        execute(engine, f"DELETE FROM layer_sql WHERE id = {delete_id}")

    results.append(bench_latency("sql.delete_by_pk", max(1, iterations // 2), sql_delete, category="sql_string"))

    pred_engine = make_engine(qm_engine)
    seed_rows(pred_engine, "layer_pred", 2000)
    execute(pred_engine, "CREATE INDEX idx_layer_pred_score ON layer_pred(score)")
    execute(pred_engine, "CREATE INDEX idx_layer_pred_category ON layer_pred(category)")
    results.extend(
        [
            bench_latency("sql.predicate_or", iterations, lambda: execute(pred_engine, "SELECT COUNT(*) FROM layer_pred WHERE score = 10 OR score = 20"), category="sql_string"),
            bench_latency("sql.indexed_string_equality", iterations, lambda: execute(pred_engine, "SELECT id FROM layer_pred WHERE category = 'cat_3'"), category="sql_string"),
            bench_latency("string_index.lookup_duplicate_heavy_count", iterations, lambda: execute(pred_engine, "SELECT COUNT(*) FROM layer_pred WHERE category = 'cat_3'"), category="string_index"),
            bench_latency("string_index.result_materialization_duplicate_heavy", iterations, lambda: execute(pred_engine, "SELECT id FROM layer_pred WHERE category = 'cat_3'"), category="string_index"),
            bench_latency("materialization.row_id_only_duplicate_heavy", iterations, lambda: execute(pred_engine, "SELECT id FROM layer_pred WHERE category = 'cat_3'"), category="materialization"),
            bench_latency("materialization.one_col_projection_duplicate_heavy", iterations, lambda: execute(pred_engine, "SELECT name FROM layer_pred WHERE category = 'cat_3'"), category="materialization"),
            bench_latency("materialization.full_row_projection_duplicate_heavy", iterations, lambda: execute(pred_engine, "SELECT * FROM layer_pred WHERE category = 'cat_3'"), category="materialization"),
            bench_latency("materialization.count_no_projection_duplicate_heavy", iterations, lambda: execute(pred_engine, "SELECT COUNT(*) FROM layer_pred WHERE category = 'cat_3'"), category="materialization"),
        ]
    )
    row_id_sample = execute(pred_engine, "SELECT id FROM layer_pred WHERE category = 'cat_3'")
    one_col_sample = execute(pred_engine, "SELECT name FROM layer_pred WHERE category = 'cat_3'")
    full_row_sample = execute(pred_engine, "SELECT * FROM layer_pred WHERE category = 'cat_3'")

    def consume_result(result: Any) -> int:
        _cols, rows, _tag = result
        total = 0
        for row in rows:
            for value in row:
                if value is not None:
                    total += len(value)
        return total

    results.extend(
        [
            skipped_result("materialization.internal_execution_only_duplicate_heavy", "materialization", "Python extension API exposes only owned boundary results; internal Rust-only materialization timing requires a crate-level bench hook"),
            skipped_result("materialization.projection_only_duplicate_heavy", "materialization", "Projection-only timing is not exposed separately from NativeSqlEngine::execute through PyO3"),
            bench_latency("materialization.python_consume_existing_row_id_duplicate_heavy", iterations, lambda: consume_result(row_id_sample), category="materialization"),
            bench_latency("materialization.python_consume_existing_one_col_duplicate_heavy", iterations, lambda: consume_result(one_col_sample), category="materialization"),
            bench_latency("materialization.python_consume_existing_full_row_duplicate_heavy", iterations, lambda: consume_result(full_row_sample), category="materialization"),
        ]
    )

    unique_engine = make_engine(qm_engine)
    execute(unique_engine, "CREATE TABLE layer_string_unique (id INTEGER PRIMARY KEY, category TEXT)")
    for i in range(1, 2001):
        execute(unique_engine, f"INSERT INTO layer_string_unique (id, category) VALUES ({i}, 'cat_unique_{i}')")
    execute(unique_engine, "CREATE INDEX idx_layer_string_unique_category ON layer_string_unique(category)")
    results.extend(
        [
            bench_latency("string_index.lookup_unique", iterations, lambda: execute(unique_engine, "SELECT id FROM layer_string_unique WHERE category = 'cat_unique_777'"), category="string_index"),
            skipped_result("string_index.lookup_interned_key", "string_index", "string interning/symbol IDs are not implemented yet"),
        ]
    )

    return results


def run_single_row(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    engine = make_engine(qm_engine)
    execute(engine, "CREATE TABLE perf_single (id INTEGER PRIMARY KEY, v INTEGER, name TEXT)")
    next_id = 0

    def insert_one() -> None:
        nonlocal next_id
        next_id += 1
        execute(engine, f"INSERT INTO perf_single (id, v, name) VALUES ({next_id}, {next_id % 31}, 'n{next_id}')")

    results = [bench_latency("single.insert_one", iterations, insert_one, category="single_row")]
    target_id = max(1, next_id // 2)
    results.append(bench_latency("single.select_by_pk", iterations, lambda: execute(engine, f"SELECT name FROM perf_single WHERE id = {target_id}"), category="single_row"))
    results.append(bench_latency("single.update_by_pk", iterations, lambda: execute(engine, f"UPDATE perf_single SET v = v + 1 WHERE id = {target_id}"), category="single_row"))

    delete_id = next_id

    def delete_one() -> None:
        nonlocal delete_id
        delete_id += 1
        execute(engine, f"INSERT INTO perf_single (id, v, name) VALUES ({delete_id}, 1, 'd')")
        execute(engine, f"DELETE FROM perf_single WHERE id = {delete_id}")

    results.append(bench_latency("single.delete_by_pk", max(1, iterations // 2), delete_one, category="single_row"))
    return results


def run_batch(qm_engine: Any, iterations: int, max_batch: int) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    for size in [100, 1000, 10000]:
        if size > max_batch:
            continue
        reps = 5 if size >= 10000 else max(5, min(25, iterations // 20))

        def batch_insert(size: int = size) -> None:
            engine = make_engine(qm_engine)
            execute(engine, "CREATE TABLE b (id INTEGER PRIMARY KEY, v INTEGER, name TEXT)")
            for i in range(1, size + 1):
                execute(engine, f"INSERT INTO b (id, v, name) VALUES ({i}, {i % 17}, 'n{i}')")

        results.append(bench_latency(f"batch.insert_{size}", reps, batch_insert, warmup=1, category="batch"))

    engine = make_engine(qm_engine)
    seed_rows(engine, "perf_batch", min(max_batch, 10000))
    ids = list(range(1, min(1000, max_batch) + 1))
    results.append(bench_latency("batch.select_1000_by_pk", max(1, iterations // 10), lambda: [execute(engine, f"SELECT v FROM perf_batch WHERE id = {i}") for i in ids], warmup=1, category="batch"))
    results.append(bench_latency("batch.update_1000_by_pk", max(1, iterations // 20), lambda: [execute(engine, f"UPDATE perf_batch SET v = v + 1 WHERE id = {i}") for i in ids], warmup=1, category="batch"))

    delete_base = max_batch

    def batch_delete() -> None:
        nonlocal delete_base
        for i in range(1, 501):
            rid = delete_base + i
            execute(engine, f"INSERT INTO perf_batch (id, v, score, name, category) VALUES ({rid}, 1, 1, 'd', 'd')")
            execute(engine, f"DELETE FROM perf_batch WHERE id = {rid}")
        delete_base += 500

    results.append(bench_latency("batch.delete_500_by_pk", max(1, iterations // 50), batch_delete, warmup=1, category="batch"))
    return results


def run_predicates(qm_engine: Any, iterations: int, rows: int) -> list[dict[str, Any]]:
    engine = make_engine(qm_engine)
    seed_rows(engine, "perf_pred", rows)
    execute(engine, "CREATE INDEX idx_perf_pred_score ON perf_pred (score)")
    execute(engine, "CREATE INDEX idx_perf_pred_category ON perf_pred (category)")
    return [
        bench_latency("predicate.pk_equality", iterations, lambda: execute(engine, f"SELECT name FROM perf_pred WHERE id = {rows // 2}"), category="predicate"),
        bench_latency("predicate.indexed_numeric_equality", iterations, lambda: execute(engine, "SELECT id FROM perf_pred WHERE score = 42"), category="predicate"),
        bench_latency("predicate.indexed_string_equality", iterations, lambda: execute(engine, "SELECT id FROM perf_pred WHERE category = 'cat_3'"), category="predicate"),
        bench_latency("predicate.range_scan", iterations, lambda: execute(engine, "SELECT COUNT(*) FROM perf_pred WHERE score BETWEEN 10 AND 30"), category="predicate"),
        bench_latency("predicate.and_scan", iterations, lambda: execute(engine, "SELECT COUNT(*) FROM perf_pred WHERE score >= 10 AND category = 'cat_2'"), category="predicate"),
        bench_latency("predicate.or_scan", iterations, lambda: execute(engine, "SELECT COUNT(*) FROM perf_pred WHERE score = 10 OR score = 20"), category="predicate"),
        bench_latency("predicate.numeric_scan", iterations, lambda: execute(engine, "SELECT COUNT(*) FROM perf_pred WHERE v >= 7"), category="predicate"),
        bench_latency("predicate.string_scan", iterations, lambda: execute(engine, "SELECT COUNT(*) FROM perf_pred WHERE name = 'name_7'"), category="predicate"),
    ]


def run_mvcc(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    engine = make_engine(qm_engine)
    execute(engine, "CREATE TABLE perf_mvcc (id INTEGER PRIMARY KEY, v INTEGER)")
    for i in range(1, 1001):
        execute(engine, f"INSERT INTO perf_mvcc (id, v) VALUES ({i}, {i})")

    results.append(bench_latency("mvcc.read_only_transaction", iterations, lambda: (execute(engine, "BEGIN"), execute(engine, "SELECT v FROM perf_mvcc WHERE id = 500"), execute(engine, "COMMIT")), category="mvcc"))
    next_id = 1000

    def write_txn() -> None:
        nonlocal next_id
        next_id += 1
        execute(engine, "BEGIN")
        execute(engine, f"INSERT INTO perf_mvcc (id, v) VALUES ({next_id}, {next_id})")
        execute(engine, "COMMIT")

    results.append(bench_latency("mvcc.write_transaction", max(1, iterations // 2), write_txn, category="mvcc"))
    results.append(bench_latency("mvcc.mixed_read_write_transaction", max(1, iterations // 2), lambda: (execute(engine, "BEGIN"), execute(engine, "SELECT v FROM perf_mvcc WHERE id = 1"), execute(engine, "UPDATE perf_mvcc SET v = v + 1 WHERE id = 1"), execute(engine, "COMMIT")), category="mvcc"))
    results.append(bench_latency("mvcc.rollback_heavy", max(1, iterations // 2), lambda: (execute(engine, "BEGIN"), execute(engine, "UPDATE perf_mvcc SET v = v + 1 WHERE id = 2"), execute(engine, "ROLLBACK")), category="mvcc"))
    results.append(bench_latency("mvcc.snapshot_visibility", max(1, iterations // 2), lambda: (execute(engine, "BEGIN"), execute(engine, "SELECT COUNT(*) FROM perf_mvcc WHERE v >= 0"), execute(engine, "COMMIT")), category="mvcc"))

    try:
        from core_db.transaction_engine.mvcc import IsolationLevel, TransactionEngine, WriteConflictError

        py_mvcc = TransactionEngine()
        seed_tx = py_mvcc.begin(IsolationLevel.SNAPSHOT)
        py_mvcc.insert(seed_tx, "py", "1", {"id": 1, "v": 1})
        py_mvcc.commit(seed_tx)

        def conflict() -> None:
            t1 = py_mvcc.begin(IsolationLevel.SNAPSHOT)
            t2 = py_mvcc.begin(IsolationLevel.SNAPSHOT)
            py_mvcc.update(t1, "py", "1", {"v": 2})
            py_mvcc.update(t2, "py", "1", {"v": 3})
            py_mvcc.commit(t1)
            try:
                py_mvcc.commit(t2)
            except WriteConflictError:
                return
            raise AssertionError("expected write conflict")

        results.append(bench_latency("mvcc.python_write_write_conflict", max(1, iterations // 4), conflict, category="mvcc"))
    except Exception as exc:
        results.append({"name": "mvcc.python_write_write_conflict", "category": "mvcc", "skipped": str(exc), "error_count": 1})

    concurrent_engine = make_engine(qm_engine)
    execute(concurrent_engine, "CREATE TABLE perf_conc (id INTEGER PRIMARY KEY, v INTEGER)")
    for i in range(1, 1001):
        execute(concurrent_engine, f"INSERT INTO perf_conc (id, v) VALUES ({i}, {i})")

    def concurrent_readers() -> None:
        threads = [threading.Thread(target=lambda: [execute(concurrent_engine, "SELECT COUNT(*) FROM perf_conc WHERE v >= 0") for _ in range(10)]) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

    results.append(bench_latency("mvcc.concurrent_readers", max(1, iterations // 20), concurrent_readers, warmup=1, category="mvcc"))

    writer_id = 1000

    def writer_plus_readers() -> None:
        nonlocal writer_id
        def writer() -> None:
            nonlocal writer_id
            for _ in range(20):
                writer_id += 1
                execute(concurrent_engine, f"INSERT INTO perf_conc (id, v) VALUES ({writer_id}, {writer_id})")

        threads = [threading.Thread(target=lambda: [execute(concurrent_engine, "SELECT COUNT(*) FROM perf_conc WHERE v >= 0") for _ in range(10)]) for _ in range(3)]
        threads.append(threading.Thread(target=writer))
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

    results.append(bench_latency("mvcc.concurrent_writer_readers", max(1, iterations // 20), writer_plus_readers, warmup=1, category="mvcc"))
    return results


def run_indexes(qm_engine: Any, iterations: int, rows: int) -> list[dict[str, Any]]:
    engine = make_engine(qm_engine)
    seed_rows(engine, "perf_idx", rows)
    results = [timed_once("index.create_secondary_on_10k", lambda: execute(engine, "CREATE INDEX idx_perf_idx_score ON perf_idx (score)"), category="index")]
    results.append(bench_latency("index.select_secondary", iterations, lambda: execute(engine, "SELECT id FROM perf_idx WHERE score = 77"), category="index"))
    next_id = rows
    def insert_indexed() -> None:
        nonlocal next_id
        next_id += 1
        execute(engine, f"INSERT INTO perf_idx (id, v, score, name, category) VALUES ({next_id}, 1, 77, 'i', 'cat_1')")
    results.append(bench_latency("index.insert_with_secondary", iterations, insert_indexed, category="index"))
    results.append(bench_latency("index.update_indexed_column", max(1, iterations // 2), lambda: execute(engine, "UPDATE perf_idx SET score = 78 WHERE id = 1"), category="index"))
    delete_id = next_id
    def delete_indexed() -> None:
        nonlocal delete_id
        delete_id += 1
        execute(engine, f"INSERT INTO perf_idx (id, v, score, name, category) VALUES ({delete_id}, 1, 77, 'd', 'cat_1')")
        execute(engine, f"DELETE FROM perf_idx WHERE id = {delete_id}")
    results.append(bench_latency("index.delete_indexed_row", max(1, iterations // 2), delete_indexed, category="index"))
    results.append(bench_latency("index.range_scan_supported_path", iterations, lambda: execute(engine, "SELECT COUNT(*) FROM perf_idx WHERE score BETWEEN 10 AND 20"), category="index"))

    with tempfile.TemporaryDirectory(prefix="qm-index-recover-") as data_dir:
        persisted = make_engine(qm_engine, data_dir)
        seed_rows(persisted, "perf_idx_recover", min(rows, 2000))
        execute(persisted, "CREATE INDEX idx_recover_score ON perf_idx_recover (score)")
        persisted.checkpoint()
        results.append(timed_once("index.recovery_after_restart", lambda: make_engine(qm_engine, data_dir).execute("SELECT id FROM perf_idx_recover WHERE score = 77"), category="index"))
    return results


def run_wal_checkpoint(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    sample_sql = "INSERT INTO perf_wal (id, v) VALUES (123, 456)"
    results.append(bench_latency("wal.record_construction", iterations, lambda: f"{sample_sql};\n".encode(), category="wal_checkpoint"))
    with tempfile.TemporaryDirectory(prefix="qm-wal-micro-") as wal_dir:
        wal_path = Path(wal_dir) / "micro.wal"
        with wal_path.open("ab", buffering=0) as wal_file:
            results.append(bench_latency("wal.file_write", iterations, lambda: wal_file.write(b"INSERT INTO perf_wal (id, v) VALUES (1, 1);\n"), category="wal_checkpoint"))
            results.append(bench_latency("wal.file_flush", max(1, iterations // 10), lambda: wal_file.flush(), category="wal_checkpoint"))
            results.append(bench_latency("wal.file_fsync", max(1, iterations // 50), lambda: os.fsync(wal_file.fileno()), category="wal_checkpoint"))
    with tempfile.TemporaryDirectory(prefix="qm-wal-perf-") as data_dir:
        engine = make_engine(qm_engine, data_dir)
        execute(engine, "CREATE TABLE perf_wal (id INTEGER PRIMARY KEY, v INTEGER)")
        engine.checkpoint()
        next_id = 0
        results.append(
            with_checkpoint_profile(
                bench_latency("checkpoint.no_dirty_tables", max(1, iterations // 50), lambda: engine.checkpoint(), warmup=1, category="wal_checkpoint"),
                engine,
            )
        )

        def append_only() -> None:
            nonlocal next_id
            next_id += 1
            execute(engine, f"INSERT INTO perf_wal (id, v) VALUES ({next_id}, {next_id})")

        results.append(bench_latency("persistent.insert_wal_append_only", max(1, iterations // 2), append_only, category="wal_checkpoint"))

        def sync_wal_only() -> None:
            nonlocal next_id
            next_id += 1
            execute(engine, f"INSERT INTO perf_wal (id, v) VALUES ({next_id}, {next_id})")
            sync = getattr(engine, "sync_wal", None)
            if sync is not None:
                sync()

        results.append(bench_latency("persistent.insert_sync_wal_only", max(1, iterations // 2), sync_wal_only, category="wal_checkpoint"))

        def commit_one() -> None:
            nonlocal next_id
            next_id += 1
            execute(engine, "BEGIN")
            execute(engine, f"INSERT INTO perf_wal (id, v) VALUES ({next_id}, {next_id})")
            execute(engine, "COMMIT")

        results.append(bench_latency("wal.commit_latency", max(1, iterations // 2), commit_one, category="wal_checkpoint"))

        def dirty_small_checkpoint() -> None:
            nonlocal next_id
            next_id += 1
            execute(engine, f"INSERT INTO perf_wal (id, v) VALUES ({next_id}, {next_id})")
            engine.checkpoint()

        results.append(
            with_checkpoint_profile(
                bench_latency("checkpoint.dirty_small_table", max(1, iterations // 50), dirty_small_checkpoint, warmup=1, category="wal_checkpoint"),
                engine,
            )
        )

        def checkpoint_pressure() -> None:
            nonlocal next_id
            for _ in range(50):
                next_id += 1
                execute(engine, f"INSERT INTO perf_wal (id, v) VALUES ({next_id}, {next_id})")
            engine.checkpoint()

        results.append(
            with_checkpoint_profile(
                bench_latency("wal.commit_with_checkpoint_pressure", max(1, iterations // 50), checkpoint_pressure, warmup=1, category="wal_checkpoint"),
                engine,
            )
        )
        results.append(
            with_checkpoint_profile(
                bench_latency("checkpoint.dirty_large_table", max(1, iterations // 100), checkpoint_pressure, warmup=1, category="wal_checkpoint"),
                engine,
            )
        )
        results.append(
            with_checkpoint_profile(
                timed_once(
                    "wal.bulk_insert_1000_then_checkpoint",
                    lambda: (
                        [
                            execute(
                                engine,
                                f"INSERT INTO perf_wal (id, v) VALUES ({next_id + 10000 + i}, {i})",
                            )
                            for i in range(1000)
                        ],
                        engine.checkpoint(),
                    ),
                    category="wal_checkpoint",
                ),
                engine,
            )
        )
        results.append(timed_once("recovery.load_from_checkpoint", lambda: make_engine(qm_engine, data_dir).execute("SELECT COUNT(*) FROM perf_wal"), category="wal_checkpoint"))
        results.append(
            with_checkpoint_profile(
                timed_once(
                    "wal.update_delete_then_checkpoint",
                    lambda: (
                        execute(engine, "UPDATE perf_wal SET v = v + 1 WHERE id = 1"),
                        execute(engine, "DELETE FROM perf_wal WHERE id = 2"),
                        engine.checkpoint(),
                    ),
                    category="wal_checkpoint",
                ),
                engine,
            )
        )
    return results


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _pg_connect(port: int) -> socket.socket:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(5.0)
    sock.connect(("127.0.0.1", port))
    user = b"admin\0"
    db = b"test\0"
    startup = b"\x00\x03\x00\x00user\0" + user + b"database\0" + db + b"\0"
    sock.sendall(struct.pack("!I", len(startup) + 4) + startup)
    sock.recv(4096)
    return sock


def _pg_query(sock: socket.socket, sql: str) -> bytes:
    payload = sql.encode() + b"\0"
    sock.sendall(b"Q" + struct.pack("!I", len(payload) + 4) + payload)
    result = b""
    while True:
        chunk = sock.recv(16384)
        if not chunk:
            break
        result += chunk
        if b"Z" in chunk:
            break
    return result


def run_gateway(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    if not hasattr(qm_engine, "PostgresGateway"):
        return [{"name": "gateway.unavailable", "category": "gateway", "skipped": "PostgresGateway unavailable", "error_count": 1}]

    results: list[dict[str, Any]] = []
    port_counter = 58433

    def lifecycle() -> None:
        nonlocal port_counter
        port_counter += 1
        gw = qm_engine.PostgresGateway("127.0.0.1", port_counter)
        gw.start(lambda _sql: (["ok"], [25], [["1"]]))
        gw.stop()

    results.append(bench_latency("gateway.startup_shutdown_callback", max(3, min(iterations, 25)), lifecycle, category="gateway"))

    with tempfile.TemporaryDirectory(prefix="qm-gateway-perf-") as data_dir:
        port = _find_free_port()
        gw = qm_engine.PostgresGateway("127.0.0.1", port)
        gw.start_native_persist(data_dir)
        time.sleep(0.2)
        try:
            sock = _pg_connect(port)
            _pg_query(sock, "CREATE TABLE gw_perf (id INT, v INT, name TEXT)")
            for i in range(1, 101):
                _pg_query(sock, f"INSERT INTO gw_perf (id, v, name) VALUES ({i}, {i}, 'n{i}')")
            results.append(bench_latency("gateway.reused_connection_noop", iterations, lambda: _pg_query(sock, "SELECT 1"), category="gateway_steady_state"))
            results.append(bench_latency("gateway.reused_connection_simple_query", iterations, lambda: _pg_query(sock, "SELECT * FROM gw_perf WHERE id = 50"), category="gateway_steady_state"))
            results.append(bench_latency("gateway.reused_connection_select_by_pk", iterations, lambda: _pg_query(sock, "SELECT name FROM gw_perf WHERE id = 50"), category="gateway_steady_state"))
            insert_id = 1000

            def gateway_insert() -> bytes:
                nonlocal insert_id
                insert_id += 1
                return _pg_query(sock, f"INSERT INTO gw_perf (id, v, name) VALUES ({insert_id}, {insert_id}, 'g')")

            results.append(bench_latency("gateway.reused_connection_insert_one", max(1, iterations // 2), gateway_insert, category="gateway_steady_state"))
            results.append(bench_latency("gateway.simple_query_pgwire", iterations, lambda: _pg_query(sock, "SELECT * FROM gw_perf WHERE id = 50"), category="gateway"))
            results.append(bench_latency("gateway.batch_query_pgwire_100", max(1, iterations // 10), lambda: [_pg_query(sock, f"SELECT * FROM gw_perf WHERE id = {i}") for i in range(1, 101)], warmup=1, category="gateway"))
            results.append(bench_latency("gateway.transaction_pgwire", max(1, iterations // 2), lambda: (_pg_query(sock, "BEGIN"), _pg_query(sock, "UPDATE gw_perf SET v = v + 1 WHERE id = 1"), _pg_query(sock, "COMMIT")), category="gateway"))
            results.append(bench_latency("gateway.serialization_deserialization", iterations, lambda: _pg_query(sock, "SELECT * FROM gw_perf"), category="gateway"))
            sock.close()
        finally:
            gw.stop()
    return results


def run_vector(qm_engine: Any, iterations: int) -> list[dict[str, Any]]:
    engine = make_engine(qm_engine)
    execute(engine, "CREATE TABLE perf_vec (id INTEGER PRIMARY KEY, category TEXT, embedding VECTOR(3))")
    next_id = 0
    def vector_insert() -> None:
        nonlocal next_id
        next_id += 1
        execute(engine, f"INSERT INTO perf_vec (id, category, embedding) VALUES ({next_id}, 'cat_{next_id % 5}', '[{next_id / 1000.0},0.1,0.2]')")
    results = [bench_latency("vector.insert", iterations, vector_insert, category="vector_cache")]
    for i in range(next_id + 1, next_id + 1001):
        execute(engine, f"INSERT INTO perf_vec (id, category, embedding) VALUES ({i}, 'cat_{i % 5}', '[{i / 1000.0},0.1,0.2]')")
    query = "SELECT id FROM perf_vec ORDER BY embedding <-> '[0.1,0.1,0.2]' LIMIT 10"
    results.append(bench_latency("vector.search_coldish", max(1, iterations // 10), lambda: execute(engine, query), warmup=1, category="vector_cache"))
    results.append(bench_latency("vector.cache_hot_path", iterations, lambda: execute(engine, query), category="vector_cache"))
    results.append(bench_latency("vector.cache_miss_different_query", max(1, iterations // 2), lambda: execute(engine, "SELECT id FROM perf_vec ORDER BY embedding <-> '[0.2,0.1,0.2]' LIMIT 10"), category="vector_cache"))
    return results


def run_profile(profile_path: Path, fn: Callable[[], Any]) -> list[dict[str, Any]]:
    profiler = cProfile.Profile()
    profiler.enable()
    fn()
    profiler.disable()
    profile_path.parent.mkdir(parents=True, exist_ok=True)
    profiler.dump_stats(str(profile_path))
    stream = io.StringIO()
    pstats.Stats(profiler, stream=stream).strip_dirs().sort_stats("cumtime").print_stats(20)
    rows = []
    for line in stream.getvalue().splitlines()[5:25]:
        rows.append({"line": line})
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=1000)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("docs/native_sql_perf_investigation_last.json"))
    parser.add_argument("--profile-output", type=Path, default=Path("docs/native_sql_perf_profile_2026_05_18.prof"))
    parser.add_argument("--build-mode", default="release")
    parser.add_argument("--feature-flags", default="default")
    parser.add_argument("--rows", type=int, default=10000)
    parser.add_argument("--max-batch", type=int, default=10000)
    args = parser.parse_args()

    try:
        import qm_engine  # type: ignore
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2

    iterations = 100 if args.quick else args.iterations
    rows = min(args.rows, 2000 if args.quick else args.rows)
    max_batch = min(args.max_batch, 1000 if args.quick else args.max_batch)
    env = {
        "os": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "cpu_count": os.cpu_count(),
        "memory_total": memory_total(),
        "python_version": platform.python_version(),
        "rust_version": command_version(["rustc", "--version"]),
        "build_mode": args.build_mode,
        "feature_flags": args.feature_flags,
        "durability_mode": "NativeSqlEngine default WAL/checkpoint behavior for persistent engines; in-memory for non-persistent engines",
        "dataset_rows": rows,
        "iterations": iterations,
        "warmup_policy": "10 warmup iterations for latency loops unless overridden; 1 warmup for large batch loops",
        "cache_policy": "warm-cache unless workload name says coldish/recovery",
        "max_rss_mb_start": max_rss_mb(),
    }

    results: list[dict[str, Any]] = []
    start = time.perf_counter()
    suites = [
        lambda: run_core_direct(iterations),
        lambda: run_layer_decomposition(qm_engine, iterations),
        lambda: run_single_row(qm_engine, iterations),
        lambda: run_batch(qm_engine, iterations, max_batch),
        lambda: run_predicates(qm_engine, iterations, rows),
        lambda: run_mvcc(qm_engine, iterations),
        lambda: run_indexes(qm_engine, iterations, rows),
        lambda: run_wal_checkpoint(qm_engine, iterations),
        lambda: run_gateway(qm_engine, iterations),
        lambda: run_vector(qm_engine, iterations),
    ]

    for suite in suites:
        results.extend(suite())

    profile_rows = run_profile(
        args.profile_output,
        lambda: run_predicates(qm_engine, max(25, iterations // 20), min(rows, 2000)),
    )
    env["max_rss_mb_end"] = max_rss_mb()
    env["peak_rss_mb"] = env["max_rss_mb_end"]
    env["total_duration_sec"] = time.perf_counter() - start

    report = {
        "schema_version": 1,
        "mode": "quick" if args.quick else "full",
        "environment": env,
        "results": results,
        "profile": {
            "format": "cProfile",
            "path": str(args.profile_output),
            "top_cumulative_rows": profile_rows,
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
