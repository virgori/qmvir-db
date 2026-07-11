#!/usr/bin/env python3
"""QM vs DuckDB OLAP workloads (scan, GROUP BY, aggregation).

Requires: pip install duckdb
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from segment_benchmark_lib import (  # noqa: E402
    SegmentResult,
    bench_pair,
    print_segment_summary,
    write_segment_result,
)


def seed_qm(rows: int) -> Any:
    import qm_engine  # type: ignore

    qm = qm_engine.NativeSqlEngine()
    qm.execute(
        "CREATE TABLE olap_bench (id INTEGER PRIMARY KEY, grp INTEGER, val DOUBLE, tag TEXT)"
    )
    t0 = time.perf_counter()
    qm.execute(
        f"INSERT INTO olap_bench SELECT "
        f"i, i % 100, (i % 17) / 17.0, 'tag_' || (i % 20) "
        f"FROM generate_series(0, {rows - 1}) AS t(i)"
    )
    load_s = time.perf_counter() - t0
    return qm, load_s


def seed_duckdb(rows: int) -> tuple[Any, float, str]:
    import duckdb

    duck = duckdb.connect(":memory:")
    t0 = time.perf_counter()
    duck.execute(
        f"""
        CREATE TABLE olap_bench AS
        SELECT
            i::INTEGER AS id,
            (i % 100)::INTEGER AS grp,
            ((i % 17) / 17.0)::DOUBLE AS val,
            ('tag_' || (i % 20))::VARCHAR AS tag
        FROM range({rows}) t(i)
        """
    )
    load_s = time.perf_counter() - t0
    version = duck.execute("SELECT version()").fetchone()[0]
    return duck, load_s, version


def run_duckdb_olap_benchmark(*, rows: int = 100_000, iterations: int = 30) -> dict:
    try:
        import duckdb  # noqa: F401
    except ImportError as exc:
        raise SystemExit("pip install duckdb") from exc

    qm, qm_load_s = seed_qm(rows)
    duck, duck_load_s, duck_version = seed_duckdb(rows)

    result = SegmentResult(
        segment="olap",
        competitor="DuckDB",
        rows=rows,
        notes=[
            "QM in-memory NativeSqlEngine vs DuckDB :memory:",
            "Same synthetic dataset: grp=i%100, val=(i%17)/17",
        ],
        extra={
            "setup": {
                "qm_load_elapsed_s": qm_load_s,
                "duckdb_load_elapsed_s": duck_load_s,
            },
            "duckdb_settings": {"version": duck_version},
        },
    )

    result.comparison.append(
        bench_pair(
            "olap.full_scan_count",
            iterations,
            lambda: qm.execute("SELECT COUNT(*) FROM olap_bench"),
            lambda: duck.execute("SELECT COUNT(*) FROM olap_bench").fetchone(),
            competitor_name="DuckDB",
        )
    )
    result.comparison.append(
        bench_pair(
            "olap.filter_scan_count",
            iterations,
            lambda: qm.execute("SELECT COUNT(*) FROM olap_bench WHERE grp = 42"),
            lambda: duck.execute(
                "SELECT COUNT(*) FROM olap_bench WHERE grp = 42"
            ).fetchone(),
            competitor_name="DuckDB",
        )
    )
    result.comparison.append(
        bench_pair(
            "olap.group_by_count",
            iterations,
            lambda: qm.execute(
                "SELECT grp, COUNT(*) AS c FROM olap_bench GROUP BY grp ORDER BY grp"
            ),
            lambda: duck.execute(
                "SELECT grp, COUNT(*) AS c FROM olap_bench GROUP BY grp ORDER BY grp"
            ).fetchall(),
            competitor_name="DuckDB",
        )
    )
    result.comparison.append(
        bench_pair(
            "olap.group_by_sum",
            iterations,
            lambda: qm.execute(
                "SELECT grp, SUM(val) AS s FROM olap_bench GROUP BY grp ORDER BY grp"
            ),
            lambda: duck.execute(
                "SELECT grp, SUM(val) AS s FROM olap_bench GROUP BY grp ORDER BY grp"
            ).fetchall(),
            competitor_name="DuckDB",
        )
    )
    result.comparison.append(
        bench_pair(
            "olap.aggregate_avg_filter",
            max(20, iterations // 2),
            lambda: qm.execute(
                "SELECT AVG(val) FROM olap_bench WHERE grp BETWEEN 10 AND 30"
            ),
            lambda: duck.execute(
                "SELECT AVG(val) FROM olap_bench WHERE grp BETWEEN 10 AND 30"
            ).fetchone(),
            competitor_name="DuckDB",
        )
    )

    return result.to_dict()


def main() -> int:
    parser = argparse.ArgumentParser(description="QM vs DuckDB OLAP benchmark")
    parser.add_argument("--rows", type=int, default=int(os.environ.get("OLAP_ROWS", "100000")))
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_duckdb_olap.json"))
    args = parser.parse_args()

    payload = run_duckdb_olap_benchmark(rows=args.rows, iterations=args.iterations)
    write_segment_result(args.output, payload)
    print(json.dumps(payload, indent=2))
    print()
    print_segment_summary(payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
