#!/usr/bin/env python3
"""QMvir vs PostgreSQL vs DuckDB — Comprehensive Database Benchmark.

Tests:
  1. Point lookup (SELECT WHERE id=?)
  2. Range scan (SELECT WHERE id BETWEEN ? AND ?)
  3. Aggregation (SUM, COUNT, AVG)
  4. GROUP BY aggregation
  5. JOIN (2-table, 3-table)
  6. Bulk INSERT
  7. UPDATE single row
  8. DELETE + re-insert
  9. OLAP scan (full table SUM with filter)

Usage:
    python benchmarks/qmvir_vs_postgres_duckdb_bench.py --profile quick
    python benchmarks/qmvir_vs_postgres_duckdb_bench.py --profile standard
    python benchmarks/qmvir_vs_postgres_duckdb_bench.py --profile heavy
"""

from __future__ import annotations

import argparse
import json
import os
import random
import statistics
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

import psycopg2
import duckdb

# ── Configuration ───────────────────────────────────────────────

SEED = 20260312

PROFILES = {
    "quick": {
        "rows_accounts": 10_000,
        "rows_products": 1_000,
        "rows_orders": 50_000,
        "ops_point": 500,
        "ops_range": 300,
        "ops_agg": 300,
        "ops_group": 200,
        "ops_join": 300,
        "ops_insert": 2_000,
        "ops_update": 500,
        "ops_olap": 200,
        "warmup": 50,
    },
    "standard": {
        "rows_accounts": 50_000,
        "rows_products": 5_000,
        "rows_orders": 200_000,
        "ops_point": 1_000,
        "ops_range": 500,
        "ops_agg": 500,
        "ops_group": 300,
        "ops_join": 500,
        "ops_insert": 5_000,
        "ops_update": 1_000,
        "ops_olap": 300,
        "warmup": 100,
    },
    "heavy": {
        "rows_accounts": 200_000,
        "rows_products": 10_000,
        "rows_orders": 1_000_000,
        "ops_point": 3_000,
        "ops_range": 1_000,
        "ops_agg": 1_000,
        "ops_group": 500,
        "ops_join": 1_000,
        "ops_insert": 10_000,
        "ops_update": 2_000,
        "ops_olap": 500,
        "warmup": 200,
    },
}


# ── Helpers ─────────────────────────────────────────────────────

def quantile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    sv = sorted(values)
    idx = max(0, min(len(sv) - 1, int(round((p / 100.0) * (len(sv) - 1)))))
    return sv[idx]


def bench_stats(latencies_ms: list[float], elapsed_s: float, ops: int) -> dict[str, float]:
    return {
        "ops": ops,
        "qps": ops / elapsed_s if elapsed_s > 0 else 0,
        "avg_ms": statistics.mean(latencies_ms) if latencies_ms else 0,
        "p50_ms": quantile(latencies_ms, 50),
        "p95_ms": quantile(latencies_ms, 95),
        "p99_ms": quantile(latencies_ms, 99),
        "min_ms": min(latencies_ms) if latencies_ms else 0,
        "max_ms": max(latencies_ms) if latencies_ms else 0,
    }


# ── Database Adapters ───────────────────────────────────────────

class PostgresAdapter:
    name = "PostgreSQL"

    def __init__(self, host: str, port: int, user: str, dbname: str, password: str = ""):
        params: dict[str, Any] = {"host": host, "port": port, "user": user, "dbname": dbname, "connect_timeout": 10}
        if password:
            params["password"] = password
        self.conn = psycopg2.connect(**params)
        self.conn.autocommit = True
        self.cur = self.conn.cursor()
        self.placeholder = "%s"

    def execute(self, sql: str, params: tuple = ()):
        self.cur.execute(sql, params)

    def fetchall(self) -> list:
        return self.cur.fetchall()

    def fetchone(self):
        return self.cur.fetchone()

    def close(self):
        self.cur.close()
        self.conn.close()


