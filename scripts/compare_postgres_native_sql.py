#!/usr/bin/env python3
"""Fair local PostgreSQL vs NativeSqlEngine comparison for supported workloads."""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit, urlunsplit


SYNC_POLICIES = {
    "per-mutation": {
        "qm_sync_policy": "per_mutation_sync",
        "sync_before_commit_return": True,
        "acknowledged_before_fsync": False,
        "durability_window_description": "Every benchmarked mutating statement or COMMIT waits for sync_wal() before returning; strongest current benchmark mode.",
        "claim_scope": "persistent NativeSqlEngine WAL with explicit sync_wal after each timed mutation/commit; durability sanity checked by reopen",
    },
    "per-commit": {
        "qm_sync_policy": "per_commit_sync",
        "sync_before_commit_return": True,
        "acknowledged_before_fsync": False,
        "durability_window_description": "Autocommit statements sync before returning; explicit transactions append during the transaction and sync once after COMMIT.",
        "claim_scope": "persistent NativeSqlEngine WAL with commit-level sync_wal; durable when COMMIT/autocommit statement returns",
    },
    "per-commit-sync-data": {
        "qm_sync_policy": "per_commit_sync_data",
        "sync_before_commit_return": True,
        "acknowledged_before_fsync": False,
        "durability_window_description": "Autocommit statements and explicit transaction COMMIT flush buffered WAL and call File::sync_data() before returning.",
        "claim_scope": "persistent NativeSqlEngine WAL with commit-level sync_data; durable data sync semantics, not silently equated with sync_all metadata semantics",
    },
    "group-commit": {
        "qm_sync_policy": "group_commit",
        "sync_before_commit_return": False,
        "acknowledged_before_fsync": True,
        "durability_window_description": "Mutations are acknowledged before fsync until the configured commit count/interval flushes pending WAL.",
        "claim_scope": "persistent NativeSqlEngine WAL with benchmark group-commit batching; not equivalent to PostgreSQL synchronous_commit=on",
    },
    "group-commit-sync": {
        "qm_sync_policy": "group_commit_sync",
        "sync_before_commit_return": True,
        "acknowledged_before_fsync": False,
        "durability_window_description": "Mutations append WAL and wait for the engine-native group commit coordinator to complete sync_all() before returning.",
        "claim_scope": "persistent NativeSqlEngine WAL with durable group commit; concurrent commits may share one sync_all",
    },
    "append-only-profile": {
        "qm_sync_policy": "wal_append_only",
        "sync_before_commit_return": False,
        "acknowledged_before_fsync": True,
        "durability_window_description": "WAL records are appended but benchmarked operations do not call sync_wal(); profiling only.",
        "claim_scope": "persistent NativeSqlEngine WAL append-only profiling; no synchronous durability claim",
    },
    "relaxed-os-buffered": {
        "qm_sync_policy": "relaxed_os_buffered",
        "sync_before_commit_return": False,
        "acknowledged_before_fsync": True,
        "durability_window_description": "WAL records are appended and flushed to the OS buffer, but benchmarked operations do not call sync_all() before returning.",
        "claim_scope": "persistent NativeSqlEngine WAL with OS-buffer flush only; faster but not strict crash-durable at return",
    },
}


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
    return text[0] if text else "unavailable"


def sanitize_dsn(dsn: str) -> str:
    if "://" not in dsn:
        parts = []
        for item in dsn.split():
            if item.lower().startswith(("password=", "pass=")):
                key = item.split("=", 1)[0]
                parts.append(f"{key}=***")
            else:
                parts.append(item)
        return " ".join(parts)
    try:
        parsed = urlsplit(dsn)
        netloc = parsed.netloc
        if "@" in netloc and ":" in netloc.split("@", 1)[0]:
            userinfo, hostinfo = netloc.split("@", 1)
            user = userinfo.split(":", 1)[0]
            netloc = f"{user}:***@{hostinfo}"
        return urlunsplit((parsed.scheme, netloc, parsed.path, parsed.query, parsed.fragment))
    except Exception:
        return "<unparseable dsn>"


def setup_sql() -> str:
    return """-- NativeSqlEngine/PostgreSQL benchmark setup
-- Run as a PostgreSQL superuser or an account allowed to create roles/databases.
DO $$
BEGIN
   IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'qm_bench') THEN
      CREATE ROLE qm_bench LOGIN PASSWORD 'change-me';
   END IF;
END
$$;
CREATE DATABASE qm_bench OWNER qm_bench;
"""


def explain_env(default_dsn: str) -> str:
    return f"""PostgreSQL comparison environment

Set a valid benchmark DSN with either --postgres-dsn/--dsn or POSTGRES_DSN.
Current sanitized DSN: {sanitize_dsn(default_dsn)}

Example:
  createuser --createdb qm_bench
  createdb --owner=qm_bench qm_bench
  POSTGRES_DSN='postgresql://qm_bench@localhost:5432/qm_bench' \\
    python3 scripts/compare_postgres_native_sql.py --iterations 1000 --strict

To emit setup SQL:
  python3 scripts/compare_postgres_native_sql.py --setup-sql-output /tmp/qm_bench_setup.sql
"""


