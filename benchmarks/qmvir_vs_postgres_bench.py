#!/usr/bin/env python3
"""QMvir vs PostgreSQL benchmark focused on JOIN and SUM.

Includes:
- deterministic data seed
- warmup phase
- optional UDS connection
- mixed stress run
- markdown + json outputs
"""

from __future__ import annotations

import argparse
import json
import os
import random
import statistics
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

try:
    import psycopg2
except ImportError as exc:  # pragma: no cover
    raise SystemExit("Install psycopg2-binary first: pip install psycopg2-binary") from exc


RANDOM_SEED = 20260309

PROFILE_CONFIG: dict[str, dict[str, int]] = {
    "quick": {
        "ops": 300,
        "warmup": 40,
        "accounts": 10000,
        "products": 1000,
        "orders": 5000,
        "stress_seconds": 8,
        "stress_clients": 6,
    },
    "standard": {
        "ops": 800,
        "warmup": 80,
        "accounts": 20000,
        "products": 2000,
        "orders": 15000,
        "stress_seconds": 20,
        "stress_clients": 8,
    },
    "heavy": {
        "ops": 1500,
        "warmup": 150,
        "accounts": 50000,
        "products": 5000,
        "orders": 60000,
        "stress_seconds": 40,
        "stress_clients": 12,
    },
}


@dataclass
class ConnSpec:
    host: str
    port: int
    user: str
    dbname: str
    password: str = ""
    unix_socket_dir: str | None = None


def _connect(spec: ConnSpec):
    params: dict[str, Any] = {
        "user": spec.user,
        "dbname": spec.dbname,
        "connect_timeout": 5,
    }
    if spec.password:
        params["password"] = spec.password
    if spec.unix_socket_dir:
        params["host"] = spec.unix_socket_dir
        params["port"] = spec.port
    else:
        params["host"] = spec.host
        params["port"] = spec.port
    return psycopg2.connect(**params)


def _safe_quantile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    idx = max(0, min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1)))))
    sv = sorted(values)
    return sv[idx]


def _bench_query(spec: ConnSpec, sql: str, params_fn, ops: int, warmup: int) -> dict[str, float]:
    conn = _connect(spec)
    conn.autocommit = True
    cur = conn.cursor()

    for _ in range(warmup):
        cur.execute(sql, params_fn())
        cur.fetchall()

    lat_ms: list[float] = []
    t0 = time.perf_counter()
    for _ in range(ops):
        s = time.perf_counter()
        cur.execute(sql, params_fn())
        cur.fetchall()
        lat_ms.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0

    cur.close()
    conn.close()

    return {
        "ops": float(ops),
        "qps": float(ops / elapsed) if elapsed > 0 else 0.0,
        "avg_ms": float(statistics.mean(lat_ms)) if lat_ms else 0.0,
        "p50_ms": _safe_quantile(lat_ms, 50),
        "p95_ms": _safe_quantile(lat_ms, 95),
        "p99_ms": _safe_quantile(lat_ms, 99),
    }


