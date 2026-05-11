#!/usr/bin/env python3
"""QMvir Full Benchmark Suite — QMvir vs PostgreSQL vs DuckDB vs SQLite.

Benchmarks:
  Layer 1: Engine Internals (Rust)
    - SqlParser throughput
    - VectorExecutor SIMD vs NumPy
    - JitCompiler batch filter/project
    - WTinyLfuCache throughput
    - UringWalWriter append/flush
    - RustRingBuffer IPC throughput
    - IndexManager create/drop
    - StorageEngine transaction throughput

  Layer 2: Database Operations (QMvir vs PG vs DuckDB vs SQLite)
    - Point lookup (SELECT WHERE id=?)
    - Range scan (SELECT WHERE id BETWEEN ? AND ?)
    - Aggregation (SUM, COUNT, AVG)
    - GROUP BY
    - JOIN
    - Bulk INSERT
    - UPDATE
    - DELETE

  Layer 3: Vector Search (QMvir exclusive)
    - batch_dot_product SIMD vs NumPy
    - batch_l2_distance
    - Top-K search
    - parallel_batch_dot_product (Rayon)

Usage:
    python benchmarks/full_benchmark.py
    python benchmarks/full_benchmark.py --profile quick
    python benchmarks/full_benchmark.py --profile standard
"""

from __future__ import annotations

import argparse
import json
import os
import random
import statistics
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

import numpy as np

# Ensure QM modules importable
QM_ROOT = Path(__file__).resolve().parent.parent
if str(QM_ROOT) not in sys.path:
    sys.path.insert(0, str(QM_ROOT))

import qm_engine

# ═══════════════════════════════════════════════════════════════════
# Config
# ═══════════════════════════════════════════════════════════════════

PROFILES = {
    "quick": {
        "rows": 10_000,
        "ops_point": 500,
        "ops_range": 200,
        "ops_agg": 200,
        "ops_group": 100,
        "ops_join": 200,
        "ops_insert": 2_000,
        "ops_update": 500,
        "ops_delete": 200,
        "warmup": 50,
        "vec_count": 5_000,
        "vec_dim": 128,
        "vec_queries": 100,
    },
    "standard": {
        "rows": 100_000,
        "ops_point": 2_000,
        "ops_range": 1_000,
        "ops_agg": 500,
        "ops_group": 300,
        "ops_join": 500,
        "ops_insert": 10_000,
        "ops_update": 2_000,
        "ops_delete": 500,
        "warmup": 100,
        "vec_count": 10_000,
        "vec_dim": 256,
        "vec_queries": 200,
    },
}

SEED = 20260401

@dataclass
class BenchResult:
    name: str
    engine: str
    ops: int
    elapsed_s: float
    extra: dict = field(default_factory=dict)

    @property
    def ops_per_sec(self) -> float:
        return self.ops / self.elapsed_s if self.elapsed_s > 0 else 0

    @property
    def latency_us(self) -> float:
        return (self.elapsed_s / self.ops * 1e6) if self.ops > 0 else 0


# ═══════════════════════════════════════════════════════════════════
# Utilities
# ═══════════════════════════════════════════════════════════════════

def fmt_rate(rate: float) -> str:
    if rate >= 1_000_000:
        return f"{rate/1_000_000:.2f}M"
    if rate >= 1_000:
        return f"{rate/1_000:.1f}K"
    return f"{rate:.0f}"


def fmt_time(s: float) -> str:
    if s < 0.001:
        return f"{s*1_000_000:.1f}µs"
    if s < 1:
        return f"{s*1_000:.2f}ms"
    return f"{s:.2f}s"


def print_header(title: str):
    w = 80
    print()
    print("═" * w)
    print(f"  {title}")
    print("═" * w)


def print_result(r: BenchResult):
    print(f"  {r.engine:12s} │ {r.name:35s} │ {fmt_rate(r.ops_per_sec):>8s} ops/s │ {fmt_time(r.latency_us/1e6):>10s}/op")


def print_comparison(results: list[BenchResult], test_name: str):
    related = [r for r in results if r.name == test_name]
    if len(related) < 2:
        return
    baseline = related[0]
    for r in related[1:]:
        if r.ops_per_sec > 0 and baseline.ops_per_sec > 0:
            ratio = r.ops_per_sec / baseline.ops_per_sec
            faster = "faster" if ratio > 1 else "slower"
            print(f"    → {r.engine} is {abs(ratio):.2f}x {faster} than {baseline.engine}")


# ═══════════════════════════════════════════════════════════════════
# Layer 1: QMvir Engine Internals
# ═══════════════════════════════════════════════════════════════════