def outer_txn_iterations(iterations: int, batch_size: int) -> int:
    """Outer transaction loop count — keep total rows inserted ~O(iterations).

    batch_size=10  -> iterations/10 outer loops (not iterations/1).
    batch_size=100 -> iterations/10
    batch_size=1000 -> iterations/100
    """
    unit = batch_size if batch_size < 100 else batch_size // 10
    return max(1, min(iterations, iterations // max(1, unit)))


def bench(name: str, iterations: int, fn: Callable[[], Any]) -> dict[str, Any]:
    latencies = []
    errors = 0
    for _ in range(min(10, iterations)):
        try:
            fn()
        except Exception:
            errors += 1
    start = time.perf_counter()
    for _ in range(iterations):
        op_start = time.perf_counter()
        try:
            fn()
        except Exception:
            errors += 1
        latencies.append((time.perf_counter() - op_start) * 1000.0)
    elapsed = time.perf_counter() - start
    return {
        "name": name,
        "p50_ms": percentile(latencies, 50),
        "p95_ms": percentile(latencies, 95),
        "p99_ms": percentile(latencies, 99),
        "min_ms": min(latencies) if latencies else None,
        "max_ms": max(latencies) if latencies else None,
        "mean_ms": statistics.fmean(latencies) if latencies else None,
        "throughput_ops_sec": iterations / elapsed if elapsed > 0 else 0.0,
        "iterations": iterations,
        "error_count": errors,
    }


def _rows(engine: Any, sql: str) -> list[list[str | None]]:
    return engine.execute(sql)[1]


def _sync_if_persistent(engine: Any, qm_mode: str) -> None:
    if qm_mode == "persistent-wal":
        engine.sync_wal()


class QmSyncController:
    def __init__(
        self,
        qm_mode: str,
        sync_policy: str,
        sync_every_n: int,
        sync_interval_ms: int,
        engine_native_sync: bool = False,
    ) -> None:
        self.qm_mode = qm_mode
        self.sync_policy = sync_policy
        self.sync_every_n = max(1, sync_every_n)
        self.sync_interval_ms = max(0, sync_interval_ms)
        self.engine_native_sync = engine_native_sync
        self.pending = 0
        self.last_sync = time.perf_counter()
        self.sync_count = 0

    def _persistent(self) -> bool:
        return self.qm_mode == "persistent-wal"

    def sync_now(self, engine: Any) -> None:
        if not self._persistent():
            return
        engine.sync_wal()
        self.pending = 0
        self.last_sync = time.perf_counter()
        self.sync_count += 1

    def _maybe_group_sync(self, engine: Any) -> None:
        if not self._persistent():
            return
        self.pending += 1
        elapsed_ms = (time.perf_counter() - self.last_sync) * 1000.0
        if self.pending >= self.sync_every_n or (
            self.sync_interval_ms > 0 and elapsed_ms >= self.sync_interval_ms
        ):
            self.sync_now(engine)

    def after_autocommit_mutation(self, engine: Any) -> None:
        if not self._persistent():
            return
        if self.sync_policy in {"per-mutation", "per-commit", "per-commit-sync-data"}:
            if not self.engine_native_sync:
                self.sync_now(engine)
        elif self.sync_policy == "group-commit":
            self._maybe_group_sync(engine)
        elif self.sync_policy in {
            "append-only-profile",
            "relaxed-os-buffered",
            "group-commit-sync",
        }:
            self.pending += 1
        else:
            raise ValueError(f"unknown sync policy {self.sync_policy}")

    def after_transaction_mutation(self, engine: Any) -> None:
        if not self._persistent():
            return
        if self.sync_policy == "per-mutation":
            # NativeSqlEngine stages explicit transaction WAL until COMMIT, so
            # the strongest safe point available here is still COMMIT return.
            self.pending += 1
        elif self.sync_policy in {
            "per-commit",
            "per-commit-sync-data",
            "group-commit-sync",
            "group-commit",
            "append-only-profile",
            "relaxed-os-buffered",
        }:
            self.pending += 1
        else:
            raise ValueError(f"unknown sync policy {self.sync_policy}")

    def after_transaction_commit(self, engine: Any) -> None:
        if not self._persistent():
            return
        if self.sync_policy in {"per-mutation", "per-commit", "per-commit-sync-data"}:
            if not self.engine_native_sync:
                self.sync_now(engine)
        elif self.sync_policy == "group-commit":
            self._maybe_group_sync(engine)
        elif self.sync_policy in {
            "append-only-profile",
            "relaxed-os-buffered",
            "group-commit-sync",
        }:
            self.pending += 1
        else:
            raise ValueError(f"unknown sync policy {self.sync_policy}")

    def finish(self, engine: Any) -> None:
        if self._persistent() and self.sync_policy == "group-commit" and self.pending:
            self.sync_now(engine)


def _new_qm_engine(qm_engine: Any, qm_mode: str, data_dir: Path | None) -> Any:
    if qm_mode == "persistent-wal":
        if data_dir is None:
            raise RuntimeError("persistent-wal mode requires a data directory")
        return qm_engine.NativeSqlEngine(str(data_dir))
    return qm_engine.NativeSqlEngine()


def _configure_qm_sync_policy(engine: Any, qm_mode: str, sync_policy: str) -> None:
    if qm_mode != "persistent-wal" or not hasattr(engine, "set_wal_sync_policy"):
        return
    if sync_policy in {
        "per-mutation",
        "per-commit",
        "per-commit-sync-data",
        "group-commit-sync",
        "relaxed-os-buffered",
    }:
        engine.set_wal_sync_policy(sync_policy)
    else:
        engine.set_wal_sync_policy("append-only-profile")


def _wal_size(data_dir: Path | None) -> int:
    if data_dir is None:
        return 0
    wal_path = data_dir / "native_sql.wal"
    return wal_path.stat().st_size if wal_path.exists() else 0


def _engine_sync_count(engine: Any) -> int:
    if hasattr(engine, "wal_sync_count"):
        return int(engine.wal_sync_count())
    return 0


def durability_sanity_checks(qm_engine: Any, data_dir: Path) -> dict[str, Any]:
    def reopen() -> Any:
        return qm_engine.NativeSqlEngine(str(data_dir))

    engine = reopen()
    engine.execute("CREATE TABLE sanity (id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, category TEXT)")
    engine.execute("CREATE INDEX idx_sanity_score ON sanity(score)")
    engine.sync_wal()

    engine.execute("BEGIN")
    engine.execute("INSERT INTO sanity (id, v, score, category) VALUES (1, 10, 7, 'keep')")
    engine.execute("COMMIT")
    engine.sync_wal()
    del engine

    engine = reopen()
    if _rows(engine, "SELECT v FROM sanity WHERE id = 1") != [["10"]]:
        raise RuntimeError("durability sanity failed: committed insert not visible after reopen")

    engine.execute("BEGIN")
    engine.execute("INSERT INTO sanity (id, v, score, category) VALUES (2, 20, 7, 'rollback')")
    engine.execute("ROLLBACK")
    engine.sync_wal()
    del engine

    engine = reopen()
    if _rows(engine, "SELECT COUNT(*) FROM sanity WHERE id = 2") != [["0"]]:
        raise RuntimeError("durability sanity failed: rolled-back insert visible after reopen")

    engine.execute("BEGIN")
    engine.execute("UPDATE sanity SET v = 11 WHERE id = 1")
    engine.execute("COMMIT")
    engine.sync_wal()
    del engine

    engine = reopen()
    if _rows(engine, "SELECT v FROM sanity WHERE id = 1") != [["11"]]:
        raise RuntimeError("durability sanity failed: committed update not visible after reopen")

    engine.execute("INSERT INTO sanity (id, v, score, category) VALUES (3, 30, 7, 'delete')")
    engine.sync_wal()
    engine.execute("BEGIN")
    engine.execute("DELETE FROM sanity WHERE id = 3")
    engine.execute("COMMIT")
    engine.sync_wal()
    del engine

    engine = reopen()
    if _rows(engine, "SELECT COUNT(*) FROM sanity WHERE id = 3") != [["0"]]:
        raise RuntimeError("durability sanity failed: committed delete visible after reopen")
    indexed_ids = sorted(row[0] for row in _rows(engine, "SELECT id FROM sanity WHERE score = 7"))
    if indexed_ids != ["1"]:
        raise RuntimeError(f"durability sanity failed: indexed SELECT returned {indexed_ids}")
    if _rows(engine, "SELECT COUNT(*) FROM sanity WHERE score BETWEEN 1 AND 10") != [["1"]]:
        raise RuntimeError("durability sanity failed: range COUNT mismatch after reopen")

    info = engine.snapshot_info()
    wal_path = data_dir / "native_sql.wal"
    return {
        "passed": True,
        "data_dir": str(data_dir),
        "wal_size_bytes": wal_path.stat().st_size if wal_path.exists() else 0,
        "snapshot_info": dict(info),
    }


def run_qm(
    qm_engine: Any,
    iterations: int,
    durability_mode: str,
    qm_mode: str,
    sync_policy: str,
    sync_every_n: int,
    sync_interval_ms: int,
    keep_artifacts: bool,
) -> tuple[dict[str, dict[str, Any]], dict[str, Any]]:
    data_dir: Path | None = None
    if qm_mode == "persistent-wal":
        data_dir = Path(tempfile.mkdtemp(prefix="qm_pg_compare_persistent_"))
    engine_native_sync = sync_policy in {
        "per-mutation",
        "per-commit",
        "per-commit-sync-data",
        "group-commit-sync",
        "relaxed-os-buffered",
    }
    sync = QmSyncController(qm_mode, sync_policy, sync_every_n, sync_interval_ms, engine_native_sync)
    engine = _new_qm_engine(qm_engine, qm_mode, data_dir)
    _configure_qm_sync_policy(engine, qm_mode, sync_policy)
    durability_check = None
    if qm_mode == "persistent-wal":
        durability_check = durability_sanity_checks(qm_engine, data_dir)  # type: ignore[arg-type]
        engine = _new_qm_engine(qm_engine, qm_mode, data_dir)
        _configure_qm_sync_policy(engine, qm_mode, sync_policy)

    engine.execute("CREATE TABLE cmp (id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, name TEXT, category TEXT)")
    sync.sync_now(engine)
    next_id = 0

    def qm_bench(name: str, count: int, fn: Callable[[], Any], eng: Any = engine, wal_dir: Path | None = data_dir) -> dict[str, Any]:
        before_wal = _wal_size(wal_dir)
        before_sync = _engine_sync_count(eng)
        item = bench(name, count, fn)
        after_wal = _wal_size(wal_dir)
        after_sync = _engine_sync_count(eng)
        sync_delta = max(0, after_sync - before_sync)
        item["wal_bytes_delta"] = max(0, after_wal - before_wal)
        item["sync_count_delta"] = sync_delta
        item["fsync_count_delta"] = sync_delta if qm_mode == "persistent-wal" and sync_policy != "append-only-profile" else 0
        item["commit_waits_for_fsync"] = qm_mode == "persistent-wal" and sync_policy in {
            "per-mutation",
            "per-commit",
            "group-commit-sync",
        }
        item["commit_waits_for_sync_data"] = (
            qm_mode == "persistent-wal" and sync_policy == "per-commit-sync-data"
        )
        return item

    def batch_iterations(batch_size: int) -> int:
        return outer_txn_iterations(iterations, batch_size)

    def insert() -> None:
        nonlocal next_id
        next_id += 1
        engine.execute(f"INSERT INTO cmp (id, v, score, name, category) VALUES ({next_id}, {next_id % 17}, {next_id % 101}, 'n{next_id}', 'cat_{next_id % 10}')")
        sync.after_autocommit_mutation(engine)

    results = [qm_bench("insert", iterations, insert)]
    target = max(1, next_id // 2)
    engine.execute("CREATE INDEX idx_cmp_score ON cmp (score)")
    engine.execute("CREATE INDEX idx_cmp_category ON cmp (category)")
    sync.after_autocommit_mutation(engine)
    results.append(qm_bench("select_by_pk", iterations, lambda: engine.execute(f"SELECT name FROM cmp WHERE id = {target}")))

    def update_by_pk() -> None:
        engine.execute(f"UPDATE cmp SET v = v + 1 WHERE id = {target}")
        sync.after_autocommit_mutation(engine)

    results.append(qm_bench("update_by_pk", iterations, update_by_pk))
    delete_count = max(1, iterations // 2)
    delete_start = next_id + 1
    for offset in range(delete_count):
        row_id = delete_start + offset
        engine.execute(f"INSERT INTO cmp (id, v, score, name, category) VALUES ({row_id}, 1, 1, 'd', 'delete')")
        sync.after_autocommit_mutation(engine)
    next_id += delete_count
    delete_id = delete_start

    def delete() -> None:
        nonlocal delete_id
        engine.execute(f"DELETE FROM cmp WHERE id = {delete_id}")
        sync.after_autocommit_mutation(engine)
        delete_id += 1

    results.append(qm_bench("delete_by_pk", delete_count, delete))
    results.append(qm_bench("indexed_integer_equality", iterations, lambda: engine.execute("SELECT id FROM cmp WHERE score = 42")))
    results.append(qm_bench("indexed_string_equality_duplicate_heavy", iterations, lambda: engine.execute("SELECT id FROM cmp WHERE category = 'cat_3'")))
    results.append(qm_bench("count_indexed_equality", iterations, lambda: engine.execute("SELECT COUNT(*) FROM cmp WHERE score = 42")))
    results.append(qm_bench("predicate_range", iterations, lambda: engine.execute("SELECT COUNT(*) FROM cmp WHERE score BETWEEN 10 AND 20")))

    def transaction_commit() -> None:
        engine.execute("BEGIN")
        engine.execute("UPDATE cmp SET v = v + 1 WHERE id = 1")
        sync.after_transaction_mutation(engine)
        engine.execute("COMMIT")
        sync.after_transaction_commit(engine)

    def transaction_rollback() -> None:
        engine.execute("BEGIN")
        engine.execute("UPDATE cmp SET v = v + 1 WHERE id = 1")
        sync.after_transaction_mutation(engine)
        engine.execute("ROLLBACK")

    results.append(qm_bench("transaction_commit", max(1, iterations // 2), transaction_commit))
    results.append(qm_bench("transaction_rollback", max(1, iterations // 2), transaction_rollback))

    batch_id = next_id + 100_000

    def transaction_batch_insert_commit(batch_size: int) -> Callable[[], None]:
        def run() -> None:
            nonlocal batch_id
            engine.execute("BEGIN")
            for _ in range(batch_size):
                batch_id += 1
                engine.execute(
                    f"INSERT INTO cmp (id, v, score, name, category) VALUES ({batch_id}, {batch_id % 17}, {batch_id % 101}, 'batch{batch_id}', 'batch')"
                )
                sync.after_transaction_mutation(engine)
            engine.execute("COMMIT")
            sync.after_transaction_commit(engine)
        return run

    for batch_size in (10, 100, 1000):
        results.append(
            qm_bench(
                f"transaction_insert_{batch_size}_commit",
                batch_iterations(batch_size),
                transaction_batch_insert_commit(batch_size),
            )
        )

    def transaction_mixed_dml_100_commit() -> None:
        nonlocal batch_id
        engine.execute("BEGIN")
        for _ in range(40):
            batch_id += 1
            engine.execute(
                f"INSERT INTO cmp (id, v, score, name, category) VALUES ({batch_id}, {batch_id % 17}, {batch_id % 101}, 'mixed{batch_id}', 'mixed')"
            )
            sync.after_transaction_mutation(engine)
        for row_id in range(1, 41):
            engine.execute(f"UPDATE cmp SET v = v + 1 WHERE id = {row_id}")
            sync.after_transaction_mutation(engine)
        for row_id in range(41, 61):
            engine.execute(f"DELETE FROM cmp WHERE id = {row_id}")
            sync.after_transaction_mutation(engine)
        engine.execute("COMMIT")
        sync.after_transaction_commit(engine)

    results.append(
        qm_bench(
            "transaction_mixed_dml_100_commit",
            batch_iterations(100),
            transaction_mixed_dml_100_commit,
        )
    )

    def transaction_rollback_100() -> None:
        engine.execute("BEGIN")
        for row_id in range(1, 101):
            engine.execute(f"UPDATE cmp SET v = v + 1 WHERE id = {row_id}")
            sync.after_transaction_mutation(engine)
        engine.execute("ROLLBACK")

    results.append(
        qm_bench(
            "transaction_rollback_100",
            batch_iterations(100),
            transaction_rollback_100,
        )
    )

    unique_dir: Path | None = None
    if qm_mode == "persistent-wal":
        unique_dir = (data_dir / "unique") if data_dir else None
    unique = _new_qm_engine(qm_engine, qm_mode, unique_dir)
    _configure_qm_sync_policy(unique, qm_mode, sync_policy)
    unique_sync = QmSyncController(qm_mode, sync_policy, sync_every_n, sync_interval_ms, engine_native_sync)
    unique.execute("CREATE TABLE cmp_unique (id INTEGER PRIMARY KEY, category TEXT)")
    unique_sync.sync_now(unique)
    for i in range(1, 2001):
        unique.execute(f"INSERT INTO cmp_unique (id, category) VALUES ({i}, 'cat_unique_{i}')")
        if i % 100 == 0:
            unique_sync.after_autocommit_mutation(unique)
    unique_sync.sync_now(unique)
    unique.execute("CREATE INDEX idx_cmp_unique_category ON cmp_unique(category)")
    unique_sync.after_autocommit_mutation(unique)
    results.append(qm_bench("indexed_string_equality_unique", iterations, lambda: unique.execute("SELECT id FROM cmp_unique WHERE category = 'cat_unique_777'"), unique, unique_dir))
    sync.finish(engine)
    unique_sync.finish(unique)
    policy_info = SYNC_POLICIES[sync_policy]
    sync_before_commit_return = bool(policy_info["sync_before_commit_return"])
    if sync_policy == "group-commit" and sync_every_n == 1 and sync_interval_ms == 0:
        sync_before_commit_return = True
    metadata = {
        "qm_mode": qm_mode,
        "qm_sync_policy": policy_info["qm_sync_policy"] if qm_mode == "persistent-wal" else "none",
        "sync_before_commit_return": sync_before_commit_return if qm_mode == "persistent-wal" else None,
        "sync_every_n": sync_every_n if qm_mode == "persistent-wal" and sync_policy == "group-commit" else None,
        "sync_interval_ms": sync_interval_ms if qm_mode == "persistent-wal" and sync_policy == "group-commit" else None,
        "acknowledged_before_fsync": policy_info["acknowledged_before_fsync"] if qm_mode == "persistent-wal" else None,
        "durability_window_description": policy_info["durability_window_description"] if qm_mode == "persistent-wal" else "in-memory mode has no WAL durability window",
        "qm_durability_mode": "native_sql_default_in_memory_autocommit"
        if qm_mode == "memory"
        else f"persistent_native_sql_wal_{policy_info['qm_sync_policy']}",
        "qm_storage_path": str(data_dir) if data_dir else None,
        "wal_enabled": qm_mode == "persistent-wal",
        "fsync_enabled": qm_mode == "persistent-wal" and sync_policy != "append-only-profile",
        "autocommit_mode": "autocommit outside explicit BEGIN/COMMIT workloads",
        "reopen_validation_passed": bool(durability_check["passed"]) if durability_check else None,
        "durability_sanity": durability_check,
        "claim_scope": "local in-memory/autocommit scalar comparison; not durability-equivalent"
        if qm_mode == "memory"
        else policy_info["claim_scope"],
        "qm_sync_count": _engine_sync_count(engine) + _engine_sync_count(unique) + sync.sync_count + unique_sync.sync_count,
    }
    if qm_mode == "persistent-wal" and not keep_artifacts and data_dir is not None:
        artifact_path = metadata["qm_storage_path"]
        shutil.rmtree(data_dir, ignore_errors=True)
        metadata["qm_storage_path"] = artifact_path
        metadata["artifacts_removed"] = True
    elif qm_mode == "persistent-wal":
        metadata["artifacts_removed"] = False
    return {item["name"]: item for item in results}, metadata


def run_pg(dsn: str, iterations: int, durability_mode: str) -> tuple[dict[str, dict[str, Any]] | None, str | None, dict[str, str]]:
    settings: dict[str, str] = {}
    try:
        import psycopg2  # type: ignore
    except Exception as exc:
        return None, f"psycopg2 unavailable: {exc}", settings

    try:
        conn = psycopg2.connect(dsn)
        conn.autocommit = True
    except Exception as exc:
        return None, f"PostgreSQL connection failed: {exc}", settings

    cur = conn.cursor()
    cur.execute("SHOW server_version")
    settings["server_version"] = cur.fetchone()[0]
    cur.execute("SHOW fsync")
    settings["fsync"] = cur.fetchone()[0]
    cur.execute("SHOW synchronous_commit")
    settings["synchronous_commit_initial"] = cur.fetchone()[0]
    if durability_mode in {"relaxed", "wal_no_fsync"}:
        cur.execute("SET synchronous_commit = off")
    else:
        cur.execute("SET synchronous_commit = on")
    cur.execute("SHOW synchronous_commit")
    settings["synchronous_commit_effective"] = cur.fetchone()[0]
    cur.execute("DROP TABLE IF EXISTS qm_cmp")
    cur.execute("CREATE TABLE qm_cmp (id INTEGER PRIMARY KEY, v INTEGER, score INTEGER, name TEXT, category TEXT)")
    next_id = 0

    def insert() -> None:
        nonlocal next_id
        next_id += 1
        cur.execute("INSERT INTO qm_cmp (id, v, score, name, category) VALUES (%s, %s, %s, %s, %s)", (next_id, next_id % 17, next_id % 101, f"n{next_id}", f"cat_{next_id % 10}"))

    results = [bench("insert", iterations, insert)]
    target = max(1, next_id // 2)
    cur.execute("CREATE INDEX idx_qm_cmp_score ON qm_cmp (score)")
    cur.execute("CREATE INDEX idx_qm_cmp_category ON qm_cmp (category)")
    results.append(bench("select_by_pk", iterations, lambda: cur.execute("SELECT name FROM qm_cmp WHERE id = %s", (target,))))
    results.append(bench("update_by_pk", iterations, lambda: cur.execute("UPDATE qm_cmp SET v = v + 1 WHERE id = %s", (target,))))
    delete_count = max(1, iterations // 2)
    delete_start = next_id + 1
    for offset in range(delete_count):
        row_id = delete_start + offset
        cur.execute("INSERT INTO qm_cmp (id, v, score, name, category) VALUES (%s, %s, %s, %s, %s)", (row_id, 1, 1, "d", "delete"))
    next_id += delete_count
    delete_id = delete_start

    def delete() -> None:
        nonlocal delete_id
        cur.execute("DELETE FROM qm_cmp WHERE id = %s", (delete_id,))
        delete_id += 1

    results.append(bench("delete_by_pk", delete_count, delete))
    results.append(bench("indexed_integer_equality", iterations, lambda: cur.execute("SELECT id FROM qm_cmp WHERE score = %s", (42,))))
    results.append(bench("indexed_string_equality_duplicate_heavy", iterations, lambda: cur.execute("SELECT id FROM qm_cmp WHERE category = %s", ("cat_3",))))
    results.append(bench("count_indexed_equality", iterations, lambda: cur.execute("SELECT COUNT(*) FROM qm_cmp WHERE score = %s", (42,))))
    results.append(bench("predicate_range", iterations, lambda: cur.execute("SELECT COUNT(*) FROM qm_cmp WHERE score BETWEEN %s AND %s", (10, 20))))
    conn.autocommit = False

    def txn() -> None:
        cur.execute("UPDATE qm_cmp SET v = v + 1 WHERE id = %s", (1,))
        conn.commit()

    results.append(bench("transaction_commit", max(1, iterations // 2), txn))
    def rollback_txn() -> None:
        cur.execute("UPDATE qm_cmp SET v = v + 1 WHERE id = %s", (1,))
        conn.rollback()

    results.append(bench("transaction_rollback", max(1, iterations // 2), rollback_txn))
    batch_id = next_id + 100_000

    def batch_iterations(batch_size: int) -> int:
        return outer_txn_iterations(iterations, batch_size)

    def transaction_batch_insert_commit(batch_size: int) -> Callable[[], None]:
        def run() -> None:
            nonlocal batch_id
            for _ in range(batch_size):
                batch_id += 1
                cur.execute(
                    "INSERT INTO qm_cmp (id, v, score, name, category) VALUES (%s, %s, %s, %s, %s)",
                    (batch_id, batch_id % 17, batch_id % 101, f"batch{batch_id}", "batch"),
                )
            conn.commit()
        return run

    for batch_size in (10, 100, 1000):
        results.append(
            bench(
                f"transaction_insert_{batch_size}_commit",
                batch_iterations(batch_size),
                transaction_batch_insert_commit(batch_size),
            )
        )

    def transaction_mixed_dml_100_commit() -> None:
        nonlocal batch_id
        for _ in range(40):
            batch_id += 1
            cur.execute(
                "INSERT INTO qm_cmp (id, v, score, name, category) VALUES (%s, %s, %s, %s, %s)",
                (batch_id, batch_id % 17, batch_id % 101, f"mixed{batch_id}", "mixed"),
            )
        for row_id in range(1, 41):
            cur.execute("UPDATE qm_cmp SET v = v + 1 WHERE id = %s", (row_id,))
        for row_id in range(41, 61):
            cur.execute("DELETE FROM qm_cmp WHERE id = %s", (row_id,))
        conn.commit()

    results.append(
        bench(
            "transaction_mixed_dml_100_commit",
            batch_iterations(100),
            transaction_mixed_dml_100_commit,
        )
    )

    def transaction_rollback_100() -> None:
        for row_id in range(1, 101):
            cur.execute("UPDATE qm_cmp SET v = v + 1 WHERE id = %s", (row_id,))
        conn.rollback()

    results.append(
        bench(
            "transaction_rollback_100",
            batch_iterations(100),
            transaction_rollback_100,
        )
    )
    conn.autocommit = True
    cur.execute("DROP TABLE IF EXISTS qm_cmp_unique")
    cur.execute("CREATE TABLE qm_cmp_unique (id INTEGER PRIMARY KEY, category TEXT)")
    cur.executemany(
        "INSERT INTO qm_cmp_unique (id, category) VALUES (%s, %s)",
        [(i, f"cat_unique_{i}") for i in range(1, 2001)],
    )
    cur.execute("CREATE INDEX idx_qm_cmp_unique_category ON qm_cmp_unique(category)")
    results.append(bench("indexed_string_equality_unique", iterations, lambda: cur.execute("SELECT id FROM qm_cmp_unique WHERE category = %s", ("cat_unique_777",))))
    cur.execute("DROP TABLE IF EXISTS qm_cmp_unique")
    cur.execute("DROP TABLE IF EXISTS qm_cmp")
    cur.close()
    conn.close()
    return {item["name"]: item for item in results}, None, settings


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=1000)
    parser.add_argument("--output", type=Path, default=Path("docs/postgres_comparison_2026_05_18.json"))
    parser.add_argument("--strict", action="store_true", help="return non-zero if PostgreSQL is unavailable")
    parser.add_argument(
        "--durability-mode",
        choices=["memory", "relaxed", "wal_no_fsync", "wal_fsync", "checkpoint_pressure"],
        default="wal_fsync",
    )
    parser.add_argument(
        "--qm-mode",
        choices=["memory", "persistent-wal"],
        default="memory",
        help="NativeSqlEngine mode: in-memory default or persistent WAL with recorded WAL sync policy",
    )
    parser.add_argument(
        "--qm-sync-policy",
        choices=sorted(SYNC_POLICIES),
        default="per-mutation",
        help="Persistent NativeSqlEngine WAL sync policy; per-mutation/per-commit are engine-native, group-commit is benchmark-controller batching",
    )
    parser.add_argument(
        "--sync-every-n",
        type=int,
        default=100,
        help="Group-commit sync threshold for --qm-sync-policy group-commit",
    )
    parser.add_argument(
        "--sync-interval-ms",
        type=int,
        default=0,
        help="Group-commit sync interval for --qm-sync-policy group-commit; 0 disables interval-based sync",
    )
    parser.add_argument("--keep-artifacts", action="store_true", help="keep persistent NativeSqlEngine benchmark directory")
    parser.add_argument(
        "--dsn",
        default=os.environ.get(
            "POSTGRES_DSN",
            os.environ.get("QM_POSTGRES_DSN", "dbname=postgres user=postgres host=/tmp"),
        ),
    )
    parser.add_argument("--postgres-dsn", dest="postgres_dsn", default=None)
    parser.add_argument("--explain-env", action="store_true")
    parser.add_argument("--setup-sql-output", type=Path, default=None)
    args = parser.parse_args()
    if args.postgres_dsn:
        args.dsn = args.postgres_dsn
    if args.setup_sql_output:
        args.setup_sql_output.parent.mkdir(parents=True, exist_ok=True)
        args.setup_sql_output.write_text(setup_sql())
        print(f"wrote PostgreSQL setup SQL to {args.setup_sql_output}")
        return 0
    if args.explain_env:
        print(explain_env(args.dsn))
        return 0

    try:
        import qm_engine  # type: ignore
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2

    try:
        qm, qm_metadata = run_qm(
            qm_engine,
            args.iterations,
            args.durability_mode,
            args.qm_mode,
            args.qm_sync_policy,
            args.sync_every_n,
            args.sync_interval_ms,
            args.keep_artifacts,
        )
    except Exception as exc:
        print(f"NativeSqlEngine benchmark failed: {exc}", file=sys.stderr)
        return 4
    pg, skip_reason, pg_settings = run_pg(args.dsn, args.iterations, args.durability_mode)

    rows = []
    if pg is not None:
        for name, qm_item in qm.items():
            pg_item = pg.get(name)
            if not pg_item:
                continue
            qm_ops = qm_item["throughput_ops_sec"]
            pg_ops = pg_item["throughput_ops_sec"]
            winner = "QM" if qm_ops >= pg_ops else "PostgreSQL"
            ratio = (qm_ops / pg_ops) if pg_ops else None
            rows.append({
                "workload": name,
                "qm": qm_item,
                "postgresql": pg_item,
                "winner": winner,
                "qm_ops_ratio_vs_postgresql": ratio,
            })

    report = {
        "schema_version": 1,
        "environment": {
            "os": platform.platform(),
            "machine": platform.machine(),
            "python_version": platform.python_version(),
            "rust_version": command_version(["rustc", "--version"]),
            "psql_version": command_version(["psql", "--version"]),
            "peak_rss_mb": max_rss_mb(),
        },
        "dsn_sanitized": sanitize_dsn(args.dsn),
        "durability_mode": args.durability_mode,
        "requested_durability_mode": args.durability_mode,
        "qm_mode": qm_metadata["qm_mode"],
        "qm_sync_policy": qm_metadata["qm_sync_policy"],
        "qm_durability_mode": qm_metadata["qm_durability_mode"],
        "qm_storage_path": qm_metadata["qm_storage_path"],
        "wal_enabled": qm_metadata["wal_enabled"],
        "fsync_enabled": qm_metadata["fsync_enabled"],
        "sync_before_commit_return": qm_metadata["sync_before_commit_return"],
        "sync_every_n": qm_metadata["sync_every_n"],
        "sync_interval_ms": qm_metadata["sync_interval_ms"],
        "acknowledged_before_fsync": qm_metadata["acknowledged_before_fsync"],
        "durability_window_description": qm_metadata["durability_window_description"],
        "autocommit_mode": qm_metadata["autocommit_mode"],
        "reopen_validation_passed": qm_metadata["reopen_validation_passed"],
        "claim_scope": qm_metadata["claim_scope"],
        "qm_metadata": qm_metadata,
        "postgresql_durability_mode": (
            "synchronous_commit=off"
            if args.durability_mode in {"relaxed", "wal_no_fsync"}
            else "synchronous_commit=on"
        ),
        "postgresql_available": pg is not None,
        "postgresql_skip_reason": skip_reason,
        "postgresql_settings": pg_settings,
        "iterations": args.iterations,
        "fairness_rules": [
            "same column shape: integer primary key, integer predicate column, text payload",
            "same query shapes for supported workloads",
            "warm-cache loops with 10 warmup iterations",
            "PostgreSQL synchronous_commit is recorded and set according to --durability-mode where possible",
            "NativeSqlEngine memory mode uses default in-memory/autocommit and is not durability-equivalent to PostgreSQL",
            "NativeSqlEngine persistent-wal mode uses a real data_dir and records the selected sync_wal policy",
            "NativeSqlEngine --qm-sync-policy controls engine-native sync for per-mutation/per-commit and benchmark-controller sync for group-commit",
            "Group-commit, append-only-profile, and relaxed-os-buffered modes are not synchronous durability claims unless sync_before_commit_return=true",
            "NativeSqlEngine comparison mode is recorded separately and must not be mixed with unmatched PostgreSQL durability claims",
        ],
        "qm_results": qm,
        "postgresql_results": pg,
        "comparison": rows,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, indent=2, sort_keys=True))
    if args.strict and pg is None:
        print(f"strict PostgreSQL comparison unavailable: {skip_reason}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