def setup_data(spec: ConnSpec, target: str, accounts: int, products: int, orders: int) -> tuple[str, str, str]:
    random.seed(RANDOM_SEED)
    conn = _connect(spec)
    conn.autocommit = True
    cur = conn.cursor()

    suffix = "" if target == "postgres" else f"_{int(time.time()) % 10000}"
    acc = f"bench_accounts{suffix}"
    prod = f"bench_products{suffix}"
    ords = f"bench_orders{suffix}"

    if target == "postgres":
        cur.execute("DROP TABLE IF EXISTS bench_orders CASCADE")
        cur.execute("DROP TABLE IF EXISTS bench_products CASCADE")
        cur.execute("DROP TABLE IF EXISTS bench_accounts CASCADE")

    for t, ddl in [
        (acc, f"CREATE TABLE {acc} (id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, name TEXT)"),
        (prod, f"CREATE TABLE {prod} (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, category TEXT)"),
        (ords, f"CREATE TABLE {ords} (id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, quantity INTEGER, total DOUBLE PRECISION)"),
    ]:
        try:
            cur.execute(ddl)
        except Exception:
            cur.execute(f"DELETE FROM {t} WHERE 1=1")

    categories = ["electronics", "books", "clothing", "food", "toys"]

    for i in range(1, accounts + 1):
        cur.execute(
            f"INSERT INTO {acc} (id, balance, name) VALUES (%s, %s, %s)",
            (i, 1000.0 + random.random() * 1000, f"user_{i}"),
        )

    for i in range(1, products + 1):
        cur.execute(
            f"INSERT INTO {prod} (id, name, price, category) VALUES (%s, %s, %s, %s)",
            (i, f"product_{i}", random.uniform(10, 500), random.choice(categories)),
        )

    for i in range(1, orders + 1):
        aid = random.randint(1, accounts)
        pid = random.randint(1, products)
        qty = random.randint(1, 10)
        total = qty * random.uniform(10, 500)
        cur.execute(
            f"INSERT INTO {ords} (id, account_id, product_id, quantity, total) VALUES (%s, %s, %s, %s, %s)",
            (i, aid, pid, qty, total),
        )

    if target == "postgres":
        cur.execute(f"CREATE INDEX IF NOT EXISTS idx_orders_account ON {ords}(account_id)")
        cur.execute(f"CREATE INDEX IF NOT EXISTS idx_orders_product ON {ords}(product_id)")
    else:
        # Create B+Tree indexes on QM for parity with PostgreSQL
        try:
            cur.execute(f"CREATE INDEX idx_{ords}_account ON {ords} (account_id)")
        except Exception:
            pass
        try:
            cur.execute(f"CREATE INDEX idx_{ords}_product ON {ords} (product_id)")
        except Exception:
            pass

    cur.close()
    conn.close()
    return acc, prod, ords


def run_join_sum(spec: ConnSpec, target: str, ops: int, warmup: int, table_sizes: tuple[int, int, int]) -> dict[str, Any]:
    acc_n, prod_n, ord_n = table_sizes
    acc, prod, ords = setup_data(spec, target, acc_n, prod_n, ord_n)

    rnd = random.Random(RANDOM_SEED + (1 if target == "postgres" else 2))

    join_sql = (
        f"SELECT a.name, o.id, p.name, o.quantity, o.total "
        f"FROM {acc} a "
        f"JOIN {ords} o ON a.id = o.account_id "
        f"JOIN {prod} p ON o.product_id = p.id "
        f"WHERE a.id = %s"
    )

    sum_sql = f"SELECT SUM(total) FROM {ords} WHERE account_id BETWEEN %s AND %s"

    join = _bench_query(
        spec,
        join_sql,
        lambda: (rnd.randint(1, acc_n),),
        ops=ops,
        warmup=warmup,
    )

    sum_result = _bench_query(
        spec,
        sum_sql,
        lambda: (
            rnd.randint(1, max(2, acc_n - 500)),
            rnd.randint(500, acc_n),
        ),
        ops=ops,
        warmup=warmup,
    )

    return {
        "target": target,
        "timestamp": datetime.now().isoformat(),
        "dataset": {
            "accounts": acc_n,
            "products": prod_n,
            "orders": ord_n,
            "accounts_table": acc,
            "products_table": prod,
            "orders_table": ords,
        },
        "join": join,
        "sum": sum_result,
    }