class QMvirAdapter(PostgresAdapter):
    name = "QMvir"


class DuckDBAdapter:
    name = "DuckDB"

    def __init__(self, path: str = ":memory:"):
        self.conn = duckdb.connect(path)
        self.cur = self.conn.cursor()
        self.placeholder = "?"

    def execute(self, sql: str, params: tuple = ()):
        # DuckDB uses $1, $2 positional params or ? placeholders
        # Convert %s to ? for compatibility
        sql_duck = sql.replace("%s", "?")
        if params:
            self.cur.execute(sql_duck, list(params))
        else:
            self.cur.execute(sql_duck)

    def fetchall(self) -> list:
        try:
            return self.cur.fetchall()
        except Exception:
            return []

    def fetchone(self):
        try:
            return self.cur.fetchone()
        except Exception:
            return None

    def close(self):
        self.conn.close()


# ── Data Setup ──────────────────────────────────────────────────

CATEGORIES = ["electronics", "books", "clothing", "food", "toys", "sports", "auto", "garden"]


def setup_tables(db, cfg: dict, rng: random.Random, label: str):
    """Create and populate benchmark tables."""
    n_acc = cfg["rows_accounts"]
    n_prod = cfg["rows_products"]
    n_ord = cfg["rows_orders"]

    print(f"  [{label}] Creating tables ({n_acc:,} accounts, {n_prod:,} products, {n_ord:,} orders)...")

    # Drop existing
    for t in ("bench_orders", "bench_products", "bench_accounts"):
        try:
            db.execute(f"DROP TABLE IF EXISTS {t}")
        except Exception:
            try:
                db.execute(f"DROP TABLE {t}")
            except Exception:
                pass

    ph = db.placeholder

    db.execute("CREATE TABLE bench_accounts (id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, name TEXT, region TEXT)")
    db.execute("CREATE TABLE bench_products (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, category TEXT)")
    db.execute("CREATE TABLE bench_orders (id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, quantity INTEGER, total DOUBLE PRECISION)")

    # Insert accounts
    regions = ["us-east", "us-west", "eu-west", "eu-east", "ap-south", "ap-north"]
    for i in range(1, n_acc + 1):
        db.execute(
            f"INSERT INTO bench_accounts (id, balance, name, region) VALUES ({ph}, {ph}, {ph}, {ph})",
            (i, round(1000 + rng.random() * 9000, 2), f"user_{i}", rng.choice(regions)),
        )

    # Insert products
    for i in range(1, n_prod + 1):
        db.execute(
            f"INSERT INTO bench_products (id, name, price, category) VALUES ({ph}, {ph}, {ph}, {ph})",
            (i, f"product_{i}", round(rng.uniform(5, 500), 2), rng.choice(CATEGORIES)),
        )

    # Insert orders
    for i in range(1, n_ord + 1):
        aid = rng.randint(1, n_acc)
        pid = rng.randint(1, n_prod)
        qty = rng.randint(1, 20)
        total = round(qty * rng.uniform(5, 500), 2)
        db.execute(
            f"INSERT INTO bench_orders (id, account_id, product_id, quantity, total) VALUES ({ph}, {ph}, {ph}, {ph}, {ph})",
            (i, aid, pid, qty, total),
        )

    # Create indexes
    if isinstance(db, DuckDBAdapter):
        # DuckDB auto-indexes PRIMARY KEY, no explicit CREATE INDEX needed for our queries
        pass
    else:
        try:
            db.execute("CREATE INDEX IF NOT EXISTS idx_orders_account ON bench_orders(account_id)")
        except Exception:
            try:
                db.execute("CREATE INDEX idx_bo_acc ON bench_orders (account_id)")
            except Exception:
                pass
        try:
            db.execute("CREATE INDEX IF NOT EXISTS idx_orders_product ON bench_orders(product_id)")
        except Exception:
            try:
                db.execute("CREATE INDEX idx_bo_prod ON bench_orders (product_id)")
            except Exception:
                pass

    print(f"  [{label}] Data loaded.")


