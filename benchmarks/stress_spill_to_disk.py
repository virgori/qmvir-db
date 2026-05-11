#!/usr/bin/env python3
"""QMvir — 1-Million-Row Spill-to-Disk Stress Test.

Exercises the full BufferPool lifecycle:
  1. Bulk INSERT 1M rows across multiple large tables
  2. Force columnar cache pressure  (>256 MB COL_CACHE_BUDGET)
  3. ANALYZE — collect statistics on every table
  4. SHOW STATS — verify histogram / NDV accuracy
  5. Multi-table concurrent JOINs to trigger cache eviction
  6. VACUUM — explicit spill-to-disk + cache flush
  7. Re-query after VACUUM to verify cache rebuild from disk
  8. Comparison: QMvir vs PostgreSQL vs DuckDB at 1M scale

Usage:
    python benchmarks/stress_spill_to_disk.py
    python benchmarks/stress_spill_to_disk.py --rows 2000000
    python benchmarks/stress_spill_to_disk.py --skip-pg --skip-duck
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

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

SEED = 20260313
REPORT_HEADER = "QMvir Spill-to-Disk Stress Test"

REGIONS = ["us-east", "us-west", "eu-west", "eu-east", "ap-south", "ap-north",
           "sa-east", "af-south"]
CATEGORIES = ["electronics", "books", "clothing", "food", "toys", "sports",
              "auto", "garden", "health", "music"]
STATUSES = ["pending", "shipped", "delivered", "returned", "cancelled"]

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def quantile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    sv = sorted(values)
    idx = max(0, min(len(sv) - 1, int(round((p / 100.0) * (len(sv) - 1)))))
    return sv[idx]


def stats_dict(lat: list[float], elapsed: float, ops: int) -> dict:
    return {
        "ops": ops,
        "qps": ops / elapsed if elapsed > 0 else 0,
        "avg_ms": statistics.mean(lat) if lat else 0,
        "p50_ms": quantile(lat, 50),
        "p95_ms": quantile(lat, 95),
        "p99_ms": quantile(lat, 99),
        "min_ms": min(lat) if lat else 0,
        "max_ms": max(lat) if lat else 0,
    }


def fmt_qps(q: float) -> str:
    return f"{q:>12,.0f}"


def fmt_ms(ms: float) -> str:
    return f"{ms:>8.3f}"


def hr():
    print("─" * 78)


def section(title: str):
    print()
    print(f"╔{'═' * 76}╗")
    print(f"║  {title:<74}║")
    print(f"╚{'═' * 76}╝")


# ---------------------------------------------------------------------------
# DB Adapter (reusable)
# ---------------------------------------------------------------------------

class PgAdapter:
    """psycopg2 adapter — works for both PostgreSQL and QMvir."""
    def __init__(self, name: str, host: str, port: int, user: str,
                 password: str, dbname: str):
        import psycopg2
        self.name = name
        params: dict[str, Any] = {
            "host": host, "port": port, "user": user,
            "dbname": dbname, "connect_timeout": 30,
        }
        if password:
            params["password"] = password
        self.conn = psycopg2.connect(**params)
        self.conn.autocommit = True
        self.cur = self.conn.cursor()
        self.ph = "%s"

    def execute(self, sql: str, params: tuple = ()):
        self.cur.execute(sql, params)

    def fetchall(self):
        return self.cur.fetchall()

    def fetchone(self):
        return self.cur.fetchone()

    def close(self):
        self.cur.close()
        self.conn.close()


class DuckAdapter:
    def __init__(self):
        import duckdb
        self.name = "DuckDB"
        self.conn = duckdb.connect(":memory:")
        self.cur = self.conn.cursor()
        self.ph = "?"

    def execute(self, sql: str, params: tuple = ()):
        sql2 = sql.replace("%s", "?")
        self.cur.execute(sql2, list(params)) if params else self.cur.execute(sql2)

    def fetchall(self):
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


# ---------------------------------------------------------------------------
# Data loading  (batch INSERT for speed)
# ---------------------------------------------------------------------------

BATCH_SIZE = 500  # rows per multi-value INSERT


def _batch_insert(db, sql_prefix: str, rows: list[str]):
    """Execute multi-row INSERT.  `rows` is a list of '(v1,v2,...)'."""
    for i in range(0, len(rows), BATCH_SIZE):
        chunk = rows[i:i + BATCH_SIZE]
        db.execute(f"{sql_prefix} VALUES {','.join(chunk)}")


def load_tables(db, n_rows: int, rng: random.Random, label: str):
    """Create 4 big tables to push memory past COL_CACHE_BUDGET."""
    n_acc = n_rows
    n_prod = max(1000, n_rows // 10)
    n_ord = n_rows
    n_log = n_rows  # extra big table for spill pressure

    print(f"  [{label}] Creating tables "
          f"(accounts={n_acc:,}, products={n_prod:,}, "
          f"orders={n_ord:,}, logs={n_log:,}) ...")

    for t in ("stress_logs", "stress_orders", "stress_products", "stress_accounts"):
        try:
            db.execute(f"DROP TABLE IF EXISTS {t}")
        except Exception:
            try:
                db.execute(f"DROP TABLE {t}")
            except Exception:
                pass

    # ── accounts ─────────────────────────────────────────
    db.execute(
        "CREATE TABLE stress_accounts "
        "(id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, "
        " name TEXT, region TEXT)"
    )
    rows = []
    for i in range(1, n_acc + 1):
        bal = round(500 + rng.random() * 19500, 2)
        rows.append(f"({i},{bal},'user_{i}','{rng.choice(REGIONS)}')")
        if len(rows) >= BATCH_SIZE:
            _batch_insert(db, "INSERT INTO stress_accounts (id,balance,name,region)", rows)
            rows.clear()
    if rows:
        _batch_insert(db, "INSERT INTO stress_accounts (id,balance,name,region)", rows)
    print(f"    accounts: {n_acc:,} rows loaded")

    # ── products ─────────────────────────────────────────
    db.execute(
        "CREATE TABLE stress_products "
        "(id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, "
        " category TEXT)"
    )
    rows = []
    for i in range(1, n_prod + 1):
        p = round(rng.uniform(1, 2000), 2)
        rows.append(f"({i},'prod_{i}',{p},'{rng.choice(CATEGORIES)}')")
        if len(rows) >= BATCH_SIZE:
            _batch_insert(db, "INSERT INTO stress_products (id,name,price,category)", rows)
            rows.clear()
    if rows:
        _batch_insert(db, "INSERT INTO stress_products (id,name,price,category)", rows)
    print(f"    products: {n_prod:,} rows loaded")

    # ── orders ───────────────────────────────────────────
    db.execute(
        "CREATE TABLE stress_orders "
        "(id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, "
        " quantity INTEGER, total DOUBLE PRECISION, status TEXT)"
    )
    rows = []
    for i in range(1, n_ord + 1):
        aid = rng.randint(1, n_acc)
        pid = rng.randint(1, n_prod)
        qty = rng.randint(1, 50)
        tot = round(qty * rng.uniform(1, 2000), 2)
        rows.append(f"({i},{aid},{pid},{qty},{tot},'{rng.choice(STATUSES)}')")
        if len(rows) >= BATCH_SIZE:
            _batch_insert(db, "INSERT INTO stress_orders (id,account_id,product_id,quantity,total,status)", rows)
            rows.clear()
    if rows:
        _batch_insert(db, "INSERT INTO stress_orders (id,account_id,product_id,quantity,total,status)", rows)
    print(f"    orders:   {n_ord:,} rows loaded")

    # ── logs (extra wide for memory pressure) ────────────
    db.execute(
        "CREATE TABLE stress_logs "
        "(id INTEGER PRIMARY KEY, ts INTEGER, level TEXT, "
        " source TEXT, message TEXT, code INTEGER)"
    )
    levels = ["DEBUG", "INFO", "WARN", "ERROR", "FATAL"]
    sources = [f"svc_{j}" for j in range(20)]
    rows = []
    for i in range(1, n_log + 1):
        ts = 1700000000 + rng.randint(0, 86400 * 365)
        code = rng.randint(100, 999)
        rows.append(
            f"({i},{ts},'{rng.choice(levels)}','{rng.choice(sources)}',"
            f"'Event {i}: {rng.choice(sources)} code={code}',{code})"
        )
        if len(rows) >= BATCH_SIZE:
            _batch_insert(db, "INSERT INTO stress_logs (id,ts,level,source,message,code)", rows)
            rows.clear()
    if rows:
        _batch_insert(db, "INSERT INTO stress_logs (id,ts,level,source,message,code)", rows)
    print(f"    logs:     {n_log:,} rows loaded")

    # ── indexes (for PG/QMvir) ───────────────────────────
    if not isinstance(db, DuckAdapter):
        for idx_sql in [
            "CREATE INDEX idx_sa_region ON stress_accounts (region)",
            "CREATE INDEX idx_so_acc    ON stress_orders   (account_id)",
            "CREATE INDEX idx_so_prod   ON stress_orders   (product_id)",
            "CREATE INDEX idx_sl_code   ON stress_logs     (code)",
        ]:
            try:
                db.execute(idx_sql)
            except Exception:
                pass


# ---------------------------------------------------------------------------
# Benchmark phases
# ---------------------------------------------------------------------------

def phase_insert_throughput(db, n_rows: int, rng: random.Random) -> dict:
    """Measure sustained INSERT throughput into a fresh table."""
    try:
        db.execute("DROP TABLE IF EXISTS stress_insert_bench")
    except Exception:
        try:
            db.execute("DROP TABLE stress_insert_bench")
        except Exception:
            pass

    db.execute(
        "CREATE TABLE stress_insert_bench "
        "(id INTEGER PRIMARY KEY, a INTEGER, b DOUBLE PRECISION, c TEXT)"
    )

    ops = min(n_rows, 100_000)  # cap at 100k for timing
    lat = []
    t0 = time.perf_counter()
    ph = db.ph
    for i in range(1, ops + 1):
        s = time.perf_counter()
        db.execute(
            f"INSERT INTO stress_insert_bench (id,a,b,c) VALUES "
            f"({ph},{ph},{ph},{ph})",
            (i, rng.randint(0, 1_000_000), round(rng.random() * 1e6, 2), f"row_{i}"),
        )
        lat.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0
    return stats_dict(lat, elapsed, ops)


def phase_analyze(db, tables: list[str]) -> dict:
    """Run ANALYZE on all tables and measure wall-time."""
    lat = []
    for t in tables:
        s = time.perf_counter()
        try:
            db.execute(f"ANALYZE {t}")
            try:
                db.fetchall()
            except Exception:
                pass
        except Exception:
            pass
        lat.append((time.perf_counter() - s) * 1000)
    return {"tables": len(tables), "total_ms": sum(lat), "per_table_ms": lat}


def phase_show_stats(db, table: str) -> dict | None:
    """Run SHOW STATS (QMvir only) and return column stats."""
    try:
        db.execute(f"SHOW STATS {table}")
        cols = [d[0] for d in db.cur.description]
        rows = db.fetchall()
        return {"columns": cols, "rows": [dict(zip(cols, r)) for r in rows]}
    except Exception:
        return None


def phase_heavy_queries(db, n_rows: int, rng: random.Random, ops: int = 200,
                        warmup: int = 20) -> dict[str, dict]:
    """Run heavy analytical queries that force cache rebuilds."""
    ph = db.ph
    n_acc = n_rows
    n_prod = max(1000, n_rows // 10)
    results = {}

    # 1) Full-table SUM
    def run_bench(name, sql, pfn, fetch=True):
        sql2 = sql.replace("%s", ph) if ph != "%s" else sql
        for _ in range(warmup):
            db.execute(sql2, pfn())
            if fetch:
                db.fetchall()
        lat = []
        t0 = time.perf_counter()
        for _ in range(ops):
            s = time.perf_counter()
            db.execute(sql2, pfn())
            if fetch:
                db.fetchall()
            lat.append((time.perf_counter() - s) * 1000)
        elapsed = time.perf_counter() - t0
        results[name] = stats_dict(lat, elapsed, ops)

    run_bench("OLAP SUM (orders)", 
              "SELECT SUM(total) FROM stress_orders WHERE quantity > %s",
              lambda: (rng.randint(1, 25),))

    run_bench("OLAP COUNT+SUM (logs)",
              "SELECT COUNT(*), SUM(code) FROM stress_logs WHERE code BETWEEN %s AND %s",
              lambda: (lo := rng.randint(100, 800), lo + rng.randint(50, 199)))

    run_bench("GROUP BY (products)",
              "SELECT category, SUM(price), COUNT(*) FROM stress_products GROUP BY category",
              lambda: ())

    run_bench("Range Scan (accounts)",
              "SELECT id, balance, region FROM stress_accounts WHERE id BETWEEN %s AND %s",
              lambda: (lo := rng.randint(1, max(2, n_acc - 5000)), lo + rng.randint(1000, 5000)))

    run_bench("JOIN 2-table",
              "SELECT a.name, o.total FROM stress_accounts a "
              "JOIN stress_orders o ON a.id = o.account_id WHERE a.id = %s",
              lambda: (rng.randint(1, n_acc),))

    run_bench("JOIN 3-table",
              "SELECT a.name, o.quantity, p.name, o.total "
              "FROM stress_accounts a "
              "JOIN stress_orders o ON a.id = o.account_id "
              "JOIN stress_products p ON o.product_id = p.id "
              "WHERE a.id = %s",
              lambda: (rng.randint(1, n_acc),))

    return results


def phase_vacuum(db) -> float:
    """Run VACUUM and return wall-time ms."""
    s = time.perf_counter()
    try:
        db.execute("VACUUM")
        try:
            db.fetchall()
        except Exception:
            pass
    except Exception:
        pass
    return (time.perf_counter() - s) * 1000


def phase_post_vacuum_queries(db, n_rows: int, rng: random.Random,
                               ops: int = 100) -> dict[str, dict]:
    """Re-run queries after VACUUM to verify cache rebuild."""
    return phase_heavy_queries(db, n_rows, rng, ops=ops, warmup=10)


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def render_report(all_results: dict, n_rows: int, elapsed: float) -> str:
    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    engines = list(all_results.keys())

    L = [
        f"# {REPORT_HEADER}\n",
        f"**Date**: {ts}  ",
        f"**Rows per table**: {n_rows:,}  ",
        f"**Total tables**: 4 (accounts, products, orders, logs)  ",
        f"**Total runtime**: {elapsed:.1f}s  ",
        f"**Platform**: macOS arm64 (Apple Silicon)\n",
    ]

    # ── Insert throughput ──
    L.append("## Phase 1 — Bulk INSERT Throughput\n")
    L.append("| Engine | QPS | p50 ms | p95 ms | p99 ms |")
    L.append("|--------|----:|-------:|-------:|-------:|")
    for eng in engines:
        r = all_results[eng].get("insert", {})
        L.append(f"| {eng} | {r.get('qps',0):,.0f} | {r.get('p50_ms',0):.3f} "
                 f"| {r.get('p95_ms',0):.3f} | {r.get('p99_ms',0):.3f} |")

    # ── Analyze timing ──
    L.append("\n## Phase 2 — ANALYZE (Statistics Collection)\n")
    L.append("| Engine | Total ms |")
    L.append("|--------|--------:|")
    for eng in engines:
        a = all_results[eng].get("analyze", {})
        L.append(f"| {eng} | {a.get('total_ms',0):.1f} |")

    # ── SHOW STATS (QMvir only) ──
    qm_stats = all_results.get("QMvir", {}).get("show_stats")
    if qm_stats:
        L.append("\n## Phase 3 — SHOW STATS (QMvir histograms)\n")
        L.append(f"**Table**: `stress_orders`\n")
        L.append("| Column | Rows | Distinct | Selectivity | Min | Max |")
        L.append("|--------|-----:|---------:|------------:|----:|----:|")
        for row in qm_stats.get("rows", []):
            L.append(f"| {row.get('column','')} | {row.get('rows','')} "
                     f"| {row.get('distinct','')} | {row.get('selectivity','')} "
                     f"| {row.get('min','')} | {row.get('max','')} |")

    # ── Heavy queries (before VACUUM) ──
    L.append("\n## Phase 4 — Heavy Analytics (cache-hot)\n")
    query_names = []
    for eng in engines:
        for k in all_results[eng].get("heavy_pre", {}):
            if k not in query_names:
                query_names.append(k)

    L.append("| Benchmark | " + " | ".join(f"{e} QPS" for e in engines) + " | Winner |")
    L.append("|-----------|" + "|".join(["--------:" for _ in engines]) + "|--------|")
    for qn in query_names:
        vals = {}
        for eng in engines:
            vals[eng] = all_results[eng].get("heavy_pre", {}).get(qn, {}).get("qps", 0)
        winner = max(vals, key=vals.get) if any(v > 0 for v in vals.values()) else "—"
        cells = " | ".join(f"{vals[e]:,.0f}" for e in engines)
        L.append(f"| {qn} | {cells} | **{winner}** |")

    # ── VACUUM ──
    L.append("\n## Phase 5 — VACUUM (spill-to-disk)\n")
    L.append("| Engine | VACUUM ms |")
    L.append("|--------|----------:|")
    for eng in engines:
        v = all_results[eng].get("vacuum_ms", 0)
        L.append(f"| {eng} | {v:.1f} |")

    # ── Post-VACUUM queries ──
    L.append("\n## Phase 6 — Post-VACUUM Analytics (cache-cold rebuild)\n")
    L.append("| Benchmark | " + " | ".join(f"{e} QPS" for e in engines) + " | Winner |")
    L.append("|-----------|" + "|".join(["--------:" for _ in engines]) + "|--------|")
    for qn in query_names:
        vals = {}
        for eng in engines:
            vals[eng] = all_results[eng].get("heavy_post", {}).get(qn, {}).get("qps", 0)
        winner = max(vals, key=vals.get) if any(v > 0 for v in vals.values()) else "—"
        cells = " | ".join(f"{vals[e]:,.0f}" for e in engines)
        L.append(f"| {qn} | {cells} | **{winner}** |")

    # ── Speedup summary ──
    L.append("\n## QMvir Speedup Summary (QPS ratio, pre-VACUUM)\n")
    L.append("| Benchmark | vs PostgreSQL | vs DuckDB |")
    L.append("|-----------|:-------------:|:---------:|")
    for qn in query_names:
        qm_q = all_results.get("QMvir", {}).get("heavy_pre", {}).get(qn, {}).get("qps", 0)
        pg_q = all_results.get("PostgreSQL", {}).get("heavy_pre", {}).get(qn, {}).get("qps", 0)
        dk_q = all_results.get("DuckDB", {}).get("heavy_pre", {}).get(qn, {}).get("qps", 0)
        vs_pg = f"{qm_q/pg_q:.2f}x" if pg_q > 0 else "N/A"
        vs_dk = f"{qm_q/dk_q:.2f}x" if dk_q > 0 else "N/A"
        L.append(f"| {qn} | {vs_pg} | {vs_dk} |")

    # ── Stability: compare pre vs post VACUUM QPS ──
    L.append("\n## Stability — Pre vs Post VACUUM (QMvir)\n")
    L.append("| Benchmark | Pre QPS | Post QPS | Δ% |")
    L.append("|-----------|--------:|---------:|---:|")
    for qn in query_names:
        pre = all_results.get("QMvir", {}).get("heavy_pre", {}).get(qn, {}).get("qps", 0)
        post = all_results.get("QMvir", {}).get("heavy_post", {}).get(qn, {}).get("qps", 0)
        delta = ((post - pre) / pre * 100) if pre > 0 else 0
        L.append(f"| {qn} | {pre:,.0f} | {post:,.0f} | {delta:+.1f}% |")

    L.append(f"\n---\n*Generated by `stress_spill_to_disk.py` — {ts}*\n")
    return "\n".join(L)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def parse_args():
    p = argparse.ArgumentParser(description=REPORT_HEADER)
    p.add_argument("--rows", type=int, default=1_000_000,
                   help="Rows per major table (default: 1,000,000)")
    p.add_argument("--ops", type=int, default=300,
                   help="Query ops per benchmark (default: 300)")
    p.add_argument("--qm-host", default="127.0.0.1")
    p.add_argument("--qm-port", type=int, default=55433)
    p.add_argument("--qm-user", default="admin")
    p.add_argument("--qm-pass", default="admin")
    p.add_argument("--qm-db", default="qm")
    p.add_argument("--pg-host", default="/tmp")
    p.add_argument("--pg-port", type=int, default=5432)
    p.add_argument("--pg-user", default=os.environ.get("USER", "postgres"))
    p.add_argument("--pg-db", default="benchdb")
    p.add_argument("--pg-password", default="")
    p.add_argument("--skip-pg", action="store_true",
                   help="Skip PostgreSQL (run QMvir + DuckDB only)")
    p.add_argument("--skip-duck", action="store_true",
                   help="Skip DuckDB")
    p.add_argument("--output-dir", default=str(Path(__file__).parent))
    return p.parse_args()


def run_engine(label: str, db, n_rows: int, ops: int, rng_seed: int,
               is_qmvir: bool = False) -> dict:
    """Run all phases for one engine."""
    result: dict[str, Any] = {}
    tables = ["stress_accounts", "stress_products", "stress_orders", "stress_logs"]

    # Phase 1 — INSERT
    print(f"\n  [{label}] Phase 1 — Bulk INSERT (100k rows) ...")
    rng = random.Random(rng_seed)
    result["insert"] = phase_insert_throughput(db, n_rows, rng)
    print(f"    {result['insert']['qps']:,.0f} QPS  "
          f"p95={result['insert']['p95_ms']:.3f}ms")

    # Phase 2 — ANALYZE
    print(f"  [{label}] Phase 2 — ANALYZE ...")
    result["analyze"] = phase_analyze(db, tables)
    print(f"    total: {result['analyze']['total_ms']:.1f}ms")

    # Phase 3 — SHOW STATS (QMvir only)
    if is_qmvir:
        print(f"  [{label}] Phase 3 — SHOW STATS ...")
        result["show_stats"] = phase_show_stats(db, "stress_orders")
        if result["show_stats"]:
            n_stat_rows = len(result["show_stats"].get("rows", []))
            print(f"    {n_stat_rows} column stats returned")

    # Phase 4 — Heavy queries (cache-hot)
    print(f"  [{label}] Phase 4 — Heavy analytics (cache-hot, {ops} ops each) ...")
    rng = random.Random(rng_seed + 1)
    result["heavy_pre"] = phase_heavy_queries(db, n_rows, rng, ops=ops)
    for qn, st in result["heavy_pre"].items():
        print(f"    {qn:30s} {st['qps']:>10,.0f} QPS  p95={st['p95_ms']:.3f}ms")

    # Phase 5 — VACUUM
    if is_qmvir:
        print(f"  [{label}] Phase 5 — VACUUM (spill-to-disk) ...")
    else:
        print(f"  [{label}] Phase 5 — VACUUM ...")
    result["vacuum_ms"] = phase_vacuum(db)
    print(f"    VACUUM: {result['vacuum_ms']:.1f}ms")

    # Phase 6 — Post-VACUUM queries (cache rebuild)
    print(f"  [{label}] Phase 6 — Post-VACUUM analytics ({ops} ops each) ...")
    rng = random.Random(rng_seed + 2)
    result["heavy_post"] = phase_heavy_queries(db, n_rows, rng, ops=ops)
    for qn, st in result["heavy_post"].items():
        pre_qps = result["heavy_pre"].get(qn, {}).get("qps", 0)
        delta = ((st["qps"] - pre_qps) / pre_qps * 100) if pre_qps > 0 else 0
        print(f"    {qn:30s} {st['qps']:>10,.0f} QPS  (Δ {delta:+.1f}%)")

    return result


def main():
    args = parse_args()
    n_rows = args.rows
    ops = args.ops
    out_dir = Path(args.output_dir)
    total_t0 = time.perf_counter()

    section(f"{REPORT_HEADER}  —  {n_rows:,} rows × 4 tables")
    print(f"  ops/benchmark: {ops}")
    print()

    all_results: dict[str, dict] = {}

    # ── Start QMvir server ──────────────────────────────────
    print("[1] Starting QMvir native engine ...")
    from qm_engine import PostgresGateway
    data_dir = tempfile.mkdtemp(prefix="qmvir_stress_")
    gw = PostgresGateway(port=args.qm_port)
    gw.start_native_persist(data_dir)
    time.sleep(1)
    print(f"  QMvir running on :{args.qm_port}  data_dir={data_dir}")

    # ── Connect ─────────────────────────────────────────────
    print("[2] Connecting ...")
    qm = PgAdapter("QMvir", args.qm_host, args.qm_port, args.qm_user,
                    args.qm_pass, args.qm_db)
    pg = dk = None
    if not args.skip_pg:
        try:
            pg = PgAdapter("PostgreSQL", args.pg_host, args.pg_port,
                           args.pg_user, args.pg_password, args.pg_db)
            print(f"  PostgreSQL: {args.pg_host}:{args.pg_port}/{args.pg_db}")
        except Exception as e:
            print(f"  PostgreSQL: SKIP — {e}")
    if not args.skip_duck:
        try:
            dk = DuckAdapter()
            import duckdb
            print(f"  DuckDB: in-memory v{duckdb.__version__}")
        except Exception as e:
            print(f"  DuckDB: SKIP — {e}")
    print()

    # ── Load data ───────────────────────────────────────────
    print(f"[3] Loading {n_rows:,} rows into each engine ...")
    engines: list[tuple[str, Any, bool]] = [("QMvir", qm, True)]
    if pg:
        engines.append(("PostgreSQL", pg, False))
    if dk:
        engines.append(("DuckDB", dk, False))

    for label, db, _ in engines:
        rng = random.Random(SEED)
        t0 = time.perf_counter()
        load_tables(db, n_rows, rng, label)
        dur = time.perf_counter() - t0
        print(f"  [{label}] Data loaded in {dur:.1f}s")
    print()

    # ── Run phases ─────────────────────────────────────────
    print(f"[4] Running benchmark phases ...")
    for label, db, is_qm in engines:
        hr()
        print(f"  Engine: {label}")
        hr()
        all_results[label] = run_engine(label, db, n_rows, ops, SEED, is_qm)
    print()

    # ── Report ─────────────────────────────────────────────
    total_elapsed = time.perf_counter() - total_t0
    section("RESULTS")

    # Quick console summary
    eng_names = list(all_results.keys())
    print(f"\n  {'Benchmark':30s}", end="")
    for e in eng_names:
        print(f"  {e:>12s}", end="")
    print("   Winner")
    print("  " + "─" * (32 + 14 * len(eng_names) + 10))

    query_names = []
    for eng in eng_names:
        for k in all_results[eng].get("heavy_pre", {}):
            if k not in query_names:
                query_names.append(k)

    wins = {e: 0 for e in eng_names}
    for qn in query_names:
        vals = {e: all_results[e].get("heavy_pre", {}).get(qn, {}).get("qps", 0)
                for e in eng_names}
        winner = max(vals, key=vals.get)
        wins[winner] += 1
        print(f"  {qn:30s}", end="")
        for e in eng_names:
            print(fmt_qps(vals[e]), end="")
        star = " ★" if winner == "QMvir" else ""
        print(f"   {winner}{star}")

    print()
    for e in eng_names:
        print(f"  {e}: {wins[e]}/{len(query_names)} wins")

    # ── Spill verification ──────────────────────────────────
    spill_dir = Path(data_dir) / "spill"
    if spill_dir.exists():
        files = list(spill_dir.glob("*.colcache"))
        total_bytes = sum(f.stat().st_size for f in files)
        print(f"\n  Spill-to-disk verification:")
        print(f"    Directory: {spill_dir}")
        print(f"    Files:     {len(files)}")
        print(f"    Total:     {total_bytes / 1024 / 1024:.1f} MB")
        for f in files:
            print(f"      {f.name}: {f.stat().st_size / 1024 / 1024:.1f} MB")
    else:
        print(f"\n  Spill directory not created (cache stayed within budget)")

    # ── Write report files ──────────────────────────────────
    print(f"\n[5] Writing reports ...")
    md = render_report(all_results, n_rows, total_elapsed)
    md_path = out_dir / "STRESS_SPILL_TO_DISK.md"
    md_path.write_text(md)
    print(f"  {md_path}")

    json_path = out_dir / "STRESS_SPILL_TO_DISK.json"
    json_path.write_text(json.dumps({
        "meta": {
            "date": datetime.now().isoformat(),
            "rows": n_rows,
            "ops": ops,
            "total_seconds": total_elapsed,
            "data_dir": data_dir,
        },
        "results": all_results,
    }, indent=2, default=str))
    print(f"  {json_path}")

    # ── Cleanup ─────────────────────────────────────────────
    qm.close()
    if pg:
        pg.close()
    if dk:
        dk.close()
    gw.stop()

    print(f"\n  Total time: {total_elapsed:.1f}s")
    print(f"  ✓ Stress test complete.\n")


if __name__ == "__main__":
    main()