def run_stress(spec: ConnSpec, target: str, duration_s: int, clients: int, acc_n: int,
               acc_tbl: str = "bench_accounts", prod_tbl: str = "bench_products",
               ords_tbl: str = "bench_orders") -> dict[str, Any]:
    stop_at = time.time() + duration_s
    join_sql = (
        f"SELECT a.name, o.id, p.name, o.quantity, o.total "
        f"FROM {acc_tbl} a "
        f"JOIN {ords_tbl} o ON a.id = o.account_id "
        f"JOIN {prod_tbl} p ON o.product_id = p.id "
        f"WHERE a.id = %s"
    )
    sum_sql = f"SELECT SUM(total) FROM {ords_tbl} WHERE account_id BETWEEN %s AND %s"

    def worker(seed: int) -> tuple[int, int]:
        rnd = random.Random(seed)
        conn = _connect(spec)
        conn.autocommit = True
        cur = conn.cursor()
        ok, err = 0, 0
        while time.time() < stop_at:
            try:
                if rnd.random() < 0.6:
                    cur.execute(join_sql, (rnd.randint(1, acc_n),))
                else:
                    lo = rnd.randint(1, max(2, acc_n - 500))
                    hi = min(acc_n, lo + rnd.randint(100, 1000))
                    cur.execute(sum_sql, (lo, hi))
                cur.fetchall()
                ok += 1
            except Exception:
                err += 1
        cur.close()
        conn.close()
        return ok, err

    with ThreadPoolExecutor(max_workers=clients) as ex:
        out = list(ex.map(worker, range(clients)))

    ok = sum(x[0] for x in out)
    err = sum(x[1] for x in out)
    return {
        "duration_s": duration_s,
        "clients": clients,
        "ok": ok,
        "errors": err,
        "throughput_qps": ok / max(1.0, duration_s),
        "error_rate": (err / max(1, ok + err)),
    }


def _speedup(pg: float, qm: float) -> str:
    if pg <= 0 or qm <= 0:
        return "-"
    return f"{(qm / pg):.2f}x"


def _rows_from_cursor(cur) -> list[dict[str, Any]]:
    cols = [d[0] for d in (cur.description or [])]
    out: list[dict[str, Any]] = []
    for row in cur.fetchall():
        out.append({cols[i]: row[i] for i in range(len(cols))})
    return out


def _shadow_compare_pg_vs_rust(
    pg: ConnSpec,
    qm: ConnSpec,
    pg_acc: str,
    pg_ords: str,
    pg_prod: str,
    qm_acc: str,
    qm_ords: str,
    qm_prod: str,
    sample_accounts: int = 128,
) -> dict[str, Any]:
    """Compare PostgreSQL and QM (Rust NativeSqlEngine) results via wire protocol."""
    rnd = random.Random(RANDOM_SEED + 99)
    ids = [rnd.randint(1, max(1, sample_accounts * 32)) for _ in range(sample_accounts)]
    ids = sorted(set(ids))
    id_set = set(ids)
    id_min = min(ids) if ids else 0
    id_max = max(ids) if ids else 0

    # Query PostgreSQL
    pg_conn = _connect(pg)
    pg_conn.autocommit = True
    pg_cur = pg_conn.cursor()
    pg_cur.execute(
        f"SELECT a.id, o.id AS order_id, o.quantity, o.total "
        f"FROM {pg_acc} a "
        f"JOIN {pg_ords} o ON a.id = o.account_id "
        f"JOIN {pg_prod} p ON o.product_id = p.id "
        f"WHERE a.id BETWEEN %s AND %s",
        (id_min, id_max),
    )
    pg_rows = [r for r in _rows_from_cursor(pg_cur) if int(r.get("id", 0) or 0) in id_set]
    pg_cur.close()
    pg_conn.close()

    # Query QM (Rust NativeSqlEngine) via wire protocol — same path as benchmark
    qm_conn = _connect(qm)
    qm_conn.autocommit = True
    qm_cur = qm_conn.cursor()
    qm_rows = []
    for aid in ids:
        try:
            qm_cur.execute(
                f"SELECT a.name, o.id, p.name, o.quantity, o.total "
                f"FROM {qm_acc} a "
                f"JOIN {qm_ords} o ON a.id = o.account_id "
                f"JOIN {qm_prod} p ON o.product_id = p.id "
                f"WHERE a.id = %s",
                (aid,),
            )
            for row in qm_cur.fetchall():
                qm_rows.append({"id": aid, "total": row[4] if len(row) > 4 else 0})
        except Exception:
            pass
    qm_cur.close()
    qm_conn.close()

    # Aggregate by account_id
    pg_agg: dict[int, dict[str, float]] = defaultdict(lambda: {"cnt": 0, "sum": 0.0})
    for r in pg_rows:
        aid = int(r.get("id", 0) or 0)
        total = float(r.get("total", 0) or 0)
        pg_agg[aid]["cnt"] += 1
        pg_agg[aid]["sum"] += total

    qm_agg: dict[int, dict[str, float]] = defaultdict(lambda: {"cnt": 0, "sum": 0.0})
    for r in qm_rows:
        aid = int(r.get("id", 0) or 0)
        total = float(r.get("total", 0) or 0)
        qm_agg[aid]["cnt"] += 1
        qm_agg[aid]["sum"] += total

    mismatches = 0
    for aid in sorted(set(pg_agg.keys()) | set(qm_agg.keys())):
        pg_v = pg_agg.get(aid, {"cnt": 0, "sum": 0.0})
        qm_v = qm_agg.get(aid, {"cnt": 0, "sum": 0.0})
        if int(pg_v["cnt"]) != int(qm_v["cnt"]) or abs(float(pg_v["sum"]) - float(qm_v["sum"])) > 1e-6:
            mismatches += 1

    return {
        "enabled": True,
        "ok": mismatches == 0,
        "sample_accounts": len(ids),
        "pg_rows": len(pg_rows),
        "qm_rows": len(qm_rows),
        "mismatch_accounts": mismatches,
    }