# ── Benchmark Runners ───────────────────────────────────────────

def run_bench(db, sql: str, params_fn, ops: int, warmup: int, fetch: bool = True) -> dict:
    ph = db.placeholder
    sql_final = sql.replace("%s", ph) if ph != "%s" else sql

    # Warmup
    for _ in range(warmup):
        db.execute(sql_final, params_fn())
        if fetch:
            db.fetchall()

    lat_ms = []
    t0 = time.perf_counter()
    for _ in range(ops):
        s = time.perf_counter()
        db.execute(sql_final, params_fn())
        if fetch:
            db.fetchall()
        lat_ms.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0

    return bench_stats(lat_ms, elapsed, ops)


def benchmark_point_lookup(db, cfg: dict, rng: random.Random) -> dict:
    n = cfg["rows_accounts"]
    return run_bench(
        db,
        "SELECT id, balance, name, region FROM bench_accounts WHERE id = %s",
        lambda: (rng.randint(1, n),),
        cfg["ops_point"], cfg["warmup"],
    )


def benchmark_range_scan(db, cfg: dict, rng: random.Random) -> dict:
    n = cfg["rows_accounts"]
    def params():
        lo = rng.randint(1, max(2, n - 1000))
        return (lo, lo + rng.randint(100, 1000))
    return run_bench(
        db,
        "SELECT id, balance, name FROM bench_accounts WHERE id BETWEEN %s AND %s",
        params, cfg["ops_range"], cfg["warmup"],
    )


def benchmark_aggregation(db, cfg: dict, rng: random.Random) -> dict:
    n = cfg["rows_accounts"]
    def params():
        lo = rng.randint(1, max(2, n - 2000))
        return (lo, lo + rng.randint(500, 2000))
    return run_bench(
        db,
        "SELECT SUM(total), COUNT(*) FROM bench_orders WHERE account_id BETWEEN %s AND %s",
        params, cfg["ops_agg"], cfg["warmup"],
    )


def benchmark_group_by(db, cfg: dict, rng: random.Random) -> dict:
    return run_bench(
        db,
        "SELECT category, SUM(price), COUNT(*) FROM bench_products GROUP BY category",
        lambda: (), cfg["ops_group"], cfg["warmup"],
    )


def benchmark_join_2table(db, cfg: dict, rng: random.Random) -> dict:
    n = cfg["rows_accounts"]
    return run_bench(
        db,
        "SELECT a.name, o.quantity, o.total FROM bench_accounts a JOIN bench_orders o ON a.id = o.account_id WHERE a.id = %s",
        lambda: (rng.randint(1, n),),
        cfg["ops_join"], cfg["warmup"],
    )


def benchmark_join_3table(db, cfg: dict, rng: random.Random) -> dict:
    n = cfg["rows_accounts"]
    return run_bench(
        db,
        "SELECT a.name, o.quantity, p.name, o.total FROM bench_accounts a JOIN bench_orders o ON a.id = o.account_id JOIN bench_products p ON o.product_id = p.id WHERE a.id = %s",
        lambda: (rng.randint(1, n),),
        cfg["ops_join"], cfg["warmup"],
    )


def benchmark_insert(db, cfg: dict, rng: random.Random) -> dict:
    """Benchmark INSERT into a fresh table."""
    try:
        db.execute("DROP TABLE IF EXISTS bench_insert_test")
    except Exception:
        try:
            db.execute("DROP TABLE bench_insert_test")
        except Exception:
            pass

    db.execute("CREATE TABLE bench_insert_test (id INTEGER PRIMARY KEY, val DOUBLE PRECISION, label TEXT)")

    ph = db.placeholder
    sql = f"INSERT INTO bench_insert_test (id, val, label) VALUES ({ph}, {ph}, {ph})"
    ops = cfg["ops_insert"]

    # No warmup for INSERT
    lat_ms = []
    t0 = time.perf_counter()
    for i in range(1, ops + 1):
        s = time.perf_counter()
        db.execute(sql, (i, rng.random() * 1000, f"item_{i}"))
        lat_ms.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0

    return bench_stats(lat_ms, elapsed, ops)


