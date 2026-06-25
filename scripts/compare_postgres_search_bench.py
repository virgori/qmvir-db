#!/usr/bin/env python3
"""QM vs PostgreSQL for JSON, text, and vector search workloads."""

from __future__ import annotations

import argparse
import json
import os
import platform
import sys
import statistics
import time
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit

try:
    import psycopg2
except ImportError as exc:  # pragma: no cover
    raise SystemExit("pip install psycopg2-binary") from exc

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
from segment_benchmark_lib import attach_deployment  # noqa: E402


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def bench_both(
    name: str,
    iterations: int,
    qm_fn: Callable[[], Any],
    pg_fn: Callable[[], Any],
) -> dict[str, Any]:
    for _ in range(min(10, iterations)):
        qm_fn()
        pg_fn()

    qm_samples: list[float] = []
    pg_samples: list[float] = []
    for _ in range(iterations):
        t0 = time.perf_counter()
        qm_fn()
        qm_samples.append((time.perf_counter() - t0) * 1000.0)
        t0 = time.perf_counter()
        pg_fn()
        pg_samples.append((time.perf_counter() - t0) * 1000.0)

    qm_p50 = percentile(qm_samples, 50)
    pg_p50 = percentile(pg_samples, 50)
    winner = "QM" if qm_p50 < pg_p50 else "PostgreSQL" if pg_p50 < qm_p50 else "tie"
    return {
        "workload": name,
        "iterations": iterations,
        "qm": {
            "p50_ms": qm_p50,
            "p95_ms": percentile(qm_samples, 95),
            "throughput_ops_sec": iterations / (sum(qm_samples) / 1000.0) if qm_samples else 0.0,
        },
        "postgresql": {
            "p50_ms": pg_p50,
            "p95_ms": percentile(pg_samples, 95),
            "throughput_ops_sec": iterations / (sum(pg_samples) / 1000.0) if pg_samples else 0.0,
        },
        "qm_ops_ratio_vs_postgresql": (pg_p50 / qm_p50) if qm_p50 > 0 else None,
        "winner": winner,
    }


def pg_connect(dsn: str):
    parts = urlsplit(dsn)
    return psycopg2.connect(
        host=parts.hostname or "localhost",
        port=parts.port or 5432,
        user=parts.username,
        password=parts.password,
        dbname=parts.path.lstrip("/") or "postgres",
    )


def setup_postgres(dsn: str, dim: int, pgvector: bool, rows: int) -> Any:
    conn = pg_connect(dsn)
    conn.autocommit = True
    cur = conn.cursor()
    cur.execute("DROP TABLE IF EXISTS pg_search_bench")
    cur.execute(
        "CREATE TABLE pg_search_bench ("
        "id INTEGER PRIMARY KEY, data JSONB, tags TEXT, body TEXT, "
        f"embedding vector({dim})"
        ")"
        if pgvector
        else "CREATE TABLE pg_search_bench (id INTEGER PRIMARY KEY, data JSONB, tags TEXT, body TEXT)"
    )
    cur.execute("CREATE INDEX pg_search_bench_tags ON pg_search_bench (tags)")
    if pgvector:
        cur.execute(
            "CREATE INDEX pg_search_bench_embedding_hnsw_l2 ON pg_search_bench "
            "USING hnsw (embedding vector_l2_ops)"
        )
        cur.execute(
            "CREATE INDEX pg_search_bench_embedding_hnsw_cosine ON pg_search_bench "
            "USING hnsw (embedding vector_cosine_ops)"
        )
    cur.execute(
        "CREATE INDEX pg_search_bench_fts ON pg_search_bench "
        "USING gin (to_tsvector('english', body))"
    )
    try:
        cur.execute("CREATE EXTENSION IF NOT EXISTS pg_trgm")
        cur.execute(
            "CREATE INDEX pg_search_bench_body_trgm ON pg_search_bench "
            "USING gin (body gin_trgm_ops)"
        )
    except Exception:
        conn.rollback()
        conn.autocommit = True
    batch = 500
    for start in range(0, rows, batch):
        end = min(rows, start + batch)
        for i in range(start, end):
            if pgvector:
                emb = "[" + ",".join(f"{((i + j) % 17) / 17.0:.6f}" for j in range(dim)) + "]"
                cur.execute(
                    "INSERT INTO pg_search_bench (id, data, tags, body, embedding) "
                    "VALUES (%s, %s::jsonb, %s, %s, %s::vector)",
                    (
                        i,
                        f'{{"name":"user{i}","score":{i % 100}}}',
                        f"tag_{i % 20}",
                        f"alpha beta gamma text chunk {i} needle",
                        emb,
                    ),
                )
            else:
                cur.execute(
                    "INSERT INTO pg_search_bench (id, data, tags, body) VALUES (%s, %s::jsonb, %s, %s)",
                    (
                        i,
                        f'{{"name":"user{i}","score":{i % 100}}}',
                        f"tag_{i % 20}",
                        f"alpha beta gamma text chunk {i} needle",
                    ),
                )
    cur.close()
    return conn


