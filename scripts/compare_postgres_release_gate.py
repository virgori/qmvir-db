#!/usr/bin/env python3
"""P0 release gate checklist with QM vs PostgreSQL measurements where applicable."""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from compare_postgres_search_bench import bench_both, pg_connect  # noqa: E402
from publish_benchmark_lib import environment_info  # noqa: E402
from release_gate_realdata import (  # noqa: E402
    RELEASE_GATE_CHECKLIST,
    make_engine,
    run_all,
    vector_torture,
)

try:
    import psycopg2
except ImportError:
    psycopg2 = None  # type: ignore


def pg_exec(cur: Any, sql: str, params: tuple | None = None) -> None:
    if params:
        cur.execute(sql, params)
    else:
        cur.execute(sql)


def compare_bulk_ingest(
    qm_engine: Any,
    dsn: str,
    *,
    rows: int = 50_000,
    batch_size: int = 2000,
) -> dict[str, Any]:
    dim = 32
    with tempfile.TemporaryDirectory(prefix="qm-pg-bulk-") as tmp:
        data_dir = Path(tmp) / "qm"
        data_dir.mkdir()
        qm = make_engine(qm_engine, data_dir)
        qm.execute(
            "CREATE TABLE bulk_cmp (id INTEGER PRIMARY KEY, tags TEXT, body TEXT, score INTEGER)"
        )

        conn = pg_connect(dsn)
        conn.autocommit = True
        cur = conn.cursor()
        cur.execute("DROP TABLE IF EXISTS pg_bulk_cmp")
        cur.execute(
            "CREATE TABLE pg_bulk_cmp (id INTEGER PRIMARY KEY, tags TEXT, body TEXT, score INTEGER)"
        )

        def qm_batch(start: int, count: int) -> None:
            values = []
            for i in range(start, start + count):
                values.append(
                    f"({i}, 'tag_{i % 50}', 'chunk {i} alpha beta', {i % 100})"
                )
            qm.execute(
                "INSERT INTO bulk_cmp (id, tags, body, score) VALUES " + ",".join(values)
            )

        def pg_batch(start: int, count: int) -> None:
            args = [
                (i, f"tag_{i % 50}", f"chunk {i} alpha beta", i % 100)
                for i in range(start, start + count)
            ]
            cur.executemany(
                "INSERT INTO pg_bulk_cmp (id, tags, body, score) VALUES (%s, %s, %s, %s)",
                args,
            )

        t0 = time.perf_counter()
        for start in range(0, rows, batch_size):
            qm_batch(start, min(batch_size, rows - start))
        qm_elapsed = time.perf_counter() - t0

        t0 = time.perf_counter()
        for start in range(0, rows, batch_size):
            pg_batch(start, min(batch_size, rows - start))
        pg_elapsed = time.perf_counter() - t0

        idx_t0 = time.perf_counter()
        qm.execute("CREATE INDEX idx_bulk_cmp_tags ON bulk_cmp (tags)")
        qm.execute("CREATE INDEX idx_bulk_cmp_body ON bulk_cmp (body) USING gin")
        qm_idx = time.perf_counter() - idx_t0

        idx_t0 = time.perf_counter()
        cur.execute("CREATE INDEX pg_bulk_cmp_tags ON pg_bulk_cmp (tags)")
        cur.execute(
            "CREATE INDEX pg_bulk_cmp_body ON pg_bulk_cmp "
            "USING gin (to_tsvector('english', body))"
        )
        pg_idx = time.perf_counter() - idx_t0

        conn.close()
        qm_rps = rows / qm_elapsed if qm_elapsed > 0 else 0.0
        pg_rps = rows / pg_elapsed if pg_elapsed > 0 else 0.0
        winner = "QM" if qm_rps > pg_rps else "PostgreSQL" if pg_rps > qm_rps else "tie"
        return {
            "workload": "bulk_ingest",
            "rows": rows,
            "batch_size": batch_size,
            "qm": {"ingest_rows_per_sec": qm_rps, "index_build_s": qm_idx},
            "postgresql": {"ingest_rows_per_sec": pg_rps, "index_build_s": pg_idx},
            "winner": winner,
        }