def benchmark_update(db, cfg: dict, rng: random.Random) -> dict:
    n = cfg["rows_accounts"]
    ph = db.placeholder
    sql = f"UPDATE bench_accounts SET balance = {ph} WHERE id = {ph}"
    ops = cfg["ops_update"]

    lat_ms = []
    t0 = time.perf_counter()
    for _ in range(ops):
        s = time.perf_counter()
        db.execute(sql, (round(rng.random() * 10000, 2), rng.randint(1, n)))
        lat_ms.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0

    return bench_stats(lat_ms, elapsed, ops)


def benchmark_olap_scan(db, cfg: dict, rng: random.Random) -> dict:
    return run_bench(
        db,
        "SELECT SUM(total) FROM bench_orders WHERE quantity > %s",
        lambda: (rng.randint(1, 10),),
        cfg["ops_olap"], cfg["warmup"],
    )


# ── Report Generation ──────────────────────────────────────────

ALL_TESTS = [
    ("Point Lookup", benchmark_point_lookup),
    ("Range Scan", benchmark_range_scan),
    ("Aggregation (SUM/COUNT)", benchmark_aggregation),
    ("GROUP BY", benchmark_group_by),
    ("JOIN 2-table", benchmark_join_2table),
    ("JOIN 3-table", benchmark_join_3table),
    ("Bulk INSERT", benchmark_insert),
    ("UPDATE", benchmark_update),
    ("OLAP Full Scan", benchmark_olap_scan),
]


def speedup_str(base: float, target: float) -> str:
    """Return speedup string. Positive = target is faster."""
    if base <= 0 or target <= 0:
        return "N/A"
    ratio = target / base
    return f"{ratio:.2f}x"


def latency_winner(vals: dict[str, float]) -> str:
    """Return name of lowest latency."""
    best = min(vals, key=vals.get)
    return best


