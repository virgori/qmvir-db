#!/usr/bin/env python3
"""Compare QM embedded NativeSqlEngine against PostgreSQL on fixed SQL workloads.

This script is intentionally dependency-light:
- PostgreSQL is driven through the `psql` CLI.
- QM is driven through the local PyO3 module `qm_engine`.

It reports p50/p95/p99 latency, throughput, result mismatches, planner notes, and
the cases where QM is slower than PostgreSQL.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from statistics import mean
from typing import Any


def schema_sql(
    accounts_table: str, orders_table: str, products_table: str, *, drop_existing: bool
) -> str:
    drops = ""
    if drop_existing:
        drops = f"""
DROP TABLE IF EXISTS {orders_table};
DROP TABLE IF EXISTS {products_table};
DROP TABLE IF EXISTS {accounts_table};
"""
    return f"""
{drops}
CREATE TABLE {accounts_table} (
    id INTEGER PRIMARY KEY,
    region INTEGER,
    balance REAL,
    name TEXT
);
CREATE TABLE {orders_table} (
    id INTEGER PRIMARY KEY,
    account_id INTEGER,
    product_id INTEGER,
    quantity INTEGER,
    total REAL,
    status TEXT
);
CREATE TABLE {products_table} (
    id INTEGER PRIMARY KEY,
    name TEXT,
    price REAL,
    category TEXT
);
CREATE INDEX idx_{accounts_table}_region ON {accounts_table} (region);
CREATE INDEX idx_{orders_table}_account_id ON {orders_table} (account_id);
CREATE INDEX idx_{orders_table}_product_id ON {orders_table} (product_id);
"""


def table_names(prefix: str) -> tuple[str, str, str]:
    safe = "".join(ch if ch.isalnum() or ch == "_" else "_" for ch in prefix.lower())
    safe = safe.strip("_") or f"qmcmp_{os.getpid()}"
    return f"bench_accounts_{safe}", f"bench_orders_{safe}", f"bench_products_{safe}"


@dataclass(frozen=True)
class QueryCase:
    name: str
    category: str
    sql: str
    min_rows: int = 1
    compare_result: bool = True


@dataclass
class CaseResult:
    name: str
    category: str
    iterations: int
    qm_p50_ms: float | None
    qm_p95_ms: float | None
    qm_p99_ms: float | None
    qm_mean_ms: float | None
    qm_ops_sec: float | None
    pg_p50_ms: float | None
    pg_p95_ms: float | None
    pg_p99_ms: float | None
    pg_mean_ms: float | None
    pg_ops_sec: float | None
    qm_vs_pg_ratio: float | None
    verdict: str
    result_match: bool | None
    qm_error: str | None = None
    pg_error: str | None = None
    qm_explain: str | None = None
    pg_plan: dict[str, Any] | None = None
    qm_profile: dict[str, Any] | None = None


@dataclass
class ComparisonReport:
    schema_version: int
    generated_at_utc: str
    rows: int
    iterations: int
    warmup: int
    qm_mode: str
    postgres_dsn: str
    environment: dict[str, str]
    results: list[CaseResult] = field(default_factory=list)
    slower_than_postgres: list[dict[str, Any]] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)


def now_utc() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    idx = round((pct / 100.0) * (len(ordered) - 1))
    return ordered[min(max(idx, 0), len(ordered) - 1)]


def latency_summary(latencies_ms: list[float]) -> dict[str, float | None]:
    if not latencies_ms:
        return {
            "p50_ms": None,
            "p95_ms": None,
            "p99_ms": None,
            "mean_ms": None,
            "ops_sec": None,
        }
    total_sec = sum(latencies_ms) / 1000.0
    return {
        "p50_ms": percentile(latencies_ms, 50),
        "p95_ms": percentile(latencies_ms, 95),
        "p99_ms": percentile(latencies_ms, 99),
        "mean_ms": mean(latencies_ms),
        "ops_sec": len(latencies_ms) / max(total_sec, sys.float_info.epsilon),
    }


def run_psql(dsn: str, sql: str, *, quiet: bool = True) -> str:
    cmd = ["psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", dsn]
    if quiet:
        cmd.extend(["-q", "-t", "-A"])
    proc = subprocess.run(
        cmd,
        input=sql,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())
    return proc.stdout.strip()


def pg_scalar_result(dsn: str, sql: str) -> list[list[str | None]]:
    out = run_psql(
        dsn,
        f"\\pset null __NULL__\n\\pset tuples_only on\n\\pset format unaligned\n{sql.rstrip(';')};\n",
    )
    if not out:
        return []
    rows: list[list[str | None]] = []
    for line in out.splitlines():
        rows.append([None if cell == "__NULL__" else cell for cell in line.split("|")])
    return rows


def pg_explain_analyze(dsn: str, sql: str) -> tuple[float, dict[str, Any]]:
    out = run_psql(dsn, f"EXPLAIN (ANALYZE, FORMAT JSON, BUFFERS TRUE) {sql.rstrip(';')};\n")
    parsed = json.loads(out)
    plan_doc = parsed[0]
    return float(plan_doc.get("Execution Time", 0.0)), plan_doc


def qm_rows(result: Any) -> list[list[str | None]]:
    rows_obj = result[1]
    return [[None if cell is None else str(cell) for cell in row] for row in rows_obj]


def normalize_rows(rows: list[list[str | None]]) -> list[list[str | None]]:
    normalized: list[list[str | None]] = []
    for row in rows:
        normalized.append([normalize_cell(cell) for cell in row])
    return sorted(normalized)


def normalize_cell(cell: str | None) -> str | None:
    if cell is None:
        return None
    try:
        numeric = float(cell)
    except ValueError:
        return cell
    if math.isfinite(numeric):
        return f"{numeric:.6g}"
    return cell


def cells_match(left: str | None, right: str | None, rel_tol: float = 1e-4) -> bool:
    if left is None or right is None:
        return left is right
    try:
        left_num = float(left)
        right_num = float(right)
    except ValueError:
        return left == right
    return math.isclose(left_num, right_num, rel_tol=rel_tol, abs_tol=rel_tol)


def results_match(left: list[list[str | None]], right: list[list[str | None]]) -> bool:
    if len(left) != len(right):
        return False
    left_rows = sorted(left, key=lambda row: json.dumps(row, sort_keys=True))
    right_rows = sorted(right, key=lambda row: json.dumps(row, sort_keys=True))
    for left_row, right_row in zip(left_rows, right_rows):
        if len(left_row) != len(right_row):
            return False
        if not all(cells_match(lcell, rcell) for lcell, rcell in zip(left_row, right_row)):
            return False
    return True


def batched_values(rows: list[tuple[Any, ...]], batch_size: int = 1000) -> list[str]:
    batches: list[str] = []
    for i in range(0, len(rows), batch_size):
        values = []
        for row in rows[i : i + batch_size]:
            cells = []
            for cell in row:
                if isinstance(cell, str):
                    cells.append("'" + cell.replace("'", "''") + "'")
                else:
                    cells.append(str(cell))
            values.append("(" + ", ".join(cells) + ")")
        batches.append(", ".join(values))
    return batches


def seed_sql(
    rows: int,
) -> tuple[list[tuple[Any, ...]], list[tuple[Any, ...]], list[tuple[Any, ...]]]:
    accounts = [
        (i, i % 16, round((i * 1.17) % 50000, 2), f"acct_{i}")
        for i in range(1, rows + 1)
    ]
    orders = [
        (
            i,
            ((i * 13) % rows) + 1,
            ((i * 7) % 1000) + 1,
            (i % 9) + 1,
            round(((i * 19) % 10000) / 3.0, 2),
            "paid" if i % 3 else "open",
        )
        for i in range(1, rows + 1)
    ]
    products = [
        (i, f"product_{i}", round(((i * 23) % 5000) / 2.0, 2), f"cat_{i % 25}")
        for i in range(1, 1001)
    ]
    return accounts, orders, products


def seed_postgres(
    dsn: str, rows: int, accounts_table: str, orders_table: str, products_table: str
) -> float:
    accounts, orders, products = seed_sql(rows)
    statements = [
        schema_sql(accounts_table, orders_table, products_table, drop_existing=True),
        "BEGIN;",
    ]
    for values in batched_values(accounts):
        statements.append(
            f"INSERT INTO {accounts_table} (id, region, balance, name) VALUES " + values + ";"
        )
    for values in batched_values(orders):
        statements.append(
            f"INSERT INTO {orders_table} (id, account_id, product_id, quantity, total, status) VALUES "
            + values
            + ";"
        )
    for values in batched_values(products):
        statements.append(
            f"INSERT INTO {products_table} (id, name, price, category) VALUES " + values + ";"
        )
    statements.append("COMMIT;")
    start = time.perf_counter()
    run_psql(dsn, "\n".join(statements), quiet=False)
    return time.perf_counter() - start


def import_qm_engine() -> Any:
    try:
        import qm_engine  # type: ignore
    except ImportError as exc:
        raise RuntimeError(
            "Cannot import qm_engine. Build/install it first, for example: "
            "python3 -m maturin develop --release --features extension-module"
        ) from exc
    return qm_engine


def seed_qm(
    qm_engine: Any,
    rows: int,
    data_dir: str | None,
    wal_sync_policy: str,
    accounts_table: str,
    orders_table: str,
    products_table: str,
) -> tuple[Any, float]:
    engine = qm_engine.NativeSqlEngine(data_dir) if data_dir else qm_engine.NativeSqlEngine()
    if data_dir and hasattr(engine, "set_wal_sync_policy"):
        engine.set_wal_sync_policy(wal_sync_policy)
    accounts, orders, products = seed_sql(rows)
    statements = [
        stmt.strip()
        for stmt in schema_sql(
            accounts_table, orders_table, products_table, drop_existing=False
        ).split(";")
        if stmt.strip()
    ]
    statements.extend(
        f"INSERT INTO {accounts_table} (id, region, balance, name) VALUES " + values
        for values in batched_values(accounts)
    )
    statements.extend(
        f"INSERT INTO {orders_table} (id, account_id, product_id, quantity, total, status) VALUES "
        + values
        for values in batched_values(orders)
    )
    statements.extend(
        f"INSERT INTO {products_table} (id, name, price, category) VALUES " + values
        for values in batched_values(products)
    )
    start = time.perf_counter()
    for statement in statements:
        engine.execute(statement)
    return engine, time.perf_counter() - start


def query_cases(
    rows: int, accounts_table: str, orders_table: str, products_table: str
) -> list[QueryCase]:
    mid = max(rows // 2, 1)
    hi = min(mid + 999, rows)
    return [
        QueryCase(
            "point_lookup_pk",
            "point_lookup",
            f"SELECT balance FROM {accounts_table} WHERE id = {mid}",
        ),
        QueryCase(
            "indexed_equality_count",
            "index",
            f"SELECT COUNT(*) FROM {orders_table} WHERE account_id = 42",
        ),
        QueryCase(
            "range_count_pk",
            "range",
            f"SELECT COUNT(*) FROM {orders_table} WHERE id BETWEEN {mid} AND {hi}",
        ),
        QueryCase("sum_large_column", "htap_aggregate", f"SELECT SUM(total) FROM {orders_table}"),
        QueryCase("avg_large_column", "htap_aggregate", f"SELECT AVG(total) FROM {orders_table}"),
        QueryCase(
            "sum_between_pk",
            "htap_range_aggregate",
            f"SELECT SUM(total) FROM {orders_table} WHERE id BETWEEN {mid} AND {hi}",
        ),
        QueryCase(
            "group_by_low_cardinality",
            "group_by",
            f"SELECT status, SUM(total), COUNT(*) FROM {orders_table} GROUP BY status",
        ),
        QueryCase(
            "join_filtered",
            "join",
            f"SELECT a.name, o.id, p.name, o.quantity, o.total "
            f"FROM {accounts_table} a "
            f"JOIN {orders_table} o ON a.id = o.account_id "
            f"JOIN {products_table} p ON o.product_id = p.id "
            "WHERE o.account_id = 42",
        ),
        QueryCase(
            "order_by_limit_large",
            "sort",
            f"SELECT id, total FROM {orders_table} ORDER BY total DESC LIMIT 20",
            compare_result=False,
        ),
    ]


def benchmark_qm(engine: Any, case: QueryCase, iterations: int, warmup: int) -> tuple[dict[str, Any], str | None, dict[str, Any] | None]:
    explain_text = None
    profile = None
    try:
        explain = engine.execute("EXPLAIN " + case.sql)
        explain_text = "\n".join("|".join("" if c is None else str(c) for c in row) for row in qm_rows(explain))
    except Exception:
        explain_text = None

    for _ in range(warmup):
        engine.execute(case.sql)

    if hasattr(engine, "reset_profile_snapshot"):
        engine.reset_profile_snapshot()

    latencies: list[float] = []
    for _ in range(iterations):
        start = time.perf_counter()
        engine.execute(case.sql)
        latencies.append((time.perf_counter() - start) * 1000.0)

    if hasattr(engine, "profile_snapshot"):
        try:
            profile = dict(engine.profile_snapshot())
        except Exception:
            profile = None
    return latency_summary(latencies), explain_text, profile


def benchmark_pg(dsn: str, case: QueryCase, iterations: int, warmup: int) -> tuple[dict[str, Any], dict[str, Any] | None]:
    for _ in range(warmup):
        pg_explain_analyze(dsn, case.sql)

    latencies: list[float] = []
    last_plan = None
    for _ in range(iterations):
        elapsed_ms, plan = pg_explain_analyze(dsn, case.sql)
        latencies.append(elapsed_ms)
        last_plan = plan
    return latency_summary(latencies), last_plan


def compare_case(
    engine: Any,
    dsn: str,
    case: QueryCase,
    iterations: int,
    warmup: int,
    strict: bool,
) -> CaseResult:
    qm_error = None
    pg_error = None
    qm_summary: dict[str, Any] = {}
    pg_summary: dict[str, Any] = {}
    qm_explain = None
    pg_plan = None
    qm_profile = None
    result_match = None

    try:
        qm_summary, qm_explain, qm_profile = benchmark_qm(engine, case, iterations, warmup)
    except Exception as exc:
        qm_error = str(exc)
        if strict:
            raise

    try:
        pg_summary, pg_plan = benchmark_pg(dsn, case, iterations, warmup)
    except Exception as exc:
        pg_error = str(exc)
        if strict:
            raise

    if case.compare_result and not qm_error and not pg_error:
        try:
            qm_result = qm_rows(engine.execute(case.sql))
            pg_result = pg_scalar_result(dsn, case.sql)
            result_match = results_match(qm_result, pg_result)
        except Exception as exc:
            result_match = False
            if strict:
                raise RuntimeError(f"{case.name} result comparison failed: {exc}") from exc

    qm_mean = qm_summary.get("mean_ms")
    pg_mean = pg_summary.get("mean_ms")
    ratio = None
    verdict = "unmeasured"
    if qm_mean is not None and pg_mean is not None:
        ratio = qm_mean / max(pg_mean, sys.float_info.epsilon)
        if ratio > 1.10:
            verdict = "qm_slower"
        elif ratio < 0.90:
            verdict = "qm_faster"
        else:
            verdict = "roughly_equal"
    if result_match is False:
        verdict = "result_mismatch"

    return CaseResult(
        name=case.name,
        category=case.category,
        iterations=iterations,
        qm_p50_ms=qm_summary.get("p50_ms"),
        qm_p95_ms=qm_summary.get("p95_ms"),
        qm_p99_ms=qm_summary.get("p99_ms"),
        qm_mean_ms=qm_mean,
        qm_ops_sec=qm_summary.get("ops_sec"),
        pg_p50_ms=pg_summary.get("p50_ms"),
        pg_p95_ms=pg_summary.get("p95_ms"),
        pg_p99_ms=pg_summary.get("p99_ms"),
        pg_mean_ms=pg_mean,
        pg_ops_sec=pg_summary.get("ops_sec"),
        qm_vs_pg_ratio=ratio,
        verdict=verdict,
        result_match=result_match,
        qm_error=qm_error,
        pg_error=pg_error,
        qm_explain=qm_explain,
        pg_plan=pg_plan,
        qm_profile=qm_profile,
    )


def print_summary(report: ComparisonReport) -> None:
    print(f"rows={report.rows} iterations={report.iterations} qm_mode={report.qm_mode}")
    print("")
    print(
        f"{'case':30} {'qm_mean_ms':>11} {'pg_mean_ms':>11} {'ratio':>8} {'verdict':>16} match"
    )
    for row in report.results:
        ratio = "" if row.qm_vs_pg_ratio is None else f"{row.qm_vs_pg_ratio:.2f}x"
        qm = "" if row.qm_mean_ms is None else f"{row.qm_mean_ms:.4f}"
        pg = "" if row.pg_mean_ms is None else f"{row.pg_mean_ms:.4f}"
        match = "" if row.result_match is None else str(row.result_match)
        print(f"{row.name:30} {qm:>11} {pg:>11} {ratio:>8} {row.verdict:>16} {match}")

    if report.slower_than_postgres:
        print("\nQM slower than PostgreSQL:")
        for item in report.slower_than_postgres:
            print(f"- {item['name']}: {item['ratio']:.2f}x slower")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compare QM NativeSqlEngine against PostgreSQL on fixed workloads."
    )
    parser.add_argument("--postgres-dsn", default=os.environ.get("POSTGRES_DSN"))
    parser.add_argument("--rows", type=int, default=100_000)
    parser.add_argument("--iterations", type=int, default=100)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument(
        "--output", type=Path, default=Path("docs/profiles/postgres_comparison_latest.json")
    )
    parser.add_argument("--qm-mode", choices=["memory", "persistent-wal"], default="memory")
    parser.add_argument(
        "--table-prefix",
        default=f"qmcmp_{os.getpid()}",
        help="Prefix for benchmark tables. Defaults to a process-specific name.",
    )
    parser.add_argument(
        "--qm-sync-policy",
        choices=["none", "per-commit", "per-mutation", "per-commit-sync-data"],
        default="per-commit",
    )
    parser.add_argument("--case", action="append", dest="cases", help="Run only a named case.")
    parser.add_argument("--strict", action="store_true", help="Fail on first measurement error.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not args.postgres_dsn:
        print("POSTGRES_DSN or --postgres-dsn is required", file=sys.stderr)
        return 2
    if not shutil.which("psql"):
        print("psql is required but was not found in PATH", file=sys.stderr)
        return 2

    qm_engine = import_qm_engine()
    accounts_table, orders_table, products_table = table_names(args.table_prefix)
    selected_cases = query_cases(args.rows, accounts_table, orders_table, products_table)
    if args.cases:
        wanted = set(args.cases)
        selected_cases = [case for case in selected_cases if case.name in wanted]
        missing = wanted - {case.name for case in selected_cases}
        if missing:
            print(f"unknown case(s): {', '.join(sorted(missing))}", file=sys.stderr)
            return 2

    qm_data_dir = None
    temp_dir = None
    if args.qm_mode == "persistent-wal":
        temp_dir = tempfile.TemporaryDirectory(prefix="qm_pg_compare_")
        qm_data_dir = temp_dir.name

    notes: list[str] = [
        f"Benchmark tables: {accounts_table}, {orders_table}, {products_table}",
        "PostgreSQL SELECT timings use EXPLAIN ANALYZE Execution Time, excluding psql process startup.",
        "QM timings are embedded Python-to-PyO3 wall-clock timings.",
        "Use --qm-mode persistent-wal to include QM WAL/checkpoint overhead during seed and write workloads.",
    ]

    print("seeding PostgreSQL...")
    pg_seed_sec = seed_postgres(
        args.postgres_dsn, args.rows, accounts_table, orders_table, products_table
    )
    print(f"PostgreSQL seed: {pg_seed_sec:.3f}s")

    print("seeding QM...")
    engine, qm_seed_sec = seed_qm(
        qm_engine,
        args.rows,
        qm_data_dir,
        args.qm_sync_policy,
        accounts_table,
        orders_table,
        products_table,
    )
    print(f"QM seed: {qm_seed_sec:.3f}s")
    notes.append(f"PostgreSQL seed elapsed_sec={pg_seed_sec:.6f}")
    notes.append(f"QM seed elapsed_sec={qm_seed_sec:.6f}")

    report = ComparisonReport(
        schema_version=1,
        generated_at_utc=now_utc(),
        rows=args.rows,
        iterations=args.iterations,
        warmup=args.warmup,
        qm_mode=args.qm_mode,
        postgres_dsn=args.postgres_dsn,
        environment={
            "python": sys.version.split()[0],
            "platform": sys.platform,
            "psql": shutil.which("psql") or "",
        },
        notes=notes,
    )

    for case in selected_cases:
        print(f"running {case.name}...")
        result = compare_case(
            engine,
            args.postgres_dsn,
            case,
            args.iterations,
            args.warmup,
            args.strict,
        )
        report.results.append(result)

    report.slower_than_postgres = [
        {
            "name": row.name,
            "category": row.category,
            "ratio": row.qm_vs_pg_ratio,
            "qm_mean_ms": row.qm_mean_ms,
            "pg_mean_ms": row.pg_mean_ms,
            "qm_explain": row.qm_explain,
        }
        for row in sorted(
            report.results,
            key=lambda item: item.qm_vs_pg_ratio or 0.0,
            reverse=True,
        )
        if row.qm_vs_pg_ratio is not None and row.qm_vs_pg_ratio > 1.10
    ]

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(asdict(report), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print_summary(report)
    print(f"\nwrote {args.output}")

    if temp_dir is not None:
        temp_dir.cleanup()
    return 1 if any(row.verdict == "result_mismatch" for row in report.results) else 0


if __name__ == "__main__":
    raise SystemExit(main())
