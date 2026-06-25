#!/usr/bin/env python3
"""QM vs PostgreSQL vector workloads: INSERT, UPDATE, KNN query."""

from __future__ import annotations

import argparse
import json
import os
import platform
import sys
import time
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit

try:
    import psycopg2
except ImportError as exc:  # pragma: no cover
    raise SystemExit("pip install psycopg2-binary") from exc

from compare_postgres_search_bench import bench_both, pg_connect  # noqa: E402

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
from segment_benchmark_lib import attach_deployment  # noqa: E402
from vector_recall_lib import build_recall_suite, qm_corpus_from_table  # noqa: E402


def vec_literal(i: int, dim: int) -> str:
    return "[" + ",".join(f"{((i + j) % 17) / 17.0:.6f}" for j in range(dim)) + "]"


def query_literal(dim: int) -> str:
    return "[" + ",".join(f"{0.1 + j * 0.01:.6f}" for j in range(dim)) + "]"


def setup_postgres(dsn: str, dim: int, rows: int) -> tuple[Any, Any, str]:
    conn = pg_connect(dsn)
    conn.autocommit = True
    cur = conn.cursor()
    cur.execute("CREATE EXTENSION IF NOT EXISTS vector")
    cur.execute("DROP TABLE IF EXISTS pg_vec_bench")
    cur.execute(
        f"CREATE TABLE pg_vec_bench (id INTEGER PRIMARY KEY, embedding vector({dim}))"
    )
    batch = 500
    for start in range(0, rows, batch):
        args = [
            (i, vec_literal(i, dim))
            for i in range(start, min(rows, start + batch))
        ]
        cur.executemany(
            "INSERT INTO pg_vec_bench (id, embedding) VALUES (%s, %s::vector)",
            args,
        )
    cur.execute(
        "CREATE INDEX pg_vec_bench_hnsw ON pg_vec_bench "
        "USING hnsw (embedding vector_l2_ops)"
    )
    cur.execute(
        "CREATE INDEX pg_vec_bench_hnsw_cos ON pg_vec_bench "
        "USING hnsw (embedding vector_cosine_ops)"
    )
    conn.commit()
    return conn, cur, query_literal(dim)