def render_report(
    results: dict[str, dict[str, dict]],
    cfg: dict,
    profile: str,
    elapsed_total: float,
) -> str:
    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    engines = list(results.keys())

    lines = [
        "# QMvir vs PostgreSQL vs DuckDB — Benchmark Report\n",
        f"**Date**: {ts}  ",
        f"**Profile**: `{profile}`  ",
        f"**Platform**: macOS arm64 (Apple Silicon)  ",
        f"**Total runtime**: {elapsed_total:.1f}s\n",
        "## Dataset\n",
        f"| Table | Rows |",
        f"|-------|-----:|",
        f"| accounts | {cfg['rows_accounts']:,} |",
        f"| products | {cfg['rows_products']:,} |",
        f"| orders | {cfg['rows_orders']:,} |\n",
        "## Results — QPS (queries per second, higher is better)\n",
        "| Benchmark | " + " | ".join(engines) + " | Winner |",
        "|-----------|" + "|".join(["--------:" for _ in engines]) + "|--------|",
    ]

    for test_name, _ in ALL_TESTS:
        row_qps = {}
        for eng in engines:
            r = results[eng].get(test_name)
            row_qps[eng] = r["qps"] if r else 0
        winner = max(row_qps, key=row_qps.get) if any(v > 0 for v in row_qps.values()) else "N/A"
        cells = " | ".join(f"{row_qps[e]:,.0f}" for e in engines)
        lines.append(f"| {test_name} | {cells} | **{winner}** |")

    lines.append("")
    lines.append("## Results — Latency p95 (ms, lower is better)\n")
    lines.append("| Benchmark | " + " | ".join(engines) + " | Winner |")
    lines.append("|-----------|" + "|".join(["--------:" for _ in engines]) + "|--------|")

    for test_name, _ in ALL_TESTS:
        row_p95 = {}
        for eng in engines:
            r = results[eng].get(test_name)
            row_p95[eng] = r["p95_ms"] if r else float("inf")
        winner = min(row_p95, key=row_p95.get) if any(v < float("inf") for v in row_p95.values()) else "N/A"
        cells = " | ".join(f"{row_p95[e]:.3f}" for e in engines)
        lines.append(f"| {test_name} | {cells} | **{winner}** |")

    # Speedup summary
    lines.append("")
    lines.append("## QMvir Speedup vs PostgreSQL (QPS ratio)\n")
    lines.append("| Benchmark | QMvir/PostgreSQL | QMvir/DuckDB |")
    lines.append("|-----------|:----------------:|:------------:|")

    for test_name, _ in ALL_TESTS:
        qm = results.get("QMvir", {}).get(test_name, {})
        pg = results.get("PostgreSQL", {}).get(test_name, {})
        dk = results.get("DuckDB", {}).get(test_name, {})
        qm_qps = qm.get("qps", 0) if qm else 0
        pg_qps = pg.get("qps", 0) if pg else 0
        dk_qps = dk.get("qps", 0) if dk else 0
        lines.append(f"| {test_name} | {speedup_str(pg_qps, qm_qps)} | {speedup_str(dk_qps, qm_qps)} |")

    # Detailed per-test stats
    lines.append("")
    lines.append("## Detailed Statistics\n")

    for test_name, _ in ALL_TESTS:
        lines.append(f"### {test_name}\n")
        lines.append("| Metric | " + " | ".join(engines) + " |")
        lines.append("|--------|" + "|".join(["------:" for _ in engines]) + "|")
        metrics = ["ops", "qps", "avg_ms", "p50_ms", "p95_ms", "p99_ms", "min_ms", "max_ms"]
        for m in metrics:
            cells = []
            for eng in engines:
                r = results[eng].get(test_name, {})
                v = r.get(m, 0)
                if m == "ops":
                    cells.append(f"{v:,.0f}")
                elif m == "qps":
                    cells.append(f"{v:,.0f}")
                else:
                    cells.append(f"{v:.3f}")
            lines.append(f"| {m} | " + " | ".join(cells) + " |")
        lines.append("")

    return "\n".join(lines)


# ── Main ────────────────────────────────────────────────────────

def parse_args():
    p = argparse.ArgumentParser(description="QMvir vs PostgreSQL vs DuckDB benchmark")
    p.add_argument("--profile", choices=["quick", "standard", "heavy"], default="quick")
    p.add_argument("--pg-host", default="/tmp")
    p.add_argument("--pg-port", type=int, default=5432)
    p.add_argument("--pg-user", default=os.environ.get("USER", "postgres"))
    p.add_argument("--pg-db", default="benchdb")
    p.add_argument("--pg-password", default="")
    p.add_argument("--qm-host", default="127.0.0.1")
    p.add_argument("--qm-port", type=int, default=55433)
    p.add_argument("--qm-user", default="admin")
    p.add_argument("--qm-pass", default="admin")
    p.add_argument("--qm-db", default="qm")
    p.add_argument("--output-dir", default=str(Path(__file__).parent))
    return p.parse_args()