def compare_vector_torture(qm_engine: Any, dsn: str, *, dim: int = 32, bulk_rows: int = 10_000) -> dict[str, Any]:
    qm_section = vector_torture(qm_engine, bulk_rows=bulk_rows, dim=dim)

    pg_dim_checks: dict[str, Any] = {"pgvector": False}
    if psycopg2 is None:
        pg_dim_checks["skipped"] = True
        return {"qm": qm_section, "postgresql": pg_dim_checks, "comparison": []}

    conn = pg_connect(dsn)
    conn.autocommit = True
    cur = conn.cursor()
    cur.execute("SELECT extname FROM pg_extension WHERE extname = 'vector'")
    pgvector = cur.fetchone() is not None
    pg_dim_checks["pgvector"] = pgvector

    if pgvector:
        cur.execute("DROP TABLE IF EXISTS pg_vec_dim")
        cur.execute(f"CREATE TABLE pg_vec_dim (id INTEGER PRIMARY KEY, embedding vector({dim}))")
        bad_plus = False
        bad_short = False
        try:
            lit = "[" + ",".join(["0.1"] * (dim + 1)) + "]"
            cur.execute(
                "INSERT INTO pg_vec_dim (id, embedding) VALUES (1, %s::vector)",
                (lit,),
            )
        except Exception:
            bad_plus = True
        try:
            cur.execute(
                "INSERT INTO pg_vec_dim (id, embedding) VALUES (2, '[0.1,0.2]'::vector)"
            )
        except Exception:
            bad_short = True
        pg_dim_checks["insert_dim_plus_one_rejects"] = bad_plus
        pg_dim_checks["insert_dim_too_short_rejects"] = bad_short

        cur.execute("DROP TABLE IF EXISTS pg_vec_bulk")
        cur.execute(f"CREATE TABLE pg_vec_bulk (id INTEGER PRIMARY KEY, embedding vector({dim}))")
        t0 = time.perf_counter()
        batch = 500
        for start in range(0, bulk_rows, batch):
            args = []
            for i in range(start, min(bulk_rows, start + batch)):
                lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
                args.append((i, lit))
            cur.executemany(
                "INSERT INTO pg_vec_bulk (id, embedding) VALUES (%s, %s::vector)",
                args,
            )
        pg_bulk_elapsed = time.perf_counter() - t0
        cur.execute(
            "CREATE INDEX pg_vec_bulk_hnsw ON pg_vec_bulk USING hnsw (embedding vector_l2_ops)"
        )
        query = "[" + ",".join(f"{0.1 + j * 0.01:.4f}" for j in range(dim)) + "]"
        pg_dim_checks["bulk_rows_per_sec"] = bulk_rows / pg_bulk_elapsed if pg_bulk_elapsed > 0 else 0.0
        conn.close()

        with tempfile.TemporaryDirectory(prefix="qm-vec-cmp-") as tmp:
            vec = make_engine(qm_engine, Path(tmp))
            vec.execute(f"CREATE TABLE vec_cmp (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
            for start in range(0, bulk_rows, batch):
                values = []
                for i in range(start, min(bulk_rows, start + batch)):
                    lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
                    values.append(f"({i}, '{lit}')")
                vec.execute(
                    "INSERT INTO vec_cmp (id, embedding) VALUES " + ",".join(values)
                )
            vec.execute("CREATE INDEX idx_vec_cmp ON vec_cmp (embedding) USING hnsw")

            comparison = [
                bench_both(
                    "vector.knn_l2_top10",
                    50,
                    lambda: vec.execute(
                        f"SELECT id FROM vec_cmp ORDER BY embedding <-> '{query}' LIMIT 10"
                    ),
                    lambda: _pg_knn(dsn, query),
                )
            ]
            qm_bulk_rps = next(
                (
                    c.get("bulk_rows_per_sec", 0)
                    for c in qm_section.get("cases", [])
                    if c.get("case") == "bulk_vector_load_knn"
                ),
                0,
            )
            return {
                "qm": qm_section,
                "postgresql": pg_dim_checks,
                "comparison": comparison,
                "bulk_winner": (
                    "QM"
                    if qm_bulk_rps > pg_dim_checks.get("bulk_rows_per_sec", 0)
                    else "PostgreSQL"
                ),
            }

    conn.close()
    return {"qm": qm_section, "postgresql": pg_dim_checks, "comparison": []}


def _pg_knn(dsn: str, query: str) -> None:
    conn = pg_connect(dsn)
    conn.autocommit = True
    cur = conn.cursor()
    cur.execute(
        "SELECT id FROM pg_vec_bulk ORDER BY embedding <-> %s::vector LIMIT 10",
        (query,),
    )
    cur.fetchall()
    conn.close()


def compare_search_fidelity_queries(
    qm_engine: Any,
    dsn: str,
    *,
    rows: int = 2000,
    iterations: int = 30,
) -> dict[str, Any]:
    dim = 32
    with tempfile.TemporaryDirectory(prefix="qm-pg-fidelity-") as tmp:
        qm = make_engine(qm_engine, Path(tmp))
        qm.execute(
            "CREATE TABLE fid_cmp (id INTEGER PRIMARY KEY, data JSON, tags TEXT, body TEXT)"
        )
        qm.execute(f"CREATE TABLE fid_vec (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
        qm.execute("CREATE INDEX fid_cmp_tags ON fid_cmp (tags)")
        for i in range(rows):
            qm.execute(
                f"INSERT INTO fid_cmp (id, data, tags, body) VALUES "
                f"({i}, '{{\"name\":\"user{i}\"}}', 'tag_{i % 20}', "
                f"'alpha beta needle chunk {i}')"
            )
        for i in range(min(rows, 128)):
            lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
            qm.execute(f"INSERT INTO fid_vec (id, embedding) VALUES ({i}, '{lit}')")
        qm.execute("CREATE INDEX idx_fid_body ON fid_cmp (body) USING gin")
        qm.execute("CREATE INDEX idx_fid_trgm ON fid_cmp (body) USING gin_trgm")
        qm.execute("CREATE INDEX idx_fid_json ON fid_cmp (data) USING json_path('name')")
        vec_query = "[" + ",".join(f"{0.1 + j * 0.01:.4f}" for j in range(dim)) + "]"

        conn = pg_connect(dsn)
        conn.autocommit = True
        cur = conn.cursor()
        cur.execute("DROP TABLE IF EXISTS pg_fid_cmp")
        cur.execute(
            "CREATE TABLE pg_fid_cmp (id INTEGER PRIMARY KEY, data JSONB, tags TEXT, body TEXT)"
        )
        cur.execute("CREATE INDEX pg_fid_cmp_tags ON pg_fid_cmp (tags)")
        cur.execute(
            "CREATE INDEX pg_fid_cmp_body ON pg_fid_cmp USING gin (to_tsvector('english', body))"
        )
        cur.execute(
            "CREATE INDEX pg_fid_cmp_trgm ON pg_fid_cmp USING gin (body gin_trgm_ops)"
        )
        for i in range(rows):
            cur.execute(
                "INSERT INTO pg_fid_cmp (id, data, tags, body) VALUES (%s, %s::jsonb, %s, %s)",
                (i, f'{{"name":"user{i}"}}', f"tag_{i % 20}", f"alpha beta needle chunk {i}"),
            )
        cur.execute("SELECT extname FROM pg_extension WHERE extname = 'vector'")
        pgvector = cur.fetchone() is not None
        if pgvector:
            cur.execute("DROP TABLE IF EXISTS pg_fid_vec")
            cur.execute(f"CREATE TABLE pg_fid_vec (id INTEGER PRIMARY KEY, embedding vector({dim}))")
            for i in range(min(rows, 128)):
                lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
                cur.execute(
                    "INSERT INTO pg_fid_vec (id, embedding) VALUES (%s, %s::vector)",
                    (i, lit),
                )
            cur.execute(
                "CREATE INDEX pg_fid_vec_hnsw ON pg_fid_vec USING hnsw (embedding vector_l2_ops)"
            )

        workloads: list[tuple[str, Callable[[], Any], Callable[[], Any]]] = [
            (
                "fidelity.fts",
                lambda: qm.execute("SELECT id FROM fid_cmp WHERE body @@ 'needle alpha' LIMIT 10"),
                lambda: cur.execute(
                    "SELECT id FROM pg_fid_cmp WHERE to_tsvector('english', body) "
                    "@@ plainto_tsquery('english', 'needle alpha') LIMIT 10"
                ),
            ),
            (
                "fidelity.json_path",
                lambda: qm.execute(
                    "SELECT id FROM fid_cmp WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'user42'"
                ),
                lambda: cur.execute(
                    "SELECT id FROM pg_fid_cmp WHERE data->>'name' = 'user42'"
                ),
            ),
            (
                "fidelity.trigram",
                lambda: qm.execute("SELECT id FROM fid_cmp WHERE body LIKE '%needle%'"),
                lambda: cur.execute("SELECT id FROM pg_fid_cmp WHERE body LIKE '%needle%'"),
            ),
            (
                "fidelity.equality",
                lambda: qm.execute("SELECT id FROM fid_cmp WHERE tags = 'tag_7'"),
                lambda: cur.execute("SELECT id FROM pg_fid_cmp WHERE tags = 'tag_7'"),
            ),
        ]
        if pgvector:
            workloads.append(
                (
                    "fidelity.vector_knn",
                    lambda: qm.execute(
                        f"SELECT id FROM fid_vec ORDER BY embedding <-> '{vec_query}' LIMIT 10"
                    ),
                    lambda: cur.execute(
                        "SELECT id FROM pg_fid_vec ORDER BY embedding <-> %s::vector LIMIT 10",
                        (vec_query,),
                    ),
                )
            )

        comparison = [bench_both(name, iterations, qm_fn, pg_fn) for name, qm_fn, pg_fn in workloads]
        conn.close()
        return {"rows": rows, "comparison": comparison}


def main() -> int:
    parser = argparse.ArgumentParser(description="P0 release gate vs PostgreSQL")
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--dsn", default=os.environ.get("POSTGRES_DSN") or os.environ.get("QM_POSTGRES_DSN"))
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_pg_release_gate.json"))
    args = parser.parse_args()

    if not args.dsn:
        print("POSTGRES_DSN required", file=sys.stderr)
        return 2
    if psycopg2 is None:
        print("pip install psycopg2-binary", file=sys.stderr)
        return 2

    try:
        import qm_engine  # type: ignore
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2

    rows = 20_000 if args.quick else 50_000
    qm_only = run_all(qm_engine, quick=args.quick)

    pg_sections = {
        "bulk_ingest": compare_bulk_ingest(qm_engine, args.dsn, rows=rows),
        "vector_torture": compare_vector_torture(
            qm_engine, args.dsn, bulk_rows=10_000 if args.quick else 50_000
        ),
        "search_fidelity_queries": compare_search_fidelity_queries(
            qm_engine, args.dsn, rows=2000 if args.quick else 5000
        ),
    }

    pg_wins = sum(
        1
        for sec in pg_sections.values()
        if sec.get("winner") == "PostgreSQL"
        or any(c.get("winner") == "PostgreSQL" for c in sec.get("comparison", []))
    )
    qm_wins = sum(
        1
        for sec in pg_sections.values()
        if sec.get("winner") == "QM"
        or any(c.get("winner") == "QM" for c in sec.get("comparison", []))
    )

    payload = {
        "label": "RELEASE_GATE_VS_POSTGRES",
        "mode": "quick" if args.quick else "full",
        "checklist": RELEASE_GATE_CHECKLIST,
        "environment": environment_info(),
        "qm_only_gate": qm_only,
        "vs_postgresql": pg_sections,
        "summary": {
            "qm_gate_ok": qm_only.get("ok"),
            "qm_gate_passed": qm_only.get("passed"),
            "qm_gate_total": qm_only.get("total"),
            "pg_comparison_qm_wins": qm_wins,
            "pg_comparison_pg_wins": pg_wins,
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(payload, indent=2, sort_keys=True)
    args.output.write_text(text + "\n")
    print(text)
    return 0 if qm_only.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