def seed_qm_tables(qm_engine: Any, dim: int, rows: int) -> tuple[Any, Any, str]:
    qm = qm_engine.NativeSqlEngine()
    qm.execute("CREATE TABLE json_bench (id INTEGER PRIMARY KEY, data JSON, tags TEXT, body TEXT)")
    qm.execute("CREATE INDEX json_bench_tags ON json_bench (tags)")
    batch = 500
    for start in range(0, rows, batch):
        end = min(rows, start + batch)
        for i in range(start, end):
            qm.execute(
                f"INSERT INTO json_bench (id, data, tags, body) VALUES "
                f"({i}, '{{\"name\":\"user{i}\",\"score\":{i % 100}}}', 'tag_{i % 20}', "
                f"'alpha beta gamma text chunk {i} needle')"
            )
    qm.execute("CREATE INDEX idx_json_bench_body ON json_bench (body) USING gin")
    qm.execute("CREATE INDEX idx_json_bench_body_trgm ON json_bench (body) USING gin_trgm")
    qm.execute("CREATE INDEX idx_json_bench_data_name ON json_bench (data) USING json_path('name')")

    vec = qm_engine.NativeSqlEngine()
    vec.execute(f"CREATE TABLE vec_bench (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    for start in range(0, rows, batch):
        end = min(rows, start + batch)
        for i in range(start, end):
            literal = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
            vec.execute(f"INSERT INTO vec_bench (id, embedding) VALUES ({i}, '{literal}')")
    # Match pgvector: separate L2 and cosine HNSW indexes (fair head-to-head).
    vec.execute(
        "CREATE INDEX idx_vec_bench_embedding_l2 ON vec_bench (embedding) "
        "USING hnsw (embedding vector_l2_ops)"
    )
    vec.execute(
        "CREATE INDEX idx_vec_bench_embedding_cosine ON vec_bench (embedding) "
        "USING hnsw (embedding vector_cosine_ops)"
    )
    query = "[" + ",".join(f"{0.1 + j * 0.01:.4f}" for j in range(dim)) + "]"
    return qm, vec, query


def run_search_benchmark(
    qm_engine: Any,
    dsn: str,
    *,
    iterations: int = 200,
    rows: int = 1000,
) -> dict[str, Any]:
    conn = pg_connect(dsn)
    cur = conn.cursor()
    cur.execute("SELECT extname FROM pg_extension WHERE extname = 'vector'")
    pgvector = cur.fetchone() is not None
    cur.execute("SHOW server_version")
    pg_version = cur.fetchone()[0]
    cur.close()
    conn.close()

    dim = 32
    conn = setup_postgres(dsn, dim, pgvector, rows)
    qm, vec, query = seed_qm_tables(qm_engine, dim, rows)

    pg_cur = conn.cursor()
    comparisons: list[dict[str, Any]] = []
    next_qm_id = rows + 100_000
    next_pg_id = rows + 100_000
    target_user = min(42, max(0, rows - 1))

    def qm_json_insert() -> None:
        nonlocal next_qm_id
        next_qm_id += 1
        qm.execute(
            f"INSERT INTO json_bench (id, data, tags, body) VALUES "
            f"({next_qm_id}, '{{\"name\":\"new\"}}', 'tag_1', 'fresh text')"
        )

    def pg_json_insert() -> None:
        nonlocal next_pg_id
        next_pg_id += 1
        pg_cur.execute(
            "INSERT INTO pg_search_bench (id, data, tags, body) VALUES (%s, %s::jsonb, %s, %s)",
            (next_pg_id, '{"name":"new"}', "tag_1", "fresh text"),
        )
        conn.commit()

    comparisons.append(
        bench_both("json.insert_autocommit", max(50, iterations // 4), qm_json_insert, pg_json_insert)
    )
    comparisons.append(
        bench_both(
            "json.path_filter",
            iterations,
            lambda: qm.execute(
                f"SELECT id FROM json_bench WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'user{target_user}'"
            ),
            lambda: (
                pg_cur.execute(
                    "SELECT id FROM pg_search_bench WHERE data->>'name' = %s",
                    (f"user{target_user}",),
                ),
                pg_cur.fetchall(),
            ),
        )
    )
    comparisons.append(
        bench_both(
            "text.indexed_equality",
            iterations,
            lambda: qm.execute("SELECT id FROM json_bench WHERE tags = 'tag_7'"),
            lambda: (
                pg_cur.execute("SELECT id FROM pg_search_bench WHERE tags = %s", ("tag_7",)),
                pg_cur.fetchall(),
            ),
        )
    )
    comparisons.append(
        bench_both(
            "text.like_contains",
            iterations,
            lambda: qm.execute("SELECT id FROM json_bench WHERE body LIKE '%needle%'"),
            lambda: (
                pg_cur.execute("SELECT id FROM pg_search_bench WHERE body LIKE %s", ("%needle%",)),
                pg_cur.fetchall(),
            ),
        )
    )
    comparisons.append(
        bench_both(
            "text.fts_plainto_tsquery",
            iterations,
            lambda: qm.execute("SELECT id FROM json_bench WHERE body @@ 'needle alpha' LIMIT 10"),
            lambda: (
                pg_cur.execute(
                    "SELECT id FROM pg_search_bench "
                    "WHERE to_tsvector('english', body) @@ plainto_tsquery('english', 'needle alpha')"
                ),
                pg_cur.fetchall(),
            ),
        )
    )

    if pgvector:
        comparisons.append(
            bench_both(
                "vector.l2_top10_hnsw",
                max(30, iterations // 2),
                lambda: vec.execute(f"SELECT id FROM vec_bench ORDER BY embedding <-> '{query}' LIMIT 10"),
                lambda: (
                    pg_cur.execute(
                        "SELECT id FROM pg_search_bench ORDER BY embedding <-> %s::vector LIMIT 10",
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
                lambda: vec.execute(f"SELECT id FROM vec_bench ORDER BY embedding <=> '{query}' LIMIT 10"),
                lambda: (
                    pg_cur.execute(
                        "SELECT id FROM pg_search_bench ORDER BY embedding <=> %s::vector LIMIT 10",
                        (query,),
                    ),
                    pg_cur.fetchall(),
                ),
            )
        )
    else:
        comparisons.append(
            {
                "workload": "vector.pgvector",
                "skipped": True,
                "reason": "pgvector extension not installed on PostgreSQL",
            }
        )

    row = bench_both(
        "text.bm25_persistent_vs_fts",
        max(30, iterations // 2),
        lambda: qm.execute("SELECT id FROM json_bench WHERE body @@ 'needle alpha' LIMIT 10"),
        lambda: (
            pg_cur.execute(
                "SELECT id FROM pg_search_bench "
                "WHERE to_tsvector('english', body) @@ plainto_tsquery('english', 'needle alpha') "
                "LIMIT 10"
            ),
            pg_cur.fetchall(),
        ),
    )
    row["postgresql_note"] = "GIN tsvector, not BM25"
    row["qm_note"] = "persistent inverted index (BMW), pre-built via CREATE INDEX USING gin"
    comparisons.append(row)

    pg_cur.close()
    conn.close()

    wins = sum(1 for c in comparisons if c.get("winner") == "QM")
    pg_wins = sum(1 for c in comparisons if c.get("winner") == "PostgreSQL")
    return attach_deployment(
        {
            "rows": rows,
            "environment": {"os": platform.platform(), "python": platform.python_version()},
            "postgresql_settings": {
                "server_version": pg_version,
                "pgvector_installed": pgvector,
            },
            "qm_note": "vector ORDER BY uses exact heap sort unless HNSW index in QM; PG vector uses HNSW when pgvector installed",
            "comparison": comparisons,
            "qm_wins": wins,
            "postgresql_wins": pg_wins,
            "total": len([c for c in comparisons if not c.get("skipped")]),
            "competitor": "PostgreSQL",
            "competitor_key": "postgresql",
        },
        "PostgreSQL",
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=200)
    parser.add_argument("--rows", type=int, default=1000)
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_pg_search_linux.json"))
    args = parser.parse_args()

    dsn = os.environ.get("POSTGRES_DSN")
    if not dsn:
        raise SystemExit("POSTGRES_DSN required")

    import qm_engine  # type: ignore

    payload = run_search_benchmark(qm_engine, dsn, iterations=args.iterations, rows=args.rows)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    print(json.dumps(payload, indent=2))


if __name__ == "__main__":
    main()