def main():
    args = parse_args()
    cfg = PROFILES[args.profile]
    out_dir = Path(args.output_dir)
    total_t0 = time.perf_counter()

    print(f"╔══════════════════════════════════════════════════════════════╗")
    print(f"║   QMvir vs PostgreSQL vs DuckDB — Benchmark Suite          ║")
    print(f"║   Profile: {args.profile:<49}║")
    print(f"╚══════════════════════════════════════════════════════════════╝")
    print()

    # ── Connect ──
    print("[1/5] Connecting to databases...")
    pg = PostgresAdapter(args.pg_host, args.pg_port, args.pg_user, args.pg_db, args.pg_password)
    qm = QMvirAdapter(args.qm_host, args.qm_port, args.qm_user, args.qm_db, args.qm_pass)
    dk = DuckDBAdapter()  # in-memory for fairness (no disk I/O)
    print(f"  PostgreSQL: {args.pg_host}:{args.pg_port}/{args.pg_db}")
    print(f"  QMvir:      {args.qm_host}:{args.qm_port}/{args.qm_db}")
    print(f"  DuckDB:     in-memory (v{duckdb.__version__})")
    print()

    # ── Setup data ──
    print("[2/5] Loading benchmark data...")
    for db, label in [(pg, "PostgreSQL"), (qm, "QMvir"), (dk, "DuckDB")]:
        rng = random.Random(SEED)
        setup_tables(db, cfg, rng, label)
    print()

    # ── Run benchmarks ──
    print("[3/5] Running benchmarks...")
    results: dict[str, dict[str, dict]] = {"QMvir": {}, "PostgreSQL": {}, "DuckDB": {}}

    adapters = [("QMvir", qm), ("PostgreSQL", pg), ("DuckDB", dk)]

    for test_name, test_fn in ALL_TESTS:
        print(f"\n  ▸ {test_name}")
        for eng_name, db in adapters:
            rng = random.Random(SEED + hash(test_name))
            try:
                t0 = time.perf_counter()
                r = test_fn(db, cfg, rng)
                dur = time.perf_counter() - t0
                results[eng_name][test_name] = r
                print(f"    {eng_name:12s}: {r['qps']:>10,.0f} QPS  p95={r['p95_ms']:.3f}ms  ({dur:.1f}s)")
            except Exception as e:
                print(f"    {eng_name:12s}: ERROR — {e}")
                results[eng_name][test_name] = {"ops": 0, "qps": 0, "avg_ms": 0, "p50_ms": 0, "p95_ms": 0, "p99_ms": 0, "min_ms": 0, "max_ms": 0}

    print()
    total_elapsed = time.perf_counter() - total_t0

    # ── Generate report ──
    print("[4/5] Generating report...")
    md = render_report(results, cfg, args.profile, total_elapsed)
    md_path = out_dir / "QMVIR_VS_POSTGRES_DUCKDB.md"
    md_path.write_text(md)

    json_path = out_dir / "QMVIR_VS_POSTGRES_DUCKDB.json"
    json_path.write_text(json.dumps({
        "meta": {
            "date": datetime.now().isoformat(),
            "profile": args.profile,
            "config": cfg,
            "platform": "macOS arm64",
            "total_seconds": total_elapsed,
        },
        "results": results,
    }, indent=2))

    # ── Cleanup ──
    print("[5/5] Cleanup...")
    for db in [pg, qm, dk]:
        try:
            db.close()
        except Exception:
            pass

    print(f"\n{'='*64}")
    print(f"  Benchmark complete in {total_elapsed:.1f}s")
    print(f"  Report: {md_path}")
    print(f"  JSON:   {json_path}")
    print(f"{'='*64}")

    # Quick summary
    print("\n  QUICK SUMMARY (QPS — higher is better):")
    print(f"  {'Test':<25s} {'QMvir':>10s} {'PostgreSQL':>12s} {'DuckDB':>10s}  {'Winner'}")
    print(f"  {'-'*25} {'-'*10} {'-'*12} {'-'*10}  {'-'*10}")
    for test_name, _ in ALL_TESTS:
        qps = {e: results[e].get(test_name, {}).get("qps", 0) for e in results}
        winner = max(qps, key=qps.get) if any(v > 0 for v in qps.values()) else "?"
        marker = " ★" if winner == "QMvir" else ""
        print(f"  {test_name:<25s} {qps['QMvir']:>10,.0f} {qps['PostgreSQL']:>12,.0f} {qps['DuckDB']:>10,.0f}  {winner}{marker}")


if __name__ == "__main__":
    main()