def render_markdown(
    pg: dict[str, Any],
    qm: dict[str, Any],
    stress_pg: dict[str, Any] | None,
    stress_qm: dict[str, Any] | None,
    profile: str,
) -> str:
    return (
        "# QMvir vs PostgreSQL - JOIN/SUM Benchmark\n\n"
        f"Date: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n\n"
        f"Profile: `{profile}`\n\n"
        "## Summary\n\n"
        "| Metric | PostgreSQL | QMvir | Speedup (QM/PG) |\n"
        "|---|---:|---:|---:|\n"
        f"| JOIN QPS | {pg['join']['qps']:.0f} | {qm['join']['qps']:.0f} | {_speedup(pg['join']['qps'], qm['join']['qps'])} |\n"
        f"| JOIN p95 (ms) | {pg['join']['p95_ms']:.3f} | {qm['join']['p95_ms']:.3f} | {_speedup(pg['join']['p95_ms'], qm['join']['p95_ms'])} |\n"
        f"| SUM QPS | {pg['sum']['qps']:.0f} | {qm['sum']['qps']:.0f} | {_speedup(pg['sum']['qps'], qm['sum']['qps'])} |\n"
        f"| SUM p95 (ms) | {pg['sum']['p95_ms']:.3f} | {qm['sum']['p95_ms']:.3f} | {_speedup(pg['sum']['p95_ms'], qm['sum']['p95_ms'])} |\n\n"
        "## Details\n\n"
        f"- PostgreSQL JOIN avg: {pg['join']['avg_ms']:.3f} ms\n"
        f"- QMvir JOIN avg: {qm['join']['avg_ms']:.3f} ms\n"
        f"- PostgreSQL SUM avg: {pg['sum']['avg_ms']:.3f} ms\n"
        f"- QMvir SUM avg: {qm['sum']['avg_ms']:.3f} ms\n\n"
        + (
            "## Stress\n\n"
            f"- PostgreSQL: qps={stress_pg['throughput_qps']:.0f}, error_rate={stress_pg['error_rate']:.4f}\n"
            f"- QMvir: qps={stress_qm['throughput_qps']:.0f}, error_rate={stress_qm['error_rate']:.4f}\n"
            if stress_pg and stress_qm
            else ""
        )
    )


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="QMvir vs PostgreSQL JOIN/SUM benchmark")
    p.add_argument("--profile", choices=["quick", "standard", "heavy"], default="standard")
    p.add_argument("--iterations", type=int, default=1)
    p.add_argument("--ops", type=int, default=None)
    p.add_argument("--warmup", type=int, default=None)
    p.add_argument("--accounts", type=int, default=None)
    p.add_argument("--products", type=int, default=None)
    p.add_argument("--orders", type=int, default=None)
    p.add_argument("--pg-host", default="127.0.0.1")
    p.add_argument("--pg-port", type=int, default=55432)
    p.add_argument("--pg-user", default=os.environ.get("USER", "postgres"))
    p.add_argument("--pg-db", default="benchdb")
    p.add_argument("--pg-uds-dir", default=None)
    p.add_argument("--qm-host", default="127.0.0.1")
    p.add_argument("--qm-port", type=int, default=55433)
    p.add_argument("--qm-user", default="admin")
    p.add_argument("--qm-pass", default="admin")
    p.add_argument("--qm-db", default="qm")
    p.add_argument("--qm-uds-dir", default=None)
    p.add_argument("--stress", action="store_true")
    p.add_argument("--stress-seconds", type=int, default=None)
    p.add_argument("--stress-clients", type=int, default=None)
    p.add_argument("--output-md", default="/Users/gengyang/Desktop/AI/QM/benchmarks/QMVIR_VS_POSTGRES.md")
    p.add_argument("--output-json", default="/Users/gengyang/Desktop/AI/QM/benchmarks/QMVIR_VS_POSTGRES.json")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    profile = PROFILE_CONFIG[args.profile]

    ops = args.ops if args.ops is not None else profile["ops"]
    warmup = args.warmup if args.warmup is not None else profile["warmup"]
    accounts = args.accounts if args.accounts is not None else profile["accounts"]
    products = args.products if args.products is not None else profile["products"]
    orders = args.orders if args.orders is not None else profile["orders"]
    stress_seconds = args.stress_seconds if args.stress_seconds is not None else profile["stress_seconds"]
    stress_clients = args.stress_clients if args.stress_clients is not None else profile["stress_clients"]

    pg = ConnSpec(
        host=args.pg_host,
        port=args.pg_port,
        user=args.pg_user,
        dbname=args.pg_db,
        unix_socket_dir=args.pg_uds_dir,
    )
    qm = ConnSpec(
        host=args.qm_host,
        port=args.qm_port,
        user=args.qm_user,
        dbname=args.qm_db,
        password=args.qm_pass,
        unix_socket_dir=args.qm_uds_dir,
    )

    dataset = (accounts, products, orders)

    print("[1/3] Benchmark PostgreSQL ...")
    pg_res = run_join_sum(pg, "postgres", ops, warmup, dataset)

    print("[2/3] Benchmark QMvir ...")
    qm_res = run_join_sum(qm, "qmvir", ops, warmup, dataset)

    shadow_mode = os.environ.get("SHADOW_MODE", "0") == "1"
    shadow_res = None
    if shadow_mode:
        print("[2.5/3] SHADOW_MODE=1: PostgreSQL vs Rust kernel compare ...")
        shadow_res = _shadow_compare_pg_vs_rust(
            pg,
            qm,
            pg_acc=pg_res["dataset"]["accounts_table"],
            pg_ords=pg_res["dataset"]["orders_table"],
            pg_prod=pg_res["dataset"]["products_table"],
            qm_acc=qm_res["dataset"]["accounts_table"],
            qm_ords=qm_res["dataset"]["orders_table"],
            qm_prod=qm_res["dataset"]["products_table"],
        )

    stress_pg = stress_qm = None
    if args.stress:
        print("[3/3] Stress test PostgreSQL + QMvir ...")
        stress_pg = run_stress(pg, "postgres", stress_seconds, stress_clients, accounts)
        stress_qm = run_stress(qm, "qmvir", stress_seconds, stress_clients, accounts,
                               acc_tbl=qm_res["dataset"]["accounts_table"],
                               prod_tbl=qm_res["dataset"]["products_table"],
                               ords_tbl=qm_res["dataset"]["orders_table"])

    md = render_markdown(pg_res, qm_res, stress_pg, stress_qm, profile=args.profile)
    out_md = Path(args.output_md)
    out_md.write_text(md)

    out_json = Path(args.output_json)
    out_json.write_text(
        json.dumps(
            {
                "postgres": pg_res,
                "qmvir": qm_res,
                "stress_postgres": stress_pg,
                "stress_qmvir": stress_qm,
                "shadow": shadow_res,
            },
            indent=2,
        )
    )

    print(f"Report written: {out_md}")
    print(f"JSON written:   {out_json}")


if __name__ == "__main__":
    main()