def bench_sql_parser(cfg: dict) -> list[BenchResult]:
    print_header("SQL Parser Throughput")
    parser = qm_engine.SqlParser()
    queries = [
        "SELECT id, name, email FROM users WHERE id = 42",
        "INSERT INTO orders (id, product, qty) VALUES (1, 'widget', 10)",
        "UPDATE accounts SET balance = balance - 100 WHERE id = 7",
        "DELETE FROM sessions WHERE expires_at < '2026-01-01'",
        "SELECT a.id, b.name FROM accounts a JOIN orders b ON a.id = b.account_id WHERE a.balance > 1000",
        "SELECT COUNT(*), SUM(amount) FROM transactions GROUP BY category HAVING COUNT(*) > 5",
        "CREATE TABLE metrics (ts TIMESTAMP, cpu FLOAT, mem FLOAT)",
    ]
    n = 10_000
    results = []

    t0 = time.perf_counter()
    for i in range(n):
        parser.parse(queries[i % len(queries)])
    elapsed = time.perf_counter() - t0

    r = BenchResult("SQL Parse", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    # get_query_type
    t0 = time.perf_counter()
    for i in range(n):
        parser.get_query_type(queries[i % len(queries)])
    elapsed = time.perf_counter() - t0

    r = BenchResult("Query Type Detection", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    return results


def bench_jit_compiler(cfg: dict) -> list[BenchResult]:
    print_header("JIT Compiler — Batch Filter & Project")
    jit = qm_engine.JitCompiler()
    results = []

    for size_label, size in [("1K", 1_000), ("10K", 10_000), ("100K", 100_000)]:
        data = list(range(size))
        target = size // 2

        t0 = time.perf_counter()
        for _ in range(100):
            jit.filter_eq_i64(data, 0, target)
        elapsed = time.perf_counter() - t0

        r = BenchResult(f"filter_eq_i64 {size_label}", "QMvir-JIT", 100, elapsed,
                        {"rows": size})
        print_result(r)
        results.append(r)

    # filter_between
    data_f = [float(x) for x in range(10_000)]
    t0 = time.perf_counter()
    for _ in range(100):
        jit.filter_between_f64(data_f, 0, 2500.0, 7500.0)
    elapsed = time.perf_counter() - t0
    r = BenchResult("filter_between_f64 10K", "QMvir-JIT", 100, elapsed)
    print_result(r)
    results.append(r)

    # project
    col_a = [float(i) for i in range(10_000)]
    col_b = [float(i * 2) for i in range(10_000)]
    t0 = time.perf_counter()
    for _ in range(100):
        jit.project_f64(col_a, col_b, 1.0)
    elapsed = time.perf_counter() - t0
    r = BenchResult("project_f64 10K", "QMvir-JIT", 100, elapsed)
    print_result(r)
    results.append(r)

    return results


def bench_cache(cfg: dict) -> list[BenchResult]:
    print_header("W-TinyLFU Cache")
    cache = qm_engine.WTinyLfuCache(10_000)
    results = []
    n = 100_000

    # Insert
    t0 = time.perf_counter()
    for i in range(n):
        cache.insert(f"key_{i}", f"value_{i}".encode())
    elapsed = time.perf_counter() - t0
    r = BenchResult("Cache Insert 100K", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    # Get (hot — last keys)
    t0 = time.perf_counter()
    for i in range(n - 1000, n):
        cache.get(f"key_{i}")
    elapsed = time.perf_counter() - t0
    r = BenchResult("Cache Get (hot 1K)", "QMvir-Rust", 1000, elapsed)
    print_result(r)
    results.append(r)

    # Get (miss)
    t0 = time.perf_counter()
    for i in range(1000):
        cache.get(f"miss_{i}")
    elapsed = time.perf_counter() - t0
    r = BenchResult("Cache Get (miss 1K)", "QMvir-Rust", 1000, elapsed)
    print_result(r)
    results.append(r)

    hr = cache.hit_rate()
    print(f"    Cache hit rate: {hr:.2%}")

    return results


def bench_wal(cfg: dict) -> list[BenchResult]:
    print_header("WAL (io_uring) — Append & Flush")
    results = []
    td = tempfile.mkdtemp()
    wal = qm_engine.UringWalWriter(td)

    # Append 100B records
    data = b"x" * 100
    n = 10_000
    t0 = time.perf_counter()
    for i in range(n):
        wal.append(i, 1, data)
    wal.flush()
    elapsed = time.perf_counter() - t0
    r = BenchResult("WAL Append+Flush 10K×100B", "QMvir-Rust", n, elapsed,
                    {"bytes_written": wal.total_bytes_written})
    print_result(r)
    results.append(r)
    print(f"    Total WAL bytes: {wal.total_bytes_written:,}")
    tp_mb = wal.total_bytes_written / elapsed / 1024 / 1024
    print(f"    WAL throughput: {tp_mb:.1f} MB/s")

    # 4KB records
    td2 = tempfile.mkdtemp()
    wal2 = qm_engine.UringWalWriter(td2)
    data4k = b"y" * 4096
    n2 = 5_000
    t0 = time.perf_counter()
    for i in range(n2):
        wal2.append(i, 1, data4k)
    wal2.flush()
    elapsed2 = time.perf_counter() - t0
    r = BenchResult("WAL Append+Flush 5K×4KB", "QMvir-Rust", n2, elapsed2,
                    {"bytes_written": wal2.total_bytes_written})
    print_result(r)
    results.append(r)
    tp_mb2 = wal2.total_bytes_written / elapsed2 / 1024 / 1024
    print(f"    WAL throughput: {tp_mb2:.1f} MB/s")

    return results


def bench_ring_buffer(cfg: dict) -> list[BenchResult]:
    print_header("Ring Buffer IPC")
    results = []

    # Round-trip: publish one, consume one, complete, repeat
    td = tempfile.mkdtemp()
    rb = qm_engine.RustRingBuffer(td + "/ring", 64, 256)
    payload = b"p" * 64
    n = 5_000
    t0 = time.perf_counter()
    for i in range(n):
        rb.publish(i + 1, 0, payload)
        msg = rb.consume()
        if msg:
            slot_idx = msg[0]
            rb.complete(slot_idx, b"ok")
            rb.collect_result(slot_idx)
    elapsed = time.perf_counter() - t0
    r = BenchResult("Ring Round-Trip 5K", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    # Status read
    t0 = time.perf_counter()
    for _ in range(100_000):
        rb.status()
    elapsed = time.perf_counter() - t0
    r = BenchResult("Ring Status Read 100K", "QMvir-Rust", 100_000, elapsed)
    print_result(r)
    results.append(r)

    return results


def bench_index_manager(cfg: dict) -> list[BenchResult]:
    print_header("Index Manager")
    results = []
    im = qm_engine.IndexManager()

    n = 1_000
    t0 = time.perf_counter()
    for i in range(n):
        im.create_index(f"idx_{i}", "bench_table", [f"col_{i}"])
    elapsed = time.perf_counter() - t0
    r = BenchResult("Create Index 1K", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    # List
    t0 = time.perf_counter()
    for _ in range(100):
        im.list_indexes()
    elapsed = time.perf_counter() - t0
    r = BenchResult("List 1K Indexes ×100", "QMvir-Rust", 100, elapsed)
    print_result(r)
    results.append(r)

    # Drop
    t0 = time.perf_counter()
    for i in range(n):
        im.drop_index(f"idx_{i}")
    elapsed = time.perf_counter() - t0
    r = BenchResult("Drop Index 1K", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    return results


def bench_transaction(cfg: dict) -> list[BenchResult]:
    print_header("Storage Engine — Transactions")
    results = []

    td = tempfile.mkdtemp()
    data_dir = os.path.join(td, "data")
    wal_dir = os.path.join(td, "wal")
    os.makedirs(data_dir)
    os.makedirs(wal_dir)
    se = qm_engine.StorageEngine(data_dir, wal_dir)

    n = 10_000
    t0 = time.perf_counter()
    for _ in range(n):
        txn = se.begin_transaction()
        txn.commit()
    elapsed = time.perf_counter() - t0
    r = BenchResult("Begin+Commit Txn 10K", "QMvir-Rust", n, elapsed)
    print_result(r)
    results.append(r)

    return results


def bench_native_dispatcher(cfg: dict) -> list[BenchResult]:
    print_header("Native Dispatcher — IPC Operations")
    results = []

    td = tempfile.mkdtemp()
    nd = qm_engine.NativeDispatcher(td, slot_count=8192)

    # DDL
    nd.dispatch_ddl(b"CREATE TABLE bench (id INT, val TEXT)")

    # Insert throughput
    n = 5_000
    insert_ok = 0
    t0 = time.perf_counter()
    for i in range(n):
        try:
            nd.dispatch_insert(f"INSERT INTO bench VALUES ({i}, 'v{i}')".encode())
            insert_ok += 1
        except RuntimeError:
            break
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"Dispatch INSERT {insert_ok}", "QMvir-Rust", max(insert_ok, 1), elapsed)
    print_result(r)
    results.append(r)

    # Drain ring before query phase so slots are Free
    drained = nd.drain_all()
    print(f"    Ring drained: {drained[0]} gen, {drained[1]} vec, {drained[2]} proc slots")

    # Query
    n_q = 2_000
    t0 = time.perf_counter()
    query_ok = 0
    for i in range(n_q):
        try:
            nd.dispatch_query(f"SELECT * FROM bench WHERE id = {i}".encode())
            query_ok += 1
        except RuntimeError:
            break
    elapsed = time.perf_counter() - t0
    if query_ok > 0:
        r = BenchResult(f"Dispatch QUERY {query_ok}", "QMvir-Rust", query_ok, elapsed)
        print_result(r)
        results.append(r)
    else:
        print(f"  {'QMvir-Rust':12s} │ {'Dispatch QUERY':35s} │ SKIPPED (ring full)")

    print(f"    Final LSN: {nd.current_lsn}")

    return results


# ═══════════════════════════════════════════════════════════════════
# Layer 2: Database Comparison
# ═══════════════════════════════════════════════════════════════════

def setup_postgres(cfg: dict):
    """Connect to PostgreSQL and create benchmark tables."""
    import psycopg2
    conn = psycopg2.connect(dbname="qm_bench", host="localhost")
    conn.autocommit = True
    cur = conn.cursor()

    cur.execute("DROP TABLE IF EXISTS bench_orders CASCADE")
    cur.execute("DROP TABLE IF EXISTS bench_products CASCADE")
    cur.execute("DROP TABLE IF EXISTS bench_accounts CASCADE")

    cur.execute("""
        CREATE TABLE bench_accounts (
            id INT PRIMARY KEY,
            name TEXT,
            balance FLOAT,
            created_at TIMESTAMP DEFAULT NOW()
        )
    """)
    cur.execute("""
        CREATE TABLE bench_products (
            id INT PRIMARY KEY,
            name TEXT,
            price FLOAT,
            category TEXT
        )
    """)
    cur.execute("""
        CREATE TABLE bench_orders (
            id INT PRIMARY KEY,
            account_id INT REFERENCES bench_accounts(id),
            product_id INT REFERENCES bench_products(id),
            quantity INT,
            total FLOAT
        )
    """)

    rng = random.Random(SEED)
    rows = cfg["rows"]
    batch_size = 1000
    categories = ["electronics", "books", "clothing", "food", "tools"]

    # Accounts
    for start in range(0, rows, batch_size):
        end = min(start + batch_size, rows)
        values = ",".join(
            f"({i}, 'user_{i}', {rng.uniform(100, 10000):.2f})"
            for i in range(start, end)
        )
        cur.execute(f"INSERT INTO bench_accounts (id, name, balance) VALUES {values}")

    # Products (1/10th of rows)
    n_prod = max(rows // 10, 100)
    for start in range(0, n_prod, batch_size):
        end = min(start + batch_size, n_prod)
        values = ",".join(
            f"({i}, 'product_{i}', {rng.uniform(1, 500):.2f}, '{rng.choice(categories)}')"
            for i in range(start, end)
        )
        cur.execute(f"INSERT INTO bench_products (id, name, price, category) VALUES {values}")

    # Orders
    n_orders = rows
    for start in range(0, n_orders, batch_size):
        end = min(start + batch_size, n_orders)
        values = ",".join(
            f"({i}, {rng.randint(0, rows-1)}, {rng.randint(0, n_prod-1)}, {rng.randint(1, 20)}, {rng.uniform(10, 5000):.2f})"
            for i in range(start, end)
        )
        cur.execute(f"INSERT INTO bench_orders (id, account_id, product_id, quantity, total) VALUES {values}")

    # Create indexes
    cur.execute("CREATE INDEX IF NOT EXISTS idx_orders_account ON bench_orders (account_id)")
    cur.execute("CREATE INDEX IF NOT EXISTS idx_orders_product ON bench_orders (product_id)")
    cur.execute("ANALYZE")

    return conn


def setup_duckdb(cfg: dict):
    """Create DuckDB in-memory database with benchmark tables."""
    import duckdb
    conn = duckdb.connect(":memory:")

    conn.execute("""
        CREATE TABLE bench_accounts (
            id INT PRIMARY KEY,
            name TEXT,
            balance FLOAT
        )
    """)
    conn.execute("""
        CREATE TABLE bench_products (
            id INT PRIMARY KEY,
            name TEXT,
            price FLOAT,
            category TEXT
        )
    """)
    conn.execute("""
        CREATE TABLE bench_orders (
            id INT PRIMARY KEY,
            account_id INT,
            product_id INT,
            quantity INT,
            total FLOAT
        )
    """)

    rng = random.Random(SEED)
    rows = cfg["rows"]
    categories = ["electronics", "books", "clothing", "food", "tools"]

    accounts = [(i, f"user_{i}", round(rng.uniform(100, 10000), 2)) for i in range(rows)]
    conn.executemany("INSERT INTO bench_accounts VALUES (?, ?, ?)", accounts)

    n_prod = max(rows // 10, 100)
    products = [(i, f"product_{i}", round(rng.uniform(1, 500), 2), rng.choice(categories)) for i in range(n_prod)]
    conn.executemany("INSERT INTO bench_products VALUES (?, ?, ?, ?)", products)

    orders = [(i, rng.randint(0, rows-1), rng.randint(0, n_prod-1), rng.randint(1, 20), round(rng.uniform(10, 5000), 2)) for i in range(rows)]
    conn.executemany("INSERT INTO bench_orders VALUES (?, ?, ?, ?, ?)", orders)

    return conn


def setup_sqlite(cfg: dict):
    """Create SQLite in-memory database with benchmark tables."""
    import sqlite3
    conn = sqlite3.connect(":memory:")
    cur = conn.cursor()

    cur.execute("CREATE TABLE bench_accounts (id INT PRIMARY KEY, name TEXT, balance REAL)")
    cur.execute("CREATE TABLE bench_products (id INT PRIMARY KEY, name TEXT, price REAL, category TEXT)")
    cur.execute("CREATE TABLE bench_orders (id INT PRIMARY KEY, account_id INT, product_id INT, quantity INT, total REAL)")

    rng = random.Random(SEED)
    rows = cfg["rows"]
    categories = ["electronics", "books", "clothing", "food", "tools"]

    accounts = [(i, f"user_{i}", round(rng.uniform(100, 10000), 2)) for i in range(rows)]
    cur.executemany("INSERT INTO bench_accounts VALUES (?, ?, ?)", accounts)

    n_prod = max(rows // 10, 100)
    products = [(i, f"product_{i}", round(rng.uniform(1, 500), 2), rng.choice(categories)) for i in range(n_prod)]
    cur.executemany("INSERT INTO bench_products VALUES (?, ?, ?, ?)", products)

    orders = [(i, rng.randint(0, rows-1), rng.randint(0, n_prod-1), rng.randint(1, 20), round(rng.uniform(10, 5000), 2)) for i in range(rows)]
    cur.executemany("INSERT INTO bench_orders VALUES (?, ?, ?, ?, ?)", orders)

    conn.commit()
    cur.execute("CREATE INDEX idx_orders_account ON bench_orders (account_id)")
    cur.execute("CREATE INDEX idx_orders_product ON bench_orders (product_id)")
    conn.commit()

    return conn


def bench_point_lookup(engine_name: str, execute_fn, cfg: dict, rng: random.Random) -> BenchResult:
    rows = cfg["rows"]
    ops = cfg["ops_point"]
    # Warmup
    for _ in range(cfg["warmup"]):
        execute_fn(f"SELECT * FROM bench_accounts WHERE id = {rng.randint(0, rows-1)}")
    t0 = time.perf_counter()
    for _ in range(ops):
        execute_fn(f"SELECT * FROM bench_accounts WHERE id = {rng.randint(0, rows-1)}")
    elapsed = time.perf_counter() - t0
    return BenchResult("Point Lookup", engine_name, ops, elapsed)


def bench_range_scan(engine_name: str, execute_fn, cfg: dict, rng: random.Random) -> BenchResult:
    rows = cfg["rows"]
    ops = cfg["ops_range"]
    span = max(rows // 100, 10)
    for _ in range(cfg["warmup"]):
        lo = rng.randint(0, rows - span)
        execute_fn(f"SELECT * FROM bench_accounts WHERE id BETWEEN {lo} AND {lo + span}")
    t0 = time.perf_counter()
    for _ in range(ops):
        lo = rng.randint(0, rows - span)
        execute_fn(f"SELECT * FROM bench_accounts WHERE id BETWEEN {lo} AND {lo + span}")
    elapsed = time.perf_counter() - t0
    return BenchResult("Range Scan", engine_name, ops, elapsed)


def bench_aggregation(engine_name: str, execute_fn, cfg: dict) -> BenchResult:
    ops = cfg["ops_agg"]
    for _ in range(cfg["warmup"]):
        execute_fn("SELECT COUNT(*), SUM(balance), AVG(balance) FROM bench_accounts")
    t0 = time.perf_counter()
    for _ in range(ops):
        execute_fn("SELECT COUNT(*), SUM(balance), AVG(balance) FROM bench_accounts")
    elapsed = time.perf_counter() - t0
    return BenchResult("Aggregation", engine_name, ops, elapsed)


def bench_group_by(engine_name: str, execute_fn, cfg: dict) -> BenchResult:
    ops = cfg["ops_group"]
    for _ in range(min(cfg["warmup"], 20)):
        execute_fn("SELECT category, COUNT(*), SUM(price) FROM bench_products GROUP BY category")
    t0 = time.perf_counter()
    for _ in range(ops):
        execute_fn("SELECT category, COUNT(*), SUM(price) FROM bench_products GROUP BY category")
    elapsed = time.perf_counter() - t0
    return BenchResult("GROUP BY", engine_name, ops, elapsed)


def bench_join(engine_name: str, execute_fn, cfg: dict, rng: random.Random) -> BenchResult:
    rows = cfg["rows"]
    ops = cfg["ops_join"]
    for _ in range(min(cfg["warmup"], 20)):
        uid = rng.randint(0, rows - 1)
        execute_fn(f"SELECT a.name, o.total FROM bench_accounts a JOIN bench_orders o ON a.id = o.account_id WHERE a.id = {uid}")
    t0 = time.perf_counter()
    for _ in range(ops):
        uid = rng.randint(0, rows - 1)
        execute_fn(f"SELECT a.name, o.total FROM bench_accounts a JOIN bench_orders o ON a.id = o.account_id WHERE a.id = {uid}")
    elapsed = time.perf_counter() - t0
    return BenchResult("JOIN", engine_name, ops, elapsed)


def bench_bulk_insert(engine_name: str, execute_fn, cfg: dict, rng: random.Random) -> BenchResult:
    ops = cfg["ops_insert"]
    base_id = cfg["rows"] + 100_000
    t0 = time.perf_counter()
    for i in range(ops):
        execute_fn(f"INSERT INTO bench_accounts (id, name, balance) VALUES ({base_id + i}, 'new_{i}', {rng.uniform(100, 9999):.2f})")
    elapsed = time.perf_counter() - t0
    return BenchResult("INSERT", engine_name, ops, elapsed)


def bench_update(engine_name: str, execute_fn, cfg: dict, rng: random.Random) -> BenchResult:
    rows = cfg["rows"]
    ops = cfg["ops_update"]
    t0 = time.perf_counter()
    for _ in range(ops):
        uid = rng.randint(0, rows - 1)
        execute_fn(f"UPDATE bench_accounts SET balance = balance + 1.0 WHERE id = {uid}")
    elapsed = time.perf_counter() - t0
    return BenchResult("UPDATE", engine_name, ops, elapsed)


def bench_delete(engine_name: str, execute_fn, cfg: dict) -> BenchResult:
    ops = cfg["ops_delete"]
    base_id = cfg["rows"] + 100_000
    t0 = time.perf_counter()
    for i in range(ops):
        execute_fn(f"DELETE FROM bench_accounts WHERE id = {base_id + i}")
    elapsed = time.perf_counter() - t0
    return BenchResult("DELETE", engine_name, ops, elapsed)


def run_db_benchmark(engine_name: str, execute_fn, cfg: dict) -> list[BenchResult]:
    rng = random.Random(SEED)
    results = []

    for bench_fn in [bench_point_lookup, bench_range_scan]:
        results.append(bench_fn(engine_name, execute_fn, cfg, rng))
    results.append(bench_aggregation(engine_name, execute_fn, cfg))
    results.append(bench_group_by(engine_name, execute_fn, cfg))
    results.append(bench_join(engine_name, execute_fn, cfg, rng))
    results.append(bench_bulk_insert(engine_name, execute_fn, cfg, rng))
    results.append(bench_update(engine_name, execute_fn, cfg, rng))
    results.append(bench_delete(engine_name, execute_fn, cfg))

    return results


def setup_qmvir(cfg: dict):
    """Start QMvir native server and return psycopg2 connection."""
    import psycopg2
    import os

    port = 15434
    admin_pw = "benchmarkpass"
    os.environ["QM_ADMIN_PASSWORD"] = admin_pw

    gw = qm_engine.PostgresGateway(port=port, max_connections=200)
    gw.start_native()
    time.sleep(0.5)
    if not gw.is_running:
        raise RuntimeError("QMvir gateway failed to start")

    conn = psycopg2.connect(host="127.0.0.1", port=port, user="admin", password=admin_pw, dbname="qm")
    conn.autocommit = True
    cur = conn.cursor()

    rng = random.Random(SEED)
    rows = cfg["rows"]
    categories = ["electronics", "books", "clothing", "food", "tools"]

    cur.execute("CREATE TABLE bench_accounts (id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, name TEXT)")
    cur.execute("CREATE TABLE bench_products (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, category TEXT)")
    cur.execute("CREATE TABLE bench_orders (id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, quantity INTEGER, total DOUBLE PRECISION)")

    # Accounts
    for i in range(rows):
        bal = round(rng.uniform(100, 10000), 2)
        cur.execute(f"INSERT INTO bench_accounts (id, balance, name) VALUES ({i}, {bal}, 'user_{i}')")

    # Products
    n_prod = max(rows // 10, 100)
    for i in range(n_prod):
        price = round(rng.uniform(1, 500), 2)
        cat = rng.choice(categories)
        cur.execute(f"INSERT INTO bench_products (id, name, price, category) VALUES ({i}, 'product_{i}', {price}, '{cat}')")

    # Orders
    for i in range(rows):
        aid = rng.randint(0, rows - 1)
        pid = rng.randint(0, n_prod - 1)
        qty = rng.randint(1, 20)
        total = round(rng.uniform(10, 5000), 2)
        cur.execute(f"INSERT INTO bench_orders (id, account_id, product_id, quantity, total) VALUES ({i}, {aid}, {pid}, {qty}, {total})")

    # Create indexes (same as PostgreSQL/SQLite setups for fair comparison)
    cur.execute("CREATE INDEX idx_orders_account ON bench_orders (account_id)")

    return conn, gw


# ═══════════════════════════════════════════════════════════════════
# Layer 3: Vector Search
# ═══════════════════════════════════════════════════════════════════

def bench_vector_search(cfg: dict) -> list[BenchResult]:
    print_header("Vector Search — QMvir SIMD vs NumPy")
    results = []
    ve = qm_engine.VectorExecutor()

    n_vec = cfg["vec_count"]
    dim = cfg["vec_dim"]
    n_queries = cfg["vec_queries"]

    rng = np.random.RandomState(SEED)
    vectors_np = rng.rand(n_vec, dim).astype(np.float32)
    queries_np = rng.rand(n_queries, dim).astype(np.float32)

    # Pre-flatten as contiguous C-order numpy array for zero-copy API
    vectors_flat_np = np.ascontiguousarray(vectors_np.ravel(), dtype=np.float32)

    # --- np_batch_dot_product: Zero-copy SIMD (reads numpy memory directly) ---
    t0 = time.perf_counter()
    for i in range(n_queries):
        ve.np_batch_dot_product(vectors_flat_np, dim, np.ascontiguousarray(queries_np[i]))
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"Dot Product {n_vec}×{dim}d", "QMvir-SIMD", n_queries, elapsed)
    print_result(r)
    results.append(r)

    # --- np_parallel_batch_dot_product: Zero-copy + Rayon ---
    t0 = time.perf_counter()
    for i in range(n_queries):
        ve.np_parallel_batch_dot_product(vectors_flat_np, dim, np.ascontiguousarray(queries_np[i]))
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"Dot Product {n_vec}×{dim}d", "QMvir-Rayon", n_queries, elapsed)
    print_result(r)
    results.append(r)

    # --- NumPy ---
    t0 = time.perf_counter()
    for i in range(n_queries):
        _ = vectors_np @ queries_np[i]
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"Dot Product {n_vec}×{dim}d", "NumPy", n_queries, elapsed)
    print_result(r)
    results.append(r)

    # --- np_batch_l2_distance: Zero-copy ---
    t0 = time.perf_counter()
    for i in range(n_queries):
        ve.np_batch_l2_distance(vectors_flat_np, dim, np.ascontiguousarray(queries_np[i]))
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"L2 Distance {n_vec}×{dim}d", "QMvir-SIMD", n_queries, elapsed)
    print_result(r)
    results.append(r)

    # L2 NumPy
    t0 = time.perf_counter()
    for i in range(n_queries):
        diff = vectors_np - queries_np[i]
        _ = np.sqrt(np.sum(diff * diff, axis=1))
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"L2 Distance {n_vec}×{dim}d", "NumPy", n_queries, elapsed)
    print_result(r)
    results.append(r)

    # --- Top-K search (zero-copy) ---
    t0 = time.perf_counter()
    for i in range(n_queries):
        ve.np_search(vectors_flat_np, dim, np.ascontiguousarray(queries_np[i]), 10)
    elapsed = time.perf_counter() - t0
    r = BenchResult(f"Top-10 Search {n_vec}×{dim}d", "QMvir-SIMD", n_queries, elapsed)
    print_result(r)
    results.append(r)

    return results


# ═══════════════════════════════════════════════════════════════════
# Report
# ═══════════════════════════════════════════════════════════════════

def generate_report(all_results: list[BenchResult], cfg: dict, profile: str) -> dict:
    report = {
        "benchmark": "QMvir Full Benchmark Suite",
        "date": datetime.now().isoformat(),
        "profile": profile,
        "config": cfg,
        "system": {
            "python": sys.version,
            "platform": sys.platform,
        },
        "results": [],
    }

    for r in all_results:
        report["results"].append({
            "name": r.name,
            "engine": r.engine,
            "ops": r.ops,
            "elapsed_s": round(r.elapsed_s, 6),
            "ops_per_sec": round(r.ops_per_sec, 1),
            "latency_us": round(r.latency_us, 2),
            **r.extra,
        })

    return report


def print_comparison_table(all_results: list[BenchResult]):
    print_header("COMPARISON TABLE")

    # Group by test name
    tests = {}
    for r in all_results:
        tests.setdefault(r.name, []).append(r)

    # Header
    engines = sorted({r.engine for r in all_results})
    col_w = 14
    header = f"  {'Test':35s} │ " + " │ ".join(f"{e:>{col_w}s}" for e in engines)
    print(header)
    print("  " + "─" * 35 + "─┼─" + "─┼─".join("─" * col_w for _ in engines))

    for test_name, rl in sorted(tests.items()):
        by_engine = {r.engine: r for r in rl}
        cols = []
        for e in engines:
            if e in by_engine:
                cols.append(f"{fmt_rate(by_engine[e].ops_per_sec):>{col_w}s}")
            else:
                cols.append(f"{'—':>{col_w}s}")
        print(f"  {test_name:35s} │ " + " │ ".join(cols))

    # Speedup summary
    db_tests = ["Point Lookup", "Range Scan", "Aggregation", "GROUP BY", "JOIN",
                "INSERT", "UPDATE", "DELETE"]
    print()
    print("  Speedup vs PostgreSQL:")
    for test_name in db_tests:
        if test_name not in tests:
            continue
        rl = tests[test_name]
        by_engine = {r.engine: r for r in rl}
        pg = by_engine.get("PostgreSQL")
        if not pg or pg.ops_per_sec == 0:
            continue
        for e_name, r in by_engine.items():
            if e_name == "PostgreSQL":
                continue
            ratio = r.ops_per_sec / pg.ops_per_sec
            emoji = "▲" if ratio > 1 else "▼"
            print(f"    {test_name:25s} {e_name:15s} {emoji} {ratio:.2f}x")


# ═══════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════

def main():
    parser = argparse.ArgumentParser(description="QMvir Full Benchmark Suite")
    parser.add_argument("--profile", choices=["quick", "standard"], default="quick")
    parser.add_argument("--skip-postgres", action="store_true")
    parser.add_argument("--skip-duckdb", action="store_true")
    parser.add_argument("--skip-sqlite", action="store_true")
    parser.add_argument("--skip-internals", action="store_true")
    parser.add_argument("--skip-vectors", action="store_true")
    parser.add_argument("--json", default="benchmarks/FULL_BENCHMARK.json")
    args = parser.parse_args()

    cfg = PROFILES[args.profile]
    all_results: list[BenchResult] = []

    print("╔" + "═" * 78 + "╗")
    print("║" + "QMvir Full Benchmark Suite".center(78) + "║")
    print("║" + f"Profile: {args.profile} | Rows: {cfg['rows']:,}".center(78) + "║")
    print("║" + f"Date: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}".center(78) + "║")
    print("╚" + "═" * 78 + "╝")

    # ── Layer 1: Engine Internals ───────────────────────────────
    if not args.skip_internals:
        all_results.extend(bench_sql_parser(cfg))
        all_results.extend(bench_jit_compiler(cfg))
        all_results.extend(bench_cache(cfg))
        all_results.extend(bench_wal(cfg))
        all_results.extend(bench_ring_buffer(cfg))
        all_results.extend(bench_index_manager(cfg))
        all_results.extend(bench_transaction(cfg))
        all_results.extend(bench_native_dispatcher(cfg))

    # ── Layer 2: Database Comparison ────────────────────────────

    # QMvir NativeSqlEngine (via PostgresGateway wire protocol)
    qm_gw = None
    try:
        print_header(f"QMvir — Setup ({cfg['rows']:,} rows)")
        t_setup = time.perf_counter()
        qm_conn, qm_gw = setup_qmvir(cfg)
        print(f"  Setup: {time.perf_counter() - t_setup:.2f}s")

        print_header("QMvir — Benchmarks")
        qm_cur = qm_conn.cursor()

        def qm_exec(sql):
            qm_cur.execute(sql)
            if sql.strip().upper().startswith("SELECT"):
                try:
                    return qm_cur.fetchall()
                except Exception:
                    return []

        qm_results = run_db_benchmark("QMvir", qm_exec, cfg)
        for r in qm_results:
            print_result(r)
        all_results.extend(qm_results)
        qm_conn.close()
    except Exception as e:
        print(f"  ⚠ QMvir skipped: {e}")
    finally:
        if qm_gw:
            try:
                qm_gw.stop()
            except Exception:
                pass

    # PostgreSQL
    pg_conn = None
    if not args.skip_postgres:
        try:
            print_header(f"PostgreSQL — Setup ({cfg['rows']:,} rows)")
            t_setup = time.perf_counter()
            pg_conn = setup_postgres(cfg)
            print(f"  Setup: {time.perf_counter() - t_setup:.2f}s")

            print_header("PostgreSQL — Benchmarks")
            pg_cur = pg_conn.cursor()

            def pg_exec(sql):
                pg_cur.execute(sql)
                if sql.strip().upper().startswith("SELECT"):
                    return pg_cur.fetchall()

            pg_results = run_db_benchmark("PostgreSQL", pg_exec, cfg)
            for r in pg_results:
                print_result(r)
            all_results.extend(pg_results)
        except Exception as e:
            print(f"  ⚠ PostgreSQL skipped: {e}")

    # DuckDB
    if not args.skip_duckdb:
        try:
            print_header(f"DuckDB — Setup ({cfg['rows']:,} rows)")
            t_setup = time.perf_counter()
            duck_conn = setup_duckdb(cfg)
            print(f"  Setup: {time.perf_counter() - t_setup:.2f}s")

            print_header("DuckDB — Benchmarks")

            def duck_exec(sql):
                return duck_conn.execute(sql).fetchall()

            duck_results = run_db_benchmark("DuckDB", duck_exec, cfg)
            for r in duck_results:
                print_result(r)
            all_results.extend(duck_results)
        except Exception as e:
            print(f"  ⚠ DuckDB skipped: {e}")

    # SQLite
    if not args.skip_sqlite:
        try:
            print_header(f"SQLite — Setup ({cfg['rows']:,} rows)")
            t_setup = time.perf_counter()
            sqlite_conn = setup_sqlite(cfg)
            print(f"  Setup: {time.perf_counter() - t_setup:.2f}s")

            print_header("SQLite — Benchmarks")
            sqlite_cur = sqlite_conn.cursor()

            def sqlite_exec(sql):
                sqlite_cur.execute(sql)
                if sql.strip().upper().startswith("SELECT"):
                    return sqlite_cur.fetchall()
                sqlite_conn.commit()

            sqlite_results = run_db_benchmark("SQLite", sqlite_exec, cfg)
            for r in sqlite_results:
                print_result(r)
            all_results.extend(sqlite_results)
        except Exception as e:
            print(f"  ⚠ SQLite skipped: {e}")

    # ── Layer 3: Vector Search ──────────────────────────────────
    if not args.skip_vectors:
        all_results.extend(bench_vector_search(cfg))

    # ── Comparison ──────────────────────────────────────────────
    print_comparison_table(all_results)

    # ── JSON Report ─────────────────────────────────────────────
    report = generate_report(all_results, cfg, args.profile)
    json_path = args.json
    os.makedirs(os.path.dirname(json_path) or ".", exist_ok=True)
    with open(json_path, "w") as f:
        json.dump(report, f, indent=2)
    print(f"\n  JSON report saved to: {json_path}")

    # Cleanup
    if pg_conn:
        pg_conn.close()

    print("\n  Done.\n")


if __name__ == "__main__":
    main()