def setup_qm(qm_engine: Any, dim: int, rows: int) -> tuple[Any, str, int]:
    qm = qm_engine.NativeSqlEngine()
    qm.execute(f"CREATE TABLE vec_bench (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    batch = 500
    for start in range(0, rows, batch):
        values = []
        for i in range(start, min(rows, start + batch)):
            lit = vec_literal(i, dim)
            values.append(f"({i}, '{lit}')")
        qm.execute(
            "INSERT INTO vec_bench (id, embedding) VALUES " + ",".join(values)
        )
    qm.execute("CREATE INDEX idx_vec_bench_hnsw ON vec_bench (embedding) USING hnsw")
    next_id = rows + 100_000
    return qm, query_literal(dim), next_id


def run_vector_benchmark(
    qm_engine: Any,
    dsn: str,
    *,
    iterations: int = 200,
    rows: int = 10_000,
    dim: int = 32,
) -> dict[str, Any]:
    conn = pg_connect(dsn)
    cur = conn.cursor()
    cur.execute("SELECT extname FROM pg_extension WHERE extname = 'vector'")
    if cur.fetchone() is None:
        cur.close()
        conn.close()
        return {"error": "pgvector extension not installed", "rows": rows}
    cur.execute("SHOW server_version")
    pg_version = cur.fetchone()[0]
    cur.close()
    conn.close()

    conn, pg_cur, query = setup_postgres(dsn, dim, rows)
    print(f"[vector] postgres setup done rows={rows}", flush=True)
    qm, _, next_qm_id = setup_qm(qm_engine, dim, rows)
    print(f"[vector] qm setup done rows={rows}", flush=True)
    next_pg_id = rows + 100_000
    update_qm_id = rows // 2
    update_pg_id = rows // 2

    comparisons: list[dict[str, Any]] = []

    def qm_insert() -> None:
        nonlocal next_qm_id
        next_qm_id += 1
        lit = vec_literal(next_qm_id, dim)
        qm.execute(f"INSERT INTO vec_bench (id, embedding) VALUES ({next_qm_id}, '{lit}')")

    def pg_insert() -> None:
        nonlocal next_pg_id
        next_pg_id += 1
        pg_cur.execute(
            "INSERT INTO pg_vec_bench (id, embedding) VALUES (%s, %s::vector)",
            (next_pg_id, vec_literal(next_pg_id, dim)),
        )
        conn.commit()

    comparisons.append(
        bench_both(
            "vector.insert_autocommit",
            max(50, iterations // 2),
            qm_insert,
            pg_insert,
        )
    )

    def qm_update() -> None:
        nonlocal update_qm_id
        update_qm_id = (update_qm_id + 1) % rows
        lit = vec_literal(update_qm_id + 7_000, dim)
        qm.execute(
            f"UPDATE vec_bench SET embedding = '{lit}' WHERE id = {update_qm_id}"
        )

    def pg_update() -> None:
        nonlocal update_pg_id
        update_pg_id = (update_pg_id + 1) % rows
        pg_cur.execute(
            "UPDATE pg_vec_bench SET embedding = %s::vector WHERE id = %s",
            (vec_literal(update_pg_id + 7_000, dim), update_pg_id),
        )
        conn.commit()

    comparisons.append(
        bench_both(
            "vector.update_autocommit",
            max(50, iterations // 2),
            qm_update,
            pg_update,
        )
    )

    comparisons.append(
        bench_both(
            "vector.l2_top10_hnsw",
            iterations,
            lambda: qm.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <-> '{query}' LIMIT 10"
            ),
            lambda: (
                pg_cur.execute(
                    "SELECT id FROM pg_vec_bench ORDER BY embedding <-> %s::vector LIMIT 10",
                    (query,),
                ),
                pg_cur.fetchall(),
            ),
        )
    )
    comparisons.append(
        bench_both(
            "vector.l2_top50_hnsw",
            max(30, iterations // 2),
            lambda: qm.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <-> '{query}' LIMIT 50"
            ),
            lambda: (
                pg_cur.execute(
                    "SELECT id FROM pg_vec_bench ORDER BY embedding <-> %s::vector LIMIT 50",
                    (query,),
                ),
                pg_cur.fetchall(),
            ),
        )
    )
    comparisons.append(
        bench_both(
            "vector.cosine_top10_hnsw",
            max(30, iterations // 2),
            lambda: qm.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <=> '{query}' LIMIT 10"
            ),
            lambda: (
                pg_cur.execute(
                    "SELECT id FROM pg_vec_bench ORDER BY embedding <=> %s::vector LIMIT 10",
                    (query,),
                ),
                pg_cur.fetchall(),
            ),
        )
    )

    # Sustained batch insert throughput (not p50 latency).
    batch_n = min(2000, max(200, rows // 10))
    qm_batch = qm_engine.NativeSqlEngine()
    qm_batch.execute(f"CREATE TABLE vec_batch (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    base = 900_000
    values = []
    for i in range(batch_n):
        values.append(f"({base + i}, '{vec_literal(base + i, dim)}')")
    t0 = time.perf_counter()
    qm_batch.execute(
        "INSERT INTO vec_batch (id, embedding) VALUES " + ",".join(values)
    )
    qm_batch.execute("CREATE INDEX idx_vec_batch ON vec_batch (embedding) USING hnsw")
    qm_batch_rps = batch_n / (time.perf_counter() - t0)

    pg_cur.execute("DROP TABLE IF EXISTS pg_vec_batch")
    pg_cur.execute(f"CREATE TABLE pg_vec_batch (id INTEGER PRIMARY KEY, embedding vector({dim}))")
    t0 = time.perf_counter()
    args = [(base + i, vec_literal(base + i, dim)) for i in range(batch_n)]
    pg_cur.executemany(
        "INSERT INTO pg_vec_batch (id, embedding) VALUES (%s, %s::vector)",
        args,
    )
    conn.commit()
    pg_cur.execute(
        "CREATE INDEX pg_vec_batch_hnsw ON pg_vec_batch USING hnsw (embedding vector_l2_ops)"
    )
    pg_batch_rps = batch_n / (time.perf_counter() - t0)

    comparisons.append(
        {
            "workload": "vector.batch_insert_multivalue",
            "rows": batch_n,
            "qm": {"rows_per_sec": qm_batch_rps},
            "postgresql": {"rows_per_sec": pg_batch_rps},
            "winner": "QM" if qm_batch_rps > pg_batch_rps else "PostgreSQL",
        }
    )

    pg_cur.close()
    conn.close()

    def vec_list(i: int, d: int) -> list[float]:
        return [((i + j) % 17) / 17.0 for j in range(d)]

    corpus = qm_corpus_from_table(qm, table="vec_bench")
    if len(corpus) < rows:
        corpus = [(i, vec_list(i, dim)) for i in range(rows)]
    query_vec = [0.1 + j * 0.01 for j in range(dim)]

    def pg_search_ids(k: int) -> list[int]:
        conn2 = pg_connect(dsn)
        cur2 = conn2.cursor()
        cur2.execute(
            "SELECT id FROM pg_vec_bench ORDER BY embedding <-> %s::vector LIMIT %s",
            (query, k),
        )
        ids = [int(r[0]) for r in cur2.fetchall()]
        cur2.close()
        conn2.close()
        return ids

    recall_suite = build_recall_suite(
        corpus=corpus,
        query=query_vec,
        qm=qm,
        query_literal=query,
        competitor_name="PostgreSQL",
        competitor_search=pg_search_ids,
        ks=(10, 50),
    )
    if os.environ.get("QM_VECTOR_SKIP_RECALL", "").strip() in ("1", "true", "TRUE"):
        recall_suite = []

    wins = sum(1 for c in comparisons if c.get("winner") == "QM")
    pg_wins = sum(1 for c in comparisons if c.get("winner") == "PostgreSQL")
    return attach_deployment(
        {
            "rows": rows,
            "dim": dim,
            "environment": {"os": platform.platform(), "python": platform.python_version()},
            "postgresql_settings": {"server_version": pg_version, "pgvector_installed": True},
            "comparison": comparisons,
            "recall": recall_suite,
            "qm_wins": wins,
            "postgresql_wins": pg_wins,
            "total": len(comparisons),
            "competitor": "PostgreSQL",
            "competitor_key": "postgresql",
            "notes": [
                "insert/update: single-row autocommit with live HNSW maintenance",
                "query: HNSW-backed KNN after CREATE INDEX USING hnsw",
                "batch_insert_multivalue: one statement, includes index build for PG at end",
                "recall@k: brute-force L2 ground truth vs HNSW approximate results",
            ],
        },
        "PostgreSQL",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="QM vs PostgreSQL vector bench")
    parser.add_argument("--iterations", type=int, default=200)
    parser.add_argument("--rows", type=int, default=10_000)
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_pg_vector_bench.json"))
    args = parser.parse_args()

    dsn = os.environ.get("POSTGRES_DSN") or os.environ.get("QM_POSTGRES_DSN")
    if not dsn:
        raise SystemExit("POSTGRES_DSN required")

    import qm_engine  # type: ignore

    payload = run_vector_benchmark(
        qm_engine,
        dsn,
        iterations=args.iterations,
        rows=args.rows,
        dim=args.dim,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    print(json.dumps(payload, indent=2))
    return 0 if "error" not in payload else 1


if __name__ == "__main__":
    raise SystemExit(main())
